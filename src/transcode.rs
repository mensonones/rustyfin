//! Direct Play / Remux / Transcode: decisão a partir do `DeviceProfile` e execução do ffmpeg.
//!
//! - **Direct Play**: o cliente toca o arquivo como está (Range, ver `api/stream.rs`).
//! - **Remux**: vídeo copiado (`-c:v copy`), só áudio/contêiner mudam.
//! - **Transcode**: vídeo recodificado (H.264) quando o codec não é suportado ou a banda é menor.
//!
//! A saída é progressiva (mkv/mp4/ts via pipe) ou HLS (MPEG-TS ou fMP4 em disco). Todo processo
//! ffmpeg é limitado por semáforo, morre junto com a conexão (`kill_on_drop`) e sessões HLS
//! ociosas são encerradas e apagadas.

use crate::{
    config::AppConfig,
    models::{DeviceProfile, MediaStream},
};
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
pub const AUDIO_BITRATE: i64 = 192_000;
pub const HLS_SEGMENT_SECS: u32 = 6;
const ABR_MIN_VIDEO_BITRATE: i64 = 2_000_000;
const ABR_MAX_VIDEO_BITRATE: i64 = 8_000_000 - AUDIO_BITRATE;
const HDR_TO_SDR: &str = "zscale=transfer=linear:npl=100,format=gbrpf32le,zscale=primaries=bt709,tonemap=tonemap=hable,zscale=transfer=bt709:matrix=bt709:range=limited,format=yuv420p";

// ---------------------------------------------------------------- configuração global

pub struct Settings {
    pub ffmpeg: String,
    pub available: bool,
    pub encoder: String,
    pub work_dir: PathBuf,
    tone_mapping: bool,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();
static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
static SESSIONS: OnceLock<Mutex<HashMap<String, HlsSession>>> = OnceLock::new();

static MAX_SLOTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Muda o limite de transcodificações simultâneas sem reiniciar. Ao reduzir, as vagas sobressalentes
/// são retiradas assim que ficam livres (transcodificações em andamento não são interrompidas).
pub fn set_max_slots(new: usize) {
    use std::sync::atomic::Ordering;
    let Some(sem) = SLOTS.get() else { return };
    let new = new.max(1);
    let old = MAX_SLOTS.swap(new, Ordering::Relaxed);
    if new > old {
        sem.add_permits(new - old);
    } else if new < old {
        let sem = sem.clone();
        let diff = (old - new) as u32;
        tokio::spawn(async move {
            if let Ok(permits) = sem.acquire_many_owned(diff).await {
                permits.forget();
            }
        });
    }
}

fn sessions() -> &'static Mutex<HashMap<String, HlsSession>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn ffmpeg_path() -> &'static str {
    SETTINGS.get().map_or("ffmpeg", |s| s.ffmpeg.as_str())
}

pub fn can_transcode() -> bool {
    SETTINGS.get().is_some_and(|s| s.available)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct HdrToneMap {
    missing_primaries: bool,
    missing_matrix: bool,
}

impl HdrToneMap {
    fn filter(self) -> String {
        // BT.2020 é o padrão de HDR10 e HLG. Completa apenas as tags ausentes.
        let defaults = match (self.missing_primaries, self.missing_matrix) {
            (true, true) => "setparams=color_primaries=bt2020:colorspace=bt2020nc,",
            (true, false) => "setparams=color_primaries=bt2020,",
            (false, true) => "setparams=colorspace=bt2020nc,",
            (false, false) => "",
        };
        format!("{defaults}{HDR_TO_SDR}")
    }
}

/// Seleciona vídeo PQ ou HLG quando o `FFmpeg` oferece os filtros necessários.
pub fn tone_map_hdr(streams_json: Option<&str>) -> Option<HdrToneMap> {
    if !SETTINGS.get().is_some_and(|s| s.tone_mapping) {
        return None;
    }
    let streams = serde_json::from_str::<Vec<MediaStream>>(streams_json?).ok()?;
    let video = streams.iter().find(|stream| stream.r#type == "Video")?;
    if !matches!(video.color_transfer.as_deref(), Some("smpte2084" | "arib-std-b67")) {
        return None;
    }
    let missing = |tag: Option<&str>| matches!(tag, None | Some("unknown" | "unspecified"));
    Some(HdrToneMap {
        missing_primaries: missing(video.color_primaries.as_deref()),
        missing_matrix: missing(video.color_space.as_deref()),
    })
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

    let tone_mapping = if available {
        Command::new(&config.ffmpeg_path)
            .args(["-hide_banner", "-filters"])
            .stdin(Stdio::null())
            .output()
            .await
            .is_ok_and(|out| {
                let filters = String::from_utf8_lossy(&out.stdout);
                let has_filter = |name| filters.lines().any(|line| line.split_whitespace().nth(1) == Some(name));
                out.status.success() && has_filter("zscale") && has_filter("tonemap")
            })
    } else {
        false
    };

    let work_dir = config.cache_dir.join("transcodes");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    let initial = config.max_transcodes.max(1);
    MAX_SLOTS.store(initial, std::sync::atomic::Ordering::Relaxed);
    let _ = SLOTS.set(Arc::new(Semaphore::new(initial)));
    let _ = SETTINGS.set(Settings {
        ffmpeg: config.ffmpeg_path.clone(),
        available,
        encoder: config.video_encoder.clone(),
        work_dir,
        tone_mapping,
    });

    if available {
        if !tone_mapping {
            warn!("FFmpeg sem os filtros zscale e tonemap; conversão HDR para SDR indisponível.");
        }
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
    pub abr: bool,
}

impl TranscodePlan {
    pub fn enable_abr(&mut self, height: Option<i32>, source_bitrate: Option<i64>) {
        if !self.hls || self.video_copy || height.is_none_or(|h| h < 360) {
            return;
        }
        let budget = self.video_bitrate.or_else(|| source_bitrate.map(|b| b - AUDIO_BITRATE));
        if let Some(ladder) = budget.and_then(AbrLadder::new) {
            self.video_bitrate = Some(ladder.high);
            self.abr = true;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbrLadder {
    pub high: i64,
    pub low: i64,
}

impl AbrLadder {
    fn new(video_bitrate: i64) -> Option<Self> {
        let high = video_bitrate.min(ABR_MAX_VIDEO_BITRATE);
        (high >= ABR_MIN_VIDEO_BITRATE).then_some(Self { high, low: high / 2 })
    }
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
        "m4v" | "mov" => "mp4".into(),
        "matroska" => "mkv".into(),
        "mpegts" => "ts".into(),
        other => other.into(),
    }
}

/// Lista de perfil ("h264,hevc"): vazia/ausente = qualquer valor.
fn list_allows(list: Option<&str>, value: &str, norm: fn(&str) -> String) -> bool {
    match list.map(str::trim) {
        None | Some("") => true,
        Some(l) => {
            let v = norm(value);
            l.split(',').any(|item| norm(item) == v)
        }
    }
}

fn is_video(kind: Option<&str>) -> bool {
    kind.is_none_or(|k| k.eq_ignore_ascii_case("Video"))
}

/// Contêiner/protocolo de saída segundo os perfis de transcodificação do cliente.
fn pick_output(profile: &DeviceProfile) -> (Container, bool) {
    for tp in profile.transcoding_profiles.iter().filter(|t| is_video(t.r#type.as_deref())) {
        let proto = tp.protocol.as_deref().unwrap_or("").to_lowercase();
        let cont = tp.container.as_deref().unwrap_or("").to_lowercase();
        if proto == "hls" || cont == "m3u8" {
            let container = match cont.trim() {
                "mp4" | "fmp4" => Container::Mp4,
                _ => Container::Ts,
            };
            return (container, true);
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
        .filter(|p| is_video(p.r#type.as_deref()))
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
        (container.is_empty() || list_allows(p.container.as_deref(), container, norm_container))
            && list_allows(p.video_codec.as_deref(), video, norm_codec)
            && audio.is_none_or(|a| list_allows(p.audio_codec.as_deref(), a, norm_codec))
    });

    if opts.enable_direct_play && direct_ok && bitrate_ok {
        return Plan::Direct;
    }

    let video_ok = video_profiles.iter().any(|p| list_allows(p.video_codec.as_deref(), video, norm_codec));
    let audio_ok = audio
        .is_none_or(|a| video_profiles.iter().any(|p| list_allows(p.audio_codec.as_deref(), a, norm_codec)));

    let (container, hls) = pick_output(profile);
    let fragmented = hls && container == Container::Mp4;
    let video_copy = video_ok
        && bitrate_ok
        && (!fragmented || matches!(norm_codec(video).as_str(), "h264" | "hevc" | "av1"));
    let audio_copy = audio_ok
        && (!fragmented || audio.is_none_or(|a| matches!(norm_codec(a).as_str(), "aac" | "ac3" | "eac3")));
    let video_bitrate = if video_copy {
        None
    } else {
        max_bitrate.map(|m| (m - AUDIO_BITRATE).max(500_000))
    };

    Plan::Transcode(TranscodePlan {
        video_copy,
        audio_copy,
        container,
        hls,
        video_bitrate,
        abr: false,
    })
}

// ---------------------------------------------------------------- URL e parâmetros

/// URL que o cliente deve usar quando o plano é Remux/Transcode.
/// Legenda de texto (extraível para SRT/VTT/ASS), em oposição às de imagem (PGS, DVD, DVB).
pub fn is_text_subtitle(codec: &str) -> bool {
    matches!(
        codec.to_ascii_lowercase().as_str(),
        "subrip" | "srt" | "ass" | "ssa" | "webvtt" | "vtt" | "mov_text" | "text" | "subviewer" | "microdvd" | "sami" | "realtext"
    )
}

/// Formato em que a legenda é entregue ao cliente.
pub fn subtitle_ext(codec: &str) -> &'static str {
    match codec.to_ascii_lowercase().as_str() {
        "ass" | "ssa" => "ass",
        "webvtt" | "vtt" => "vtt",
        _ => "srt",
    }
}

pub fn transcode_url(
    item_id: &str,
    plan: &TranscodePlan,
    play_session_id: &str,
    audio_index: Option<i32>,
    start_ticks: Option<i64>,
    burn_subtitle: Option<i32>,
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
    if plan.hls && plan.container == Container::Mp4 {
        q.push("segmentContainer=mp4".to_string());
    }
    if plan.abr {
        q.push("abr=true".to_string());
    }
    if let Some(i) = audio_index.filter(|i| *i >= 0) {
        q.push(format!("audioStreamIndex={i}"));
    }
    if let Some(t) = start_ticks.filter(|t| *t > 0) {
        q.push(format!("startTimeTicks={t}"));
    }
    if let Some(i) = burn_subtitle.filter(|i| *i >= 0) {
        q.push(format!("subtitleStreamIndex={i}"));
        q.push("subtitleMethod=encode".to_string());
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
    /// Legenda de imagem (índice absoluto da trilha) queimada no vídeo.
    pub burn_subtitle: Option<i32>,
    pub tone_map_hdr: Option<HdrToneMap>,
    pub abr: Option<AbrLadder>,
    pub has_audio: bool,
    /// HLS "VOD": playlist completa já na largada; o ffmpeg roda a partir deste segmento e é
    /// reiniciado quando o cliente pede um ponto fora do que ele vai gerar.
    pub vod_start: Option<u32>,
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
        let container = if hls {
            match get("segmentContainer").map(str::to_ascii_lowercase).as_deref() {
                Some("mp4" | "fmp4") => Container::Mp4,
                _ => Container::Ts,
            }
        } else {
            container
        };
        let video_copy = mode.eq_ignore_ascii_case("remux");
        let video_bitrate = get("videoBitrate").and_then(|v| v.parse().ok()).filter(|b: &i64| *b > 0);
        let abr = if hls && !video_copy && get("abr") == Some("true") {
            video_bitrate.and_then(AbrLadder::new)
        } else {
            None
        };
        Some(Self {
            video_copy,
            audio_copy: get("audioCodec").is_some_and(|a| a.eq_ignore_ascii_case("copy")),
            container,
            hls,
            audio_index: get("audioStreamIndex").and_then(|v| v.parse().ok()).filter(|i: &i32| *i >= 0),
            start_secs: get("startTimeTicks")
                .and_then(|v| v.parse::<i64>().ok())
                .filter(|t| *t > 0)
                .map_or(0.0, |t| t as f64 / 10_000_000.0),
            video_bitrate,
            burn_subtitle: if get("subtitleMethod").is_some_and(|m| m.eq_ignore_ascii_case("encode")) {
                get("subtitleStreamIndex").and_then(|v| v.parse().ok()).filter(|i: &i32| *i >= 0)
            } else {
                None
            },
            tone_map_hdr: None,
            abr,
            has_audio: false,
            vod_start: None,
        })
    }
}

// ---------------------------------------------------------------- argumentos do ffmpeg

fn add<S: AsRef<std::ffi::OsStr>>(args: &mut Vec<OsString>, s: S) {
    args.push(s.as_ref().to_os_string());
}

fn map_video(args: &mut Vec<OsString>, p: &TranscodeParams, hdr_filter: Option<&str>) {
    let mut filters = Vec::new();
    let mut video = "[0:V:0]";
    if let Some(filter) = hdr_filter {
        filters.push(format!("{video}{filter}[sdr]"));
        video = "[sdr]";
    }
    if let Some(i) = p.burn_subtitle.filter(|_| !p.video_copy) {
        filters.push(format!("{video}[0:{i}]overlay=eof_action=pass[v]"));
        video = "[v]";
    }
    if p.abr.is_some() {
        filters.push(format!("{video}split=2[high][downscale];[downscale]scale=-2:trunc(ih/4)*2[low]"));
    }
    if filters.is_empty() {
        args.extend(["-map", "0:V:0"].map(OsString::from));
    } else {
        add(args, "-filter_complex");
        add(args, filters.join(";"));
        if p.abr.is_some() {
            args.extend(["-map", "[high]", "-map", "[low]"].map(OsString::from));
        } else {
            args.extend(["-map", video].map(OsString::from));
        }
    }
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
    let hdr_filter = p.tone_map_hdr.filter(|_| !p.video_copy).map(HdrToneMap::filter);
    map_video(&mut a, p, hdr_filter.as_deref());
    let audio = p.audio_index.map_or_else(|| "0:a:0?".to_string(), |i| format!("0:{i}"));
    let audio_outputs = match (p.abr, p.has_audio) {
        (Some(_), true) => 2,
        (Some(_), false) => 0,
        (None, _) => 1,
    };
    for _ in 0..audio_outputs {
        add(&mut a, "-map");
        add(&mut a, &audio);
    }
    add(&mut a, "-sn");
    add(&mut a, "-dn");
    // Sem metadados do original: o MKV herdaria a duração do arquivo inteiro (errada após -ss).
    // Os clientes usam o RunTimeTicks do PlaybackInfo.
    add(&mut a, "-map_metadata");
    add(&mut a, "-1");
    add(&mut a, "-map_chapters");
    add(&mut a, "-1");
    if hdr_filter.is_some() {
        for s in ["-color_primaries", "bt709", "-color_trc", "bt709", "-colorspace", "bt709", "-color_range", "tv"] {
            add(&mut a, s);
        }
    }

    if p.video_copy {
        add(&mut a, "-c:v");
        add(&mut a, "copy");
        add(&mut a, "-avoid_negative_ts");
        add(&mut a, "make_zero");
    } else if let Some(ladder) = p.abr {
        add(&mut a, "-c:v");
        add(&mut a, encoder);
        if encoder == "libx264" {
            for s in ["-preset", "veryfast", "-pix_fmt", "yuv420p"] {
                add(&mut a, s);
            }
        }
        for (i, bitrate) in [ladder.high, ladder.low].into_iter().enumerate() {
            for option in ["b", "maxrate", "bufsize"] {
                add(&mut a, format!("-{option}:v:{i}"));
                add(&mut a, bitrate.to_string());
            }
        }
    } else if encoder == "libx264" {
        for s in ["-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-pix_fmt", "yuv420p"] {
            add(&mut a, s);
        }
        if let Some(b) = p.video_bitrate {
            add(&mut a, "-maxrate");
            add(&mut a, b.to_string());
            add(&mut a, "-bufsize");
            add(&mut a, b.saturating_mul(2).to_string());
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

    if p.audio_copy && p.abr.is_none() {
        add(&mut a, "-c:a");
        add(&mut a, "copy");
    } else {
        for s in ["-c:a", "aac", "-b:a", "192k", "-ac", "2"] {
            add(&mut a, s);
        }
    }
    add(&mut a, "-max_muxing_queue_size");
    add(&mut a, "1024");
    if let Some(n) = p.vod_start.filter(|n| *n > 0 && hls_dir.is_some()) {
        // Os segmentos mantêm a linha do tempo original: o segmento N começa em N × duração.
        add(&mut a, "-output_ts_offset");
        add(&mut a, format!("{}", n * HLS_SEGMENT_SECS));
        add(&mut a, "-start_number");
        add(&mut a, n.to_string());
    }

    match (hls_dir, p.container) {
        (Some(dir), container) => {
            let abr = p.abr.is_some();
            if abr {
                add(&mut a, "-var_stream_map");
                add(&mut a, if p.has_audio { "v:0,a:0,name:high v:1,a:1,name:low" } else { "v:0,name:high v:1,name:low" });
            }
            let (segment_type, extension) = if container == Container::Mp4 {
                add(&mut a, "-hls_fmp4_init_filename");
                add(&mut a, if abr { "init_%v.mp4" } else { "init.mp4" });
                ("fmp4", "m4s")
            } else {
                ("mpegts", "ts")
            };
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
                segment_type,
                "-hls_flags",
                "independent_segments+temp_file",
                "-hls_segment_filename",
            ] {
                add(&mut a, s);
            }
            let segment = if abr { "seg_%v_%05d" } else { "seg_%05d" };
            add(&mut a, dir.join(format!("{segment}.{extension}")));
            add(&mut a, dir.join(if abr { "main_%v.m3u8" } else { "main.m3u8" }));
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
fn segment_parts(name: &str) -> Option<(&str, u32, &str)> {
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
        let plan = TranscodePlan { video_copy: false, audio_copy: false, container: Container::Mkv, hls: false, video_bitrate: Some(6_000_000), abr: false };
        let url = transcode_url("abc", &plan, "sess1", Some(2), Some(50_000_000), None);
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
        TranscodeParams { video_copy, audio_copy, container, hls, audio_index: None, start_secs: 0.0, video_bitrate: None, burn_subtitle: None, tone_map_hdr: None, abr: None, has_audio: false, vod_start: None }
    }

    #[test]
    fn burn_in_image_subtitle() {
        let mut p = params(false, false, Container::Mkv, false);
        p.burn_subtitle = Some(4);
        let a = args_str(&p, None);
        assert!(a.contains("-filter_complex [0:V:0][0:4]overlay=eof_action=pass[v]"), "{a}");
        assert!(a.contains("-map [v]") && !a.contains("-map 0:V:0"));
        // Com vídeo copiado não dá para queimar: segue o mapeamento normal.
        let mut r = params(true, false, Container::Mkv, false);
        r.burn_subtitle = Some(4);
        assert!(args_str(&r, None).contains("-map 0:V:0"));

        let q: HashMap<String, String> = [("transcode", "full"), ("subtitleStreamIndex", "4"), ("subtitleMethod", "encode")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(TranscodeParams::from_query(&q, Container::Mkv, false).unwrap().burn_subtitle, Some(4));
        let mut q2 = q.clone();
        q2.remove("subtitleMethod");
        assert_eq!(TranscodeParams::from_query(&q2, Container::Mkv, false).unwrap().burn_subtitle, None);
    }

    #[test]
    fn subtitle_kinds() {
        assert!(is_text_subtitle("subrip") && is_text_subtitle("ASS") && is_text_subtitle("mov_text"));
        assert!(!is_text_subtitle("hdmv_pgs_subtitle") && !is_text_subtitle("dvd_subtitle"));
        assert_eq!(subtitle_ext("ssa"), "ass");
        assert_eq!(subtitle_ext("webvtt"), "vtt");
        assert_eq!(subtitle_ext("mov_text"), "srt");
    }

    #[test]
    fn vod_playlist_lists_every_segment() {
        let p = vod_playlist(20.0, "/Videos/x/hls/k/", None, false);
        assert!(p.starts_with("#EXTM3U
#EXT-X-VERSION:3
#EXT-X-TARGETDURATION:6
"));
        assert!(p.contains("#EXT-X-PLAYLIST-TYPE:VOD") && p.trim_end().ends_with("#EXT-X-ENDLIST"));
        // 20 s / 6 s = 4 segmentos; o último dura 2 s
        assert_eq!(p.matches("#EXTINF").count(), 4);
        assert!(p.contains("#EXTINF:6.000,
/Videos/x/hls/k/seg_00000.ts
"));
        assert!(p.contains("#EXTINF:2.000,
/Videos/x/hls/k/seg_00003.ts
"));
        let a = vod_playlist(6.0, "/p/", Some("low"), true);
        assert!(a.contains("#EXT-X-MAP:URI=\"/p/init_low.mp4\"") && a.contains("/p/seg_low_00000.m4s"));
        assert_eq!(a.matches("#EXTINF").count(), 1);
    }

    #[test]
    fn segment_names() {
        assert_eq!(segment_parts("seg_00012.ts"), Some(("seg", 12, "ts")));
        assert_eq!(segment_parts("seg_high_00007.m4s"), Some(("seg_high", 7, "m4s")));
        assert_eq!(segment_parts("init.mp4"), None);
        assert_eq!(segment_parts("main.m3u8"), None);
    }

    #[test]
    fn vod_run_keeps_original_timeline() {
        let mut p = params(false, false, Container::Ts, true);
        p.vod_start = Some(10);
        p.start_secs = 60.0;
        let a = args_str(&p, Some(Path::new("/w")));
        assert!(a.contains("-ss 60.000"), "{a}");
        assert!(a.contains("-output_ts_offset 60 -start_number 10"), "{a}");
        // sem VOD (ou no segmento 0) nada disso aparece
        let b = args_str(&params(false, false, Container::Ts, true), Some(Path::new("/w")));
        assert!(!b.contains("-output_ts_offset") && !b.contains("-start_number"));
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
        assert_eq!(hls_file_mime("seg_00012.ts"), Some("video/mp2t"));
        assert_eq!(hls_file_mime("init.mp4"), Some("video/mp4"));
        assert_eq!(hls_file_mime("seg_00012.m4s"), Some("video/iso.segment"));
        assert_eq!(hls_file_mime("init_high.mp4"), Some("video/mp4"));
        assert_eq!(hls_file_mime("seg_low_00012.m4s"), Some("video/iso.segment"));
        for bad in ["../init.mp4", "seg_.ts", "seg_1a.ts", "seg_00001.m4s/../x", "other.m3u8", "seg_1.m4s.tmp", "seg_other_001.ts", ""] {
            assert!(hls_file_mime(bad).is_none(), "{bad}");
        }
        assert!(is_valid_key("0123456789abcdef") && !is_valid_key("../..") && !is_valid_key("short"));
        let out = rewrite_playlist("#EXTM3U\n#EXTINF:6.0,\nseg_00000.ts\n#EXT-X-ENDLIST\n", "/Videos/i/hls/k/");
        assert_eq!(out, "#EXTM3U\n#EXTINF:6.0,\n/Videos/i/hls/k/seg_00000.ts\n#EXT-X-ENDLIST\n");
        let out = rewrite_playlist(
            "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6.0,\nseg_00000.m4s\n",
            "/Videos/i/hls/k/",
        );
        assert_eq!(out, "#EXTM3U\n#EXT-X-MAP:URI=\"/Videos/i/hls/k/init.mp4\"\n#EXTINF:6.0,\n/Videos/i/hls/k/seg_00000.m4s\n");
    }

    #[test]
    fn fmp4_hls_follows_the_client_profile() {
        let p = DeviceProfile {
            transcoding_profiles: vec![tp("mp4", "hls")],
            ..profile()
        };
        let opts = RequestOptions { enable_direct_play: false, ..OPTS };
        let Plan::Transcode(plan) = decide(&src("mkv", "h264", "aac"), Some(&p), &opts, true) else {
            panic!("esperava HLS");
        };
        assert!(plan.hls && plan.container == Container::Mp4 && plan.video_copy);
        let url = transcode_url("abc", &plan, "sess", None, None, None);
        let query = serde_urlencoded::from_str(url.split_once('?').unwrap().1).unwrap();
        let params = TranscodeParams::from_query(&query, Container::Ts, true).unwrap();
        let args = args_str(&params, Some(Path::new("/w/k")));
        assert!(args.contains("-hls_segment_type fmp4"));
        assert!(args.contains("-hls_fmp4_init_filename init.mp4"));
        assert!(args.contains("seg_%05d.m4s"));
    }

    #[test]
    fn hdr_filter_applies_only_when_reencoding_and_before_subtitles() {
        let mut p = params(false, false, Container::Mkv, false);
        p.tone_map_hdr = Some(HdrToneMap::default());
        let args = args_str(&p, None);
        assert!(args.contains("-filter_complex [0:V:0]zscale=transfer=linear") && args.contains("-color_trc bt709"));
        p.burn_subtitle = Some(4);
        let args = args_str(&p, None);
        assert!(args.contains("format=yuv420p[sdr];[sdr][0:4]overlay"));
        p.video_copy = true;
        assert!(!args_str(&p, None).contains("tonemap"));
    }

    #[test]
    fn abr_requires_video_reencode_and_budget() {
        let opts = RequestOptions { enable_direct_play: false, max_bitrate: Some(3_000_000), ..OPTS };
        let Plan::Transcode(mut plan) = decide(&src("mkv", "av1", "aac"), Some(&profile()), &opts, true) else {
            panic!("esperava HLS");
        };
        plan.enable_abr(Some(240), Some(6_000_000));
        assert!(!plan.abr);
        plan.enable_abr(Some(720), Some(6_000_000));
        assert!(plan.abr);
        let url = transcode_url("abc", &plan, "sess", None, None, None);
        let query = serde_urlencoded::from_str(url.split_once('?').unwrap().1).unwrap();
        let mut p = TranscodeParams::from_query(&query, Container::Ts, true).unwrap();
        assert_eq!(p.abr, Some(AbrLadder { high: 2_808_000, low: 1_404_000 }));
        p.has_audio = true;
        let args = args_str(&p, Some(Path::new("/w/k")));
        assert!(args.contains("-var_stream_map v:0,a:0,name:high v:1,a:1,name:low"));
        assert!(args.contains("-b:v:1 1404000") && args.contains("seg_%v_%05d.ts"));

        let mut lower_budget = plan.clone();
        lower_budget.abr = false;
        lower_budget.video_bitrate = Some(1_000_000);
        lower_budget.enable_abr(Some(720), Some(6_000_000));
        assert!(!lower_budget.abr);
    }

    #[test]
    fn session_key_is_stable_and_param_sensitive() {
        let mut q = HashMap::new();
        q.insert("transcode".to_string(), "remux".to_string());
        let k = session_key("item", &q, None);
        assert_eq!(k, session_key("item", &q, None));
        assert_ne!(k, session_key("item", &q, Some(HdrToneMap::default())));
        assert!(is_valid_key(&k));
        q.insert("startTimeTicks".to_string(), "10".to_string());
        assert_ne!(k, session_key("item", &q, None));
        assert_ne!(k, session_key("other", &HashMap::new(), None));
    }
}
