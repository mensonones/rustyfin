use std::collections::HashMap;

use super::{TranscodePlan, Container, HdrToneMap, AbrLadder};


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
