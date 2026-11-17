//! Rotas de remux/transcode: stream progressivo e HLS (`master.m3u8`, `main.m3u8`, segmentos).

use crate::{
    api::{auth::{CurrentCreds, CurrentUser}, shows::normalize_id},
    db::DbPool,
    transcode::{self, Container, SpawnError, TranscodeParams},
    users::{self, AuthUser},
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
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
    duration_ticks: Option<i64>,
}

async fn item_input(pool: &DbPool, user: &AuthUser, id: &str) -> Result<ItemInput, StatusCode> {
    let id = normalize_id(id);
    if !users::item_allowed(pool, user, &id).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        return Err(StatusCode::NOT_FOUND);
    }
    sqlx::query_as("SELECT path, streams_json, audio_codec IS NOT NULL AS has_audio, duration_ticks FROM items WHERE id = ?")
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

/// Usa o token da sessão nas URLs porque players HLS podem omitir os cabeçalhos.
fn token_url(token: Option<&str>) -> Result<String, StatusCode> {
    let token = token.ok_or(StatusCode::UNAUTHORIZED)?;
    serde_urlencoded::to_string([("api_key", token)]).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

fn autorizar_segmentos(playlist: &str, prefixo: &str, query: &str) -> String {
    let mut resposta = String::with_capacity(playlist.len());
    for linha in playlist.lines() {
        if linha.starts_with(prefixo) {
            resposta.push_str(&format!("{linha}?{query}"));
        } else if let Some(uri) = linha.strip_prefix("#EXT-X-MAP:URI=\"")
            .and_then(|uri| uri.strip_suffix('"'))
            .filter(|uri| uri.starts_with(prefixo))
        {
            resposta.push_str(&format!("#EXT-X-MAP:URI=\"{uri}?{query}\""));
        } else {
            resposta.push_str(linha);
        }
        resposta.push('\n');
    }
    resposta
}

/// Dá a cada item um espaço de sessões próprio, inclusive para requisições sem playlist.
fn chave_interna(id: &str, chave: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, format!("rustyfin:hls:{id}:{chave}").as_bytes())
        .simple()
        .to_string()
}

/// A mesma query nas variantes permite compartilhar a sessão `FFmpeg`.
pub async fn hls_master(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    CurrentCreds(creds): CurrentCreds,
    Path(id): Path<String>,
    Query(mut q): Query<HashMap<String, String>>,
) -> Result<Response, StatusCode> {
    let params = TranscodeParams::from_query(&q, Container::Ts, true).ok_or(StatusCode::BAD_REQUEST)?;
    item_input(&pool, &user, &id).await?;
    remover_tokens(&mut q);
    let query = serde_urlencoded::to_string(&q).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let token = token_url(creds.token.as_deref())?;
    let query = if query.is_empty() { token } else { format!("{query}&{token}") };
    let with_audio = |video: i64| video.saturating_add(transcode::AUDIO_BITRATE);
    let bandwidth = params.abr.map(|ladder| ladder.high).or(params.video_bitrate).map_or(8_000_000, with_audio);
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
    CurrentCreds(creds): CurrentCreds,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, StatusCode> {
    hls_playlist(pool, user, id, q, token_url(creds.token.as_deref())?, false).await
}

pub async fn hls_low(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    CurrentCreds(creds): CurrentCreds,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, StatusCode> {
    hls_playlist(pool, user, id, q, token_url(creds.token.as_deref())?, true).await
}

fn remover_tokens(query: &mut HashMap<String, String>) {
    query.retain(|chave, _| !["api_key", "apikey", "token"].iter()
        .any(|nome| chave.eq_ignore_ascii_case(nome)));
}

async fn hls_playlist(
    pool: DbPool,
    user: AuthUser,
    id: String,
    mut q: HashMap<String, String>,
    credencial: String,
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
    remover_tokens(&mut q);
    let chave = transcode::session_key(&id, &q, params.tone_map_hdr);
    let prefixo = format!("/Videos/{id}/hls/{chave}/");
    let key = chave_interna(&id, &chave);

    // Transcodificação completa: playlist VOD com todos os segmentos (o cliente pode avançar para
    // qualquer ponto) e segmentos gerados sob demanda. Com vídeo copiado os cortes dependem dos
    // quadros-chave do arquivo e não dá para prever; esses seguem com a playlist que cresce.
    let duration_secs = input.duration_ticks.unwrap_or(0) as f64 / 10_000_000.0;
    if !params.video_copy && duration_secs > 0.0 {
        let count = (duration_secs / f64::from(transcode::HLS_SEGMENT_SECS)).ceil().max(1.0) as u32;
        let start_seg = ((params.start_secs / f64::from(transcode::HLS_SEGMENT_SECS)) as u32).min(count - 1);
        transcode::start_vod(&key, std::path::Path::new(&input.path), &params, start_seg)
            .await
            .map_err(spawn_status)?;
        let rendition = match (params.abr, low) {
            (Some(_), true) => Some("low"),
            (Some(_), false) => Some("high"),
            (None, _) => None,
        };
        let playlist = transcode::vod_playlist(
            duration_secs, &prefixo, rendition, params.container == Container::Mp4,
        );
        return text_response(PLAYLIST_MIME, autorizar_segmentos(&playlist, &prefixo, &credencial));
    }

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
        autorizar_segmentos(&transcode::rewrite_playlist(&content, &prefixo), &prefixo, &credencial),
    )
}

/// Segmento MPEG-TS ou fMP4, ou arquivo de inicialização fMP4.
pub async fn hls_segment(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path((id, key, file)): Path<(String, String, String)>,
) -> Result<Response, StatusCode> {
    if !transcode::is_valid_key(&key) || file == "main.m3u8" {
        return Err(StatusCode::NOT_FOUND);
    }
    let mime = transcode::hls_file_mime(&file).ok_or(StatusCode::NOT_FOUND)?;
    item_input(&pool, &user, &id).await?;
    let chave = chave_interna(&normalize_id(&id), &key);
    let path = transcode::ensure_segment(&chave, &file, SEGMENT_TIMEOUT)
        .await
        .map_err(spawn_status)?
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


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_autentica_segmentos_e_inicializacao() {
        let prefixo = "/Videos/abc/hls/chave/";
        let playlist = format!(
            "#EXTM3U\n#EXT-X-MAP:URI=\"{prefixo}init.mp4\"\n{prefixo}seg_00000.m4s\n"
        );
        let query = token_url(Some("a+b&c")).unwrap();
        let protegida = autorizar_segmentos(&playlist, prefixo, &query);
        assert!(protegida.contains("init.mp4?api_key=a%2Bb%26c\""));
        assert!(protegida.contains("seg_00000.m4s?api_key=a%2Bb%26c\n"));
        assert_ne!(chave_interna("abc", "chave"), chave_interna("outro", "chave"));
    }
}
