use super::{
    admin::AdminState,
    auth::CurrentUser,
    shows::normalize_id,
    subtitles::{FonteLegenda, ajuste_salvo, srt_aceita},
};
use crate::db::DbPool;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{Value, json};

async fn alterada(path: Option<&str>) -> Option<i64> {
    let Some(path) = path else {
        return Some(0);
    };
    let mtime = tokio::fs::metadata(path).await.ok()?.modified().ok()?;
    i64::try_from(mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos()).ok()
}

type RevisaoAjustes = Vec<(i32, String, String)>;

async fn revisao_ajustes(
    conn: &mut sqlx::SqliteConnection,
    id: &str,
) -> Result<RevisaoAjustes, sqlx::Error> {
    sqlx::query_as(
        "SELECT track_index, source_hash, adjustment FROM subtitle_adjustments
                    WHERE item_id = ? ORDER BY track_index",
    )
    .bind(id)
    .fetch_all(conn)
    .await
}

pub(super) async fn indexar(
    State(st): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let fonte = FonteLegenda::buscar(&st.pool, &id).await?;
    let (path, hash, externa, mtime): (String, Option<Vec<u8>>, Option<String>, Option<String>) =
        sqlx::query_as(
            "SELECT path, content_sha256, subtitle_path, content_mtime FROM items WHERE id = ?",
        )
        .bind(&fonte.id)
        .fetch_one(&st.pool)
        .await
        .map_err(erro_db)?;
    let hash = hash.ok_or(StatusCode::CONFLICT)?;
    let revisao_externa = alterada(externa.as_deref())
        .await
        .ok_or(StatusCode::CONFLICT)?;
    let revisao_video = alterada(Some(&path)).await.map(|valor| valor.to_string());
    if revisao_video.is_none() || revisao_video != mtime {
        return Err(StatusCode::CONFLICT);
    }
    let ajustes = {
        let mut conn = st.pool.acquire().await.map_err(erro_db)?;
        revisao_ajustes(&mut conn, &fonte.id)
            .await
            .map_err(erro_db)?
    };
    let mut linhas = Vec::new();
    for faixa in fonte.faixas().iter().filter(|f| srt_aceita(f)).take(16) {
        let original = fonte.original(&st.config, faixa).await?;
        let ajuste = ajuste_salvo(&st.pool, &fonte.id, faixa.index)
            .await?
            .filter(|(hash, _)| *hash == original.assinatura)
            .and_then(|(_, a)| a.mapa());
        for trecho in original.trechos().await? {
            let tempos = ajuste.map_or(Some((trecho.inicio_ms, trecho.fim_ms)), |m| {
                m.trecho(&trecho)
            });
            if let Some((inicio, fim)) = tempos {
                linhas.push((faixa.index, inicio * 10000, fim * 10000, trecho.texto));
            }
        }
    }
    if alterada(externa.as_deref()).await != Some(revisao_externa) {
        return Err(StatusCode::CONFLICT);
    }
    let mut tx = st
        .pool
        .begin_with("BEGIN IMMEDIATE")
        .await
        .map_err(erro_db)?;
    let atual: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM items WHERE id = ? AND path = ? AND content_sha256 = ?)",
    )
    .bind(&fonte.id)
    .bind(&path)
    .bind(&hash)
    .fetch_one(&mut *tx)
    .await
    .map_err(erro_db)?;
    if !atual || revisao_ajustes(&mut tx, &fonte.id).await.map_err(erro_db)? != ajustes {
        return Err(StatusCode::CONFLICT);
    }
    sqlx::query("DELETE FROM cenas_legenda WHERE item_id = ?")
        .bind(&fonte.id)
        .execute(&mut *tx)
        .await
        .map_err(erro_db)?;
    let quantidade = linhas.len();
    for (faixa, inicio, fim, texto) in linhas {
        sqlx::query("INSERT INTO cenas_legenda VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&fonte.id)
            .bind(faixa)
            .bind(inicio)
            .bind(fim)
            .bind(&texto)
            .bind(texto.to_lowercase())
            .execute(&mut *tx)
            .await
            .map_err(erro_db)?;
    }
    sqlx::query(
        "INSERT OR REPLACE INTO cenas_fontes (item_id, hash, caminho, alterada) VALUES (?, ?, ?, ?)",
    ).bind(&fonte.id).bind(hash).bind(externa.unwrap_or_default()).bind(revisao_externa)
        .execute(&mut *tx).await.map_err(erro_db)?;
    tx.commit().await.map_err(erro_db)?;
    Ok(Json(json!({ "trechos": quantidade })))
}

fn erro_db(e: sqlx::Error) -> StatusCode {
    tracing::error!("Falha no índice de cenas: {e}");
    StatusCode::INTERNAL_SERVER_ERROR
}

#[derive(Deserialize)]
pub(super) struct Consulta {
    q: String,
    #[serde(default)]
    spoilers: bool,
}

#[derive(sqlx::FromRow)]
struct Cena {
    id: String,
    name: String,
    faixa: i32,
    inicio: i64,
    fim: i64,
    texto: String,
    subtitle_path: Option<String>,
    alterada: i64,
}

pub(super) async fn buscar(
    State(pool): State<DbPool>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<Consulta>,
) -> Result<Json<Vec<Value>>, StatusCode> {
    let termo = q.q.trim();
    if !(2..=128).contains(&termo.chars().count()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let mut sql = String::from(
        "SELECT i.id, i.name, c.faixa, c.inicio, c.fim, c.texto, i.subtitle_path, f.alterada
         FROM cenas_legenda c JOIN items i ON i.id = c.item_id
         JOIN cenas_fontes f ON f.item_id = i.id AND f.hash = i.content_sha256
           AND f.caminho = COALESCE(i.subtitle_path, '')
         LEFT JOIN playback_state p ON p.item_id = i.id AND p.user_id = ?
         WHERE instr(c.texto_busca, ?) > 0
           AND (? OR COALESCE(p.is_played, 0) OR c.fim <= COALESCE(p.position_ticks, 0))",
    );
    let filtro = user.library_clause("i.library_id");
    if let Some((condicao, _)) = &filtro {
        sql.push_str(" AND ");
        sql.push_str(condicao);
    }
    sql.push_str(" ORDER BY i.name, c.inicio LIMIT 80");
    let mut consulta = sqlx::query_as::<_, Cena>(&sql)
        .bind(&user.id)
        .bind(termo.to_lowercase())
        .bind(q.spoilers);
    if let Some((_, valores)) = filtro {
        for valor in valores {
            consulta = consulta.bind(valor);
        }
    }
    let linhas = consulta.fetch_all(&pool).await.map_err(erro_db)?;
    let mut resposta = Vec::new();
    for c in linhas {
        if alterada(c.subtitle_path.as_deref()).await == Some(c.alterada) {
            resposta.push(json!({
                "itemId": normalize_id(&c.id), "titulo": c.name, "faixa": c.faixa,
                "inicioTicks": c.inicio, "fimTicks": c.fim, "previa": c.texto,
            }));
        }
    }
    Ok(Json(resposta))
}

pub(super) async fn pagina() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../cenas.html"))
}
