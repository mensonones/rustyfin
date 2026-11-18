use crate::{
    api::{
        auth::{CurrentCreds, CurrentUser},
        items::build_streams,
        preparos::{self, Preparos},
        reproducao_admin::Perfis,
        shows::normalize_id,
    },
    db::DbPool,
    models::{MediaSourceInfo, MediaStream, PlaybackInfoRequest, PlaybackInfoResponse, PlaybackProgressReport},
    transcode, users,
};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Extension, Json,
};
use uuid::Uuid;

#[derive(sqlx::FromRow)]
pub(super) struct FonteReproducao {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) container: Option<String>,
    duration_ticks: Option<i64>,
    size_bytes: Option<i64>,
    pub(super) video_codec: Option<String>,
    audio_codec: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    streams_json: Option<String>,
    subtitle_path: Option<String>,
}

pub(super) struct Avaliacao {
    pub(super) plano: transcode::Plan,
    pub(super) motivos: Vec<transcode::MotivoReproducao>,
    pub(super) audio_codec: Option<String>,
    legenda_queimada: Option<i32>,
}

impl FonteReproducao {
    pub(super) async fn buscar(pool: &DbPool, item_id: &str) -> Result<Self, StatusCode> {
        sqlx::query_as(
            "SELECT id, name, container, duration_ticks, size_bytes, video_codec, audio_codec, \
             width, height, streams_json, subtitle_path FROM items WHERE id = ?",
        )
        .bind(normalize_id(item_id))
        .fetch_optional(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)
    }

    pub(super) fn faixas(&self) -> Vec<MediaStream> {
        let mut faixas = build_streams(
            self.streams_json.as_deref(), self.video_codec.as_deref(),
            self.audio_codec.as_deref(), self.width, self.height, self.subtitle_path.is_some(),
        );
        crate::api::items::decorate_subtitles(&self.id, &mut faixas);
        faixas
    }

    pub(super) fn bitrate(&self) -> Option<i64> {
        match (self.size_bytes, self.duration_ticks) {
            (Some(size), Some(ticks)) if size > 0 && ticks > 0 => {
                Some((size as f64 * 8.0 / (ticks as f64 / 10_000_000.0)) as i64)
            }
            _ => None,
        }
    }

    pub(super) fn avaliar(
        &self, req: &PlaybackInfoRequest, faixas: &[MediaStream], ffmpeg_ok: bool,
    ) -> Avaliacao {
        let audio_codec = req.audio_stream_index
            .and_then(|i| faixas.iter().find(|s| s.index == i && s.r#type == "Audio"))
            .map(|s| s.codec.clone())
            .or_else(|| self.audio_codec.clone());
        let legenda_imagem = req.subtitle_stream_index.filter(|i| {
            faixas.iter().any(|s| {
                s.index == *i && s.r#type == "Subtitle" && !transcode::is_text_subtitle(&s.codec)
            })
        });
        let conversao = req.enable_transcoding.unwrap_or(true);
        let legenda_queimada = legenda_imagem.filter(|_| conversao && ffmpeg_ok);
        let bitrate = self.bitrate();
        let (mut plano, mut motivos) = transcode::decidir_com_motivos(
            &transcode::SourceInfo {
                container: self.container.as_deref(),
                video_codec: self.video_codec.as_deref(),
                audio_codec: audio_codec.as_deref(),
                bitrate,
            },
            req.device_profile.as_ref(),
            &transcode::RequestOptions {
                enable_direct_play: req.enable_direct_play.unwrap_or(true),
                enable_transcoding: conversao,
                max_bitrate: req.max_streaming_bitrate,
            },
            ffmpeg_ok,
        );
        if legenda_queimada.is_some() {
            motivos.push(transcode::MotivoReproducao::LegendaDeImagem);
            match &mut plano {
                transcode::Plan::Direct => {
                    plano = transcode::Plan::Transcode(transcode::TranscodePlan {
                        video_copy: false, audio_copy: false, container: transcode::Container::Mkv,
                        hls: false, video_bitrate: None, abr: false,
                    });
                }
                transcode::Plan::Transcode(t) => t.video_copy = false,
            }
        } else if legenda_imagem.is_some() {
            motivos.push(transcode::MotivoReproducao::LegendaDeImagemSemConversao);
        }
        if let transcode::Plan::Transcode(t) = &mut plano {
            t.enable_abr(self.height, bitrate);
        }
        Avaliacao { plano, motivos, audio_codec, legenda_queimada }
    }
}

pub(super) async fn get_playback_info(
    State((config, pool)): State<(std::sync::Arc<crate::config::AppConfig>, DbPool)>,
    Extension(preparos): Extension<Preparos>,
    CurrentUser(user): CurrentUser,
    CurrentCreds(creds): CurrentCreds,
    Extension(perfis): Extension<Perfis>,
    Path(item_id): Path<String>,
    payload: Option<Json<PlaybackInfoRequest>>,
) -> Result<impl IntoResponse, StatusCode> {
    if !users::item_allowed(&pool, &user, &normalize_id(&item_id)).await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let fonte = FonteReproducao::buscar(&pool, &item_id).await?;
    let mut streams = fonte.faixas();
    let req = payload.map(|Json(r)| r).unwrap_or_default();
    let ffmpeg_ok = transcode::can_transcode();
    let avaliacao = fonte.avaliar(&req, &streams, ffmpeg_ok);
    perfis.registrar(&creds.device, &req, &fonte, &avaliacao).await;
    let Avaliacao { plano, motivos, legenda_queimada, .. } = avaliacao;
    tracing::debug!(item = fonte.id, ?motivos, ?plano, "Decisão de reprodução");

    if let Some(i) = legenda_queimada {
        if let Some(s) = streams.iter_mut().find(|s| s.index == i) {
            s.delivery_method = Some("Encode".to_string());
        }
    }
    let play_session_id = Uuid::new_v4().simple().to_string();
    let bitrate = fonte.bitrate();
    let mut source = MediaSourceInfo {
        id: fonte.id.clone(),
        path: None,
        protocol: "File".to_string(),
        media_streams: streams,
        container: crate::api::stream::jellyfin_container(fonte.container.as_deref()),
        size: fonte.size_bytes,
        name: fonte.name,
        is_remote: false,
        run_time_ticks: fonte.duration_ticks,
        supports_transcoding: ffmpeg_ok,
        bitrate,
        supports_direct_stream: true,
        supports_direct_play: true,
        // O cliente monta a URL de Direct Play para garantir Range na TV Samsung.
        direct_stream_url: None,
        transcoding_url: None,
        transcoding_sub_protocol: None,
        transcoding_container: None,
    };
    let favorita = if let transcode::Plan::Transcode(t) = &plano {
        if !t.hls && t.container == transcode::Container::Mp4 && !t.video_copy && !t.audio_copy
            && legenda_queimada.is_none() && req.audio_stream_index.is_none()
            && transcode::tone_map_hdr(fonte.streams_json.as_deref()).is_none()
        {
            let taxa = t.video_bitrate.unwrap_or(4_000_000).clamp(500_000, 30_000_000);
            preparos.observar(&pool, &play_session_id, &user.id, &fonte.id, &source.name, taxa).await
        } else { None }
    } else { None };
    if let transcode::Plan::Transcode(t) = plano {
        source.supports_direct_play = false;
        source.supports_direct_stream = false;
        source.supports_transcoding = true;
        source.transcoding_url = Some(transcode::transcode_url(
            &fonte.id, &t, &play_session_id, req.audio_stream_index,
            req.start_time_ticks, legenda_queimada,
        ));
        source.transcoding_sub_protocol = Some(if t.hls { "hls" } else { "http" }.to_string());
        source.transcoding_container = Some(t.container.ext().to_string());
    }
    if req.start_time_ticks.unwrap_or(0) == 0 {
        if let Some(chave) = favorita {
            if preparos::pronta(&pool, &config, &fonte.id, &chave).await {
                source.transcoding_url = Some(format!("/Videos/{}/preparado/{chave}", fonte.id));
            }
        }
    }
    Ok(Json(PlaybackInfoResponse {
        media_sources: vec![source],
        play_session_id,
    }))
}

pub(super) async fn report_playing(
    CurrentUser(user): CurrentUser,
    Extension(preparos): Extension<Preparos>,
    payload: Option<Json<PlaybackProgressReport>>,
) -> impl IntoResponse {
    if let Some(Json(p)) = payload {
        preparos.relatar(&user.id, &p, false).await;
    }
    StatusCode::NO_CONTENT
}

pub async fn ping_session() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

/// Capabilities: aceitas e ignoradas (localhost, sem controle remoto).
pub async fn post_capabilities() -> impl IntoResponse {
    StatusCode::NO_CONTENT
}

pub(super) async fn report_progress(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Extension(preparos): Extension<Preparos>,
    Json(payload): Json<PlaybackProgressReport>,
) -> impl IntoResponse {
    preparos.relatar(&user.id, &payload, false).await;
    if let (Some(item_id), Some(position_ticks)) = (payload.item_id, payload.position_ticks) {
        let item_id = crate::api::shows::normalize_id(&item_id);
        if let Err(e) = save_position(&pool, &user.id, &item_id, position_ticks, false).await {
            tracing::warn!("Falha ao salvar progresso de {}: {}", item_id, e);
        }
    }
    StatusCode::NO_CONTENT
}

pub(super) async fn report_stopped(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Extension(preparos): Extension<Preparos>,
    Json(payload): Json<PlaybackProgressReport>,
) -> impl IntoResponse {
    preparos.relatar(&user.id, &payload, true).await;
    // Parou de assistir: libera o ffmpeg e a vaga de transcodificação na hora (em vez de esperar os 90 s ociosos).
    if let Some(play_session) = payload.play_session_id.as_deref() {
        crate::transcode::stop_play_session(play_session).await;
    }
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
        r"
        INSERT INTO playback_state (user_id, item_id, position_ticks, is_played, play_count, last_updated)
        VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
        ON CONFLICT(user_id, item_id) DO UPDATE SET
            position_ticks = excluded.position_ticks,
            is_played = CASE WHEN excluded.is_played = 1 THEN 1 ELSE playback_state.is_played END,
            play_count = playback_state.play_count + excluded.play_count,
            last_updated = CURRENT_TIMESTAMP
        ",
    )
    .bind(user_id)
    .bind(item_id)
    .bind(stored_position)
    .bind(completed)
    .bind(i32::from(completed))
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
