mod api;
mod config;
mod db;
mod discovery;
mod libraries;
mod library;
mod metadata;
mod models;
mod parser;
mod scanner;
mod settings;
mod transcode;
mod users;

use config::AppConfig;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rustyfin=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Iniciando Rustyfin (Jellyfin compatível em Rust)...");

    // Load configuration
    let config = Arc::new(AppConfig::load());
    info!("ID do Servidor: {}", config.server_id);
    info!("Pastas monitoradas: {:?}", config.media_directories);

    // Initialize database
    let pool = db::init_db(&config.database_url).await?;
    info!("Banco de dados SQLite inicializado com sucesso (modo WAL).");

    // Bibliotecas e configurações vivem no banco; o ambiente só define os valores iniciais.
    libraries::seed_if_empty(&pool, &config).await?;
    settings::load(&pool, &config).await;
    users::ensure_seed_admin(&pool).await?;

    transcode::init(&config).await;
    discovery::spawn(config.clone());

    // Scan inicial + periódico + sob demanda (/Library/Refresh), com metadados TMDB opcionais
    let scan_trigger: library::ScanTrigger = Arc::new(tokio::sync::Notify::new());
    library::spawn_autoscan(pool.clone(), config.clone(), scan_trigger.clone());
    if settings::tmdb_api_key().is_none() {
        info!("TMDB_API_KEY não definida: metadados online desativados.");
    }

    // Build API Router
    let app = api::build_router(config.clone(), pool, scan_trigger);

    // Bind server to port 8096
    let bind_addr = format!("{}:{}", config.host, config.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;

    println!();
    println!("===========================================================");
    println!("  🦀 Rustyfin rodando com sucesso!");
    println!("  URL Local:   http://localhost:{}", config.port);
    println!("  Escutando em: http://{}", bind_addr);
    println!("  Compatível com: Moonfin, Swiftfin, Findroid, Web");
    println!("  Usuários:    gerencie em /admin (Admin criado sem senha)");
    println!("  Painel:      http://localhost:{}/admin", config.port);
    println!("===========================================================");
    println!();

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;

    Ok(())
}
