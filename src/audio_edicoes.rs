use crate::{db::DbPool, transcode};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command, sync::Semaphore};

const PASSO_TICKS: i64 = 1_000_000; // 100 ms
const AMOSTRAS: usize = 400;
const JANELA: usize = 64;
const LIMITE_QUADROS: usize = 108_000;
static VAGA: Semaphore = Semaphore::const_new(1);

async fn extrair(path: &str) -> Option<Vec<u8>> {
    let mut filho = Command::new(transcode::ffmpeg_path())
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(Path::new(path))
        .args([
            "-map", "0:a:0", "-vn", "-ac", "1", "-ar", "4000", "-t", "10800", "-f", "s16le",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut saida = filho.stdout.take()?;
    let mut quadros = Vec::new();
    let mut bloco = [0_u8; AMOSTRAS * 2];
    while quadros.len() / 2 < LIMITE_QUADROS && saida.read_exact(&mut bloco).await.is_ok() {
        let mut energia = 0_u32;
        let mut cruzamentos = 0_u16;
        let mut anterior = 0_i16;
        for bytes in bloco.chunks_exact(2) {
            let valor = i16::from_le_bytes([bytes[0], bytes[1]]);
            energia += u32::from(valor.unsigned_abs());
            cruzamentos += u16::from((valor < 0) != (anterior < 0));
            anterior = valor;
        }
        let media = energia / 400;
        quadros.push(u8::try_from(media.checked_ilog2().unwrap_or(0) * 12).unwrap_or(u8::MAX));
        quadros.push(u8::try_from(cruzamentos / 2).unwrap_or(u8::MAX));
    }
    drop(saida);
    (filho.wait().await.ok()?.success() && quadros.len() >= JANELA * 2).then_some(quadros)
}

pub(crate) async fn indexar(pool: &DbPool, item: &str) -> Result<bool, sqlx::Error> {
    let Some((path, hash, mtime)) = sqlx::query_as::<_, (String, Vec<u8>, String)>(
        "SELECT path, content_sha256, content_mtime FROM items
         WHERE id = ? AND content_sha256 IS NOT NULL AND content_mtime IS NOT NULL",
    )
    .bind(item)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(false);
    };
    let existe: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM audio_assinaturas WHERE hash = ?)")
            .bind(&hash)
            .fetch_one(pool)
            .await?;
    if existe {
        return Ok(true);
    }
    let Ok(_vaga) = VAGA.try_acquire() else {
        return Ok(false);
    };
    let antes = tokio::fs::metadata(&path)
        .await
        .ok()
        .and_then(|m| m.modified().ok());
    if antes
        .and_then(|data| data.duration_since(std::time::UNIX_EPOCH).ok())
        .is_none_or(|data| data.as_nanos().to_string() != mtime)
    {
        return Ok(false);
    }
    let Some(quadros) = tokio::time::timeout(Duration::from_secs(120), extrair(&path))
        .await
        .ok()
        .flatten()
    else {
        return Ok(false);
    };
    if antes.is_none()
        || antes
            != tokio::fs::metadata(&path)
                .await
                .ok()
                .and_then(|m| m.modified().ok())
    {
        return Ok(false);
    }
    sqlx::query(
        "INSERT OR IGNORE INTO audio_assinaturas SELECT ?, ? FROM items
         WHERE id = ? AND path = ? AND content_sha256 = ?",
    )
    .bind(&hash)
    .bind(quadros)
    .bind(item)
    .bind(path)
    .bind(&hash)
    .execute(pool)
    .await?;
    Ok(true)
}

type Assinaturas = (Vec<u8>, Vec<u8>);

pub(crate) async fn carregar(
    pool: &DbPool,
    revisao: &str,
    destino: &str,
) -> Result<Option<Assinaturas>, sqlx::Error> {
    sqlx::query_as(
        "SELECT antiga.quadros, nova.quadros FROM media_reviews r
         JOIN audio_assinaturas antiga ON antiga.hash = r.audio_hash
         JOIN media_review_targets t ON t.review_id = r.id
         JOIN items i ON i.path = t.path AND i.content_sha256 = t.content_sha256
         JOIN audio_assinaturas nova ON nova.hash = i.content_sha256
         WHERE r.id = ? AND i.id = ?",
    )
    .bind(revisao)
    .bind(destino)
    .fetch_optional(pool)
    .await
}

pub(crate) fn alinhar(antiga: &[u8], nova: &[u8], posicao: i64) -> Option<i64> {
    let centro = usize::try_from(posicao / PASSO_TICKS).ok()?;
    let inicio = centro.checked_sub(JANELA / 2)?;
    let amostra = antiga.get(inicio * 2..(inicio + JANELA) * 2)?;
    let variacao = amostra
        .chunks_exact(2)
        .map(|v| v[1])
        .max()?
        .abs_diff(amostra.chunks_exact(2).map(|v| v[1]).min()?);
    if variacao < 8 {
        return None;
    }
    let custo = |trecho: &[u8]| -> u32 {
        amostra
            .iter()
            .zip(trecho)
            .map(|(a, b)| u32::from(a.abs_diff(*b)))
            .sum()
    };
    let pontuacoes: Vec<_> = (0..=nova.len().checked_sub(JANELA * 2)?)
        .step_by(2)
        .map(|i| custo(&nova[i..i + JANELA * 2]))
        .collect();
    let (melhor, custo_min) = pontuacoes
        .iter()
        .enumerate()
        .min_by_key(|(_, erro)| *erro)?;
    let segundo = pontuacoes
        .iter()
        .enumerate()
        .filter(|(i, _)| i.abs_diff(melhor) > 10)
        .map(|(_, v)| *v)
        .min()?;
    if *custo_min > 360 || segundo.saturating_sub(*custo_min) < 220 {
        return None;
    }
    Some(i64::try_from(melhor + JANELA / 2).ok()? * PASSO_TICKS + posicao % PASSO_TICKS)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn banco() -> DbPool {
        crate::db::init_db("sqlite::memory:").await.unwrap()
    }

    #[tokio::test]
    async fn indexar_recusa_titulo_inexistente_ou_sem_hash() {
        let pool = banco().await;
        assert!(!indexar(&pool, "nao-existe").await.unwrap());
        sqlx::query("INSERT INTO items (id, name, path, media_type) VALUES ('a', 'A', '/x/a.mp4', 'Movie')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(!indexar(&pool, "a").await.unwrap(), "sem hash e sem content_mtime não há o que indexar");
    }

    #[tokio::test]
    async fn indexar_reaproveita_a_assinatura_que_ja_existe_sem_rodar_o_ffmpeg() {
        let pool = banco().await;
        sqlx::query(
            "INSERT INTO items (id, name, path, media_type, content_sha256, content_mtime)
             VALUES ('a', 'A', '/arquivo/que/nao/existe.mp4', 'Movie', x'0102', '1')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO audio_assinaturas VALUES (x'0102', x'0000')").execute(&pool).await.unwrap();
        assert!(indexar(&pool, "a").await.unwrap());
    }

    #[tokio::test]
    async fn indexar_recusa_arquivo_que_mudou_desde_o_hash() {
        let pool = banco().await;
        let arquivo = std::env::temp_dir().join(format!("rustyfin-audio-{}.mp4", uuid::Uuid::new_v4()));
        std::fs::write(&arquivo, b"video").unwrap();
        sqlx::query(
            "INSERT INTO items (id, name, path, media_type, content_sha256, content_mtime)
             VALUES ('a', 'A', ?, 'Movie', x'0304', 'data-que-nao-bate')",
        )
        .bind(arquivo.to_str().unwrap())
        .execute(&pool)
        .await
        .unwrap();
        let resultado = indexar(&pool, "a").await.unwrap();
        std::fs::remove_file(&arquivo).unwrap();
        assert!(!resultado, "a data do arquivo não confere com a do hash: a assinatura seria de outra edição");
        let guardadas: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audio_assinaturas").fetch_one(&pool).await.unwrap();
        assert_eq!(guardadas, 0);
    }

    #[test]
    fn alinhar_nao_chuta_quando_nao_ha_o_que_comparar() {
        let silencio = vec![0_u8; 400];
        // trecho sem variação (silêncio) não identifica nada
        assert_eq!(alinhar(&silencio, &silencio, 100 * PASSO_TICKS), None);
        // posição antes de caber a janela de comparação
        let audio: Vec<u8> = (0..400_u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        assert_eq!(alinhar(&audio, &audio, 0), None);
        // edição nova menor que a janela
        assert_eq!(alinhar(&audio, &audio[..10], 100 * PASSO_TICKS), None);
    }

    #[test]
    fn acha_deslocamento_unico_e_rejeita_duas_copias() {
        let mut seed = 123_u32;
        let audio: Vec<_> = (0..400)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (seed >> 24) as u8
            })
            .collect();
        let mut nova = vec![0; 90];
        nova.extend_from_slice(&audio);
        let posicao = 100 * PASSO_TICKS;
        assert_eq!(
            alinhar(&audio, &nova, posicao),
            Some(posicao + 45 * PASSO_TICKS)
        );
        nova.extend_from_slice(&audio);
        assert_eq!(alinhar(&audio, &nova, posicao), None);
    }
}
