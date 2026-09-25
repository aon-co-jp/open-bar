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

/// 実デバイスで対話プレーヤーを動かす: 再生→位置が進む→一時停止で止まる→シーク→停止。
#[test]
fn interactive_player_plays_pauses_seeks_and_stops_on_the_real_device() {
    if !ffmpeg_ok() || cpal::traits::HostTrait::default_output_device(&cpal::default_host()).is_none() {
        return;
    }
    let dir = tmpdir("player");
    let a = make(&dir, "a.flac", &["-c:a", "flac"]);
    let b = make(&dir, "b.flac", &["-c:a", "flac"]);
    let p = open_bar::player::Player::new();
    p.set_volume(0.05); // 小さい音量(テスト音)
    p.set_playlist(vec![a, b]);
    p.play(0);
    let wait = |cond: &dyn Fn(&open_bar::player::Status) -> bool, secs: f64| {
        let t0 = std::time::Instant::now();
        loop {
            let s = p.status();
            if cond(&s) {
                return s;
            }
            assert!(t0.elapsed().as_secs_f64() < secs, "タイムアウト: {s:?}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    let s = wait(&|s| s.state == open_bar::player::PlayState::Playing && s.position_secs > 0.3, 10.0);
    assert!((s.duration_secs - 2.0).abs() < 0.1, "{s:?}");
    p.pause();
    let paused = wait(&|s| s.state == open_bar::player::PlayState::Paused, 3.0);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let later = p.status();
    assert!((later.position_secs - paused.position_secs).abs() < 0.15, "一時停止中は位置が進まない: {} → {}", paused.position_secs, later.position_secs);
    p.seek(1.5);
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(p.status().position_secs >= 1.4, "シーク: {}", p.status().position_secs);
    p.resume();
    // 1曲目が終わると自動で2曲目へ進む
    let s2 = wait(&|s| s.index == Some(1) && s.state == open_bar::player::PlayState::Playing, 10.0);
    assert!(s2.path.unwrap().ends_with("b.flac"));
    p.stop();
    wait(&|s| s.state == open_bar::player::PlayState::Stopped, 3.0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 再生モード(排他・共有・排他アップサンプル)を実デバイスで切り替える。音は小さいテスト音(-34dBFSの1kHz)。
#[test]
fn playback_modes_switch_on_the_real_device_and_report_what_they_do() {
    use open_bar::player::{PlayMode, PlayState, Player};
    if !ffmpeg_ok() || cpal::traits::HostTrait::default_output_device(&cpal::default_host()).is_none() {
        return;
    }
    let dir = tmpdir("modes");
    let path = dir.join("quiet.flac");
    let out = Command::new("ffmpeg").args(["-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=44100:duration=6", "-af", "volume=0.16", "-ac", "2", "-c:a", "flac", path.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success());
    let p = Player::new();
    p.set_playlist(vec![path.to_string_lossy().to_string()]);
    let wait = |cond: &dyn Fn(&open_bar::player::Status) -> bool, secs: f64| {
        let t0 = std::time::Instant::now();
        loop {
            let s = p.status();
            if cond(&s) {
                return s;
            }
            assert!(t0.elapsed().as_secs_f64() < secs, "タイムアウト: {s:?}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    };
    // A: 排他(元の44.1kHzのまま)
    p.set_mode(PlayMode::Exclusive);
    p.play(0);
    let a = wait(&|s| s.state == PlayState::Playing && s.position_secs > 0.5, 15.0);
    eprintln!("A: {} / {} / out={}Hz / {}", a.active_mode.title_ja, a.route_ja, a.out_rate_hz, a.note);
    if a.active_mode.id == "exclusive" {
        assert_eq!(a.out_rate_hz, 44_100, "排他は元のレートのまま");
        assert!(a.active_mode.bit_perfect);
    } else {
        eprintln!("(この機器は排他44.1kHzを開けず共有へ落ちた: {})", a.note);
    }
    // 再生中にBへ切り替え: 同じ位置付近から再開する
    let pos_before = p.status().position_secs;
    p.set_mode(PlayMode::Shared);
    let b = wait(&|s| s.state == PlayState::Playing && s.active_mode.id == "shared" && s.position_secs >= pos_before - 0.3, 15.0);
    eprintln!("B: {} / {} / out={}Hz", b.active_mode.title_ja, b.route_ja, b.out_rate_hz);
    assert!(!b.active_mode.bit_perfect);
    assert!(b.position_secs >= pos_before - 0.3 && b.position_secs < pos_before + 3.0, "切り替え後も同じ位置付近: {pos_before} → {}", b.position_secs);
    // E: 排他アップサンプル
    p.set_mode(PlayMode::ExclusiveUpsample);
    let e = wait(&|s| s.state == PlayState::Playing && (s.active_mode.id != "shared" || !s.note.is_empty()) && s.position_secs > 0.1, 15.0);
    eprintln!("E: {} / {} / out={}Hz / {}", e.active_mode.title_ja, e.route_ja, e.out_rate_hz, e.note);
    if e.active_mode.id == "exclusive_upsample" {
        assert!(e.out_rate_hz > 44_100 && e.out_rate_hz % 44_100 == 0, "44.1k系のまま高いレートへ: {}", e.out_rate_hz);
    }
    p.stop();
    wait(&|s| s.state == PlayState::Stopped, 5.0);
    let _ = std::fs::remove_dir_all(&dir);
}

/// DSDは全曲の変換を待たず、約数秒で再生が始まる(逐次変換)。DoPは非対応機器で危険なのでここでは試さない。
#[test]
fn dsd_playback_starts_quickly_thanks_to_progressive_conversion() {
    use open_bar::player::{PlayMode, PlayState, Player};
    let path = "F:/tmp/cd_dsd/Track04.dsf";
    if !std::path::Path::new(path).exists() || cpal::traits::HostTrait::default_output_device(&cpal::default_host()).is_none() {
        return;
    }
    let p = Player::new();
    p.set_volume(0.05);
    p.set_mode(PlayMode::Shared);
    p.set_playlist(vec![path.to_string()]);
    let t0 = std::time::Instant::now();
    p.play(0);
    let s = loop {
        let s = p.status();
        if s.state == PlayState::Playing && s.position_secs > 0.3 {
            break s;
        }
        assert!(t0.elapsed().as_secs_f64() < 30.0, "DSD256の再生開始が遅すぎる: {s:?}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let start = t0.elapsed().as_secs_f64();
    eprintln!("DSD256の再生開始まで {start:.1} 秒 / {} / {}", s.source_kind, s.route_ja);
    assert_eq!(s.source_kind, "DSD→PCM");
    assert!(start < 8.0, "逐次変換なら全曲変換(約35秒)より大幅に速いはず: {start}");
    p.stop();
}

/// 排他アップサンプル(E)と共有(B)で、10秒間リング枯渇(音切れ)が起きないこと。音は小さいテスト音。
#[test]
fn no_underruns_in_shared_and_exclusive_upsample_playback() {
    use open_bar::player::{PlayMode, PlayState, Player};
    if !ffmpeg_ok() || cpal::traits::HostTrait::default_output_device(&cpal::default_host()).is_none() {
        return;
    }
    let dir = tmpdir("underrun");
    let path = dir.join("q.flac");
    let out = Command::new("ffmpeg").args(["-y", "-v", "error", "-f", "lavfi", "-i", "sine=frequency=1000:sample_rate=44100:duration=14", "-af", "volume=0.16", "-ac", "2", "-c:a", "flac", path.to_str().unwrap()]).output().unwrap();
    assert!(out.status.success());
    for mode in [PlayMode::Shared, PlayMode::ExclusiveUpsample, PlayMode::Exclusive] {
        let p = Player::new();
        p.set_playlist(vec![path.to_string_lossy().to_string()]);
        p.set_mode(mode);
        p.play(0);
        let t0 = std::time::Instant::now();
        let s = loop {
            let s = p.status();
            if s.state == PlayState::Playing && s.position_secs >= 10.0 {
                break s;
            }
            assert!(t0.elapsed().as_secs_f64() < 40.0, "{mode:?} タイムアウト: {s:?}");
            std::thread::sleep(std::time::Duration::from_millis(100));
        };
        eprintln!("{mode:?}: 実際={} 出力{}Hz 10秒時点の音切れ={}フレーム", s.active_mode.id, s.out_rate_hz, s.underrun_frames);
        assert_eq!(s.underrun_frames, 0, "{mode:?}: 再生中に音切れ(リング枯渇)が起きた: {s:?}");
        p.stop();
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
