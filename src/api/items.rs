use crate::{
    api::{
        auth::CurrentUser,
        images::image_tag,
        shows::{self, normalize_id, season_name, VKind},
        views,
    },
    config::AppConfig,
    db::DbPool,
    models::{BaseItemDto, MediaSourceInfo, MediaStream, QueryResult, UserItemDataDto},
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

/// Parâmetros de consulta Jellyfin. Os clientes divergem em maiúsculas/minúsculas
/// (`ParentId`, `parentId`, `parentid`), então as chaves são normalizadas para minúsculas.
#[derive(Debug, Default)]
pub struct ItemQuery {
    pub parent_id: Option<String>,
    pub start_index: i64,
    pub limit: Option<i64>,
    pub sort_by: Vec<String>,
    pub descending: bool,
    pub filters: Vec<String>,
    pub is_played: Option<bool>,
    pub search_term: Option<String>,
    pub include_item_types: Vec<String>,
    pub season_number: Option<i32>,
    pub season_id: Option<String>,
    pub genres: Vec<String>,
    pub years: Vec<i32>,
    pub official_ratings: Vec<String>,
}

impl ItemQuery {
    pub fn from_params(params: HashMap<String, String>) -> Self {
        let p: HashMap<String, String> = params
            .into_iter()
            .map(|(k, v)| (k.to_lowercase(), v))
            .collect();
        let list = |key: &str| -> Vec<String> {
            p.get(key)
                .map(|v| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default()
        };
        Self {
            parent_id: p.get("parentid").filter(|s| !s.is_empty()).cloned(),
            start_index: p
                .get("startindex")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
                .max(0),
            limit: p
                .get("limit")
                .and_then(|v| v.parse().ok())
                .map(|l: i64| l.max(0)),
            sort_by: list("sortby"),
            descending: p
                .get("sortorder")
                .is_some_and(|v| v.to_lowercase().contains("descending")),
            filters: list("filters"),
            is_played: p.get("isplayed").and_then(|v| v.parse().ok()),
            search_term: p.get("searchterm").filter(|s| !s.is_empty()).cloned(),
            include_item_types: list("includeitemtypes"),
            season_number: p
                .get("season")
                .or_else(|| p.get("parentindexnumber"))
                .and_then(|v| v.parse().ok()),
            season_id: p.get("seasonid").filter(|s| !s.is_empty()).cloned(),
            genres: crate::api::facets::parse_pipe_list(p.get("genres")),
            years: list("years").iter().filter_map(|y| y.parse().ok()).collect(),
            official_ratings: crate::api::facets::parse_pipe_list(p.get("officialratings")),
        }
    }
}

#[derive(FromRow)]
struct DbItemRow {
    id: String,
    name: String,
    path: String,
    #[allow(dead_code)]
    media_type: String,
    container: Option<String>,
    duration_ticks: Option<i64>,
    size_bytes: Option<i64>,
    width: Option<i32>,
    height: Option<i32>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    streams_json: Option<String>,
    subtitle_path: Option<String>,
    modified_at: i64,
    poster_path: Option<String>,
    backdrop_path: Option<String>,
    production_year: Option<i32>,
    series_name: Option<String>,
    season_number: Option<i32>,
    episode_number: Option<i32>,
    series_id: Option<String>,
    season_id: Option<String>,
    library_id: Option<String>,
    tmdb_title: Option<String>,
    overview: Option<String>,
    rating: Option<f64>,
    genres: Option<String>,
    extra: Option<String>,
    position_ticks: Option<i64>,
    is_played: Option<bool>,
    play_count: Option<i32>,
    is_favorite: Option<bool>,
}

const SELECT_ITEM: &str = r"
    SELECT
        i.id, i.name, i.path, i.media_type, i.container, i.duration_ticks, i.size_bytes,
        i.width, i.height, i.video_codec, i.audio_codec, i.streams_json, i.subtitle_path,
        i.modified_at,
        COALESCE(i.poster_path, i.tmdb_poster) AS poster_path,
        COALESCE(i.backdrop_path, i.tmdb_backdrop) AS backdrop_path,
        i.production_year, i.series_name, i.season_number, i.episode_number,
        i.series_id, i.season_id, i.library_id,
        i.tmdb_title, i.overview, i.rating, i.genres,
        COALESCE(i.extra, (SELECT s.extra FROM series s WHERE s.id = i.series_id)) AS extra,
        p.position_ticks, p.is_played, p.play_count, p.is_favorite
    FROM items i
    LEFT JOIN playback_state p ON i.id = p.item_id AND p.user_id = ?
";

// O `userId` das rotas é ignorado: vale sempre o usuário do token (cada um vê só o que é seu).
pub async fn get_user_items(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    Path(_user_id): Path<String>,
    CurrentUser(user): CurrentUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    fetch_items_response(&config, &pool, &user, ItemQuery::from_params(params), false).await
}

pub async fn get_items(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    CurrentUser(user): CurrentUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    fetch_items_response(&config, &pool, &user, ItemQuery::from_params(params), false).await
}

/// "Continuar Assistindo"
pub async fn get_resume_items(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    Path(_user_id): Path<String>,
    CurrentUser(user): CurrentUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    fetch_items_response(&config, &pool, &user, ItemQuery::from_params(params), true).await
}

/// Variante sem `userId` na rota (`/UserItems/Resume`, usada por clientes 10.9+).
pub async fn get_resume_items_no_user(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    CurrentUser(user): CurrentUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    fetch_items_response(&config, &pool, &user, ItemQuery::from_params(params), true).await
}

/// "Adicionados recentemente": devolve um array simples, não um `QueryResult`.
/// Em views de séries devolve as séries mais recentes; em filmes, só filmes.
async fn latest_response(
    config: &AppConfig,
    pool: &DbPool,
    user: &AuthUser,
    params: HashMap<String, String>,
) -> Result<Json<Vec<BaseItemDto>>, StatusCode> {
    let q = ItemQuery::from_params(params);
    let limit = q.limit.unwrap_or(20);
    let scope = resolve_scope(pool, user, q.parent_id.as_deref()).await?;

    let (cond, bind): (&str, Option<String>) = match &scope {
        Scope::Unknown => return Ok(Json(vec![])),
        Scope::Library { id, tv: true } => {
            let mut q2 = q;
            q2.sort_by = vec!["DateCreated".to_string()];
            q2.descending = true;
            q2.limit = Some(limit);
            let r = shows::list_virtual(config, pool, user, VKind::Series, None, Some(id.clone()), &q2).await?;
            return Ok(Json(r.0.items));
        }
        Scope::Library { id, tv: false } => {
            ("WHERE i.media_type = 'Movie' AND i.library_id = ?", Some(id.clone()))
        }
        Scope::Series(id) => ("WHERE i.series_id = ?", Some(id.clone())),
        Scope::Season(id) => ("WHERE i.season_id = ?", Some(id.clone())),
        Scope::All => ("", None),
    };

    // Restrição às bibliotecas do usuário (junto da condição do escopo).
    let mut conds: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();
    if !cond.is_empty() {
        conds.push(cond.trim_start_matches("WHERE ").to_string());
        binds.extend(bind);
    }
    if let Some((sql, vals)) = user.library_clause("i.library_id") {
        conds.push(sql);
        binds.extend(vals);
    }
    let where_sql = if conds.is_empty() { String::new() } else { format!("WHERE {}", conds.join(" AND ")) };
    let sql = format!("{SELECT_ITEM} {where_sql} ORDER BY i.created_at DESC, i.name ASC LIMIT ?");
    let mut query = sqlx::query_as::<_, DbItemRow>(&sql).bind(&user.id);
    for b in &binds {
        query = query.bind(b);
    }
    let rows = query.bind(limit).fetch_all(pool).await.map_err(db_err)?;
    Ok(Json(rows.into_iter().map(|r| map_db_item_to_dto(config, r)).collect()))
}

pub async fn get_latest_items(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    Path(_user_id): Path<String>,
    CurrentUser(user): CurrentUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    latest_response(&config, &pool, &user, params).await
}

/// `/Items/Latest?userId=` (10.9+)
pub async fn get_latest_items_no_user(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    CurrentUser(user): CurrentUser,
    Query(params): Query<HashMap<String, String>>,
) -> Result<impl IntoResponse, StatusCode> {
    latest_response(&config, &pool, &user, params).await
}

pub fn db_err(e: sqlx::Error) -> StatusCode {
    tracing::error!("Erro de banco de dados: {}", e);
    StatusCode::INTERNAL_SERVER_ERROR
}

fn empty_result() -> Json<QueryResult<BaseItemDto>> {
    Json(QueryResult {
        items: vec![],
        total_record_count: 0,
        start_index: 0,
    })
}

/// Escapa curingas do LIKE (usa `\` como caractere de escape).
pub fn escape_like(term: &str) -> String {
    let mut out = String::with_capacity(term.len());
    for c in term.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Contexto de listagem derivado de `ParentId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    /// Biblioteca de filmes (`tv = false`) ou de séries (`tv = true`)
    Library { id: String, tv: bool },
    /// ID da série
    Series(String),
    /// ID da temporada
    Season(String),
    Unknown,
}

pub async fn resolve_scope(
    pool: &DbPool,
    user: &AuthUser,
    parent: Option<&str>,
) -> Result<Scope, StatusCode> {
    let Some(raw) = parent else {
        return Ok(Scope::All);
    };
    let pid = normalize_id(raw);
    if let Some(lib) = crate::libraries::get_by_id(pool, &pid).await.map_err(db_err)? {
        // Biblioteca sem permissão: como se não existisse.
        if !user.can_access_library(Some(&lib.id)) {
            return Ok(Scope::Unknown);
        }
        return Ok(Scope::Library { id: lib.id, tv: lib.collection_type == "tvshows" });
    }
    let series: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM items WHERE series_id = ? LIMIT 1")
        .bind(&pid)
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;
    if series.is_some() {
        return Ok(Scope::Series(pid));
    }
    let season: Option<(i64,)> = sqlx::query_as("SELECT 1 FROM items WHERE season_id = ? LIMIT 1")
        .bind(&pid)
        .fetch_optional(pool)
        .await
        .map_err(db_err)?;
    if season.is_some() {
        return Ok(Scope::Season(pid));
    }
    Ok(Scope::Unknown)
}

async fn fetch_items_response(
    config: &AppConfig,
    pool: &DbPool,
    user: &AuthUser,
    query: ItemQuery,
    resume_only: bool,
) -> Result<Json<QueryResult<BaseItemDto>>, StatusCode> {
    let scope = resolve_scope(pool, user, query.parent_id.as_deref()).await?;
    fetch_scoped(config, pool, user, query, resume_only, scope).await
}

pub async fn fetch_scoped(
    config: &AppConfig,
    pool: &DbPool,
    user: &AuthUser,
    query: ItemQuery,
    resume_only: bool,
    scope: Scope,
) -> Result<Json<QueryResult<BaseItemDto>>, StatusCode> {
    // Tipos pedidos (Video = Movie + Episode).
    let (mut want_movie, mut want_episode, mut want_series, mut want_season) =
        (false, false, false, false);
    for t in &query.include_item_types {
        match t.to_lowercase().as_str() {
            "movie" => want_movie = true,
            "episode" => want_episode = true,
            "series" => want_series = true,
            "season" => want_season = true,
            "video" => {
                want_movie = true;
                want_episode = true;
            }
            _ => {}
        }
    }
    if !query.include_item_types.is_empty()
        && !want_movie
        && !want_episode
        && !want_series
        && !want_season
    {
        return Ok(empty_result());
    }

    // Séries e temporadas são itens virtuais; o resto vem da tabela `items`.
    match &scope {
        Scope::Unknown => return Ok(empty_result()),
        Scope::Library { id, tv: true } => {
            // "Continuar assistindo" é sempre de episódios, nunca das séries agrupadas.
            if !resume_only && (!want_episode || want_series || want_season) {
                let kind = if want_season && !want_series { VKind::Season } else { VKind::Series };
                return shows::list_virtual(config, pool, user, kind, None, Some(id.clone()), &query).await;
            }
        }
        Scope::Series(sid) => {
            if !want_episode || want_season {
                return shows::list_virtual(config, pool, user, VKind::Season, Some(sid.clone()), None, &query).await;
            }
        }
        Scope::All => {
            if (want_series || want_season) && !(want_movie || want_episode) {
                let kind = if want_series { VKind::Series } else { VKind::Season };
                return shows::list_virtual(config, pool, user, kind, None, None, &query).await;
            }
        }
        Scope::Library { tv: false, .. } | Scope::Season(_) => {}
    }

    let mut conds: Vec<String> = Vec::new();
    let mut binds: Vec<String> = Vec::new();
    match &scope {
        Scope::Library { id, tv: false } => {
            conds.push("i.media_type = 'Movie' AND i.library_id = ?".to_string());
            binds.push(id.clone());
        }
        Scope::Library { id, tv: true } => {
            conds.push("i.media_type = 'Episode' AND i.library_id = ?".to_string());
            binds.push(id.clone());
        }
        Scope::Series(sid) => {
            conds.push("i.media_type = 'Episode' AND i.series_id = ?".to_string());
            binds.push(sid.clone());
        }
        Scope::Season(sid) => {
            conds.push("i.season_id = ?".to_string());
            binds.push(sid.clone());
        }
        Scope::All | Scope::Unknown => {}
    }
    // Só as bibliotecas que o usuário pode ver.
    if let Some((sql, vals)) = user.library_clause("i.library_id") {
        conds.push(sql);
        binds.extend(vals);
    }
    match (want_movie, want_episode) {
        (true, false) => conds.push("i.media_type = 'Movie'".to_string()),
        (false, true) => conds.push("i.media_type = 'Episode'".to_string()),
        _ => {}
    }
    if let Some(n) = query.season_number {
        conds.push("i.season_number = ?".to_string());
        binds.push(n.to_string());
    }

    if resume_only
        || query
            .filters
            .iter()
            .any(|f| f.eq_ignore_ascii_case("IsResumable"))
    {
        conds.push("IFNULL(p.position_ticks, 0) > 0 AND IFNULL(p.is_played, 0) = 0".to_string());
    }
    if query.filters.iter().any(|f| f.eq_ignore_ascii_case("IsFavorite")) {
        conds.push("IFNULL(p.is_favorite, 0) = 1".to_string());
    }
    let want_played = if query.filters.iter().any(|f| f.eq_ignore_ascii_case("IsPlayed")) {
        Some(true)
    } else if query.filters.iter().any(|f| f.eq_ignore_ascii_case("IsUnplayed")) {
        Some(false)
    } else {
        query.is_played
    };
    match want_played {
        Some(true) => conds.push("IFNULL(p.is_played, 0) = 1".to_string()),
        Some(false) => conds.push("IFNULL(p.is_played, 0) = 0".to_string()),
        None => {}
    }
    if let Some(ref term) = query.search_term {
        binds.push(format!("%{}%", escape_like(term)));
        conds.push("i.name LIKE ? ESCAPE '\\'".to_string());
    }
    let (fc, fb) = crate::api::facets::filter_conds(
        &query,
        "COALESCE(i.genres, (SELECT s.genres FROM series s WHERE s.id = i.series_id))",
        "i.production_year",
        "json_extract(COALESCE(i.extra, (SELECT s.extra FROM series s WHERE s.id = i.series_id)), '$.official_rating')",
    );
    conds.extend(fc);
    binds.extend(fb);
    let where_sql = if conds.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", conds.join(" AND "))
    };

    // Ordenação: lista branca de colunas (nunca interpola entrada do cliente).
    let dir = if query.descending { "DESC" } else { "ASC" };
    let mut order: Vec<String> = Vec::new();
    for key in &query.sort_by {
        match key.to_lowercase().as_str() {
            "name" | "sortname" => order.push(format!("i.name COLLATE NOCASE {dir}")),
            "datecreated" | "datelastcontentadded" => order.push(format!("i.created_at {dir}")),
            "runtime" | "runtimeticks" => order.push(format!("i.duration_ticks {dir}")),
            "dateplayed" | "datelastsaved" => order.push(format!("p.last_updated {dir}")),
            "random" => order.push("RANDOM()".to_string()),
            _ => {}
        }
    }
    if order.is_empty() && matches!(scope, Scope::Series(_) | Scope::Season(_)) {
        order.push("i.season_number ASC".to_string());
        order.push("i.episode_number ASC".to_string());
    }
    if resume_only && order.is_empty() {
        order.push("p.last_updated DESC".to_string());
    }
    order.push("i.name COLLATE NOCASE ASC".to_string());
    let order_sql = order.join(", ");

    let count_sql = format!(
        "SELECT COUNT(*) FROM items i LEFT JOIN playback_state p ON i.id = p.item_id AND p.user_id = ? {where_sql}"
    );
    let mut count_q = sqlx::query_as::<_, (i64,)>(&count_sql).bind(&user.id);
    for b in &binds {
        count_q = count_q.bind(b);
    }
    let (total,) = count_q.fetch_one(pool).await.map_err(db_err)?;

    let list_sql = format!("{SELECT_ITEM} {where_sql} ORDER BY {order_sql} LIMIT ? OFFSET ?");
    let mut list_q = sqlx::query_as::<_, DbItemRow>(&list_sql).bind(&user.id);
    for b in &binds {
        list_q = list_q.bind(b);
    }
    // LIMIT -1 = sem limite no SQLite (o Jellyfin devolve tudo quando Limit não é enviado).
    let rows = list_q
        .bind(query.limit.unwrap_or(-1))
        .bind(query.start_index)
        .fetch_all(pool)
        .await
        .map_err(db_err)?;

    let items = rows
        .into_iter()
        .map(|row| map_db_item_to_dto(config, row))
        .collect();

    Ok(Json(QueryResult {
        items,
        total_record_count: total as usize,
        start_index: query.start_index as usize,
    }))
}

pub async fn get_user_item_by_id(
    State(state): State<(Arc<AppConfig>, DbPool)>,
    Path((_user_id, item_id)): Path<(String, String)>,
    user: CurrentUser,
) -> Result<impl IntoResponse, StatusCode> {
    get_item_by_id(State(state), Path(item_id), user).await
}

pub async fn get_item_by_id(
    State((config, pool)): State<(Arc<AppConfig>, DbPool)>,
    Path(item_id): Path<String>,
    CurrentUser(user): CurrentUser,
) -> Result<impl IntoResponse, StatusCode> {
    let id = normalize_id(&item_id);
    let sql = format!("{SELECT_ITEM} WHERE i.id = ?");
    let row = sqlx::query_as::<_, DbItemRow>(&sql)
        .bind(&user.id)
        .bind(&id)
        .fetch_optional(&pool)
        .await
        .map_err(db_err)?;
    if let Some(row) = row {
        if !user.can_access_library(row.library_id.as_deref()) {
            return Err(StatusCode::NOT_FOUND);
        }
        return Ok(Json(map_db_item_to_dto(&config, row)));
    }
    // Views, séries e temporadas não estão em `items`.
    if let Some(view) = views::view_by_id(&config, &pool, &id).await {
        if !user.can_access_library(Some(&view.id)) {
            return Err(StatusCode::NOT_FOUND);
        }
        return Ok(Json(view));
    }
    match shows::get_virtual_item(&config, &pool, &user, &id).await.map_err(db_err)? {
        Some(dto) => Ok(Json(dto)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

/// Busca itens por ID preservando a ordem pedida (usado por `NextUp`).
pub async fn fetch_dtos_by_ids(
    config: &AppConfig,
    pool: &DbPool,
    user: &AuthUser,
    ids: &[String],
) -> Result<Vec<BaseItemDto>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let sql = format!("{SELECT_ITEM} WHERE i.id IN ({placeholders})");
    let mut q = sqlx::query_as::<_, DbItemRow>(&sql).bind(&user.id);
    for id in ids {
        q = q.bind(id);
    }
    let mut rows = q.fetch_all(pool).await?;
    rows.sort_by_key(|r| ids.iter().position(|i| i == &r.id).unwrap_or(usize::MAX));
    Ok(rows.into_iter().map(|r| map_db_item_to_dto(config, r)).collect())
}

/// Legendas de texto são entregues como arquivo externo (`DeliveryUrl`); as de imagem (PGS/DVD) só
/// funcionam queimadas no vídeo (transcode).
pub fn decorate_subtitles(item_id: &str, streams: &mut [MediaStream]) {
    for s in streams.iter_mut().filter(|s| s.r#type == "Subtitle") {
        if crate::transcode::is_text_subtitle(&s.codec) {
            s.is_text_subtitle_stream = true;
            s.supports_external_stream = true;
            s.delivery_method = Some("External".to_string());
            s.delivery_url = Some(format!(
                "/Videos/{item_id}/{item_id}/Subtitles/{}/0/Stream.{}",
                s.index,
                crate::transcode::subtitle_ext(&s.codec)
            ));
        }
    }
}

/// Usa as trilhas do ffprobe (ou os codecs principais) e acrescenta a legenda do mesmo nome.
pub fn build_streams(
    streams_json: Option<&str>,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    width: Option<i32>,
    height: Option<i32>,
    has_sidecar: bool,
) -> Vec<MediaStream> {
    let mut streams = embedded_streams(streams_json, video_codec, audio_codec, width, height);
    if has_sidecar {
        let last_index = streams.iter().map(|s| s.index).max().unwrap_or(-1);
        if let Some(index) = last_index.checked_add(1) {
            streams.push(MediaStream {
                display_title: Some("SRT".to_string()),
                is_default: false,
                is_external: true,
                ..MediaStream::basic("subrip", "Subtitle", index)
            });
        }
    }
    streams
}

fn embedded_streams(
    streams_json: Option<&str>,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    width: Option<i32>,
    height: Option<i32>,
) -> Vec<MediaStream> {
    if let Some(parsed) = streams_json.and_then(|j| serde_json::from_str::<Vec<MediaStream>>(j).ok()) {
        if !parsed.is_empty() {
            return parsed;
        }
    }

    let mut streams = Vec::new();
    if let Some(v) = video_codec {
        streams.push(MediaStream {
            width,
            height,
            display_title: Some(match height {
                Some(h) => format!("{v} ({h}p)"),
                None => v.to_string(),
            }),
            ..MediaStream::basic(v, "Video", 0)
        });
    }
    if let Some(a) = audio_codec {
        streams.push(MediaStream {
            channels: Some(2),
            sample_rate: Some(48000),
            display_title: Some(a.to_string()),
            ..MediaStream::basic(a, "Audio", 1)
        });
    }
    streams
}

fn map_db_item_to_dto(config: &AppConfig, row: DbItemRow) -> BaseItemDto {
    let mut streams = build_streams(
        row.streams_json.as_deref(),
        row.video_codec.as_deref(),
        row.audio_codec.as_deref(),
        row.width,
        row.height,
        row.subtitle_path.is_some(),
    );
    decorate_subtitles(&row.id, &mut streams);

    let container = row.container.clone();
    let media_source = MediaSourceInfo {
        id: row.id.clone(),
        path: Some(row.path.clone()),
        protocol: "File".to_string(),
        media_streams: streams.clone(),
        container: crate::api::stream::jellyfin_container(container.as_deref()),
        size: row.size_bytes,
        name: row.name.clone(),
        is_remote: false,
        run_time_ticks: row.duration_ticks,
        supports_transcoding: false,
        bitrate: match (row.size_bytes, row.duration_ticks) {
            (Some(size), Some(ticks)) if size > 0 && ticks > 0 => {
                Some((size as f64 * 8.0 / (ticks as f64 / 10_000_000.0)) as i64)
            }
            _ => None,
        },
        supports_direct_stream: true,
        supports_direct_play: true,
        // Como o Jellyfin: sem URL pronta; o cliente monta `/Videos/{id}/stream.{container}?...` sozinho.
        direct_stream_url: None,
        transcoding_url: None,
        transcoding_sub_protocol: None,
        transcoding_container: None,
    };

    // Clientes só pedem imagens listadas em ImageTags. Primary existe se há poster local
    // ou se o vídeo foi inspecionado (miniatura via ffmpeg); Backdrop só com arquivo local.
    let mut image_tags = std::collections::HashMap::new();
    if row.poster_path.is_some() || row.video_codec.is_some() {
        image_tags.insert(
            "Primary".to_string(),
            image_tag(row.poster_path.as_deref(), row.modified_at),
        );
    }
    // Moonfin e outros clientes só usam itens com Backdrop na faixa de destaque. Sem arquivo
    // local/TMDB, o servidor gera um quadro do vídeo (ver api/images.rs).
    let backdrop_image_tags = match (row.backdrop_path.as_deref(), row.video_codec.as_deref()) {
        (Some(b), _) => vec![image_tag(Some(b), row.modified_at)],
        (None, Some(_)) => vec![image_tag(Some("frame-backdrop"), row.modified_at)],
        (None, None) => Vec::new(),
    };

    let display_name = row.tmdb_title.clone().unwrap_or_else(|| row.name.clone());
    let item_type = if row.media_type == "Episode" { "Episode" } else { "Movie" };
    let genres = row
        .genres
        .as_deref()
        .and_then(|g| serde_json::from_str::<Vec<String>>(g).ok())
        .unwrap_or_default();
    let is_episode = item_type == "Episode";
    let played = row.is_played.unwrap_or(false);
    let extra_json = row.extra.clone();
    let mut dto = BaseItemDto {
        id: row.id,
        name: display_name,
        original_title: None,
        server_id: config.server_id.clone(),
        r#type: item_type.to_string(),
        media_type: Some("Video".to_string()),
        container: crate::api::stream::jellyfin_container(container.as_deref()),
        path: Some(row.path),
        run_time_ticks: row.duration_ticks,
        size: row.size_bytes,
        width: row.width,
        height: row.height,
        is_folder: false,
        user_data: Some(UserItemDataDto {
            playback_position_ticks: row.position_ticks.unwrap_or(0),
            play_count: row.play_count.unwrap_or(i32::from(played)),
            is_favorite: row.is_favorite.unwrap_or(false),
            played,
            unplayed_item_count: None,
        }),
        media_sources: Some(vec![media_source]),
        media_streams: Some(streams),
        image_tags,
        backdrop_image_tags,
        production_year: row.production_year,
        overview: row.overview,
        community_rating: row.rating,
        genres,
        series_name: if is_episode { row.series_name } else { None },
        parent_index_number: if is_episode { row.season_number } else { None },
        index_number: if is_episode { row.episode_number } else { None },
        series_id: if is_episode { row.series_id } else { None },
        season_id: if is_episode { row.season_id } else { None },
        season_name: if is_episode { row.season_number.map(|n| season_name(Some(n))) } else { None },
        ..Default::default()
    };
    crate::api::facets::apply_extra(&mut dto, extra_json.as_deref());
    dto
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_keys_are_case_insensitive() {
        let mut m = HashMap::new();
        m.insert("ParentId".into(), "abc".into());
        m.insert("startindex".into(), "5".into());
        m.insert("SortOrder".into(), "Descending".into());
        m.insert("Filters".into(), "IsPlayed,IsResumable".into());
        let q = ItemQuery::from_params(m);
        assert_eq!(q.parent_id.as_deref(), Some("abc"));
        assert_eq!(q.start_index, 5);
        assert!(q.descending);
        assert_eq!(q.filters.len(), 2);
        assert_eq!(q.limit, None);
    }

    #[test]
    fn like_escape() {
        assert_eq!(escape_like("50%_a\\"), "50\\%\\_a\\\\");
    }
}
