//! Aba **Acervo** do painel: lista os filmes e episódios com o estado de cada um (metadados, capa,
//! legenda e assinatura), para achar o que a varredura entendeu errado ou deixou incompleto.

use super::admin::AdminState;
use crate::libraries;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::collections::HashMap;

const POR_PAGINA: i64 = 50;
const MAX_POR_PAGINA: i64 = 200;
const MAX_BUSCA: usize = 128;

const SQL_CAPA: &str =
    "(poster_path IS NOT NULL OR tmdb_poster IS NOT NULL OR series_poster_path IS NOT NULL)";
const SQL_LEGENDA: &str =
    "(subtitle_path IS NOT NULL OR COALESCE(streams_json, '') LIKE '%\"Type\":\"Subtitle\"%')";

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub(super) struct Filtro {
    busca: Option<String>,
    biblioteca: Option<String>,
    problema: Option<String>,
    pagina: Option<i64>,
    por_pagina: Option<i64>,
}

#[derive(FromRow)]
struct Linha {
    id: String,
    nome: String,
    tipo: String,
    ano: Option<i32>,
    serie: Option<String>,
    temporada: Option<i32>,
    episodio: Option<i32>,
    caminho: String,
    biblioteca_id: Option<String>,
    container: Option<String>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    largura: Option<i32>,
    altura: Option<i32>,
    duracao_ticks: Option<i64>,
    tamanho: Option<i64>,
    metadados: i64,
    capa: i64,
    legenda: i64,
    assinatura: i64,
    analisado: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Selos {
    metadados: bool,
    capa: bool,
    legenda: bool,
    assinatura: bool,
    analisado: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Titulo {
    id: String,
    nome: String,
    tipo: String,
    ano: Option<i32>,
    serie: Option<String>,
    temporada: Option<i32>,
    episodio: Option<i32>,
    biblioteca: Option<String>,
    caminho: String,
    container: Option<String>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    largura: Option<i32>,
    altura: Option<i32>,
    duracao_segundos: i64,
    tamanho: i64,
    selos: Selos,
}

#[derive(Serialize, FromRow, Default)]
#[serde(rename_all = "camelCase")]
struct Resumo {
    total: i64,
    filmes: i64,
    episodios: i64,
    sem_metadados: i64,
    sem_capa: i64,
    sem_legenda: i64,
    sem_assinatura: i64,
    nao_analisados: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Listagem {
    total: i64,
    pagina: i64,
    por_pagina: i64,
    itens: Vec<Titulo>,
    resumo: Resumo,
}

/// Condição SQL de cada filtro de problema; o texto é fixo, nunca vem do cliente.
fn condicao_problema(problema: &str) -> Option<String> {
    Some(match problema {
        "metadados" => "tmdb_id IS NULL".into(),
        "capa" => format!("NOT {SQL_CAPA}"),
        "legenda" => format!("NOT {SQL_LEGENDA}"),
        "assinatura" => "content_sha256 IS NULL".into(),
        "analise" => "probed = 0".into(),
        _ => return None,
    })
}

pub(super) async fn listar(
    State(st): State<AdminState>,
    Query(filtro): Query<Filtro>,
) -> Result<Json<Listagem>, StatusCode> {
    let erro = |_| StatusCode::INTERNAL_SERVER_ERROR;
    let mut condicoes = vec!["media_type IN ('Movie', 'Episode')".to_owned()];
    let mut textos: Vec<String> = Vec::new();

    if let Some(busca) = filtro.busca.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        if busca.chars().count() > MAX_BUSCA {
            return Err(StatusCode::BAD_REQUEST);
        }
        textos.push(busca.to_lowercase());
        condicoes.push(
            "(instr(lower(name), ?) > 0 OR instr(lower(COALESCE(series_name, '')), ?) > 0 \
             OR instr(lower(path), ?) > 0)"
                .into(),
        );
        let busca = textos[0].clone();
        textos.extend([busca.clone(), busca]);
    }
    if let Some(problema) = filtro.problema.as_deref().filter(|p| !p.is_empty()) {
        condicoes.push(condicao_problema(problema).ok_or(StatusCode::BAD_REQUEST)?);
    }
    let bibliotecas = libraries::list(&st.pool).await.map_err(erro)?;
    if let Some(id) = filtro.biblioteca.as_deref().filter(|b| !b.is_empty()) {
        if !bibliotecas.iter().any(|b| b.id == id) {
            return Err(StatusCode::NOT_FOUND);
        }
        condicoes.push("library_id = ?".into());
        textos.push(id.to_owned());
    }
    let onde = condicoes.join(" AND ");

    let consulta_total = format!("SELECT COUNT(*) FROM items WHERE {onde}");
    let mut total = sqlx::query_scalar::<_, i64>(&consulta_total);
    for texto in &textos {
        total = total.bind(texto);
    }
    let total = total.fetch_one(&st.pool).await.map_err(erro)?;

    let pagina = filtro.pagina.unwrap_or(1).clamp(1, 100_000);
    let por_pagina = filtro.por_pagina.unwrap_or(POR_PAGINA).clamp(10, MAX_POR_PAGINA);
    let consulta = format!(
        "SELECT id, name AS nome, media_type AS tipo, production_year AS ano, series_name AS serie, \
         season_number AS temporada, episode_number AS episodio, path AS caminho, library_id AS biblioteca_id, container, \
         video_codec, audio_codec, width AS largura, height AS altura, \
         duration_ticks AS duracao_ticks, size_bytes AS tamanho, \
         (tmdb_id IS NOT NULL) AS metadados, {SQL_CAPA} AS capa, {SQL_LEGENDA} AS legenda, \
         (content_sha256 IS NOT NULL) AS assinatura, probed AS analisado \
         FROM items WHERE {onde} \
         ORDER BY lower(COALESCE(series_name, name)), season_number, episode_number, lower(name), id \
         LIMIT {por_pagina} OFFSET {}",
        (pagina - 1) * por_pagina
    );
    let mut linhas = sqlx::query_as::<_, Linha>(&consulta);
    for texto in &textos {
        linhas = linhas.bind(texto);
    }
    let linhas = linhas.fetch_all(&st.pool).await.map_err(erro)?;

    let resumo: Resumo = sqlx::query_as(&format!(
        "SELECT COUNT(*) AS total, \
         COALESCE(SUM(media_type = 'Movie'), 0) AS filmes, \
         COALESCE(SUM(media_type = 'Episode'), 0) AS episodios, \
         COALESCE(SUM(tmdb_id IS NULL), 0) AS sem_metadados, \
         COALESCE(SUM(NOT {SQL_CAPA}), 0) AS sem_capa, \
         COALESCE(SUM(NOT {SQL_LEGENDA}), 0) AS sem_legenda, \
         COALESCE(SUM(content_sha256 IS NULL), 0) AS sem_assinatura, \
         COALESCE(SUM(probed = 0), 0) AS nao_analisados \
         FROM items WHERE media_type IN ('Movie', 'Episode')"
    ))
    .fetch_one(&st.pool)
    .await
    .map_err(erro)?;

    let itens = linhas
        .into_iter()
        .map(|l| Titulo {
            biblioteca: l.biblioteca_id.as_deref().and_then(|id| {
                bibliotecas.iter().find(|b| b.id == id).map(|b| b.name.clone())
            }),
            duracao_segundos: l.duracao_ticks.unwrap_or(0) / 10_000_000,
            tamanho: l.tamanho.unwrap_or(0),
            selos: Selos {
                metadados: l.metadados != 0,
                capa: l.capa != 0,
                legenda: l.legenda != 0,
                assinatura: l.assinatura != 0,
                analisado: l.analisado != 0,
            },
            id: l.id,
            nome: l.nome,
            tipo: l.tipo,
            ano: l.ano,
            serie: l.serie,
            temporada: l.temporada,
            episodio: l.episodio,
            caminho: l.caminho,
            container: l.container,
            video_codec: l.video_codec,
            audio_codec: l.audio_codec,
            largura: l.largura,
            altura: l.altura,
        })
        .collect();

    Ok(Json(Listagem { total, pagina, por_pagina, itens, resumo }))
}

/// Capa de um título do acervo, para as miniaturas da lista do painel.
pub(super) async fn capa(
    State(st): State<AdminState>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    super::images::capa_para_painel(&st.config, &st.pool, &id, &q, &headers).await
}
