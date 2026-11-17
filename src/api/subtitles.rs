//! Legendas embutidas entregues como arquivo externo:
//! `/Videos/{id}/{mediaSourceId}/Subtitles/{index}/[{startTicks}/]Stream.{srt|vtt|ass}`.
//! O ffmpeg extrai a trilha uma vez e o resultado fica em cache (`<cache>/subs`).

use crate::{
    api::{auth::CurrentUser, items::build_streams, shows::normalize_id},
    config::AppConfig,
    db::DbPool,
    transcode, users,
};
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::Response,
};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{process::Command, sync::Semaphore};
use tracing::warn;

static SLOTS: Semaphore = Semaphore::const_new(2);
const TIMEOUT: Duration = Duration::from_secs(120);

type St = State<(Arc<AppConfig>, DbPool)>;

pub async fn get_subtitle(
    state: St,
    user: CurrentUser,
    Path((id, _source, index, ext)): Path<(String, String, i32, String)>,
) -> Result<Response, StatusCode> {
    serve(state, user, id, index, ext).await
}

/// Variante com a posição inicial (ignorada: o cliente aplica o deslocamento).
pub async fn get_subtitle_from(
    state: St,
    user: CurrentUser,
    Path((id, _source, index, _start, ext)): Path<(String, String, i32, String, String)>,
) -> Result<Response, StatusCode> {
    serve(state, user, id, index, ext).await
}

/// (extensão do arquivo, nome do formato no ffmpeg, MIME)
fn format_for(ext: &str) -> (&'static str, &'static str, &'static str) {
    match ext.to_ascii_lowercase().as_str() {
        "vtt" | "webvtt" => ("vtt", "webvtt", "text/vtt; charset=utf-8"),
        "ass" | "ssa" => ("ass", "ass", "text/x-ssa; charset=utf-8"),
        _ => ("srt", "srt", "application/x-subrip; charset=utf-8"),
    }
}

async fn serve(
    State((config, pool)): St,
    CurrentUser(user): CurrentUser,
    id: String,
    index: i32,
    ext: String,
) -> Result<Response, StatusCode> {
    let id = normalize_id(&id);
    if !users::item_allowed(&pool, &user, &id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        return Err(StatusCode::NOT_FOUND);
    }
    type Row = (String, Option<String>, Option<String>, Option<String>);
    let row: Option<Row> =
        sqlx::query_as("SELECT path, streams_json, video_codec, audio_codec FROM items WHERE id = ?")
            .bind(&id)
            .fetch_optional(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (path, streams_json, vc, ac) = row.ok_or(StatusCode::NOT_FOUND)?;

    // Só trilhas de legenda de texto existentes.
    let streams = build_streams(streams_json.as_deref(), vc.as_deref(), ac.as_deref(), None, None);
    let ok = streams
        .iter()
        .any(|s| s.index == index && s.r#type == "Subtitle" && transcode::is_text_subtitle(&s.codec));
    if !ok {
        return Err(StatusCode::NOT_FOUND);
    }

    let (file_ext, fmt, mime) = format_for(&ext);
    let out = extract(&config, &id, std::path::Path::new(&path), index, file_ext, fmt)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let bytes = tokio::fs::read(&out).await.map_err(|_| StatusCode::NOT_FOUND)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, bytes.len().to_string())
        .header(header::CACHE_CONTROL, "private, max-age=3600")
        .body(Body::from(bytes))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn extract(
    config: &AppConfig,
    id: &str,
    video: &std::path::Path,
    index: i32,
    file_ext: &str,
    fmt: &str,
) -> Option<PathBuf> {
    // O ID vem da URL: só hex, para nunca formar caminhos arbitrários.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) || index < 0 {
        return None;
    }
    let rev = tokio::fs::metadata(video)
        .await
        .ok()?
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = config.cache_dir.join("subs");
    let out = dir.join(format!("{id}-{rev}-{index}.{file_ext}"));
    if tokio::fs::metadata(&out).await.is_ok() {
        return Some(out);
    }
    tokio::fs::create_dir_all(&dir).await.ok()?;
    let _permit = SLOTS.acquire().await.ok()?;
    if tokio::fs::metadata(&out).await.is_ok() {
        return Some(out);
    }

    let tmp = dir.join(format!("{id}-{rev}-{index}.tmp.{file_ext}"));
    let run = Command::new(transcode::ffmpeg_path())
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(video)
        .args(["-map", &format!("0:{index}"), "-f", fmt])
        .arg(&tmp)
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(TIMEOUT, run).await {
        Ok(Ok(o)) if o.status.success() => {}
        Ok(Ok(o)) => {
            warn!("ffmpeg falhou ao extrair legenda {index}: {}", String::from_utf8_lossy(&o.stderr));
            let _ = tokio::fs::remove_file(&tmp).await;
            return None;
        }
        _ => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return None;
        }
    }
    tokio::fs::rename(&tmp, &out).await.ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_for("SRT").0, "srt");
        assert_eq!(format_for("vtt").1, "webvtt");
        assert_eq!(format_for("ssa").0, "ass");
        assert_eq!(format_for("qualquer").0, "srt");
    }
}
