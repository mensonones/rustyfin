pub mod admin;
pub mod auth;
pub mod compat;
pub mod images;
pub mod items;
pub mod playback;
pub mod shows;
pub mod stream;
pub mod subtitles;
pub mod system;
pub mod transcoding;
pub mod views;

use crate::{config::AppConfig, db::DbPool};
use axum::{
    middleware::from_fn_with_state,
    routing::{get, post},
    Router,
};
use std::sync::Arc;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

pub fn build_router(config: Arc<AppConfig>, pool: DbPool, scan: crate::library::ScanTrigger) -> Router {
    let app_state_tuple = (config.clone(), pool.clone());

    Router::new()
        // 1. System & Discovery
        .route("/System/Info/Public", get(system::get_system_info_public).with_state(config.clone()))
        .route("/System/Info", get(system::get_system_info).with_state(config.clone()))
        .route("/System/Ping", get(system::ping).post(system::ping))
        .route("/Library/Refresh", post(system::refresh_library).with_state(scan.clone()))
        .route("/Items/:id/Refresh", post(system::refresh_library).with_state(scan.clone()))
        .route("/Branding/Configuration", get(system::branding_configuration))
        .route(
            "/DisplayPreferences/:id",
            get(system::get_display_preferences).post(system::save_display_preferences),
        )
        // 2. Authentication (Localhost auto-auth)
        .route("/Users/Public", get(auth::get_public_users).with_state(app_state_tuple.clone()))
        .route("/Users/AuthenticateByName", post(auth::authenticate_by_name).with_state(app_state_tuple.clone()))
        .route("/Users", get(auth::list_users).with_state(app_state_tuple.clone()))
        .route("/Users/New", post(auth::create_user).with_state(app_state_tuple.clone()))
        .route(
            "/Users/:user_id",
            get(auth::get_user_by_id).delete(auth::delete_user).with_state(app_state_tuple.clone()),
        )
        .route("/Users/:user_id/Password", post(auth::change_password).with_state(app_state_tuple.clone()))
        .route("/Users/:user_id/Policy", post(auth::update_policy).with_state(app_state_tuple.clone()))
        .route("/Users/Me", get(auth::get_current_user).with_state(app_state_tuple.clone()))
        // 3. Views & Libraries
        .route("/Users/:user_id/Views", get(views::get_user_views).with_state(app_state_tuple.clone()))
        // 4. Items & Navigation
        .route("/Users/:user_id/Items", get(items::get_user_items).with_state(app_state_tuple.clone()))
        .route("/Users/:user_id/Items/Resume", get(items::get_resume_items).with_state(app_state_tuple.clone()))
        .route("/Users/:user_id/Items/Latest", get(items::get_latest_items).with_state(app_state_tuple.clone()))
        .route("/Users/:user_id/Items/:id", get(items::get_user_item_by_id).with_state(app_state_tuple.clone()))
        .route("/UserItems/Resume", get(items::get_resume_items_no_user).with_state(app_state_tuple.clone()))
        .route("/Items", get(items::get_items).with_state(app_state_tuple.clone()))
        .route("/Items/:id", get(items::get_item_by_id).with_state(app_state_tuple.clone()))
        // 4a. Séries (itens virtuais agrupados a partir dos episódios)
        .route("/Shows/NextUp", get(shows::get_next_up).with_state(app_state_tuple.clone()))
        .route("/Shows/:id/Seasons", get(shows::get_seasons).with_state(app_state_tuple.clone()))
        .route("/Shows/:id/Episodes", get(shows::get_episodes).with_state(app_state_tuple.clone()))
        // 4b. Imagens (posters locais, backdrops e miniaturas)
        .route("/Items/:id/Images/Primary", get(images::get_primary_image).with_state(app_state_tuple.clone()))
        .route("/Items/:id/Images/Primary/:index", get(images::get_primary_image_indexed).with_state(app_state_tuple.clone()))
        .route("/Items/:id/Images/Backdrop", get(images::get_backdrop_image).with_state(app_state_tuple.clone()))
        .route("/Items/:id/Images/Backdrop/:index", get(images::get_backdrop_image_indexed).with_state(app_state_tuple.clone()))
        // 5. Playback Handshake & Scrobbling
        .route("/Items/:id/PlaybackInfo", get(playback::get_playback_info).post(playback::get_playback_info).with_state(pool.clone()))
        .route("/Sessions/Capabilities", post(playback::post_capabilities))
        .route("/Sessions/Capabilities/Full", post(playback::post_capabilities))
        .route("/Sessions/Playing", post(playback::report_playing))
        .route("/Sessions/Playing/Ping", post(playback::ping_session))
        .route("/Sessions/Playing/Progress", post(playback::report_progress).with_state(pool.clone()))
        .route("/Sessions/Playing/Stopped", post(playback::report_stopped).with_state(pool.clone()))
        // 5b. Compatibilidade com clientes 10.9+
        .route("/QuickConnect/Enabled", get(compat::quick_connect_enabled))
        .route("/socket", get(compat::socket))
        .route("/System/Configuration/encoding", get(compat::encoding_configuration))
        .route("/Items/:id/Ancestors", get(compat::empty_array))
        .route("/Items/:id/SpecialFeatures", get(compat::empty_array))
        .route("/Items/:id/LocalTrailers", get(compat::empty_array))
        .route("/Users/:user_id/Items/:id/LocalTrailers", get(compat::empty_array))
        .route("/Users/:user_id/Items/:id/SpecialFeatures", get(compat::empty_array))
        .route("/Users/:user_id/Items/:id/Intros", get(compat::empty_query))
        .route("/Branding/Css", get(compat::branding_css))
        .route("/System/Endpoint", get(compat::system_endpoint))
        .route("/UserViews", get(views::get_user_views_all).with_state(app_state_tuple.clone()))
        .route("/Library/MediaFolders", get(views::get_user_views_all).with_state(app_state_tuple.clone()))
        .route("/Items/Latest", get(items::get_latest_items_no_user).with_state(app_state_tuple.clone()))
        .route("/Items/Filters", get(compat::query_filters))
        .route("/Items/Filters2", get(compat::query_filters))
        .route("/Genres", get(compat::empty_query))
        .route("/Studios", get(compat::empty_query))
        .route("/Persons", get(compat::empty_query))
        .route("/Items/:id/Similar", get(compat::empty_query))
        .route("/Items/:id/Intros", get(compat::empty_query))
        .route("/Items/:id/ThemeMedia", get(compat::theme_media))
        .route("/MediaSegments/:id", get(compat::empty_query))
        .route("/Playback/BitrateTest", get(compat::bitrate_test))
        .route("/Sessions", get(compat::sessions_list))
        .route("/Sessions/Logout", post(auth::logout).with_state(app_state_tuple.clone()))
        .route(
            "/UserPlayedItems/:id",
            post(compat::mark_played).delete(compat::mark_unplayed).with_state(pool.clone()),
        )
        .route(
            "/Users/:user_id/PlayedItems/:id",
            post(compat::mark_played_user).delete(compat::mark_unplayed_user).with_state(pool.clone()),
        )
        .route(
            "/UserFavoriteItems/:id",
            post(compat::mark_favorite).delete(compat::unmark_favorite).with_state(pool.clone()),
        )
        .route(
            "/Users/:user_id/FavoriteItems/:id",
            post(compat::mark_favorite_user).delete(compat::unmark_favorite_user).with_state(pool.clone()),
        )
        .route("/Videos/:id/master.m3u8", get(transcoding::hls_master))
        .route("/Videos/:id/main.m3u8", get(transcoding::hls_main).with_state(pool.clone()))
        .route("/Videos/:id/hls/:key/:file", get(transcoding::hls_segment))
        .route("/Items/:id/Download", get(stream::stream_video).with_state(pool.clone()))
        // 6. Direct Play Streaming with Range Requests
        .route("/Videos/:id/stream", get(stream::stream_video).with_state(pool.clone()))
        .route("/Videos/:id/stream.:ext", get(stream::stream_video_with_ext).with_state(pool.clone()))
        .route("/Audio/:id/stream", get(stream::stream_audio).with_state(pool.clone()))
        .route("/Videos/:id/:source/Subtitles/:index/Stream.:ext", get(subtitles::get_subtitle).with_state(app_state_tuple.clone()))
        .route("/Videos/:id/:source/Subtitles/:index/:start/Stream.:ext", get(subtitles::get_subtitle_from).with_state(app_state_tuple.clone()))
        // Administração (painel /admin e /Library/VirtualFolders*): só rede local
        .merge(admin::router(config.clone(), pool.clone(), scan))
        // Toda rota (exceto as públicas) exige token válido; o CORS fica por fora para o preflight passar
        .layer(from_fn_with_state(pool.clone(), auth::require_user))
        // Allow permissive CORS for web clients and cross-origin mobile apps
        .layer(CorsLayer::permissive())
        // Log por requisição (nível DEBUG): RUST_LOG=tower_http=debug
        // Só método e caminho: a URL completa pode conter `api_key=` (credencial) e iria para os logs.
        .layer(TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<axum::body::Body>| {
            tracing::debug_span!("request", method = %req.method(), path = %req.uri().path())
        }))
}
