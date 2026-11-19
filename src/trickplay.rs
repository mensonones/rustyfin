//! Miniaturas ao avançar (trickplay): quadros do vídeo em folhas de 10 x 10 miniaturas, uma a cada 10 segundos, que os apps
//! mostram ao arrastar a barra de progresso. O formato é o do Jellyfin: `Trickplay` nos detalhes do item e as folhas em
//! `GET /Videos/{id}/Trickplay/{largura}/{n}.jpg`.
//!
//! As folhas são geradas pelo ffmpeg **em segundo plano**, uma por vez, e só enquanto nenhuma sessão de transcodificação está
//! ativa. Enquanto um título não tem folhas, ele não anuncia `Trickplay` e o app simplesmente não mostra a prévia.

use crate::{db::DbPool, transcode};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::Command;
use tracing::{info, warn};

/// Largura de cada miniatura, em pixels.
pub const LARGURA: u32 = 320;
/// Tempo entre duas miniaturas, em milissegundos.
pub const INTERVALO_MS: u32 = 10_000;
/// Colunas e linhas de miniaturas por folha.
pub const GRADE: u32 = 10;
/// Vídeos mais curtos que isto não ganham miniaturas (2 minutos, em ticks de 100 ns).
const DURACAO_MINIMA_TICKS: i64 = 1_200_000_000;
/// Tempo máximo do ffmpeg em um título.
const LIMITE_DO_FFMPEG: Duration = Duration::from_secs(3 * 3600);

/// Descrição das folhas de uma largura, como a API do Jellyfin a entrega (`TrickplayInfoDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct Info {
    pub width: u32,
    pub height: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub thumbnail_count: u32,
    pub interval: u32,
    pub bandwidth: u64,
}

/// Altura da miniatura para a largura fixa, mantendo a proporção do vídeo e sempre par (o ffmpeg exige).
pub fn altura_para(video_largura: Option<i32>, video_altura: Option<i32>) -> u32 {
    match (video_largura, video_altura) {
        (Some(l), Some(a)) if l > 0 && a > 0 => {
            let a = (f64::from(LARGURA) * f64::from(a) / f64::from(l)).round() as u32;
            a.max(2).div_ceil(2) * 2
        }
        _ => 180,
    }
}

/// Quantas miniaturas o vídeo tem: uma por intervalo completo (o ffmpeg entrega de 0 a 1 a menos, conforme onde caem os
/// quadros-chave; a diferença é só uma célula em branco no fim da última folha).
pub fn contar_miniaturas(duracao_ticks: i64) -> u32 {
    let ms = u64::try_from(duracao_ticks / 10_000).unwrap_or(0);
    u32::try_from(ms / u64::from(INTERVALO_MS)).unwrap_or(u32::MAX).max(1)
}

/// Pasta das folhas de um título para uma largura.
pub fn pasta(cache: &Path, id: &str, largura: u32) -> PathBuf {
    cache.join("trickplay").join(id).join(largura.to_string())
}

/// A folha `n` (a partir de 0), se o id e os números forem válidos. O id só tem hexadecimais: nada de `..` ou barras.
pub fn arquivo_da_folha(cache: &Path, id: &str, largura: u32, n: u32) -> Option<PathBuf> {
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_hexdigit())).then(|| pasta(cache, id, largura).join(format!("{n}.jpg")))
}

/// Os argumentos do ffmpeg que escrevem as folhas `0.jpg`, `1.jpg`... em `saida`.
pub fn argumentos_ffmpeg(entrada: &Path, saida: &Path) -> Vec<String> {
    let filtro = format!("fps=1000/{INTERVALO_MS},scale={LARGURA}:-2,tile={GRADE}x{GRADE}");
    [
        "-nostdin", "-v", "error", "-y", "-threads", "1", "-skip_frame", "nokey", "-i",
    ]
    .iter()
    .map(ToString::to_string)
    .chain([
        entrada.to_string_lossy().into_owned(),
        "-an".into(),
        "-sn".into(),
        "-dn".into(),
        "-vf".into(),
        filtro,
        "-q:v".into(),
        "4".into(),
        "-start_number".into(),
        "0".into(),
        "-f".into(),
        "image2".into(),
        saida.join("%d.jpg").to_string_lossy().into_owned(),
    ])
    .collect()
}

/// `Info` de uma pasta de folhas já gerada. Largura de banda como o Jellyfin: a maior folha, em bits por segundo de vídeo
/// que ela cobre.
fn descrever(altura: u32, miniaturas: u32, maior_folha_bytes: u64) -> Info {
    let segundos = f64::from(GRADE * GRADE * INTERVALO_MS) / 1000.0;
    Info {
        width: LARGURA,
        height: altura,
        tile_width: GRADE,
        tile_height: GRADE,
        thumbnail_count: miniaturas,
        interval: INTERVALO_MS,
        bandwidth: ((maior_folha_bytes as f64 * 8.0) / segundos).ceil() as u64,
    }
}

/// Gera as folhas de um título numa pasta temporária e a põe no lugar de uma vez (nunca fica um conjunto pela metade).
pub async fn gerar(
    cache: &Path,
    id: &str,
    entrada: &Path,
    duracao_ticks: i64,
    video_largura: Option<i32>,
    video_altura: Option<i32>,
) -> Result<Info, String> {
    let temporaria = cache.join("trickplay").join(format!(".gerando-{id}"));
    let _ = tokio::fs::remove_dir_all(&temporaria).await;
    tokio::fs::create_dir_all(&temporaria).await.map_err(|e| format!("não criou a pasta temporária: {e}"))?;

    let ffmpeg = Command::new(transcode::ffmpeg_path())
        .args(argumentos_ffmpeg(entrada, &temporaria))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let resultado = match tokio::time::timeout(LIMITE_DO_FFMPEG, ffmpeg).await {
        Ok(Ok(saida)) if saida.status.success() => Ok(()),
        Ok(Ok(saida)) => Err(format!(
            "o ffmpeg falhou: {}",
            String::from_utf8_lossy(&saida.stderr).lines().last().unwrap_or("sem mensagem")
        )),
        Ok(Err(e)) => Err(format!("não foi possível iniciar o ffmpeg: {e}")),
        Err(_) => Err("o ffmpeg passou do tempo limite".into()),
    };
    if let Err(e) = resultado {
        let _ = tokio::fs::remove_dir_all(&temporaria).await;
        return Err(e);
    }

    let (mut folhas, mut maior) = (0_u32, 0_u64);
    let mut dir = tokio::fs::read_dir(&temporaria).await.map_err(|e| e.to_string())?;
    while let Ok(Some(entrada_dir)) = dir.next_entry().await {
        if entrada_dir.file_name().to_string_lossy().ends_with(".jpg") {
            folhas += 1;
            maior = maior.max(entrada_dir.metadata().await.map_or(0, |m| m.len()));
        }
    }
    let miniaturas = contar_miniaturas(duracao_ticks);
    if folhas == 0 || folhas < miniaturas.div_ceil(GRADE * GRADE) {
        let _ = tokio::fs::remove_dir_all(&temporaria).await;
        return Err(format!("o ffmpeg gerou {folhas} folha(s), esperava {}", miniaturas.div_ceil(GRADE * GRADE)));
    }

    let destino = pasta(cache, id, LARGURA);
    let _ = tokio::fs::remove_dir_all(&destino).await;
    if let Some(pai) = destino.parent() {
        tokio::fs::create_dir_all(pai).await.map_err(|e| e.to_string())?;
    }
    tokio::fs::rename(&temporaria, &destino).await.map_err(|e| format!("não moveu as folhas: {e}"))?;
    Ok(descrever(altura_para(video_largura, video_altura), miniaturas, maior))
}

type Pendente = (String, String, i64, Option<i32>, Option<i32>);

/// Apaga as pastas de títulos que não existem mais no catálogo.
async fn limpar_orfas(pool: &DbPool, cache: &Path) {
    let Ok(mut dir) = tokio::fs::read_dir(cache.join("trickplay")).await else {
        return;
    };
    while let Ok(Some(entrada)) = dir.next_entry().await {
        let nome = entrada.file_name().to_string_lossy().into_owned();
        if nome.starts_with('.') {
            // Sobra de uma geração interrompida.
            let _ = tokio::fs::remove_dir_all(entrada.path()).await;
            continue;
        }
        let existe: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM items WHERE id = ?)")
            .bind(&nome)
            .fetch_one(pool)
            .await
            .unwrap_or(true);
        if !existe {
            let _ = tokio::fs::remove_dir_all(entrada.path()).await;
        }
    }
}

/// Gera as miniaturas dos títulos que ainda não têm, um por vez e só com o servidor sem transcodificações.
/// Quem chama garante que só uma execução roda por vez. `{}` no banco marca o título como sem miniaturas (falhou ou não
/// tem vídeo): ele só é tentado de novo se o arquivo mudar.
pub async fn gerar_pendentes(pool: &DbPool, cache: &Path) {
    if !transcode::can_transcode() {
        return;
    }
    limpar_orfas(pool, cache).await;
    loop {
        let proximo: Option<Pendente> = sqlx::query_as(
            "SELECT id, path, duration_ticks, width, height FROM items
             WHERE trickplay_json IS NULL AND probed = 1 AND video_codec IS NOT NULL
               AND media_type IN ('Movie', 'Episode') AND duration_ticks >= ?
             ORDER BY created_at DESC, name LIMIT 1",
        )
        .bind(DURACAO_MINIMA_TICKS)
        .fetch_optional(pool)
        .await
        .unwrap_or(None);
        let Some((id, caminho, duracao, largura, altura)) = proximo else {
            return;
        };
        // Cede o processador a quem está assistindo.
        while transcode::sessoes_hls_ativas().await > 0 {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        info!("Miniaturas de avanço: gerando para {caminho}");
        let (json, resultado) = match gerar(cache, &id, Path::new(&caminho), duracao, largura, altura).await {
            Ok(i) => (HashMap::from([(LARGURA.to_string(), i)]), None),
            Err(e) => (HashMap::new(), Some(e)),
        };
        if let Some(e) = resultado {
            warn!("Miniaturas de avanço de {caminho} não geradas: {e}");
        }
        let texto = serde_json::to_string(&json).unwrap_or_else(|_| "{}".into());
        let _ = sqlx::query("UPDATE items SET trickplay_json = ? WHERE id = ? AND path = ?")
            .bind(texto)
            .bind(&id)
            .bind(&caminho)
            .execute(pool)
            .await;
    }
}

/// O que o item anuncia em `Trickplay`: a chave externa é o ID da fonte de mídia (o do próprio item) e a interna, a largura.
pub fn anuncio(id: &str, json: Option<&str>) -> Option<HashMap<String, HashMap<String, Info>>> {
    let por_largura: HashMap<String, Info> = serde_json::from_str(json?).ok()?;
    (!por_largura.is_empty()).then(|| HashMap::from([(id.to_owned(), por_largura)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn altura_segue_a_proporcao_e_e_par() {
        assert_eq!(altura_para(Some(1920), Some(1080)), 180);
        assert_eq!(altura_para(Some(1920), Some(800)), 134);
        assert_eq!(altura_para(Some(720), Some(480)), 214);
        assert_eq!(altura_para(None, Some(480)), 180);
        assert_eq!(altura_para(Some(0), Some(0)), 180);
        assert!(altura_para(Some(4096), Some(1)) >= 2);
    }

    #[test]
    fn miniaturas_uma_por_intervalo_completo() {
        assert_eq!(contar_miniaturas(0), 1);
        assert_eq!(contar_miniaturas(99_000_000), 1);
        assert_eq!(contar_miniaturas(200_000_000), 2);
        assert_eq!(contar_miniaturas(1_250_000_000), 12);
        assert_eq!(contar_miniaturas(7_200 * 10_000_000), 720);
    }

    #[test]
    fn folha_so_com_id_hexadecimal() {
        let c = Path::new("/cache");
        assert_eq!(arquivo_da_folha(c, "ab12", 320, 3).unwrap(), c.join("trickplay").join("ab12").join("320").join("3.jpg"));
        for ruim in ["", "../x", "a/b", "xyz", "ab12/../.."] {
            assert!(arquivo_da_folha(c, ruim, 320, 0).is_none(), "{ruim}");
        }
    }

    #[test]
    fn argumentos_do_ffmpeg_pedem_um_quadro_a_cada_intervalo_em_folhas() {
        let a = argumentos_ffmpeg(Path::new("/m/filme.mkv"), Path::new("/tmp/x"));
        let filtro = &a[a.iter().position(|x| x == "-vf").unwrap() + 1];
        assert_eq!(filtro, "fps=1000/10000,scale=320:-2,tile=10x10");
        assert!(a.windows(2).any(|p| p == ["-i", "/m/filme.mkv"]));
        assert!(a.contains(&"-an".to_string()) && a.last().unwrap().ends_with("%d.jpg"));
    }

    #[test]
    fn info_no_formato_do_jellyfin_e_anuncio_por_fonte_de_midia() {
        let info = descrever(180, 721, 250_000);
        assert_eq!((info.width, info.height, info.tile_width, info.tile_height, info.interval), (320, 180, 10, 10, 10_000));
        assert_eq!(info.bandwidth, 2_000); // 250 kB * 8 em 1.000 s de vídeo por folha
        let json = serde_json::to_string(&HashMap::from([("320".to_string(), info.clone())])).unwrap();
        let a = anuncio("abc", Some(&json)).unwrap();
        assert_eq!(a["abc"]["320"], info);
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["abc"]["320"]["ThumbnailCount"], 721);
        // Sem folhas ({}) ou ainda não geradas (NULL): nada a anunciar.
        assert!(anuncio("abc", Some("{}")).is_none() && anuncio("abc", None).is_none());
    }
}
