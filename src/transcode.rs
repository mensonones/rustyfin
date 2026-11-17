//! Direct Play / Remux / Transcode: decisão a partir do `DeviceProfile` e execução do ffmpeg.
//!
//! - **Direct Play**: o cliente toca o arquivo como está (Range, ver `api/stream.rs`).
//! - **Remux**: vídeo copiado (`-c:v copy`), só áudio/contêiner mudam.
//! - **Transcode**: vídeo recodificado (H.264) quando o codec não é suportado ou a banda é menor.
//!
//! A saída é progressiva (mkv/mp4/ts via pipe) ou HLS (segmentos `.ts` em disco). Todo processo
//! ffmpeg é limitado por semáforo, morre junto com a conexão (`kill_on_drop`) e sessões HLS
//! ociosas são encerradas e apagadas.

use crate::{config::AppConfig, models::DeviceProfile};
use axum::body::Bytes;
use futures_core::Stream;
use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    ffi::OsString,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{Arc, OnceLock},
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::{
    process::{Child, ChildStdout, Command},
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
};
use tokio_util::io::ReaderStream;
use tracing::{info, warn};

/// Tempo sem requisições após o qual uma sessão HLS é encerrada.
const HLS_IDLE: Duration = Duration::from_secs(90);
const AUDIO_BITRATE: i64 = 192_000;
const HLS_SEGMENT_SECS: u32 = 6;

// ---------------------------------------------------------------- configuração global

pub struct Settings {
    pub ffmpeg: String,
    pub available: bool,
    pub encoder: String,
    pub work_dir: PathBuf,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();
static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
static SESSIONS: OnceLock<Mutex<HashMap<String, HlsSession>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, HlsSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn ffmpeg_path() -> &'static str {
    SETTINGS.get().map(|s| s.ffmpeg.as_str()).unwrap_or("ffmpeg")
}

pub fn can_transcode() -> bool {
    SETTINGS.get().map(|s| s.available).unwrap_or(false)
}

/// Verifica o ffmpeg, limpa restos de execuções anteriores e inicia o coletor de sessões ociosas.
pub async fn init(config: &AppConfig) {
    let available = match Command::new(&config.ffmpeg_path)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
    {
        Ok(s) => s.success(),
        Err(_) => false,
    };

    let work_dir = config.cache_dir.join("transcodes");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    let _ = SLOTS.set(Arc::new(Semaphore::new(config.max_transcodes.max(1))));
    let _ = SETTINGS.set(Settings {
        ffmpeg: config.ffmpeg_path.clone(),
        available,
        encoder: config.video_encoder.clone(),
        work_dir,
    });

    if available {
        info!(
            "ffmpeg encontrado: transcodificação sob demanda ativa (máx. {} simultâneas, encoder {}).",
            config.max_transcodes.max(1),
            config.video_encoder
        );
        tokio::spawn(reaper());
    } else {
        info!("ffmpeg não encontrado: apenas Direct Play (sem remux/transcode/miniaturas).");
    }
}

// ---------------------------------------------------------------- decisão

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Mkv,
    Mp4,
    Ts,
}

impl Container {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_lowercase().as_str() {
            "mkv" | "matroska" => Some(Self::Mkv),
            "mp4" | "m4v" => Some(Self::Mp4),
            "ts" | "mpegts" => Some(Self::Ts),
            _ => None,
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            Self::Mkv => "mkv",
            Self::Mp4 => "mp4",
            Self::Ts => "ts",
        }
    }
    pub fn mime(self) -> &'static str {
        match self {
            Self::Mkv => "video/x-matroska",
            Self::Mp4 => "video/mp4",
            Self::Ts => "video/mp2t",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscodePlan {
    pub video_copy: bool,
    pub audio_copy: bool,
    pub container: Container,
    pub hls: bool,
    pub video_bitrate: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    Direct,
    Transcode(TranscodePlan),
}

pub struct SourceInfo<'a> {
    pub container: Option<&'a str>,
    pub video_codec: Option<&'a str>,
    pub audio_codec: Option<&'a str>,
    pub bitrate: Option<i64>,
}

pub struct RequestOptions {
    pub enable_direct_play: bool,
    pub enable_transcoding: bool,
    pub max_bitrate: Option<i64>,
}

fn norm_codec(c: &str) -> String {
    match c.trim().to_lowercase().as_str() {
        "h265" | "hevc" => "hevc".into(),
        "avc" | "h264" => "h264".into(),
        "dca" => "dts".into(),
        "ac-3" => "ac3".into(),
        "e-ac-3" | "eac-3" => "eac3".into(),
        other => other.into(),
    }
}

fn norm_container(c: &str) -> String {
    match c.trim().to_lowercase().as_str() {
        "m4v" => "mp4".into(),
        "matroska" => "mkv".into(),
        "mpegts" => "ts".into(),
        other => other.into(),
    }
}

/// Lista de perfil ("h264,hevc"): vazia/ausente = qualquer valor.
fn list_allows(list: &Option<String>, value: &str, norm: fn(&str) -> String) -> bool {
    match list.as_deref().map(str::trim) {
        None | Some("") => true,
        Some(l) => {
            let v = norm(value);
            l.split(',').any(|item| norm(item) == v)
        }
    }
}

fn is_video(kind: &Option<String>) -> bool {
    kind.as_deref().map(|k| k.eq_ignore_ascii_case("Video")).unwrap_or(true)
}

/// Contêiner/protocolo de saída segundo os perfis de transcodificação do cliente.
fn pick_output(profile: &DeviceProfile) -> (Container, bool) {
    for tp in profile.transcoding_profiles.iter().filter(|t| is_video(&t.r#type)) {
        let proto = tp.protocol.as_deref().unwrap_or("").to_lowercase();
        let cont = tp.container.as_deref().unwrap_or("").to_lowercase();
        if proto == "hls" || cont == "m3u8" {
            return (Container::Ts, true);
        }
        if let Some(c) = Container::from_name(&cont) {
            return (c, false);
        }
    }
    (Container::Mkv, false)
}

pub fn decide(
    src: &SourceInfo,
    profile: Option<&DeviceProfile>,
    opts: &RequestOptions,
    ffmpeg_ok: bool,
) -> Plan {
    let Some(profile) = profile else {
        return Plan::Direct;
    };
    let video_profiles: Vec<_> = profile
        .direct_play_profiles
        .iter()
        .filter(|p| is_video(&p.r#type))
        .collect();
    // Sem informação suficiente (perfil vazio, ffprobe ausente) ou sem como transcodificar.
    let Some(video) = src.video_codec else {
        return Plan::Direct;
    };
    if video_profiles.is_empty() || !ffmpeg_ok || !opts.enable_transcoding {
        return Plan::Direct;
    }

    let max_bitrate = [profile.max_streaming_bitrate, opts.max_bitrate]
        .into_iter()
        .flatten()
        .filter(|b| *b > 0)
        .min();
    let bitrate_ok = match (max_bitrate, src.bitrate) {
        (Some(max), Some(actual)) => actual <= max,
        _ => true,
    };

    let audio = src.audio_codec;
    let container = src.container.unwrap_or("");
    let direct_ok = video_profiles.iter().any(|p| {
        (container.is_empty() || list_allows(&p.container, container, norm_container))
            && list_allows(&p.video_codec, video, norm_codec)
            && audio.map(|a| list_allows(&p.audio_codec, a, norm_codec)).unwrap_or(true)
    });

    if opts.enable_direct_play && direct_ok && bitrate_ok {
        return Plan::Direct;
    }

    let video_ok = video_profiles.iter().any(|p| list_allows(&p.video_codec, video, norm_codec));
    let audio_ok = audio
        .map(|a| video_profiles.iter().any(|p| list_allows(&p.audio_codec, a, norm_codec)))
        .unwrap_or(true);

    let video_copy = video_ok && bitrate_ok;
    let (container, hls) = pick_output(profile);
    let video_bitrate = if video_copy {
        None
    } else {
        max_bitrate.map(|m| (m - AUDIO_BITRATE).max(500_000))
    };

    Plan::Transcode(TranscodePlan {
        video_copy,
        audio_copy: audio_ok,
        container,
        hls,
        video_bitrate,
    })
}

// ---------------------------------------------------------------- URL e parâmetros

/// URL que o cliente deve usar quando o plano é Remux/Transcode.
pub fn transcode_url(
    item_id: &str,
    plan: &TranscodePlan,
    play_session_id: &str,
    audio_index: Option<i32>,
    start_ticks: Option<i64>,
) -> String {
    let path = if plan.hls {
        format!("/Videos/{item_id}/master.m3u8")
    } else {
        format!("/Videos/{item_id}/stream.{}", plan.container.ext())
    };
    let mut q = vec![
        format!("mediaSourceId={item_id}"),
        format!("transcode={}", if plan.video_copy { "remux" } else { "full" }),
        format!("audioCodec={}", if plan.audio_copy { "copy" } else { "aac" }),
        format!("PlaySessionId={play_session_id}"),
    ];
    if let Some(i) = audio_index.filter(|i| *i >= 0) {
        q.push(format!("audioStreamIndex={i}"));
    }
    if let Some(t) = start_ticks.filter(|t| *t > 0) {
        q.push(format!("startTimeTicks={t}"));
    }
    if let Some(b) = plan.video_bitrate {
        q.push(format!("videoBitrate={b}"));
    }
    format!("{path}?{}", q.join("&"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscodeParams {
    pub video_copy: bool,
    pub audio_copy: bool,
    pub container: Container,
    pub hls: bool,
    pub audio_index: Option<i32>,
    pub start_secs: f64,
    pub video_bitrate: Option<i64>,
}

impl TranscodeParams {
    /// `None` quando a requisição não pede transcodificação (Direct Play).
    pub fn from_query(q: &HashMap<String, String>, container: Container, hls: bool) -> Option<Self> {
        let get = |k: &str| {
            q.iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(k))
                .map(|(_, v)| v.as_str())
        };
        let mode = get("transcode")?;
        Some(Self {
            video_copy: mode.eq_ignore_ascii_case("remux"),
            audio_copy: get("audioCodec").map(|a| a.eq_ignore_ascii_case("copy")).unwrap_or(false),
            container,
            hls,
            audio_index: get("audioStreamIndex").and_then(|v| v.parse().ok()).filter(|i: &i32| *i >= 0),
            start_secs: get("startTimeTicks")
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|t| *t > 0)
                .map(|t| t as f64 / 10_000_000.0)
                .unwrap_or(0.0),
            video_bitrate: get("videoBitrate").and_then(|v| v.parse().ok()).filter(|b: &i64| *b > 0),
        })
    }
}

// ---------------------------------------------------------------- argumentos do ffmpeg

fn add<S: AsRef<std::ffi::OsStr>>(args: &mut Vec<OsString>, s: S) {
    args.push(s.as_ref().to_os_string());
}

pub fn build_args(
    encoder: &str,
    input: &Path,
    p: &TranscodeParams,
    hls_dir: Option<&Path>,
) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();

    add(&mut a, "-nostdin");
    add(&mut a, "-hide_banner");
    add(&mut a, "-loglevel");
    add(&mut a, "error");
    if p.start_secs > 0.0 {
        add(&mut a, "-ss");
        add(&mut a, format!("{:.3}", p.start_secs));
    }
    add(&mut a, "-i");
    add(&mut a, input);

    // Trilhas: primeiro vídeo + áudio escolhido (ou o primeiro, se existir); sem legendas.
    add(&mut a, "-map");
    add(&mut a, "0:v:0");
    add(&mut a, "-map");
    match p.audio_index {
        Some(i) => add(&mut a, format!("0:{i}")),
        None => add(&mut a, "0:a:0?"),
    }
    add(&mut a, "-sn");
    add(&mut a, "-dn");
    // Sem metadados do original: o MKV herdaria a duração do arquivo inteiro (errada após -ss).
    // Os clientes usam o RunTimeTicks do PlaybackInfo.
    add(&mut a, "-map_metadata");
    add(&mut a, "-1");
    add(&mut a, "-map_chapters");
    add(&mut a, "-1");

    if p.video_copy {
        add(&mut a, "-c:v");
        add(&mut a, "copy");
        add(&mut a, "-avoid_negative_ts");
        add(&mut a, "make_zero");
    } else if encoder == "libx264" {
        for s in ["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p"] {
            add(&mut a, s);
        }
        if let Some(b) = p.video_bitrate {
            add(&mut a, "-maxrate");
            add(&mut a, b.to_string());
            add(&mut a, "-bufsize");
            add(&mut a, (b * 2).to_string());
        }
    } else {
        // Encoders de hardware (h264_nvenc, h264_qsv, h264_videotoolbox...): controle por bitrate.
        add(&mut a, "-c:v");
        add(&mut a, encoder);
        add(&mut a, "-b:v");
        add(&mut a, p.video_bitrate.unwrap_or(8_000_000).to_string());
    }
    if !p.video_copy && p.hls {
        // Keyframes alinhados aos segmentos HLS.
        add(&mut a, "-force_key_frames");
        add(&mut a, format!("expr:gte(t,n_forced*{HLS_SEGMENT_SECS})"));
    }

    if p.audio_copy {
        add(&mut a, "-c:a");
        add(&mut a, "copy");
    } else {
        for s in ["-c:a", "aac", "-b:a", "192k", "-ac", "2"] {
            add(&mut a, s);
        }
    }
    add(&mut a, "-max_muxing_queue_size");
    add(&mut a, "1024");

    match (hls_dir, p.container) {
        (Some(dir), _) => {
            for s in [
                "-f",
                "hls",
                "-hls_time",
                &HLS_SEGMENT_SECS.to_string(),
                "-hls_list_size",
                "0",
                "-hls_playlist_type",
                "event",
                "-hls_segment_type",
                "mpegts",
                "-hls_flags",
                "independent_segments",
                "-hls_segment_filename",
            ] {
                add(&mut a, s);
            }
            add(&mut a, dir.join("seg_%05d.ts"));
            add(&mut a, dir.join("main.m3u8"));
        }
        (None, Container::Mkv) => {
            for s in ["-f", "matroska", "pipe:1"] {
                add(&mut a, s);
            }
        }
        (None, Container::Ts) => {
            for s in ["-f", "mpegts", "pipe:1"] {
                add(&mut a, s);
            }
        }
        (None, Container::Mp4) => {
            for s in ["-movflags", "frag_keyframe+empty_moov+default_base_moof", "-f", "mp4", "pipe:1"] {
                add(&mut a, s);
            }
        }
    }
    a
}

// ---------------------------------------------------------------- execução

#[derive(Debug)]
pub enum SpawnError {
    Unavailable,
    Busy,
    Io(String),
}

fn settings() -> Result<&'static Settings, SpawnError> {
    match SETTINGS.get() {
        Some(s) if s.available => Ok(s),
        _ => Err(SpawnError::Unavailable),
    }
}

fn acquire_slot() -> Result<OwnedSemaphorePermit, SpawnError> {
    SLOTS
        .get()
        .ok_or(SpawnError::Unavailable)?
        .clone()
        .try_acquire_owned()
        .map_err(|_| SpawnError::Busy)
}

/// Saída do ffmpeg como stream HTTP. Ao descartar (cliente desconectou) o processo é morto e
/// a vaga do semáforo é liberada.
pub struct ProcessStream {
    inner: ReaderStream<ChildStdout>,
    _child: Child,
    _permit: OwnedSemaphorePermit,
}

impl Stream for ProcessStream {
    type Item = std::io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

pub fn spawn_progressive(input: &Path, p: &TranscodeParams) -> Result<ProcessStream, SpawnError> {
    let s = settings()?;
    let permit = acquire_slot()?;
    let args = build_args(&s.encoder, input, p, None);
    let mut child = Command::new(&s.ffmpeg)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| SpawnError::Io(e.to_string()))?;
    let stdout = child.stdout.take().ok_or_else(|| SpawnError::Io("sem stdout".into()))?;
    Ok(ProcessStream {
        inner: ReaderStream::with_capacity(stdout, 256 * 1024),
        _child: child,
        _permit: permit,
    })
}

// ---------------------------------------------------------------- sessões HLS

struct HlsSession {
    dir: PathBuf,
    last_access: Instant,
    child: Child,
    permit: Option<OwnedSemaphorePermit>,
}

/// Chave estável da sessão: mesmo item + mesmos parâmetros → mesma sessão (mesmos segmentos).
pub fn session_key(item_id: &str, q: &HashMap<String, String>) -> String {
    let mut pairs: Vec<(String, &String)> = q.iter().map(|(k, v)| (k.to_lowercase(), v)).collect();
    pairs.sort();
    let mut h = DefaultHasher::new();
    item_id.hash(&mut h);
    for (k, v) in pairs {
        k.hash(&mut h);
        v.hash(&mut h);
    }
    format!("{:016x}", h.finish())
}

pub fn is_valid_key(key: &str) -> bool {
    key.len() == 16 && key.chars().all(|c| c.is_ascii_hexdigit())
}

/// Aceita apenas `seg_00001.ts` e `main.m3u8` (impede travessia de diretórios).
pub fn is_valid_hls_file(name: &str) -> bool {
    if name == "main.m3u8" {
        return true;
    }
    name.strip_prefix("seg_")
        .and_then(|r| r.strip_suffix(".ts"))
        .map(|d| !d.is_empty() && d.len() <= 9 && d.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}

/// Reescreve os segmentos da playlist para URLs absolutas da sessão.
pub fn rewrite_playlist(content: &str, prefix: &str) -> String {
    let mut out = String::with_capacity(content.len() + 64);
    for line in content.lines() {
        if !line.starts_with('#') && is_valid_hls_file(line.trim()) {
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
        HlsSession { dir, last_access: Instant::now(), child, permit: Some(permit) },
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
async fn reaper() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DirectPlayProfile, TranscodingProfile};

    fn dp(container: &str, video: &str, audio: &str) -> DirectPlayProfile {
        DirectPlayProfile {
            container: Some(container.into()),
            video_codec: Some(video.into()),
            audio_codec: Some(audio.into()),
            r#type: Some("Video".into()),
        }
    }

    fn tp(container: &str, protocol: &str) -> TranscodingProfile {
        TranscodingProfile {
            container: Some(container.into()),
            protocol: Some(protocol.into()),
            r#type: Some("Video".into()),
            ..Default::default()
        }
    }

    fn profile() -> DeviceProfile {
        DeviceProfile {
            direct_play_profiles: vec![dp("mp4,m4v", "h264,hevc", "aac,ac3"), dp("mkv", "h264", "aac")],
            transcoding_profiles: vec![tp("ts", "hls")],
            ..Default::default()
        }
    }

    fn src<'a>(c: &'a str, v: &'a str, a: &'a str) -> SourceInfo<'a> {
        SourceInfo { container: Some(c), video_codec: Some(v), audio_codec: Some(a), bitrate: Some(8_000_000) }
    }

    const OPTS: RequestOptions = RequestOptions { enable_direct_play: true, enable_transcoding: true, max_bitrate: None };

    #[test]
    fn direct_play_when_everything_matches() {
        assert_eq!(decide(&src("mp4", "h264", "aac"), Some(&profile()), &OPTS, true), Plan::Direct);
        assert_eq!(decide(&src("mkv", "h264", "aac"), Some(&profile()), &OPTS, true), Plan::Direct);
    }

    #[test]
    fn no_profile_or_no_ffmpeg_means_direct() {
        assert_eq!(decide(&src("mkv", "hevc", "dts"), None, &OPTS, true), Plan::Direct);
        assert_eq!(decide(&src("mkv", "hevc", "dts"), Some(&profile()), &OPTS, false), Plan::Direct);
        let no_transcode = RequestOptions { enable_transcoding: false, ..OPTS };
        assert_eq!(decide(&src("mkv", "hevc", "dts"), Some(&profile()), &no_transcode, true), Plan::Direct);
    }

    #[test]
    fn remux_when_only_audio_or_container_is_wrong() {
        // HEVC em MKV com DTS: vídeo ok (mp4 aceita hevc), áudio não → copia vídeo, recodifica áudio
        let Plan::Transcode(t) = decide(&src("mkv", "hevc", "dts"), Some(&profile()), &OPTS, true) else {
            panic!("esperava transcode");
        };
        assert!(t.video_copy && !t.audio_copy && t.hls && t.container == Container::Ts);
        // H.264+AAC em AVI: só o contêiner muda → copia tudo
        let Plan::Transcode(t) = decide(&src("avi", "h264", "aac"), Some(&profile()), &OPTS, true) else {
            panic!("esperava remux");
        };
        assert!(t.video_copy && t.audio_copy);
    }

    #[test]
    fn full_transcode_for_unsupported_video() {
        let Plan::Transcode(t) = decide(&src("mkv", "av1", "aac"), Some(&profile()), &OPTS, true) else {
            panic!("esperava transcode");
        };
        assert!(!t.video_copy && t.audio_copy);
    }

    #[test]
    fn bitrate_limit_forces_video_reencode() {
        let opts = RequestOptions { max_bitrate: Some(4_000_000), ..OPTS };
        let Plan::Transcode(t) = decide(&src("mp4", "h264", "aac"), Some(&profile()), &opts, true) else {
            panic!("esperava transcode");
        };
        assert!(!t.video_copy);
        assert_eq!(t.video_bitrate, Some(4_000_000 - AUDIO_BITRATE));
    }

    #[test]
    fn client_can_force_transcode() {
        let opts = RequestOptions { enable_direct_play: false, ..OPTS };
        let Plan::Transcode(t) = decide(&src("mp4", "h264", "aac"), Some(&profile()), &opts, true) else {
            panic!("esperava remux");
        };
        assert!(t.video_copy && t.audio_copy);
    }

    #[test]
    fn unknown_codecs_stay_direct() {
        let s = SourceInfo { container: Some("mkv"), video_codec: None, audio_codec: None, bitrate: None };
        assert_eq!(decide(&s, Some(&profile()), &OPTS, true), Plan::Direct);
    }

    #[test]
    fn codec_aliases() {
        let p = DeviceProfile { direct_play_profiles: vec![dp("mp4", "h265,avc", "aac")], ..Default::default() };
        assert_eq!(decide(&src("mp4", "hevc", "aac"), Some(&p), &OPTS, true), Plan::Direct);
        assert_eq!(decide(&src("m4v", "h264", "aac"), Some(&p), &OPTS, true), Plan::Direct);
    }

    #[test]
    fn progressive_output_when_profile_has_no_hls() {
        let p = DeviceProfile {
            direct_play_profiles: vec![dp("mp4", "h264", "aac")],
            transcoding_profiles: vec![tp("webm", "http"), tp("mp4", "http")],
            ..Default::default()
        };
        let Plan::Transcode(t) = decide(&src("mkv", "h264", "aac"), Some(&p), &OPTS, true) else {
            panic!();
        };
        assert!(!t.hls && t.container == Container::Mp4);
    }

    #[test]
    fn url_roundtrip() {
        let plan = TranscodePlan { video_copy: false, audio_copy: false, container: Container::Mkv, hls: false, video_bitrate: Some(6_000_000) };
        let url = transcode_url("abc", &plan, "sess1", Some(2), Some(50_000_000));
        assert!(url.starts_with("/Videos/abc/stream.mkv?"));
        let q: HashMap<String, String> = url
            .split_once('?')
            .unwrap()
            .1
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let p = TranscodeParams::from_query(&q, Container::Mkv, false).unwrap();
        assert!(!p.video_copy && !p.audio_copy);
        assert_eq!(p.audio_index, Some(2));
        assert_eq!(p.start_secs, 5.0);
        assert_eq!(p.video_bitrate, Some(6_000_000));
        assert!(TranscodeParams::from_query(&HashMap::new(), Container::Mkv, false).is_none());
    }

    fn args_str(p: &TranscodeParams, hls: Option<&Path>) -> String {
        build_args("libx264", Path::new("/m/a.mkv"), p, hls)
            .iter()
            .map(|a| a.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn params(video_copy: bool, audio_copy: bool, container: Container, hls: bool) -> TranscodeParams {
        TranscodeParams { video_copy, audio_copy, container, hls, audio_index: None, start_secs: 0.0, video_bitrate: None }
    }

    #[test]
    fn ffmpeg_args_remux_and_transcode() {
        let a = args_str(&params(true, false, Container::Mkv, false), None);
        assert!(a.contains("-c:v copy") && a.contains("-c:a aac") && a.ends_with("-f matroska pipe:1"));
        assert!(a.contains("-map 0:a:0?") && a.contains("-sn"));
        assert!(a.contains("-map_metadata -1") && a.contains("-map_chapters -1"));
        assert!(!a.contains("-ss"));

        let mut p = params(false, true, Container::Mp4, false);
        p.start_secs = 90.5;
        p.audio_index = Some(3);
        p.video_bitrate = Some(5_000_000);
        let a = args_str(&p, None);
        assert!(a.contains("-ss 90.500 -i /m/a.mkv"));
        assert!(a.contains("-map 0:3") && a.contains("-c:v libx264") && a.contains("-maxrate 5000000"));
        assert!(a.contains("-c:a copy") && a.contains("frag_keyframe+empty_moov"));
    }

    #[test]
    fn ffmpeg_args_hls() {
        let a = args_str(&params(false, false, Container::Ts, true), Some(Path::new("/w/k")));
        assert!(a.contains("-f hls") && a.contains("-hls_playlist_type event"));
        assert!(a.contains("-force_key_frames"));
        assert!(a.contains("seg_%05d.ts") && a.ends_with("main.m3u8"));
        // Hardware encoder usa bitrate em vez de CRF
        let hw = build_args("h264_nvenc", Path::new("/m/a.mkv"), &params(false, true, Container::Mkv, false), None);
        let hw: Vec<String> = hw.iter().map(|a| a.to_string_lossy().to_string()).collect();
        assert!(hw.join(" ").contains("-c:v h264_nvenc -b:v 8000000"));
    }

    #[test]
    fn hls_names_and_playlist() {
        assert!(is_valid_hls_file("main.m3u8") && is_valid_hls_file("seg_00012.ts"));
        for bad in ["../x.ts", "seg_.ts", "seg_1a.ts", "seg_00001.ts/../x", "other.m3u8", ""] {
            assert!(!is_valid_hls_file(bad), "{bad}");
        }
        assert!(is_valid_key("0123456789abcdef") && !is_valid_key("../..") && !is_valid_key("short"));
        let out = rewrite_playlist("#EXTM3U\n#EXTINF:6.0,\nseg_00000.ts\n#EXT-X-ENDLIST\n", "/Videos/i/hls/k/");
        assert_eq!(out, "#EXTM3U\n#EXTINF:6.0,\n/Videos/i/hls/k/seg_00000.ts\n#EXT-X-ENDLIST\n");
    }

    #[test]
    fn session_key_is_stable_and_param_sensitive() {
        let mut q = HashMap::new();
        q.insert("transcode".to_string(), "remux".to_string());
        let k = session_key("item", &q);
        assert_eq!(k, session_key("item", &q));
        assert!(is_valid_key(&k));
        q.insert("startTimeTicks".to_string(), "10".to_string());
        assert_ne!(k, session_key("item", &q));
        assert_ne!(k, session_key("other", &HashMap::new()));
    }
}
