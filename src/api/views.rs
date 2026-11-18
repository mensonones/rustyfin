use crate::{
    api::{auth::CurrentUser, items::db_err},
    config::AppConfig,
    db::DbPool,
    libraries::{self, Library},
    models::{BaseItemDto, QueryResult},
    users::AuthUser,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use std::sync::Arc;

fn collection_folder(config: &AppConfig, lib: &Library) -> BaseItemDto {
    BaseItemDto {
        id: lib.id.clone(),
        name: lib.name.clone(),
        server_id: config.server_id.clone(),
        r#type: "CollectionFolder".to_string(),
        is_folder: true,
        collection_type: Some(lib.jellyfin_type().to_string()),
        ..Default::default()
    }
}

/// `/Items/{id}` para o ID de uma biblioteca.
pub async fn view_by_id(config: &AppConfig, pool: &DbPool, id: &str) -> Option<BaseItemDto> {
    libraries::get_by_id(pool, id)
        .await
        .ok()
        .flatten()
        .map(|lib| collection_folder(config, &lib))
}

async fn views_result(
    config: &AppConfig,
    pool: &DbPool,
    user: &AuthUser,
) -> Result<Json<QueryResult<BaseItemDto>>, StatusCode> {
    let libs = libraries::list(pool).await.map_err(db_err)?;
    let items: Vec<BaseItemDto> = libs
        .iter()
        .filter(|l| user.can_access_library(Some(&l.id)))
        .map(|l| collection_folder(config, l))
        .collect();
    Ok(Json(QueryResult {
        total_record_count: items.len(),
        items,
        start_index: 0,
    }))
}

pub async fn get_user_views(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    Path(_user_id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, StatusCode> {
    views_result(&config, &pool, &user).await
}

/// `/UserViews` e `/Library/MediaFolders`: mesmas views, sem `userId` na rota.
pub async fn get_user_views_all(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, StatusCode> {
    views_result(&config, &pool, &user).await
}
