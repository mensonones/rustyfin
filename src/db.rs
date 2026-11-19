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
        r"
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

        CREATE TABLE IF NOT EXISTS subtitle_adjustments (
            item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
            track_index INTEGER NOT NULL,
            source_hash TEXT NOT NULL,
            adjustment TEXT NOT NULL,
            PRIMARY KEY (item_id, track_index)
        );

        CREATE TABLE IF NOT EXISTS media_reviews (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            path TEXT NOT NULL,
            duration_ticks INTEGER,
            reason TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS media_review_targets (
            review_id TEXT NOT NULL REFERENCES media_reviews(id) ON DELETE CASCADE,
            path TEXT NOT NULL,
            content_sha256 BLOB NOT NULL,
            PRIMARY KEY (review_id, path)
        );

        CREATE TABLE IF NOT EXISTS media_review_state (
            review_id TEXT NOT NULL REFERENCES media_reviews(id) ON DELETE CASCADE,
            user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            is_played BOOLEAN NOT NULL,
            is_favorite BOOLEAN NOT NULL,
            position_ticks INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (review_id, user_id)
        );

        -- As assinaturas de áudio das edições foram removidas (0.30.0): eram derivadas e só serviam à revisão manual.
        DROP TABLE IF EXISTS audio_assinaturas;

        -- A busca de cenas foi removida (0.29.0): o índice era derivado das legendas, então some junto com a feature.
        DROP TABLE IF EXISTS cenas_legenda;
        DROP TABLE IF EXISTS cenas_fontes;

        CREATE TABLE IF NOT EXISTS versoes_preparadas (
            chave TEXT PRIMARY KEY,
            item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
            hash BLOB NOT NULL,
            tamanho INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS playback_state (
            user_id TEXT NOT NULL,
            item_id TEXT NOT NULL,
            position_ticks INTEGER NOT NULL DEFAULT 0,
            is_played BOOLEAN NOT NULL DEFAULT 0,
            last_updated DATETIME DEFAULT CURRENT_TIMESTAMP,
            PRIMARY KEY (user_id, item_id)
        );
        ",
    )
    .execute(&pool)
    .await?;

    // Migrações incrementais (bancos criados por versões anteriores).
    add_column_if_missing(&pool, "media_reviews", "audio_hash", "BLOB").await?;
    add_column_if_missing(&pool, "media_review_state", "position_ticks",
        "INTEGER NOT NULL DEFAULT 0").await?;
    add_column_if_missing(&pool, "items", "modified_at", "INTEGER NOT NULL DEFAULT 0").await?;
    add_column_if_missing(&pool, "items", "content_sha256", "BLOB").await?;
    add_column_if_missing(&pool, "items", "content_mtime", "TEXT").await?;
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
        r"
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
        ",
    )
    .execute(&pool)
    .await?;
    // Bibliotecas, pastas e configurações editáveis pelo painel /admin.
    sqlx::query(
        r"
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
        ",
    )
    .execute(&pool)
    .await?;
    // Poster específico de cada temporada (TMDB); `poster` nulo = sem arte própria (usa a da série).
    add_column_if_missing(&pool, "series", "extra", "TEXT").await?;
    // AniList (só séries das bibliotecas de anime): o ID da obra e a última consulta.
    add_column_if_missing(&pool, "series", "anilist_id", "INTEGER").await?;
    add_column_if_missing(&pool, "series", "anilist_checked_at", "INTEGER").await?;
    // Foto (caminho no TMDB) de cada pessoa do elenco; o ID é o mesmo exposto em `People[].Id`.
    sqlx::query("CREATE TABLE IF NOT EXISTS people (id TEXT PRIMARY KEY, name TEXT NOT NULL, image TEXT)")
        .execute(&pool)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS season_art (season_id TEXT PRIMARY KEY, poster TEXT)")
        .execute(&pool)
        .await?;
    // Usuários, permissões por biblioteca e tokens de acesso (um por dispositivo/login).
    sqlx::query(
        r"
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
        ",
    )
    .execute(&pool)
    .await?;
    // `is_hidden` era o perfil oculto, removido na 0.30.0: a coluna fica no banco, sem uso. Foto de perfil: nome do
    // arquivo em cache/avatars.
    add_column_if_missing(&pool, "users", "is_hidden", "BOOLEAN NOT NULL DEFAULT 0").await?;
    add_column_if_missing(&pool, "users", "avatar", "TEXT").await?;
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

    // Revisões do acervo pendentes de uma versão anterior (a fila foi removida na 0.30.0): quando o único destino
    // sugerido já existe, os marcadores de assistido e favorito vão para ele e a revisão sai. As demais (sem destino ou
    // com mais de um) ficam nas tabelas `media_review*`, sem uso, para não perder dados.
    sqlx::query(
        r"
        INSERT INTO playback_state (user_id, item_id, is_played, is_favorite)
        SELECT s.user_id, i.id, s.is_played, s.is_favorite
        FROM media_reviews r
        JOIN media_review_state s ON s.review_id = r.id AND (s.is_played OR s.is_favorite)
        JOIN media_review_targets t ON t.review_id = r.id
        JOIN items i ON i.path = t.path AND i.content_sha256 = t.content_sha256
        WHERE (SELECT COUNT(*) FROM media_review_targets t2 WHERE t2.review_id = r.id) = 1
        ON CONFLICT(user_id, item_id) DO UPDATE SET
            is_played = (playback_state.is_played OR excluded.is_played),
            is_favorite = (playback_state.is_favorite OR excluded.is_favorite)
        ",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        r"
        DELETE FROM media_reviews WHERE id IN (
            SELECT r.id FROM media_reviews r
            JOIN media_review_targets t ON t.review_id = r.id
            JOIN items i ON i.path = t.path AND i.content_sha256 = t.content_sha256
            WHERE (SELECT COUNT(*) FROM media_review_targets t2 WHERE t2.review_id = r.id) = 1
        )
        ",
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn as_tabelas_da_busca_de_cenas_removida_somem_na_proxima_subida() {
        let arquivo = std::env::temp_dir().join(format!("rustyfin-db-cenas-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&arquivo);
        let url = format!("sqlite://{}?mode=rwc", arquivo.to_string_lossy().replace('\\', "/"));
        let pool = init_db(&url).await.unwrap();
        // Um banco de uma versão anterior ainda tem as tabelas, com dados.
        sqlx::query("CREATE TABLE cenas_legenda (item_id TEXT, texto TEXT)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO cenas_legenda VALUES ('x', 'fala')").execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE cenas_fontes (item_id TEXT)").execute(&pool).await.unwrap();
        pool.close().await;

        let pool = init_db(&url).await.unwrap();
        let restam: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('cenas_legenda', 'cenas_fontes')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(restam, 0);
        pool.close().await;
        let _ = std::fs::remove_file(&arquivo);
    }

    #[tokio::test]
    async fn revisoes_pendentes_de_um_unico_destino_viram_marcadores_e_as_demais_ficam() {
        let arquivo = std::env::temp_dir().join(format!("rustyfin-db-revisoes-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&arquivo);
        let url = format!("sqlite://{}?mode=rwc", arquivo.to_string_lossy().replace('\\', "/"));
        let pool = init_db(&url).await.unwrap();
        for sql in [
            "INSERT INTO users (id, name) VALUES ('u1', 'Ana')",
            "INSERT INTO items (id, name, path, media_type, content_sha256) VALUES ('nova', 'Filme', '/m/nova.mp4', 'Movie', X'01')",
            "INSERT INTO items (id, name, path, media_type, content_sha256) VALUES ('a', 'Filme', '/m/a.mp4', 'Movie', X'02')",
            "INSERT INTO items (id, name, path, media_type, content_sha256) VALUES ('b', 'Filme', '/m/b.mp4', 'Movie', X'02')",
            "INSERT INTO media_reviews (id, title, path, reason) VALUES ('unica', 'Antiga', '/m/antiga.mp4', 'possivel_edicao')",
            "INSERT INTO media_review_targets VALUES ('unica', '/m/nova.mp4', X'01')",
            "INSERT INTO media_review_state VALUES ('unica', 'u1', 1, 1, 500)",
            "INSERT INTO media_reviews (id, title, path, reason) VALUES ('ambigua', 'Outra', '/m/outra.mp4', 'mesmo_conteudo')",
            "INSERT INTO media_review_targets VALUES ('ambigua', '/m/a.mp4', X'02')",
            "INSERT INTO media_review_targets VALUES ('ambigua', '/m/b.mp4', X'02')",
            "INSERT INTO media_review_state VALUES ('ambigua', 'u1', 1, 0, 0)",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        pool.close().await;

        let pool = init_db(&url).await.unwrap();
        let (visto, favorito, posicao): (bool, bool, i64) = sqlx::query_as(
            "SELECT is_played, is_favorite, position_ticks FROM playback_state WHERE item_id = 'nova'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(visto && favorito, "os marcadores vão para o único destino");
        assert_eq!(posicao, 0, "a posição não é transferida");
        let restam: Vec<String> = sqlx::query_scalar("SELECT id FROM media_reviews ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
        assert_eq!(restam, ["ambigua"], "a de vários destinos fica, sem uso, para não perder dados");
        pool.close().await;
        let _ = std::fs::remove_file(&arquivo);
    }
}
