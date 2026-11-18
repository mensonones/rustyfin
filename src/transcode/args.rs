use std::{
    ffi::OsString,
    path::Path,
};

use super::{TranscodeParams, HdrToneMap, HLS_SEGMENT_SECS, Container};


fn add<S: AsRef<std::ffi::OsStr>>(args: &mut Vec<OsString>, s: S) {
    args.push(s.as_ref().to_os_string());
}

/// Família de encoder de hardware que o Rustyfin sabe montar (GPUs Intel e AMD no Linux).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hw {
    /// VAAPI: o caminho padrão para Intel (driver `iHD`) e AMD.
    Vaapi,
    /// Intel Quick Sync (experimental): usa o VAAPI como dispositivo-pai.
    Qsv,
}

impl Hw {
    pub fn from_encoder(encoder: &str) -> Option<Self> {
        match encoder {
            "h264_vaapi" => Some(Self::Vaapi),
            "h264_qsv" => Some(Self::Qsv),
            _ => None,
        }
    }

    /// Filtro que leva os quadros (decodificados na CPU) para a memória da GPU. Sem `extra_hw_frames`: o D3D11 do
    /// Windows recusa o valor 64 que o Jellyfin usa no Linux ("Could not create the texture").
    fn upload() -> &'static str {
        "format=nv12,hwupload"
    }

    /// Opções de entrada que fazem a GPU decodificar e manter os quadros na sua memória.
    fn decode_in(self) -> &'static [&'static str] {
        match self {
            Self::Vaapi => &["-hwaccel", "vaapi", "-hwaccel_output_format", "vaapi", "-hwaccel_device", "hw"],
            Self::Qsv => &["-hwaccel", "qsv", "-hwaccel_output_format", "qsv"],
        }
    }

    /// Conversão de formato feita na própria GPU, entre o decodificador e o encoder.
    fn gpu_filter(self) -> &'static str {
        match self {
            Self::Vaapi => "scale_vaapi=format=nv12",
            Self::Qsv => "vpp_qsv=format=nv12",
        }
    }
}

/// Decodifica na GPU (sem passar os quadros pela CPU): só quando nenhum filtro de software precisa deles.
fn full_hw(p: &TranscodeParams) -> bool {
    p.hw_decode && !p.video_copy && p.burn_subtitle.is_none() && p.tone_map_hdr.is_none() && p.abr.is_none()
}

/// Argumentos globais que abrem o dispositivo de hardware (antes do `-i`).
pub fn hw_init_args(hw: Hw, device: &str) -> Vec<OsString> {
    let args: Vec<String> = match hw {
        Hw::Vaapi => vec!["-init_hw_device".into(), format!("vaapi=hw:{device}"), "-filter_hw_device".into(), "hw".into()],
        // Windows: o QSV deriva do D3D11 da GPU Intel (vendor 0x8086), sem `/dev/dri`.
        Hw::Qsv if cfg!(windows) => vec![
            "-init_hw_device".into(),
            "d3d11va=d3d11:,vendor_id=0x8086".into(),
            "-init_hw_device".into(),
            "qsv=hw@d3d11".into(),
            "-filter_hw_device".into(),
            "hw".into(),
        ],
        // Linux: o QSV deriva do dispositivo VAAPI com o driver `iHD`, como o Jellyfin.
        Hw::Qsv => vec![
            "-init_hw_device".into(),
            format!("vaapi=va:{device},driver=iHD"),
            "-init_hw_device".into(),
            "qsv=hw@va".into(),
            "-filter_hw_device".into(),
            "hw".into(),
        ],
    };
    args.into_iter().map(OsString::from).collect()
}

/// Comando que codifica um quadro de teste no encoder de hardware: se ele falhar, o encoder não serve nesta máquina.
pub fn hw_probe_args(hw: Hw, encoder: &str, device: &str) -> Vec<OsString> {
    // `debug` porque o log informa qual driver VAAPI abriu (iHD, i965, Mesa).
    let mut a: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "debug"].map(OsString::from).to_vec();
    a.extend(hw_init_args(hw, device));
    for s in ["-f", "lavfi", "-i", "color=c=black:s=256x144:d=0.2:r=25", "-vf", Hw::upload(), "-c:v", encoder, "-frames:v", "1", "-f", "null", "-"] {
        add(&mut a, s);
    }
    a
}

/// Comando que decodifica um vídeo de teste na GPU e o codifica de volta: confirma que a decodificação por hardware
/// funciona nesta máquina (senão o Rustyfin só usa a GPU para codificar).
pub fn hw_decode_probe_args(hw: Hw, encoder: &str, device: &str, sample: &Path) -> Vec<OsString> {
    let mut a: Vec<OsString> = ["-nostdin", "-hide_banner", "-loglevel", "error"].map(OsString::from).to_vec();
    a.extend(hw_init_args(hw, device));
    a.extend(hw.decode_in().iter().map(OsString::from));
    add(&mut a, "-i");
    add(&mut a, sample);
    for s in ["-frames:v", "5", "-vf", hw.gpu_filter(), "-c:v", encoder, "-f", "null", "-"] {
        add(&mut a, s);
    }
    a
}

fn map_video(args: &mut Vec<OsString>, p: &TranscodeParams, hdr_filter: Option<&str>, hw: Option<Hw>) {
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
    // Encoder de hardware: os filtros rodam na CPU e só o resultado sobe para a GPU, antes do encoder.
    let mut maps: Vec<String> = if p.abr.is_some() { vec!["[high]".into(), "[low]".into()] } else { vec![video.to_string()] };
    if let Some(hw) = hw {
        let saidas: &[&str] = if p.abr.is_some() { &["[hwhigh]", "[hwlow]"] } else { &["[hwv]"] };
        // Decodificação na GPU: os quadros já estão lá e só trocam de formato; senão sobem da CPU.
        let filtro = if full_hw(p) { hw.gpu_filter() } else { Hw::upload() };
        for (label, saida) in maps.iter().zip(saidas) {
            filters.push(format!("{label}{filtro}{saida}"));
        }
        maps = saidas.iter().map(|s| (*s).to_string()).collect();
    }
    if filters.is_empty() {
        args.extend(["-map", "0:V:0"].map(OsString::from));
    } else {
        add(args, "-filter_complex");
        add(args, filters.join(";"));
        for m in maps {
            add(args, "-map");
            add(args, m);
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
    let hw = Hw::from_encoder(encoder).filter(|_| !p.video_copy);
    if let Some(hw) = hw {
        a.extend(hw_init_args(hw, super::hw_device()));
        if full_hw(p) {
            a.extend(hw.decode_in().iter().map(OsString::from));
        }
    }
    if p.start_secs > 0.0 {
        add(&mut a, "-ss");
        add(&mut a, format!("{:.3}", p.start_secs));
    }
    add(&mut a, "-i");
    add(&mut a, input);

    // Trilhas: primeiro vídeo + áudio escolhido (ou o primeiro, se existir); sem legendas.
    let hdr_filter = p.tone_map_hdr.filter(|_| !p.video_copy).map(HdrToneMap::filter);
    map_video(&mut a, p, hdr_filter.as_deref(), hw);
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
        if hw == Some(Hw::Vaapi) {
            add(&mut a, "-rc_mode");
            add(&mut a, if super::hw_i965() { "CBR" } else { "VBR" });
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
        let bitrate = p.video_bitrate.unwrap_or(8_000_000);
        let bufsize = bitrate.saturating_mul(2).to_string();
        match hw {
            Some(Hw::Vaapi) => {
                // O driver i965 pixeliza em VBR (observação do Jellyfin): usa CBR nele.
                for s in ["-rc_mode", if super::hw_i965() { "CBR" } else { "VBR" }, "-b:v"] {
                    add(&mut a, s);
                }
                add(&mut a, bitrate.to_string());
                add(&mut a, "-maxrate");
                add(&mut a, bitrate.to_string());
                add(&mut a, "-bufsize");
                add(&mut a, bufsize);
            }
            Some(Hw::Qsv) => {
                // `veryfast` troca um pouco de qualidade por velocidade (o padrão `medium` é ~20% mais lento).
                add(&mut a, "-preset");
                add(&mut a, "veryfast");
                // maxrate = bitrate + 1 aciona o VBR do Quick Sync (igual ao Jellyfin).
                add(&mut a, "-b:v");
                add(&mut a, bitrate.to_string());
                add(&mut a, "-maxrate");
                add(&mut a, bitrate.saturating_add(1).to_string());
                add(&mut a, "-rc_init_occupancy");
                add(&mut a, bitrate.to_string());
                add(&mut a, "-bufsize");
                add(&mut a, bufsize);
            }
            None => {
                add(&mut a, "-b:v");
                add(&mut a, bitrate.to_string());
            }
        }
    }
    if encoder == "libx264" && !p.video_copy {
        // Mesmos ajustes de busca de movimento do Jellyfin: mais rápido que o `veryfast` puro, com perda
        // de qualidade imperceptível em streaming ao vivo.
        add(&mut a, "-x264opts");
        add(&mut a, "subme=0:me_range=16:rc_lookahead=10:me=hex:open_gop=0");
    }
    if !p.video_copy && p.hls {
        if hw == Some(Hw::Qsv) {
            // O Quick Sync ignora `-force_key_frames` (gera segmentos de ~10 s e a playlist promete 6 s): o intervalo
            // fixo de quadros-chave, `ceil(segmento × fps)`, é o que alinha os cortes (igual ao Jellyfin).
            let gop = p.hw_gop.unwrap_or(HLS_SEGMENT_SECS * 25).to_string();
            for s in ["-g", &gop, "-keyint_min", &gop] {
                add(&mut a, s);
            }
        } else {
            // Keyframes alinhados aos segmentos HLS.
            add(&mut a, "-force_key_frames");
            add(&mut a, format!("expr:gte(t,n_forced*{HLS_SEGMENT_SECS})"));
        }
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
