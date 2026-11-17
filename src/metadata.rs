//! Provider de metadados TMDB (opcional: só roda com `TMDB_API_KEY`).
//!
//! Usa `curl` como cliente HTTP (mesma abordagem do `ffprobe`): sem dependência de TLS no
//! binário. A chave é enviada por stdin (`curl -K -`) e nunca aparece na lista de processos.

use crate::{config::AppConfig, db::DbPool};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncWriteExt, process::Command};
use tracing::{info, warn};

const RETRY_AFTER_SECS: i64 = 7 * 24 * 3600;
const REQUEST_TIMEOUT_SECS: u32 = 20;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Info {
    pub id: i64,
    pub title: Option<String>,
    pub overview: Option<String>,
    pub rating: Option<f64>,
    pub genres: Vec<String>,
    pub year: Option<i32>,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
}

#[derive(Debug)]
enum FetchError {
    /// Sem rede / curl ausente: aborta a rodada (tenta de novo no próximo scan).
    Transport(String),
    /// 401/403: chave inválida, aborta a rodada.
    Unauthorized,
    /// Outros status HTTP (404 etc.): tratado como "não encontrado".
    Http(u16),
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Tokens v4 ("API Read Access Token") são JWTs longos; chaves v3 têm 32 caracteres.
fn is_bearer_token(key: &str) -> bool {
    key.len() > 40
}

fn build_url(config: &AppConfig, key: &str, path: &str, params: &[(&str, &str)]) -> String {
    let mut url = format!("{}{}", config.tmdb_base_url.trim_end_matches('/'), path);
    let mut sep = '?';
    for (k, v) in params {
        url.push(sep);
        url.push_str(&format!("{}={}", k, percent_encode(v)));
        sep = '&';
    }
    if !is_bearer_token(key) {
        url.push(sep);
        url.push_str(&format!("api_key={}", percent_encode(key)));
    }
    url
}

/// Executa `curl` com a configuração via stdin. Retorna (status HTTP, corpo).
async fn curl(url: &str, bearer: Option<&str>, output: Option<&PathBuf>) -> Result<(u16, Vec<u8>), FetchError> {
    let mut cfg = format!("url = \"{url}\"\n");
    if let Some(token) = bearer {
        cfg.push_str(&format!("header = \"Authorization: Bearer {token}\"\n"));
    }
    if let Some(out) = output {
        cfg.push_str(&format!("output = \"{}\"\n", out.to_string_lossy().replace('\\', "/")));
    }

    let mut child = Command::new("curl")
        .args(["-sS", "-L", "--max-time", &REQUEST_TIMEOUT_SECS.to_string()])
        .args(["-w", "\n%{http_code}", "-K", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| FetchError::Transport(format!("curl indisponível: {e}")))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(cfg.as_bytes())
            .await
            .map_err(|e| FetchError::Transport(e.to_string()))?;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| FetchError::Transport(e.to_string()))?;

    if !out.status.success() {
        return Err(FetchError::Transport(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }

    // Corpo + "\n" + código HTTP
    let stdout = out.stdout;
    let split = stdout.iter().rposition(|b| *b == b'\n').unwrap_or(0);
    let status: u16 = String::from_utf8_lossy(&stdout[split..])
        .trim()
        .parse()
        .unwrap_or(0);
    let body = stdout[..split].to_vec();

    match status {
        200..=299 => Ok((status, body)),
        401 | 403 => Err(FetchError::Unauthorized),
        s => Err(FetchError::Http(s)),
    }
}

async fn get_json(
    config: &AppConfig,
    key: &str,
    path: &str,
    params: &[(&str, &str)],
) -> Result<Value, FetchError> {
    let url = build_url(config, key, path, params);
    let bearer = is_bearer_token(key).then_some(key);
    let (_, body) = curl(&url, bearer, None).await?;
    serde_json::from_slice(&body).map_err(|e| FetchError::Transport(format!("JSON inválido: {e}")))
}

// ---------- parsing (puro, testável) ----------

fn year_from(date: Option<&str>) -> Option<i32> {
    date?.get(..4)?.parse().ok()
}

fn non_empty(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// Detalhes de filme (`/movie/{id}`) ou série (`/tv/{id}`).
pub fn parse_details(v: &Value) -> Option<Info> {
    let id = v.get("id")?.as_i64()?;
    let genres = v
        .get("genres")
        .and_then(|g| g.as_array())
        .map(|a| a.iter().filter_map(|g| non_empty(g.get("name"))).collect())
        .unwrap_or_default();
    Some(Info {
        id,
        title: non_empty(v.get("title")).or_else(|| non_empty(v.get("name"))),
        overview: non_empty(v.get("overview")),
        rating: v.get("vote_average").and_then(|r| r.as_f64()).filter(|r| *r > 0.0),
        genres,
        year: year_from(
            non_empty(v.get("release_date"))
                .or_else(|| non_empty(v.get("first_air_date")))
                .as_deref(),
        ),
        poster: non_empty(v.get("poster_path")),
        backdrop: non_empty(v.get("backdrop_path")),
    })
}

/// Primeiro resultado de `/search/*`.
pub fn first_result_id(v: &Value) -> Option<i64> {
    v.get("results")?.as_array()?.first()?.get("id")?.as_i64()
}

/// Caminhos de imagem do TMDB são `/hash.jpg`; rejeita qualquer outra coisa.
fn safe_image_path(p: &str) -> Option<&str> {
    let name = p.strip_prefix('/')?;
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    .then_some(name)
}

async fn download_image(config: &AppConfig, rel: &str, size: &str) -> Option<String> {
    let name = safe_image_path(rel)?;
    let dir = config.cache_dir.join("tmdb");
    tokio::fs::create_dir_all(&dir).await.ok()?;
    let dest = dir.join(format!("{size}_{name}"));
    if tokio::fs::metadata(&dest).await.is_ok() {
        return Some(dest.to_string_lossy().to_string());
    }
    let tmp = dir.join(format!("{size}_{name}.part"));
    let url = format!("{}/{}{}", config.tmdb_image_base.trim_end_matches('/'), size, rel);
    // Imagens do CDN não exigem chave.
    match curl(&url, None, Some(&tmp)).await {
        Ok(_) if tokio::fs::metadata(&tmp).await.is_ok() => {
            tokio::fs::rename(&tmp, &dest).await.ok()?;
            Some(dest.to_string_lossy().to_string())
        }
        _ => {
            let _ = tokio::fs::remove_file(&tmp).await;
            None
        }
    }
}

// ---------- fluxo principal ----------

type PendingRow = (String, String, String, Option<i32>, Option<String>, Option<i32>, Option<i32>, Option<String>);

/// Busca metadados para itens ainda não resolvidos. Sem `TMDB_API_KEY` não faz nada.
const TMDB_CONCURRENCY: usize = 6;

pub async fn enrich_pending(pool: &DbPool, config: &AppConfig) {
    let Some(key) = crate::settings::tmdb_api_key() else {
        return;
    };

    // Independe dos itens pendentes: também preenche séries já enriquecidas antes desta função existir.
    enrich_seasons(pool, config, &key).await;

    let rows: Result<Vec<PendingRow>, _> = sqlx::query_as(
        "SELECT id, media_type, name, production_year, series_name, season_number, episode_number, series_id
         FROM items WHERE tmdb_id IS NULL AND tmdb_checked_at < ? ORDER BY name",
    )
    .bind(now_secs() - RETRY_AFTER_SECS)
    .fetch_all(pool)
    .await;
    let rows = match rows {
        Ok(r) => r,
        Err(e) => {
            warn!("TMDB: falha ao listar itens pendentes: {}", e);
            return;
        }
    };
    if rows.is_empty() {
        return;
    }
    info!("TMDB: buscando metadados de {} itens...", rows.len());

    // Lotes independentes rodam em paralelo (limitado): cada filme é um lote; os episódios de uma
    // mesma série ficam juntos para compartilhar o cache de busca da série.
    let mut batches: Vec<Vec<PendingRow>> = Vec::new();
    let mut by_series: HashMap<String, usize> = HashMap::new();
    for row in rows {
        if row.1 == "Episode" {
            let key = row.4.clone().unwrap_or_default().to_lowercase();
            match by_series.get(&key) {
                Some(&i) => batches[i].push(row),
                None => {
                    by_series.insert(key, batches.len());
                    batches.push(vec![row]);
                }
            }
        } else {
            batches.push(vec![row]);
        }
    }

    let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(TMDB_CONCURRENCY));
    let config = std::sync::Arc::new(config.clone());
    let key = std::sync::Arc::new(key);
    let mut tasks = tokio::task::JoinSet::new();
    for batch in batches {
        let (pool, config, key, gate) = (pool.clone(), config.clone(), key.clone(), gate.clone());
        tasks.spawn(async move {
            let Ok(_permit) = gate.acquire_owned().await else {
                return (0, false);
            };
            process_batch(&pool, &config, &key, batch).await
        });
    }
    let mut found = 0;
    while let Some(res) = tasks.join_next().await {
        if let Ok((n, abort)) = res {
            found += n;
            if abort {
                // Chave recusada ou sem rede: não adianta continuar este ciclo.
                tasks.abort_all();
            }
        }
    }
    info!("TMDB: {} itens enriquecidos.", found);
}

/// Processa um lote sequencialmente. Devolve (enriquecidos, abortar_ciclo).
async fn process_batch(
    pool: &DbPool,
    config: &AppConfig,
    key: &str,
    rows: Vec<PendingRow>,
) -> (usize, bool) {
    let mut series_cache: HashMap<String, Option<Info>> = HashMap::new();
    let mut found = 0;
    let mut stored_series: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (id, kind, name, year, series, season, episode, series_id) in rows {
        let mut show_info: Option<Info> = None;
        let result = if kind == "Episode" {
            match (series, season, episode) {
                (Some(s), Some(se), Some(ep)) => {
                    match lookup_episode(config, key, &mut series_cache, &s, se, ep).await {
                        Ok(Some((episode_info, show))) => {
                            show_info = Some(show);
                            Ok(Some(episode_info))
                        }
                        Ok(None) => Ok(None),
                        Err(e) => Err(e),
                    }
                }
                _ => Ok(None),
            }
        } else {
            lookup_movie(config, key, &name, year).await
        };

        match result {
            Ok(Some(info)) => {
                if let (Some(show), Some(sid)) = (show_info.as_ref(), series_id.as_ref()) {
                    if stored_series.insert(sid.clone()) {
                        if let Err(e) = store_series(pool, config, sid, show).await {
                            warn!("TMDB: falha ao salvar série {}: {}", sid, e);
                        }
                    }
                }
                if let Err(e) = store(pool, config, &id, &info).await {
                    warn!("TMDB: falha ao salvar {}: {}", name, e);
                } else {
                    found += 1;
                }
            }
            Ok(None) => {
                // Sem correspondência: não repetir por 7 dias.
                let _ = sqlx::query("UPDATE items SET tmdb_checked_at = ? WHERE id = ?")
                    .bind(now_secs())
                    .bind(&id)
                    .execute(pool)
                    .await;
            }
            Err(FetchError::Unauthorized) => {
                warn!("TMDB: chave de API recusada (401/403). Verifique TMDB_API_KEY.");
                return (found, true);
            }
            Err(FetchError::Transport(e)) => {
                warn!("TMDB: sem conexão, tentando no próximo scan ({})", e);
                return (found, true);
            }
            Err(FetchError::Http(s)) => {
                warn!("TMDB: HTTP {} para '{}'", s, name);
            }
        }
    }
    (found, false)
}

/// Poster de cada temporada (`/tv/{id}/season/{n}`). Sem poster próprio, grava `NULL` para não repetir.
async fn enrich_seasons(pool: &DbPool, config: &AppConfig, key: &str) {
    type Row = (String, i32, i64);
    let rows: Vec<Row> = match sqlx::query_as(
        "SELECT DISTINCT i.season_id, i.season_number, s.tmdb_id
         FROM items i
         JOIN series s ON s.id = i.series_id
         LEFT JOIN season_art a ON a.season_id = i.season_id
         WHERE i.media_type = 'Episode' AND i.season_id IS NOT NULL AND i.season_number IS NOT NULL
           AND s.tmdb_id IS NOT NULL AND a.season_id IS NULL",
    )
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            warn!("TMDB: falha ao listar temporadas: {}", e);
            return;
        }
    };
    if rows.is_empty() {
        return;
    }

    let lang_owned = crate::settings::tmdb_language();
    let lang = lang_owned.as_str();
    for (season_id, season, tmdb_id) in rows {
        let path = format!("/tv/{tmdb_id}/season/{season}");
        let poster = match get_json(config, key, &path, &[("language", lang)]).await {
            Ok(v) => non_empty(v.get("poster_path")),
            Err(FetchError::Http(_)) => None,
            // Sem rede ou chave recusada: tenta de novo no próximo scan.
            Err(_) => return,
        };
        let file = match poster.as_deref() {
            Some(p) => download_image(config, p, "w500").await,
            None => None,
        };
        let _ = sqlx::query("INSERT OR REPLACE INTO season_art (season_id, poster) VALUES (?, ?)")
            .bind(&season_id)
            .bind(file)
            .execute(pool)
            .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn lookup_movie(
    config: &AppConfig,
    key: &str,
    title: &str,
    year: Option<i32>,
) -> Result<Option<Info>, FetchError> {
    let lang_owned = crate::settings::tmdb_language();
    let lang = lang_owned.as_str();
    let year_s = year.map(|y| y.to_string());

    let mut id = None;
    if let Some(ref y) = year_s {
        let v = get_json(config, key, "/search/movie", &[("query", title), ("year", y), ("language", lang)]).await?;
        id = first_result_id(&v);
    }
    if id.is_none() {
        let v = get_json(config, key, "/search/movie", &[("query", title), ("language", lang)]).await?;
        id = first_result_id(&v);
    }
    let Some(id) = id else { return Ok(None) };

    let details = get_json(config, key, &format!("/movie/{id}"), &[("language", lang)]).await?;
    Ok(parse_details(&details))
}

async fn lookup_episode(
    config: &AppConfig,
    key: &str,
    cache: &mut HashMap<String, Option<Info>>,
    series: &str,
    season: i32,
    episode: i32,
) -> Result<Option<(Info, Info)>, FetchError> {
    let lang_owned = crate::settings::tmdb_language();
    let lang = lang_owned.as_str();

    if !cache.contains_key(series) {
        let v = get_json(config, key, "/search/tv", &[("query", series), ("language", lang)]).await?;
        let info = match first_result_id(&v) {
            Some(id) => {
                let d = get_json(config, key, &format!("/tv/{id}"), &[("language", lang)]).await?;
                parse_details(&d)
            }
            None => None,
        };
        cache.insert(series.to_string(), info);
    }
    let Some(show) = cache.get(series).cloned().flatten() else {
        return Ok(None);
    };

    // Detalhes do episódio são "melhor esforço": sem eles ficam os dados da série.
    let ep_path = format!("/tv/{}/season/{season}/episode/{episode}", show.id);
    // Sem dados do episódio, mantém o nome parseado do arquivo (nunca o título/sinopse da série).
    let mut info = Info { title: None, overview: None, rating: None, ..show.clone() };
    match get_json(config, key, &ep_path, &[("language", lang)]).await {
        Ok(ep) => {
            if let Some(name) = non_empty(ep.get("name")) {
                info.title = Some(name);
            }
            info.overview = non_empty(ep.get("overview"));
            info.rating = ep
                .get("vote_average")
                .and_then(|r| r.as_f64())
                .filter(|r| *r > 0.0);
            if let Some(still) = non_empty(ep.get("still_path")) {
                info.poster = Some(still);
            }
        }
        Err(FetchError::Http(_)) => {}
        Err(e) => return Err(e),
    }
    Ok(Some((info, show)))
}

async fn store_series(pool: &DbPool, config: &AppConfig, id: &str, show: &Info) -> Result<(), sqlx::Error> {
    let poster = match show.poster.as_deref() {
        Some(p) => download_image(config, p, "w500").await,
        None => None,
    };
    let backdrop = match show.backdrop.as_deref() {
        Some(b) => download_image(config, b, "w1280").await,
        None => None,
    };
    let genres = serde_json::to_string(&show.genres).unwrap_or_else(|_| "[]".to_string());
    sqlx::query(
        "INSERT INTO series (id, tmdb_id, tmdb_title, overview, rating, genres, year, poster, backdrop)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET tmdb_id = excluded.tmdb_id, tmdb_title = excluded.tmdb_title,
            overview = excluded.overview, rating = excluded.rating, genres = excluded.genres,
            year = excluded.year, poster = excluded.poster, backdrop = excluded.backdrop",
    )
    .bind(id)
    .bind(show.id)
    .bind(&show.title)
    .bind(&show.overview)
    .bind(show.rating)
    .bind(genres)
    .bind(show.year)
    .bind(poster)
    .bind(backdrop)
    .execute(pool)
    .await?;
    Ok(())
}

async fn store(pool: &DbPool, config: &AppConfig, id: &str, info: &Info) -> Result<(), sqlx::Error> {
    let poster = match info.poster.as_deref() {
        Some(p) => download_image(config, p, "w500").await,
        None => None,
    };
    let backdrop = match info.backdrop.as_deref() {
        Some(b) => download_image(config, b, "w1280").await,
        None => None,
    };
    let genres = serde_json::to_string(&info.genres).unwrap_or_else(|_| "[]".to_string());

    sqlx::query(
        r#"
        UPDATE items SET
            tmdb_id = ?, tmdb_title = ?, overview = ?, rating = ?, genres = ?,
            production_year = COALESCE(production_year, ?),
            tmdb_poster = ?, tmdb_backdrop = ?, tmdb_checked_at = ?
        WHERE id = ?
        "#,
    )
    .bind(info.id)
    .bind(&info.title)
    .bind(&info.overview)
    .bind(info.rating)
    .bind(genres)
    .bind(info.year)
    .bind(poster)
    .bind(backdrop)
    .bind(now_secs())
    .bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encodes_query() {
        assert_eq!(percent_encode("Amélie & Co"), "Am%C3%A9lie%20%26%20Co");
    }

    #[test]
    fn parses_movie_details() {
        let v = json!({
            "id": 603, "title": "Matrix", "overview": " Neo. ", "vote_average": 8.2,
            "release_date": "1999-03-31", "poster_path": "/abc.jpg", "backdrop_path": null,
            "genres": [{"id": 28, "name": "Ação"}, {"id": 878, "name": "Ficção"}]
        });
        let i = parse_details(&v).unwrap();
        assert_eq!(i.id, 603);
        assert_eq!(i.title.as_deref(), Some("Matrix"));
        assert_eq!(i.overview.as_deref(), Some("Neo."));
        assert_eq!(i.year, Some(1999));
        assert_eq!(i.genres, vec!["Ação", "Ficção"]);
        assert_eq!(i.backdrop, None);
    }

    #[test]
    fn parses_tv_details_and_zero_rating() {
        let v = json!({"id": 1, "name": "Dark", "first_air_date": "2017-12-01", "vote_average": 0.0});
        let i = parse_details(&v).unwrap();
        assert_eq!(i.title.as_deref(), Some("Dark"));
        assert_eq!(i.year, Some(2017));
        assert_eq!(i.rating, None);
    }

    #[test]
    fn first_result() {
        assert_eq!(first_result_id(&json!({"results": [{"id": 5}, {"id": 6}]})), Some(5));
        assert_eq!(first_result_id(&json!({"results": []})), None);
    }

    #[test]
    fn image_path_is_validated() {
        assert_eq!(safe_image_path("/abc-1_2.jpg"), Some("abc-1_2.jpg"));
        assert_eq!(safe_image_path("/../etc/passwd"), None);
        assert_eq!(safe_image_path("abc.jpg"), None);
        assert_eq!(safe_image_path("/a/b.jpg"), None);
    }

    #[test]
    fn token_kind() {
        assert!(!is_bearer_token("0123456789abcdef0123456789abcdef"));
        assert!(is_bearer_token(&"x".repeat(200)));
    }
}
