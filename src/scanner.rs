use crate::{
    db::DbPool,
    libraries::{self, LibRef, Library},
    models::MediaStream,
    parser::{self, Kind, Parsed},
};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
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

#[derive(Debug, Default, PartialEq, Eq)]
pub struct LocalImages {
    pub poster: Option<String>,
    pub backdrop: Option<String>,
}

/// Procura imagens locais ao lado do vídeo (bloqueante: use `spawn_blocking`).
///
/// - `<nome>.jpg`, `<nome>-poster.jpg` → poster; `<nome>-fanart.jpg`, `<nome>-backdrop.jpg` → backdrop
/// - `poster.jpg`/`cover.jpg`/`folder.jpg`... só valem se o vídeo é o único da pasta
///   (evita que todos os filmes de uma pasta "solta" herdem o mesmo poster).
pub fn find_local_images(video: &Path) -> LocalImages {
    let (Some(dir), Some(stem)) = (video.parent(), video.file_stem().and_then(|s| s.to_str())) else {
        return LocalImages::default();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return LocalImages::default();
    };

    let stem = stem.to_lowercase();
    let mut videos = 0usize;
    // (prioridade, caminho): menor prioridade vence.
    let mut poster: Option<(u8, PathBuf)> = None;
    let mut backdrop: Option<(u8, PathBuf)> = None;
    let mut folder_poster: Option<(u8, PathBuf)> = None;
    let mut folder_backdrop: Option<(u8, PathBuf)> = None;

    for entry in entries.filter_map(|e| e.ok()) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
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
            if slot.as_ref().map(|(p0, _)| prio < *p0).unwrap_or(true) {
                *slot = Some((prio, p.clone()));
            }
        };

        if name == format!("{stem}-poster") {
            consider(&mut poster, 0);
        } else if name == stem {
            consider(&mut poster, 1);
        } else if name == format!("{stem}-fanart") || name == format!("{stem}-backdrop") {
            consider(&mut backdrop, 0);
        } else if let Some(i) = FOLDER_POSTERS.iter().position(|n| *n == name) {
            consider(&mut folder_poster, i as u8);
        } else if let Some(i) = FOLDER_BACKDROPS.iter().position(|n| *n == name) {
            consider(&mut folder_backdrop, i as u8);
        }
    }

    let alone = videos <= 1;
    let to_s = |o: Option<(u8, PathBuf)>| o.map(|(_, p)| p.to_string_lossy().to_string());
    LocalImages {
        poster: to_s(poster.or(if alone { folder_poster } else { None })),
        backdrop: to_s(backdrop.or(if alone { folder_backdrop } else { None })),
    }
}

fn normalize_series_key(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// ID determinístico da série (UUID v5 do nome normalizado): mesmos episódios, mesma série.
pub fn series_id_for(series_name: &str) -> String {
    let key = format!("series:{}", normalize_series_key(series_name));
    Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()).simple().to_string()
}

pub fn season_id_for(series_name: &str, season: i32) -> String {
    let key = format!("season:{}:{}", normalize_series_key(series_name), season);
    Uuid::new_v5(&Uuid::NAMESPACE_URL, key.as_bytes()).simple().to_string()
}

/// Lista arquivos de vídeo (bloqueante: rode dentro de `spawn_blocking`).
fn collect_video_files(dir: &Path) -> Vec<PathBuf> {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .map(|e| e.into_path())
        .collect()
}

/// Arquivos processados em paralelo (`RUSTYFIN_SCAN_CONCURRENCY`; padrão: núcleos, entre 2 e 8).
fn scan_concurrency() -> usize {
    std::env::var("RUSTYFIN_SCAN_CONCURRENCY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2)
                .clamp(2, 8)
        })
}

/// Varre as pastas de todas as bibliotecas e devolve quantos arquivos de vídeo foram vistos.
pub async fn run_scan(pool: &DbPool, libraries: &[Library]) -> usize {
    info!("Iniciando varredura das pastas de mídia...");

    let mut seen: HashSet<String> = HashSet::new();
    let mut all_dirs_ok = true;

    for (dir, refs) in libraries::scan_roots(libraries) {
        // Pasta ausente (drive desmontado, erro de digitação): não cria nada e preserva os itens.
        if !dir.is_dir() {
            warn!("Pasta indisponível, ignorada neste scan: {}", dir.display());
            all_dirs_ok = false;
            continue;
        }

        info!("Varrendo diretório: {}", dir.display());

        let dir_owned = dir.clone();
        let files = match tokio::task::spawn_blocking(move || collect_video_files(&dir_owned)).await {
            Ok(f) => f,
            Err(e) => {
                error!("Falha na varredura de {}: {}", dir.display(), e);
                all_dirs_ok = false;
                continue;
            }
        };

        // ffprobe domina o custo: processa vários arquivos ao mesmo tempo (limitado).
        let refs = std::sync::Arc::new(refs);
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(scan_concurrency()));
        let mut tasks = tokio::task::JoinSet::new();
        for path in files {
            // Pastas mais profundas vêm primeiro: o arquivo fica com a biblioteca mais específica.
            if !seen.insert(path.to_string_lossy().to_string()) {
                continue;
            }
            let (pool, refs, gate) = (pool.clone(), refs.clone(), gate.clone());
            tasks.spawn(async move {
                let Ok(_permit) = gate.acquire_owned().await else {
                    return;
                };
                if let Err(err) = process_media_file(&pool, &path, &refs).await {
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
    libs: &[LibRef],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let path_str = path.to_string_lossy().to_string();
    let parsed = parser::parse_path(path);
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
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let images = {
        let p = path.to_path_buf();
        tokio::task::spawn_blocking(move || find_local_images(&p))
            .await
            .unwrap_or_default()
    };

    let existing: Option<(String, i64, i64, bool)> =
        sqlx::query_as("SELECT id, size_bytes, modified_at, probed FROM items WHERE path = ?")
            .bind(&path_str)
            .fetch_optional(pool)
            .await?;

    // Scan incremental: arquivo inalterado e já inspecionado → só atualiza nome/imagens (baratos).
    if let Some((_, old_size, old_mtime, probed)) = &existing {
        if *old_size == size_bytes && *old_mtime == modified_at && *probed {
            update_parsed(pool, &path_str, &parsed, &images, library_id.as_deref()).await?;
            return Ok(());
        }
    }

    let item_id = existing
        .map(|(id, ..)| id)
        .unwrap_or_else(|| item_id_for_path(&path_str));

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
        r#"
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
        "#,
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

    update_parsed(pool, &path_str, &parsed, &images, library_id.as_deref()).await?;

    info!("Indexado: {} [{}]", parsed.title, item_id);
    Ok(())
}

/// Campos derivados do nome do arquivo e das imagens locais (recalculados a cada scan).
async fn update_parsed(
    pool: &DbPool,
    path: &str,
    parsed: &Parsed,
    images: &LocalImages,
    library_id: Option<&str>,
) -> Result<(), sqlx::Error> {
    let (kind, series, season, episode) = match &parsed.kind {
        Kind::Movie => ("Movie", None, None, None),
        Kind::Episode { series, season, episode } => {
            ("Episode", Some(series.as_str()), Some(*season), Some(*episode))
        }
    };
    let series_id = series.map(series_id_for);
    let season_id = series.zip(season).map(|(s, n)| season_id_for(s, n));
    sqlx::query(
        r#"
        UPDATE items SET
            name = ?, media_type = ?, production_year = COALESCE(?, production_year),
            series_name = ?, season_number = ?, episode_number = ?,
            series_id = ?, season_id = ?,
            poster_path = ?, backdrop_path = ?, library_id = ?
        WHERE path = ?
        "#,
    )
    .bind(&parsed.title)
    .bind(kind)
    .bind(parsed.year)
    .bind(series)
    .bind(season)
    .bind(episode)
    .bind(series_id)
    .bind(season_id)
    .bind(&images.poster)
    .bind(&images.backdrop)
    .bind(library_id)
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
                .and_then(|v| v.as_i64())
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
                .and_then(|i| i.as_i64())
                .unwrap_or(pos as i64) as i32;
            let w = stream.get("width").and_then(|v| v.as_i64()).map(|v| v as i32);
            let h = stream.get("height").and_then(|v| v.as_i64()).map(|v| v as i32);
            let channels = stream.get("channels").and_then(|v| v.as_i64()).map(|v| v as i32);
            let sample_rate = stream
                .get("sample_rate")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<i32>().ok());
            let language = stream
                .pointer("/tags/language")
                .and_then(|v| v.as_str())
                .map(String::from);
            let title = stream.pointer("/tags/title").and_then(|v| v.as_str());
            let is_default = stream
                .pointer("/disposition/default")
                .and_then(|v| v.as_i64())
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
                (None, Some(l)) => format!("{} ({})", l, codec),
                (None, None) => codec.clone(),
            });

            streams.push(MediaStream {
                codec,
                language,
                r#type: kind.to_string(),
                index,
                is_default,
                width: w,
                height: h,
                channels,
                sample_rate,
                display_title,
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
    fn finds_local_images() {
        let dir = std::env::temp_dir().join(format!("rf_img_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let touch = |n: &str| std::fs::write(dir.join(n), b"x").unwrap();
        touch("Filme.mkv");
        touch("Poster.JPG");
        touch("fanart.png");
        let r = find_local_images(&dir.join("Filme.mkv"));
        assert!(r.poster.unwrap().to_lowercase().ends_with("poster.jpg"));
        assert!(r.backdrop.unwrap().ends_with("fanart.png"));

        // Com dois vídeos, imagens de pasta não são herdadas; imagens por nome sim.
        touch("Outro.mkv");
        touch("Filme-poster.jpg");
        let r = find_local_images(&dir.join("Filme.mkv"));
        assert!(r.poster.unwrap().ends_with("Filme-poster.jpg"));
        assert_eq!(r.backdrop, None);
        assert_eq!(find_local_images(&dir.join("Outro.mkv")), LocalImages::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn series_ids_ignore_case_and_spacing() {
        assert_eq!(series_id_for("Dark"), series_id_for("  dark "));
        assert_ne!(series_id_for("Dark"), series_id_for("Lost"));
        assert_ne!(season_id_for("Dark", 1), season_id_for("Dark", 2));
        assert_ne!(series_id_for("Dark"), season_id_for("Dark", 1));
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
