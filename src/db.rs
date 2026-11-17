use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    Pool, Sqlite,
};
use std::{error::Error, str::FromStr};

pub type DbPool = Pool<Sqlite>;

pub async fn init_db(database_url: &str) -> Result<DbPool, Box<dyn Error + Send + Sync>> {
    // WAL é aplicado em todas as conexões do pool via opções de conexão.
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal);

    let pool = SqlitePoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS items (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            media_type TEXT NOT NULL,
            container TEXT,
            duration_ticks INTEGER DEFAULT 0,
            size_bytes INTEGER DEFAULT 0,
            width INTEGER,
            height INTEGER,
            video_codec TEXT,
            audio_codec TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS playback_state (
            user_id TEXT NOT NULL,
            item_id TEXT NOT NULL,
            position_ticks INTEGER NOT NULL DEFAULT 0,
            is_played BOOLEAN NOT NULL DEFAULT 0,
            last_updated DATETIME DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (user_id, item_id)
        );
        "#,
    )
    .execute(&pool)
    .await?;

    // Migrações incrementais (bancos criados por versões anteriores).
    add_column_if_missing(&pool, "items", "modified_at", "INTEGER NOT NULL DEFAULT 0").await?;
    add_column_if_missing(&pool, "items", "probed", "BOOLEAN NOT NULL DEFAULT 0").await?;
    add_column_if_missing(&pool, "items", "streams_json", "TEXT").await?;
    add_column_if_missing(&pool, "items", "subtitle_path", "TEXT").await?;
    add_column_if_missing(&pool, "items", "poster_path", "TEXT").await?;
    add_column_if_missing(&pool, "items", "backdrop_path", "TEXT").await?;
    add_column_if_missing(&pool, "items", "series_poster_path", "TEXT").await?;
    add_column_if_missing(&pool, "items", "series_backdrop_path", "TEXT").await?;
    add_column_if_missing(&pool, "items", "season_poster_path", "TEXT").await?;
    add_column_if_missing(&pool, "items", "production_year", "INTEGER").await?;
    add_column_if_missing(&pool, "items", "series_name", "TEXT").await?;
    add_column_if_missing(&pool, "items", "season_number", "INTEGER").await?;
    add_column_if_missing(&pool, "items", "episode_number", "INTEGER").await?;
    add_column_if_missing(&pool, "items", "tmdb_id", "INTEGER").await?;
    add_column_if_missing(&pool, "items", "tmdb_title", "TEXT").await?;
    add_column_if_missing(&pool, "items", "overview", "TEXT").await?;
    add_column_if_missing(&pool, "items", "rating", "REAL").await?;
    add_column_if_missing(&pool, "items", "genres", "TEXT").await?;
    // Elenco, estúdios, classificação e trailers do TMDB (JSON); `NULL` = ainda não buscado.
    add_column_if_missing(&pool, "items", "extra", "TEXT").await?;
    add_column_if_missing(&pool, "items", "tmdb_poster", "TEXT").await?;
    add_column_if_missing(&pool, "items", "tmdb_backdrop", "TEXT").await?;
    add_column_if_missing(&pool, "items", "tmdb_checked_at", "INTEGER NOT NULL DEFAULT 0").await?;
    add_column_if_missing(&pool, "items", "series_id", "TEXT").await?;
    add_column_if_missing(&pool, "items", "library_id", "TEXT").await?;
    add_column_if_missing(&pool, "items", "season_id", "TEXT").await?;
    add_column_if_missing(&pool, "playback_state", "is_favorite", "BOOLEAN NOT NULL DEFAULT 0").await?;
    // Reinspeciona no próximo scan os arquivos com trilhas gravadas antes dos novos metadados.
    sqlx::query(
        r#"UPDATE items SET probed = 0 WHERE streams_json IS NOT NULL
           AND (streams_json NOT LIKE '%"IsForced"%' OR streams_json NOT LIKE '%"ColorTransfer"%')"#,
    )
    .execute(&pool)
    .await?;
    add_column_if_missing(&pool, "playback_state", "play_count", "INTEGER NOT NULL DEFAULT 0").await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_items_name ON items(name COLLATE NOCASE)")
        .execute(&pool)
        .await?;
    // Metadados por série (TMDB). O ID é o mesmo `series_id` dos episódios.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS series (
            id TEXT PRIMARY KEY,
            tmdb_id INTEGER,
            tmdb_title TEXT,
            overview TEXT,
            rating REAL,
            genres TEXT,
            year INTEGER,
            poster TEXT,
            backdrop TEXT
        )
        "#,
    )
    .execute(&pool)
    .await?;
    // Bibliotecas, pastas e configurações editáveis pelo painel /admin.
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS libraries (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE COLLATE NOCASE,
            collection_type TEXT NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );
        CREATE TABLE IF NOT EXISTS library_paths (
            library_id TEXT NOT NULL,
            path TEXT NOT NULL,
            PRIMARY KEY (library_id, path)
        );
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await?;
    // Poster específico de cada temporada (TMDB); `poster` nulo = sem arte própria (usa a da série).
    add_column_if_missing(&pool, "series", "extra", "TEXT").await?;
    // Foto (caminho no TMDB) de cada pessoa do elenco; o ID é o mesmo exposto em `People[].Id`.
    sqlx::query("CREATE TABLE IF NOT EXISTS people (id TEXT PRIMARY KEY, name TEXT NOT NULL, image TEXT)")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS season_art (season_id TEXT PRIMARY KEY, poster TEXT)")
        .execute(&pool)
        .await?;
    // Usuários, permissões por biblioteca e tokens de acesso (um por dispositivo/login).
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE COLLATE NOCASE,
            password_hash TEXT,
            is_admin BOOLEAN NOT NULL DEFAULT 0,
            is_disabled BOOLEAN NOT NULL DEFAULT 0,
            all_libraries BOOLEAN NOT NULL DEFAULT 1,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );
        CREATE TABLE IF NOT EXISTS user_libraries (
            user_id TEXT NOT NULL,
            library_id TEXT NOT NULL,
            PRIMARY KEY (user_id, library_id)
        );
        CREATE TABLE IF NOT EXISTS access_tokens (
            token TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            client TEXT,
            device TEXT,
            device_id TEXT,
            created_at INTEGER NOT NULL,
            last_used INTEGER NOT NULL
        );
        "#,
    )
    .execute(&pool)
    .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_tokens_user ON access_tokens(user_id)")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_items_library ON items(library_id)")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_items_series ON items(series_id, season_number, episode_number)")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_items_season ON items(season_id)")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_items_created ON items(created_at)")
        .execute(&pool)
        .await?;

    Ok(pool)
}

async fn add_column_if_missing(
    pool: &DbPool,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    // `table`/`column` são constantes internas, nunca entrada do usuário.
    let exists: Option<(String,)> = sqlx::query_as(&format!(
        "SELECT name FROM pragma_table_info('{table}') WHERE name = ?"
    ))
    .bind(column)
    .fetch_optional(pool)
    .await?;

    if exists.is_none() {
        sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"))
            .execute(pool)
            .await?;
    }
    Ok(())
}
