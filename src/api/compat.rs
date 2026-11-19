//! Rotas que clientes Jellyfin 10.9+ chamam durante a conexão e a navegação:
//! marcar como assistido/favorito, listas vazias (gêneros, estúdios...), teste de banda etc.

use crate::{
    api::{auth::CurrentUser, items::db_err, shows::normalize_id},
    users::{self, AuthUser},
    db::DbPool,
    models::{QueryResult, UserItemDataDto},
};
use axum::{
    body::Body,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use std::collections::HashMap;

/// Listas que o Rustyfin ainda não modela (ancestrais, extras, trailers locais): `[]`.
pub async fn empty_array() -> impl IntoResponse {
    Json(Vec::<serde_json::Value>::new())
}

pub async fn encoding_configuration() -> impl IntoResponse {
    Json(json!({
        "EnableHardwareEncoding": false,
        "HardwareAccelerationType": "none",
        "EnableTonemapping": false,
        "H264Crf": 23
    }))
}

/// WebSocket `/socket`: os clientes o usam para keep-alive e comandos remotos. Aceitamos a conexão
/// (sem comandos remotos) para evitar loops de reconexão e mantemos o keep-alive no padrão Jellyfin.
pub async fn socket(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(handle_socket)
}

async fn handle_socket(mut socket: WebSocket) {
    let keep_alive = json!({ "MessageType": "KeepAlive" }).to_string();
    let hello = json!({ "MessageType": "ForceKeepAlive", "Data": 60 }).to_string();
    if socket.send(Message::Text(hello)).await.is_err() {
        return;
    }
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
    tick.tick().await; // o primeiro tick é imediato
    loop {
        tokio::select! {
            msg = socket.recv() => match msg {
                Some(Ok(Message::Text(text))) => {
                    if text.contains("KeepAlive") && socket.send(Message::Text(keep_alive.clone())).await.is_err() {
                        break;
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                _ => {}
            },
            _ = tick.tick() => {
                if socket.send(Message::Text(keep_alive.clone())).await.is_err() {
                    break;
                }
            }
        }
    }
}

pub async fn branding_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], "")
}

pub async fn system_endpoint() -> impl IntoResponse {
    Json(json!({ "IsLocal": true, "IsInNetwork": true }))
}

/// Resposta padrão "sem resultados" para recursos que o Rustyfin ainda não modela.
pub async fn empty_query() -> impl IntoResponse {
    Json(QueryResult::<serde_json::Value> {
        items: vec![],
        total_record_count: 0,
        start_index: 0,
    })
}

pub async fn theme_media() -> impl IntoResponse {
    let empty = json!({ "Items": [], "TotalRecordCount": 0, "StartIndex": 0 });
    Json(json!({
        "ThemeVideosResult": empty,
        "ThemeSongsResult": empty,
        "SoundtrackSongsResult": empty
    }))
}

pub async fn sessions_list() -> impl IntoResponse {
    Json(Vec::<serde_json::Value>::new())
}

/// `/Playback/BitrateTest?size=N`: N bytes de zeros (limitado a 10 MB) para medir a banda.
pub async fn bitrate_test(Query(params): Query<HashMap<String, String>>) -> Response {
    const MAX: usize = 10 * 1024 * 1024;
    let size = params
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("size"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(500_000)
        .min(MAX);
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, size.to_string())
        .body(Body::from(vec![0u8; size]))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ---------------- assistido / favorito ----------------

/// Itens afetados: o próprio ID, ou todos os episódios se for série/temporada.
async fn target_item_ids(pool: &DbPool, user: &AuthUser, raw_id: &str) -> Result<Vec<String>, sqlx::Error> {
    let id = normalize_id(raw_id);
    let own: Option<(String,)> = sqlx::query_as("SELECT id FROM items WHERE id = ?")
        .bind(&id)
        .fetch_optional(pool)
        .await?;
    if own.is_some() {
        // Item de biblioteca sem permissão: como se não existisse.
        return Ok(if users::item_allowed(pool, user, &id).await? { vec![id] } else { vec![] });
    }
    let rows: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT id, library_id FROM items WHERE series_id = ?1 OR season_id = ?1",
    )
    .bind(&id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter(|(_, lib)| user.can_access_library(lib.as_deref()))
        .map(|(i, _)| i)
        .collect())
}

async fn set_played(pool: &DbPool, user: &AuthUser, raw_id: &str, played: bool) -> Result<UserItemDataDto, StatusCode> {
    let ids = target_item_ids(pool, user, raw_id).await.map_err(db_err)?;
    if ids.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    let mut tx = pool.begin().await.map_err(db_err)?;
    for id in &ids {
        sqlx::query(
            r"
            INSERT INTO playback_state (user_id, item_id, position_ticks, is_played, play_count, last_updated)
            VALUES (?, ?, 0, ?, ?, CURRENT_TIMESTAMP)
            ON CONFLICT(user_id, item_id) DO UPDATE SET
                is_played = excluded.is_played,
                position_ticks = 0,
                play_count = playback_state.play_count + excluded.play_count,
                last_updated = CURRENT_TIMESTAMP
            ",
        )
        .bind(&user.id)
        .bind(id)
        .bind(played)
        .bind(i32::from(played))
        .execute(&mut *tx)
        .await
        .map_err(db_err)?;
    }
    tx.commit().await.map_err(db_err)?;

    Ok(UserItemDataDto {
        play_count: i32::from(played),
        played,
        ..Default::default()
    })
}

pub async fn mark_played(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    Ok(Json(set_played(&pool, &user, &id, true).await?))
}

pub async fn mark_unplayed(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    Ok(Json(set_played(&pool, &user, &id, false).await?))
}

/// Variantes `/Users/{userId}/PlayedItems/{id}`
pub async fn mark_played_user(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path((_user, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    Ok(Json(set_played(&pool, &user, &id, true).await?))
}

pub async fn mark_unplayed_user(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path((_user, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    Ok(Json(set_played(&pool, &user, &id, false).await?))
}

async fn set_favorite(pool: &DbPool, user: &AuthUser, raw_id: &str, favorite: bool) -> Result<UserItemDataDto, StatusCode> {
    let id = normalize_id(raw_id);
    // Favoritos valem para filmes, episódios, séries e temporadas.
    if target_item_ids(pool, user, &id).await.map_err(db_err)?.is_empty() {
        return Err(StatusCode::NOT_FOUND);
    }
    sqlx::query(
        r"
        INSERT INTO playback_state (user_id, item_id, position_ticks, is_played, is_favorite)
        VALUES (?, ?, 0, 0, ?)
        ON CONFLICT(user_id, item_id) DO UPDATE SET is_favorite = excluded.is_favorite
        ",
    )
    .bind(&user.id)
    .bind(&id)
    .bind(favorite)
    .execute(pool)
    .await
    .map_err(db_err)?;
    Ok(UserItemDataDto { is_favorite: favorite, ..Default::default() })
}

pub async fn mark_favorite(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    Ok(Json(set_favorite(&pool, &user, &id, true).await?))
}

pub async fn unmark_favorite(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, StatusCode> {
    Ok(Json(set_favorite(&pool, &user, &id, false).await?))
}

pub async fn mark_favorite_user(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path((_user, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    Ok(Json(set_favorite(&pool, &user, &id, true).await?))
}

pub async fn unmark_favorite_user(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path((_user, id)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    Ok(Json(set_favorite(&pool, &user, &id, false).await?))
}
