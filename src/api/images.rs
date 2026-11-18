use crate::{
    api::{auth::CurrentUser, shows::{self, normalize_id}},
    config::AppConfig,
    db::DbPool,
    users::{self, AuthUser},
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
    CurrentUser(user): CurrentUser,
    Path(item_id): Path<String>,
    Query(q): Params,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    serve_image(&config, &pool, Some(&user), &item_id, ImageKind::Primary, &q, &headers).await
}

/// `/Items/:id/Images/Primary/:index` e variantes com índice.
pub async fn get_primary_image_indexed(
    state: ImgState,
    user: CurrentUser,
    Path((item_id, _index)): Path<(String, String)>,
    q: Params,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    get_primary_image(state, user, Path(item_id), q, headers).await
}

pub async fn get_backdrop_image(
    State((config, pool)): ImgState,
    CurrentUser(user): CurrentUser,
    Path(item_id): Path<String>,
    Query(q): Params,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    serve_image(&config, &pool, Some(&user), &item_id, ImageKind::Backdrop, &q, &headers).await
}

pub async fn get_backdrop_image_indexed(
    state: ImgState,
    user: CurrentUser,
    Path((item_id, _index)): Path<(String, String)>,
    q: Params,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    get_backdrop_image(state, user, Path(item_id), q, headers).await
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

/// Capa de um título para o painel (que não tem token de usuário): a imagem local, a do TMDB ou um
/// quadro do próprio vídeo, com `maxWidth`/`maxHeight` opcionais.
pub(super) async fn capa_para_painel(
    config: &AppConfig,
    pool: &DbPool,
    item_id: &str,
    q: &HashMap<String, String>,
    headers: &HeaderMap,
) -> Result<Response, StatusCode> {
    serve_image(config, pool, None, item_id, ImageKind::Primary, q, headers).await
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
    user: Option<&AuthUser>,
    item_id: &str,
    kind: ImageKind,
    q: &HashMap<String, String>,
    req_headers: &HeaderMap,
) -> Result<Response, StatusCode> {
    // Itens inexistentes e views (sem imagem) devolvem 404.
    let item_id = normalize_id(item_id);
    let mut target = item_id.clone();
    let mut candidates = Vec::new();
    let mut row = fetch_image_row(pool, &target).await?;

    if let (Some(user), true) = (user, row.is_some()) {
        if !users::item_allowed(pool, user, &item_id).await.map_err(db_error)? {
            return Err(StatusCode::NOT_FOUND);
        }
    }

    // Série/temporada não estão em `items`: usam a arte da série ou o primeiro episódio.
    if row.is_none() {
        // Sem usuário (painel), só os itens do catálogo têm imagem.
        let Some(user) = user else {
            return Err(StatusCode::NOT_FOUND);
        };
        let found = shows::resolve_art(pool, user, &item_id, kind == ImageKind::Backdrop)
            .await
            .map_err(db_error)?;
        if let Some((art, episode_id)) = found {
            candidates = art;
            target = episode_id;
            row = fetch_image_row(pool, &target).await?;
        }
    }
    let Some((video_path, duration_ticks, poster, backdrop)) = row else {
        // Pessoa do elenco: foto do TMDB, baixada na primeira requisição e guardada em cache.
        if kind == ImageKind::Primary {
            if let Some(path) = person_photo(config, pool, &item_id).await {
                return respond_file(config, path, q, req_headers).await;
            }
        }
        return Err(StatusCode::NOT_FOUND);
    };

    candidates.extend(match kind {
        ImageKind::Primary => poster,
        ImageKind::Backdrop => backdrop,
    });

    let mut available = None;
    for path in candidates {
        if tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_file()) {
            available = Some(PathBuf::from(path));
            break;
        }
    }
    let image_path = match available {
        Some(path) => path,
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

    respond_file(config, image_path, q, req_headers).await
}

async fn person_photo(config: &AppConfig, pool: &DbPool, id: &str) -> Option<PathBuf> {
    let (image,): (Option<String>,) = sqlx::query_as("SELECT image FROM people WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
        .ok()??;
    crate::metadata::download_image(config, image.as_deref()?, "w185")
        .await
        .map(PathBuf::from)
}

/// Redimensiona (se pedido), aplica ETag/304 e devolve o arquivo de imagem.
async fn respond_file(
    config: &AppConfig,
    image_path: PathBuf,
    q: &HashMap<String, String>,
    req_headers: &HeaderMap,
) -> Result<Response, StatusCode> {
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
        .map_or(0, |d| d.as_nanos());
    let mut tag = DefaultHasher::new();
    (&image_path, len, mtime).hash(&mut tag);
    let etag = format!("\"{:016x}\"", tag.finish());

    // Cache condicional: o cliente já tem esta versão.
    let not_modified = req_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    if not_modified {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, &etag)
            .header(header::CACHE_CONTROL, "private, no-cache")
            .body(Body::empty())
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
    }

    let mime = mime_guess::from_path(&image_path).first_or_octet_stream();
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CONTENT_LENGTH, len.to_string())
        .header(header::ETAG, &etag)
        .header(header::CACHE_CONTROL, "private, no-cache")
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
        .map_or(0, |d| d.as_nanos());
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

    // Um por vez: pedidos de clientes que caem no mesmo semáforo nunca esperam atrás de uma fila longa.
    for (id, path, dur, poster, backdrop) in rows {
        for (art, is_backdrop) in [(poster, false), (backdrop, true)] {
            let has_art = match art {
                Some(p) => tokio::fs::metadata(&p).await.is_ok(),
                None => false,
            };
            if has_art {
                continue;
            }
            let _ = thumbnail_for(&config, &id, FsPath::new(&path), dur.unwrap_or(0), is_backdrop).await;
        }
    }
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
    // A data do vídeo entra no nome: arquivo substituído no mesmo caminho gera miniatura nova.
    let rev = tokio::fs::metadata(video)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let out = dir.join(format!("{item_id}{suffix}-{rev}.jpg"));
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
    let tmp = dir.join(format!("{item_id}{suffix}-{rev}.tmp.jpg"));
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
        let detalhe = String::from_utf8_lossy(&output.stderr);
        warn!("ffmpeg falhou ao gerar miniatura de {}: {}", video.display(), detalhe.trim());
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

    #[tokio::test]
    async fn series_and_season_serve_local_art_before_tmdb() {
        use crate::{libraries::Library, scanner, users::AuthUser};

        let pool = crate::db::init_db("sqlite::memory:").await.unwrap();
        let config = AppConfig::load();
        let root = std::env::temp_dir().join(format!("rf_art_{}", uuid::Uuid::new_v4()));
        let show = root.join("Ghosts (2019)");
        let season = show.join("Season 01");
        let shared = root.join("Mixed");
        std::fs::create_dir_all(&season).unwrap();
        std::fs::create_dir(&shared).unwrap();
        let poster = show.join("Poster.JPG");
        let backdrop = show.join("fanart.png");
        let season_poster = season.join("folder.jpg");
        let tmdb_poster = root.join("tmdb-series.jpg");
        let tmdb_season = root.join("tmdb-season.jpg");
        let tmdb_backdrop = root.join("tmdb-backdrop.jpg");
        for (path, bytes) in [
            (&poster, b"series local".as_slice()),
            (&backdrop, b"backdrop local"),
            (&season_poster, b"season local"),
            (&tmdb_poster, b"series remote"),
            (&tmdb_season, b"season remote"),
            (&tmdb_backdrop, b"backdrop remote"),
            (&shared.join("poster.jpg"), b"shared folder"),
        ] {
            std::fs::write(path, bytes).unwrap();
        }

        // Prepara linhas já inspecionadas para exercitar a varredura incremental sem ffprobe.
        for (id, path) in [
            ("a", season.join("Ghosts S01E01.mkv")),
            ("b", season.join("Ghosts S01E02.mkv")),
            ("c", shared.join("Other Show S01E01.mkv")),
        ] {
            std::fs::write(&path, b"x").unwrap();
            let modified = std::fs::metadata(&path).unwrap().modified().unwrap()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
            sqlx::query("INSERT INTO items (id, name, path, media_type, size_bytes, modified_at, probed)
                         VALUES (?, ?, ?, 'Episode', 1, ?, 1)")
                .bind(id).bind(id).bind(path.to_str().unwrap()).bind(modified)
                .execute(&pool).await.unwrap();
        }
        let library = [Library {
            id: "tv".into(),
            name: "Séries".into(),
            collection_type: "tvshows".into(),
            paths: vec![root.to_str().unwrap().into()],
        }];
        assert_eq!(scanner::run_scan(&pool, &library).await, 3);

        let series_id = scanner::series_id_for("Ghosts", Some(2019));
        let season_id = scanner::season_id_for("Ghosts", Some(2019), 1);
        sqlx::query("INSERT OR REPLACE INTO series (id, poster, backdrop) VALUES (?, ?, ?)")
            .bind(&series_id).bind(tmdb_poster.to_str()).bind(tmdb_backdrop.to_str())
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO season_art (season_id, poster) VALUES (?, ?)")
            .bind(&season_id).bind(tmdb_season.to_str())
            .execute(&pool).await.unwrap();
        let usuario = AuthUser { id: "admin".into(), name: "Admin".into(), is_admin: true, allowed_libraries: None };
        let unrelated = scanner::series_id_for("Other Show", None);
        assert!(shows::resolve_art(&pool, &usuario, &unrelated, false).await.unwrap().unwrap().0.is_empty());

        let serve = |id: &str, kind| {
            let id = id.to_owned();
            let (pool, config, usuario) = (&pool, &config, &usuario);
            async move {
                let result = serve_image(config, pool, Some(usuario), &id, kind, &HashMap::new(), &HeaderMap::new())
                    .await.unwrap();
                axum::body::to_bytes(result.into_body(), 100).await.unwrap()
            }
        };
        assert_eq!(serve(&series_id, ImageKind::Primary).await, "series local");
        assert_eq!(serve(&season_id, ImageKind::Primary).await, "season local");
        assert_eq!(serve(&season_id, ImageKind::Backdrop).await, "backdrop local");

        std::fs::remove_file(&season_poster).unwrap();
        std::fs::remove_file(&backdrop).unwrap();
        assert_eq!(serve(&season_id, ImageKind::Primary).await, "season remote");
        assert_eq!(serve(&series_id, ImageKind::Backdrop).await, "backdrop remote");

        assert_eq!(scanner::run_scan(&pool, &library).await, 3);
        let user = AuthUser { id: "test".into(), name: "Test".into(), is_admin: false, allowed_libraries: None };
        let season_item = shows::get_virtual_item(&config, &pool, &user, &season_id).await.unwrap().unwrap();
        assert_eq!(season_item.image_tags["Primary"], image_tag(tmdb_season.to_str(), 0));
        std::fs::remove_dir_all(root).unwrap();
    }
}
