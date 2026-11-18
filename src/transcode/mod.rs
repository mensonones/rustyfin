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

pub use args::{Hw, build_args, hw_probe_args};
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

/// Escolhe o encoder: `auto` usa VAAPI quando há GPU (`/dev/dri`) e um quadro de teste codifica; um encoder de hardware
/// pedido explicitamente que não funciona cai para `libx264` em vez de quebrar todas as transcodificações.
async fn resolve_encoder(ffmpeg: &str, requested: &str, device: &str) -> String {
    let requested = requested.trim().to_ascii_lowercase();
    let (encoder, explicit) = match requested.as_str() {
        "" | "auto" => ("h264_vaapi", false),
        other => (other, true),
    };
    let Some(hw) = Hw::from_encoder(encoder) else {
        return if explicit { requested } else { "libx264".to_string() };
    };
    if !std::path::Path::new(device).exists() {
        if explicit {
            warn!("Encoder {encoder} pedido, mas {device} não existe (GPU não repassada ao container?). Usando libx264.");
        }
        return "libx264".to_string();
    }
    let probe = Command::new(ffmpeg)
        .args(hw_probe_args(hw, encoder, device))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(20), probe).await {
        Ok(Ok(out)) if out.status.success() => {
            info!("Aceleração por hardware ativa: {encoder} em {device}.");
            encoder.to_string()
        }
        Ok(Ok(out)) => {
            let erro = String::from_utf8_lossy(&out.stderr);
            warn!("Encoder {encoder} indisponível em {device} ({}); usando libx264.", erro.lines().last().unwrap_or("falha no teste"));
            "libx264".to_string()
        }
        _ => {
            warn!("O teste do encoder {encoder} falhou ou demorou demais; usando libx264.");
            "libx264".to_string()
        }
    }
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

    let hw_device = std::env::var("RUSTYFIN_HWACCEL_DEVICE").unwrap_or_else(|_| DEFAULT_HW_DEVICE.to_string());
    let encoder = if available {
        resolve_encoder(&config.ffmpeg_path, &config.video_encoder, &hw_device).await
    } else {
        "libx264".to_string()
    };

    let work_dir = config.cache_dir.join("transcodes");
    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    let initial = config.max_transcodes.max(1);
    MAX_SLOTS.store(initial, std::sync::atomic::Ordering::Relaxed);
    let _ = SLOTS.set(Arc::new(Semaphore::new(initial)));
    let _ = SETTINGS.set(Settings {
        ffmpeg: config.ffmpeg_path.clone(),
        available,
        encoder: encoder.clone(),
        hw_device,
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
            encoder
        );
        tokio::spawn(hls::reaper());
    } else {
        info!("ffmpeg não encontrado: apenas Direct Play (sem remux/transcode/miniaturas).");
    }
}
