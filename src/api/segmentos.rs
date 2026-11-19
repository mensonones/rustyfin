//! `GET /MediaSegments/{id}`: os trechos que os apps oferecem para pular (abertura, créditos, resumo e prévia).
//! Hoje vêm dos capítulos do arquivo (veja `capitulos.rs`).

use super::{auth::CurrentUser, shows::normalize_id};
use crate::{
    capitulos::{self, Capitulo},
    db::DbPool,
    users,
};
use axum::{
    Json,
    extract::{Path, RawQuery, State},
    http::StatusCode,
};
use serde_json::{Value, json};
use uuid::Uuid;

/// Tipos pedidos em `includeSegmentTypes` (repetido ou separado por vírgula). Vazio = todos.
fn tipos_pedidos(consulta: Option<&str>) -> Vec<String> {
    consulta
        .unwrap_or("")
        .split('&')
        .filter_map(|par| par.split_once('='))
        .filter(|(chave, _)| chave.eq_ignore_ascii_case("includeSegmentTypes"))
        .flat_map(|(_, valor)| valor.split(','))
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
        .collect()
}

pub(super) async fn segmentos_do_item(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    RawQuery(consulta): RawQuery,
) -> Result<Json<Value>, StatusCode> {
    let id = normalize_id(&id);
    let erro = |e: sqlx::Error| {
        tracing::error!("Falha ao ler os segmentos de {id}: {e}");
        StatusCode::INTERNAL_SERVER_ERROR
    };
    if !users::item_allowed(&pool, &user, &id).await.map_err(erro)? {
        return Err(StatusCode::NOT_FOUND);
    }
    let linha: Option<(Option<String>, Option<i64>)> =
        sqlx::query_as("SELECT chapters_json, duration_ticks FROM items WHERE id = ?")
            .bind(&id)
            .fetch_optional(&pool)
            .await
            .map_err(erro)?;
    let Some((json_capitulos, duracao)) = linha else {
        return Err(StatusCode::NOT_FOUND);
    };
    let capitulos: Vec<Capitulo> = json_capitulos.and_then(|j| serde_json::from_str(&j).ok()).unwrap_or_default();
    let pedidos = tipos_pedidos(consulta.as_deref());

    let itens: Vec<Value> = capitulos::segmentos(&capitulos, duracao.unwrap_or(0))
        .into_iter()
        .filter(|s| pedidos.is_empty() || pedidos.contains(&s.tipo.nome().to_lowercase()))
        .map(|s| {
            let ident = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("segmento:{id}:{}:{}", s.tipo.nome(), s.inicio_ticks).as_bytes(),
            );
            json!({
                "Id": ident.simple().to_string(),
                "ItemId": id,
                "Type": s.tipo.nome(),
                "StartTicks": s.inicio_ticks,
                "EndTicks": s.fim_ticks,
            })
        })
        .collect();
    Ok(Json(json!({ "Items": itens, "TotalRecordCount": itens.len(), "StartIndex": 0 })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tipos_pedidos_aceita_repeticao_virgula_e_qualquer_caixa() {
        assert_eq!(
            tipos_pedidos(Some("includeSegmentTypes=Intro&x=1&IncludeSegmentTypes=Outro,Recap")),
            ["intro", "outro", "recap"]
        );
        assert!(tipos_pedidos(None).is_empty());
        assert!(tipos_pedidos(Some("outra=1")).is_empty());
    }
}
