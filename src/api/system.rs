use crate::{
    config::AppConfig,
    models::{PublicSystemInfo, SystemInfo},
};
use axum::{extract::State, response::IntoResponse, Json};
use std::sync::Arc;

pub async fn get_system_info_public(State(config): State<Arc<AppConfig>>) -> impl IntoResponse {
    Json(PublicSystemInfo {
        id: config.server_id.clone(),
        server_name: crate::settings::server_name(),
        version: config.version.clone(),
        product_name: "Rustyfin".to_string(),
        operating_system: std::env::consts::OS.to_string(),
        startup_wizard_completed: true,
    })
}

pub async fn get_system_info(State(config): State<Arc<AppConfig>>) -> impl IntoResponse {
    Json(SystemInfo {
        id: config.server_id.clone(),
        server_name: crate::settings::server_name(),
        version: config.version.clone(),
        product_name: "Rustyfin".to_string(),
        operating_system: std::env::consts::OS.to_string(),
        startup_wizard_completed: true,
        can_self_restart: false,
        can_self_update: false,
        can_launch_web_browser: false,
    })
}

/// Jellyfin responde texto simples em /System/Ping.
/// Pede um novo scan da biblioteca (coalescido se já houver um em andamento).
pub async fn refresh_library(
    State(trigger): State<crate::library::ScanTrigger>,
) -> axum::http::StatusCode {
    crate::library::request_scan_now(&trigger);
    axum::http::StatusCode::NO_CONTENT
}

pub async fn ping() -> impl IntoResponse {
    "Jellyfin Server"
}

pub async fn branding_configuration() -> impl IntoResponse {
    Json(serde_json::json!({
        "LoginDisclaimer": "",
        "CustomCss": "",
        "SplashscreenEnabled": false
    }))
}

pub async fn get_display_preferences(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    Json(serde_json::json!({
        "Id": id,
        "SortBy": "SortName",
        "SortOrder": "Ascending",
        "RememberIndexing": false,
        "RememberSorting": false,
        "PrimaryImageHeight": 250,
        "PrimaryImageWidth": 250,
        "ScrollDirection": "Horizontal",
        "ShowBackdrop": true,
        "ShowSidebar": false,
        "CustomPrefs": {}
    }))
}

/// Preferências de exibição não são persistidas (localhost).
pub async fn save_display_preferences() -> impl IntoResponse {
    axum::http::StatusCode::NO_CONTENT
}
