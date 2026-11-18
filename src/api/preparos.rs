use super::{admin::AdminState, auth::CurrentUser, stream::servir_arquivo};
use crate::{db::DbPool, models::PlaybackProgressReport, transcode, users};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{process::Command, sync::Mutex};

const INTERVALO: Duration = Duration::from_millis(250);
const SESSAO: Duration = Duration::from_secs(90);

#[derive(Clone, Default)]
pub(super) struct Preparos(Arc<Mutex<Estado>>);

#[derive(Default)]
struct Estado {
    consultas: HashMap<String, Consulta>,
    ativos: HashMap<String, Instant>,
    sugestoes: HashMap<String, Sugestao>,
    trabalho: Option<(String, &'static str)>,
}

#[derive(Clone)]
struct Sugestao {
    item: String,
    titulo: String,
    hash: Vec<u8>,
    bitrate: i64,
    sessoes: u32,
}

struct Consulta {
    sugestao: Sugestao,
    usuario: String,
    quando: Instant,
}

impl Estado {
    fn limpar(&mut self) {
        self.ativos.retain(|_, desde| desde.elapsed() < SESSAO);
        self.consultas
            .retain(|_, c| c.quando.elapsed() < Duration::from_secs(600));
    }
}

impl Preparos {
    pub(super) async fn observar(
        &self,
        pool: &DbPool,
        sessao: &str,
        usuario: &str,
        item: &str,
        titulo: &str,
        bitrate: i64,
    ) -> Option<String> {
        let hash = sqlx::query_scalar::<_, Vec<u8>>(
            "SELECT content_sha256 FROM items WHERE id = ? AND content_sha256 IS NOT NULL",
        )
        .bind(item)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()?;
        let mut dig = Sha256::new();
        dig.update(&hash);
        dig.update(item.as_bytes());
        dig.update(bitrate.to_le_bytes());
        let chave = format!("{:x}", dig.finalize());
        let mut estado = self.0.lock().await;
        estado.limpar();
        if estado.consultas.len() < 256 {
            estado.consultas.insert(
                sessao.to_owned(),
                Consulta {
                    sugestao: Sugestao {
                        item: item.to_owned(),
                        titulo: titulo.to_owned(),
                        hash,
                        bitrate,
                        sessoes: 0,
                    },
                    usuario: usuario.to_owned(),
                    quando: Instant::now(),
                },
            );
        }
        Some(chave)
    }

    pub(super) async fn relatar(&self, usuario: &str, p: &PlaybackProgressReport, parou: bool) {
        let chave_ativa = format!(
            "{usuario}/{}",
            p.play_session_id
                .as_deref()
                .or(p.item_id.as_deref())
                .unwrap_or("")
        );
        let mut estado = self.0.lock().await;
        estado.limpar();
        if parou || p.is_paused == Some(true) {
            estado.ativos.remove(&chave_ativa);
            return;
        }
        if estado.ativos.len() < 256 {
            estado.ativos.insert(chave_ativa, Instant::now());
        }
        let Some(sessao) = p.play_session_id.as_deref() else {
            return;
        };
        let autorizada = estado.consultas.get(sessao).is_some_and(|c| {
            c.usuario == usuario
                && p.item_id
                    .as_deref()
                    .is_none_or(|id| super::shows::normalize_id(id) == c.sugestao.item)
        });
        if autorizada {
            if let Some(consulta) = estado.consultas.remove(sessao) {
                let s = consulta.sugestao;
                let mut dig = Sha256::new();
                dig.update(&s.hash);
                dig.update(s.item.as_bytes());
                dig.update(s.bitrate.to_le_bytes());
                let chave = format!("{:x}", dig.finalize());
                if estado.sugestoes.len() < 128 || estado.sugestoes.contains_key(&chave) {
                    let entrada = estado.sugestoes.entry(chave).or_insert(s);
                    entrada.sessoes = entrada.sessoes.saturating_add(1);
                }
            }
        }
    }

    async fn ocupado(&self) -> bool {
        let mut estado = self.0.lock().await;
        estado.limpar();
        !estado.ativos.is_empty()
    }
}

fn limite() -> u64 {
    std::env::var("RUSTYFIN_PREPARO_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2 * 1024 * 1024 * 1024)
}

fn caminho(config: &crate::config::AppConfig, chave: &str) -> PathBuf {
    config
        .cache_dir
        .join("preparos")
        .join(format!("{chave}.mp4"))
}

pub(super) async fn pronta(
    pool: &DbPool,
    config: &crate::config::AppConfig,
    item: &str,
    chave: &str,
) -> bool {
    let existe = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM versoes_preparadas v JOIN items i
           ON i.id = v.item_id AND i.content_sha256 = v.hash WHERE v.chave = ? AND i.id = ?)",
    )
    .bind(chave)
    .bind(item)
    .fetch_one(pool)
    .await
    .unwrap_or(false);
    existe
        && tokio::fs::metadata(caminho(config, chave))
            .await
            .is_ok_and(|m| m.is_file())
}

pub(super) async fn listar(
    State(st): State<AdminState>,
    Extension(preparos): Extension<Preparos>,
) -> Json<Value> {
    let (sugestoes, trabalho) = {
        let estado = preparos.0.lock().await;
        (estado.sugestoes.clone(), estado.trabalho.clone())
    };
    let mut dados = Vec::new();
    for (chave, s) in sugestoes {
        if s.sessoes >= 2 {
            dados.push(
                json!({"chave": chave, "titulo": s.titulo, "sessoes": s.sessoes,
                "pronta": pronta(&st.pool, &st.config, &s.item, &chave).await }),
            );
        }
    }
    dados.sort_by_key(|v| std::cmp::Reverse(v["sessoes"].as_u64()));
    Json(json!({ "sugestoes": dados, "limiteBytes": limite(), "trabalho": trabalho }))
}

pub(super) async fn iniciar(
    State(st): State<AdminState>,
    Extension(preparos): Extension<Preparos>,
    Path(chave): Path<String>,
) -> Result<StatusCode, StatusCode> {
    let sugestao = {
        let mut estado = preparos.0.lock().await;
        if estado
            .trabalho
            .as_ref()
            .is_some_and(|(_, s)| matches!(*s, "aguardando" | "preparando"))
        {
            return Err(StatusCode::CONFLICT);
        }
        let s = estado
            .sugestoes
            .get(&chave)
            .filter(|s| s.sessoes >= 2)
            .cloned()
            .ok_or(StatusCode::NOT_FOUND)?;
        estado.trabalho = Some((chave.clone(), "aguardando"));
        s
    };
    tokio::spawn(async move {
        let ok = tokio::time::timeout(
            Duration::from_secs(10800),
            preparar(&st, &preparos, &chave, &sugestao),
        )
        .await
        .unwrap_or(false);
        let _ = tokio::fs::remove_file(caminho(&st.config, &chave).with_extension("tmp")).await;
        preparos.0.lock().await.trabalho = Some((chave, if ok { "pronta" } else { "falhou" }));
    });
    Ok(StatusCode::ACCEPTED)
}

async fn disponivel(dir: &std::path::Path) -> Option<u64> {
    let mut arquivos = tokio::fs::read_dir(dir).await.ok()?;
    let mut usados = 0_u64;
    while let Some(entrada) = arquivos.next_entry().await.ok()? {
        usados = usados.saturating_add(entrada.metadata().await.ok()?.len());
    }
    Some(limite().saturating_sub(usados))
}

async fn executar(
    preparos: &Preparos,
    input: &str,
    tmp: &std::path::Path,
    bitrate: i64,
    restante: u64,
) -> Option<bool> {
    let mut filho = Command::new(transcode::ffmpeg_path())
        .args(["-nostdin", "-v", "error", "-y", "-i"])
        .arg(input)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0?",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-vf",
            "format=yuv420p",
            "-b:v",
            &bitrate.to_string(),
            "-c:a",
            "aac",
            "-ac",
            "2",
            "-b:a",
            "128k",
            "-fs",
            &restante.to_string(),
            "-movflags",
            "+faststart",
            "-f",
            "mp4",
        ])
        .arg(tmp)
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    loop {
        if preparos.ocupado().await {
            filho.kill().await.ok()?;
            return Some(false);
        }
        if let Some(status) = filho.try_wait().ok()? {
            return status.success().then_some(true);
        }
        tokio::time::sleep(INTERVALO).await;
    }
}

async fn preparar(st: &AdminState, preparos: &Preparos, chave: &str, s: &Sugestao) -> bool {
    if pronta(&st.pool, &st.config, &s.item, chave).await {
        return true;
    }
    let destino = caminho(&st.config, chave);
    let tmp = destino.with_extension("tmp");
    let Some(dir) = destino.parent() else {
        return false;
    };
    if tokio::fs::create_dir_all(dir).await.is_err() {
        return false;
    }
    let Ok(Some((input, ticks, mtime))) = sqlx::query_as::<_, (String, i64, String)>(
        "SELECT path, duration_ticks, content_mtime FROM items
         WHERE id = ? AND content_sha256 = ? AND duration_ticks > 0 AND content_mtime IS NOT NULL",
    )
    .bind(&s.item)
    .bind(&s.hash)
    .fetch_optional(&st.pool)
    .await
    else {
        return false;
    };
    let antes = tokio::fs::metadata(&input)
        .await
        .ok()
        .and_then(|m| m.modified().ok());
    if antes
        .and_then(|data| data.duration_since(std::time::UNIX_EPOCH).ok())
        .is_none_or(|data| data.as_nanos().to_string() != mtime)
    {
        return false;
    }
    loop {
        while preparos.ocupado().await {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        let Some(restante) = disponivel(dir).await.filter(|b| *b > 1024 * 1024) else {
            return false;
        };
        let estimado = i128::from(ticks) * i128::from(s.bitrate + 128_000) / 80_000_000;
        if estimado * 110 / 100 > i128::from(restante) {
            return false;
        }
        preparos.0.lock().await.trabalho = Some((chave.to_owned(), "preparando"));
        match executar(preparos, &input, &tmp, s.bitrate, restante - 1024 * 1024).await {
            Some(false) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                preparos.0.lock().await.trabalho = Some((chave.to_owned(), "aguardando"));
            }
            Some(true) => break,
            None => return false,
        }
    }
    let Ok(tamanho) = tokio::fs::metadata(&tmp).await.map(|m| m.len()) else {
        return false;
    };
    if antes.is_none()
        || antes
            != tokio::fs::metadata(&input)
                .await
                .ok()
                .and_then(|m| m.modified().ok())
        || tamanho == 0
        || disponivel(dir).await == Some(0)
    {
        return false;
    }
    let medicao = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(&tmp)
        .kill_on_drop(true)
        .output();
    let Ok(Ok(dados)) = tokio::time::timeout(Duration::from_secs(15), medicao).await else {
        return false;
    };
    let segundos = String::from_utf8_lossy(&dados.stdout)
        .trim()
        .parse::<f64>()
        .ok();
    if !dados.status.success()
        || segundos
            .is_none_or(|s| !s.is_finite() || (s * 10_000_000.0 - ticks as f64).abs() > 5_000_000.0)
    {
        return false;
    }
    if tokio::fs::rename(&tmp, &destino).await.is_err() {
        return false;
    }
    let gravado = sqlx::query(
        "INSERT OR REPLACE INTO versoes_preparadas
         SELECT ?, id, content_sha256, ? FROM items WHERE id = ? AND path = ? AND content_sha256 = ?",
    ).bind(chave).bind(i64::try_from(tamanho).unwrap_or(i64::MAX))
        .bind(&s.item).bind(input).bind(&s.hash).execute(&st.pool).await;
    if gravado.is_ok_and(|r| r.rows_affected() == 1) {
        return true;
    }
    let _ = tokio::fs::remove_file(&destino).await;
    false
}

pub(super) async fn servir(
    State((config, pool)): State<(Arc<crate::config::AppConfig>, DbPool)>,
    CurrentUser(user): CurrentUser,
    Path((id, chave)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, StatusCode> {
    let id = super::shows::normalize_id(&id);
    if chave.len() != 64
        || !chave.bytes().all(|b| b.is_ascii_hexdigit())
        || !users::item_allowed(&pool, &user, &id)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        || !pronta(&pool, &config, &id, &chave).await
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let path = caminho(&config, &chave);
    servir_arquivo(
        path.to_str().ok_or(StatusCode::NOT_FOUND)?,
        Some("mp4"),
        headers,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn pool_com_item(hash: Option<&[u8]>) -> DbPool {
        let pool = crate::db::init_db("sqlite::memory:").await.unwrap();
        sqlx::query("INSERT INTO items (id, name, path, media_type, content_sha256) VALUES ('filme', 'Filme', '/m/f.mp4', 'Movie', ?)")
            .bind(hash)
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    fn relato(sessao: &str, pausado: bool) -> PlaybackProgressReport {
        PlaybackProgressReport {
            item_id: Some("filme".into()),
            position_ticks: Some(0),
            is_paused: Some(pausado),
            play_session_id: Some(sessao.into()),
        }
    }

    #[tokio::test]
    async fn so_observa_titulos_com_hash_e_a_chave_e_estavel() {
        let sem_hash = pool_com_item(None).await;
        let preparos = Preparos::default();
        assert!(preparos.observar(&sem_hash, "s1", "ana", "filme", "Filme", 2_000_000).await.is_none());

        let pool = pool_com_item(Some(&[1, 2, 3])).await;
        let a = preparos.observar(&pool, "s1", "ana", "filme", "Filme", 2_000_000).await.unwrap();
        let b = preparos.observar(&pool, "s2", "bia", "filme", "Filme", 2_000_000).await.unwrap();
        let outro_bitrate = preparos.observar(&pool, "s3", "ana", "filme", "Filme", 4_000_000).await.unwrap();
        assert_eq!(a, b, "mesma mídia e mesma configuração: mesma versão preparada");
        assert_ne!(a, outro_bitrate);
    }

    #[tokio::test]
    async fn conta_uma_sessao_so_para_quem_pediu_e_so_se_reportar_reproducao() {
        let pool = pool_com_item(Some(&[9])).await;
        let preparos = Preparos::default();
        let chave = preparos.observar(&pool, "s1", "ana", "filme", "Filme", 2_000_000).await.unwrap();
        // Outro usuário reportando a mesma sessão não conta.
        preparos.relatar("bia", &relato("s1", false), false).await;
        assert!(preparos.0.lock().await.sugestoes.is_empty());
        // A sessão da dona conta uma vez, e o relato seguinte da mesma sessão não conta de novo.
        preparos.relatar("ana", &relato("s1", false), false).await;
        preparos.relatar("ana", &relato("s1", false), false).await;
        assert_eq!(preparos.0.lock().await.sugestoes[&chave].sessoes, 1);
        // Uma segunda sessão com a mesma configuração chega às duas necessárias para recomendar.
        preparos.observar(&pool, "s2", "ana", "filme", "Filme", 2_000_000).await.unwrap();
        preparos.relatar("ana", &relato("s2", false), false).await;
        assert_eq!(preparos.0.lock().await.sugestoes[&chave].sessoes, 2);
    }

    #[tokio::test]
    async fn sessao_pausada_ou_parada_nao_deixa_o_servidor_ocupado() {
        let preparos = Preparos::default();
        assert!(!preparos.ocupado().await);
        preparos.relatar("ana", &relato("s1", false), false).await;
        assert!(preparos.ocupado().await, "uma reprodução em andamento adia o preparo");
        preparos.relatar("ana", &relato("s1", true), false).await;
        assert!(!preparos.ocupado().await, "pausar libera o servidor");
        preparos.relatar("ana", &relato("s1", false), false).await;
        preparos.relatar("ana", &relato("s1", false), true).await;
        assert!(!preparos.ocupado().await, "parar libera o servidor");
    }

    #[tokio::test]
    async fn sessao_sem_relato_recente_expira() {
        let preparos = Preparos::default();
        {
            let mut estado = preparos.0.lock().await;
            let antiga = Instant::now().checked_sub(SESSAO + Duration::from_secs(5)).unwrap();
            estado.ativos.insert("ana/s1".into(), antiga);
        }
        assert!(!preparos.ocupado().await, "um relato mais velho que a janela não segura o preparo");
    }

    #[test]
    fn o_cache_respeita_o_limite_configurado() {
        // sem a variável, o padrão é de 2 GiB
        if std::env::var("RUSTYFIN_PREPARO_MAX_BYTES").is_err() {
            assert_eq!(limite(), 2 * 1024 * 1024 * 1024);
        }
    }
}
