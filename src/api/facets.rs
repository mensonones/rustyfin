//! Gêneros, estúdios, pessoas, classificações e anos: campos extras dos itens, listas
//! (`/Genres`, `/Studios`, `/Persons`, `/Items/Filters`) e condições SQL dos filtros de `/Items`.

use crate::{
    api::{auth::CurrentUser, items::ItemQuery},
    config::AppConfig,
    db::DbPool,
    metadata::Extra,
    models::{BaseItemDto, MediaUrl, NameId, PersonDto, QueryResult},
};
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
};
use uuid::Uuid;

/// ID determinístico de gênero/estúdio/pessoa (os clientes só o repassam).
pub fn stable_id(kind: &str, name: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("{kind}:{}", name.trim().to_lowercase()).as_bytes())
        .simple()
        .to_string()
}

/// Preenche gêneros (com ID), estúdios, elenco, classificação, slogan e trailers no DTO.
pub fn apply_extra(dto: &mut BaseItemDto, extra_json: Option<&str>) {
    dto.genre_items = dto
        .genres
        .iter()
        .map(|g| NameId { name: g.clone(), id: stable_id("genre", g) })
        .collect();
    let Some(extra) = extra_json.and_then(|j| serde_json::from_str::<Extra>(j).ok()) else {
        return;
    };
    dto.studios = extra
        .studios
        .iter()
        .map(|s| NameId { name: s.clone(), id: stable_id("studio", s) })
        .collect();
    dto.people = extra
        .people
        .iter()
        .map(|p| PersonDto {
            name: p.name.clone(),
            id: stable_id("person", &p.name),
            role: p.role.clone(),
            r#type: p.kind.clone(),
        })
        .collect();
    dto.official_rating = extra.official_rating;
    dto.taglines = extra.tagline.into_iter().collect();
    dto.remote_trailers = extra
        .trailers
        .into_iter()
        .map(|t| MediaUrl { url: t.url, name: t.name })
        .collect();
}

fn pipe_or_comma_list(v: &str) -> Vec<String> {
    v.split(['|', ','])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Condições SQL e valores para `Genres`, `Years` e `OfficialRatings` do pedido.
/// `genres_col` guarda um array JSON de nomes; `rating_expr` é a expressão da classificação.
pub fn filter_conds(q: &ItemQuery, genres_col: &str, year_col: &str, rating_expr: &str) -> (Vec<String>, Vec<String>) {
    let mut conds = Vec::new();
    let mut binds = Vec::new();
    if !q.genres.is_empty() {
        let parts: Vec<String> = q.genres.iter().map(|_| format!("{genres_col} LIKE ? ESCAPE '\\'")).collect();
        conds.push(format!("({})", parts.join(" OR ")));
        for g in &q.genres {
            let json_name = serde_json::to_string(g).unwrap_or_default();
            binds.push(format!("%{}%", crate::api::items::escape_like(&json_name)));
        }
    }
    if !q.years.is_empty() {
        conds.push(format!("{year_col} IN ({})", vec!["?"; q.years.len()].join(",")));
        binds.extend(q.years.iter().map(|y| y.to_string()));
    }
    if !q.official_ratings.is_empty() {
        conds.push(format!("{rating_expr} IN ({})", vec!["?"; q.official_ratings.len()].join(",")));
        binds.extend(q.official_ratings.iter().cloned());
    }
    (conds, binds)
}

pub fn parse_pipe_list(v: Option<&String>) -> Vec<String> {
    v.map(|v| pipe_or_comma_list(v)).unwrap_or_default()
}

#[derive(Default)]
struct Facets {
    genres: BTreeSet<String>,
    studios: BTreeSet<String>,
    people: BTreeMap<String, String>, // nome -> tipo
    ratings: BTreeSet<String>,
    years: BTreeSet<i32>,
}

async fn collect(pool: &DbPool, user: &crate::users::AuthUser) -> Result<Facets, sqlx::Error> {
    let mut sql = String::from(
        "SELECT DISTINCT i.genres, COALESCE(i.extra, s.extra), COALESCE(i.production_year, s.year)
         FROM items i LEFT JOIN series s ON s.id = i.series_id WHERE 1 = 1",
    );
    let lib = user.library_clause("i.library_id");
    if let Some((c, _)) = &lib {
        sql.push_str(" AND ");
        sql.push_str(c);
    }
    let mut q = sqlx::query_as::<_, (Option<String>, Option<String>, Option<i32>)>(&sql);
    if let Some((_, vals)) = &lib {
        for v in vals {
            q = q.bind(v);
        }
    }
    let mut out = Facets::default();
    for (genres, extra, year) in q.fetch_all(pool).await? {
        if let Some(g) = genres.and_then(|g| serde_json::from_str::<Vec<String>>(&g).ok()) {
            out.genres.extend(g);
        }
        if let Some(e) = extra.and_then(|e| serde_json::from_str::<Extra>(&e).ok()) {
            out.studios.extend(e.studios);
            for p in e.people {
                out.people.entry(p.name).or_insert(p.kind);
            }
            out.ratings.extend(e.official_rating);
        }
        out.years.extend(year);
    }
    Ok(out)
}

fn named(config: &AppConfig, kind: &str, item_type: &str, name: &str) -> BaseItemDto {
    BaseItemDto {
        id: stable_id(kind, name),
        name: name.to_string(),
        server_id: config.server_id.clone(),
        r#type: item_type.to_string(),
        is_folder: true,
        ..Default::default()
    }
}

fn paged(config: &AppConfig, mut items: Vec<BaseItemDto>, params: &HashMap<String, String>) -> Json<QueryResult<BaseItemDto>> {
    let _ = config;
    let get = |k: &str| {
        params
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.as_str())
    };
    if let Some(term) = get("searchTerm").map(str::to_lowercase).filter(|t| !t.is_empty()) {
        items.retain(|i| i.name.to_lowercase().contains(&term));
    }
    let total = items.len();
    let start = get("startIndex").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
    let limit = get("limit").and_then(|v| v.parse::<usize>().ok());
    let items: Vec<BaseItemDto> = items.into_iter().skip(start).take(limit.unwrap_or(usize::MAX)).collect();
    Json(QueryResult { items, total_record_count: total, start_index: start })
}

type St = State<(Arc<AppConfig>, DbPool)>;

fn db_err(e: sqlx::Error) -> StatusCode {
    tracing::error!("Erro de banco de dados: {e}");
    StatusCode::INTERNAL_SERVER_ERROR
}

pub async fn genres(
    State((config, pool)): St,
    CurrentUser(user): CurrentUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    let f = collect(&pool, &user).await.map_err(db_err)?;
    let items = f.genres.iter().map(|g| named(&config, "genre", "Genre", g)).collect();
    Ok(paged(&config, items, &params))
}

pub async fn studios(
    State((config, pool)): St,
    CurrentUser(user): CurrentUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    let f = collect(&pool, &user).await.map_err(db_err)?;
    let items = f.studios.iter().map(|s| named(&config, "studio", "Studio", s)).collect();
    Ok(paged(&config, items, &params))
}

pub async fn persons(
    State((config, pool)): St,
    CurrentUser(user): CurrentUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    let f = collect(&pool, &user).await.map_err(db_err)?;
    let wanted = params
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("personTypes"))
        .map(|(_, v)| pipe_or_comma_list(v))
        .unwrap_or_default();
    let items = f
        .people
        .iter()
        .filter(|(_, kind)| wanted.is_empty() || wanted.iter().any(|w| w.eq_ignore_ascii_case(kind)))
        .map(|(name, _)| named(&config, "person", "Person", name))
        .collect();
    Ok(paged(&config, items, &params))
}

/// `/Items/Filters`: valores disponíveis para filtrar a biblioteca.
pub async fn filters(State((_, pool)): St, CurrentUser(user): CurrentUser) -> Result<impl IntoResponse, StatusCode> {
    let f = collect(&pool, &user).await.map_err(db_err)?;
    Ok(Json(json!({
        "Genres": f.genres,
        "Tags": [],
        "OfficialRatings": f.ratings,
        "Years": f.years.iter().rev().collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_case_insensitive() {
        assert_eq!(stable_id("genre", "Ação"), stable_id("genre", " ação "));
        assert_ne!(stable_id("genre", "Ação"), stable_id("studio", "Ação"));
    }

    #[test]
    fn extra_fills_dto() {
        let mut dto = BaseItemDto { genres: vec!["Ação".into()], ..Default::default() };
        let json = r#"{"studios":["Lucasfilm"],"people":[{"name":"Pedro Pascal","role":"Din Djarin","kind":"Actor"}],
            "official_rating":"14","tagline":"Esta é a missão","trailers":[{"name":"Trailer","url":"https://www.youtube.com/watch?v=abc"}]}"#;
        apply_extra(&mut dto, Some(json));
        assert_eq!(dto.genre_items.len(), 1);
        assert_eq!(dto.studios[0].name, "Lucasfilm");
        assert_eq!(dto.people[0].r#type, "Actor");
        assert_eq!(dto.people[0].role.as_deref(), Some("Din Djarin"));
        assert_eq!(dto.official_rating.as_deref(), Some("14"));
        assert_eq!(dto.taglines, vec!["Esta é a missão"]);
        assert_eq!(dto.remote_trailers[0].url, "https://www.youtube.com/watch?v=abc");
        // sem dados extras só os gêneros
        let mut d2 = BaseItemDto { genres: vec!["Drama".into()], ..Default::default() };
        apply_extra(&mut d2, None);
        assert_eq!(d2.genre_items.len(), 1);
        assert!(d2.people.is_empty() && d2.official_rating.is_none());
    }

    #[test]
    fn filter_conditions() {
        let q = ItemQuery {
            genres: vec!["Ação".into(), "Drama".into()],
            years: vec![2014, 2021],
            official_ratings: vec!["14".into()],
            ..Default::default()
        };
        let (c, b) = filter_conds(&q, "i.genres", "i.production_year", "r");
        assert_eq!(c.len(), 3);
        assert!(c[0].starts_with("(i.genres LIKE ?") && c[0].contains(" OR "));
        assert_eq!(c[1], "i.production_year IN (?,?)");
        assert_eq!(b, vec!["%\"Ação\"%", "%\"Drama\"%", "2014", "2021", "14"]);
    }
}
