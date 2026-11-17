use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub server_id: String,
    pub server_name: String,
    pub version: String,
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub media_directories: Vec<PathBuf>,
    /// Cache de miniaturas geradas via ffmpeg.
    pub cache_dir: PathBuf,
    /// Intervalo do scan automático em segundos (0 = só no início e via /Library/Refresh).
    pub scan_interval_secs: u64,
    /// Monitora as pastas por eventos do sistema de arquivos (além do polling).
    pub watch_enabled: bool,
    /// Responde à descoberta automática (UDP 7359).
    pub discovery_enabled: bool,
    /// Se definida, o painel `/admin` exige essa senha (HTTP Basic, qualquer usuário).
    pub admin_password: Option<String>,
    /// Endereço HTTP anunciado na descoberta (obrigatório na prática em Docker).
    pub public_url: Option<String>,
    /// Executável do ffmpeg (miniaturas, remux e transcode).
    pub ffmpeg_path: String,
    /// Máximo de processos de transcodificação simultâneos.
    pub max_transcodes: usize,
    /// Encoder de vídeo: `libx264` (padrão) ou de hardware (`h264_nvenc`, `h264_qsv`, `h264_videotoolbox`).
    pub video_encoder: String,
    /// Chave v3 (32 chars) ou token v4 do TMDB. Sem ela, nenhum metadado online é buscado.
    pub tmdb_api_key: Option<String>,
    pub tmdb_language: String,
    pub tmdb_base_url: String,
    pub tmdb_image_base: String,
}

impl AppConfig {
    pub fn load() -> Self {
        let server_id = std::env::var("RUSTYFIN_SERVER_ID")
            .unwrap_or_else(|_| "a1b2c3d4e5f6478980123456789abcdef".to_string());
        
        let server_name = std::env::var("RUSTYFIN_SERVER_NAME")
            .unwrap_or_else(|_| "Rustyfin Local".to_string());
        
        let version = "10.9.0".to_string();
        
        let host = std::env::var("RUSTYFIN_HOST")
            .unwrap_or_else(|_| "0.0.0.0".to_string());
        
        let port = std::env::var("RUSTYFIN_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(8096);
        
        let database_url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "sqlite://rustyfin.db?mode=rwc".to_string());

        let cache_dir = std::env::var("RUSTYFIN_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./cache"));

        let scan_interval_secs = std::env::var("RUSTYFIN_SCAN_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300);
        let watch_enabled = std::env::var("RUSTYFIN_WATCH")
            .map(|v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"))
            .unwrap_or(true);
        let ffmpeg_path = std::env::var("RUSTYFIN_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
        let max_transcodes = std::env::var("RUSTYFIN_MAX_TRANSCODES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(2);
        let video_encoder =
            std::env::var("RUSTYFIN_VIDEO_ENCODER").unwrap_or_else(|_| "libx264".to_string());
        let discovery_enabled = std::env::var("RUSTYFIN_DISCOVERY")
            .map(|v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"))
            .unwrap_or(true);
        let admin_password = std::env::var("RUSTYFIN_ADMIN_PASSWORD")
            .ok()
            .filter(|p| !p.is_empty());
        let public_url = std::env::var("RUSTYFIN_PUBLIC_URL")
            .ok()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty());
        let tmdb_api_key = std::env::var("TMDB_API_KEY")
            .ok()
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty());
        let tmdb_language =
            std::env::var("RUSTYFIN_TMDB_LANGUAGE").unwrap_or_else(|_| "pt-BR".to_string());
        let tmdb_base_url = std::env::var("RUSTYFIN_TMDB_BASE_URL")
            .unwrap_or_else(|_| "https://api.themoviedb.org/3".to_string());
        let tmdb_image_base = std::env::var("RUSTYFIN_TMDB_IMAGE_BASE")
            .unwrap_or_else(|_| "https://image.tmdb.org/t/p".to_string());

        let mut media_directories = Vec::new();

        // 1. Check if configured via environment variable (comma-separated, useful for Docker)
        if let Ok(dirs_env) = std::env::var("RUSTYFIN_MEDIA_DIRS") {
            for dir_str in dirs_env.split(',') {
                let trimmed = dir_str.trim();
                if !trimmed.is_empty() {
                    media_directories.push(PathBuf::from(trimmed));
                }
            }
        }

        // 2. Default paths if no env var was set
        if media_directories.is_empty() {
            // Check Docker standard mount path
            let docker_media = PathBuf::from("/media");
            if docker_media.exists() {
                media_directories.push(docker_media);
            }

            // Detect user's local Videos directory on Windows/Linux/macOS
            if let Some(video_dir) = dirs::video_dir() {
                if video_dir.exists() {
                    media_directories.push(video_dir);
                }
            }

            // Local ./media folder
            let local_media = PathBuf::from("./media");
            if !media_directories.contains(&local_media) {
                media_directories.push(local_media);
            }
        }

        Self {
            server_id,
            server_name,
            version,
            host,
            port,
            database_url,
            media_directories,
            cache_dir,
            scan_interval_secs,
            watch_enabled,
            discovery_enabled,
            admin_password,
            public_url,
            ffmpeg_path,
            max_transcodes,
            video_encoder,
            tmdb_api_key,
            tmdb_language,
            tmdb_base_url,
            tmdb_image_base,
        }
    }
}
