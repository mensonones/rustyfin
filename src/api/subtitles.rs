//! Entrega legendas de texto embutidas e arquivos SRT ao lado do vídeo:
//! `/Videos/{id}/{mediaSourceId}/Subtitles/{index}/[{startTicks}/]Stream.{srt|vtt|ass}`.
//! O ffmpeg extrai ou converte trilhas sob demanda, com cache em `<cache>/subs`.

pub(super) mod srt;

use crate::{
    api::{auth::CurrentUser, items::build_streams, shows::normalize_id},
    config::AppConfig,
    db::DbPool,
    models::MediaStream, transcode, users,
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
use sha2::{Digest, Sha256};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, process::Command, sync::Semaphore};
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
    let fonte = FonteLegenda::buscar(&pool, &id).await?;
    let faixas = fonte.faixas();
    let faixa = faixas.iter()
        .find(|s| s.index == index && s.r#type == "Subtitle" && transcode::is_text_subtitle(&s.codec))
        .ok_or(StatusCode::NOT_FOUND)?;
    let (file_ext, fmt, mime) = format_for(&ext);
    if srt_aceita(faixa) {
        if let Some(corpo) = legenda_corrigida(&config, &pool, &fonte, faixa, file_ext).await? {
            return Response::builder()
                .header(header::CONTENT_TYPE, mime)
                .header(header::CACHE_CONTROL, "private, no-store")
                .body(Body::from(corpo))
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
    let out = fonte.arquivo(&config, faixa, file_ext, fmt).await?;
    let file = tokio::fs::File::open(&out).await.map_err(|_| StatusCode::NOT_FOUND)?;
    let len = file.metadata().await.map_err(|_| StatusCode::NOT_FOUND)?.len();
    let cache_control = if faixa.is_external || srt_aceita(faixa) {
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

const MAX_SRT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(sqlx::FromRow)]
pub(super) struct FonteLegenda {
    pub(super) id: String,
    path: String,
    streams_json: Option<String>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    subtitle_path: Option<String>,
    content_sha256: Option<Vec<u8>>,
}

pub(super) struct OriginalSrt {
    pub(super) assinatura: String,
    texto: String,
}

impl OriginalSrt {
    pub(super) async fn trechos(self) -> Result<Vec<srt::Trecho>, StatusCode> {
        tokio::task::spawn_blocking(move || srt::ler(&self.texto))
            .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::UNPROCESSABLE_ENTITY)
    }
}

pub(super) fn srt_aceita(faixa: &MediaStream) -> bool {
    faixa.r#type == "Subtitle"
        && (faixa.codec.eq_ignore_ascii_case("srt") || faixa.codec.eq_ignore_ascii_case("subrip"))
}

impl FonteLegenda {
    pub(super) async fn buscar(pool: &DbPool, id: &str) -> Result<Self, StatusCode> {
        sqlx::query_as(
            "SELECT id, path, streams_json, video_codec, audio_codec, subtitle_path, content_sha256 \
             FROM items WHERE id = ?",
        )
        .bind(normalize_id(id))
        .fetch_optional(pool)
        .await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)
    }

    pub(super) fn faixas(&self) -> Vec<MediaStream> {
        build_streams(
            self.streams_json.as_deref(), self.video_codec.as_deref(), self.audio_codec.as_deref(),
            None, None, self.subtitle_path.is_some(),
        )
    }

    async fn arquivo(
        &self, config: &AppConfig, faixa: &MediaStream, ext: &str, fmt: &str,
    ) -> Result<PathBuf, StatusCode> {
        let (origem, indice) = if faixa.is_external {
            let path = FsPath::new(self.subtitle_path.as_deref().ok_or(StatusCode::NOT_FOUND)?);
            let metadata = tokio::fs::symlink_metadata(path)
                .await.map_err(|_| StatusCode::NOT_FOUND)?;
            if !metadata.is_file() {
                return Err(StatusCode::NOT_FOUND);
            }
            if ext == "srt" {
                return Ok(path.to_path_buf());
            }
            (path, None)
        } else {
            (FsPath::new(&self.path), Some(faixa.index))
        };
        render(config, &self.id, origem, indice, ext, fmt).await.ok_or(StatusCode::NOT_FOUND)
    }

    pub(super) async fn original(
        &self, config: &AppConfig, faixa: &MediaStream,
    ) -> Result<OriginalSrt, StatusCode> {
        let path = self.arquivo(config, faixa, "srt", "srt").await?;
        let arquivo = tokio::fs::File::open(path).await.map_err(|_| StatusCode::NOT_FOUND)?;
        let mut bytes = Vec::new();
        arquivo.take(MAX_SRT_BYTES + 1).read_to_end(&mut bytes)
            .await.map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
        if bytes.len() as u64 > MAX_SRT_BYTES {
            return Err(StatusCode::PAYLOAD_TOO_LARGE);
        }
        // A assinatura inclui o hash do vídeo. Logo depois da primeira varredura ele ainda está sendo calculado em segundo
        // plano; um ajuste salvo agora seria descartado quando o hash aparecesse e a assinatura mudasse. Recusa (409).
        let video = self.content_sha256.as_ref().ok_or(StatusCode::CONFLICT)?;
        let mut hash = Sha256::new();
        hash.update(video);
        hash.update([0]);
        hash.update(&bytes);
        let assinatura = format!("{:x}", hash.finalize());
        let texto = String::from_utf8(bytes).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
        Ok(OriginalSrt { assinatura, texto })
    }
}

pub(super) async fn ajuste_salvo(
    pool: &DbPool, id: &str, indice: i32,
) -> Result<Option<(String, srt::Ajuste)>, StatusCode> {
    let linha: Option<(String, String)> = sqlx::query_as(
        "SELECT source_hash, adjustment FROM subtitle_adjustments \
         WHERE item_id = ? AND track_index = ?",
    )
    .bind(id).bind(indice)
    .fetch_optional(pool).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    linha.map(|(hash, json)| {
        serde_json::from_str(&json)
            .map(|ajuste| (hash, ajuste)).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
    }).transpose()
}

async fn legenda_corrigida(
    config: &AppConfig, pool: &DbPool, fonte: &FonteLegenda, faixa: &MediaStream, ext: &str,
) -> Result<Option<Vec<u8>>, StatusCode> {
    let Some((hash, ajuste)) = ajuste_salvo(pool, &fonte.id, faixa.index).await? else {
        return Ok(None);
    };
    let original = match fonte.original(config, faixa).await {
        Ok(original) if original.assinatura == hash => original,
        _ => return Ok(None),
    };
    let Some(mapa) = ajuste.mapa() else {
        return Ok(None);
    };
    let vtt = ext == "vtt";
    let corrigida = tokio::task::spawn_blocking(move || {
        srt::renderizar(&srt::ler(&original.texto)?, mapa, vtt)
    }).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let Some(corrigida) = corrigida else {
        return Ok(None);
    };
    if ext == "ass" {
        let ass = converter_ass(corrigida).await.ok_or(StatusCode::UNPROCESSABLE_ENTITY)?;
        return Ok(Some(ass));
    }
    Ok(Some(corrigida.into_bytes()))
}

async fn converter_ass(srt: String) -> Option<Vec<u8>> {
    let _permit = SLOTS.acquire().await.ok()?;
    let mut filho = Command::new(transcode::ffmpeg_path())
        .args(["-nostdin", "-v", "error", "-f", "srt", "-i", "pipe:0", "-f", "ass", "pipe:1"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true).spawn().ok()?;
    let mut entrada = filho.stdin.take()?;
    let enviar = async move { entrada.write_all(srt.as_bytes()).await };
    let ((), saida) = tokio::time::timeout(TIMEOUT, async {
        tokio::try_join!(enviar, filho.wait_with_output())
    }).await.ok()?.ok()?;
    saida.status.success().then_some(saida.stdout)
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
