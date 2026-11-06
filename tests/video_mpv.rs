//! 実際のmpvを起動して、音声主導の同期を確かめるテスト。mpv本体が必要なので、環境変数`OPEN_BAR_MPV`に
//! `mpv`の実行ファイルのパスがあるときだけ動く(無ければ即成功)。ffmpegも必要。映像出力は`--vo=null`。

use open_bar::video::{spawn_mpv, Correction};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn real_mpv_follows_the_audio_clock_and_resyncs_after_a_jump() {
    let Ok(mpv) = std::env::var("OPEN_BAR_MPV") else { return };
    let dir = std::env::temp_dir().join(format!("open_bar_video_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let video = dir.join("v.mp4").to_string_lossy().into_owned();
    let out = Command::new("ffmpeg").args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:duration=20", "-pix_fmt", "yuv420p", "-c:v", "libx264", &video]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut m = spawn_mpv(&mpv, &video, 0.0, 0.0, 15.0, &["--vo=null"]).expect("mpvを起動できる");
    // 擬似の音声時計: 実時間で進み、途中で5秒ぶん飛ぶ(シーク相当)
    let t0 = Instant::now();
    let mut jump = 0.0;
    let mut seeks = 0;
    let mut drifts_after_settle = Vec::new();
    let mut steady = Vec::new();
    while t0.elapsed() < Duration::from_secs(8) {
        let t = t0.elapsed().as_secs_f64();
        if t > 4.0 && jump == 0.0 {
            jump = 5.0;
        }
        let audio = t + jump;
        let c = m.sync.tick(audio, true).expect("tick");
        eprintln!("t={t:.2} audio={audio:.2} drift={:?} {c:?}", m.sync.last_drift);
        if matches!(c, Correction::Seek(_)) {
            seeks += 1;
        }
        if t > 2.0 && t < 4.0 {
            if let Some(d) = m.sync.last_drift {
                steady.push(d.abs());
            }
        }
        if t > 2.0 && (t < 4.0 || t > 6.0) {
            if let Some(d) = m.sync.last_drift {
                drifts_after_settle.push(d.abs());
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    m.sync.ipc.quit();
    let _ = m.child.wait();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(seeks >= 1, "音声が飛んだらシークで追従する");
    assert!(!drifts_after_settle.is_empty());
    let worst = drifts_after_settle.iter().cloned().fold(0.0, f64::max);
    // 数値での同期の計測: 音声が飛ぶ前の安定した区間(2〜4秒)は、ずれが0.15秒未満であること(Windows実測0.014秒、WSL実測0.10秒)
    let steady_worst = steady.iter().cloned().fold(0.0, f64::max);
    eprintln!("安定区間の最大ずれ={steady_worst:.3}秒 ({}点) / 飛んだ後の最大ずれ={worst:.3}秒", steady.len());
    assert!(steady.len() >= 10, "計測点が足りない");
    assert!(steady_worst < 0.15, "安定区間のずれが大きい: {steady_worst:.3}秒");
    assert!(worst < 0.25, "落ち着いた後のずれが大きい: {worst:.3}秒");
}

/// 起動に1度失敗しても(初回は遅いことがある)、その曲のあいだ再試行して映像を開く。
/// mpvの置き場所が間違っている状態→正しい状態に直した、という失敗の再現で確かめる。
#[test]
fn follower_retries_after_a_failed_start_instead_of_giving_up() {
    use open_bar::player::{PlayState, Player};
    use open_bar::video::VideoFollower;
    let Ok(mpv) = std::env::var("OPEN_BAR_MPV") else { return };
    let dir = std::env::temp_dir().join(format!("open_bar_follow_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let video = dir.join("v.mp4").to_string_lossy().into_owned();
    let out = Command::new("ffmpeg").args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x240:rate=30:duration=20", "-pix_fmt", "yuv420p", "-c:v", "libx264", &video]).output().unwrap();
    assert!(out.status.success());
    let mut st = Player::new().status();
    st.state = PlayState::Playing;
    st.path = Some(video.clone());
    st.position_secs = 0.0;

    let mut f = VideoFollower::new("このmpvは存在しない");
    f.retry_delay = Duration::ZERO;
    f.extra_args = vec!["--vo=null".into()];
    f.step(&st, true);
    assert!(!f.is_open());
    assert!(!f.last_error.is_empty(), "失敗が表示される");
    assert_eq!(f.attempts, 1);
    // 1回の失敗であきらめない: 直ったら同じ曲のまま開く
    f.mpv_path = mpv;
    f.step(&st, true);
    assert!(f.is_open(), "再試行で開く: {}", f.last_error);
    assert!(f.last_error.is_empty());
    drop(f);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 何度やっても開けないときは、max_attempts回で見切って、その曲では開き直さない(ずっと再試行して負荷をかけない)。
#[test]
fn follower_gives_up_after_max_attempts() {
    use open_bar::player::{PlayState, Player};
    use open_bar::video::VideoFollower;
    let mut st = Player::new().status();
    st.state = PlayState::Playing;
    st.path = Some("x.mp4".into());
    let mut f = VideoFollower::new("このmpvは存在しない");
    f.retry_delay = Duration::ZERO;
    for _ in 0..10 {
        f.step(&st, true);
    }
    assert_eq!(f.attempts, f.max_attempts);
    assert!(!f.is_open());
    // 別の曲になれば、また試す
    st.path = Some("y.mp4".into());
    f.step(&st, true);
    assert_eq!(f.attempts, 1);
}
