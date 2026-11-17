use crate::{
    api::shows::{self, normalize_id},
    config::AppConfig,
    db::DbPool,
};
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::Response,
};
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    path::{Path as FsPath, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{process::Command, sync::Semaphore};
use tokio_util::io::ReaderStream;
use tracing::warn;

/// Limita extrações simultâneas de miniatura (cada uma é um processo ffmpeg).
static FFMPEG_SLOTS: Semaphore = Semaphore::const_new(2);
static RESIZE_SLOTS: Semaphore = Semaphore::const_new(4);
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(30);

/// Tag de cache enviada em `ImageTags`; muda quando o vídeo ou a imagem local mudam.
pub fn image_tag(poster_path: Option<&str>, modified_at: i64) -> String {
    let mut h = DefaultHasher::new();
    poster_path.hash(&mut h);
    modified_at.hash(&mut h);
    format!("{:016x}", h.finish())
}

type ImgState = State<(Arc<AppConfig>, DbPool)>;
type Params = Query<HashMap<String, String>>;

pub async fn get_primary_image(
    State((config, pool)): ImgState,
    Path(item_id): Path<String>,
    Query(q): Params,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    serve_image(&config, &pool, &item_id, ImageKind::Primary, &q, &headers).await
}

/// `/Items/:id/Images/Primary/:index` e variantes com índice.
pub async fn get_primary_image_indexed(
    state: ImgState,
    Path((item_id, _index)): Path<(String, String)>,
    q: Params,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    get_primary_image(state, Path(item_id), q, headers).await
}

pub async fn get_backdrop_image(
    State((config, pool)): ImgState,
    Path(item_id): Path<String>,
    Query(q): Params,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    serve_image(&config, &pool, &item_id, ImageKind::Backdrop, &q, &headers).await
}

pub async fn get_backdrop_image_indexed(
    state: ImgState,
    Path((item_id, _index)): Path<(String, String)>,
    q: Params,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    get_backdrop_image(state, Path(item_id), q, headers).await
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ImageKind {
    Primary,
    Backdrop,
}

type ImageRow = (String, Option<i64>, Option<String>, Option<String>);

fn db_error(e: sqlx::Error) -> StatusCode {
    tracing::error!("Erro de banco de dados: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
}

async fn fetch_image_row(pool: &DbPool, id: &str) -> Result<Option<ImageRow>, StatusCode> {
    sqlx::query_as(
        "SELECT path, duration_ticks, COALESCE(poster_path, tmdb_poster), COALESCE(backdrop_path, tmdb_backdrop) FROM items WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(db_error)
}

async fn serve_image(
    config: &AppConfig,
    pool: &DbPool,
    item_id: &str,
    kind: ImageKind,
    q: &HashMap<String, String>,
    req_headers: &HeaderMap,
) -> Result<Response, StatusCode> {
    // Itens inexistentes e views (sem imagem) devolvem 404.
    let item_id = normalize_id(item_id);
    let mut target = item_id.clone();
    let mut art_override: Option<String> = None;
    let mut row = fetch_image_row(pool, &target).await?;

    // Série/temporada não estão em `items`: usam a arte da série ou o primeiro episódio.
    if row.is_none() {
        let found = shows::resolve_art(pool, &item_id, kind == ImageKind::Backdrop)
            .await
            .map_err(db_error)?;
        if let Some((art, episode_id)) = found {
            art_override = art;
            target = episode_id;
            row = fetch_image_row(pool, &target).await?;
        }
    }
    let (video_path, duration_ticks, poster, backdrop) = row.ok_or(StatusCode::NOT_FOUND)?;

    let local = art_override.or(match kind {
        ImageKind::Primary => poster,
        ImageKind::Backdrop => backdrop,
    });

    let image_path = match local {
        Some(p) if tokio::fs::metadata(&p).await.is_ok() => PathBuf::from(p),
        // Sem imagem local/TMDB: quadro do próprio vídeo (miniatura para Primary, 16:9 largo para Backdrop).
        _ => thumbnail_for(
            config,
            &target,
            FsPath::new(&video_path),
            duration_ticks.unwrap_or(0),
            kind == ImageKind::Backdrop,
        )
        .await
        .ok_or(StatusCode::NOT_FOUND)?,
    };

    // Redimensionamento sob demanda (maxWidth/maxHeight/quality); falha → serve o original.
    let image_path = match ResizeOpts::from_query(q) {
        Some(o) => resized_for(config, &image_path, o).await.unwrap_or(image_path),
        None => image_path,
    };

    let file = tokio::fs::File::open(&image_path)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let meta = file
        .metadata()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let etag = format!("\"{:x}-{:x}\"", len, mtime);

    // Cache condicional: o cliente já tem esta versão.
    let not_modified = req_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"))
        .unwrap_or(false);
    if not_modified {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .header(header::CACHE_CONTROL, "public, max-age=86400")
            .body(Body::empty())
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
    }

    let mime = mime_guess::from_path(&image_path).first_or_octet_stream();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::ETAG, &etag)
        .header(header::CACHE_CONTROL, "public, max-age=86400")
        .body(Body::from_stream(ReaderStream::new(file)))
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ResizeOpts {
    max_w: u32,
    max_h: u32,
    quality: u32,
}

const MAX_DIM: u32 = 4096;

impl ResizeOpts {
    /// Lê `maxWidth/maxHeight/width/height/quality` (nomes sem diferenciar maiúsculas).
    fn from_query(q: &HashMap<String, String>) -> Option<Self> {
        let get = |names: &[&str]| -> Option<u32> {
            q.iter()
                .find(|(k, _)| names.iter().any(|n| k.eq_ignore_ascii_case(n)))
                .and_then(|(_, v)| v.parse::<u32>().ok())
                .filter(|n| *n > 0)
        };
        let max_w = get(&["maxWidth", "width"]).unwrap_or(0).min(MAX_DIM);
        let max_h = get(&["maxHeight", "height"]).unwrap_or(0).min(MAX_DIM);
        if max_w == 0 && max_h == 0 {
            return None;
        }
        let quality = get(&["quality"]).unwrap_or(90).clamp(1, 100);
        Some(Self { max_w, max_h, quality })
    }
}

/// Versão reduzida em cache (`<cache>/resized/<hash>.jpg`); `None` se não for possível.
async fn resized_for(config: &AppConfig, src: &FsPath, o: ResizeOpts) -> Option<PathBuf> {
    let meta = tokio::fs::metadata(src).await.ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut h = DefaultHasher::new();
    (src.to_string_lossy().as_ref(), meta.len(), mtime, o.max_w, o.max_h, o.quality / 5).hash(&mut h);
    let key = h.finish();
    let dir = config.cache_dir.join("resized");
    let out = dir.join(format!("{key:016x}.jpg"));
    if tokio::fs::metadata(&out).await.is_ok() {
        return Some(out);
    }
    tokio::fs::create_dir_all(&dir).await.ok()?;
    let _permit = RESIZE_SLOTS.acquire().await.ok()?;
    if tokio::fs::metadata(&out).await.is_ok() {
        return Some(out);
    }

    let w = if o.max_w == 0 { "iw".to_string() } else { format!("min({},iw)", o.max_w) };
    let hh = if o.max_h == 0 { "ih".to_string() } else { format!("min({},ih)", o.max_h) };
    let vf = format!("scale='{w}':'{hh}':force_original_aspect_ratio=decrease");
    // quality 100 -> q 2, quality 1 -> q 31
    let q = (31 - (o.quality * 29 / 100)).clamp(2, 31);
    let tmp = dir.join(format!("{key:016x}.tmp.jpg"));
    let run = Command::new(crate::transcode::ffmpeg_path())
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args(["-frames:v", "1", "-vf", &vf, "-q:v", &q.to_string()])
        .arg(&tmp)
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(FFMPEG_TIMEOUT, run).await {
        Ok(Ok(out)) if out.status.success() => {}
        _ => {
            let _ = tokio::fs::remove_file(&tmp).await;
            return None;
        }
    }
    tokio::fs::rename(&tmp, &out).await.ok()?;
    Some(out)
}

/// Gera antecipadamente (em segundo plano) o quadro de Primary e Backdrop dos itens sem arte local/TMDB,
/// para que o primeiro acesso do cliente já encontre o arquivo em cache.
pub async fn pregenerate(pool: DbPool, config: Arc<AppConfig>) {
    type Row = (String, String, Option<i64>, Option<String>, Option<String>);
    let rows: Vec<Row> = match sqlx::query_as(
        "SELECT id, path, duration_ticks, COALESCE(poster_path, tmdb_poster), COALESCE(backdrop_path, tmdb_backdrop) FROM items",
    )
    .fetch_all(&pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("Pré-geração de miniaturas: falha ao listar itens: {}", e);
            return;
        }
    };

    let mut tasks = tokio::task::JoinSet::new();
    for (id, path, dur, poster, backdrop) in rows {
        for (art, is_backdrop) in [(poster, false), (backdrop, true)] {
            let has_art = match art {
                Some(p) => tokio::fs::metadata(&p).await.is_ok(),
                None => false,
            };
            if has_art {
                continue;
            }
            let (config, id, path) = (config.clone(), id.clone(), path.clone());
            // O semáforo interno (FFMPEG_SLOTS) limita os ffmpeg simultâneos.
            tasks.spawn(async move {
                let _ = thumbnail_for(&config, &id, FsPath::new(&path), dur.unwrap_or(0), is_backdrop).await;
            });
        }
    }
    while tasks.join_next().await.is_some() {}
}

/// Miniatura em cache (`<cache_dir>/thumbs/<id>.jpg`), gerada com ffmpeg sob demanda.
async fn thumbnail_for(
    config: &AppConfig,
    item_id: &str,
    video: &FsPath,
    duration_ticks: i64,
    backdrop: bool,
) -> Option<PathBuf> {
    // O ID vem da URL: só aceitamos hex para nunca formar caminhos arbitrários.
    if item_id.is_empty() || !item_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let dir = config.cache_dir.join("thumbs");
    let suffix = if backdrop { "_bd" } else { "" };
    let out = dir.join(format!("{item_id}{suffix}.jpg"));
    if tokio::fs::metadata(&out).await.is_ok() {
        return Some(out);
    }
    tokio::fs::create_dir_all(&dir).await.ok()?;

    let _permit = FFMPEG_SLOTS.acquire().await.ok()?;
    // Outra requisição pode ter gerado o arquivo enquanto esperávamos a vaga.
    if tokio::fs::metadata(&out).await.is_ok() {
        return Some(out);
    }

    let (seek, scale) = if backdrop {
        (backdrop_seek_seconds(duration_ticks), "scale=1280:-2")
    } else {
        (thumbnail_seek_seconds(duration_ticks), "scale=-2:720")
    };
    let tmp = dir.join(format!("{item_id}{suffix}.tmp.jpg"));
    let run = Command::new(crate::transcode::ffmpeg_path())
        .args(["-v", "error", "-y", "-ss", &format!("{seek:.1}"), "-i"])
        .arg(video)
        .args(["-frames:v", "1", "-vf", scale, "-q:v", "4"])
        .arg(&tmp)
        .kill_on_drop(true)
        .output();

    let output = match tokio::time::timeout(FFMPEG_TIMEOUT, run).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            warn!("ffmpeg indisponível para miniaturas: {}", e);
            return None;
        }
        Err(_) => {
            warn!("Timeout gerando miniatura de {}", video.display());
            let _ = tokio::fs::remove_file(&tmp).await;
            return None;
        }
    };

    if !output.status.success() || tokio::fs::metadata(&tmp).await.is_err() {
        warn!("ffmpeg falhou ao gerar miniatura de {}", video.display());
        let _ = tokio::fs::remove_file(&tmp).await;
        return None;
    }
    // Escrita atômica: nunca serve um arquivo parcial.
    tokio::fs::rename(&tmp, &out).await.ok()?;
    Some(out)
}

/// Ponto do vídeo usado como miniatura: 10% da duração (máx. 5 min), ou 5s se desconhecida.
pub fn thumbnail_seek_seconds(duration_ticks: i64) -> f64 {
    if duration_ticks <= 0 {
        return 5.0;
    }
    let secs = duration_ticks as f64 / 10_000_000.0;
    (secs * 0.10).clamp(1.0, 300.0).min(secs / 2.0)
}

/// Quadro do backdrop: 30% da duração (máx. 10 min), diferente do usado na miniatura.
pub fn backdrop_seek_seconds(duration_ticks: i64) -> f64 {
    if duration_ticks <= 0 {
        return 8.0;
    }
    let secs = duration_ticks as f64 / 10_000_000.0;
    (secs * 0.30).clamp(1.0, 600.0).min(secs / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backdrop_seek_point() {
        assert_eq!(backdrop_seek_seconds(0), 8.0);
        assert_eq!(backdrop_seek_seconds(100 * 10_000_000), 30.0);
        assert_eq!(backdrop_seek_seconds(10_000_000), 0.5);
        assert_eq!(backdrop_seek_seconds(7200 * 10_000_000), 600.0);
    }

    #[test]
    fn seek_point() {
        assert_eq!(thumbnail_seek_seconds(0), 5.0);
        // 2h → 10% = 720s, limitado a 300s
        assert_eq!(thumbnail_seek_seconds(7200 * 10_000_000), 300.0);
        // 100s → 10s
        assert_eq!(thumbnail_seek_seconds(100 * 10_000_000), 10.0);
        // 1s → nunca passa da metade
        assert_eq!(thumbnail_seek_seconds(10_000_000), 0.5);
    }

    #[test]
    fn resize_opts_parse() {
        let q = |kv: &[(&str, &str)]| {
            kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>()
        };
        assert_eq!(ResizeOpts::from_query(&q(&[])), None);
        assert_eq!(ResizeOpts::from_query(&q(&[("quality", "90")])), None);
        let o = ResizeOpts::from_query(&q(&[("maxwidth", "300"), ("Quality", "80")])).unwrap();
        assert_eq!((o.max_w, o.max_h, o.quality), (300, 0, 80));
        let o = ResizeOpts::from_query(&q(&[("maxHeight", "99999")])).unwrap();
        assert_eq!(o.max_h, MAX_DIM);
        assert_eq!(ResizeOpts::from_query(&q(&[("maxWidth", "0")])), None);
    }

    #[test]
    fn tag_changes_with_inputs() {
        let a = image_tag(Some("/a.jpg"), 1);
        assert_eq!(a, image_tag(Some("/a.jpg"), 1));
        assert_ne!(a, image_tag(Some("/a.jpg"), 2));
        assert_ne!(a, image_tag(None, 1));
    }
}
