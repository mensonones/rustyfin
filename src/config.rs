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
    /// Capas e fundos de títulos (`/Items/{id}/Images/*`) abertos aos clientes da rede local que não enviam token. Os apps
    /// que carregam imagens com `<img>` (como o Moonfin na TV Samsung) não conseguem enviar cabeçalhos; o Jellyfin também
    /// serve as imagens sem login. Fora da rede local, e com `false`, as imagens exigem token.
    pub anonymous_images: bool,
    /// Quick Connect: entrar num app com um código autorizado por um aparelho já logado (`RUSTYFIN_QUICK_CONNECT=0` desliga).
    pub quick_connect: bool,
    /// Se definida, o painel `/admin` exige essa senha (HTTP Basic, qualquer usuário).
    pub admin_password: Option<String>,
    /// Endereço HTTP anunciado na descoberta (obrigatório na prática em Docker).
    pub public_url: Option<String>,
    /// Proxies reversos (IPs ou redes) cujo `X-Forwarded-For` vale como o IP do cliente. Vazio = nenhum. Veja `proxy.rs`.
    pub trusted_proxies: Vec<crate::proxy::Rede>,
    /// Executável do ffmpeg (miniaturas, remux e transcode).
    pub ffmpeg_path: String,
    /// Máximo de transcodificações simultâneas; `None` = automático (calculado pelo hardware na subida).
    pub max_transcodes: Option<usize>,
    /// Encoder de vídeo: `auto` (padrão: VAAPI se houver GPU Intel/AMD, senão `libx264`), `libx264` ou de hardware
    /// (`h264_vaapi`, `h264_qsv`, `h264_nvenc`, `h264_videotoolbox`).
    pub video_encoder: String,
    /// Chave v3 (32 chars) ou token v4 do TMDB. Sem ela, nenhum metadado online é buscado.
    pub tmdb_api_key: Option<String>,
    pub tmdb_language: String,
    pub tmdb_base_url: String,
    pub tmdb_image_base: String,
}

/// URL do SQLite dentro da pasta de dados (barras normais, que o sqlx aceita também no Windows).
fn database_url_in(dir: &std::path::Path) -> String {
    format!("sqlite://{}?mode=rwc", dir.join("rustyfin.db").to_string_lossy().replace('\\', "/"))
}

/// `RUSTYFIN_DATA_DIR`: a pasta é criada se não existir. `None` se a variável não estiver definida ou estiver vazia.
fn data_dir_from_env() -> Option<PathBuf> {
    let dir = std::env::var("RUSTYFIN_DATA_DIR")
        .ok()
        .map(|d| PathBuf::from(d.trim()))
        .filter(|d| !d.as_os_str().is_empty())?;
    if let Err(erro) = std::fs::create_dir_all(&dir) {
        // O servidor segue e o erro ao abrir o banco mostrará o caminho; aqui só avisamos de onde ele veio.
        eprintln!("Não foi possível criar RUSTYFIN_DATA_DIR ({}): {erro}", dir.display());
    }
    Some(dir)
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
        
        // `RUSTYFIN_DATA_DIR` junta o banco e o cache numa pasta só (instalação nativa, serviços). Sem ela, valem os
        // padrões antigos (`./rustyfin.db` e `./cache`), e `DATABASE_URL` e `RUSTYFIN_CACHE_DIR` continuam mandando.
        let data_dir = data_dir_from_env();
        let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            data_dir.as_deref().map_or_else(|| "sqlite://rustyfin.db?mode=rwc".to_string(), database_url_in)
        });

        let cache_dir = std::env::var("RUSTYFIN_CACHE_DIR").map_or_else(
            |_| data_dir.as_ref().map_or_else(|| PathBuf::from("./cache"), |dir| dir.join("cache")),
            PathBuf::from,
        );

        let scan_interval_secs = std::env::var("RUSTYFIN_SCAN_INTERVAL_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(300);
        let watch_enabled = std::env::var("RUSTYFIN_WATCH")
            .map_or(true, |v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"));
        let ffmpeg_path = std::env::var("RUSTYFIN_FFMPEG").unwrap_or_else(|_| "ffmpeg".to_string());
        let max_transcodes = std::env::var("RUSTYFIN_MAX_TRANSCODES").ok().and_then(|v| v.trim().parse::<usize>().ok());
        let video_encoder =
            std::env::var("RUSTYFIN_VIDEO_ENCODER").unwrap_or_else(|_| "auto".to_string());
        let discovery_enabled = std::env::var("RUSTYFIN_DISCOVERY")
            .map_or(true, |v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"));
        let anonymous_images = std::env::var("RUSTYFIN_IMAGENS_ANONIMAS")
            .map_or(true, |v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"));
        let quick_connect = std::env::var("RUSTYFIN_QUICK_CONNECT")
            .map_or(true, |v| !matches!(v.trim().to_lowercase().as_str(), "0" | "false" | "no" | "off"));
        let admin_password = std::env::var("RUSTYFIN_ADMIN_PASSWORD")
            .ok()
            .filter(|p| !p.is_empty());
        let public_url = std::env::var("RUSTYFIN_PUBLIC_URL")
            .ok()
            .map(|u| u.trim().to_string())
            .filter(|u| !u.is_empty());
        let (trusted_proxies, invalidos) =
            crate::proxy::ler_lista(&std::env::var("RUSTYFIN_TRUSTED_PROXIES").unwrap_or_default());
        for erro in invalidos {
            // Ignorar é o lado seguro: o servidor passa a confiar em menos proxies, nunca em mais.
            eprintln!("RUSTYFIN_TRUSTED_PROXIES: entrada ignorada ({erro}).");
        }
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
            anonymous_images,
            quick_connect,
            admin_password,
            public_url,
            trusted_proxies,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_database_lives_inside_the_data_dir() {
        assert_eq!(database_url_in(std::path::Path::new("/var/lib/rustyfin")), "sqlite:///var/lib/rustyfin/rustyfin.db?mode=rwc");
        // Caminhos do Windows usam barras normais na URL.
        let url = database_url_in(std::path::Path::new("C:\\Dados\\Rustyfin"));
        assert!(url.ends_with("/rustyfin.db?mode=rwc"), "{url}");
        assert!(!url.contains('\\'), "{url}");
    }

    #[test]
    fn the_ffmpeg_hint_names_a_package_command() {
        assert!(crate::transcode::dica_instalar_ffmpeg().contains("ffmpeg"));
    }
}
