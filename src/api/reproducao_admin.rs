use super::{
    admin::AdminState,
    playback::{Avaliacao, FonteReproducao},
};
use crate::{
    models::PlaybackInfoRequest,
    transcode::{self, Plan},
    users::DeviceInfo,
};
use axum::{
    Extension, Json,
    extract::{Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::VecDeque, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

const MAX_PERFIS: usize = 32;
const MAX_BYTES_PERFIL: usize = 16 * 1024;

#[derive(Clone, Default)]
pub(super) struct Perfis(Arc<Mutex<VecDeque<Perfil>>>);

struct Perfil {
    id: Uuid,
    chave: String,
    dispositivo: String,
    cliente: Option<String>,
    pedido: PlaybackInfoRequest,
    ultima_decisao: Value,
}

impl Perfis {
    pub(super) async fn registrar(
        &self,
        dispositivo: &DeviceInfo,
        pedido: &PlaybackInfoRequest,
        fonte: &FonteReproducao,
        avaliacao: &Avaliacao,
    ) {
        let Some(perfil) = pedido
            .device_profile
            .as_ref()
            .filter(|p| !p.direct_play_profiles.is_empty())
        else {
            return;
        };
        let Some(chave) = dispositivo.device_id.as_deref().filter(|id| !id.is_empty()) else {
            return;
        };
        if chave.len() > 256 {
            return;
        }
        let pedido = PlaybackInfoRequest {
            max_streaming_bitrate: pedido.max_streaming_bitrate,
            enable_direct_play: pedido.enable_direct_play,
            enable_transcoding: pedido.enable_transcoding,
            device_profile: Some(perfil.clone()),
            ..PlaybackInfoRequest::default()
        };
        if serde_json::to_vec(&pedido).map_or(true, |bytes| bytes.len() > MAX_BYTES_PERFIL) {
            return;
        }

        let nome = dispositivo.device.as_ref().or(perfil.name.as_ref());
        let mut perfis = self.0.lock().await;
        let id = perfis
            .iter()
            .position(|p| p.chave == chave)
            .and_then(|i| perfis.remove(i))
            .map_or_else(Uuid::new_v4, |p| p.id);
        perfis.push_front(Perfil {
            id,
            chave: chave.to_owned(),
            dispositivo: nome.map_or_else(
                || "Dispositivo sem nome".into(),
                |s| s.chars().take(120).collect(),
            ),
            cliente: dispositivo
                .client
                .as_ref()
                .map(|s| s.chars().take(120).collect()),
            pedido,
            ultima_decisao: json!({
                "titulo": fonte.name, "resultado": resultado(avaliacao),
            }),
        });
        perfis.truncate(MAX_PERFIS);
    }
}

fn resultado(a: &Avaliacao) -> Value {
    let (modo, container, video_copiado, audio_copiado, protocolo) = match &a.plano {
        Plan::Direct => ("direct_play", None, true, true, None),
        Plan::Transcode(t) => (
            if t.video_copy { "remux" } else { "transcode" },
            Some(t.container.ext()),
            t.video_copy,
            t.audio_copy,
            Some(if t.hls { "HLS" } else { "HTTP" }),
        ),
    };
    json!({
        "modo": modo, "container": container, "protocolo": protocolo,
        "videoCopiado": video_copiado, "audioCopiado": audio_copiado,
        "audioOrigem": a.audio_codec, "motivos": a.motivos,
    })
}

pub(super) async fn listar_perfis(Extension(perfis): Extension<Perfis>) -> Json<Vec<Value>> {
    let dados = perfis
        .0
        .lock()
        .await
        .iter()
        .map(|p| {
            json!({
                "id": p.id.to_string(), "dispositivo": p.dispositivo, "cliente": p.cliente,
                "ultimaDecisao": p.ultima_decisao,
            })
        })
        .collect();
    Json(dados)
}

#[derive(Deserialize)]
pub(super) struct Busca {
    busca: String,
}

#[derive(sqlx::FromRow, Serialize)]
pub(super) struct Titulo {
    id: String,
    nome: String,
    ano: Option<i32>,
    serie: Option<String>,
    temporada: Option<i32>,
    episodio: Option<i32>,
}

pub(super) async fn buscar_titulos(
    State(st): State<AdminState>,
    Query(q): Query<Busca>,
) -> Result<Json<Vec<Titulo>>, StatusCode> {
    let busca = q.busca.trim();
    if busca.is_empty() || busca.chars().count() > 128 {
        return Err(StatusCode::BAD_REQUEST);
    }
    sqlx::query_as(
        "SELECT id, name AS nome, production_year AS ano, series_name AS serie, \
         season_number AS temporada, episode_number AS episodio FROM items \
         WHERE media_type IN ('Movie', 'Episode') AND \
           (instr(lower(name), lower(?1)) > 0 OR instr(lower(series_name), lower(?1)) > 0) \
         ORDER BY COALESCE(series_name, name), season_number, episode_number, name, id LIMIT 20",
    )
    .bind(busca)
    .fetch_all(&st.pool)
    .await
    .map(Json)
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Simulacao {
    item_id: String,
    perfis: Vec<String>,
    audio_index: Option<i32>,
    legenda_index: Option<i32>,
}

pub(super) async fn simular(
    State(st): State<AdminState>,
    Extension(perfis): Extension<Perfis>,
    Json(simulacao): Json<Simulacao>,
) -> Result<Json<Value>, StatusCode> {
    if simulacao.perfis.is_empty() || simulacao.perfis.len() > 2 {
        return Err(StatusCode::BAD_REQUEST);
    }
    let pedidos = {
        let perfis = perfis.0.lock().await;
        simulacao
            .perfis
            .iter()
            .map(|id| {
                let id = Uuid::parse_str(id).map_err(|_| StatusCode::BAD_REQUEST)?;
                let perfil = perfis
                    .iter()
                    .find(|p| p.id == id)
                    .ok_or(StatusCode::NOT_FOUND)?;
                Ok((id, perfil.dispositivo.clone(), perfil.pedido.clone()))
            })
            .collect::<Result<Vec<_>, StatusCode>>()?
    };
    let fonte = FonteReproducao::buscar(&st.pool, &simulacao.item_id).await?;
    let faixas = fonte.faixas();
    let ffmpeg = transcode::can_transcode();
    for (indice, tipo) in [
        (simulacao.audio_index, "Audio"),
        (simulacao.legenda_index, "Subtitle"),
    ] {
        if indice.is_some_and(|i| !faixas.iter().any(|s| s.index == i && s.r#type == tipo)) {
            return Err(StatusCode::BAD_REQUEST);
        }
    }
    let resultados: Vec<_> = pedidos
        .into_iter()
        .map(|(id, dispositivo, mut pedido)| {
            pedido.audio_stream_index = simulacao.audio_index;
            pedido.subtitle_stream_index = simulacao.legenda_index;
            json!({
                "id": id.to_string(), "dispositivo": dispositivo,
                "resultado": resultado(&fonte.avaliar(&pedido, &faixas, ffmpeg)),
            })
        })
        .collect();
    let faixas: Vec<_> = faixas
        .into_iter()
        .filter(|s| matches!(s.r#type.as_str(), "Audio" | "Subtitle"))
        .map(|s| {
            json!({
                "indice": s.index, "tipo": s.r#type, "codec": s.codec, "idioma": s.language,
            })
        })
        .collect();
    Ok(Json(json!({
        "titulo": fonte.name, "container": fonte.container, "video": fonte.video_codec,
        "bitrate": fonte.bitrate(), "faixas": faixas, "resultados": resultados,
    })))
}
