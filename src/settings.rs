//! Configurações editáveis em tempo de execução (painel `/admin`), persistidas no banco.
//!
//! Valor efetivo = o salvo no banco, senão o das variáveis de ambiente.

use crate::{config::AppConfig, db::DbPool};
use std::sync::{OnceLock, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    pub tmdb_api_key: Option<String>,
    pub tmdb_language: String,
}

static CURRENT: OnceLock<RwLock<Runtime>> = OnceLock::new();

fn cell() -> &'static RwLock<Runtime> {
    CURRENT.get_or_init(|| {
        RwLock::new(Runtime { tmdb_api_key: None, tmdb_language: "pt-BR".to_string() })
    })
}

pub fn snapshot() -> Runtime {
    cell().read().map(|r| r.clone()).unwrap_or(Runtime {
        tmdb_api_key: None,
        tmdb_language: "pt-BR".to_string(),
    })
}

pub fn tmdb_api_key() -> Option<String> {
    snapshot().tmdb_api_key
}

pub fn tmdb_language() -> String {
    snapshot().tmdb_language
}

async fn read(pool: &DbPool, key: &str) -> Option<String> {
    sqlx::query_as::<_, (String,)>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
        .map(|r| r.0)
}

async fn write(pool: &DbPool, key: &str, value: Option<&str>) -> Result<(), sqlx::Error> {
    match value {
        Some(v) => {
            sqlx::query(
                "INSERT INTO settings (key, value) VALUES (?, ?) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .bind(key)
            .bind(v)
            .execute(pool)
            .await?;
        }
        None => {
            sqlx::query("DELETE FROM settings WHERE key = ?").bind(key).execute(pool).await?;
        }
    }
    Ok(())
}

/// Combina ambiente e banco (o banco vence). Chamar uma vez na inicialização.
pub async fn load(pool: &DbPool, config: &AppConfig) {
    let key = read(pool, "tmdb_api_key").await.or_else(|| config.tmdb_api_key.clone());
    let lang = read(pool, "tmdb_language").await.unwrap_or_else(|| config.tmdb_language.clone());
    if let Ok(mut w) = cell().write() {
        *w = Runtime { tmdb_api_key: key.filter(|k| !k.is_empty()), tmdb_language: lang };
    }
}

/// `key`: `Some("")` apaga a chave salva (volta ao valor do ambiente, se houver).
pub async fn update_tmdb(
    pool: &DbPool,
    config: &AppConfig,
    key: Option<&str>,
    language: Option<&str>,
) -> Result<(), sqlx::Error> {
    if let Some(k) = key {
        let k = k.trim();
        write(pool, "tmdb_api_key", if k.is_empty() { None } else { Some(k) }).await?;
    }
    if let Some(l) = language {
        let l = l.trim();
        write(pool, "tmdb_language", if l.is_empty() { None } else { Some(l) }).await?;
    }
    load(pool, config).await;
    Ok(())
}

/// Chave para exibir sem revelar: só os 4 últimos caracteres.
pub fn mask_key(key: &str) -> String {
    let tail: String = key.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    format!("••••{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_keys() {
        assert_eq!(mask_key("0123456789abcdef"), "••••cdef");
        assert_eq!(mask_key("ab"), "••••ab");
    }
}
