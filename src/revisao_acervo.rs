use crate::db::DbPool;
use serde::Serialize;
use sqlx::{Sqlite, Transaction};

pub(crate) struct Sugestao {
    pub(crate) id: String,
    pub(crate) caminho: String,
    pub(crate) motivo: &'static str,
    pub(crate) destinos: Vec<(String, Vec<u8>)>,
}

pub(crate) async fn arquivar(
    tx: &mut Transaction<'_, Sqlite>,
    sugestao: &Sugestao,
) -> Result<(), sqlx::Error> {
    let inserido = sqlx::query(
        "INSERT OR IGNORE INTO media_reviews (id, title, path, duration_ticks, reason)
         SELECT id, name, path, duration_ticks, ? FROM items WHERE id = ? AND path = ?",
    )
    .bind(sugestao.motivo)
    .bind(&sugestao.id)
    .bind(&sugestao.caminho)
    .execute(&mut **tx)
    .await?
    .rows_affected();
    if inserido == 0 {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO media_review_state (review_id, user_id, is_played, is_favorite)
         SELECT p.item_id, p.user_id, p.is_played, p.is_favorite FROM playback_state p
         JOIN users u ON u.id = p.user_id
         WHERE p.item_id = ? AND (p.is_played OR p.is_favorite)",
    )
    .bind(&sugestao.id)
    .execute(&mut **tx)
    .await?;
    for (caminho, hash) in &sugestao.destinos {
        sqlx::query(
            "INSERT INTO media_review_targets (review_id, path, content_sha256) VALUES (?, ?, ?)",
        )
        .bind(&sugestao.id)
        .bind(caminho)
        .bind(hash)
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query("DELETE FROM playback_state WHERE item_id = ?")
        .bind(&sugestao.id)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM items WHERE id = ? AND path = ?")
        .bind(&sugestao.id)
        .bind(&sugestao.caminho)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[derive(sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Destino {
    caminho: String,
    item_id: Option<String>,
    titulo: Option<String>,
    duracao_ticks: Option<i64>,
}

#[derive(sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Revisao {
    id: String,
    titulo: String,
    caminho: String,
    duracao_ticks: Option<i64>,
    motivo: String,
    perfis: i64,
    #[sqlx(skip)]
    destinos: Vec<Destino>,
}

pub(crate) async fn listar(pool: &DbPool) -> Result<Vec<Revisao>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut revisoes: Vec<Revisao> = sqlx::query_as(
        "SELECT r.id, r.title AS titulo, r.path AS caminho, r.duration_ticks AS duracao_ticks,
                r.reason AS motivo,
                (SELECT COUNT(*) FROM media_review_state s WHERE s.review_id = r.id) AS perfis
         FROM media_reviews r ORDER BY r.rowid LIMIT 30",
    )
    .fetch_all(&mut *tx)
    .await?;
    for revisao in &mut revisoes {
        revisao.destinos = sqlx::query_as(
            "SELECT t.path AS caminho, i.id AS item_id, i.name AS titulo,
                    i.duration_ticks AS duracao_ticks
             FROM media_review_targets t LEFT JOIN items i
               ON i.path = t.path AND i.content_sha256 = t.content_sha256
             WHERE t.review_id = ? ORDER BY t.path",
        )
        .bind(&revisao.id)
        .fetch_all(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(revisoes)
}

pub(crate) enum Resolucao {
    Aplicada,
    RevisaoAusente,
    DestinoIndisponivel,
}

pub(crate) async fn resolver(
    pool: &DbPool,
    id: &str,
    destino: Option<&str>,
) -> Result<Resolucao, sqlx::Error> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let existe: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM media_reviews WHERE id = ?)")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
    if !existe {
        return Ok(Resolucao::RevisaoAusente);
    }
    if let Some(destino) = destino {
        let valido: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM media_review_targets t JOIN items i
              ON i.path = t.path AND i.content_sha256 = t.content_sha256
              WHERE t.review_id = ? AND i.id = ?)",
        )
        .bind(id)
        .bind(destino)
        .fetch_one(&mut *tx)
        .await?;
        if !valido {
            return Ok(Resolucao::DestinoIndisponivel);
        }
        sqlx::query(
            "INSERT INTO playback_state (user_id, item_id, is_played, is_favorite)
             SELECT user_id, ?, is_played, is_favorite FROM media_review_state WHERE review_id = ?
             ON CONFLICT(user_id, item_id) DO UPDATE SET
             is_played = (playback_state.is_played OR excluded.is_played),
             is_favorite = (playback_state.is_favorite OR excluded.is_favorite)",
        )
        .bind(destino)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    sqlx::query("DELETE FROM media_reviews WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Resolucao::Aplicada)
}
