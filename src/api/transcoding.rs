//! Rotas de remux/transcode: stream progressivo e HLS (`master.m3u8`, `main.m3u8`, segmentos).

use crate::{
    api::{auth::CurrentUser, shows::normalize_id},
    db::DbPool,
    transcode::{self, Container, SpawnError, TranscodeParams},
    users::{self, AuthUser},
};
use axum::{
    body::Body,
    extract::{Path, Query, RawQuery, State},
    http::{header, StatusCode},
    response::Response,
};
use std::{collections::HashMap, time::Duration};
use tokio_util::io::ReaderStream;
use tracing::{error, warn};

const PLAYLIST_MIME: &str = "application/vnd.apple.mpegurl";
/// Primeiro segmento: ffmpeg precisa abrir o arquivo e gerar ~6 s de mídia.
const PLAYLIST_TIMEOUT: Duration = Duration::from_secs(40);
const SEGMENT_TIMEOUT: Duration = Duration::from_secs(25);

fn spawn_status(e: SpawnError) -> StatusCode {
    match e {
        SpawnError::Unavailable => StatusCode::NOT_IMPLEMENTED,
        SpawnError::Busy => {
            warn!("Limite de transcodificações simultâneas atingido.");
            StatusCode::SERVICE_UNAVAILABLE
        }
        SpawnError::Io(msg) => {
            error!("Falha ao iniciar ffmpeg: {}", msg);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[derive(sqlx::FromRow)]
struct ItemInput {
    path: String,
    streams_json: Option<String>,
    has_audio: bool,
}

async fn item_input(pool: &DbPool, user: &AuthUser, id: &str) -> Result<ItemInput, StatusCode> {
    let id = normalize_id(id);
    if !users::item_allowed(pool, user, &id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        return Err(StatusCode::NOT_FOUND);
    }
    sqlx::query_as("SELECT path, streams_json, audio_codec IS NOT NULL AS has_audio FROM items WHERE id = ?")
        .bind(&id)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            error!("Erro de banco de dados: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)
}

/// Stream progressivo (`/Videos/{id}/stream.{ext}?transcode=...`).
/// `Ok(None)` quando a requisição não pede transcodificação (segue o Direct Play).
pub async fn try_progressive(
    pool: &DbPool,
    user: &AuthUser,
    id: &str,
    ext: Option<&str>,
    query: &HashMap<String, String>,
) -> Result<Option<Response>, StatusCode> {
    if !query.keys().any(|k| k.eq_ignore_ascii_case("transcode")) {
        return Ok(None);
    }
    let container = ext
        .and_then(Container::from_name)
        .or_else(|| {
            query
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("container"))
                .and_then(|(_, v)| Container::from_name(v))
        })
        .unwrap_or(Container::Mkv);
    let mut params = TranscodeParams::from_query(query, container, false).ok_or(StatusCode::BAD_REQUEST)?;
    let input = item_input(pool, user, id).await?;
    if !params.video_copy {
        params.tone_map_hdr = transcode::tone_map_hdr(input.streams_json.as_deref());
    }

    let stream = transcode::spawn_progressive(std::path::Path::new(&input.path), &params).map_err(spawn_status)?;
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, container.mime())
        .header(header::ACCEPT_RANGES, "none")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(stream))
        .map(Some)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn text_response(mime: &'static str, body: String) -> Result<Response, StatusCode> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

/// A mesma query nas variantes permite compartilhar a sessão FFmpeg.
pub async fn hls_master(
    Path(_id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, StatusCode> {
    let params = TranscodeParams::from_query(&q, Container::Ts, true).ok_or(StatusCode::BAD_REQUEST)?;
    let query = raw.unwrap_or_default();
    let with_audio = |video: i64| video.saturating_add(transcode::AUDIO_BITRATE);
    let bandwidth = params.abr.map(|ladder| ladder.high).or(params.video_bitrate).map(with_audio).unwrap_or(8_000_000);
    let version = if params.container == Container::Mp4 { "#EXT-X-VERSION:7\n" } else { "" };
    let mut body = format!("#EXTM3U\n{version}#EXT-X-STREAM-INF:BANDWIDTH={bandwidth}\nmain.m3u8?{query}\n");
    if let Some(ladder) = params.abr {
        body.push_str(&format!("#EXT-X-STREAM-INF:BANDWIDTH={}\nlow.m3u8?{query}\n", with_audio(ladder.low)));
    }
    text_response(PLAYLIST_MIME, body)
}

/// `main.m3u8`: inicia (ou reaproveita) a sessão ffmpeg e devolve a playlist de segmentos.
pub async fn hls_main(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, StatusCode> {
    hls_playlist(pool, user, id, q, false).await
}

pub async fn hls_low(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, StatusCode> {
    hls_playlist(pool, user, id, q, true).await
}

async fn hls_playlist(
    pool: DbPool,
    user: AuthUser,
    id: String,
    q: HashMap<String, String>,
    low: bool,
) -> Result<Response, StatusCode> {
    let mut params = TranscodeParams::from_query(&q, Container::Ts, true).ok_or(StatusCode::BAD_REQUEST)?;
    let file_name = match (params.abr, low) {
        (Some(_), false) => "main_high.m3u8",
        (Some(_), true) => "main_low.m3u8",
        (None, false) => "main.m3u8",
        (None, true) => return Err(StatusCode::NOT_FOUND),
    };
    let id = normalize_id(&id);
    let input = item_input(&pool, &user, &id).await?;
    if !params.video_copy {
        params.tone_map_hdr = transcode::tone_map_hdr(input.streams_json.as_deref());
    }
    params.has_audio = input.has_audio;
    let key = transcode::session_key(&id, &q, params.tone_map_hdr);

    transcode::ensure_hls(&key, std::path::Path::new(&input.path), &params)
        .await
        .map_err(spawn_status)?;

    let file = transcode::wait_for_file(&key, file_name, PLAYLIST_TIMEOUT)
        .await
        .ok_or_else(|| {
            error!("ffmpeg não gerou a playlist HLS de {}", id);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    let content = tokio::fs::read_to_string(&file)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    text_response(
        PLAYLIST_MIME,
        transcode::rewrite_playlist(&content, &format!("/Videos/{id}/hls/{key}/")),
    )
}

/// Segmento MPEG-TS ou fMP4, ou arquivo de inicialização fMP4.
pub async fn hls_segment(
    Path((_id, key, file)): Path<(String, String, String)>,
) -> Result<Response, StatusCode> {
    if !transcode::is_valid_key(&key) {
        return Err(StatusCode::NOT_FOUND);
    }
    let mime = transcode::hls_file_mime(&file).ok_or(StatusCode::NOT_FOUND)?;
    let path = transcode::wait_for_file(&key, &file, SEGMENT_TIMEOUT)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let f = tokio::fs::File::open(&path).await.map_err(|_| StatusCode::NOT_FOUND)?;
    let len = f.metadata().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.len();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(ReaderStream::with_capacity(f, 256 * 1024)))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
