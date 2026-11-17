use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// System Info returned to clients on discovery
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct PublicSystemInfo {
    pub id: String,
    pub server_name: String,
    pub version: String,
    pub product_name: String,
    pub operating_system: String,
    pub startup_wizard_completed: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct SystemInfo {
    pub id: String,
    pub server_name: String,
    pub version: String,
    pub product_name: String,
    pub operating_system: String,
    pub startup_wizard_completed: bool,
    pub can_self_restart: bool,
    pub can_self_update: bool,
    pub can_launch_web_browser: bool,
}

/// User DTOs for authentication
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct UserDto {
    pub id: String,
    pub name: String,
    pub server_id: String,
    pub has_password: bool,
    pub has_configured_password: bool,
    pub has_configured_easy_password: bool,
    pub enable_auto_login: bool,
    pub configuration: UserConfiguration,
    pub policy: UserPolicy,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_image_tag: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase")]
pub struct UserConfiguration {
    pub play_default_audio_track: bool,
    pub subtitle_language_preference: Option<String>,
    pub display_missing_episodes: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase")]
pub struct UserPolicy {
    pub is_administrator: bool,
    pub is_hidden: bool,
    pub is_disabled: bool,
    pub enable_media_playback: bool,
    pub enable_audio_playback_transcoding: bool,
    pub enable_video_playback_transcoding: bool,
    pub enable_playback_remuxing: bool,
    pub enable_content_downloading: bool,
    pub enable_all_folders: bool,
    pub enabled_folders: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
#[allow(dead_code)] // campos do contrato Jellyfin ainda não utilizados
pub struct AuthenticateUserByName {
    pub username: Option<String>,
    pub pw: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct AuthenticationResult {
    pub user: UserDto,
    pub access_token: String,
    pub server_id: String,
}

/// Media items and Library representations
#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase")]
pub struct BaseItemDto {
    pub id: String,
    pub name: String,
    pub original_title: Option<String>,
    pub server_id: String,
    pub r#type: String, // "CollectionFolder", "Movie", "Episode", "Series", "Folder"
    pub media_type: Option<String>, // "Video", "Audio"
    pub container: Option<String>,
    pub path: Option<String>,
    pub run_time_ticks: Option<i64>,
    pub size: Option<i64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub is_folder: bool,
    pub user_data: Option<UserItemDataDto>,
    pub media_sources: Option<Vec<MediaSourceInfo>>,
    pub media_streams: Option<Vec<MediaStream>>,
    pub image_tags: HashMap<String, String>,
    pub backdrop_image_tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub production_year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub overview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub community_rating: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub genres: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_name: Option<String>,
    /// Temporada (episódios)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_index_number: Option<i32>,
    /// Número do episódio
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index_number: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season_name: Option<String>,
    /// Séries: nº de temporadas; temporadas: nº de episódios.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_count: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recursive_item_count: Option<i32>,
    /// Só em views: "movies" ou "tvshows".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_type: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub genre_items: Vec<NameId>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub studios: Vec<NameId>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<PersonDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub official_rating: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub taglines: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remote_trailers: Vec<MediaUrl>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase")]
pub struct NameId {
    pub name: String,
    pub id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase")]
pub struct PersonDto {
    pub name: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// "Actor", "Director" ou "Writer"
    pub r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary_image_tag: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase")]
pub struct MediaUrl {
    pub url: String,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase")]
pub struct UserItemDataDto {
    pub playback_position_ticks: i64,
    pub play_count: i32,
    pub is_favorite: bool,
    pub played: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unplayed_item_count: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct QueryResult<T> {
    pub items: Vec<T>,
    pub total_record_count: usize,
    pub start_index: usize,
}

/// Perfil de dispositivo enviado pelo cliente no `PlaybackInfo` (só os campos usados na decisão).
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct DeviceProfile {
    pub name: Option<String>,
    pub max_streaming_bitrate: Option<i64>,
    pub direct_play_profiles: Vec<DirectPlayProfile>,
    pub transcoding_profiles: Vec<TranscodingProfile>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct DirectPlayProfile {
    pub container: Option<String>,
    pub audio_codec: Option<String>,
    pub video_codec: Option<String>,
    pub r#type: Option<String>,
}

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct TranscodingProfile {
    pub container: Option<String>,
    pub audio_codec: Option<String>,
    pub video_codec: Option<String>,
    pub protocol: Option<String>,
    pub r#type: Option<String>,
}

/// Playback info DTOs
#[derive(Debug, Deserialize, Clone, Default)]
#[serde(rename_all = "PascalCase")]
#[allow(dead_code)] // campos do contrato Jellyfin ainda não utilizados
pub struct PlaybackInfoRequest {
    pub user_id: Option<String>,
    pub max_streaming_bitrate: Option<i64>,
    pub start_time_ticks: Option<i64>,
    pub audio_stream_index: Option<i32>,
    pub subtitle_stream_index: Option<i32>,
    pub enable_direct_play: Option<bool>,
    pub enable_direct_stream: Option<bool>,
    pub enable_transcoding: Option<bool>,
    pub device_profile: Option<DeviceProfile>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct PlaybackInfoResponse {
    pub media_sources: Vec<MediaSourceInfo>,
    pub play_session_id: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct MediaSourceInfo {
    pub id: String,
    pub path: Option<String>,
    pub protocol: String, // "File"
    pub media_streams: Vec<MediaStream>,
    pub container: Option<String>,
    pub size: Option<i64>,
    pub name: String,
    pub is_remote: bool,
    pub run_time_ticks: Option<i64>,
    pub supports_transcoding: bool,
    /// Bitrate médio (bits/s), como o Jellyfin informa.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitrate: Option<i64>,
    pub supports_direct_stream: bool,
    pub supports_direct_play: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direct_stream_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcoding_url: Option<String>,
    /// "hls" ou "http" (progressivo)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcoding_sub_protocol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcoding_container: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
pub struct MediaStream {
    pub codec: String,
    pub language: Option<String>,
    pub r#type: String, // "Video", "Audio", "Subtitle"
    pub index: i32,
    pub is_default: bool,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub channels: Option<i32>,
    pub sample_rate: Option<i32>,
    #[serde(default)]
    pub color_transfer: Option<String>,
    #[serde(default)]
    pub color_primaries: Option<String>,
    #[serde(default)]
    pub color_space: Option<String>,
    pub display_title: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub is_forced: bool,
    #[serde(default)]
    pub is_external: bool,
    #[serde(default)]
    pub is_text_subtitle_stream: bool,
    #[serde(default)]
    pub supports_external_stream: bool,
    /// "External" (cliente baixa `DeliveryUrl`) ou "Encode" (queimada no vídeo).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_url: Option<String>,
}

impl MediaStream {
    /// Faixa básica (sem legenda/idioma): usada ao reconstruir trilhas só a partir dos codecs.
    pub fn basic(codec: &str, kind: &str, index: i32) -> Self {
        Self {
            codec: codec.to_string(),
            language: None,
            r#type: kind.to_string(),
            index,
            is_default: true,
            width: None,
            height: None,
            channels: None,
            sample_rate: None,
            color_transfer: None,
            color_primaries: None,
            color_space: None,
            display_title: None,
            title: None,
            is_forced: false,
            is_external: false,
            is_text_subtitle_stream: false,
            supports_external_stream: false,
            delivery_method: None,
            delivery_url: None,
        }
    }
}

/// Playback session reporting
#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "PascalCase")]
#[allow(dead_code)] // campos do contrato Jellyfin ainda não utilizados
pub struct PlaybackProgressReport {
    pub item_id: Option<String>,
    pub position_ticks: Option<i64>,
    pub is_paused: Option<bool>,
    pub play_session_id: Option<String>,
}
