use super::{
    admin::AdminState,
    shows::normalize_id,
    stream::servir_catalogado,
    subtitles::{FonteLegenda, ajuste_salvo, srt, srt_aceita},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) async fn faixas(
    State(st): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<Value>>, StatusCode> {
    let fonte = FonteLegenda::buscar(&st.pool, &id).await?;
    let faixas = fonte
        .faixas()
        .into_iter()
        .filter(srt_aceita)
        .map(|s| {
            json!({
                "indice": s.index, "idioma": s.language, "externa": s.is_external,
            })
        })
        .collect();
    Ok(Json(faixas))
}

async fn carregar(
    st: &AdminState,
    id: &str,
    indice: i32,
) -> Result<(String, String, Vec<srt::Trecho>), StatusCode> {
    let fonte = FonteLegenda::buscar(&st.pool, id).await?;
    let faixa = fonte
        .faixas()
        .into_iter()
        .find(|s| s.index == indice && srt_aceita(s))
        .ok_or(StatusCode::NOT_FOUND)?;
    let original = fonte.original(&st.config, &faixa).await?;
    let assinatura = original.assinatura.clone();
    Ok((fonte.id, assinatura, original.trechos().await?))
}

pub(super) async fn detalhe(
    State(st): State<AdminState>,
    Path((id, indice)): Path<(String, i32)>,
) -> Result<Json<Value>, StatusCode> {
    let (id, versao, trechos) = carregar(&st, &id, indice).await?;
    let salvo = ajuste_salvo(&st.pool, &id, indice).await?;
    let obsoleto = salvo.as_ref().is_some_and(|(hash, _)| *hash != versao);
    let ajuste = salvo
        .filter(|(hash, _)| *hash == versao)
        .map(|(_, ajuste)| ajuste);
    Ok(Json(json!({
        "versao": versao, "ajuste": ajuste, "obsoleto": obsoleto,
        "amostras": srt::amostras(
            &trechos, ajuste.and_then(srt::Ajuste::referencias), ajuste.and_then(srt::Ajuste::mapa),
        ),
    })))
}

#[derive(Deserialize)]
pub(super) struct Pedido {
    versao: String,
    ajuste: srt::Ajuste,
}

async fn aplicar(
    st: &AdminState,
    id: &str,
    indice: i32,
    pedido: &Pedido,
) -> Result<(String, String, Vec<srt::Trecho>, srt::Mapa, String), StatusCode> {
    let mapa = pedido.ajuste.mapa().ok_or(StatusCode::BAD_REQUEST)?;
    let (id, versao, trechos) = carregar(st, id, indice).await?;
    if versao != pedido.versao {
        return Err(StatusCode::CONFLICT);
    }
    let (trechos, vtt) = tokio::task::spawn_blocking(move || {
        let vtt = srt::renderizar(&trechos, mapa, true);
        (trechos, vtt)
    })
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((
        id,
        versao,
        trechos,
        mapa,
        vtt.ok_or(StatusCode::UNPROCESSABLE_ENTITY)?,
    ))
}

pub(super) async fn previa(
    State(st): State<AdminState>,
    Path((id, indice)): Path<(String, i32)>,
    Json(pedido): Json<Pedido>,
) -> Result<Json<Value>, StatusCode> {
    let (_, _, trechos, mapa, vtt) = aplicar(&st, &id, indice, &pedido).await?;
    Ok(Json(json!({
        "amostras": srt::amostras(&trechos, pedido.ajuste.referencias(), Some(mapa)),
        "vtt": vtt,
    })))
}

pub(super) async fn salvar(
    State(st): State<AdminState>,
    Path((id, indice)): Path<(String, i32)>,
    Json(pedido): Json<Pedido>,
) -> Result<StatusCode, StatusCode> {
    let (id, versao, _, _, _) = aplicar(&st, &id, indice, &pedido).await?;
    let ajuste =
        serde_json::to_string(&pedido.ajuste).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut tx = st.pool.begin().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    sqlx::query(
        "INSERT INTO subtitle_adjustments (item_id, track_index, source_hash, adjustment)          VALUES (?, ?, ?, ?) ON CONFLICT(item_id, track_index) DO UPDATE SET          source_hash = excluded.source_hash, adjustment = excluded.adjustment",
    )
    .bind(&id).bind(indice).bind(versao).bind(ajuste)
    .execute(&mut *tx).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    tx.commit().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn excluir(
    State(st): State<AdminState>,
    Path((id, indice)): Path<(String, i32)>,
) -> Result<StatusCode, StatusCode> {
    let id = normalize_id(&id);
    let mut tx = st.pool.begin().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    sqlx::query("DELETE FROM subtitle_adjustments WHERE item_id = ? AND track_index = ?")
        .bind(&id)
        .bind(indice)
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    tx.commit().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn video(
    State(st): State<AdminState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    servir_catalogado(&st.pool, &normalize_id(&id), headers).await
}
