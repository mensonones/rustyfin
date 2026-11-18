//! Usuários, senhas (Argon2), tokens de acesso e permissões por biblioteca.

use crate::db::DbPool;
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use serde::Serialize;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

/// ID do usuário `Admin` criado na migração (o mesmo de antes: o progresso já salvo continua valendo).
pub const ADMIN_ID: &str = "00000000000000000000000000000001";
pub const MAX_PASSWORD_LEN: usize = 256;
const MAX_NAME_LEN: usize = 64;

#[derive(Debug)]
pub enum UserError {
    InvalidName,
    PasswordTooLong,
    NameExists,
    NotFound,
    /// Não é possível remover/rebaixar/desativar o último administrador ativo.
    LastAdmin,
    Hash,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for UserError {
    fn from(e: sqlx::Error) -> Self {
        UserError::Db(e)
    }
}

impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UserError::InvalidName => write!(f, "Nome inválido (1 a {MAX_NAME_LEN} caracteres)."),
            UserError::PasswordTooLong => write!(f, "Senha longa demais (máx. {MAX_PASSWORD_LEN})."),
            UserError::NameExists => write!(f, "Já existe um usuário com esse nome."),
            UserError::NotFound => write!(f, "Usuário não encontrado."),
            UserError::LastAdmin => write!(f, "Deve existir ao menos um administrador ativo."),
            UserError::Hash => write!(f, "Falha ao processar a senha."),
            UserError::Db(e) => write!(f, "Erro de banco de dados: {e}"),
        }
    }
}

#[derive(Debug)]
pub enum AuthError {
    BadCredentials,
    Disabled,
    /// Segundos até poder tentar de novo.
    Locked(u64),
    Internal,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct User {
    pub id: String,
    pub name: String,
    pub has_password: bool,
    pub is_admin: bool,
    pub is_disabled: bool,
    pub all_libraries: bool,
    /// Fora da lista de perfis da tela de login (o login digitando o nome continua valendo).
    pub is_hidden: bool,
    /// Nome do arquivo da foto de perfil em `<cache>/avatars`.
    pub avatar: Option<String>,
    /// IDs das bibliotecas permitidas (só relevante se `all_libraries` for falso).
    pub libraries: Vec<String>,
}

/// Usuário autenticado numa requisição.
#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: String,
    #[allow(dead_code)] // útil em logs/diagnóstico
    pub name: String,
    pub is_admin: bool,
    /// `None` = todas as bibliotecas.
    pub allowed_libraries: Option<Vec<String>>,
}

impl AuthUser {
    pub fn can_access_library(&self, library_id: Option<&str>) -> bool {
        match (&self.allowed_libraries, library_id) {
            (None, _) => true,
            (Some(list), Some(id)) => list.iter().any(|l| l == id),
            (Some(_), None) => false,
        }
    }

    /// Cláusula SQL (`col IN (?, ?)`) e valores para restringir uma consulta às bibliotecas do usuário.
    pub fn library_clause(&self, column: &str) -> Option<(String, Vec<String>)> {
        let list = self.allowed_libraries.as_ref()?;
        if list.is_empty() {
            return Some(("0 = 1".to_string(), Vec::new()));
        }
        let placeholders = vec!["?"; list.len()].join(", ");
        Some((format!("{column} IN ({placeholders})"), list.clone()))
    }
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

// ---------------------------------------------------------------- senhas

pub fn hash_password(password: &str) -> Result<String, UserError> {
    if password.len() > MAX_PASSWORD_LEN {
        return Err(UserError::PasswordTooLong);
    }
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| UserError::Hash)
}

pub fn verify_password(hash: &str, password: &str) -> bool {
    if password.len() > MAX_PASSWORD_LEN {
        return false;
    }
    PasswordHash::new(hash)
        .is_ok_and(|p| Argon2::default().verify_password(password.as_bytes(), &p).is_ok())
}

/// Hash/verificação custam ~dezenas de ms: fora do runtime async.
async fn hash_blocking(password: String) -> Result<String, UserError> {
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(|_| UserError::Hash)?
}

async fn verify_blocking(hash: String, password: String) -> bool {
    tokio::task::spawn_blocking(move || verify_password(&hash, &password))
        .await
        .unwrap_or(false)
}

/// Hash de uma senha qualquer: usado para gastar o mesmo tempo quando o usuário não existe.
fn dummy_hash() -> &'static str {
    static H: OnceLock<String> = OnceLock::new();
    H.get_or_init(|| hash_password("rustyfin-dummy").unwrap_or_default())
}

// ---------------------------------------------------------------- limite de tentativas

const MAX_FAILURES: u32 = 5;
const LOCK_WINDOW: Duration = Duration::from_secs(300);

fn limiter() -> &'static Mutex<HashMap<String, (u32, Instant)>> {
    static L: OnceLock<Mutex<HashMap<String, (u32, Instant)>>> = OnceLock::new();
    L.get_or_init(|| Mutex::new(HashMap::new()))
}

/// `Err(segundos)` se a chave estiver bloqueada.
pub fn limiter_check(key: &str) -> Result<(), u64> {
    let mut map = limiter().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((count, first)) = map.get(key).copied() {
        let elapsed = first.elapsed();
        if elapsed >= LOCK_WINDOW {
            map.remove(key);
        } else if count >= MAX_FAILURES {
            return Err(LOCK_WINDOW.checked_sub(elapsed).unwrap().as_secs().max(1));
        }
    }
    Ok(())
}

pub fn limiter_fail(key: &str) {
    let mut map = limiter().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = map.entry(key.to_string()).or_insert((0, Instant::now()));
    entry.0 += 1;
    if map.len() > 10_000 {
        map.retain(|_, (_, t)| t.elapsed() < LOCK_WINDOW);
    }
}

pub fn limiter_ok(key: &str) {
    limiter().lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(key);
}

// ---------------------------------------------------------------- consultas

type UserRow = (String, String, Option<String>, bool, bool, bool, bool, Option<String>);

const USER_COLS: &str = "id, name, password_hash, is_admin, is_disabled, all_libraries, is_hidden, avatar";

async fn build_users(pool: &DbPool, rows: Vec<UserRow>) -> Result<Vec<User>, sqlx::Error> {
    let access: Vec<(String, String)> =
        sqlx::query_as("SELECT user_id, library_id FROM user_libraries").fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, hash, is_admin, is_disabled, all_libraries, is_hidden, avatar)| User {
            libraries: access.iter().filter(|(u, _)| *u == id).map(|(_, l)| l.clone()).collect(),
            id,
            name,
            has_password: hash.is_some(),
            is_admin,
            is_disabled,
            all_libraries,
            is_hidden,
            avatar,
        })
        .collect())
}

pub async fn list(pool: &DbPool) -> Result<Vec<User>, sqlx::Error> {
    let rows: Vec<UserRow> =
        sqlx::query_as(&format!("SELECT {USER_COLS} FROM users ORDER BY name COLLATE NOCASE"))
            .fetch_all(pool)
            .await?;
    build_users(pool, rows).await
}

pub async fn get(pool: &DbPool, id: &str) -> Result<Option<User>, sqlx::Error> {
    let rows: Vec<UserRow> = sqlx::query_as(&format!("SELECT {USER_COLS} FROM users WHERE id = ?"))
        .bind(id)
        .fetch_all(pool)
        .await?;
    Ok(build_users(pool, rows).await?.into_iter().next())
}

async fn get_row_by_name(pool: &DbPool, name: &str) -> Result<Option<(UserRow,)>, sqlx::Error> {
    let row: Option<UserRow> =
        sqlx::query_as(&format!("SELECT {USER_COLS} FROM users WHERE name = ? COLLATE NOCASE"))
            .bind(name.trim())
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|r| (r,)))
}

#[cfg(test)]
pub async fn get_by_name(pool: &DbPool, name: &str) -> Result<Option<User>, sqlx::Error> {
    match get_row_by_name(pool, name).await? {
        Some((row,)) => Ok(build_users(pool, vec![row]).await?.into_iter().next()),
        None => Ok(None),
    }
}

/// Banco sem usuários: cria o `Admin` (sem senha), como era antes desta versão.
pub async fn ensure_seed_admin(pool: &DbPool) -> Result<(), sqlx::Error> {
    let (count,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users").fetch_one(pool).await?;
    if count == 0 {
        sqlx::query(
            "INSERT INTO users (id, name, password_hash, is_admin, is_disabled, all_libraries) \
             VALUES (?, 'Admin', NULL, 1, 0, 1)",
        )
        .bind(ADMIN_ID)
        .execute(pool)
        .await?;
        tracing::info!("Usuário 'Admin' criado (sem senha). Defina uma senha no painel /admin.");
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<&str, UserError> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > MAX_NAME_LEN || n.chars().any(char::is_control) {
        return Err(UserError::InvalidName);
    }
    Ok(n)
}

pub async fn create(
    pool: &DbPool,
    name: &str,
    password: Option<&str>,
    is_admin: bool,
) -> Result<User, UserError> {
    let name = validate_name(name)?;
    if get_row_by_name(pool, name).await?.is_some() {
        return Err(UserError::NameExists);
    }
    let hash = match password.filter(|p| !p.is_empty()) {
        Some(p) => Some(hash_blocking(p.to_string()).await?),
        None => None,
    };
    let id = Uuid::new_v4().simple().to_string();
    sqlx::query(
        "INSERT INTO users (id, name, password_hash, is_admin, is_disabled, all_libraries) VALUES (?, ?, ?, ?, 0, 1)",
    )
    .bind(&id)
    .bind(name)
    .bind(&hash)
    .bind(is_admin)
    .execute(pool)
    .await?;
    get(pool, &id).await?.ok_or(UserError::NotFound)
}

async fn active_admin_count(pool: &DbPool, excluding: Option<&str>) -> Result<i64, sqlx::Error> {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM users WHERE is_admin = 1 AND is_disabled = 0 AND id != ?",
    )
    .bind(excluding.unwrap_or(""))
    .fetch_one(pool)
    .await?;
    Ok(n)
}

/// Serializa as mudanças que podem tirar o poder de administrador (verificação + escrita atômicas).
static ADMIN_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub async fn delete(pool: &DbPool, id: &str) -> Result<(), UserError> {
    let _guard = ADMIN_GUARD.lock().await;
    let user = get(pool, id).await?.ok_or(UserError::NotFound)?;
    if user.is_admin && !user.is_disabled && active_admin_count(pool, Some(id)).await? == 0 {
        return Err(UserError::LastAdmin);
    }
    let mut tx = pool.begin().await?;
    for sql in [
        "DELETE FROM access_tokens WHERE user_id = ?",
        "DELETE FROM user_libraries WHERE user_id = ?",
        "DELETE FROM playback_state WHERE user_id = ?",
        "DELETE FROM users WHERE id = ?",
    ] {
        sqlx::query(sql).bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

pub async fn rename(pool: &DbPool, id: &str, new_name: &str) -> Result<(), UserError> {
    let name = validate_name(new_name)?;
    get(pool, id).await?.ok_or(UserError::NotFound)?;
    if let Some((row,)) = get_row_by_name(pool, name).await? {
        if row.0 != id {
            return Err(UserError::NameExists);
        }
    }
    sqlx::query("UPDATE users SET name = ? WHERE id = ?").bind(name).bind(id).execute(pool).await?;
    Ok(())
}

/// `None`/vazio remove a senha. Todos os tokens do usuário são revogados, exceto `keep_token`.
pub async fn set_password(
    pool: &DbPool,
    id: &str,
    password: Option<&str>,
    keep_token: Option<&str>,
) -> Result<(), UserError> {
    get(pool, id).await?.ok_or(UserError::NotFound)?;
    let hash = match password.filter(|p| !p.is_empty()) {
        Some(p) => Some(hash_blocking(p.to_string()).await?),
        None => None,
    };
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(&hash)
        .bind(id)
        .execute(pool)
        .await?;
    revoke_user_tokens(pool, id, keep_token).await?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct PolicyUpdate {
    pub is_admin: Option<bool>,
    pub is_disabled: Option<bool>,
    pub is_hidden: Option<bool>,
    pub all_libraries: Option<bool>,
    pub libraries: Option<Vec<String>>,
}

pub async fn set_policy(pool: &DbPool, id: &str, upd: PolicyUpdate) -> Result<(), UserError> {
    let _guard = ADMIN_GUARD.lock().await;
    let user = get(pool, id).await?.ok_or(UserError::NotFound)?;
    let is_admin = upd.is_admin.unwrap_or(user.is_admin);
    let is_disabled = upd.is_disabled.unwrap_or(user.is_disabled);

    // Sempre deve sobrar um administrador ativo.
    let loses_admin_power = user.is_admin && !user.is_disabled && (!is_admin || is_disabled);
    if loses_admin_power && active_admin_count(pool, Some(id)).await? == 0 {
        return Err(UserError::LastAdmin);
    }

    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE users SET is_admin = ?, is_disabled = ?, is_hidden = ?, all_libraries = ? WHERE id = ?")
        .bind(is_admin)
        .bind(is_disabled)
        .bind(upd.is_hidden.unwrap_or(user.is_hidden))
        .bind(upd.all_libraries.unwrap_or(user.all_libraries))
        .bind(id)
        .execute(&mut *tx)
        .await?;
    if let Some(libs) = upd.libraries {
        sqlx::query("DELETE FROM user_libraries WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
        for lib in libs {
            // Só IDs de bibliotecas existentes.
            sqlx::query(
                "INSERT OR IGNORE INTO user_libraries (user_id, library_id) \
                 SELECT ?, id FROM libraries WHERE id = ?",
            )
            .bind(id)
            .bind(lib)
            .execute(&mut *tx)
            .await?;
        }
    }
    if is_disabled {
        sqlx::query("DELETE FROM access_tokens WHERE user_id = ?").bind(id).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// A senha informada confere? (usuário sem senha aceita senha vazia)
pub async fn check_password(pool: &DbPool, id: &str, password: &str) -> Result<bool, sqlx::Error> {
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT password_hash FROM users WHERE id = ?").bind(id).fetch_optional(pool).await?;
    Ok(match row {
        Some((Some(hash),)) => verify_blocking(hash, password.to_string()).await,
        Some((None,)) => password.is_empty(),
        None => false,
    })
}

/// Há algum administrador ativo com senha? (então o painel passa a exigir login)
pub async fn any_admin_has_password(pool: &DbPool) -> bool {
    sqlx::query_as::<_, (i64,)>(
        "SELECT COUNT(*) FROM users WHERE is_admin = 1 AND is_disabled = 0 AND password_hash IS NOT NULL",
    )
    .fetch_one(pool)
    .await
    .is_ok_and(|r| r.0 > 0)
}

// ---------------------------------------------------------------- autenticação e tokens

#[derive(Debug, Default, Clone)]
pub struct DeviceInfo {
    pub client: Option<String>,
    pub device: Option<String>,
    pub device_id: Option<String>,
}

/// Confere nome e senha. Gasta o mesmo tempo mesmo para usuários inexistentes.
pub async fn authenticate(
    pool: &DbPool,
    name: &str,
    password: &str,
    limiter_key: &str,
) -> Result<User, AuthError> {
    authenticate_from(pool, name, password, limiter_key, true).await
}

/// `from_lan = false` (origem fora de rede privada) recusa contas sem senha: o acesso sem senha
/// existe só para a rede local, e o `Admin` inicial nunca vira token para a internet.
pub async fn authenticate_from(
    pool: &DbPool,
    name: &str,
    password: &str,
    limiter_key: &str,
    from_lan: bool,
) -> Result<User, AuthError> {
    limiter_check(limiter_key).map_err(AuthError::Locked)?;

    let row = get_row_by_name(pool, name).await.map_err(|_| AuthError::Internal)?;
    let result = if let Some((r,)) = row {
        let (id, _, hash, _, is_disabled, ..) = &r;
        let ok = if let Some(h) = hash {
            verify_blocking(h.clone(), password.to_string()).await
        } else {
            // Sem senha: entra direto, mas só da rede local.
            if !from_lan {
                tracing::warn!(
                    "Login de '{name}' recusado: o usuário não tem senha e o cliente não é da rede local (atrás de um \
                     proxy reverso, confira RUSTYFIN_TRUSTED_PROXIES). Defina uma senha para acessar de fora."
                );
            }
            from_lan
        };
        if !ok {
            Err(AuthError::BadCredentials)
        } else if *is_disabled {
            Err(AuthError::Disabled)
        } else {
            get(pool, id).await.ok().flatten().ok_or(AuthError::Internal)
        }
    } else {
        verify_blocking(dummy_hash().to_string(), password.to_string()).await;
        Err(AuthError::BadCredentials)
    };

    match &result {
        Ok(_) => limiter_ok(limiter_key),
        Err(AuthError::BadCredentials) => limiter_fail(limiter_key),
        Err(_) => {}
    }
    result
}

pub async fn create_token(pool: &DbPool, user_id: &str, device: &DeviceInfo) -> Result<String, sqlx::Error> {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let now = now_secs();
    sqlx::query(
        "INSERT INTO access_tokens (token, user_id, client, device, device_id, created_at, last_used) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&token)
    .bind(user_id)
    .bind(&device.client)
    .bind(&device.device)
    .bind(&device.device_id)
    .bind(now)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(token)
}

/// Usuário dono do token (ativo), com suas bibliotecas permitidas.
/// Validade de um token sem uso (`RUSTYFIN_TOKEN_TTL_DAYS`; 0 ou ausente = não expira).
fn token_ttl_secs() -> i64 {
    static TTL: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *TTL.get_or_init(|| {
        std::env::var("RUSTYFIN_TOKEN_TTL_DAYS")
            .ok()
            .and_then(|v| v.trim().parse::<i64>().ok())
            .filter(|d| *d > 0)
            .map_or(0, |d| d.saturating_mul(86_400))
    })
}

pub async fn lookup_token(pool: &DbPool, token: &str) -> Result<Option<AuthUser>, sqlx::Error> {
    lookup_token_with_ttl(pool, token, token_ttl_secs()).await
}

async fn lookup_token_with_ttl(pool: &DbPool, token: &str, ttl_secs: i64) -> Result<Option<AuthUser>, sqlx::Error> {
    let row: Option<(String, String, bool, bool, i64)> = sqlx::query_as(
        "SELECT u.id, u.name, u.is_admin, u.all_libraries, t.last_used \
         FROM access_tokens t JOIN users u ON u.id = t.user_id \
         WHERE t.token = ? AND u.is_disabled = 0",
    )
    .bind(token)
    .fetch_optional(pool)
    .await?;
    let Some((id, name, is_admin, all_libraries, last_used)) = row else {
        return Ok(None);
    };
    // Token parado além da validade: some do banco e o app volta à tela de login.
    if ttl_secs > 0 && now_secs() - last_used > ttl_secs {
        revoke_token(pool, token).await?;
        return Ok(None);
    }

    // Atualiza o último uso no máximo uma vez por minuto (evita escrita a cada requisição).
    if now_secs() - last_used > 60 {
        let _ = sqlx::query("UPDATE access_tokens SET last_used = ? WHERE token = ?")
            .bind(now_secs())
            .bind(token)
            .execute(pool)
            .await;
    }

    let allowed_libraries = if all_libraries {
        None
    } else {
        let libs: Vec<(String,)> = sqlx::query_as("SELECT library_id FROM user_libraries WHERE user_id = ?")
            .bind(&id)
            .fetch_all(pool)
            .await?;
        Some(libs.into_iter().map(|l| l.0).collect())
    };
    Ok(Some(AuthUser { id, name, is_admin, allowed_libraries }))
}

/// Dispositivo conectado (um token). O token em si nunca sai do servidor.
#[derive(Debug, sqlx::FromRow)]
pub struct SessionRow {
    pub id: i64,
    pub user_id: String,
    pub user_name: String,
    pub client: Option<String>,
    pub device: Option<String>,
    pub device_id: Option<String>,
    pub created_at: i64,
    pub last_used: i64,
}

pub async fn list_sessions(pool: &DbPool) -> Result<Vec<SessionRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT t.rowid AS id, t.user_id, u.name AS user_name, t.client, t.device, t.device_id, \
                t.created_at, t.last_used \
         FROM access_tokens t JOIN users u ON u.id = t.user_id ORDER BY t.last_used DESC",
    )
    .fetch_all(pool)
    .await
}

/// Encerra uma sessão; `true` se ela existia.
pub async fn revoke_session(pool: &DbPool, id: i64) -> Result<bool, sqlx::Error> {
    let r = sqlx::query("DELETE FROM access_tokens WHERE rowid = ?").bind(id).execute(pool).await?;
    Ok(r.rows_affected() > 0)
}

pub async fn revoke_all_sessions(pool: &DbPool) -> Result<u64, sqlx::Error> {
    Ok(sqlx::query("DELETE FROM access_tokens").execute(pool).await?.rows_affected())
}

/// Grava (ou, com `None`, remove) a foto de perfil. Devolve o arquivo anterior, para apagar do disco.
pub async fn set_avatar(pool: &DbPool, id: &str, file: Option<&str>) -> Result<Option<String>, UserError> {
    let user = get(pool, id).await?.ok_or(UserError::NotFound)?;
    sqlx::query("UPDATE users SET avatar = ? WHERE id = ?").bind(file).bind(id).execute(pool).await?;
    Ok(user.avatar)
}

pub async fn revoke_token(pool: &DbPool, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM access_tokens WHERE token = ?").bind(token).execute(pool).await?;
    Ok(())
}

pub async fn revoke_user_tokens(pool: &DbPool, user_id: &str, keep: Option<&str>) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM access_tokens WHERE user_id = ? AND token != ?")
        .bind(user_id)
        .bind(keep.unwrap_or(""))
        .execute(pool)
        .await?;
    Ok(())
}

/// O usuário pode acessar o item? (a biblioteca do item precisa estar entre as permitidas)
pub async fn item_allowed(pool: &DbPool, user: &AuthUser, item_id: &str) -> Result<bool, sqlx::Error> {
    if user.allowed_libraries.is_none() {
        return Ok(true);
    }
    let row: Option<(Option<String>,)> =
        sqlx::query_as("SELECT library_id FROM items WHERE id = ?").bind(item_id).fetch_optional(pool).await?;
    Ok(match row {
        Some((lib,)) => user.can_access_library(lib.as_deref()),
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_roundtrip() {
        let h = hash_password("segredo").unwrap();
        assert!(h.starts_with("$argon2"));
        assert!(verify_password(&h, "segredo"));
        assert!(!verify_password(&h, "Segredo"));
        assert!(!verify_password("lixo", "segredo"));
        // salts diferentes: mesmo texto, hashes diferentes
        assert_ne!(h, hash_password("segredo").unwrap());
        assert!(matches!(hash_password(&"x".repeat(MAX_PASSWORD_LEN + 1)), Err(UserError::PasswordTooLong)));
        assert!(!verify_password(&h, &"x".repeat(MAX_PASSWORD_LEN + 1)));
    }

    #[test]
    fn name_validation() {
        assert!(validate_name("  ana ").is_ok());
        assert_eq!(validate_name("  ana ").unwrap(), "ana");
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("a\nb").is_err());
        assert!(validate_name(&"x".repeat(65)).is_err());
    }

    #[test]
    fn library_access() {
        let all = AuthUser { id: "u".into(), name: "u".into(), is_admin: false, allowed_libraries: None };
        assert!(all.can_access_library(Some("x")) && all.can_access_library(None));
        assert!(all.library_clause("i.library_id").is_none());

        let some = AuthUser { allowed_libraries: Some(vec!["a".into(), "b".into()]), ..all.clone() };
        assert!(some.can_access_library(Some("a")));
        assert!(!some.can_access_library(Some("c")));
        assert!(!some.can_access_library(None));
        let (sql, binds) = some.library_clause("i.library_id").unwrap();
        assert_eq!(sql, "i.library_id IN (?, ?)");
        assert_eq!(binds, vec!["a", "b"]);

        let none = AuthUser { allowed_libraries: Some(vec![]), ..all };
        assert_eq!(none.library_clause("x").unwrap().0, "0 = 1");
    }

    #[test]
    fn limiter_locks_after_failures() {
        let key = format!("teste|{}", Uuid::new_v4());
        assert!(limiter_check(&key).is_ok());
        for _ in 0..MAX_FAILURES {
            limiter_fail(&key);
        }
        assert!(limiter_check(&key).unwrap_err() > 0);
        limiter_ok(&key);
        assert!(limiter_check(&key).is_ok());
    }

    async fn test_pool(tag: &str) -> (DbPool, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("rf_users_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.join("t.db").to_string_lossy().replace('\\', "/"));
        (crate::db::init_db(&url).await.unwrap(), dir)
    }

    #[tokio::test]
    async fn user_lifecycle() {
        let (pool, dir) = test_pool("life").await;
        ensure_seed_admin(&pool).await.unwrap();
        ensure_seed_admin(&pool).await.unwrap(); // idempotente
        assert_eq!(list(&pool).await.unwrap().len(), 1);

        let ana = create(&pool, "Ana", Some("teste123"), false).await.unwrap();
        assert!(ana.has_password && !ana.is_admin);
        assert!(matches!(create(&pool, "ANA", None, false).await, Err(UserError::NameExists)));
        assert!(matches!(create(&pool, " ", None, false).await, Err(UserError::InvalidName)));

        // login
        let u = authenticate(&pool, "ana", "teste123", "k1").await.unwrap();
        assert_eq!(u.id, ana.id);
        assert!(matches!(authenticate(&pool, "ana", "errada", "k2").await, Err(AuthError::BadCredentials)));
        assert!(matches!(authenticate(&pool, "fantasma", "x", "k3").await, Err(AuthError::BadCredentials)));
        // Admin sem senha entra direto
        assert!(authenticate(&pool, "Admin", "", "k4").await.is_ok());
        // Conta sem senha não entra por origem fora da rede local.
        assert!(matches!(
            authenticate_from(&pool, "Admin", "", "k4b", false).await,
            Err(AuthError::BadCredentials)
        ));
        assert!(authenticate_from(&pool, "Admin", "", "k4c", true).await.is_ok());

        // bloqueio por tentativas
        for _ in 0..MAX_FAILURES {
            let _ = authenticate(&pool, "ana", "x", "brute").await;
        }
        assert!(matches!(authenticate(&pool, "ana", "teste123", "brute").await, Err(AuthError::Locked(_))));

        // tokens
        let tok = create_token(&pool, &ana.id, &DeviceInfo::default()).await.unwrap();
        assert_eq!(tok.len(), 64);
        let au = lookup_token(&pool, &tok).await.unwrap().unwrap();
        assert_eq!(au.name, "Ana");
        assert!(!au.is_admin && au.allowed_libraries.is_none());
        assert!(lookup_token(&pool, "inexistente").await.unwrap().is_none());

        // sessões: listagem sem o token, expiração por inatividade e encerramento
        let sessions = list_sessions(&pool).await.unwrap();
        let mine = sessions.iter().find(|s| s.user_id == ana.id).expect("sessão da Ana");
        assert_eq!(mine.user_name, "Ana");
        sqlx::query("UPDATE access_tokens SET last_used = ? WHERE token = ?")
            .bind(now_secs() - 40 * 86_400)
            .bind(&tok)
            .execute(&pool)
            .await
            .unwrap();
        // com TTL de 30 dias o token parado há 40 expira e é apagado (sem TTL nunca expira)
        assert!(lookup_token_with_ttl(&pool, &tok, 30 * 86_400).await.unwrap().is_none());
        assert!(lookup_token_with_ttl(&pool, &tok, 0).await.unwrap().is_none());
        let t3 = create_token(&pool, &ana.id, &DeviceInfo::default()).await.unwrap();
        let id3 = list_sessions(&pool).await.unwrap().into_iter().find(|s| s.user_id == ana.id).unwrap().id;
        assert!(revoke_session(&pool, id3).await.unwrap());
        assert!(!revoke_session(&pool, id3).await.unwrap());
        assert!(lookup_token(&pool, &t3).await.unwrap().is_none());

        // senha nova revoga tokens antigos
        set_password(&pool, &ana.id, Some("nova"), None).await.unwrap();
        assert!(lookup_token(&pool, &tok).await.unwrap().is_none());
        assert!(authenticate(&pool, "ana", "nova", "k5").await.is_ok());
        assert!(authenticate(&pool, "ana", "teste123", "k6").await.is_err());

        // desativar impede login e derruba tokens
        let tok2 = create_token(&pool, &ana.id, &DeviceInfo::default()).await.unwrap();
        set_policy(&pool, &ana.id, PolicyUpdate { is_disabled: Some(true), ..Default::default() }).await.unwrap();
        assert!(lookup_token(&pool, &tok2).await.unwrap().is_none());
        assert!(matches!(authenticate(&pool, "ana", "nova", "k7").await, Err(AuthError::Disabled)));

        rename(&pool, &ana.id, "Ana Maria").await.unwrap();
        assert!(get_by_name(&pool, "ana maria").await.unwrap().is_some());

        delete(&pool, &ana.id).await.unwrap();
        assert!(get(&pool, &ana.id).await.unwrap().is_none());
        assert!(matches!(delete(&pool, &ana.id).await, Err(UserError::NotFound)));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn last_admin_is_protected() {
        let (pool, dir) = test_pool("admin").await;
        ensure_seed_admin(&pool).await.unwrap();
        assert!(matches!(delete(&pool, ADMIN_ID).await, Err(UserError::LastAdmin)));
        let demote = PolicyUpdate { is_admin: Some(false), ..Default::default() };
        assert!(matches!(set_policy(&pool, ADMIN_ID, demote).await, Err(UserError::LastAdmin)));
        let disable = PolicyUpdate { is_disabled: Some(true), ..Default::default() };
        assert!(matches!(set_policy(&pool, ADMIN_ID, disable).await, Err(UserError::LastAdmin)));

        // com um segundo admin, o primeiro pode ser rebaixado
        let b = create(&pool, "Beto", None, true).await.unwrap();
        set_policy(&pool, ADMIN_ID, PolicyUpdate { is_admin: Some(false), ..Default::default() }).await.unwrap();
        assert!(matches!(delete(&pool, &b.id).await, Err(UserError::LastAdmin)));
        assert!(!any_admin_has_password(&pool).await);
        set_password(&pool, &b.id, Some("x"), None).await.unwrap();
        assert!(any_admin_has_password(&pool).await);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn library_restrictions_and_item_access() {
        let (pool, dir) = test_pool("libs").await;
        ensure_seed_admin(&pool).await.unwrap();
        sqlx::query("INSERT INTO libraries (id, name, collection_type) VALUES ('L1','Um','movies'), ('L2','Dois','movies')")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO items (id, name, path, media_type, library_id) VALUES ('i1','a','/a','Movie','L1'), ('i2','b','/b','Movie','L2')")
            .execute(&pool).await.unwrap();

        let kid = create(&pool, "Kid", None, false).await.unwrap();
        set_policy(&pool, &kid.id, PolicyUpdate {
            all_libraries: Some(false),
            libraries: Some(vec!["L1".into(), "inexistente".into()]),
            ..Default::default()
        }).await.unwrap();
        let tok = create_token(&pool, &kid.id, &DeviceInfo::default()).await.unwrap();
        let au = lookup_token(&pool, &tok).await.unwrap().unwrap();
        assert_eq!(au.allowed_libraries, Some(vec!["L1".to_string()]));
        assert!(item_allowed(&pool, &au, "i1").await.unwrap());
        assert!(!item_allowed(&pool, &au, "i2").await.unwrap());
        assert!(!item_allowed(&pool, &au, "nao-existe").await.unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }
}
