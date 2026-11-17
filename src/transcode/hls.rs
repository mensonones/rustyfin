use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
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


struct HlsSession {
    dir: PathBuf,
    last_access: Instant,
    child: Child,
    permit: Option<OwnedSemaphorePermit>,
    /// Primeiro segmento que este ffmpeg gera (0 nas sessões que não são VOD).
    start_seg: u32,
}

/// Como (re)criar uma sessão HLS VOD: guardada além da vida do ffmpeg, para que um segmento pedido
/// depois de a sessão ociosa ser encerrada ainda possa ser gerado.
#[derive(Clone)]
struct Recipe {
    input: PathBuf,
    params: TranscodeParams,
    created: Instant,
}

static RECIPES: OnceLock<Mutex<HashMap<String, Recipe>>> = OnceLock::new();
const RECIPE_TTL: Duration = Duration::from_secs(6 * 3600);
/// Quantos segmentos à frente do que o ffmpeg já gerou ainda valem a espera (senão, reinicia).
const VOD_LOOKAHEAD: u32 = 4;

fn recipes() -> &'static Mutex<HashMap<String, Recipe>> {
    RECIPES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Registra a receita da sessão VOD e inicia o ffmpeg no segmento inicial, sem esperar por ele.
pub async fn start_vod(key: &str, input: &Path, params: &TranscodeParams, start_seg: u32) -> Result<(), SpawnError> {
    let s = settings()?;
    {
        let mut r = recipes().lock().await;
        r.retain(|_, v| v.created.elapsed() < RECIPE_TTL);
        r.insert(
            key.to_string(),
            Recipe { input: input.to_path_buf(), params: params.clone(), created: Instant::now() },
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
        None => acquire_slot()?,
    };
    let dir = s.work_dir.join(key);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| SpawnError::Io(e.to_string()))?;

    let mut params = recipe.params.clone();
    params.vod_start = Some(seg);
    params.start_secs = f64::from(seg * HLS_SEGMENT_SECS);
    let args = build_args(&s.encoder, &recipe.input, &params, Some(&dir));
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
        HlsSession { dir, last_access: Instant::now(), child, permit: Some(permit), start_seg: seg },
    );
    Ok(())
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
    let Some(recipe) = recipe else {
        return Ok(wait_for_file(key, file, timeout).await);
    };

    let parts = segment_parts(file);
    let wanted = parts.map(|p| p.1);
    let (running, start) = {
        let mut map = sessions().lock().await;
        match map.get_mut(key) {
            Some(sess) => {
                sess.last_access = Instant::now();
                (matches!(sess.child.try_wait(), Ok(None)), Some(sess.start_seg))
            }
            None => (false, None),
        }
    };
    let restart_at = match (start, wanted) {
        // Sem sessão: começa onde foi pedido (ou no início, para o arquivo de inicialização).
        (None, w) => Some(w.unwrap_or(0)),
        (Some(_), None) => None,
        (Some(start), Some(n)) => {
            // Último segmento contíguo já gerado a partir do início desta execução.
            let mut head = start;
            if let Some((prefix, _, ext)) = parts {
                while head < n.saturating_add(VOD_LOOKAHEAD + 1)
                    && tokio::fs::metadata(s.work_dir.join(key).join(format!("{prefix}_{:05}.{ext}", head + 1)))
                        .await
                        .is_ok()
                {
                    head += 1;
                }
            }
            (!running || n < start || n > head + VOD_LOOKAHEAD).then_some(n)
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
    Ok(wait_for_file(key, file, timeout).await)
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
pub async fn ensure_hls(key: &str, input: &Path, p: &TranscodeParams) -> Result<(), SpawnError> {
    let s = settings()?;
    let mut map = sessions().lock().await;
    if let Some(sess) = map.get_mut(key) {
        sess.last_access = Instant::now();
        return Ok(());
    }

    let permit = acquire_slot()?;
    let dir = s.work_dir.join(key);
    let _ = tokio::fs::remove_dir_all(&dir).await;
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
        HlsSession { dir, last_access: Instant::now(), child, permit: Some(permit), start_seg: 0 },
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
        let mut expired: Vec<HlsSession> = Vec::new();
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
                if let Some(s) = map.remove(&k) {
                    expired.push(s);
                }
            }
        }
        for mut s in expired {
            let _ = s.child.start_kill();
            if let Err(e) = tokio::fs::remove_dir_all(&s.dir).await {
                warn!("Falha ao limpar sessão HLS {}: {}", s.dir.display(), e);
            }
        }
    }
}
