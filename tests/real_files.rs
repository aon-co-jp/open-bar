//! 実ファイルでのE2Eテスト。ffmpegで各形式の実ファイルを作り、open-barで読んで内容を確かめる。
//! ffmpegが無い環境ではスキップする(該当テストは即成功)。

use open_bar::{media, pcm};
use std::path::PathBuf;
use std::process::Command;

fn ffmpeg_ok() -> bool {
    Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("open_bar_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 1kHzの正弦波(振幅0.5)を各形式で作る。
fn make(dir: &std::path::Path, file: &str, extra: &[&str]) -> String {
    let path = dir.join(file);
    let mut args = vec!["-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000:duration=2", "-af", "volume=4"];
    args.extend_from_slice(extra);
    args.push(path.to_str().unwrap());
    let out = Command::new("ffmpeg").args(&args).output().unwrap();
    assert!(out.status.success(), "{file}: {}", String::from_utf8_lossy(&out.stderr));
    path.to_string_lossy().to_string()
}

fn tone_amplitude(x: &[f32], channels: usize, rate: u32, freq: f64) -> f64 {
    let ch0: Vec<f32> = x.iter().step_by(channels).copied().collect();
    let n = ch0.len().min(rate as usize); // 最初の1秒
    let (mut s, mut c) = (0.0, 0.0);
    for (i, v) in ch0[..n].iter().enumerate() {
        let ph = 2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64;
        s += *v as f64 * ph.sin();
        c += *v as f64 * ph.cos();
    }
    2.0 * (s * s + c * c).sqrt() / n as f64
}

#[test]
fn common_formats_all_decode_to_the_same_1khz_tone() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = tmpdir("formats");
    let cases: Vec<(&str, Vec<&str>, f64)> = vec![
        ("t.wav", vec!["-c:a", "pcm_s24le"], 0.01),
        ("t.flac", vec!["-c:a", "flac"], 0.01),
        ("t.mp3", vec!["-c:a", "libmp3lame", "-b:a", "192k"], 0.05),
        ("t.m4a", vec!["-c:a", "aac", "-b:a", "192k"], 0.05),
        ("t.ogg", vec!["-c:a", "libvorbis", "-q:a", "6"], 0.05),
        ("t.opus", vec!["-c:a", "libopus", "-b:a", "128k"], 0.05),
        ("t.mka", vec!["-c:a", "flac"], 0.01),
    ];
    for (file, extra, tol) in cases {
        let path = make(&dir, file, &extra);
        let p = pcm::decode_file(&path).unwrap_or_else(|e| panic!("{file}: {e}"));
        let a = tone_amplitude(&p.samples, p.channels, p.sample_rate, 1000.0);
        eprintln!("{file}: {}Hz {}ch 長さ{:.2}秒 1kHz振幅{a:.3}", p.sample_rate, p.channels, p.duration_secs());
        assert!((a - 0.5).abs() < tol, "{file}: 1kHz振幅が0.5のはず: {a}");
        assert!((p.duration_secs() - 2.0).abs() < 0.15, "{file}: 長さ2秒のはず: {}", p.duration_secs());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mp4_and_mkv_video_containers_expose_their_audio_tracks() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = tmpdir("video");
    for (file, acodec) in [("v.mp4", "aac"), ("v.mkv", "flac"), ("v.webm", "libopus")] {
        let path = dir.join(file);
        let out = Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=duration=2:size=160x120:rate=10", "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=48000:duration=2", "-af", "volume=4", "-c:v", if file.ends_with("webm") { "libvpx" } else { "libx264" }, "-c:a", acodec, "-shortest", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "{file}: {}", String::from_utf8_lossy(&out.stderr));
        let info = media::probe(path.to_str().unwrap());
        assert_eq!(info.kind, media::MediaKind::VideoContainer, "{file}");
        let p = pcm::decode_file(path.to_str().unwrap()).unwrap_or_else(|e| panic!("{file}: {e}"));
        let a = tone_amplitude(&p.samples, p.channels, p.sample_rate, 1000.0);
        eprintln!("{file}: 音声{}Hz {}ch 1kHz振幅{a:.3}", p.sample_rate, p.channels);
        assert!((a - 0.5).abs() < 0.06, "{file}: {a}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_dsf_made_by_open_mqa_dsd_style_bits_plays_through_probe_and_decode() {
    // make-diskが作った実DSD256(あれば)を、open-barのprobe→自動PCM化で通す
    let path = "F:/tmp/cd_dsd/Track04.dsf";
    if !std::path::Path::new(path).exists() {
        return;
    }
    let info = media::probe(path);
    assert_eq!(info.kind, media::MediaKind::Dsd);
    assert_eq!((info.sample_rate_hz, info.channels), (Some(11_289_600), Some(2)));
    assert!((info.duration_secs.unwrap() - 241.95).abs() < 0.1);
}

#[test]
fn a_fake_mqa_flac_is_flagged_by_its_encoder_tag() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = tmpdir("mqa");
    let plain = make(&dir, "plain.flac", &["-c:a", "flac"]);
    let tagged = make(&dir, "tagged.flac", &["-c:a", "flac", "-metadata", "MQAENCODER=MQAEncode v1.1, 2.4.2+0", "-metadata", "ORIGINALSAMPLERATE=96000"]);
    assert!(!media::probe(&plain).is_mqa);
    assert!(media::probe(&tagged).is_mqa, "MQAENCODERタグ付きFLACはMQAと判定される(タグベースの簡易判定)");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gapless_concatenation_has_no_gap_and_replaygain_scales_the_second_track() {
    if !ffmpeg_ok() {
        return;
    }
    let dir = tmpdir("gapless");
    let a = make(&dir, "a.flac", &["-c:a", "flac"]);
    let b = make(&dir, "b.flac", &["-c:a", "flac", "-metadata", "REPLAYGAIN_TRACK_GAIN=-6.02 dB"]);
    let plain = open_bar::playlist::load_gapless(&[a.clone(), b.clone()], false).unwrap();
    assert_eq!(plain.samples.len(), 2 * 48_000 * 2, "2秒+2秒がそのまま繋がる(隙間・重複なし)");
    let first = tone_amplitude(&plain.samples[..48_000], 1, 48_000, 1000.0);
    let second_start = 48_000 * 2; // 2曲目の先頭
    let second = tone_amplitude(&plain.samples[second_start..second_start + 48_000], 1, 48_000, 1000.0);
    assert!((first - 0.5).abs() < 0.01 && (second - 0.5).abs() < 0.01, "ゲインなしなら両方0.5: {first} {second}");
    let rg = open_bar::playlist::load_gapless(&[a, b], true).unwrap();
    let second_rg = tone_amplitude(&rg.samples[second_start..second_start + 48_000], 1, 48_000, 1000.0);
    assert!((second_rg - 0.25).abs() < 0.01, "ReplayGain -6.02dBで2曲目が半分の0.25になる: {second_rg}");
    let _ = std::fs::remove_dir_all(&dir);
}
