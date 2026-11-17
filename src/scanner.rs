use crate::{
    db::DbPool,
    libraries::{self, LibRef, Library},
    models::MediaStream,
    parser::{self, Kind, Parsed},
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};
use tokio::process::Command;
use tracing::{error, info, warn};
use uuid::Uuid;
use walkdir::WalkDir;

const VIDEO_EXTENSIONS: &[&str] = &["mkv", "mp4", "avi", "mov", "webm", "m4v", "ts"];

pub struct MediaMetadata {
    pub duration_ticks: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub streams: Vec<MediaStream>,
}

/// ID determinístico (UUID v5 do caminho): reindexar não muda os IDs.
pub fn item_id_for_path(path: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, path.as_bytes())
        .simple()
        .to_string()
}

const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];
/// Nomes de pasta (sem extensão) aceitos como poster/backdrop quando o filme está sozinho na pasta.
const FOLDER_POSTERS: &[&str] = &["poster", "cover", "folder", "movie"];
const FOLDER_BACKDROPS: &[&str] = &["backdrop", "fanart", "background", "landscape"];
const SERIES_POSTERS: &[&str] = &["poster", "cover", "folder"];
const SEASON_POSTERS: &[&str] = &["folder", "poster"];

type FolderArt = HashMap<PathBuf, HashMap<String, PathBuf>>;

/// Arquivo relevante (vídeo, imagem ou legenda) de uma pasta, listado uma única vez por varredura.
struct DirEntryInfo {
    path: PathBuf,
    is_file: bool,
}

/// Conteúdo de cada pasta com vídeos. Sem isto, cada vídeo relistava a própria pasta e fazia um `stat`
/// por entrada (N vídeos × N entradas): em pastas grandes ou em montagens lentas a varredura não terminava.
type DirListings = HashMap<PathBuf, Vec<DirEntryInfo>>;

/// Lista uma pasta sem um `stat` por entrada (só os links simbólicos precisam resolver o destino).
fn list_dir(dir: &Path) -> Vec<DirEntryInfo> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .filter_map(std::result::Result::ok)
        .map(|entry| {
            let is_file = match entry.file_type() {
                Ok(kind) if kind.is_symlink() => entry.path().is_file(),
                Ok(kind) => kind.is_file(),
                Err(_) => false,
            };
            DirEntryInfo { path: entry.path(), is_file }
        })
        .collect()
}

#[derive(Debug, Default, PartialEq, Eq)]
struct LocalAssets {
    poster: Option<String>,
    backdrop: Option<String>,
    subtitle: Option<String>,
    series_poster: Option<String>,
    series_backdrop: Option<String>,
    season_poster: Option<String>,
}

/// Procura imagens e legendas locais ao lado do vídeo (bloqueante: use `spawn_blocking`).
///
/// - `<nome>.jpg`, `<nome>-poster.jpg` → poster; `<nome>-fanart.jpg`, `<nome>-backdrop.jpg` → backdrop
/// - `poster.jpg`/`cover.jpg`/`folder.jpg`... só valem se o vídeo é o único da pasta
///   (evita que todos os filmes de uma pasta "solta" herdem o mesmo poster).
/// - `<nome>.srt` → legenda de texto; o nome base deve coincidir com o do vídeo.
fn find_local_assets(video: &Path, listings: &DirListings) -> LocalAssets {
    let (Some(dir), Some(stem)) = (video.parent(), video.file_stem().and_then(|s| s.to_str())) else {
        return LocalAssets::default();
    };
    let fallback;
    let entries: &[DirEntryInfo] = if let Some(listing) = listings.get(dir) {
        listing
    } else {
        fallback = list_dir(dir);
        &fallback
    };

    let image_stem = stem.to_lowercase();
    let mut videos = 0usize;
    // (prioridade, caminho): menor prioridade vence.
    let mut poster: Option<(u8, PathBuf)> = None;
    let mut backdrop: Option<(u8, PathBuf)> = None;
    let mut folder_poster: Option<(u8, PathBuf)> = None;
    let mut folder_backdrop: Option<(u8, PathBuf)> = None;
    let mut subtitle: Option<(bool, PathBuf)> = None;

    for info in entries {
        let p = &info.path;
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ext.eq_ignore_ascii_case("srt") {
            if p.file_stem().and_then(|s| s.to_str()) == Some(stem) && info.is_file {
                // Prefere a extensão minúscula se houver mais de um arquivo com o mesmo nome.
                let candidate = (ext != "srt", p.clone());
                if subtitle.as_ref().is_none_or(|current| &candidate < current) {
                    subtitle = Some(candidate);
                }
            }
            continue;
        }
        if !info.is_file {
            continue;
        }
        let ext = ext.to_lowercase();
        if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
            videos += 1;
            continue;
        }
        if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        let name = p
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();

        let consider = |slot: &mut Option<(u8, PathBuf)>, prio: u8| {
            if slot.as_ref().is_none_or(|(p0, _)| prio < *p0) {
                *slot = Some((prio, p.clone()));
            }
        };

        if name == format!("{image_stem}-poster") {
            consider(&mut poster, 0);
        } else if name == image_stem {
            consider(&mut poster, 1);
        } else if name == format!("{image_stem}-fanart") || name == format!("{image_stem}-backdrop") {
            consider(&mut backdrop, 0);
        } else if let Some(i) = FOLDER_POSTERS.iter().position(|n| *n == name) {
            consider(&mut folder_poster, i as u8);
        } else if let Some(i) = FOLDER_BACKDROPS.iter().position(|n| *n == name) {
            consider(&mut folder_backdrop, i as u8);
        }
    }

    let alone = videos <= 1;
    let to_s = |o: Option<(u8, PathBuf)>| o.map(|(_, p)| p.to_string_lossy().to_string());
    LocalAssets {
        poster: to_s(poster.or(if alone { folder_poster } else { None })),
        backdrop: to_s(backdrop.or(if alone { folder_backdrop } else { None })),
        subtitle: subtitle.map(|(_, path)| path.to_string_lossy().into_owned()),
        ..Default::default()
    }
}

fn add_folder_art(art: &mut FolderArt, path: &Path) {
    let (Some(dir), Some(stem)) = (path.parent(), path.file_stem().and_then(|s| s.to_str())) else {
        return;
    };
    let name = stem.to_lowercase();
    if SERIES_POSTERS.contains(&name.as_str()) || FOLDER_BACKDROPS.contains(&name.as_str()) {
        let entry = art.entry(dir.to_path_buf()).or_default().entry(name);
        entry
            .and_modify(|current| {
                if path < current.as_path() {
                    *current = path.to_path_buf();
                }
            })
            .or_insert_with(|| path.to_path_buf());
    }
}

fn folder_image(art: &FolderArt, dir: &Path, names: &[&str]) -> Option<String> {
    let entries = art.get(dir)?;
    names.iter().find_map(|&name| entries.get(name)).map(|p| p.to_string_lossy().into_owned())
}

fn normalize_series_key(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Identidade da série: nome normalizado + ano de estreia quando conhecido, para que
/// "Ghosts (2019)" e "Ghosts (2021)" sejam séries distintas.
fn series_key(name: &str, year: Option<i32>) -> String {
    match year {
        Some(y) => format!("{} ({y})", normalize_series_key(name)),
        None => normalize_series_key(name),
    }
}

/// ID determinístico da série (UUID v5): mesmos episódios, mesma série.
pub fn series_id_for(series_name: &str, year: Option<i32>) -> String {
    let key = format!("series:{}", series_key(series_name, year));
    Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()).simple().to_string()
}

pub fn season_id_for(series_name: &str, year: Option<i32>, season: i32) -> String {
    let key = format!("season:{}:{}", series_key(series_name, year), season);
    Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()).simple().to_string()
}

/// Antes de o ano entrar na identidade, o ID dependia só do nome. Ao passar para o ID novo, os dados
/// da série (TMDB) e o poster da temporada são copiados do ID antigo, para não perder o enriquecimento
/// nem refazer as buscas. Só copia se o ano gravado for compatível; o progresso dos usuários fica nos
/// episódios (`playback_state`) e não é afetado.
async fn migrate_legacy_series(
    pool: &DbPool,
    name: &str,
    year: i32,
    season: Option<i32>,
) -> Result<(), sqlx::Error> {
    let old_series = series_id_for(name, None);
    let new_series = series_id_for(name, Some(year));
    sqlx::query(
        "INSERT OR IGNORE INTO series (id, tmdb_id, tmdb_title, overview, rating, genres, year, poster, backdrop) \
         SELECT ?, tmdb_id, tmdb_title, overview, rating, genres, year, poster, backdrop FROM series \
         WHERE id = ? AND (year IS NULL OR year = ?)",
    )
    .bind(&new_series)
    .bind(&old_series)
    .bind(year)
    .execute(pool)
    .await?;
    if let Some(n) = season {
        sqlx::query(
            "INSERT OR IGNORE INTO season_art (season_id, poster) \
             SELECT ?, poster FROM season_art WHERE season_id = ? \
               AND EXISTS (SELECT 1 FROM series WHERE id = ?)",
        )
        .bind(season_id_for(name, Some(year), n))
        .bind(season_id_for(name, None, n))
        .bind(&new_series)
        .execute(pool)
        .await?;
    }
    Ok(())
}

/// Indexa vídeos e arte de pasta na mesma passagem (rode dentro de `spawn_blocking`).
/// Se houve erro de leitura, arquivos não visitados não podem ser tratados como removidos.
fn collect_media_files(dir: &Path, visitadas: &HashSet<PathBuf>) -> (Vec<PathBuf>, FolderArt, DirListings, bool) {
    let mut complete = true;
    let mut files = Vec::new();
    let mut folder_art = FolderArt::new();
    let mut listings = DirListings::new();
    for entry in WalkDir::new(dir).into_iter().filter_entry(|e| !visitadas.contains(e.path())) {
        match entry {
            Ok(e) => {
                let ext = e.path().extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
                let is_video = VIDEO_EXTENSIONS.contains(&ext.as_str());
                let is_image = IMAGE_EXTENSIONS.contains(&ext.as_str());
                let arquivo = e.file_type().is_file();
                if (is_video || is_image || ext == "srt")
                    && (arquivo || (e.file_type().is_symlink() && e.path().is_file()))
                {
                    if let Some(parent) = e.path().parent() {
                        listings
                            .entry(parent.to_path_buf())
                            .or_default()
                            .push(DirEntryInfo { path: e.path().to_path_buf(), is_file: true });
                    }
                }
                if arquivo && is_video {
                    files.push(e.into_path());
                } else if arquivo && is_image {
                    add_folder_art(&mut folder_art, e.path());
                }
            }
            Err(err) => {
                warn!("Erro ao ler {}: {}", err.path().unwrap_or(dir).display(), err);
                complete = false;
            }
        }
    }
    (files, folder_art, listings, complete)
}

/// Associa episódios sem ano só dentro da mesma pasta de série (ou pasta imediata).
/// Anos conflitantes nessa pasta deixam os episódios sem ano no grupo original.
fn completar_anos(arquivos: &mut [(PathBuf, Parsed)]) {
    let pasta_serie = |path: &Path, series: &str, season| {
        parser::artwork_dirs(path, series, season)
            .map(|(pasta, _)| pasta)
            .or_else(|| path.parent())
            .map(Path::to_path_buf)
    };
    let mut anos = HashMap::new();
    for (caminho, item) in arquivos.iter() {
        if let (Kind::Episode { series, season, .. }, Some(ano)) = (&item.kind, item.year) {
            let chave = (pasta_serie(caminho, series, *season), normalize_series_key(series));
            anos.entry(chave)
                .and_modify(|anterior| {
                    if *anterior != Some(ano) {
                        *anterior = None;
                    }
                })
                .or_insert(Some(ano));
        }
    }
    for (caminho, item) in arquivos {
        if let Kind::Episode { series, season, .. } = &item.kind {
            if item.year.is_none() {
                let chave = (pasta_serie(caminho, series, *season), normalize_series_key(series));
                item.year = anos.get(&chave).copied().flatten();
            }
        }
    }
}

/// Arquivos processados em paralelo (`RUSTYFIN_SCAN_CONCURRENCY`; padrão: núcleos, entre 2 e 8).
fn scan_concurrency() -> usize {
    std::env::var("RUSTYFIN_SCAN_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map_or(2, std::num::NonZero::get)
                .clamp(2, 8)
        })
}

/// Varre as pastas de todas as bibliotecas e devolve quantos arquivos de vídeo foram vistos.
pub async fn run_scan(pool: &DbPool, libraries: &[Library]) -> usize {
    info!("Iniciando varredura das pastas de mídia...");

    let mut seen: HashSet<String> = HashSet::new();
    let mut all_dirs_ok = true;
    let mut visitadas = HashSet::new();

    for (dir, refs) in libraries::scan_roots(libraries) {
        // Pasta ausente (drive desmontado, erro de digitação): não cria nada e preserva os itens.
        if !dir.is_dir() {
            warn!("Pasta indisponível, ignorada neste scan: {}", dir.display());
            all_dirs_ok = false;
            continue;
        }

        info!("Varrendo diretório: {}", dir.display());

        let dir_owned = dir.clone();
        let anteriores = visitadas.clone();
        let leitura = tokio::task::spawn_blocking(move || collect_media_files(&dir_owned, &anteriores));
        let (files, folder_art, listings) = match leitura.await {
            Ok((files, folder_art, listings, complete)) => {
                if complete {
                    visitadas.insert(dir.clone());
                } else {
                    warn!("Varredura incompleta de {}: nada será removido neste ciclo.", dir.display());
                    all_dirs_ok = false;
                }
                (files, folder_art, listings)
            }
            Err(e) => {
                error!("Falha na varredura de {}: {}", dir.display(), e);
                all_dirs_ok = false;
                continue;
            }
        };

        // ffprobe domina o custo: processa vários arquivos ao mesmo tempo (limitado).
        let refs = Arc::new(refs);
        let gate = Arc::new(tokio::sync::Semaphore::new(scan_concurrency()));
        let folder_art = Arc::new(folder_art);
        let listings = Arc::new(listings);
        let mut arquivos: Vec<_> = files.into_iter().map(|path| {
            let parsed = parser::parse_path(&path);
            (path, parsed)
        }).collect();
        completar_anos(&mut arquivos);
        let mut tasks = tokio::task::JoinSet::new();
        for (path, parsed) in arquivos {
            // Pastas mais profundas vêm primeiro: o arquivo fica com a biblioteca mais específica.
            if !seen.insert(path.to_string_lossy().to_string()) {
                continue;
            }
            let (pool, refs, gate) = (pool.clone(), refs.clone(), gate.clone());
            let folder_art = folder_art.clone();
            let listings = listings.clone();
            tasks.spawn(async move {
                let Ok(_permit) = gate.acquire_owned().await else {
                    return;
                };
                if let Err(err) = process_media_file(&pool, &path, &parsed, &refs, &folder_art, &listings).await {
                    error!("Erro ao processar {}: {}", path.display(), err);
                }
            });
        }
        while tasks.join_next().await.is_some() {}
    }

    // Só remove órfãos se todas as pastas foram lidas (evita apagar tudo com um drive desmontado).
    if all_dirs_ok {
        if let Err(e) = remove_missing(pool, &seen).await {
            error!("Erro ao remover itens ausentes: {}", e);
        }
    }

    info!("Varredura de mídia concluída ({} arquivos).", seen.len());
    seen.len()
}

async fn remove_missing(
    pool: &DbPool,
    seen: &HashSet<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, path FROM items")
        .fetch_all(pool)
        .await?;

    for (id, path) in rows {
        if !seen.contains(&path) {
            sqlx::query("DELETE FROM items WHERE id = ?").bind(&id).execute(pool).await?;
            sqlx::query("DELETE FROM playback_state WHERE item_id = ?")
                .bind(&id)
                .execute(pool)
                .await?;
            info!("Removido (arquivo ausente): {}", path);
        }
    }
    Ok(())
}

async fn process_media_file(
    pool: &DbPool,
    path: &Path,
    parsed: &Parsed,
    libs: &[LibRef],
    folder_art: &FolderArt,
    listings: &DirListings,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path_str = path.to_string_lossy().to_string();
    let library_id = libraries::pick_library(libs, matches!(parsed.kind, Kind::Episode { .. }));
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("mkv")
        .to_lowercase();

    let metadata = tokio::fs::metadata(path).await?;
    let size_bytes = metadata.len() as i64;
    let modified_at = metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);

    let mut assets = find_local_assets(path, listings);
    if let Kind::Episode { series, season, .. } = &parsed.kind {
        if let Some((show_dir, season_dir)) = parser::artwork_dirs(path, series, *season) {
            assets.series_poster = folder_image(folder_art, show_dir, SERIES_POSTERS);
            assets.series_backdrop = folder_image(folder_art, show_dir, FOLDER_BACKDROPS);
            assets.season_poster = season_dir.and_then(|dir| folder_image(folder_art, dir, SEASON_POSTERS));
        }
    }

    let existing: Option<(String, i64, i64, bool)> =
        sqlx::query_as("SELECT id, size_bytes, modified_at, probed FROM items WHERE path = ?")
            .bind(&path_str)
            .fetch_optional(pool)
            .await?;

    // Atualiza arquivos auxiliares mesmo quando o vídeo já foi inspecionado e não mudou.
    if let Some((_, old_size, old_mtime, probed)) = &existing {
        if *old_size == size_bytes && *old_mtime == modified_at && *probed {
            update_parsed(pool, &path_str, parsed, &assets, library_id.as_deref()).await?;
            return Ok(());
        }
    }

    let item_id = existing.map_or_else(|| item_id_for_path(&path_str), |(id, ..)| id);

    // Sem ffprobe não inventamos codecs: ficam NULL e `probed = 0` (nova tentativa no próximo scan).
    let probed_meta = probe_file(path).await;
    let probed = probed_meta.is_some();
    let media_meta = probed_meta.unwrap_or(MediaMetadata {
        duration_ticks: 0,
        width: None,
        height: None,
        video_codec: None,
        audio_codec: None,
        streams: Vec::new(),
    });
    let streams_json = if media_meta.streams.is_empty() {
        None
    } else {
        serde_json::to_string(&media_meta.streams).ok()
    };

    sqlx::query(
        r"
        INSERT INTO items (id, name, path, media_type, container, duration_ticks, size_bytes,
                           width, height, video_codec, audio_codec, modified_at, probed, streams_json)
        VALUES (?, ?, ?, 'Movie', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(path) DO UPDATE SET
            container = excluded.container,
            size_bytes = excluded.size_bytes,
            duration_ticks = excluded.duration_ticks,
            width = excluded.width,
            height = excluded.height,
            video_codec = excluded.video_codec,
            audio_codec = excluded.audio_codec,
            modified_at = excluded.modified_at,
            probed = excluded.probed,
            streams_json = excluded.streams_json
        ",
    )
    .bind(&item_id)
    .bind(&parsed.title)
    .bind(&path_str)
    .bind(&ext)
    .bind(media_meta.duration_ticks)
    .bind(size_bytes)
    .bind(media_meta.width)
    .bind(media_meta.height)
    .bind(media_meta.video_codec)
    .bind(media_meta.audio_codec)
    .bind(modified_at)
    .bind(probed)
    .bind(streams_json)
    .execute(pool)
    .await?;

    update_parsed(pool, &path_str, parsed, &assets, library_id.as_deref()).await?;

    info!("Indexado: {} [{}]", parsed.title, item_id);
    Ok(())
}

/// Campos derivados do nome do arquivo e dos arquivos auxiliares (recalculados a cada scan).
async fn update_parsed(
    pool: &DbPool,
    path: &str,
    parsed: &Parsed,
    assets: &LocalAssets,
    library_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    let (kind, series, season, episode) = match &parsed.kind {
        Kind::Movie => ("Movie", None, None, None),
        Kind::Episode { series, season, episode } => {
            ("Episode", Some(series.as_str()), Some(*season), Some(*episode))
        }
    };
    let series_id = series.map(|s| series_id_for(s, parsed.year));
    let season_id = series.zip(season).map(|(s, n)| season_id_for(s, parsed.year, n));
    if let (Some(name), Some(year)) = (series, parsed.year) {
        migrate_legacy_series(pool, name, year, season).await?;
    }
    sqlx::query(
        r"
        UPDATE items SET
            name = ?, media_type = ?, production_year = COALESCE(?, production_year),
            series_name = ?, season_number = ?, episode_number = ?,
            series_id = ?, season_id = ?,
            poster_path = ?, backdrop_path = ?, subtitle_path = ?, library_id = ?,
            series_poster_path = ?, series_backdrop_path = ?, season_poster_path = ?
        WHERE path = ?
        ",
    )
    .bind(&parsed.title)
    .bind(kind)
    .bind(parsed.year)
    .bind(series)
    .bind(season)
    .bind(episode)
    .bind(series_id)
    .bind(season_id)
    .bind(&assets.poster)
    .bind(&assets.backdrop)
    .bind(&assets.subtitle)
    .bind(library_id)
    .bind(&assets.series_poster)
    .bind(&assets.series_backdrop)
    .bind(&assets.season_poster)
    .bind(path)
    .execute(pool)
    .await?;
    Ok(())
}

async fn probe_file(path: &Path) -> Option<MediaMetadata> {
    let output = Command::new("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    parse_ffprobe(&output.stdout)
}

fn parse_ffprobe(stdout: &[u8]) -> Option<MediaMetadata> {
    let json: serde_json::Value = serde_json::from_slice(stdout).ok()?;

    let duration_secs = json
        .get("format")
        .and_then(|f| f.get("duration"))
        .and_then(|d| d.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0);

    // Jellyfin: 1 segundo = 10.000.000 ticks
    let duration_ticks = (duration_secs * 10_000_000.0) as i64;

    let mut video_codec = None;
    let mut audio_codec = None;
    let mut width = None;
    let mut height = None;
    let mut streams = Vec::new();

    if let Some(list) = json.get("streams").and_then(|s| s.as_array()) {
        for (pos, stream) in list.iter().enumerate() {
            let codec_type = stream.get("codec_type").and_then(|t| t.as_str()).unwrap_or("");
            let kind = match codec_type {
                "video" => "Video",
                "audio" => "Audio",
                "subtitle" => "Subtitle",
                _ => continue, // attachments/data não são expostos
            };
            // Capas embutidas (mjpeg/png) aparecem como "video" com disposition attached_pic.
            let is_pic = stream
                .pointer("/disposition/attached_pic")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
                == 1;
            if kind == "Video" && is_pic {
                continue;
            }

            let codec = stream
                .get("codec_name")
                .and_then(|c| c.as_str())
                .unwrap_or("unknown")
                .to_string();
            let index = stream
                .get("index")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(pos as i64) as i32;
            let w = stream.get("width").and_then(serde_json::Value::as_i64).map(|v| v as i32);
            let h = stream.get("height").and_then(serde_json::Value::as_i64).map(|v| v as i32);
            let channels = stream.get("channels").and_then(serde_json::Value::as_i64).map(|v| v as i32);
            let sample_rate = stream
                .get("sample_rate")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<i32>().ok());
            let language = stream
                .pointer("/tags/language")
                .and_then(|v| v.as_str())
                .map(String::from);
            let title = stream.pointer("/tags/title").and_then(|v| v.as_str());
            let is_forced = stream
                .pointer("/disposition/forced")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
                == 1;
            let is_default = stream
                .pointer("/disposition/default")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
                == 1;

            if kind == "Video" && video_codec.is_none() {
                video_codec = Some(codec.clone());
                width = w;
                height = h;
            } else if kind == "Audio" && audio_codec.is_none() {
                audio_codec = Some(codec.clone());
            }

            let display_title = Some(match (title, &language) {
                (Some(t), _) => t.to_string(),
                (None, Some(l)) => format!("{l} ({codec})"),
                (None, None) => codec.clone(),
            });

            streams.push(MediaStream {
                width: w,
                height: h,
                channels,
                sample_rate,
                color_transfer: stream.get("color_transfer").and_then(|v| v.as_str()).map(String::from),
                color_primaries: stream.get("color_primaries").and_then(|v| v.as_str()).map(String::from),
                color_space: stream.get("color_space").and_then(|v| v.as_str()).map(String::from),
                display_title,
                language,
                is_default,
                title: title.map(String::from),
                is_forced,
                ..MediaStream::basic(&codec, kind, index)
            });
        }
    }

    Some(MediaMetadata {
        duration_ticks,
        width,
        height,
        video_codec,
        audio_codec,
        streams,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completa_ano_so_na_mesma_pasta_e_sem_conflito() {
        let caminhos = [
            "/media/a/Ghosts/Season 1/Ghosts (2019) S01E01.mkv",
            "/media/a/Ghosts/Season 2/Ghosts S02E01.mkv",
            "/media/b/Ghosts/Season 1/Ghosts (2021) S01E01.mkv",
            "/media/b/Ghosts/Season 1/Ghosts S01E02.mkv",
            "/media/c/Ghosts/Ghosts (2019) S01E01.mkv",
            "/media/c/Ghosts/Ghosts (2021) S01E02.mkv",
            "/media/c/Ghosts/Ghosts S01E03.mkv",
            "/media/d/Ghosts/Ghosts S01E01.mkv",
        ];
        let mut itens: Vec<_> = caminhos.iter().map(|caminho| {
            let path = PathBuf::from(caminho);
            let parsed = parser::parse_path(&path);
            (path, parsed)
        }).collect();
        completar_anos(&mut itens);
        let anos: Vec<_> = itens.into_iter().map(|(_, item)| item.year).collect();
        assert_eq!(anos, [Some(2019), Some(2019), Some(2021), Some(2021),
            Some(2019), Some(2021), None, None]);
    }

    #[test]
    fn finds_local_images() {
        let dir = std::env::temp_dir().join(format!("rf_img_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let touch = |n: &str| std::fs::write(dir.join(n), b"x").unwrap();
        touch("Filme.mkv");
        touch("Poster.JPG");
        touch("fanart.png");
        let r = find_local_assets(&dir.join("Filme.mkv"), &DirListings::new());
        assert!(r.poster.unwrap().to_lowercase().ends_with("poster.jpg"));
        assert!(r.backdrop.unwrap().ends_with("fanart.png"));

        // Com dois vídeos, imagens de pasta não são herdadas; imagens por nome sim.
        touch("Outro.mkv");
        touch("Filme-poster.jpg");
        let r = find_local_assets(&dir.join("Filme.mkv"), &DirListings::new());
        assert!(r.poster.unwrap().ends_with("Filme-poster.jpg"));
        assert_eq!(r.backdrop, None);
        assert_eq!(find_local_assets(&dir.join("Outro.mkv"), &DirListings::new()), LocalAssets::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn incremental_scan_refreshes_same_name_subtitles() {
        let pool = crate::db::init_db("sqlite::memory:").await.unwrap();
        let dir = std::env::temp_dir().join(format!("rf_sub_{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let video = dir.join("Filme.mkv");
        let subtitle = dir.join("Filme.SRT");
        std::fs::write(&video, b"x").unwrap();
        std::fs::write(dir.join("Outro.srt"), b"x").unwrap();
        let path = video.to_str().unwrap();
        let modified_at = std::fs::metadata(&video)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        sqlx::query("INSERT INTO items (id, name, path, media_type, size_bytes, modified_at, probed) VALUES ('old', 'Filme', ?, 'Movie', 1, ?, 1)")
            .bind(path)
            .bind(modified_at)
            .execute(&pool)
            .await
            .unwrap();
        let read_subtitle = || {
            sqlx::query_scalar::<_, Option<String>>("SELECT subtitle_path FROM items WHERE id = 'old'")
                .fetch_one(&pool)
        };

        process_media_file(&pool, &video, &parser::parse_path(&video), &[], &FolderArt::new(), &DirListings::new()).await.unwrap();
        assert_eq!(read_subtitle().await.unwrap(), None);

        std::fs::write(&subtitle, b"x").unwrap();
        process_media_file(&pool, &video, &parser::parse_path(&video), &[], &FolderArt::new(), &DirListings::new()).await.unwrap();
        assert_eq!(read_subtitle().await.unwrap().as_deref(), subtitle.to_str());

        std::fs::remove_file(&subtitle).unwrap();
        process_media_file(&pool, &video, &parser::parse_path(&video), &[], &FolderArt::new(), &DirListings::new()).await.unwrap();
        assert_eq!(read_subtitle().await.unwrap(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn series_ids_ignore_case_and_spacing() {
        assert_eq!(series_id_for("Dark", None), series_id_for("  dark ", None));
        assert_ne!(series_id_for("Dark", None), series_id_for("Lost", None));
        assert_ne!(season_id_for("Dark", None, 1), season_id_for("Dark", None, 2));
        assert_ne!(series_id_for("Dark", None), season_id_for("Dark", None, 1));
        // Mesmo nome, anos diferentes = séries diferentes (e temporadas diferentes).
        assert_ne!(series_id_for("Ghosts", Some(2019)), series_id_for("Ghosts", Some(2021)));
        assert_ne!(series_id_for("Ghosts", Some(2019)), series_id_for("Ghosts", None));
        assert_ne!(season_id_for("Ghosts", Some(2019), 1), season_id_for("Ghosts", Some(2021), 1));
    }

    #[test]
    fn ids_are_deterministic_and_32_hex() {
        let a = item_id_for_path("/media/a.mkv");
        assert_eq!(a, item_id_for_path("/media/a.mkv"));
        assert_ne!(a, item_id_for_path("/media/b.mkv"));
        assert_eq!(a.len(), 32);
    }

    #[test]
    fn parses_ffprobe_json() {
        let json = br#"{"format":{"duration":"10.5"},"streams":[
            {"codec_type":"video","codec_name":"hevc","width":1920,"height":1080},
            {"codec_type":"audio","codec_name":"dts"}]}"#;
        let m = parse_ffprobe(json).unwrap();
        assert_eq!(m.duration_ticks, 105_000_000);
        assert_eq!(m.video_codec.as_deref(), Some("hevc"));
        assert_eq!(m.audio_codec.as_deref(), Some("dts"));
        assert_eq!(m.height, Some(1080));
        assert_eq!(m.streams.len(), 2);
    }

    #[test]
    fn parses_tracks_languages_and_skips_cover_art() {
        let json = br#"{"format":{"duration":"1"},"streams":[
            {"index":0,"codec_type":"video","codec_name":"h264","width":1280,"height":720},
            {"index":1,"codec_type":"audio","codec_name":"aac","channels":6,"sample_rate":"48000",
             "tags":{"language":"por"},"disposition":{"default":1}},
            {"index":2,"codec_type":"audio","codec_name":"ac3","tags":{"language":"eng"}},
            {"index":3,"codec_type":"subtitle","codec_name":"ass","tags":{"language":"eng","title":"Full"}},
            {"index":4,"codec_type":"video","codec_name":"mjpeg","disposition":{"attached_pic":1}},
            {"index":5,"codec_type":"attachment","codec_name":"ttf"}]}"#;
        let m = parse_ffprobe(json).unwrap();
        assert_eq!(m.streams.len(), 4);
        assert_eq!(m.streams[1].channels, Some(6));
        assert_eq!(m.streams[1].language.as_deref(), Some("por"));
        assert!(m.streams[1].is_default);
        assert_eq!(m.streams[3].r#type, "Subtitle");
        assert_eq!(m.streams[3].display_title.as_deref(), Some("Full"));
    }
}
