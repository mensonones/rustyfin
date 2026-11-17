use crate::db::DbPool;
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, Transaction};
use std::collections::HashSet;

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
        "INSERT OR IGNORE INTO media_reviews (id, title, path, duration_ticks, reason, audio_hash)
         SELECT id, name, path, duration_ticks, ?, content_sha256 FROM items WHERE id = ? AND path = ?",
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
        "INSERT INTO media_review_state (review_id, user_id, is_played, is_favorite, position_ticks)
         SELECT p.item_id, p.user_id, p.is_played, p.is_favorite, p.position_ticks FROM playback_state p
         JOIN users u ON u.id = p.user_id
         WHERE p.item_id = ? AND (p.is_played OR p.is_favorite OR p.position_ticks > 0)",
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
    retomadas: i64,
    #[sqlx(skip)]
    destinos: Vec<Destino>,
}

pub(crate) async fn listar(pool: &DbPool) -> Result<Vec<Revisao>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut revisoes: Vec<Revisao> = sqlx::query_as(
        "SELECT r.id, r.title AS titulo, r.path AS caminho, r.duration_ticks AS duracao_ticks,
                r.reason AS motivo,
                (SELECT COUNT(*) FROM media_review_state s WHERE s.review_id = r.id
                 AND (s.is_played OR s.is_favorite)) AS perfis,
                (SELECT COUNT(*) FROM media_review_state s WHERE s.review_id = r.id
                 AND s.position_ticks > 0) AS retomadas
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
    RetomadaInvalida,
    PosicaoAlterada,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MapaRetomada {
    anterior_a: i64,
    novo_a: i64,
    anterior_b: i64,
    novo_b: i64,
}

impl MapaRetomada {
    fn valido(&self, anterior: i64, novo: i64) -> bool {
        0 <= self.anterior_a
            && self.anterior_a < self.anterior_b
            && self.anterior_b <= anterior
            && 0 <= self.novo_a
            && self.novo_a < self.novo_b
            && self.novo_b <= novo
    }

    fn sugerir(&self, posicao: i64, duracao: i64) -> Option<i64> {
        if !(self.anterior_a..=self.anterior_b).contains(&posicao) {
            return None;
        }
        crate::linha_do_tempo::interpolar(
            posicao,
            self.anterior_a,
            self.novo_a,
            self.novo_b - self.novo_a,
            self.anterior_b - self.anterior_a,
        )
        .filter(|posicao| *posicao < duracao)
    }
}

#[derive(sqlx::FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Posicao {
    perfil_id: String,
    perfil: String,
    anterior: i64,
    atual: i64,
    #[sqlx(skip)]
    sugerida: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Previa {
    duracao_destino: i64,
    posicoes: Vec<Posicao>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Retomada {
    perfil_id: String,
    posicao_ticks: i64,
    posicao_esperada_ticks: i64,
}

async fn duracoes(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    destino: &str,
) -> Result<Option<(i64, i64)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT COALESCE(r.duration_ticks, 0), COALESCE(i.duration_ticks, 0)
         FROM media_review_targets t JOIN items i
           ON i.path = t.path AND i.content_sha256 = t.content_sha256
         JOIN media_reviews r ON r.id = t.review_id WHERE r.id = ? AND i.id = ?",
    )
    .bind(id)
    .bind(destino)
    .fetch_optional(&mut **tx)
    .await
}

pub(crate) async fn previa(
    pool: &DbPool,
    id: &str,
    destino: &str,
    mapa: Option<&MapaRetomada>,
    automatico: bool,
) -> Result<Result<Previa, Resolucao>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let Some((anterior, nova)) = duracoes(&mut tx, id, destino).await? else {
        return Ok(Err(Resolucao::DestinoIndisponivel));
    };
    if mapa.is_some_and(|m| !m.valido(anterior, nova)) {
        return Ok(Err(Resolucao::RetomadaInvalida));
    }
    let mut posicoes: Vec<Posicao> = sqlx::query_as(
        "SELECT s.user_id AS perfil_id, u.name AS perfil, s.position_ticks AS anterior,
                COALESCE(p.position_ticks, 0) AS atual
         FROM media_review_state s JOIN users u ON u.id = s.user_id
         LEFT JOIN playback_state p ON p.user_id = s.user_id AND p.item_id = ?
         WHERE s.review_id = ? AND s.position_ticks > 0 ORDER BY u.name, u.id",
    )
    .bind(destino)
    .bind(id)
    .fetch_all(&mut *tx)
    .await?;
    for p in &mut posicoes {
        p.sugerida = mapa.and_then(|m| m.sugerir(p.anterior, nova));
    }
    tx.commit().await?;
    if automatico && mapa.is_none() && nova > 0 {
        let _ = crate::audio_edicoes::indexar(pool, destino).await?;
        if let Some((antiga, atual)) = crate::audio_edicoes::carregar(pool, id, destino).await? {
            posicoes = tokio::task::spawn_blocking(move || {
                for p in posicoes.iter_mut().take(24) {
                    p.sugerida = crate::audio_edicoes::alinhar(&antiga, &atual, p.anterior)
                        .filter(|ticks| *ticks < nova);
                }
                posicoes
            }).await.map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        }
    }
    Ok(Ok(Previa {
        duracao_destino: nova,
        posicoes,
    }))
}

async fn aplicar_retomadas(
    tx: &mut Transaction<'_, Sqlite>,
    id: &str,
    destino: &str,
    duracao: i64,
    retomadas: &[Retomada],
) -> Result<Result<(), Resolucao>, sqlx::Error> {
    let mut vistos = HashSet::new();
    for r in retomadas {
        if !(0..duracao).contains(&r.posicao_ticks) || !vistos.insert(&r.perfil_id) {
            return Ok(Err(Resolucao::RetomadaInvalida));
        }
        let atual: Option<i64> = sqlx::query_scalar(
            "SELECT COALESCE(p.position_ticks, 0)
             FROM media_review_state s JOIN users u ON u.id = s.user_id
             LEFT JOIN playback_state p ON p.user_id = s.user_id AND p.item_id = ?
             WHERE s.review_id = ? AND s.user_id = ? AND s.position_ticks > 0",
        )
        .bind(destino)
        .bind(id)
        .bind(&r.perfil_id)
        .fetch_optional(&mut **tx)
        .await?;
        if atual != Some(r.posicao_esperada_ticks) {
            return Ok(Err(Resolucao::PosicaoAlterada));
        }
        sqlx::query(
            "INSERT INTO playback_state (user_id, item_id, position_ticks) VALUES (?, ?, ?)
             ON CONFLICT(user_id, item_id) DO UPDATE SET
             position_ticks = excluded.position_ticks, last_updated = CURRENT_TIMESTAMP",
        )
        .bind(&r.perfil_id)
        .bind(destino)
        .bind(r.posicao_ticks)
        .execute(&mut **tx)
        .await?;
    }
    Ok(Ok(()))
}

pub(crate) async fn resolver(
    pool: &DbPool,
    id: &str,
    destino: Option<&str>,
    retomadas: &[Retomada],
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
        let Some((_, duracao)) = duracoes(&mut tx, id, destino).await? else {
            return Ok(Resolucao::DestinoIndisponivel);
        };
        if let Err(erro) = aplicar_retomadas(&mut tx, id, destino, duracao, retomadas).await? {
            return Ok(erro);
        }
        sqlx::query(
            "INSERT INTO playback_state (user_id, item_id, is_played, is_favorite)
             SELECT user_id, ?, is_played, is_favorite FROM media_review_state WHERE review_id = ?
             AND (is_played OR is_favorite)
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
