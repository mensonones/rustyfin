use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    process::{Child, Command},
    sync::{Mutex, OwnedSemaphorePermit},
};
use tracing::{info, warn};

use super::{TranscodeParams, SpawnError, Settings, HLS_SEGMENT_SECS, build_args, HdrToneMap, Container, HLS_IDLE};
use super::process::{acquire_slot, settings};

static SESSIONS: OnceLock<Mutex<HashMap<String, HlsSession>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, HlsSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}


static LIXO: AtomicU64 = AtomicU64::new(0);

/// Tira a pasta de uma sessão do caminho da chave sem esperar para apagá-la: renomeia (rápido e atômico) para um nome
/// único e apaga o conteúdo em segundo plano. Assim a limpeza nunca alcança a pasta de uma sessão nova que reutilize a
/// mesma chave, e quem chama pode estar com a trava das sessões sem bloquear os outros espectadores por causa de uma
/// árvore com milhares de segmentos. Chame com a trava das sessões ainda em mãos, para a troca não ser ultrapassada.
async fn descartar_pasta(dir: &Path) {
    let numero = LIXO.fetch_add(1, Ordering::Relaxed);
    let base = dir.file_name().map_or_else(|| "sessao".into(), |n| n.to_string_lossy().into_owned());
    let destino = dir.with_file_name(format!("{base}.apagar-{numero}"));
    let alvo = if tokio::fs::rename(dir, &destino).await.is_ok() {
        destino
    } else if tokio::fs::metadata(dir).await.is_ok() {
        // Não deu para renomear (no Windows, enquanto o ffmpeg ainda segura um arquivo): apaga no lugar, em segundo plano.
        dir.to_path_buf()
    } else {
        return;
    };
    tokio::spawn(async move {
        if let Err(erro) = tokio::fs::remove_dir_all(&alvo).await {
            warn!("Falha ao limpar a pasta de sessão HLS {}: {erro}", alvo.display());
        }
    });
}

struct HlsSession {
    dir: PathBuf,
    last_access: Instant,
    child: Child,
    permit: Option<OwnedSemaphorePermit>,
    /// Primeiro segmento que este ffmpeg gera (0 nas sessões que não são VOD).
    start_seg: u32,
    /// `PlaySessionId` do cliente: permite encerrar o ffmpeg quando o app avisa que parou de assistir.
    play_session: Option<String>,
}

/// Como (re)criar uma sessão HLS VOD: guardada além da vida do ffmpeg, para que um segmento pedido
/// depois de a sessão ociosa ser encerrada ainda possa ser gerado.
#[derive(Clone)]
struct Recipe {
    input: PathBuf,
    params: TranscodeParams,
    created: Instant,
    play_session: Option<String>,
    /// O ffmpeg com GPU falhou nesta sessão: as próximas execuções usam `libx264`.
    software: bool,
}

/// Quanto tempo sem pedidos uma sessão precisa ter para ceder a vaga a outra quando o limite de
/// transcodificações está cheio. Um player ativo pede um segmento a cada poucos segundos.
const EVICT_IDLE: Duration = Duration::from_secs(12);

/// Escolhe a sessão que perde a vaga: a que está há mais tempo sem pedidos, desde que passe de `EVICT_IDLE`.
fn pick_victim<'a>(idle: impl Iterator<Item = (&'a String, Duration)>) -> Option<String> {
    idle.filter(|(_, d)| *d > EVICT_IDLE).max_by_key(|(_, d)| *d).map(|(k, _)| k.clone())
}

/// Pega uma vaga de transcodificação. Se o limite está cheio, encerra a sessão parada há mais tempo (um app
/// que fechou o vídeo sem avisar segura a vaga até a limpeza de 90 s) em vez de recusar com 503.
async fn acquire_or_evict(map: &mut HashMap<String, HlsSession>) -> Result<OwnedSemaphorePermit, SpawnError> {
    match acquire_slot() {
        Err(SpawnError::Busy) => {}
        other => return other,
    }
    let victim = pick_victim(map.iter().map(|(k, s)| (k, s.last_access.elapsed())));
    let Some(old) = victim.and_then(|k| map.remove(&k)) else {
        return Err(SpawnError::Busy);
    };
    info!("Limite de transcodificações cheio: sessão parada {} cede a vaga", old.dir.display());
    let HlsSession { dir, mut child, permit, .. } = old;
    let _ = child.start_kill();
    drop(permit);
    // Só renomeia aqui (quem chama segura a trava das sessões): apagar a árvore inteira atrasaria os outros espectadores.
    descartar_pasta(&dir).await;
    acquire_slot()
}

/// Encerramento do servidor: mata todos os ffmpeg das sessões HLS e apaga os diretórios de trabalho.
pub async fn shutdown() {
    let todas: Vec<HlsSession> = {
        let mut map = sessions().lock().await;
        map.drain().map(|(_, s)| s).collect()
    };
    for mut s in todas {
        let _ = s.child.start_kill();
        let _ = tokio::fs::remove_dir_all(&s.dir).await;
    }
}

/// O app avisou que parou de assistir (`Sessions/Playing/Stopped`): encerra o ffmpeg e libera a vaga na hora.
/// A receita fica guardada, então retomar a reprodução recria a sessão.
pub async fn stop_play_session(play_session: &str) {
    let mut map = sessions().lock().await;
    let keys: Vec<String> = map
        .iter()
        .filter(|(_, s)| s.play_session.as_deref() == Some(play_session))
        .map(|(k, _)| k.clone())
        .collect();
    // A pasta sai do caminho da chave ainda com a trava: uma sessão recriada logo depois não perde os arquivos.
    for mut s in keys.iter().filter_map(|k| map.remove(k)) {
        let _ = s.child.start_kill();
        descartar_pasta(&s.dir).await;
        info!("Sessão HLS encerrada a pedido do cliente: {}", s.dir.display());
    }
}

static RECIPES: OnceLock<Mutex<HashMap<String, Recipe>>> = OnceLock::new();
const RECIPE_TTL: Duration = Duration::from_secs(6 * 3600);
/// Quantos segmentos à frente do que o ffmpeg já gerou ainda valem a espera (senão, reinicia).
const VOD_LOOKAHEAD: u32 = 4;

fn recipes() -> &'static Mutex<HashMap<String, Recipe>> {
    RECIPES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registra a receita da sessão VOD e inicia o ffmpeg no segmento inicial, sem esperar por ele.
pub async fn start_vod(
    key: &str,
    input: &Path,
    params: &TranscodeParams,
    start_seg: u32,
    play_session: Option<&str>,
) -> Result<(), SpawnError> {
    let s = settings()?;
    {
        let mut r = recipes().lock().await;
        r.retain(|_, v| v.created.elapsed() < RECIPE_TTL);
        r.insert(
            key.to_string(),
            Recipe {
                input: input.to_path_buf(),
                params: params.clone(),
                created: Instant::now(),
                play_session: play_session.map(str::to_owned),
                software: false,
            },
        );
    }
    let recipe = recipes().lock().await.get(key).cloned();
    let mut map = sessions().lock().await;
    match (map.get_mut(key), recipe) {
        (Some(sess), _) => sess.last_access = Instant::now(),
        (None, Some(r)) => restart_vod(&mut map, key, &r, start_seg, s).await?,
        (None, None) => {}
    }
    Ok(())
}

async fn restart_vod(
    map: &mut HashMap<String, HlsSession>,
    key: &str,
    recipe: &Recipe,
    seg: u32,
    s: &Settings,
) -> Result<(), SpawnError> {
    let permit = match map.remove(key) {
        Some(mut old) => {
            let _ = old.child.start_kill();
            old.permit.take()
        }
        None => None,
    };
    let permit = match permit {
        Some(p) => p,
        None => acquire_or_evict(map).await?,
    };
    let dir = s.work_dir.join(key);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| SpawnError::Io(e.to_string()))?;

    let mut params = recipe.params.clone();
    params.vod_start = Some(seg);
    params.start_secs = f64::from(seg * HLS_SEGMENT_SECS);
    let encoder = if recipe.software { "libx264" } else { s.encoder.as_str() };
    let args = build_args(encoder, &recipe.input, &params, Some(&dir));
    let child = Command::new(&s.ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| SpawnError::Io(e.to_string()))?;
    info!("Sessão HLS {} (VOD) iniciada no segmento {} de {}", key, seg, recipe.input.display());
    map.insert(
        key.to_string(),
        HlsSession {
            dir,
            last_access: Instant::now(),
            child,
            permit: Some(permit),
            start_seg: seg,
            play_session: recipe.play_session.clone(),
        },
    );
    Ok(())
}

/// O ffmpeg deve recomeçar no segmento `n`? Sim se ele parou, se o pedido está antes do ponto em que ele começou ou se
/// está longe demais do que ele já gerou (`perto` = o segmento `n - VOD_LOOKAHEAD` já existe, ou `n` está dentro da
/// janela inicial).
fn deve_reiniciar(running: bool, start: u32, n: u32, perto: bool) -> bool {
    !running || n < start || !perto
}

/// O ffmpeg terminou por conta própria com código de erro (não foi encerrado por nós, que o tiramos do mapa antes).
fn exited_with_error(estado: &std::io::Result<Option<std::process::ExitStatus>>) -> bool {
    matches!(estado, Ok(Some(st)) if !st.success())
}

/// Volta para `libx264` quando um encoder de hardware falhou e a sessão ainda não está em software.
fn should_fall_back_to_software(crashed: bool, encoder: &str, already_software: bool) -> bool {
    crashed && !already_software && super::Hw::from_encoder(encoder).is_some()
}

/// Divide `seg_high_00012.ts` em (`seg_high`, 12, `ts`).
pub(super) fn segment_parts(name: &str) -> Option<(&str, u32, &str)> {
    if !name.starts_with("seg_") {
        return None;
    }
    let (prefix, rest) = name.rsplit_once('_')?;
    let (number, ext) = rest.split_once('.')?;
    Some((prefix, number.parse().ok()?, ext))
}

/// Entrega o arquivo da sessão VOD, reiniciando o ffmpeg se o segmento pedido está fora do alcance
/// do que ele vai gerar (avanço ou retrocesso do usuário, ffmpeg encerrado ou sessão ociosa).
pub async fn ensure_segment(key: &str, file: &str, timeout: Duration) -> Result<Option<PathBuf>, SpawnError> {
    let ativa = sessions().lock().await.contains_key(key);
    if !ativa && !recipes().lock().await.contains_key(key) {
        return Ok(None);
    }
    let s = settings()?;
    let path = s.work_dir.join(key).join(file);
    if tokio::fs::metadata(&path).await.is_ok() {
        if let Some(sess) = sessions().lock().await.get_mut(key) {
            sess.last_access = Instant::now();
        }
        return Ok(Some(path));
    }
    let recipe = recipes().lock().await.get(key).cloned();
    let Some(mut recipe) = recipe else {
        return Ok(wait_for_file(key, file, timeout).await);
    };

    let parts = segment_parts(file);
    let wanted = parts.map(|p| p.1);
    let (running, start, crashed) = {
        let mut map = sessions().lock().await;
        match map.get_mut(key) {
            Some(sess) => {
                sess.last_access = Instant::now();
                let estado = sess.child.try_wait();
                (matches!(estado, Ok(None)), Some(sess.start_seg), exited_with_error(&estado))
            }
            None => (false, None, false),
        }
    };
    // Um ffmpeg com GPU que morreu com erro não deve ser reiniciado do mesmo jeito (falharia de novo): refaz em software.
    if should_fall_back_to_software(crashed, &s.encoder, recipe.software) {
        warn!("O ffmpeg com {} falhou na sessão {key}; refazendo em software (libx264).", s.encoder);
        recipe.software = true;
        if let Some(r) = recipes().lock().await.get_mut(key) {
            r.software = true;
        }
    }
    let restart_at = match (start, wanted) {
        // Sem sessão: começa onde foi pedido (ou no início, para o arquivo de inicialização).
        (None, w) => Some(w.unwrap_or(0)),
        (Some(_), None) => None,
        (Some(start), Some(n)) => {
            // O ffmpeg grava em ordem: se o segmento `n - VOD_LOOKAHEAD` já existe, os anteriores também, e ele está perto
            // o bastante de `n` para valer a espera. Um único acesso ao disco, não um por segmento gerado.
            let perto = match (n <= start.saturating_add(VOD_LOOKAHEAD), parts) {
                (true, _) => true,
                (false, Some((prefix, _, ext))) => {
                    tokio::fs::metadata(s.work_dir.join(key).join(format!("{prefix}_{:05}.{ext}", n - VOD_LOOKAHEAD)))
                        .await
                        .is_ok()
                }
                (false, None) => false,
            };
            deve_reiniciar(running, start, n, perto).then_some(n)
        }
    };
    if let Some(seg) = restart_at {
        let mut map = sessions().lock().await;
        // Outra requisição pode ter reiniciado nesse meio tempo para um ponto próximo.
        let reuse = map.get(key).is_some_and(|sess| {
            Some(sess.start_seg) != start && sess.start_seg <= seg && seg <= sess.start_seg + VOD_LOOKAHEAD * 2
        });
        if !reuse {
            restart_vod(&mut map, key, &recipe, seg, s).await?;
        }
    }
    let pronto = wait_for_file(key, file, timeout).await;
    if pronto.is_some() {
        return Ok(pronto);
    }
    // O ffmpeg com GPU pode ter morrido enquanto esperávamos o segmento: refaz já em software, no mesmo ponto,
    // em vez de devolver 404 e deixar o player tentar de novo com o mesmo erro.
    let caiu = {
        let mut map = sessions().lock().await;
        map.get_mut(key).map(|sess| (exited_with_error(&sess.child.try_wait()), sess.start_seg))
    };
    match caiu {
        Some((true, seg)) if should_fall_back_to_software(true, &s.encoder, recipe.software) => {
            warn!("O ffmpeg com {} falhou na sessão {key}; refazendo em software (libx264).", s.encoder);
            recipe.software = true;
            if let Some(r) = recipes().lock().await.get_mut(key) {
                r.software = true;
            }
            {
                let mut map = sessions().lock().await;
                restart_vod(&mut map, key, &recipe, seg, s).await?;
            }
            Ok(wait_for_file(key, file, timeout).await)
        }
        _ => Ok(None),
    }
}

/// Playlist VOD completa: todos os segmentos de `HLS_SEGMENT_SECS`, para o cliente poder avançar
/// para qualquer ponto. `rendition` é `high`/`low` no ABR.
pub fn vod_playlist(duration_secs: f64, prefix: &str, rendition: Option<&str>, fmp4: bool) -> String {
    let seg = f64::from(HLS_SEGMENT_SECS);
    let count = (duration_secs / seg).ceil().max(1.0) as u32;
    let (ext, seg_name, init) = match rendition {
        Some(r) => (if fmp4 { "m4s" } else { "ts" }, format!("seg_{r}_"), format!("init_{r}.mp4")),
        None => (if fmp4 { "m4s" } else { "ts" }, "seg_".to_string(), "init.mp4".to_string()),
    };
    let mut out = String::with_capacity(64 + count as usize * 48);
    out.push_str("#EXTM3U\n");
    out.push_str(if fmp4 { "#EXT-X-VERSION:7\n" } else { "#EXT-X-VERSION:3\n" });
    out.push_str(&format!("#EXT-X-TARGETDURATION:{HLS_SEGMENT_SECS}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n"));
    if fmp4 {
        out.push_str(&format!("#EXT-X-MAP:URI=\"{prefix}{init}\"\n"));
    }
    for i in 0..count {
        let len = if i + 1 == count { (duration_secs - seg * f64::from(i)).clamp(0.001, seg) } else { seg };
        out.push_str(&format!("#EXTINF:{len:.3},\n{prefix}{seg_name}{i:05}.{ext}\n"));
    }
    out.push_str("#EXT-X-ENDLIST\n");
    out
}

/// Chave estável da sessão: mesmo item + mesmos parâmetros → mesma sessão (mesmos segmentos).
pub fn session_key(item_id: &str, q: &HashMap<String, String>, tone_map_hdr: Option<HdrToneMap>) -> String {
    let mut pairs: Vec<(String, &String)> = q.iter().map(|(k, v)| (k.to_lowercase(), v)).collect();
    pairs.sort();
    let mut h = DefaultHasher::new();
    item_id.hash(&mut h);
    tone_map_hdr.hash(&mut h);
    for (k, v) in pairs {
        k.hash(&mut h);
        v.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

pub fn is_valid_key(key: &str) -> bool {
    key.len() == 16 && key.chars().all(|c| c.is_ascii_hexdigit())
}

/// Reconhece os arquivos que podem ser servidos pela rota de segmentos.
pub fn hls_file_mime(name: &str) -> Option<&'static str> {
    if name == "main.m3u8" {
        return Some("application/vnd.apple.mpegurl");
    }
    if is_hls_init(name) {
        return Some(Container::Mp4.mime());
    }
    let segment = name.strip_prefix("seg_")?;
    let segment = segment
        .strip_prefix("high_")
        .or_else(|| segment.strip_prefix("low_"))
        .unwrap_or(segment);
    let (number, ext) = segment.split_once('.')?;
    if number.is_empty() || number.len() > 9 || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    match ext {
        "ts" => Some(Container::Ts.mime()),
        "m4s" => Some("video/iso.segment"),
        _ => None,
    }
}

fn is_hls_init(name: &str) -> bool {
    matches!(name, "init.mp4" | "init_high.mp4" | "init_low.mp4")
}

/// Reescreve os segmentos da playlist para URLs absolutas da sessão.
pub fn rewrite_playlist(content: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(content.len() + 64);
    for line in content.lines() {
        let init = line
            .strip_prefix("#EXT-X-MAP:URI=\"")
            .and_then(|s| s.strip_suffix('"'))
            .filter(|name| is_hls_init(name));
        if let Some(init) = init {
            out.push_str("#EXT-X-MAP:URI=\"");
            out.push_str(prefix);
            out.push_str(init);
            out.push('"');
        } else if hls_file_mime(line.trim()).is_some() {
            out.push_str(prefix);
            out.push_str(line.trim());
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Garante que existe um ffmpeg HLS para a sessão (inicia se necessário).
pub async fn ensure_hls(key: &str, input: &Path, p: &TranscodeParams, play_session: Option<&str>) -> Result<(), SpawnError> {
    let s = settings()?;
    let mut map = sessions().lock().await;
    if let Some(sess) = map.get_mut(key) {
        sess.last_access = Instant::now();
        return Ok(());
    }

    let permit = acquire_or_evict(&mut map).await?;
    let dir = s.work_dir.join(key);
    descartar_pasta(&dir).await;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| SpawnError::Io(e.to_string()))?;

    let args = build_args(&s.encoder, input, p, Some(&dir));
    let child = Command::new(&s.ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| SpawnError::Io(e.to_string()))?;

    info!("Sessão HLS {} iniciada para {}", key, input.display());
    map.insert(
        key.to_string(),
        HlsSession {
            dir,
            last_access: Instant::now(),
            child,
            permit: Some(permit),
            start_seg: 0,
            play_session: play_session.map(str::to_owned),
        },
    );
    Ok(())
}

/// Espera um arquivo da sessão aparecer. `None` se o ffmpeg terminou sem gerá-lo ou deu timeout.
pub async fn wait_for_file(key: &str, name: &str, timeout: Duration) -> Option<PathBuf> {
    let deadline = Instant::now() + timeout;
    loop {
        let (path, running) = {
            let mut map = sessions().lock().await;
            let sess = map.get_mut(key)?;
            sess.last_access = Instant::now();
            let running = matches!(sess.child.try_wait(), Ok(None));
            (sess.dir.join(name), running)
        };
        if tokio::fs::metadata(&path).await.is_ok() {
            return Some(path);
        }
        if !running || Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Encerra sessões ociosas e libera vagas de processos que já terminaram.
pub(super) async fn reaper() {
    loop {
        tokio::time::sleep(Duration::from_secs(5)).await;
        {
            let mut map = sessions().lock().await;
            for sess in map.values_mut() {
                if sess.permit.is_some() && !matches!(sess.child.try_wait(), Ok(None)) {
                    sess.permit = None; // ffmpeg terminou: libera a vaga
                }
            }
            let keys: Vec<String> = map
                .iter()
                .filter(|(_, s)| s.last_access.elapsed() > HLS_IDLE)
                .map(|(k, _)| k.clone())
                .collect();
            for k in keys {
                if let Some(mut s) = map.remove(&k) {
                    let _ = s.child.start_kill();
                    descartar_pasta(&s.dir).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn so_cai_para_software_quando_o_encoder_de_hardware_falhou() {
        assert!(should_fall_back_to_software(true, "h264_qsv", false));
        assert!(should_fall_back_to_software(true, "h264_vaapi", false));
        // em software não há para onde cair, e uma falha repetida não reinicia o mesmo ciclo
        assert!(!should_fall_back_to_software(true, "libx264", false));
        assert!(!should_fall_back_to_software(true, "h264_qsv", true));
        // sem falha, nada muda
        assert!(!should_fall_back_to_software(false, "h264_qsv", false));
    }

    #[test]
    fn o_reinicio_do_vod_so_olha_para_a_frente_do_que_foi_gerado() {
        // Rodando, pedido logo à frente: espera o ffmpeg.
        assert!(!deve_reiniciar(true, 0, 3, true));
        // Rodando, mas o pedido está longe do que ele já gerou (avanço do usuário): reinicia no ponto pedido.
        assert!(deve_reiniciar(true, 0, 300, false));
        // Pedido antes do ponto em que esta execução começou (retrocesso): reinicia.
        assert!(deve_reiniciar(true, 120, 40, true));
        // ffmpeg parado: reinicia mesmo com o segmento por perto.
        assert!(deve_reiniciar(false, 0, 3, true));
    }

    #[tokio::test]
    async fn a_limpeza_de_uma_sessao_nao_alcanca_a_pasta_da_sessao_recriada() {
        let raiz = std::env::temp_dir().join(format!("rustyfin-hls-descarte-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&raiz);
        let pasta = raiz.join("0123456789abcdef");
        std::fs::create_dir_all(&pasta).unwrap();
        std::fs::write(pasta.join("seg_00000.ts"), b"velho").unwrap();

        descartar_pasta(&pasta).await;
        // O caminho da chave fica livre na hora: uma sessão nova pode usá-lo já.
        assert!(!pasta.exists());
        std::fs::create_dir_all(&pasta).unwrap();
        std::fs::write(pasta.join("seg_00000.ts"), b"novo").unwrap();

        // A limpeza roda em segundo plano, mas só na pasta renomeada.
        let sobras = || {
            std::fs::read_dir(&raiz)
                .unwrap()
                .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".apagar-"))
                .count()
        };
        for _ in 0..50 {
            if sobras() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(sobras(), 0, "a pasta renomeada foi apagada");
        assert_eq!(std::fs::read(pasta.join("seg_00000.ts")).unwrap(), b"novo");
        let _ = std::fs::remove_dir_all(&raiz);
    }

    #[test]
    fn evicts_only_the_longest_idle_session_past_the_threshold() {
        let (a, b, c) = ("a".to_string(), "b".to_string(), "c".to_string());
        let ativos = [(&a, Duration::from_secs(3)), (&b, Duration::from_secs(8))];
        assert_eq!(pick_victim(ativos.into_iter()), None, "ninguém passou do limite: segue recusando");
        let mistos = [(&a, Duration::from_secs(3)), (&b, Duration::from_secs(40)), (&c, Duration::from_secs(20))];
        assert_eq!(pick_victim(mistos.into_iter()).as_deref(), Some("b"));
    }
}
