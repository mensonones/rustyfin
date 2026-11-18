use super::*;
use crate::models::{DeviceProfile, DirectPlayProfile, TranscodingProfile};
use std::{collections::HashMap, path::Path};

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
    TranscodeParams { video_copy, audio_copy, container, hls, audio_index: None, start_secs: 0.0, video_bitrate: None, burn_subtitle: None, tone_map_hdr: None, abr: None, has_audio: false, vod_start: None, hw_decode: false, hw_gop: None }
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

fn hw_args(encoder: &str, p: &TranscodeParams) -> String {
    build_args(encoder, Path::new("/m/a.mkv"), p, None)
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn vaapi_sobe_os_quadros_para_a_gpu_antes_do_encoder() {
    let a = hw_args("h264_vaapi", &params(false, true, Container::Mkv, false));
    assert!(a.contains("-init_hw_device vaapi=hw:/dev/dri/renderD128 -filter_hw_device hw"), "{a}");
    assert!(a.contains("-filter_complex [0:V:0]format=nv12,hwupload[hwv]"), "{a}");
    assert!(a.contains("-map [hwv]"), "{a}");
    assert!(a.contains("-c:v h264_vaapi -rc_mode VBR -b:v 8000000 -maxrate 8000000 -bufsize 16000000"), "{a}");
    assert!(!a.contains("-crf") && !a.contains("-x264opts") && !a.contains("-preset"), "{a}");
}

#[test]
fn vaapi_com_filtros_de_software_usa_a_mesma_cadeia() {
    let mut p = params(false, true, Container::Mkv, false);
    p.burn_subtitle = Some(3);
    let a = hw_args("h264_vaapi", &p);
    assert!(a.contains("[0:V:0][0:3]overlay=eof_action=pass[v];[v]format=nv12,hwupload[hwv]"), "{a}");
    assert!(a.contains("-map [hwv]"), "{a}");
}

#[test]
fn vaapi_abr_envia_as_duas_qualidades_para_a_gpu() {
    let mut p = params(false, false, Container::Mp4, true);
    p.abr = Some(AbrLadder { high: 6_000_000, low: 1_500_000 });
    let a = hw_args("h264_vaapi", &p);
    assert!(a.contains("[high]format=nv12,hwupload[hwhigh];[low]format=nv12,hwupload[hwlow]"), "{a}");
    assert!(a.contains("-map [hwhigh] -map [hwlow]"), "{a}");
    assert!(a.contains("-b:v:0 6000000") && a.contains("-b:v:1 1500000"), "{a}");
}

#[test]
fn qsv_deriva_o_dispositivo_do_vaapi() {
    let a = hw_args("h264_qsv", &params(false, true, Container::Mkv, false));
    if cfg!(windows) {
        assert!(a.contains("-init_hw_device d3d11va=d3d11:,vendor_id=0x8086 -init_hw_device qsv=hw@d3d11 -filter_hw_device hw"), "{a}");
    } else {
        assert!(a.contains("-init_hw_device vaapi=va:/dev/dri/renderD128,driver=iHD -init_hw_device qsv=hw@va -filter_hw_device hw"), "{a}");
    }
    assert!(a.contains("format=nv12,hwupload[hwv]"), "{a}");
    assert!(a.contains("-c:v h264_qsv -preset veryfast -b:v 8000000 -maxrate 8000001 -rc_init_occupancy 8000000 -bufsize 16000000"), "{a}");
}

#[test]
fn video_copiado_nao_abre_a_gpu() {
    let a = hw_args("h264_vaapi", &params(true, true, Container::Mkv, false));
    assert!(!a.contains("-init_hw_device") && !a.contains("hwupload"), "{a}");
    assert!(a.contains("-c:v copy"), "{a}");
}

#[test]
fn o_teste_do_encoder_codifica_um_quadro() {
    let a: Vec<String> = hw_probe_args(Hw::Vaapi, "h264_vaapi", "/dev/dri/renderD129").iter().map(|s| s.to_string_lossy().to_string()).collect();
    let a = a.join(" ");
    assert!(a.contains("-init_hw_device vaapi=hw:/dev/dri/renderD129"), "{a}");
    assert!(a.contains("-vf format=nv12,hwupload -c:v h264_vaapi -frames:v 1 -f null -"), "{a}");
}

#[test]
fn decodificacao_na_gpu_mantem_os_quadros_la() {
    let mut p = params(false, true, Container::Mkv, false);
    p.hw_decode = true;
    let v = hw_args("h264_vaapi", &p);
    assert!(v.contains("-hwaccel vaapi -hwaccel_output_format vaapi -hwaccel_device hw"), "{v}");
    assert!(v.contains("-filter_complex [0:V:0]scale_vaapi=format=nv12[hwv]"), "{v}");
    assert!(!v.contains("hwupload"), "{v}");
    let q = hw_args("h264_qsv", &p);
    assert!(q.contains("-hwaccel qsv -hwaccel_output_format qsv"), "{q}");
    assert!(q.contains("-filter_complex [0:V:0]vpp_qsv=format=nv12[hwv]"), "{q}");
    // a opção de decodificação vem antes do arquivo de entrada
    assert!(v.find("-hwaccel vaapi").unwrap() < v.find("-i /m/a.mkv").unwrap(), "{v}");
}

#[test]
fn filtros_de_software_forcam_a_cpu_a_decodificar() {
    let mut p = params(false, true, Container::Mkv, false);
    p.hw_decode = true;
    p.burn_subtitle = Some(3);
    let a = hw_args("h264_vaapi", &p);
    assert!(!a.contains("-hwaccel"), "{a}");
    assert!(a.contains("hwupload"), "{a}");
    let mut abr = params(false, false, Container::Mp4, true);
    abr.hw_decode = true;
    abr.abr = Some(AbrLadder { high: 6_000_000, low: 1_500_000 });
    let a = hw_args("h264_vaapi", &abr);
    assert!(!a.contains("-hwaccel") && a.contains("-rc_mode VBR"), "{a}");
}

#[test]
fn o_teste_de_decodificacao_usa_a_gpu_nos_dois_lados() {
    let a: Vec<String> = hw_decode_probe_args(Hw::Vaapi, "h264_vaapi", "/dev/dri/renderD128", Path::new("/tmp/a.mp4"))
        .iter()
        .map(|s| s.to_string_lossy().to_string())
        .collect();
    let a = a.join(" ");
    assert!(a.contains("-hwaccel vaapi -hwaccel_output_format vaapi") && a.contains("-i /tmp/a.mp4"), "{a}");
    assert!(a.contains("-vf scale_vaapi=format=nv12 -c:v h264_vaapi"), "{a}");
}

#[test]
fn qsv_alinha_os_segmentos_com_o_intervalo_de_quadros_chave() {
    let mut p = params(false, false, Container::Ts, true);
    p.hw_gop = Some(144);
    let q = build_args("h264_qsv", Path::new("/m/a.mkv"), &p, Some(Path::new("/w/k")))
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(q.contains("-g 144 -keyint_min 144"), "{q}");
    assert!(!q.contains("force_key_frames"), "{q}");
    // sem o fps (ffprobe ausente) usa 25 quadros por segundo
    p.hw_gop = None;
    let q = build_args("h264_qsv", Path::new("/m/a.mkv"), &p, Some(Path::new("/w/k")))
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(q.contains("-g 150 -keyint_min 150"), "{q}");
    // o VAAPI continua usando quadros-chave forçados
    let v = build_args("h264_vaapi", Path::new("/m/a.mkv"), &p, Some(Path::new("/w/k")))
        .iter()
        .map(|a| a.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(v.contains("-force_key_frames") && !v.contains("-keyint_min"), "{v}");
}

#[test]
fn o_limite_automatico_acompanha_o_hardware() {
    // em software: metade dos núcleos, de 2 a 8
    assert_eq!(auto_max_transcodes(false, 1), 2);
    assert_eq!(auto_max_transcodes(false, 4), 2);
    assert_eq!(auto_max_transcodes(false, 8), 4);
    assert_eq!(auto_max_transcodes(false, 64), 8);
    // com GPU, cabem mais sessões, qualquer que seja a CPU
    assert_eq!(auto_max_transcodes(true, 2), 4);
}

#[test]
fn sem_a_gpu_nao_ha_dica_de_permissao() {
    assert_eq!(device_permission_hint("/dev/dri/renderD999-inexistente"), None);
}

#[test]
fn a_gpu_so_decodifica_o_que_o_hardware_suporta() {
    assert!(gpu_decodes(Some("h264"), Some("yuv420p")));
    assert!(gpu_decodes(Some("hevc"), Some("yuv420p")));
    assert!(gpu_decodes(Some("hevc"), Some("yuv420p10le")));
    // H.264 de 10 bits (Hi10P) não tem decodificação em hardware nas GPUs Intel
    assert!(!gpu_decodes(Some("h264"), Some("yuv420p10le")));
    // 4:2:2, 4:4:4 e outros codecs ficam na CPU
    assert!(!gpu_decodes(Some("hevc"), Some("yuv422p10le")));
    assert!(!gpu_decodes(Some("vp9"), Some("yuv420p")));
    assert!(!gpu_decodes(None, Some("yuv420p")));
}
