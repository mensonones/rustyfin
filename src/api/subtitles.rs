//! Entrega legendas de texto embutidas e arquivos SRT ao lado do vídeo:
//! `/Videos/{id}/{mediaSourceId}/Subtitles/{index}/[{startTicks}/]Stream.{srt|vtt|ass}`.
//! O ffmpeg extrai ou converte trilhas sob demanda, com cache em `<cache>/subs`.

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
use std::{
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};
use tokio::{process::Command, sync::Semaphore};
use tokio_util::io::ReaderStream;
use tracing::warn;
use uuid::Uuid;

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
    type Row = (String, Option<String>, Option<String>, Option<String>, Option<String>);
    let row: Option<Row> =
        sqlx::query_as("SELECT path, streams_json, video_codec, audio_codec, subtitle_path FROM items WHERE id = ?")
            .bind(&id)
            .fetch_optional(&pool)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let (path, streams_json, vc, ac, subtitle_path) = row.ok_or(StatusCode::NOT_FOUND)?;

    let streams = build_streams(
        streams_json.as_deref(),
        vc.as_deref(),
        ac.as_deref(),
        None,
        None,
        subtitle_path.is_some(),
    );
    let stream = streams
        .iter()
        .find(|s| s.index == index && s.r#type == "Subtitle" && transcode::is_text_subtitle(&s.codec))
        .ok_or(StatusCode::NOT_FOUND)?;

    let (file_ext, fmt, mime) = format_for(&ext);
    let out = if stream.is_external {
        let subtitle = FsPath::new(subtitle_path.as_deref().ok_or(StatusCode::NOT_FOUND)?);
        let metadata = tokio::fs::symlink_metadata(subtitle)
            .await
            .map_err(|_| StatusCode::NOT_FOUND)?;
        if !metadata.is_file() {
            return Err(StatusCode::NOT_FOUND);
        }
        if file_ext == "srt" {
            subtitle.to_path_buf()
        } else {
            render(&config, &id, subtitle, None, file_ext, fmt)
                .await
                .ok_or(StatusCode::NOT_FOUND)?
        }
    } else {
        render(&config, &id, FsPath::new(&path), Some(index), file_ext, fmt)
            .await
            .ok_or(StatusCode::NOT_FOUND)?
    };
    let file = tokio::fs::File::open(&out).await.map_err(|_| StatusCode::NOT_FOUND)?;
    let len = file.metadata().await.map_err(|_| StatusCode::NOT_FOUND)?.len();
    let cache_control = if stream.is_external {
        "private, no-store"
    } else {
        "private, max-age=3600"
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::CACHE_CONTROL, cache_control)
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

async fn render(
    config: &AppConfig,
    id: &str,
    input: &FsPath,
    embedded_index: Option<i32>,
    file_ext: &str,
    fmt: &str,
) -> Option<PathBuf> {
    // O ID vem da URL: só hex, para nunca formar caminhos arbitrários.
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_hexdigit()) || embedded_index.is_some_and(|i| i < 0) {
        return None;
    }
    let metadata = tokio::fs::metadata(input).await.ok()?;
    let rev = metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    let source = match embedded_index {
        Some(index) => index.to_string(),
        None => Uuid::new_v5(&Uuid::NAMESPACE_URL, input.as_os_str().as_encoded_bytes())
            .simple()
            .to_string(),
    };
    let key = format!("{id}-{}-{rev}-{source}", metadata.len());
    let dir = config.cache_dir.join("subs");
    let out = dir.join(format!("{key}.{file_ext}"));
    if tokio::fs::metadata(&out).await.is_ok() {
        return Some(out);
    }
    tokio::fs::create_dir_all(&dir).await.ok()?;
    let _permit = SLOTS.acquire().await.ok()?;
    if tokio::fs::metadata(&out).await.is_ok() {
        return Some(out);
    }

    let tmp = dir.join(format!("{key}.{}.tmp.{file_ext}", Uuid::new_v4().simple()));
    let mut command = Command::new(transcode::ffmpeg_path());
    command.args(["-nostdin", "-v", "error", "-y"]);
    if embedded_index.is_none() {
        command.args(["-f", "srt"]);
    }
    let input_index = embedded_index.unwrap_or(0);
    let run = command
        .arg("-i")
        .arg(input)
        .args(["-map", &format!("0:{input_index}"), "-f", fmt])
        .arg(&tmp)
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(TIMEOUT, run).await {
        Ok(Ok(o)) if o.status.success() => {}
        Ok(Ok(o)) => {
            warn!("ffmpeg falhou ao preparar legenda {input_index}: {}", String::from_utf8_lossy(&o.stderr));
            let _ = tokio::fs::remove_file(&tmp).await;
            return None;
        }
        _ => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return None;
        }
    }
    if tokio::fs::rename(&tmp, &out).await.is_err() {
        let _ = tokio::fs::remove_file(&tmp).await;
        tokio::fs::metadata(&out).await.ok()?;
    }
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
