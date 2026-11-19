//! `GET /Videos/{id}/Trickplay/{largura}/{n}.jpg`: as folhas de miniaturas ao avançar (veja `trickplay.rs`).

use super::{auth::CurrentUser, shows::normalize_id};
use crate::{config::AppConfig, db::DbPool, trickplay, users};
use axum::{
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

pub(super) async fn folha(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    CurrentUser(user): CurrentUser,
    Path((id, largura, arquivo)): Path<(String, u32, String)>,
) -> Response {
    let id = normalize_id(&id);
    let Some(n) = arquivo.strip_suffix(".jpg").and_then(|n| n.parse::<u32>().ok()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match users::item_allowed(&pool, &user, &id).await {
        Ok(true) => {}
        Ok(false) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
    let Some(caminho) = trickplay::arquivo_da_folha(&config.cache_dir, &id, largura, n) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(&caminho).await {
        Ok(bytes) => (
            [(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "private, max-age=86400")],
            bytes,
        )
            .into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}
