//! Séries e temporadas: itens virtuais derivados dos episódios indexados.
//!
//! Não há linhas de "Series"/"Season" no banco: elas são agrupamentos de episódios por
//! `series_id`/`season_id` (UUID v5 do nome), então sobrevivem a reindexações.

use crate::{
    api::{
        auth::CurrentUser,
        images::image_tag,
        items::{self, db_err, escape_like, ItemQuery, Scope},
    },
    config::AppConfig,
    db::DbPool,
    models::{BaseItemDto, QueryResult, UserItemDataDto},
    users::AuthUser,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use sqlx::FromRow;
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VKind {
    Series,
    Season,
}

#[derive(Default)]
pub struct VFilter {
    /// Só temporadas/episódios desta série.
    pub series_id: Option<String>,
    /// Só itens desta biblioteca.
    pub library_id: Option<String>,
    /// Um único item (para `/Items/{id}`).
    pub group_id: Option<String>,
    pub search: Option<String>,
    pub genres: Vec<String>,
    pub years: Vec<i32>,
    pub official_ratings: Vec<String>,
}

#[derive(FromRow)]
struct VRow {
    id: String,
    series_id: String,
    series_name: Option<String>,
    season_number: Option<i32>,
    episode_count: i64,
    season_count: i64,
    unplayed: i64,
    year: Option<i32>,
    st_title: Option<String>,
    st_overview: Option<String>,
    st_rating: Option<f64>,
    st_genres: Option<String>,
    st_extra: Option<String>,
    st_poster: Option<String>,
    st_backdrop: Option<String>,
    season_poster: Option<String>,
    fav: i64,
}

pub fn season_name(n: Option<i32>) -> String {
    match n {
        Some(0) => "Especiais".to_string(),
        Some(n) => format!("Temporada {n}"),
        None => "Temporada".to_string(),
    }
}

/// IDs de clientes podem vir com hífens ou em maiúsculas.
pub fn normalize_id(id: &str) -> String {
    id.replace('-', "").to_lowercase()
}

#[allow(clippy::too_many_arguments)]
async fn fetch_virtual(
    pool: &DbPool,
    user: &AuthUser,
    kind: VKind,
    filter: &VFilter,
    sort_by: &[String],
    descending: bool,
    limit: Option<i64>,
    offset: i64,
) -> Result<(Vec<VRow>, i64), sqlx::Error> {
    let key = match kind {
        VKind::Series => "i.series_id",
        VKind::Season => "i.season_id",
    };

    let mut conds = String::new();
    let mut binds: Vec<String> = Vec::new();
    if let Some(ref s) = filter.series_id {
        conds.push_str(" AND i.series_id = ?");
        binds.push(s.clone());
    }
    if let Some(ref l) = filter.library_id {
        conds.push_str(" AND i.library_id = ?");
        binds.push(l.clone());
    }
    if let Some((sql, vals)) = user.library_clause("i.library_id") {
        conds.push_str(&format!(" AND {sql}"));
        binds.extend(vals);
    }
    if let Some(ref g) = filter.group_id {
        conds.push_str(&format!(" AND {key} = ?"));
        binds.push(g.clone());
    }
    if let Some(ref term) = filter.search {
        conds.push_str(" AND i.series_name LIKE ? ESCAPE '\\'");
        binds.push(format!("%{}%", escape_like(term)));
    }
    {
        let facet_query = ItemQuery {
            genres: filter.genres.clone(),
            years: filter.years.clone(),
            official_ratings: filter.official_ratings.clone(),
            ..Default::default()
        };
        let (fc, fb) = crate::api::facets::filter_conds(
            &facet_query,
            "s.genres",
            "COALESCE(i.production_year, s.year)",
            "json_extract(s.extra, '$.official_rating')",
        );
        for c in fc {
            conds.push_str(&format!(" AND {c}"));
        }
        binds.extend(fb);
    }

    let base = format!(
        r"
        SELECT {key} AS id, i.series_id AS series_id, MAX(i.series_name) AS series_name,
               MAX(i.season_number) AS season_number,
               COUNT(*) AS episode_count, COUNT(DISTINCT i.season_number) AS season_count,
               SUM(CASE WHEN IFNULL(p.is_played, 0) = 0 THEN 1 ELSE 0 END) AS unplayed,
               COALESCE(MAX(i.production_year), MAX(s.year)) AS year, MAX(i.created_at) AS added,
               MAX(s.tmdb_title) AS st_title, MAX(s.overview) AS st_overview,
               MAX(s.rating) AS st_rating, MAX(s.genres) AS st_genres, MAX(s.extra) AS st_extra,
               COALESCE(MIN(i.series_poster_path), MAX(s.poster)) AS st_poster,
               COALESCE(MIN(i.series_backdrop_path), MAX(s.backdrop)) AS st_backdrop,
               COALESCE(MIN(i.season_poster_path), MAX(sa.poster)) AS season_poster,
               MAX(IFNULL(f.is_favorite, 0)) AS fav
        FROM items i
        LEFT JOIN playback_state p ON i.id = p.item_id AND p.user_id = ?
        LEFT JOIN series s ON s.id = i.series_id
        LEFT JOIN season_art sa ON sa.season_id = i.season_id
        LEFT JOIN playback_state f ON f.item_id = {key} AND f.user_id = ?
        WHERE i.media_type = 'Episode' AND i.series_id IS NOT NULL{conds}
        GROUP BY {key}
        "
    );

    // Ordenação: lista branca (nunca interpola entrada do cliente).
    let dir = if descending { "DESC" } else { "ASC" };
    let mut order: Vec<String> = Vec::new();
    for k in sort_by {
        match k.to_lowercase().as_str() {
            "name" | "sortname" => order.push(format!("series_name COLLATE NOCASE {dir}")),
            "datecreated" | "datelastcontentadded" => order.push(format!("added {dir}")),
            "random" => order.push("RANDOM()".to_string()),
            _ => {}
        }
    }
    if kind == VKind::Season {
        order.push("series_name COLLATE NOCASE ASC".to_string());
        order.push("season_number ASC".to_string());
    } else {
        order.push("series_name COLLATE NOCASE ASC".to_string());
    }
    let order_sql = order.join(", ");

    let count_sql = format!("SELECT COUNT(*) FROM ({base})");
    let mut count_q = sqlx::query_as::<_, (i64,)>(&count_sql)
        .bind(&user.id)
        .bind(&user.id);
    for b in &binds {
        count_q = count_q.bind(b);
    }
    let (total,) = count_q.fetch_one(pool).await?;

    let list_sql = format!("SELECT * FROM ({base}) ORDER BY {order_sql} LIMIT ? OFFSET ?");
    let mut list_q = sqlx::query_as::<_, VRow>(&list_sql)
        .bind(&user.id)
        .bind(&user.id);
    for b in &binds {
        list_q = list_q.bind(b);
    }
    let rows = list_q.bind(limit.unwrap_or(-1)).bind(offset).fetch_all(pool).await?;
    Ok((rows, total))
}

fn row_to_dto(config: &AppConfig, kind: VKind, row: VRow) -> BaseItemDto {
    let series_display = row
        .st_title
        .clone()
        .or_else(|| row.series_name.clone())
        .unwrap_or_default();
    let genres = row
        .st_genres
        .as_deref()
        .and_then(|g| serde_json::from_str::<Vec<String>>(g).ok())
        .unwrap_or_default();

    // Primary sempre existe (cai para a imagem do primeiro episódio); backdrop só com arquivo da série.
    let mut image_tags = HashMap::new();
    image_tags.insert(
        "Primary".to_string(),
        image_tag(
            match kind {
                VKind::Season => row.season_poster.as_deref().or(row.st_poster.as_deref()),
                VKind::Series => row.st_poster.as_deref(),
            }
            .or(Some(&row.id)),
            0,
        ),
    );
    // Sempre há backdrop: arte da série ou quadro do primeiro episódio.
    let backdrop_image_tags = vec![image_tag(row.st_backdrop.as_deref().or(Some(&row.id)), 1)];

    let user_data = Some(UserItemDataDto {
        unplayed_item_count: Some(row.unplayed as i32),
        played: row.unplayed == 0,
        is_favorite: row.fav == 1,
        ..Default::default()
    });

    let extra_json = row.st_extra.clone();
    let mut dto = match kind {
        VKind::Series => BaseItemDto {
            id: row.id,
            name: series_display,
            server_id: config.server_id.clone(),
            r#type: "Series".to_string(),
            is_folder: true,
            production_year: row.year,
            overview: row.st_overview,
            community_rating: row.st_rating,
            genres,
            image_tags,
            backdrop_image_tags,
            user_data,
            child_count: Some(row.season_count as i32),
            recursive_item_count: Some(row.episode_count as i32),
            ..Default::default()
        },
        VKind::Season => BaseItemDto {
            id: row.id,
            name: season_name(row.season_number),
            server_id: config.server_id.clone(),
            r#type: "Season".to_string(),
            is_folder: true,
            production_year: row.year,
            series_id: Some(row.series_id),
            series_name: Some(series_display),
            index_number: row.season_number,
            season_name: Some(season_name(row.season_number)),
            image_tags,
            user_data,
            child_count: Some(row.episode_count as i32),
            recursive_item_count: Some(row.episode_count as i32),
            ..Default::default()
        },
    };
    if kind == VKind::Series {
        crate::api::facets::apply_extra(&mut dto, extra_json.as_deref());
    }
    dto
}

/// Lista séries (ou temporadas) com paginação e ordenação.
pub async fn list_virtual(
    config: &AppConfig,
    pool: &DbPool,
    user: &AuthUser,
    kind: VKind,
    series_id: Option<String>,
    library_id: Option<String>,
    q: &ItemQuery,
) -> Result<Json<QueryResult<BaseItemDto>>, StatusCode> {
    let filter = VFilter {
        series_id,
        library_id,
        group_id: None,
        search: q.search_term.clone(),
        genres: q.genres.clone(),
        years: q.years.clone(),
        official_ratings: q.official_ratings.clone(),
    };
    let (rows, total) = fetch_virtual(pool, user, kind, &filter, &q.sort_by, q.descending, q.limit, q.start_index)
        .await
        .map_err(db_err)?;
    Ok(Json(QueryResult {
        items: rows.into_iter().map(|r| row_to_dto(config, kind, r)).collect(),
        total_record_count: total as usize,
        start_index: q.start_index as usize,
    }))
}

/// `/Items/{id}` para IDs de série ou temporada.
pub async fn get_virtual_item(
    config: &AppConfig,
    pool: &DbPool,
    user: &AuthUser,
    id: &str,
) -> Result<Option<BaseItemDto>, sqlx::Error> {
    for kind in [VKind::Series, VKind::Season] {
        let filter = VFilter { group_id: Some(id.to_string()), ..Default::default() };
        let (mut rows, _) = fetch_virtual(pool, user, kind, &filter, &[], false, Some(1), 0).await?;
        if let Some(row) = rows.pop() {
            return Ok(Some(row_to_dto(config, kind, row)));
        }
    }
    Ok(None)
}

#[derive(FromRow)]
struct VirtualArtwork {
    episode_id: String,
    local_season_poster: Option<String>,
    tmdb_season_poster: Option<String>,
    local_series_poster: Option<String>,
    tmdb_series_poster: Option<String>,
    local_series_backdrop: Option<String>,
    tmdb_series_backdrop: Option<String>,
}

/// Arte em ordem de preferência e ID do primeiro episódio como reserva.
pub async fn resolve_art(
    pool: &DbPool,
    user: &AuthUser,
    id: &str,
    backdrop: bool,
) -> Result<Option<(Vec<String>, String)>, sqlx::Error> {
    let mut sql = String::from(
        "SELECT i.id AS episode_id,
                MIN(CASE WHEN i.season_id = ?1 THEN i.season_poster_path END) OVER () AS local_season_poster,
                sa.poster AS tmdb_season_poster,
                MIN(i.series_poster_path) OVER () AS local_series_poster,
                s.poster AS tmdb_series_poster,
                MIN(i.series_backdrop_path) OVER () AS local_series_backdrop,
                s.backdrop AS tmdb_series_backdrop
         FROM items i
         LEFT JOIN series s ON s.id = i.series_id
         LEFT JOIN season_art sa ON sa.season_id = ?1
         WHERE (i.series_id = ?1 OR i.season_id = ?1) AND i.media_type = 'Episode'",
    );
    let mut valores = Vec::new();
    if let Some((filtro, binds)) = user.library_clause("i.library_id") {
        sql.push_str(" AND ");
        sql.push_str(&filtro);
        valores = binds;
    }
    sql.push_str(" ORDER BY i.season_number, i.episode_number, i.id LIMIT 1");
    let mut consulta = sqlx::query_as(&sql).bind(id);
    for valor in valores {
        consulta = consulta.bind(valor);
    }
    let art: Option<VirtualArtwork> = consulta.fetch_optional(pool).await?;
    Ok(art.map(|row| {
        let candidates = if backdrop {
            vec![row.local_series_backdrop, row.tmdb_series_backdrop]
        } else {
            vec![row.local_season_poster, row.tmdb_season_poster, row.local_series_poster, row.tmdb_series_poster]
        };
        (candidates.into_iter().flatten().collect(), row.episode_id)
    }))
}

// ---------------- rotas /Shows ----------------

pub async fn get_seasons(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    CurrentUser(user): CurrentUser,
    Path(series_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    let q = ItemQuery::from_params(params);
    list_virtual(&config, &pool, &user, VKind::Season, Some(normalize_id(&series_id)), None, &q).await
}

pub async fn get_episodes(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    CurrentUser(user): CurrentUser,
    Path(series_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    let mut q = ItemQuery::from_params(params);
    q.include_item_types = vec!["Episode".to_string()];
    let scope = match q.season_id.clone() {
        Some(season_id) => Scope::Season(normalize_id(&season_id)),
        None => Scope::Series(normalize_id(&series_id)),
    };
    items::fetch_scoped(&config, &pool, &user, q, false, scope).await
}

/// "Próximo episódio": para cada série com algo assistido, o primeiro não assistido depois do
/// último episódio visto. Séries mais recentemente assistidas primeiro.
pub async fn get_next_up(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    CurrentUser(user): CurrentUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    let raw = params
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("seriesid"))
        .map(|(_, v)| normalize_id(v));
    let q = ItemQuery::from_params(params);

    type Row = (String, String, bool, String);
    let mut sql = String::from(
        "SELECT i.series_id, i.id, IFNULL(p.is_played, 0), IFNULL(p.last_updated, '')
         FROM items i LEFT JOIN playback_state p ON i.id = p.item_id AND p.user_id = ?
         WHERE i.media_type = 'Episode' AND i.series_id IS NOT NULL",
    );
    if raw.is_some() {
        sql.push_str(" AND i.series_id = ?");
    }
    // Só séries em que o usuário já assistiu algo: não carrega os episódios do resto da biblioteca.
    sql.push_str(
        " AND i.series_id IN (SELECT i2.series_id FROM items i2           JOIN playback_state p2 ON p2.item_id = i2.id AND p2.user_id = ? AND p2.is_played = 1           WHERE i2.series_id IS NOT NULL)",
    );
    let lib_clause = user.library_clause("i.library_id");
    if let Some((c, _)) = &lib_clause {
        sql.push_str(" AND ");
        sql.push_str(c);
    }
    sql.push_str(" ORDER BY i.series_id, i.season_number, i.episode_number, i.name");
    let mut query = sqlx::query_as::<_, Row>(&sql).bind(&user.id);
    if let Some(ref s) = raw {
        query = query.bind(s);
    }
    query = query.bind(&user.id);
    if let Some((_, vals)) = &lib_clause {
        for v in vals {
            query = query.bind(v);
        }
    }
    let rows = query.fetch_all(&pool).await.map_err(db_err)?;

    let ids = pick_next_up(rows);
    let limit = q.limit.unwrap_or(20).max(0) as usize;
    let ids: Vec<String> = ids.into_iter().take(limit).collect();
    let total = ids.len();
    let items = items::fetch_dtos_by_ids(&config, &pool, &user, &ids).await.map_err(db_err)?;

    Ok(Json(QueryResult {
        items,
        total_record_count: total,
        start_index: 0,
    }))
}

/// Entrada: episódios ordenados por série/temporada/episódio (série, id, assistido, última atividade).
/// Saída: IDs do próximo episódio de cada série, séries mais recentes primeiro.
pub fn pick_next_up(rows: Vec<(String, String, bool, String)>) -> Vec<String> {
    let mut result: Vec<(String, String)> = Vec::new(); // (última atividade, id do próximo)

    let mut start = 0;
    while start < rows.len() {
        let series = &rows[start].0;
        let end = rows[start..]
            .iter()
            .position(|r| &r.0 != series)
            .map_or(rows.len(), |p| start + p);
        let group = &rows[start..end];
        start = end;

        // Último episódio visto: o de atividade mais recente (empate → o mais adiante na ordem).
        let last_played = group
            .iter()
            .enumerate()
            .filter(|(_, r)| r.2)
            .max_by(|a, b| a.1 .3.cmp(&b.1 .3).then(a.0.cmp(&b.0)));
        let Some((idx, last)) = last_played else {
            continue;
        };
        if let Some(next) = group[idx + 1..].iter().find(|r| !r.2) {
            result.push((last.3.clone(), next.1.clone()));
        }
    }

    result.sort_by(|a, b| b.0.cmp(&a.0));
    result.into_iter().map(|(_, id)| id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(series: &str, id: &str, played: bool, at: &str) -> (String, String, bool, String) {
        (series.into(), id.into(), played, at.into())
    }

    #[test]
    fn next_up_picks_first_unplayed_after_last_played() {
        let rows = vec![
            r("a", "a1", true, "2026-01-01"),
            r("a", "a2", true, "2026-01-02"),
            r("a", "a3", false, ""),
            r("a", "a4", false, ""),
            r("b", "b1", true, "2026-02-01"),
            r("b", "b2", false, ""),
            r("c", "c1", false, ""), // nunca assistida: fora
            r("d", "d1", true, "2026-03-01"), // sem próximo: fora
        ];
        assert_eq!(pick_next_up(rows), vec!["b2", "a3"]);
    }

    #[test]
    fn next_up_skips_unplayed_gaps_before_last_played() {
        let rows = vec![
            r("a", "a1", false, ""),
            r("a", "a2", true, "2026-01-02"),
            r("a", "a3", false, ""),
        ];
        assert_eq!(pick_next_up(rows), vec!["a3"]);
    }

    #[test]
    fn season_names() {
        assert_eq!(season_name(Some(0)), "Especiais");
        assert_eq!(season_name(Some(3)), "Temporada 3");
    }

    #[test]
    fn ids_are_normalized() {
        assert_eq!(normalize_id("ABCD-ef01"), "abcdef01");
    }
}
