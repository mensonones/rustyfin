use crate::{
    api::{auth::CurrentUser, items::build_streams, shows::normalize_id},
    db::DbPool,
    models::{MediaSourceInfo, PlaybackInfoRequest, PlaybackInfoResponse, PlaybackProgressReport},
    transcode, users,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use uuid::Uuid;

pub async fn get_playback_info(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(item_id): Path<String>,
    payload: Option<Json<PlaybackInfoRequest>>,
) -> Result<impl IntoResponse, StatusCode> {
    if !users::item_allowed(&pool, &user, &normalize_id(&item_id)).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)? {
        return Err(StatusCode::NOT_FOUND);
    }
    type Row = (String, String, Option<String>, Option<i64>, Option<i64>, Option<String>, Option<String>, Option<i32>, Option<i32>, Option<String>, Option<String>);
    let row = sqlx::query_as::<_, Row>(
        "SELECT id, name, container, duration_ticks, size_bytes, video_codec, audio_codec, width, height, streams_json, subtitle_path FROM items WHERE id = ?"
    )
    .bind(normalize_id(&item_id))
    .fetch_optional(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::NOT_FOUND)?;

    let (id, name, container, duration_ticks, size_bytes, video_codec, audio_codec, width, height, streams_json, subtitle_path) = row;
    let mut streams = build_streams(
        streams_json.as_deref(),
        video_codec.as_deref(),
        audio_codec.as_deref(),
        width,
        height,
        subtitle_path.is_some(),
    );
    crate::api::items::decorate_subtitles(&id, &mut streams);

    // Bitrate médio do arquivo (bits/s), usado para respeitar MaxStreamingBitrate.
    let source_bitrate = match (size_bytes, duration_ticks) {
        (Some(size), Some(ticks)) if size > 0 && ticks > 0 => {
            Some((size as f64 * 8.0 / (ticks as f64 / 10_000_000.0)) as i64)
        }
        _ => None,
    };

    let req = payload.map(|Json(r)| r).unwrap_or_default();
    // O codec de áudio do plano é o da faixa que o ffmpeg vai mapear (não necessariamente a primeira).
    let selected_audio = req.audio_stream_index.and_then(|i| {
        streams
            .iter()
            .find(|s| s.index == i && s.r#type == "Audio")
            .map(|s| s.codec.clone())
    });
    let audio_codec = selected_audio.or(audio_codec);
    // Legenda de imagem (PGS/DVD) só aparece queimada no vídeo: força transcode com overlay.
    let burn_subtitle = req
        .subtitle_stream_index
        .filter(|i| {
            streams
                .iter()
                .any(|s| s.index == *i && s.r#type == "Subtitle" && !transcode::is_text_subtitle(&s.codec))
        })
        .filter(|_| req.enable_transcoding.unwrap_or(true) && transcode::can_transcode());
    let plan = transcode::decide(
        &transcode::SourceInfo {
            container: container.as_deref(),
            video_codec: video_codec.as_deref(),
            audio_codec: audio_codec.as_deref(),
            bitrate: source_bitrate,
        },
        req.device_profile.as_ref(),
        &transcode::RequestOptions {
            enable_direct_play: req.enable_direct_play.unwrap_or(true),
            enable_transcoding: req.enable_transcoding.unwrap_or(true),
            max_bitrate: req.max_streaming_bitrate,
        },
        transcode::can_transcode(),
    );

    // Diagnóstico: por que um cliente recebeu Direct Play ou transcode.
    tracing::debug!(
        "PlaybackInfo {}: origem container={:?} video={:?} audio={:?} bitrate={:?} | pedido direct_play={:?} transcoding={:?} max_bitrate={:?} | perfil {:?} max={:?} direct_play_profiles={:?} | plano {:?}",
        id,
        container,
        video_codec,
        audio_codec,
        source_bitrate,
        req.enable_direct_play,
        req.enable_transcoding,
        req.max_streaming_bitrate,
        req.device_profile.as_ref().and_then(|p| p.name.clone()),
        req.device_profile.as_ref().and_then(|p| p.max_streaming_bitrate),
        req.device_profile.as_ref().map(|p| &p.direct_play_profiles),
        plan
    );

    let play_session_id = Uuid::new_v4().simple().to_string();
    let mut source = MediaSourceInfo {
        id: id.clone(),
        path: None,
        protocol: "File".to_string(),
        media_streams: streams,
        container: crate::api::stream::jellyfin_container(container.as_deref()),
        size: size_bytes,
        name,
        is_remote: false,
        run_time_ticks: duration_ticks,
        supports_transcoding: transcode::can_transcode(),
        bitrate: source_bitrate,
        supports_direct_stream: true,
        supports_direct_play: true,
        // Como o Jellyfin: sem URL pronta; o cliente monta `/Videos/{id}/stream.{container}?...` sozinho
        // (a TV Samsung só lê por trechos quando monta a própria URL).
        direct_stream_url: None,
        transcoding_url: None,
        transcoding_sub_protocol: None,
        transcoding_container: None,
    };

    let plan = match (plan, burn_subtitle) {
        (transcode::Plan::Direct, Some(_)) => transcode::Plan::Transcode(transcode::TranscodePlan {
            video_copy: false,
            audio_copy: false,
            container: transcode::Container::Mkv,
            hls: false,
            video_bitrate: None,
            abr: false,
        }),
        (transcode::Plan::Transcode(mut t), Some(_)) => {
            t.video_copy = false;
            transcode::Plan::Transcode(t)
        }
        (plan, _) => plan,
    };
    if let (Some(i), transcode::Plan::Transcode(_)) = (burn_subtitle, &plan) {
        if let Some(s) = source.media_streams.iter_mut().find(|s| s.index == i) {
            s.delivery_method = Some("Encode".to_string());
        }
    }

    if let transcode::Plan::Transcode(mut t) = plan {
        t.enable_abr(height, source_bitrate);
        source.supports_direct_play = false;
        source.supports_direct_stream = false;
        source.supports_transcoding = true;
        source.direct_stream_url = None;
        source.transcoding_url = Some(transcode::transcode_url(
            &id,
            &t,
            &play_session_id,
            req.audio_stream_index,
            req.start_time_ticks,
            burn_subtitle,
        ));
        source.transcoding_sub_protocol = Some(if t.hls { "hls" } else { "http" }.to_string());
        source.transcoding_container = Some(t.container.ext().to_string());
    }

    Ok(Json(PlaybackInfoResponse {
        media_sources: vec![source],
        play_session_id,
    }))
}

pub async fn report_playing() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

pub async fn ping_session() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

/// Capabilities: aceitas e ignoradas (localhost, sem controle remoto).
pub async fn post_capabilities() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

pub async fn report_progress(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<PlaybackProgressReport>,
) -> impl IntoResponse {
    if let (Some(item_id), Some(position_ticks)) = (payload.item_id, payload.position_ticks) {
        let item_id = crate::api::shows::normalize_id(&item_id);
        if let Err(e) = save_position(&pool, &user.id, &item_id, position_ticks, false).await {
            tracing::warn!("Falha ao salvar progresso de {}: {}", item_id, e);
        }
    }
    StatusCode::NO_CONTENT
}

pub async fn report_stopped(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Json(payload): Json<PlaybackProgressReport>,
) -> impl IntoResponse {
    if let (Some(item_id), Some(position_ticks)) = (payload.item_id, payload.position_ticks) {
        let item_id = crate::api::shows::normalize_id(&item_id);
        if let Err(e) = save_position(&pool, &user.id, &item_id, position_ticks, true).await {
            tracing::warn!("Falha ao salvar fim de reprodução de {}: {}", item_id, e);
        }
    }
    StatusCode::NO_CONTENT
}

/// Fração da duração a partir da qual um item é considerado assistido.
const PLAYED_THRESHOLD: f64 = 0.90;

pub fn is_completed(position_ticks: i64, duration_ticks: i64) -> bool {
    duration_ticks > 0 && position_ticks as f64 >= duration_ticks as f64 * PLAYED_THRESHOLD
}

async fn save_position(
    pool: &DbPool,
    user_id: &str,
    item_id: &str,
    position_ticks: i64,
    stopped: bool,
) -> Result<(), sqlx::Error> {
    let duration: Option<(Option<i64>,)> =
        sqlx::query_as("SELECT duration_ticks FROM items WHERE id = ?")
            .bind(item_id)
            .fetch_optional(pool)
            .await?;
    // Item inexistente: não cria linha órfã.
    let Some((duration,)) = duration else {
        return Ok(());
    };

    let completed = stopped && is_completed(position_ticks, duration.unwrap_or(0));
    // Ao completar, zera a posição (some de "Continuar Assistindo") e conta a reprodução.
    let stored_position = if completed { 0 } else { position_ticks.max(0) };

    sqlx::query(
        r#"
        INSERT INTO playback_state (user_id, item_id, position_ticks, is_played, play_count, last_updated)
        VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
        ON CONFLICT(user_id, item_id) DO UPDATE SET
            position_ticks = excluded.position_ticks,
            is_played = CASE WHEN excluded.is_played = 1 THEN 1 ELSE playback_state.is_played END,
            play_count = playback_state.play_count + excluded.play_count,
            last_updated = CURRENT_TIMESTAMP
        "#,
    )
    .bind(user_id)
    .bind(item_id)
    .bind(stored_position)
    .bind(completed)
    .bind(if completed { 1 } else { 0 })
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_completed;

    #[test]
    fn completion_threshold() {
        assert!(is_completed(95, 100));
        assert!(!is_completed(50, 100));
        assert!(!is_completed(100, 0));
    }
}
