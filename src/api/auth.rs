//! Autenticação HTTP: extração do token, middleware, login, senha, logout e administração de
//! usuários no formato do Jellyfin.

use crate::{
    config::AppConfig,
    db::DbPool,
    models::{
        AuthenticateUserByName, AuthenticationResult, UserConfiguration, UserDto, UserPolicy,
    },
    users::{self, AuthError, AuthUser, DeviceInfo, PolicyUpdate, User, UserError},
};
use axum::{
    extract::{ConnectInfo, FromRequestParts, Path, Request, State},
    http::{header, request::Parts, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};

type AppState = (Arc<AppConfig>, DbPool);

// ---------------------------------------------------------------- credenciais

/// Dados do cliente enviados no cabeçalho (`MediaBrowser Client="..", Device="..", Token=".."`).
#[derive(Debug, Clone, Default)]
pub struct ClientCreds {
    pub token: Option<String>,
    pub device: DeviceInfo,
}

/// `Esquema k1="v1", k2="v2"` → mapa com chaves em minúsculas.
pub fn parse_kv_header(value: &str) -> HashMap<String, String> {
    let rest = value.trim();
    let rest = rest.split_once(char::is_whitespace).map(|(_, r)| r).unwrap_or(rest);
    let mut out = HashMap::new();
    let mut chars = rest.chars().peekable();
    loop {
        while matches!(chars.peek(), Some(c) if c.is_whitespace() || *c == ',') {
            chars.next();
        }
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c == ',' {
                break;
            }
            key.push(c);
            chars.next();
        }
        if chars.next() != Some('=') {
            break;
        }
        let mut val = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                val.push(c);
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == ',' {
                    break;
                }
                val.push(c);
                chars.next();
            }
        }
        let key = key.trim().to_lowercase();
        if !key.is_empty() {
            out.insert(key, val.trim().to_string());
        }
    }
    out
}

pub fn extract_credentials(headers: &HeaderMap, query: Option<&str>) -> ClientCreds {
    let header_str = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());

    let mut kv = HashMap::new();
    for name in ["authorization", "x-emby-authorization"] {
        if let Some(v) = header_str(name) {
            if v.contains('=') {
                for (k, val) in parse_kv_header(v) {
                    kv.entry(k).or_insert(val);
                }
            }
        }
    }

    let token = header_str("x-emby-token")
        .or_else(|| header_str("x-mediabrowser-token"))
        .map(String::from)
        .or_else(|| kv.get("token").cloned())
        .or_else(|| {
            query
                .and_then(|q| serde_urlencoded::from_str::<Vec<(String, String)>>(q).ok())
                .and_then(|pairs| {
                    pairs
                        .into_iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("api_key") || k.eq_ignore_ascii_case("apikey"))
                        .map(|(_, v)| v)
                })
        })
        .filter(|t| !t.is_empty());

    ClientCreds {
        token,
        device: DeviceInfo {
            client: kv.get("client").cloned(),
            device: kv.get("device").cloned(),
            device_id: kv.get("deviceid").cloned(),
        },
    }
}

// ---------------------------------------------------------------- middleware e extratores

/// Rotas acessíveis sem token (descoberta, login, imagens e segmentos HLS).
pub fn is_public(path: &str) -> bool {
    matches!(
        path,
        "/System/Info/Public"
            | "/System/Ping"
            | "/Users/Public"
            | "/Users/AuthenticateByName"
            | "/QuickConnect/Enabled"
            | "/Branding/Configuration"
            | "/Branding/Css"
    ) || path.starts_with("/admin") // tem guarda própria (api/admin.rs)
        || path.starts_with("/Library/VirtualFolders")
        || (path.starts_with("/Items/") && path.contains("/Images/"))
        || (path.starts_with("/Users/") && path.contains("/Images/")) // avatar: 404 em vez de 401
        || (path.starts_with("/Videos/") && path.contains("/hls/"))
}

pub async fn require_user(State(pool): State<DbPool>, mut req: Request, next: Next) -> Response {
    let creds = extract_credentials(req.headers(), req.uri().query());
    let public = is_public(req.uri().path());

    if let Some(token) = &creds.token {
        match users::lookup_token(&pool, token).await {
            Ok(Some(user)) => {
                req.extensions_mut().insert(user);
            }
            Ok(None) if !public => return StatusCode::UNAUTHORIZED.into_response(),
            Ok(None) => {}
            Err(e) => {
                tracing::error!("Erro ao validar token: {}", e);
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        }
    } else if !public {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    req.extensions_mut().insert(creds);
    next.run(req).await
}

/// Usuário autenticado (o middleware já rejeitou requisições sem token válido).
pub struct CurrentUser(pub AuthUser);

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for CurrentUser {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<AuthUser>()
            .cloned()
            .map(CurrentUser)
            .ok_or(StatusCode::UNAUTHORIZED)
    }
}

pub struct CurrentCreds(pub ClientCreds);

#[axum::async_trait]
impl<S: Send + Sync> FromRequestParts<S> for CurrentCreds {
    type Rejection = StatusCode;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(CurrentCreds(parts.extensions.get::<ClientCreds>().cloned().unwrap_or_default()))
    }
}

// ---------------------------------------------------------------- DTOs

pub fn user_dto(config: &AppConfig, u: &User) -> UserDto {
    UserDto {
        id: u.id.clone(),
        name: u.name.clone(),
        server_id: config.server_id.clone(),
        has_password: u.has_password,
        has_configured_password: u.has_password,
        has_configured_easy_password: false,
        enable_auto_login: !u.has_password,
        configuration: UserConfiguration {
            play_default_audio_track: true,
            subtitle_language_preference: None,
            display_missing_episodes: false,
        },
        policy: UserPolicy {
            is_administrator: u.is_admin,
            is_hidden: false,
            is_disabled: u.is_disabled,
            enable_media_playback: true,
            enable_audio_playback_transcoding: true,
            enable_video_playback_transcoding: true,
            enable_playback_remuxing: true,
            enable_content_downloading: true,
            enable_all_folders: u.all_libraries,
            enabled_folders: u.libraries.clone(),
        },
    }
}

fn db_error(e: sqlx::Error) -> Response {
    tracing::error!("Erro de banco de dados: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

pub fn user_error_response(e: UserError) -> Response {
    let status = match &e {
        UserError::InvalidName | UserError::PasswordTooLong => StatusCode::BAD_REQUEST,
        UserError::NameExists | UserError::LastAdmin => StatusCode::CONFLICT,
        UserError::NotFound => StatusCode::NOT_FOUND,
        UserError::Hash | UserError::Db(_) => {
            tracing::error!("Erro em usuários: {}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, Json(json!({ "Message": e.to_string() }))).into_response()
}

// ---------------------------------------------------------------- login

/// Tela de login: usuários ativos (como no Jellyfin, quando não estão ocultos).
pub async fn get_public_users(State((config, pool)): State<AppState>) -> Response {
    match users::list(&pool).await {
        Ok(list) => Json(
            list.iter()
                .filter(|u| !u.is_disabled)
                .map(|u| user_dto(&config, u))
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => db_error(e),
    }
}

pub async fn authenticate_by_name(
    State((config, pool)): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    CurrentCreds(creds): CurrentCreds,
    payload: Option<Json<AuthenticateUserByName>>,
) -> Response {
    let Some(Json(body)) = payload else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let name = body.username.unwrap_or_default();
    let password = body.pw.unwrap_or_default();
    if name.trim().is_empty() {
        return StatusCode::BAD_REQUEST.into_response();
    }

    let key = format!("{}|{}", addr.ip(), name.trim().to_lowercase());
    match users::authenticate(&pool, &name, &password, &key).await {
        Ok(user) => match users::create_token(&pool, &user.id, &creds.device).await {
            Ok(token) => Json(AuthenticationResult {
                user: user_dto(&config, &user),
                access_token: token,
                server_id: config.server_id.clone(),
            })
            .into_response(),
            Err(e) => db_error(e),
        },
        Err(AuthError::Locked(secs)) => (
            StatusCode::TOO_MANY_REQUESTS,
            [(header::RETRY_AFTER, secs.to_string())],
            "Muitas tentativas. Aguarde antes de tentar de novo.",
        )
            .into_response(),
        Err(AuthError::BadCredentials) | Err(AuthError::Disabled) => {
            (StatusCode::UNAUTHORIZED, "Usuário ou senha inválidos.").into_response()
        }
        Err(AuthError::Internal) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn logout(State((_, pool)): State<AppState>, CurrentCreds(creds): CurrentCreds) -> StatusCode {
    if let Some(token) = creds.token {
        let _ = users::revoke_token(&pool, &token).await;
    }
    StatusCode::NO_CONTENT
}

// ---------------------------------------------------------------- consulta

pub async fn get_current_user(
    State((config, pool)): State<AppState>,
    CurrentUser(me): CurrentUser,
) -> Response {
    match users::get(&pool, &me.id).await {
        Ok(Some(u)) => Json(user_dto(&config, &u)).into_response(),
        Ok(None) => StatusCode::UNAUTHORIZED.into_response(),
        Err(e) => db_error(e),
    }
}

fn normalize(id: &str) -> String {
    id.replace('-', "").to_lowercase()
}

pub async fn get_user_by_id(
    State((config, pool)): State<AppState>,
    CurrentUser(me): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    let id = normalize(&id);
    if id != me.id && !me.is_admin {
        return StatusCode::FORBIDDEN.into_response();
    }
    match users::get(&pool, &id).await {
        Ok(Some(u)) => Json(user_dto(&config, &u)).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => db_error(e),
    }
}

// ---------------------------------------------------------------- administração (token de admin)

fn require_admin(me: &AuthUser) -> Option<Response> {
    (!me.is_admin).then(|| StatusCode::FORBIDDEN.into_response())
}

pub async fn list_users(State((config, pool)): State<AppState>, CurrentUser(me): CurrentUser) -> Response {
    if let Some(r) = require_admin(&me) {
        return r;
    }
    match users::list(&pool).await {
        Ok(list) => Json(list.iter().map(|u| user_dto(&config, u)).collect::<Vec<_>>()).into_response(),
        Err(e) => db_error(e),
    }
}

/// `POST /Users/New` com `{ "Name": .., "Password": .. }`
pub async fn create_user(
    State((config, pool)): State<AppState>,
    CurrentUser(me): CurrentUser,
    Json(body): Json<Value>,
) -> Response {
    if let Some(r) = require_admin(&me) {
        return r;
    }
    let name = body.get("Name").and_then(|v| v.as_str()).unwrap_or("");
    let password = body.get("Password").and_then(|v| v.as_str());
    match users::create(&pool, name, password, false).await {
        Ok(u) => Json(user_dto(&config, &u)).into_response(),
        Err(e) => user_error_response(e),
    }
}

pub async fn delete_user(
    State((_, pool)): State<AppState>,
    CurrentUser(me): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    if let Some(r) = require_admin(&me) {
        return r;
    }
    match users::delete(&pool, &normalize(&id)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => user_error_response(e),
    }
}

/// Converte o corpo `UserPolicy` do Jellyfin em atualização parcial.
pub fn policy_from_json(body: &Value) -> PolicyUpdate {
    PolicyUpdate {
        is_admin: body.get("IsAdministrator").and_then(|v| v.as_bool()),
        is_disabled: body.get("IsDisabled").and_then(|v| v.as_bool()),
        all_libraries: body.get("EnableAllFolders").and_then(|v| v.as_bool()),
        libraries: body.get("EnabledFolders").and_then(|v| v.as_array()).map(|a| {
            a.iter().filter_map(|v| v.as_str()).map(|s| s.replace('-', "").to_lowercase()).collect()
        }),
    }
}

pub async fn update_policy(
    State((_, pool)): State<AppState>,
    CurrentUser(me): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    if let Some(r) = require_admin(&me) {
        return r;
    }
    match users::set_policy(&pool, &normalize(&id), policy_from_json(&body)).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => user_error_response(e),
    }
}

/// `POST /Users/{id}/Password` com `{ CurrentPw, NewPw, ResetPassword }`.
/// O próprio usuário informa a senha atual; um administrador pode redefinir a de outros.
pub async fn change_password(
    State((_, pool)): State<AppState>,
    CurrentUser(me): CurrentUser,
    CurrentCreds(creds): CurrentCreds,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let id = normalize(&id);
    let is_self = id == me.id;
    if !is_self && !me.is_admin {
        return StatusCode::FORBIDDEN.into_response();
    }
    let new_pw = body.get("NewPw").and_then(|v| v.as_str()).unwrap_or("");
    let current_pw = body.get("CurrentPw").and_then(|v| v.as_str()).unwrap_or("");
    let reset = body.get("ResetPassword").and_then(|v| v.as_bool()).unwrap_or(false);

    if is_self && !reset {
        match users::check_password(&pool, &id, current_pw).await {
            Ok(true) => {}
            Ok(false) => return (StatusCode::FORBIDDEN, "Senha atual incorreta.").into_response(),
            Err(e) => return db_error(e),
        }
    }
    let password = if reset { None } else { Some(new_pw) };
    match users::set_password(&pool, &id, password, if is_self { creds.token.as_deref() } else { None }).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => user_error_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn parses_mediabrowser_header() {
        let kv = parse_kv_header(
            r#"MediaBrowser Client="Moonfin", Device="SM-S711B", DeviceId="abc-123", Version="3.0.0", Token="tok123""#,
        );
        assert_eq!(kv["client"], "Moonfin");
        assert_eq!(kv["device"], "SM-S711B");
        assert_eq!(kv["deviceid"], "abc-123");
        assert_eq!(kv["token"], "tok123");
        // vírgulas dentro de aspas e valores sem aspas
        let kv = parse_kv_header(r#"Emby Device="TV, sala", Token=xyz"#);
        assert_eq!(kv["device"], "TV, sala");
        assert_eq!(kv["token"], "xyz");
        assert!(parse_kv_header("Bearer").is_empty());
    }

    #[test]
    fn token_sources() {
        let mut h = HeaderMap::new();
        h.insert("x-emby-token", HeaderValue::from_static("aaa"));
        assert_eq!(extract_credentials(&h, None).token.as_deref(), Some("aaa"));

        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static(r#"MediaBrowser Client="C", Token="bbb""#));
        let c = extract_credentials(&h, None);
        assert_eq!(c.token.as_deref(), Some("bbb"));
        assert_eq!(c.device.client.as_deref(), Some("C"));

        let mut h = HeaderMap::new();
        h.insert("x-emby-authorization", HeaderValue::from_static(r#"MediaBrowser Token="ccc""#));
        assert_eq!(extract_credentials(&h, None).token.as_deref(), Some("ccc"));

        let h = HeaderMap::new();
        assert_eq!(extract_credentials(&h, Some("a=1&ApiKey=ddd")).token.as_deref(), Some("ddd"));
        assert_eq!(extract_credentials(&h, Some("api_key=eee")).token.as_deref(), Some("eee"));
        assert_eq!(extract_credentials(&h, Some("x=1")).token, None);
        assert_eq!(extract_credentials(&h, Some("api_key=")).token, None);
    }

    #[test]
    fn public_paths() {
        for p in [
            "/System/Info/Public",
            "/Users/Public",
            "/Users/AuthenticateByName",
            "/Items/abc/Images/Primary",
            "/Users/abc/Images/Primary",
            "/Videos/abc/hls/0123456789abcdef/seg_00001.ts",
            "/admin",
            "/admin/api/status",
            "/Library/VirtualFolders",
        ] {
            assert!(is_public(p), "{p}");
        }
        for p in ["/Users/Me", "/Items", "/Users/abc/Items", "/Videos/abc/stream", "/System/Info", "/Sessions/Playing", "/Videos/abc/master.m3u8"] {
            assert!(!is_public(p), "{p}");
        }
    }

    #[test]
    fn policy_parsing() {
        let p = policy_from_json(&json!({
            "IsAdministrator": true, "EnableAllFolders": false,
            "EnabledFolders": ["AB-CD", "ef"]
        }));
        assert_eq!(p.is_admin, Some(true));
        assert_eq!(p.is_disabled, None);
        assert_eq!(p.all_libraries, Some(false));
        assert_eq!(p.libraries, Some(vec!["abcd".to_string(), "ef".to_string()]));
    }
}
