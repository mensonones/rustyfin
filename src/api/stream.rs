use crate::{
    api::{auth::CurrentUser, transcoding},
    db::DbPool,
    users::{self, AuthUser},
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use std::{collections::HashMap, io::SeekFrom};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

pub async fn stream_video(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(item_id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    if let Some(resp) = transcoding::try_progressive(&pool, &user, &item_id, None, &query).await? {
        return Ok(resp);
    }
    stream_media_by_id(&pool, &user, &item_id, headers).await
}

pub async fn stream_video_with_ext(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path((item_id, ext)): Path<(String, String)>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    if let Some(resp) = transcoding::try_progressive(&pool, &user, &item_id, Some(&ext), &query).await? {
        return Ok(resp);
    }
    stream_media_by_id(&pool, &user, &item_id, headers).await
}

pub async fn stream_audio(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(item_id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    stream_media_by_id(&pool, &user, &item_id, headers).await
}

async fn stream_media_by_id(
    pool: &DbPool,
    user: &AuthUser,
    item_id: &str,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    if !users::item_allowed(pool, user, item_id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        return Err(StatusCode::NOT_FOUND);
    }
    let (file_path, container): (String, Option<String>) = sqlx::query_as(
        "SELECT path, container FROM items WHERE id = ?"
    )
    .bind(item_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let mut file = tokio::fs::File::open(&file_path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    let total_size = file
        .metadata()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .len();

    let mime_type = match container.as_deref() {
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("avi") => "video/x-msvideo",
        Some("ts") => "video/mp2t",
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("aac") => "audio/aac",
        _ => "application/octet-stream",
    };

    if let Some(range_header) = headers.get(header::RANGE).and_then(|h| h.to_str().ok()) {
        match parse_range_header(range_header, total_size) {
            RangeResult::Satisfiable(start, end) => {
                let chunk_size = end - start + 1;

                file.seek(SeekFrom::Start(start))
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

                let body = Body::from_stream(ReaderStream::with_capacity(file.take(chunk_size), 256 * 1024));

                return Response::builder()
                    .status(StatusCode::PARTIAL_CONTENT)
                    .header(header::CONTENT_TYPE, mime_type)
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_LENGTH, chunk_size.to_string())
                    .header(
                        header::CONTENT_RANGE,
                        format!("bytes {}-{}/{}", start, end, total_size),
                    )
                    .body(body)
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
            }
            RangeResult::NotSatisfiable => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::ACCEPT_RANGES, "bytes")
                    .header(header::CONTENT_RANGE, format!("bytes */{}", total_size))
                    .body(Body::empty())
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
            }
            // Cabeçalho malformado ou multi-range: RFC 7233 permite ignorar e servir tudo.
            RangeResult::Ignore => {}
        }
    }

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime_type)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, total_size.to_string())
        .body(Body::from_stream(ReaderStream::with_capacity(file, 256 * 1024)))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Debug, PartialEq, Eq)]
enum RangeResult {
    Satisfiable(u64, u64),
    NotSatisfiable,
    Ignore,
}

fn parse_range_header(range_val: &str, total_size: u64) -> RangeResult {
    let Some(spec) = range_val.trim().strip_prefix("bytes=") else {
        return RangeResult::Ignore;
    };
    // Multi-range (`0-1,5-9`) não é suportado: serve o arquivo completo.
    if spec.contains(',') {
        return RangeResult::Ignore;
    }
    let Some((start_str, end_str)) = spec.split_once('-') else {
        return RangeResult::Ignore;
    };
    let (start_str, end_str) = (start_str.trim(), end_str.trim());

    if total_size == 0 {
        return RangeResult::NotSatisfiable;
    }

    let (start, end) = if start_str.is_empty() {
        // Sufixo: bytes=-500 (últimos 500 bytes)
        let Ok(len) = end_str.parse::<u64>() else {
            return RangeResult::Ignore;
        };
        if len == 0 {
            return RangeResult::NotSatisfiable;
        }
        (total_size.saturating_sub(len), total_size - 1)
    } else {
        let Ok(start) = start_str.parse::<u64>() else {
            return RangeResult::Ignore;
        };
        let end = if end_str.is_empty() {
            total_size - 1
        } else {
            match end_str.parse::<u64>() {
                Ok(e) => e,
                Err(_) => return RangeResult::Ignore,
            }
        };
        (start, end)
    };

    if start > end && !end_str.is_empty() {
        // `bytes=10-5` é sintaticamente inválido → ignorar.
        return RangeResult::Ignore;
    }
    if start >= total_size {
        return RangeResult::NotSatisfiable;
    }

    RangeResult::Satisfiable(start, end.min(total_size - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use RangeResult::*;

    #[test]
    fn range_forms() {
        assert_eq!(parse_range_header("bytes=0-99", 1000), Satisfiable(0, 99));
        assert_eq!(parse_range_header("bytes=900-", 1000), Satisfiable(900, 999));
        assert_eq!(parse_range_header("bytes=-100", 1000), Satisfiable(900, 999));
        assert_eq!(parse_range_header("bytes=-5000", 1000), Satisfiable(0, 999));
        assert_eq!(parse_range_header("bytes=500-9999", 1000), Satisfiable(500, 999));
    }

    #[test]
    fn range_errors() {
        assert_eq!(parse_range_header("bytes=1000-", 1000), NotSatisfiable);
        assert_eq!(parse_range_header("bytes=2000-3000", 1000), NotSatisfiable);
        assert_eq!(parse_range_header("bytes=-0", 1000), NotSatisfiable);
        assert_eq!(parse_range_header("bytes=0-1", 0), NotSatisfiable);
        assert_eq!(parse_range_header("bytes=10-5", 1000), Ignore);
        assert_eq!(parse_range_header("bytes=0-1,5-9", 1000), Ignore);
        assert_eq!(parse_range_header("items=0-1", 1000), Ignore);
        assert_eq!(parse_range_header("bytes=abc-", 1000), Ignore);
    }
}
