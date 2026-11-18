//! Administração: painel web (`/admin`), API do painel e as rotas de biblioteca do Jellyfin
//! (`/Library/VirtualFolders*`).
//!
//! Acesso: só clientes de rede privada/loopback (o servidor é pensado para a rede local) e, se
//! `RUSTYFIN_ADMIN_PASSWORD` estiver definida, senha HTTP Basic nas rotas `/admin*`.

use crate::{
    config::AppConfig,
    db::DbPool,
    libraries::{self, LibError, Library},
    library::{self, ScanTrigger},
    revisao_acervo::{self, MapaRetomada, Previa, Resolucao, Retomada, Revisao},
    settings, transcode,
    users::{self, PolicyUpdate},
};
use axum::{
    extract::{ConnectInfo, Query, RawQuery, Request, State},
    http::{header, StatusCode},
    middleware::{from_fn_with_state, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub struct AdminState {
    pub config: Arc<AppConfig>,
    pub pool: DbPool,
    pub scan: ScanTrigger,
}

pub fn router(config: Arc<AppConfig>, pool: DbPool, scan: ScanTrigger) -> Router {
    let state = AdminState { config, pool, scan };
    Router::new()
        .route("/admin", get(page))
        .route("/admin/", get(page))
        .route("/admin/favicon.png", get(favicon))
        .route("/admin/favicon-dark.png", get(favicon_dark))
        .route("/admin/logo.png", get(logo))
        .route("/admin/logo-dark.png", get(logo_dark))
        .route("/admin/api/status", get(status))
        .route("/admin/api/reproducao/perfis", get(super::reproducao_admin::listar_perfis))
        .route("/admin/api/reproducao/titulos", get(super::reproducao_admin::buscar_titulos))
        .route("/admin/api/reproducao/simular", post(super::reproducao_admin::simular))
        .route("/admin/api/legendas/:id", get(super::legendas_admin::faixas))
        .route("/admin/api/legendas/:id/video", get(super::legendas_admin::video))
        .route("/admin/api/legendas/:id/:indice/previa", post(super::legendas_admin::previa))
        .route(
            "/admin/api/legendas/:id/:indice",
            get(super::legendas_admin::detalhe)
                .put(super::legendas_admin::salvar)
                .delete(super::legendas_admin::excluir),
        )
        .route("/admin/api/acervo", get(super::acervo_admin::listar))
        .route("/admin/api/acervo/:id/capa", get(super::acervo_admin::capa))
        .route("/admin/api/acervo/revisoes", get(listar_revisoes))
        .route("/admin/api/acervo/revisoes/:id/previa", post(prever_retomada))
        .route("/admin/api/acervo/revisoes/:id", post(resolver_revisao))
        .route("/admin/api/cenas/:id", post(super::cenas::indexar))
        .route("/admin/api/acervo/audio/:id", post(indexar_audio))
        .route("/admin/api/preparos", get(super::preparos::listar))
        .route("/admin/api/preparos/:chave", post(super::preparos::iniciar))
        .route("/admin/api/settings", get(get_settings).put(put_settings))
        .route("/admin/api/browse", get(browse))
        .route("/admin/api/scan", post(scan_now))
        .route("/admin/api/desligar", post(desligar))
        .route(
            "/Library/VirtualFolders",
            get(list_folders).post(create_folder).delete(delete_folder),
        )
        .route("/Library/VirtualFolders/Paths", post(add_path).delete(remove_path))
        .route("/Library/VirtualFolders/Name", post(rename_folder))
        // Mesmas rotas sob /admin: é o que o painel usa (a senha Basic vale para todo /admin)
        .route(
            "/admin/api/folders",
            get(list_folders).post(create_folder).delete(delete_folder),
        )
        .route("/admin/api/folders/paths", post(add_path).delete(remove_path))
        .route("/admin/api/folders/name", post(rename_folder))
        // Usuários
        .route("/admin/api/sessions", get(sessions_list).delete(sessions_revoke_all))
        .route("/admin/api/sessions/:id", axum::routing::delete(sessions_revoke))
        .route("/admin/api/users", get(users_list).post(users_create))
        .route("/admin/api/users/:id", axum::routing::delete(users_delete))
        .route(
            "/admin/api/users/:id/avatar",
            post(users_avatar_set)
                .delete(users_avatar_clear)
                .layer(axum::extract::DefaultBodyLimit::max(crate::api::avatars::MAX_BYTES)),
        )
        .route("/admin/api/users/:id/password", post(users_password))
        .route("/admin/api/users/:id/policy", post(users_policy))
        .route("/admin/api/users/:id/rename", post(users_rename))
        .layer(from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

// ---------------------------------------------------------------- acesso

pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private(IpAddr::V4(v4));
            }
            let seg0 = v6.segments()[0];
            v6.is_loopback() || (seg0 & 0xfe00) == 0xfc00 || (seg0 & 0xffc0) == 0xfe80
        }
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Confere `Authorization: Basic base64(usuario:senha)` (o usuário é ignorado).
pub fn basic_password_ok(header_value: Option<&str>, password: &str) -> bool {
    let Some(encoded) = header_value.and_then(|h| h.strip_prefix("Basic ")) else {
        return false;
    };
    let Ok(decoded) = STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let text = String::from_utf8_lossy(&decoded);
    let given = text.split_once(':').map_or("", |(_, p)| p);
    constant_time_eq(given.as_bytes(), password.as_bytes())
}

enum Access {
    Allowed,
    Unauthorized,
    Forbidden,
}

/// `Authorization: Basic ...` → (usuário, senha)
fn basic_credentials(header_value: Option<&str>) -> Option<(String, String)> {
    let encoded = header_value?.strip_prefix("Basic ")?;
    let decoded = STANDARD.decode(encoded.trim()).ok()?;
    let text = String::from_utf8_lossy(&decoded).to_string();
    text.split_once(':').map(|(u, p)| (u.to_string(), p.to_string()))
}

/// Quem pode administrar:
/// 1. token de usuário administrador (apps como o Moonfin); token de usuário comum → proibido;
/// 2. se houver `RUSTYFIN_ADMIN_PASSWORD` ou algum administrador com senha, é preciso HTTP Basic
///    (a senha do ambiente ou usuário+senha de um administrador);
/// 3. sem nenhuma senha de administrador, a rede local basta.
async fn panel_access(st: &AdminState, headers: &axum::http::HeaderMap, query: Option<&str>) -> Access {
    let creds = crate::api::auth::extract_credentials(headers, query);
    if let Some(token) = creds.token {
        if let Ok(Some(u)) = users::lookup_token(&st.pool, &token).await {
            return if u.is_admin { Access::Allowed } else { Access::Forbidden };
        }
    }

    let env_pw = st.config.admin_password.as_deref();
    let admin_has_pw = users::any_admin_has_password(&st.pool).await;
    if env_pw.is_none() && !admin_has_pw {
        return Access::Allowed;
    }

    let auth = headers.get(header::AUTHORIZATION).and_then(|h| h.to_str().ok());
    if let Some(pw) = env_pw {
        if basic_password_ok(auth, pw) {
            return Access::Allowed;
        }
    }
    if admin_has_pw {
        if let Some((name, password)) = basic_credentials(auth) {
            let key = format!("painel|{}", name.to_lowercase());
            if let Ok(u) = users::authenticate(&st.pool, &name, &password, &key).await {
                if u.is_admin {
                    return Access::Allowed;
                }
            }
        }
    }
    Access::Unauthorized
}

async fn guard(
    State(st): State<AdminState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if !is_private(addr.ip()) {
        return (StatusCode::FORBIDDEN, "A administração só está disponível na rede local.").into_response();
    }
    // Vale para o painel e para as rotas de biblioteca do Jellyfin (criam/apagam bibliotecas).
    let path = req.uri().path();
    if path.starts_with("/admin") || path.starts_with("/Library/VirtualFolders") {
        match panel_access(&st, req.headers(), req.uri().query()).await {
            Access::Allowed => {}
            Access::Forbidden => {
                return (StatusCode::FORBIDDEN, "Apenas administradores.").into_response();
            }
            Access::Unauthorized => {
                return Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header(header::WWW_AUTHENTICATE, "Basic realm=\"Rustyfin Admin\"")
                    .body(axum::body::Body::from("Login de administrador necessário."))
                    .unwrap_or_else(|_| StatusCode::UNAUTHORIZED.into_response());
            }
        }
    }
    next.run(req).await
}

// ---------------------------------------------------------------- helpers

fn err_response(e: LibError) -> Response {
    let status = match &e {
        LibError::EmptyName | LibError::InvalidType | LibError::InvalidPath(_) => StatusCode::BAD_REQUEST,
        LibError::NameExists | LibError::PathExists => StatusCode::CONFLICT,
        LibError::NotFound => StatusCode::NOT_FOUND,
        LibError::Db(err) => {
            tracing::error!("Erro de banco de dados: {}", err);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, Json(json!({ "Message": e.to_string() }))).into_response()
}

fn db_error(e: sqlx::Error) -> Response {
    tracing::error!("Erro de banco de dados: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

fn query_pairs(raw: Option<String>) -> Vec<(String, String)> {
    raw.and_then(|r| serde_urlencoded::from_str::<Vec<(String, String)>>(&r).ok())
        .unwrap_or_default()
}

/// Primeiro valor da chave (sem diferenciar maiúsculas).
fn param<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(key))
        .map(|(_, v)| v.as_str())
}

fn body_str<'a>(body: &'a Value, pointers: &[&str]) -> Option<&'a str> {
    pointers.iter().find_map(|p| body.pointer(p).and_then(|v| v.as_str()))
}

fn folder_json(l: &Library) -> Value {
    json!({
        "Name": l.name,
        "Locations": l.paths,
        "CollectionType": l.collection_type,
        "LibraryOptions": {
            "PathInfos": l.paths.iter().map(|p| json!({ "Path": p })).collect::<Vec<_>>()
        },
        "ItemId": l.id,
        "Id": l.id,
        "PrimaryImageItemId": null,
        "RefreshProgress": null,
        "RefreshStatus": "Idle"
    })
}

// ---------------------------------------------------------------- /Library/VirtualFolders

async fn list_folders(State(st): State<AdminState>) -> Response {
    match libraries::list(&st.pool).await {
        Ok(libs) => Json(libs.iter().map(folder_json).collect::<Vec<_>>()).into_response(),
        Err(e) => db_error(e),
    }
}

async fn create_folder(
    State(st): State<AdminState>,
    RawQuery(raw): RawQuery,
    body: Option<Json<Value>>,
) -> Response {
    let pairs = query_pairs(raw);
    let name = param(&pairs, "name").unwrap_or("");
    let ctype = param(&pairs, "collectionType").unwrap_or("movies");

    let mut paths: Vec<String> = pairs
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("paths"))
        .flat_map(|(_, v)| v.split(',').map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect();
    if let Some(Json(b)) = &body {
        if let Some(infos) = b.pointer("/LibraryOptions/PathInfos").and_then(|v| v.as_array()) {
            paths.extend(infos.iter().filter_map(|i| i.get("Path").and_then(|p| p.as_str())).map(String::from));
        }
    }

    match libraries::create(&st.pool, name, ctype, &paths).await {
        Ok(_) => {
            library::request_scan_now(&st.scan);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err_response(e),
    }
}

async fn delete_folder(State(st): State<AdminState>, RawQuery(raw): RawQuery) -> Response {
    let pairs = query_pairs(raw);
    match libraries::delete(&st.pool, param(&pairs, "name").unwrap_or("")).await {
        Ok(()) => {
            library::request_scan_now(&st.scan);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err_response(e),
    }
}

async fn add_path(
    State(st): State<AdminState>,
    RawQuery(raw): RawQuery,
    body: Option<Json<Value>>,
) -> Response {
    let pairs = query_pairs(raw);
    let b = body.map_or(Value::Null, |Json(v)| v);
    let name = param(&pairs, "name").or_else(|| body_str(&b, &["/Name"])).unwrap_or("");
    let path = param(&pairs, "path")
        .or_else(|| body_str(&b, &["/PathInfo/Path", "/Path"]))
        .unwrap_or("");
    match libraries::add_path(&st.pool, name, path).await {
        Ok(()) => {
            library::request_scan_now(&st.scan);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err_response(e),
    }
}

async fn remove_path(State(st): State<AdminState>, RawQuery(raw): RawQuery) -> Response {
    let pairs = query_pairs(raw);
    match libraries::remove_path(
        &st.pool,
        param(&pairs, "name").unwrap_or(""),
        param(&pairs, "path").unwrap_or(""),
    )
    .await
    {
        Ok(()) => {
            library::request_scan_now(&st.scan);
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => err_response(e),
    }
}

async fn rename_folder(State(st): State<AdminState>, RawQuery(raw): RawQuery) -> Response {
    let pairs = query_pairs(raw);
    match libraries::rename(
        &st.pool,
        param(&pairs, "name").unwrap_or(""),
        param(&pairs, "newName").unwrap_or(""),
    )
    .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}

// ---------------------------------------------------------------- usuários (painel)

fn user_json(u: &users::User) -> Value {
    json!({
        "id": u.id, "name": u.name, "hasPassword": u.has_password, "isAdmin": u.is_admin,
        "isDisabled": u.is_disabled, "isHidden": u.is_hidden, "allLibraries": u.all_libraries,
        "libraries": u.libraries, "avatarTag": u.avatar.as_deref().map(crate::api::avatars::image_tag)
    })
}

async fn sessions_list(State(st): State<AdminState>) -> Response {
    match users::list_sessions(&st.pool).await {
        Ok(rows) => Json(
            rows.iter()
                .map(|s| {
                    json!({
                        "id": s.id,
                        "userId": s.user_id,
                        "userName": s.user_name,
                        "client": s.client,
                        "device": s.device,
                        "deviceId": s.device_id,
                        "createdAt": s.created_at,
                        "lastUsed": s.last_used,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => db_error(e),
    }
}

async fn sessions_revoke(State(st): State<AdminState>, axum::extract::Path(id): axum::extract::Path<i64>) -> Response {
    match users::revoke_session(&st.pool, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => db_error(e),
    }
}

async fn sessions_revoke_all(State(st): State<AdminState>) -> Response {
    match users::revoke_all_sessions(&st.pool).await {
        Ok(n) => Json(json!({ "revoked": n })).into_response(),
        Err(e) => db_error(e),
    }
}

async fn users_list(State(st): State<AdminState>) -> Response {
    match users::list(&st.pool).await {
        Ok(list) => Json(list.iter().map(user_json).collect::<Vec<_>>()).into_response(),
        Err(e) => db_error(e),
    }
}

async fn users_create(State(st): State<AdminState>, Json(body): Json<Value>) -> Response {
    let name = body.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let password = body.get("password").and_then(|v| v.as_str());
    let is_admin = body.get("isAdmin").and_then(serde_json::Value::as_bool).unwrap_or(false);
    match users::create(&st.pool, name, password, is_admin).await {
        Ok(u) => Json(user_json(&u)).into_response(),
        Err(e) => crate::api::auth::user_error_response(e),
    }
}

async fn users_delete(State(st): State<AdminState>, axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    let avatar = users::get(&st.pool, &id).await.ok().flatten().and_then(|u| u.avatar);
    match users::delete(&st.pool, &id).await {
        Ok(()) => {
            if let Some(a) = avatar {
                let _ = tokio::fs::remove_file(st.config.cache_dir.join("avatars").join(a)).await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => crate::api::auth::user_error_response(e),
    }
}

async fn users_avatar_set(
    State(st): State<AdminState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    body: axum::body::Bytes,
) -> Response {
    match crate::api::avatars::save(&st.config, &st.pool, &id, &body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(code) => code.into_response(),
    }
}

async fn users_avatar_clear(State(st): State<AdminState>, axum::extract::Path(id): axum::extract::Path<String>) -> Response {
    match crate::api::avatars::clear(&st.config, &st.pool, &id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(code) => code.into_response(),
    }
}

async fn users_password(
    State(st): State<AdminState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let pw = body.get("password").and_then(|v| v.as_str());
    match users::set_password(&st.pool, &id, pw, None).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => crate::api::auth::user_error_response(e),
    }
}

async fn users_policy(
    State(st): State<AdminState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let upd = PolicyUpdate {
        is_admin: body.get("isAdmin").and_then(serde_json::Value::as_bool),
        is_disabled: body.get("isDisabled").and_then(serde_json::Value::as_bool),
        is_hidden: body.get("isHidden").and_then(serde_json::Value::as_bool),
        all_libraries: body.get("allLibraries").and_then(serde_json::Value::as_bool),
        libraries: body
            .get("libraries")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).map(String::from).collect()),
    };
    match users::set_policy(&st.pool, &id, upd).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => crate::api::auth::user_error_response(e),
    }
}

async fn users_rename(
    State(st): State<AdminState>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<Value>,
) -> Response {
    match users::rename(&st.pool, &id, body.get("name").and_then(|v| v.as_str()).unwrap_or("")).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => crate::api::auth::user_error_response(e),
    }
}

// ---------------------------------------------------------------- API do painel

async fn page() -> impl IntoResponse {
    (
        [(header::CACHE_CONTROL, "no-store")],
        Html(include_str!("../admin.html")),
    )
}

/// Ícones do painel, embutidos no binário (gerados a partir do logotipo do projeto).
async fn favicon() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=86400")], include_bytes!("../assets/favicon.png").as_slice())
}

async fn favicon_dark() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=86400")], include_bytes!("../assets/favicon-dark.png").as_slice())
}

async fn logo_dark() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=86400")], include_bytes!("../assets/icon-dark.png").as_slice())
}

/// Fontes do painel e da busca de cenas (Geist, licença OFL), embutidas no binário. Públicas e imutáveis.
pub async fn fonte_geist() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/woff2"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], include_bytes!("../assets/fontes/geist-latin.woff2").as_slice())
}

pub async fn fonte_geist_mono() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "font/woff2"), (header::CACHE_CONTROL, "public, max-age=31536000, immutable")], include_bytes!("../assets/fontes/geist-mono-latin.woff2").as_slice())
}

async fn logo() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png"), (header::CACHE_CONTROL, "public, max-age=86400")], include_bytes!("../assets/icon.png").as_slice())
}

async fn listar_revisoes(State(st): State<AdminState>) -> Result<Json<Vec<Revisao>>, StatusCode> {
    revisao_acervo::listar(&st.pool).await.map(Json).map_err(|erro| {
        tracing::error!("Falha ao listar revisões do acervo: {erro}");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

#[derive(serde::Deserialize)]
#[serde(tag = "acao", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum AcaoRevisao {
    Transferir {
        item_id: String,
        #[serde(default)]
        retomadas: Vec<Retomada>,
    },
    ManterSeparados,
}

async fn resolver_revisao(
    State(st): State<AdminState>, axum::extract::Path(id): axum::extract::Path<String>,
    Json(acao): Json<AcaoRevisao>,
) -> Result<StatusCode, StatusCode> {
    let (destino, retomadas) = match &acao {
        AcaoRevisao::Transferir { item_id, retomadas } => {
            (Some(item_id.as_str()), retomadas.as_slice())
        }
        AcaoRevisao::ManterSeparados => (None, [].as_slice()),
    };
    match revisao_acervo::resolver(&st.pool, &id, destino, retomadas).await {
        Ok(Resolucao::Aplicada) => Ok(StatusCode::NO_CONTENT),
        Ok(erro) => Err(status_revisao(erro)),
        Err(erro) => {
            tracing::error!("Falha ao resolver revisão do acervo: {erro}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

fn status_revisao(erro: Resolucao) -> StatusCode {
    match erro {
        Resolucao::RevisaoAusente => StatusCode::NOT_FOUND,
        Resolucao::DestinoIndisponivel | Resolucao::PosicaoAlterada => StatusCode::CONFLICT,
        Resolucao::RetomadaInvalida => StatusCode::BAD_REQUEST,
        Resolucao::Aplicada => StatusCode::NO_CONTENT,
    }
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConsultaRetomada {
    item_id: String,
    mapa: Option<MapaRetomada>,
    #[serde(default)]
    automatico: bool,
}

async fn prever_retomada(
    State(st): State<AdminState>, axum::extract::Path(id): axum::extract::Path<String>,
    Json(consulta): Json<ConsultaRetomada>,
) -> Result<Json<Previa>, StatusCode> {
    revisao_acervo::previa(&st.pool, &id, &consulta.item_id, consulta.mapa.as_ref(), consulta.automatico)
        .await
        .map_err(|erro| {
            tracing::error!("Falha ao calcular a retomada: {erro}");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .map(Json)
        .map_err(status_revisao)
}

async fn indexar_audio(
    State(st): State<AdminState>, axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<StatusCode, StatusCode> {
    let id = crate::api::shows::normalize_id(&id);
    match crate::audio_edicoes::indexar(&st.pool, &id).await {
        Ok(true) => Ok(StatusCode::NO_CONTENT),
        Ok(false) => Err(StatusCode::UNPROCESSABLE_ENTITY),
        Err(erro) => {
            tracing::error!("Falha ao indexar áudio: {erro}");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Desliga o servidor de forma graciosa. Só aceita o próprio computador (o ícone da bandeja do Windows usa isto), mesmo
/// com a administração liberada para a rede local: outro aparelho da rede não deve poder desligar o servidor.
async fn desligar(ConnectInfo(addr): ConnectInfo<SocketAddr>) -> StatusCode {
    if !addr.ip().is_loopback() {
        return StatusCode::FORBIDDEN;
    }
    crate::shutdown::DESLIGAR.notify_one();
    StatusCode::ACCEPTED
}

async fn scan_now(State(st): State<AdminState>) -> StatusCode {
    library::request_scan_now(&st.scan);
    StatusCode::NO_CONTENT
}

async fn status(State(st): State<AdminState>) -> Response {
    let counts: Vec<(String, i64)> =
        match sqlx::query_as("SELECT media_type, COUNT(*) FROM items GROUP BY media_type")
            .fetch_all(&st.pool)
            .await
        {
            Ok(c) => c,
            Err(e) => return db_error(e),
        };
    let count_of = |kind: &str| counts.iter().find(|(k, _)| k == kind).map_or(0, |(_, n)| *n);
    let series: (i64,) = sqlx::query_as(
        "SELECT COUNT(DISTINCT series_id) FROM items WHERE media_type = 'Episode' AND series_id IS NOT NULL",
    )
    .fetch_one(&st.pool)
    .await
    .unwrap_or((0,));
    let libs = libraries::list(&st.pool).await.map_or(0, |l| l.len());
    // Arquivos cujo hash de conteúdo ainda é calculado em segundo plano: até lá, indexar áudio e cenas e ajustar legendas
    // respondem 409.
    let assinaturas_pendentes: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM items WHERE content_sha256 IS NULL")
        .fetch_one(&st.pool)
        .await
        .unwrap_or(0);
    let (running, pending, last_finished, last_files) = library::scan_status();
    let rt = settings::snapshot();

    Json(json!({
        "version": st.config.version,
        "serverName": rt.server_name,
        "serverId": st.config.server_id,
        "port": st.config.port,
        "publicUrl": st.config.public_url,
        "discovery": st.config.discovery_enabled,
        "passwordProtected": st.config.admin_password.is_some() || users::any_admin_has_password(&st.pool).await,
        "users": users::list(&st.pool).await.map_or(0, |u| u.len()),
        "ffmpeg": {
            "available": transcode::can_transcode(),
            "path": st.config.ffmpeg_path,
            "encoder": transcode::active_encoder(),
            "maxTranscodes": rt.max_transcodes
        },
        "tmdb": { "configured": rt.tmdb_api_key.is_some(), "language": rt.tmdb_language },
        "items": { "movies": count_of("Movie"), "episodes": count_of("Episode"), "series": series.0 },
        "libraries": libs,
        "scan": { "running": running, "pending": pending, "lastFinished": last_finished, "lastFiles": last_files, "signaturesPending": assinaturas_pendentes }
    }))
    .into_response()
}

fn settings_json() -> Value {
    let rt = settings::snapshot();
    json!({
        "tmdbConfigured": rt.tmdb_api_key.is_some(),
        "tmdbKeyPreview": rt.tmdb_api_key.as_deref().map(settings::mask_key),
        "tmdbLanguage": rt.tmdb_language,
        "serverName": rt.server_name,
        "scanIntervalSecs": rt.scan_interval_secs,
        "maxTranscodes": rt.max_transcodes
    })
}

async fn get_settings() -> Json<Value> {
    Json(settings_json())
}

/// `tmdbApiKey: ""` apaga a chave salva; campos ausentes não mudam.
async fn put_settings(State(st): State<AdminState>, Json(body): Json<Value>) -> Response {
    let key = body.get("tmdbApiKey").and_then(|v| v.as_str());
    let lang = body.get("tmdbLanguage").and_then(|v| v.as_str());
    if let Some(l) = lang {
        let l = l.trim();
        let ok = l.is_empty() || (l.len() <= 10 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        if !ok {
            return (StatusCode::BAD_REQUEST, Json(json!({ "Message": "Idioma inválido (ex.: pt-BR, en-US)." })))
                .into_response();
        }
    }
    let name = body.get("serverName").and_then(|v| v.as_str());
    let interval = body.get("scanIntervalSecs").and_then(serde_json::Value::as_u64);
    let max_tx = body.get("maxTranscodes").and_then(serde_json::Value::as_u64).map(|v| v as usize);
    let alteracoes = settings::Alteracoes {
        server_name: name,
        scan_interval_secs: interval,
        max_transcodes: max_tx,
        tmdb_api_key: key,
        tmdb_language: lang,
    };
    // Tudo numa gravação só: um pedido inválido ou uma falha do banco não aplica metade das configurações.
    match settings::update(&st.pool, &st.config, &alteracoes).await {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => return (StatusCode::BAD_REQUEST, Json(json!({ "Message": msg }))).into_response(),
        Err(e) => return db_error(e),
    }
    // Com chave nova, itens ainda sem metadados são buscados no próximo ciclo.
    library::request_scan_now(&st.scan);
    Json(settings_json()).into_response()
}

/// Lista subpastas para o seletor de pastas do painel.
async fn browse(
    State(st): State<AdminState>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let requested = params.get("path").map(|p| p.trim().to_string()).filter(|p| !p.is_empty());
    let start = match requested {
        Some(p) => PathBuf::from(p),
        None => default_browse_start(&st).await,
    };

    let result = tokio::task::spawn_blocking(move || list_dirs(&start)).await;
    match result {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(msg)) => (StatusCode::BAD_REQUEST, Json(json!({ "Message": msg }))).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn default_browse_start(st: &AdminState) -> PathBuf {
    if let Ok(libs) = libraries::list(&st.pool).await {
        if let Some(p) = libs.iter().flat_map(|l| l.paths.iter()).find(|p| Path::new(p).is_dir()) {
            return PathBuf::from(p);
        }
    }
    if let Some(d) = st.config.media_directories.iter().find(|d| d.is_dir()) {
        return std::path::absolute(d).unwrap_or_else(|_| d.clone());
    }
    if cfg!(windows) {
        PathBuf::from("C:\\")
    } else {
        PathBuf::from("/")
    }
}

fn list_dirs(dir: &Path) -> Result<Value, String> {
    if !dir.is_absolute() {
        return Err("O caminho precisa ser absoluto.".into());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| format!("Não foi possível abrir {}: {}", dir.display(), e))?;
    let mut dirs: Vec<(String, String)> = entries
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (!name.starts_with('.')).then(|| (name, e.path().to_string_lossy().to_string()))
        })
        .collect();
    dirs.sort_by_key(|(n, _)| n.to_lowercase());
    dirs.truncate(500);

    Ok(json!({
        "path": dir.to_string_lossy(),
        "parent": dir.parent().map(|p| p.to_string_lossy().to_string()),
        "dirs": dirs.into_iter().map(|(name, path)| json!({ "name": name, "path": path })).collect::<Vec<_>>()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ranges() {
        for ip in ["127.0.0.1", "10.1.2.3", "172.20.0.1", "192.168.1.71", "169.254.1.1", "::1", "fd00::1", "fe80::1", "::ffff:192.168.1.5"] {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "172.32.0.1", "2001:4860:4860::8888", "::ffff:8.8.8.8"] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn basic_auth() {
        let ok = format!("Basic {}", STANDARD.encode("qualquer:segredo"));
        assert!(basic_password_ok(Some(&ok), "segredo"));
        assert!(!basic_password_ok(Some(&ok), "outra"));
        assert!(!basic_password_ok(Some("Bearer x"), "segredo"));
        assert!(!basic_password_ok(Some("Basic !!!"), "segredo"));
        assert!(!basic_password_ok(None, "segredo"));
        let empty = format!("Basic {}", STANDARD.encode("u:"));
        assert!(!basic_password_ok(Some(&empty), "segredo"));
    }

    #[test]
    fn query_params() {
        let pairs = query_pairs(Some("name=Meus%20Filmes&paths=/a&paths=/b&collectionType=movies".into()));
        assert_eq!(param(&pairs, "NAME"), Some("Meus Filmes"));
        assert_eq!(pairs.iter().filter(|(k, _)| k == "paths").count(), 2);
        assert_eq!(param(&pairs, "ausente"), None);
    }

    #[test]
    fn lists_subdirectories_only() {
        let dir = std::env::temp_dir().join(format!("rf_browse_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Beta")).unwrap();
        std::fs::create_dir_all(dir.join("alfa")).unwrap();
        std::fs::create_dir_all(dir.join(".oculta")).unwrap();
        std::fs::write(dir.join("arquivo.txt"), b"x").unwrap();
        let v = list_dirs(&dir).unwrap();
        let names: Vec<&str> = v["dirs"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["alfa", "Beta"]);
        assert!(list_dirs(Path::new("relativo")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
