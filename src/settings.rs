//! Configurações editáveis em tempo de execução (painel `/admin`), persistidas no banco.
//!
//! Valor efetivo = o salvo no banco, senão o das variáveis de ambiente.

use crate::{config::AppConfig, db::DbPool};
use std::sync::{OnceLock, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    pub tmdb_api_key: Option<String>,
    pub tmdb_language: String,
    pub server_name: String,
    /// Intervalo do scan automático em segundos (0 = só ao iniciar e sob demanda).
    pub scan_interval_secs: u64,
    pub max_transcodes: usize,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            tmdb_api_key: None,
            tmdb_language: "pt-BR".to_string(),
            server_name: "Rustyfin".to_string(),
            scan_interval_secs: 300,
            max_transcodes: 2,
        }
    }
}

static CURRENT: OnceLock<RwLock<Runtime>> = OnceLock::new();

/// O limite de transcodificações não foi definido (nem no painel nem no ambiente): vale o automático do hardware.
static AUTO_SLOTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn max_transcodes_is_auto() -> bool {
    AUTO_SLOTS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Guarda o limite calculado pelo hardware, para o painel mostrar o valor em uso.
pub fn set_auto_max_transcodes(n: usize) {
    if let Ok(mut w) = cell().write() {
        w.max_transcodes = n;
    }
}

fn cell() -> &'static RwLock<Runtime> {
    CURRENT.get_or_init(|| RwLock::new(Runtime::default()))
}

pub fn snapshot() -> Runtime {
    cell().read().map(|r| r.clone()).unwrap_or_default()
}

pub fn tmdb_api_key() -> Option<String> {
    snapshot().tmdb_api_key
}

pub fn tmdb_language() -> String {
    snapshot().tmdb_language
}

pub fn server_name() -> String {
    snapshot().server_name
}

pub fn scan_interval_secs() -> u64 {
    snapshot().scan_interval_secs
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
    let server_name = read(pool, "server_name").await.unwrap_or_else(|| config.server_name.clone());
    let scan_interval_secs = read(pool, "scan_interval_secs")
        .await
        .and_then(|v| v.parse().ok())
        .unwrap_or(config.scan_interval_secs);
    // Prioridade: valor salvo no painel, depois o ambiente; sem nenhum dos dois, o limite automático do hardware
    // (calculado em `transcode::init`) continua valendo.
    let explicit = read(pool, "max_transcodes")
        .await
        .and_then(|v| v.parse::<usize>().ok())
        .or(config.max_transcodes)
        .map(|m| m.clamp(1, MAX_TRANSCODES_LIMIT));
    AUTO_SLOTS.store(explicit.is_none(), std::sync::atomic::Ordering::Relaxed);
    let max_transcodes = explicit.unwrap_or_else(|| snapshot().max_transcodes);
    if let Ok(mut w) = cell().write() {
        *w = Runtime {
            tmdb_api_key: key.filter(|k| !k.is_empty()),
            tmdb_language: lang,
            server_name,
            scan_interval_secs,
            max_transcodes,
        };
    }
    crate::transcode::set_max_slots(max_transcodes);
}

pub const MAX_TRANSCODES_LIMIT: usize = 16;

/// Valida e grava nome do servidor, intervalo do scan e limite de transcodificações (aplicados na hora).
/// Devolve a mensagem de erro de um valor inválido, sem gravar nada.
pub async fn update_server(
    pool: &DbPool,
    config: &AppConfig,
    name: Option<&str>,
    scan_interval_secs: Option<u64>,
    max_transcodes: Option<usize>,
) -> Result<Result<(), &'static str>, sqlx::Error> {
    let name = name.map(str::trim);
    if let Some(n) = name {
        if n.is_empty() || n.chars().count() > 64 || n.chars().any(char::is_control) {
            return Ok(Err("Nome do servidor inválido (1 a 64 caracteres)."));
        }
    }
    if scan_interval_secs.is_some_and(|s| s != 0 && !(10..=86_400).contains(&s)) {
        return Ok(Err("Intervalo do scan: 0 (desligado) ou de 10 a 86400 segundos."));
    }
    if max_transcodes.is_some_and(|m| !(1..=MAX_TRANSCODES_LIMIT).contains(&m)) {
        return Ok(Err("Transcodificações simultâneas: de 1 a 16."));
    }
    static GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = GUARD.lock().await;
    if let Some(n) = name {
        write(pool, "server_name", Some(n)).await?;
    }
    if let Some(s) = scan_interval_secs {
        write(pool, "scan_interval_secs", Some(&s.to_string())).await?;
    }
    if let Some(m) = max_transcodes {
        write(pool, "max_transcodes", Some(&m.to_string())).await?;
    }
    load(pool, config).await;
    Ok(Ok(()))
}

/// `key`: `Some("")` apaga a chave salva (volta ao valor do ambiente, se houver).
pub async fn update_tmdb(
    pool: &DbPool,
    config: &AppConfig,
    key: Option<&str>,
    language: Option<&str>,
) -> Result<(), sqlx::Error> {
    // Gravação e publicação do cache como uma operação só: duas requisições não se invertem.
    static UPDATE_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = UPDATE_GUARD.lock().await;
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
    fn defaults_are_sane() {
        let r = Runtime::default();
        assert_eq!((r.server_name.as_str(), r.scan_interval_secs, r.max_transcodes), ("Rustyfin", 300, 2));
    }

    #[test]
    fn masks_keys() {
        assert_eq!(mask_key("0123456789abcdef"), "••••cdef");
        assert_eq!(mask_key("ab"), "••••ab");
    }
}
