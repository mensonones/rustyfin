use crate::models::DeviceProfile;

use super::{AUDIO_BITRATE, ABR_MAX_VIDEO_BITRATE, ABR_MIN_VIDEO_BITRATE};


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
    pub(super) fn new(video_bitrate: i64) -> Option<Self> {
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
