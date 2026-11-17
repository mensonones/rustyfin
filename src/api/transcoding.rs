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

async fn item_path(pool: &DbPool, user: &AuthUser, id: &str) -> Result<String, StatusCode> {
    let id = normalize_id(id);
    if !users::item_allowed(pool, user, &id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        return Err(StatusCode::NOT_FOUND);
    }
    let row: Option<(String,)> = sqlx::query_as("SELECT path FROM items WHERE id = ?")
        .bind(&id)
        .fetch_optional(pool)
        .await
        .map_err(|e| {
            error!("Erro de banco de dados: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    row.map(|r| r.0).ok_or(StatusCode::NOT_FOUND)
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
    let params = TranscodeParams::from_query(query, container, false).ok_or(StatusCode::BAD_REQUEST)?;
    let path = item_path(pool, user, id).await?;

    let stream = transcode::spawn_progressive(std::path::Path::new(&path), &params).map_err(spawn_status)?;
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

/// `master.m3u8`: playlist mestre com uma única variante (`main.m3u8` com a mesma query).
pub async fn hls_master(
    Path(_id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    RawQuery(raw): RawQuery,
) -> Result<Response, StatusCode> {
    let params = TranscodeParams::from_query(&q, Container::Ts, true).ok_or(StatusCode::BAD_REQUEST)?;
    let bandwidth = params.video_bitrate.map(|b| b + 192_000).unwrap_or(8_000_000);
    text_response(
        PLAYLIST_MIME,
        format!(
            "#EXTM3U\n#EXT-X-STREAM-INF:PROGRAM-ID=1,BANDWIDTH={bandwidth}\nmain.m3u8?{}\n",
            raw.unwrap_or_default()
        ),
    )
}

/// `main.m3u8`: inicia (ou reaproveita) a sessão ffmpeg e devolve a playlist de segmentos.
pub async fn hls_main(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, StatusCode> {
    let params = TranscodeParams::from_query(&q, Container::Ts, true).ok_or(StatusCode::BAD_REQUEST)?;
    let id = normalize_id(&id);
    let path = item_path(&pool, &user, &id).await?;
    let key = transcode::session_key(&id, &q);

    transcode::ensure_hls(&key, std::path::Path::new(&path), &params)
        .await
        .map_err(spawn_status)?;

    let file = transcode::wait_for_file(&key, "main.m3u8", PLAYLIST_TIMEOUT)
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

/// `/Videos/{id}/hls/{key}/seg_00001.ts`
pub async fn hls_segment(
    Path((_id, key, file)): Path<(String, String, String)>,
) -> Result<Response, StatusCode> {
    if !transcode::is_valid_key(&key) || !transcode::is_valid_hls_file(&file) {
        return Err(StatusCode::NOT_FOUND);
    }
    let path = transcode::wait_for_file(&key, &file, SEGMENT_TIMEOUT)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;
    let f = tokio::fs::File::open(&path).await.map_err(|_| StatusCode::NOT_FOUND)?;
    let len = f.metadata().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.len();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "video/mp2t")
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from_stream(ReaderStream::with_capacity(f, 256 * 1024)))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
