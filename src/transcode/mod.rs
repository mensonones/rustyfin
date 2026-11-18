//! Direct Play / Remux / Transcode: decisão a partir do `DeviceProfile` e execução do ffmpeg.
//!
//! - **Direct Play**: o cliente toca o arquivo como está (Range, ver `api/stream.rs`).
//! - **Remux**: vídeo copiado (`-c:v copy`), só áudio/contêiner mudam.
//! - **Transcode**: vídeo recodificado (H.264) quando o codec não é suportado ou a banda é menor.
//!
//! A saída é progressiva (mkv/mp4/ts via pipe) ou HLS (MPEG-TS ou fMP4 em disco). Todo processo
//! ffmpeg é limitado por semáforo, morre junto com a conexão (`kill_on_drop`) e sessões HLS
//! ociosas são encerradas e apagadas.

mod args;
mod decide;
mod hls;
mod params;
mod process;
#[cfg(test)]
mod tests;
#[cfg(test)]
use hls::segment_parts;

use crate::{
    config::AppConfig,
    models::MediaStream,
};
use std::{
    hash::Hash,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::{
    process::Command,
    sync::Semaphore,
};
use tracing::{info, warn};

pub use args::{Hw, build_args, hw_decode_probe_args, hw_probe_args};
pub use decide::*;
pub use hls::*;
pub use params::*;
pub use process::*;


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
    /// Dispositivo DRM usado pelos encoders de hardware (`/dev/dri/renderD128`).
    pub hw_device: String,
    /// A GPU também decodifica (testado na subida com um vídeo de exemplo).
    pub hw_decode: bool,
    /// O driver VAAPI é o `i965` (Intel antiga), que pede CBR em vez de VBR.
    pub vaapi_i965: bool,
    pub work_dir: PathBuf,
    tone_mapping: bool,
}

static SETTINGS: OnceLock<Settings> = OnceLock::new();
static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();

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


/// Dispositivo DRM dos encoders de hardware (VAAPI e QSV).
pub fn hw_device() -> &'static str {
    SETTINGS.get().map_or(DEFAULT_HW_DEVICE, |s| s.hw_device.as_str())
}

/// Encoder de vídeo em uso, já resolvido (`auto` vira `h264_vaapi` ou `libx264`).
pub fn active_encoder() -> &'static str {
    SETTINGS.get().map_or("libx264", |s| s.encoder.as_str())
}

const DEFAULT_HW_DEVICE: &str = "/dev/dri/renderD128";

/// Resultado da escolha do encoder na subida.
struct Escolha {
    encoder: String,
    decode: bool,
    i965: bool,
}

impl Escolha {
    fn software(encoder: impl Into<String>) -> Self {
        Self { encoder: encoder.into(), decode: false, i965: false }
    }
}

/// O que a GPU precisa saber da origem: se pode decodificá-la e quantos quadros cabem num segmento HLS.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HwSource {
    pub decode: bool,
    pub gop: Option<u32>,
}

/// A GPU decodifica este codec neste formato de pixel? 10 bits só no HEVC: as GPUs Intel não decodificam `H.264 Hi10P` em
/// hardware, e o ffmpeg falha em vez de cair para a CPU.
fn gpu_decodes(codec: Option<&str>, pix_fmt: Option<&str>) -> bool {
    matches!(
        (codec, pix_fmt),
        (Some("hevc"), Some("yuv420p" | "yuvj420p" | "yuv420p10le")) | (Some("h264"), Some("yuv420p" | "yuvj420p"))
    )
}

/// Lê (com o ffprobe, ~50 ms) o formato de pixel e a taxa de quadros do primeiro vídeo. Só é chamada com um encoder de
/// hardware ativo; com `libx264` devolve o padrão sem tocar no arquivo.
///
/// `decode`: a GPU decodifica este vídeo? Só H.264 de 8 bits e HEVC de 8 ou 10 bits, 4:2:0 (o que as GPUs Intel decodificam em
/// hardware), e só se o teste da subida confirmou a decodificação na GPU. `gop`: `ceil(6 s × fps)`, usado como intervalo
/// de quadros-chave pelo Quick Sync (o `-force_key_frames` não vale para ele).
pub async fn hw_source(path: &std::path::Path, streams_json: Option<&str>) -> HwSource {
    let Some(settings) = SETTINGS.get().filter(|s| Hw::from_encoder(&s.encoder).is_some()) else {
        return HwSource::default();
    };
    let codec = streams_json
        .and_then(|j| serde_json::from_str::<Vec<MediaStream>>(j).ok())
        .and_then(|streams| streams.into_iter().find(|s| s.r#type == "Video").map(|s| s.codec.to_ascii_lowercase()));
    let out = Command::new(ffprobe_path())
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt,avg_frame_rate", "-of", "json"])
        .arg(path)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(5), out).await else {
        return HwSource::default();
    };
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return HwSource::default();
    };
    let stream = &json["streams"][0];
    let gpu_ok = gpu_decodes(codec.as_deref(), stream["pix_fmt"].as_str());
    let fps = stream["avg_frame_rate"].as_str().and_then(|r| {
        let (n, d) = r.split_once('/')?;
        let (n, d) = (n.parse::<f64>().ok()?, d.parse::<f64>().ok()?);
        (d > 0.0 && n > 0.0).then(|| n / d)
    });
    HwSource { decode: settings.hw_decode && gpu_ok, gop: fps.map(|f| (f * f64::from(HLS_SEGMENT_SECS)).ceil() as u32) }
}

/// Limite automático de transcodificações simultâneas, quando ninguém o define: com GPU a codificação sai do
/// processador, então cabem mais sessões; em software cada sessão consome CPU, e metade dos núcleos (de 2 a 8) mantém
/// cada uma acima do tempo real com folga (medido: 4 CPUs sustentam 2 sessões a ~4,6× e 4 a ~1,3×).
pub fn auto_max_transcodes(hardware: bool, cpus: usize) -> usize {
    if hardware { 4 } else { (cpus / 2).clamp(2, 8) }
}

/// A GPU existe mas o processo não pode abri-la (típico no Docker sem `group_add`): devolve a dica de correção.
#[cfg(unix)]
fn device_permission_hint(device: &str) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let gid = std::fs::metadata(device).ok()?.gid();
    match std::fs::OpenOptions::new().read(true).write(true).open(device) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Some(format!(
            "sem permissão para abrir {device} (pertence ao grupo {gid}): no docker-compose, adicione `group_add: [\"{gid}\"]` \
             (ou rode tools/gpu-setup.sh)"
        )),
        _ => None,
    }
}

#[cfg(not(unix))]
fn device_permission_hint(_device: &str) -> Option<String> {
    None
}

/// Escolhe o encoder: `auto` usa a GPU quando há uma e um quadro de teste codifica (VAAPI no Linux, Quick Sync no
/// Windows); um encoder de hardware pedido explicitamente que não funciona cai para `libx264` em vez de quebrar todas
/// as transcodificações. Depois confirma, com um vídeo de teste, se a GPU também decodifica.
async fn resolve_encoder(ffmpeg: &str, requested: &str, device: &str) -> Escolha {
    let requested = requested.trim().to_ascii_lowercase();
    let auto_encoder = if cfg!(windows) { "h264_qsv" } else { "h264_vaapi" };
    let (encoder, explicit) = match requested.as_str() {
        "" | "auto" => (auto_encoder, false),
        other => (other, true),
    };
    let Some(hw) = Hw::from_encoder(encoder) else {
        return Escolha::software(if explicit { requested } else { "libx264".to_string() });
    };
    // No Windows não há nó DRM: o dispositivo vem do D3D11.
    if !cfg!(windows) && !std::path::Path::new(device).exists() {
        if explicit {
            warn!("Encoder {encoder} pedido, mas {device} não existe (GPU não repassada ao container?). Usando libx264.");
        }
        // `/dev/dxg` sem `/dev/dri` é o Docker Desktop (WSL2): ele não repassa a GPU Intel ao container.
        if std::path::Path::new("/dev/dxg").exists() {
            info!(
                "Docker Desktop (WSL2) detectado: a GPU Intel não é repassada ao container. Para usar o Quick Sync, rode o Rustyfin direto no Windows (docs/reproducao.md#windows)."
            );
        }
        return Escolha::software("libx264");
    }
    if let Some(dica) = device_permission_hint(device) {
        warn!("GPU encontrada, mas {dica}. Usando libx264.");
        return Escolha::software("libx264");
    }
    let probe = Command::new(ffmpeg)
        .args(hw_probe_args(hw, encoder, device))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let Ok(Ok(out)) = tokio::time::timeout(Duration::from_secs(20), probe).await else {
        warn!("O teste do encoder {encoder} falhou ou demorou demais; usando libx264.");
        return Escolha::software("libx264");
    };
    let log = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        let erro = log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("falha no teste");
        warn!("Encoder {encoder} indisponível em {device} ({erro}); usando libx264.");
        return Escolha::software("libx264");
    }
    let i965 = hw == Hw::Vaapi && log.contains("Intel i965 driver");
    let decode = std::env::var("RUSTYFIN_HWACCEL_DECODE")
        .map_or(true, |v| !matches!(v.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off"))
        && probe_hw_decode(ffmpeg, hw, encoder, device).await;
    info!(
        "Aceleração por hardware ativa: {encoder}{} ({}).",
        if cfg!(windows) { String::new() } else { format!(" em {device}") },
        if decode { "decodifica e codifica na GPU" } else { "só codifica na GPU" },
    );
    Escolha { encoder: encoder.to_string(), decode, i965 }
}

/// Gera um vídeo H.264 de 1 s e o decodifica na GPU: se der certo, a decodificação por hardware está disponível.
async fn probe_hw_decode(ffmpeg: &str, hw: Hw, encoder: &str, device: &str) -> bool {
    let sample = std::env::temp_dir().join(format!("rustyfin-hwprobe-{}.mp4", std::process::id()));
    let made = Command::new(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=s=640x360:d=1:r=25"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-y"])
        .arg(&sample)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .status();
    let ok = if matches!(tokio::time::timeout(Duration::from_secs(20), made).await, Ok(Ok(s)) if s.success()) {
        let test = Command::new(ffmpeg)
            .args(hw_decode_probe_args(hw, encoder, device, &sample))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status();
        matches!(tokio::time::timeout(Duration::from_secs(20), test).await, Ok(Ok(s)) if s.success())
    } else {
        false
    };
    let _ = tokio::fs::remove_file(&sample).await;
    ok
}

/// O driver VAAPI é o `i965` (Intel antiga)?
pub fn hw_i965() -> bool {
    SETTINGS.get().is_some_and(|s| s.vaapi_i965)
}

pub fn ffmpeg_path() -> &'static str {
    SETTINGS.get().map_or("ffmpeg", |s| s.ffmpeg.as_str())
}

/// Executável do `ffprobe`: o que fica ao lado do `ffmpeg` configurado (`RUSTYFIN_FFMPEG`) ou, sem ele, o do `PATH`.
/// Assim um `ffmpeg` instalado numa pasta fora do `PATH` (o setup do Windows, por exemplo) basta para o servidor.
#[must_use]
pub fn ffprobe_path() -> std::ffi::OsString {
    std::path::Path::new(ffmpeg_path())
        .parent()
        .map(|d| d.join(if cfg!(windows) { "ffprobe.exe" } else { "ffprobe" }))
        .filter(|p| p.exists())
        .map_or_else(|| "ffprobe".into(), std::path::PathBuf::into_os_string)
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
/// Como instalar o ffmpeg no sistema em que o servidor roda (ou apontar `RUSTYFIN_FFMPEG`).
#[must_use]
pub fn dica_instalar_ffmpeg() -> &'static str {
    if cfg!(windows) {
        "Instale com `winget install Gyan.FFmpeg` (ou baixe um build win64-gpl) e reinicie o Rustyfin, ou aponte RUSTYFIN_FFMPEG para o ffmpeg.exe."
    } else if cfg!(target_os = "macos") {
        "Instale com `brew install ffmpeg` e reinicie o Rustyfin, ou aponte RUSTYFIN_FFMPEG para o executável."
    } else {
        "Instale com `sudo apt install ffmpeg` (Debian, Ubuntu), `sudo dnf install ffmpeg` (Fedora) ou o gerenciador da sua distribuição, ou aponte RUSTYFIN_FFMPEG para o executável."
    }
}

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

    let hw_device = std::env::var("RUSTYFIN_HWACCEL_DEVICE").unwrap_or_else(|_| DEFAULT_HW_DEVICE.to_string());
    let escolha = if available {
        resolve_encoder(&config.ffmpeg_path, &config.video_encoder, &hw_device).await
    } else {
        Escolha::software("libx264")
    };
    let encoder = escolha.encoder.clone();

    let work_dir = config.cache_dir.join("transcodes");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    let hardware = Hw::from_encoder(&encoder).is_some();
    let auto_slots = crate::settings::max_transcodes_is_auto();
    let initial = if auto_slots {
        let n = auto_max_transcodes(hardware, std::thread::available_parallelism().map_or(2, usize::from));
        crate::settings::set_auto_max_transcodes(n);
        n
    } else {
        crate::settings::snapshot().max_transcodes
    }
    .max(1);
    MAX_SLOTS.store(initial, std::sync::atomic::Ordering::Relaxed);
    let _ = SLOTS.set(Arc::new(Semaphore::new(initial)));
    let _ = SETTINGS.set(Settings {
        ffmpeg: config.ffmpeg_path.clone(),
        available,
        encoder: encoder.clone(),
        hw_device,
        hw_decode: escolha.decode,
        vaapi_i965: escolha.i965,
        work_dir,
        tone_mapping,
    });

    if available {
        if !tone_mapping {
            warn!("FFmpeg sem os filtros zscale e tonemap; conversão HDR para SDR indisponível.");
        }
        info!(
            "ffmpeg encontrado: transcodificação sob demanda ativa (máx. {initial} simultâneas{}, encoder {encoder}).",
            if auto_slots { ", automático" } else { "" }
        );
        tokio::spawn(hls::reaper());
    } else {
        info!("ffmpeg não encontrado: apenas Direct Play (sem remux/transcode/miniaturas). {}", dica_instalar_ffmpeg());
    }
}
