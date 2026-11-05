//! 映像の再生(mpvを別プロセスで起動し、JSON IPCで操作する)。**音声が時間の基準**で、映像がそれに追従する。
//!
//! 音声はopen-bar自身(`player`)が鳴らす。mpvは`--no-audio`で映像だけを出し、定期的に`time-pos`を読んで、
//! 音声の位置とのずれに応じて再生速度をわずかに変える(小さなずれ)か、シークする(大きなずれ)。
//! libmpvを直接リンクしない理由: Tauriへの埋め込みは難所で、まず外部プロセスで同期を確立する方針(README)。
//! IPCの行(JSON)はmpvの`--input-ipc-server`の仕様どおり: 要求`{"command":[...],"request_id":N}`、
//! 応答`{"request_id":N,"error":"success","data":...}`、非同期イベント(`"event"`を持つ行)は読み飛ばす。

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};

/// ずれ(映像 − 音声、秒)に対する補正。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Correction {
    /// 何もしない(通常速度に戻す)。
    None,
    /// 再生速度を変える(1.0が等速)。
    Speed(f64),
    /// 音声の位置(秒)へシークする。
    Seek(f64),
}

/// これより小さいずれは気にしない(約1フレーム)。
pub const DEAD_ZONE_SECS: f64 = 0.040;
/// これより大きいずれは速度調整では追いつけないのでシークする。
pub const SEEK_THRESHOLD_SECS: f64 = 0.250;
/// 速度調整の上限(±5%。それ以上は映像の動きが不自然になる)。
pub const MAX_SPEED_DELTA: f64 = 0.05;

/// ずれから補正を決める(純粋関数)。`drift`は映像の位置 − 音声の位置(正なら映像が進みすぎ)。
/// `audio_pos`は音声の位置(シークの行き先)。
pub fn plan_correction(drift: f64, audio_pos: f64) -> Correction {
    let a = drift.abs();
    if a >= SEEK_THRESHOLD_SECS {
        Correction::Seek(audio_pos.max(0.0))
    } else if a <= DEAD_ZONE_SECS {
        Correction::None
    } else {
        // ずれ0.04〜0.25秒を、速度の変化0〜5%へ比例させる(進みすぎなら遅く、遅れているなら速く)
        let t = (a - DEAD_ZONE_SECS) / (SEEK_THRESHOLD_SECS - DEAD_ZONE_SECS);
        let delta = (0.01 + t * (MAX_SPEED_DELTA - 0.01)).min(MAX_SPEED_DELTA);
        Correction::Speed(if drift > 0.0 { 1.0 - delta } else { 1.0 + delta })
    }
}

#[derive(Debug)]
pub enum VideoError {
    Io(std::io::Error),
    Mpv(String),
    Protocol(String),
}

impl std::fmt::Display for VideoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VideoError::Io(e) => write!(f, "mpvとの通信エラー: {e}"),
            VideoError::Mpv(e) => write!(f, "mpvのエラー: {e}"),
            VideoError::Protocol(e) => write!(f, "mpvの応答が不正です: {e}"),
        }
    }
}

impl std::error::Error for VideoError {}

impl From<std::io::Error> for VideoError {
    fn from(e: std::io::Error) -> Self {
        VideoError::Io(e)
    }
}

/// mpvのJSON IPCクライアント(読み書きは何でもよい。実体はパイプ/ソケット、テストでは疑似サーバー)。
pub struct MpvIpc<R: BufRead, W: Write> {
    reader: R,
    writer: W,
    next_id: u64,
}

impl<R: BufRead, W: Write> MpvIpc<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self { reader, writer, next_id: 1 }
    }

    /// コマンドを1つ送り、同じ`request_id`の応答の`data`を返す。途中のイベント行は読み飛ばす。
    pub fn command(&mut self, args: &[Value]) -> Result<Value, VideoError> {
        let id = self.next_id;
        self.next_id += 1;
        let line = json!({ "command": args, "request_id": id }).to_string();
        self.writer.write_all(line.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        loop {
            let mut buf = String::new();
            if self.reader.read_line(&mut buf)? == 0 {
                return Err(VideoError::Protocol("mpvが接続を閉じました".into()));
            }
            let v: Value = serde_json::from_str(buf.trim()).map_err(|e| VideoError::Protocol(format!("{e}: {buf}")))?;
            if v.get("event").is_some() || v.get("request_id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            return match v.get("error").and_then(Value::as_str) {
                Some("success") | None => Ok(v.get("data").cloned().unwrap_or(Value::Null)),
                Some(e) => Err(VideoError::Mpv(e.to_string())),
            };
        }
    }

    pub fn time_pos(&mut self) -> Result<Option<f64>, VideoError> {
        match self.command(&[json!("get_property"), json!("time-pos")]) {
            Ok(v) => Ok(v.as_f64()),
            // 読み込み中などで`time-pos`がまだ無いとき
            Err(VideoError::Mpv(e)) if e.contains("unavailable") => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn set_speed(&mut self, speed: f64) -> Result<(), VideoError> {
        self.command(&[json!("set_property"), json!("speed"), json!(speed)]).map(|_| ())
    }

    pub fn set_pause(&mut self, pause: bool) -> Result<(), VideoError> {
        self.command(&[json!("set_property"), json!("pause"), json!(pause)]).map(|_| ())
    }

    pub fn seek_absolute(&mut self, secs: f64) -> Result<(), VideoError> {
        self.command(&[json!("seek"), json!(secs), json!("absolute+exact")]).map(|_| ())
    }

    /// 映像の描画範囲を窓の端からの割合(0〜1)で狭める。アプリの窓へ埋め込むとき、映像を所定の枠に収める。
    pub fn set_margins(&mut self, left: f64, right: f64, top: f64, bottom: f64) -> Result<(), VideoError> {
        for (name, v) in [("video-margin-ratio-left", left), ("video-margin-ratio-right", right), ("video-margin-ratio-top", top), ("video-margin-ratio-bottom", bottom)] {
            self.command(&[json!("set_property"), json!(name), json!(v.clamp(0.0, 0.95))])?;
        }
        Ok(())
    }

    pub fn quit(&mut self) {
        let _ = self.command(&[json!("quit")]);
    }
}

/// 映像を音声に合わせて動かす制御器。`tick`を100msほどの周期で呼ぶ。
pub struct VideoSync<R: BufRead, W: Write> {
    pub ipc: MpvIpc<R, W>,
    /// 音声を映像より何秒遅らせるか(`Combo::audio_offset_ms`/1000。正なら映像は音声より先に進める)。
    pub offset_secs: f64,
    speed: f64,
    paused: bool,
    /// 直近のずれ(映像 − 音声、秒)。表示・試験用。
    pub last_drift: Option<f64>,
    /// シーク直後はmpvの映像が少し止まる(デコードのやり直し。Linuxで0.2秒ほど)ので、その間は続けてシークしない。
    pub seek_grace: std::time::Duration,
    seeked_at: Option<std::time::Instant>,
    /// シーク後に映像が止まる秒数の見積もり(猶予明けのずれから学習)。次のシークは、その分だけ先へ飛ぶ。
    seek_lead: f64,
}

impl<R: BufRead, W: Write> VideoSync<R, W> {
    pub fn new(ipc: MpvIpc<R, W>, offset_secs: f64) -> Self {
        // mpvは`--pause`で起動するので、最初は一時停止中
        Self { ipc, offset_secs, speed: 1.0, paused: true, last_drift: None, seek_grace: std::time::Duration::from_millis(600), seeked_at: None, seek_lead: 0.0 }
    }

    /// 音声の位置(秒)と再生中かどうかから、映像を合わせる。行った補正を返す。
    pub fn tick(&mut self, audio_pos: f64, playing: bool) -> Result<Correction, VideoError> {
        let target = audio_pos + self.offset_secs;
        if playing == self.paused {
            if playing {
                // 再開の直前に音声の位置へ合わせておく(一時停止中のずれや起動時の位置を引きずらない)
                // mpvがファイルを読み込み終える前はシークが失敗する。その間は一時停止のまま次の周期で再試行する
                if self.ipc.seek_absolute(target.max(0.0)).is_err() {
                    return Ok(Correction::None);
                }
            }
            self.ipc.set_pause(!playing)?;
            self.paused = !playing;
        }
        if !playing {
            // 一時停止中は位置だけ合わせる(シーク中の一時停止にも追従する)
            if let Some(v) = self.ipc.time_pos()? {
                if (v - target).abs() > DEAD_ZONE_SECS {
                    self.ipc.seek_absolute(target.max(0.0))?;
                }
            }
            return Ok(Correction::None);
        }
        let Some(v) = self.ipc.time_pos()? else { return Ok(Correction::None) };
        let drift = v - target;
        self.last_drift = Some(drift);
        let mut c = plan_correction(drift, target);
        if let Some(t0) = self.seeked_at {
            if t0.elapsed() < self.seek_grace {
                // 猶予中: シークの連打を避け、速度補正だけにとどめる
                if matches!(c, Correction::Seek(_)) {
                    c = Correction::Speed(1.0 + MAX_SPEED_DELTA);
                }
            } else {
                // 猶予明けの最初のずれから、シーク後の停止ぶんを学習する
                self.seek_lead = (-drift).clamp(0.0, 1.0);
                self.seeked_at = None;
                c = plan_correction(drift, target);
            }
        }
        if let Correction::Seek(t) = c {
            c = Correction::Seek(t + self.seek_lead);
        }
        match c {
            Correction::None => {
                if (self.speed - 1.0).abs() > f64::EPSILON {
                    self.ipc.set_speed(1.0)?;
                    self.speed = 1.0;
                }
            }
            Correction::Speed(s) => {
                self.ipc.set_speed(s)?;
                self.speed = s;
            }
            Correction::Seek(t) => {
                self.ipc.seek_absolute(t)?;
                self.seeked_at = Some(std::time::Instant::now());
                if (self.speed - 1.0).abs() > f64::EPSILON {
                    self.ipc.set_speed(1.0)?;
                    self.speed = 1.0;
                }
            }
        }
        Ok(c)
    }
}

/// mpvを起動する引数(映像だけ。音声はopen-barが鳴らすので`--no-audio`)。`ipc_path`はパイプ名/ソケットのパス。
pub fn mpv_args(video: &str, ipc_path: &str, start_secs: f64) -> Vec<String> {
    vec![
        "--no-audio".into(),
        "--pause".into(),
        "--idle=no".into(),
        "--keep-open=yes".into(),
        "--force-window=yes".into(),
        "--no-terminal".into(),
        "--hwdec=auto-safe".into(),
        "--osc=no".into(),
        format!("--input-ipc-server={ipc_path}"),
        format!("--start={start_secs}"),
        video.into(),
    ]
}

/// このプロセス用のIPC名(Windowsは名前付きパイプ、それ以外はtempのソケット)。
pub fn ipc_name() -> String {
    let id = std::process::id();
    if cfg!(windows) {
        format!(r"\\.\pipe\open-bar-mpv-{id}")
    } else {
        std::env::temp_dir().join(format!("open-bar-mpv-{id}.sock")).to_string_lossy().into_owned()
    }
}

/// 起動したmpvとそのIPC接続。
pub struct Mpv {
    pub child: std::process::Child,
    pub sync: VideoSync<BufReader<Box<dyn std::io::Read + Send>>, Box<dyn Write + Send>>,
}

/// mpvを起動して接続する(IPCが開くまで最大`wait_secs`秒待つ)。`mpv_path`は`mpv`の実行ファイル。
pub fn spawn_mpv(mpv_path: &str, video: &str, start_secs: f64, offset_secs: f64, wait_secs: f64, extra_args: &[&str]) -> Result<Mpv, VideoError> {
    let ipc = ipc_name();
    let mut child = std::process::Command::new(mpv_path).args(extra_args).args(mpv_args(video, &ipc, start_secs)).stdin(std::process::Stdio::null()).spawn()?;
    let t0 = std::time::Instant::now();
    loop {
        match connect(&ipc) {
            Ok((r, w)) => {
                let sync = VideoSync::new(MpvIpc::new(BufReader::new(r), w), offset_secs);
                return Ok(Mpv { child, sync });
            }
            Err(e) => {
                if let Ok(Some(status)) = child.try_wait() {
                    return Err(VideoError::Mpv(format!("mpvが終了しました({status})")));
                }
                if t0.elapsed().as_secs_f64() > wait_secs {
                    let _ = child.kill();
                    return Err(VideoError::Io(e));
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

type Conn = (Box<dyn std::io::Read + Send>, Box<dyn Write + Send>);

#[cfg(windows)]
fn connect(path: &str) -> std::io::Result<Conn> {
    // 名前付きパイプは通常のファイルとして開ける(読み書き両用)
    let f = std::fs::OpenOptions::new().read(true).write(true).open(path)?;
    let w = f.try_clone()?;
    Ok((Box::new(f), Box::new(w)))
}

#[cfg(unix)]
fn connect(path: &str) -> std::io::Result<Conn> {
    let s = std::os::unix::net::UnixStream::connect(path)?;
    let w = s.try_clone()?;
    Ok((Box::new(s), Box::new(w)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn small_drift_is_ignored_and_medium_drift_changes_speed_toward_the_audio() {
        assert_eq!(plan_correction(0.0, 10.0), Correction::None);
        assert_eq!(plan_correction(-0.039, 10.0), Correction::None);
        // 映像が進みすぎ → 遅く / 遅れている → 速く
        match plan_correction(0.2, 10.0) {
            Correction::Speed(s) => assert!(s < 1.0 && s >= 1.0 - MAX_SPEED_DELTA, "{s}"),
            c => panic!("{c:?}"),
        }
        match plan_correction(-0.2, 10.0) {
            Correction::Speed(s) => assert!(s > 1.0 && s <= 1.0 + MAX_SPEED_DELTA, "{s}"),
            c => panic!("{c:?}"),
        }
        // ずれが大きいほど補正も大きい(単調)
        let sp = |d: f64| match plan_correction(d, 0.0) {
            Correction::Speed(s) => s,
            _ => 1.0,
        };
        assert!(sp(-0.2) > sp(-0.1));
        // 大きなずれはシーク(行き先は音声の位置、負にはならない)
        assert_eq!(plan_correction(0.25, 12.5), Correction::Seek(12.5));
        assert_eq!(plan_correction(-3.0, 7.0), Correction::Seek(7.0));
        assert_eq!(plan_correction(3.0, -1.0), Correction::Seek(0.0));
    }

    /// 疑似mpv: 要求ごとに、先にイベント行を1つ挟んでから応答を返す。
    fn fake_mpv_replies(time_pos: &[Option<f64>], requests: usize) -> Vec<u8> {
        let mut out = String::new();
        let mut tp = time_pos.iter();
        for id in 1..=requests {
            out.push_str("{\"event\":\"tick\"}\n");
            match tp.next() {
                Some(Some(v)) => out.push_str(&format!("{{\"request_id\":{id},\"error\":\"success\",\"data\":{v}}}\n")),
                Some(None) => out.push_str(&format!("{{\"request_id\":{id},\"error\":\"property unavailable\"}}\n")),
                None => out.push_str(&format!("{{\"request_id\":{id},\"error\":\"success\"}}\n")),
            }
        }
        out.into_bytes()
    }

    fn sent_commands(buf: &[u8]) -> Vec<Value> {
        String::from_utf8_lossy(buf).lines().map(|l| serde_json::from_str::<Value>(l).unwrap()["command"].clone()).collect()
    }

    #[test]
    fn ipc_skips_events_and_maps_errors() {
        let replies = fake_mpv_replies(&[Some(3.25), None], 2);
        let mut sent = Vec::new();
        let mut ipc = MpvIpc::new(Cursor::new(replies), &mut sent);
        assert_eq!(ipc.time_pos().unwrap(), Some(3.25));
        assert_eq!(ipc.time_pos().unwrap(), None, "読み込み中の`unavailable`は値なし");
        assert_eq!(sent_commands(&sent)[0], json!(["get_property", "time-pos"]));
    }

    #[test]
    fn tick_pauses_follows_the_audio_and_corrects_drift() {
        // 再生開始: 音声の位置へseek → pause解除 → time-pos(映像が音声より0.2秒進んでいる) → speed設定
        let all = "{\"request_id\":1,\"error\":\"success\"}
{\"request_id\":2,\"error\":\"success\"}
{\"request_id\":3,\"error\":\"success\",\"data\":10.2}
{\"request_id\":4,\"error\":\"success\"}
";
        let mut sent = Vec::new();
        let mut v = VideoSync::new(MpvIpc::new(Cursor::new(all.as_bytes().to_vec()), &mut sent), 0.0);
        let c = v.tick(10.0, true).unwrap();
        assert!(matches!(c, Correction::Speed(s) if s < 1.0), "{c:?}");
        assert!((v.last_drift.unwrap() - 0.2).abs() < 1e-9);
        let cmds = sent_commands(&sent);
        assert_eq!(cmds[0], json!(["seek", 10.0, "absolute+exact"]));
        assert_eq!(cmds[1], json!(["set_property", "pause", false]));
        assert_eq!(cmds[2], json!(["get_property", "time-pos"]));
        assert_eq!(cmds[3][1], json!("speed"));
    }

    #[test]
    fn tick_seeks_on_a_big_drift_and_applies_the_audio_offset() {
        // オフセット+0.5秒: 目標は音声10.0+0.5=10.5。映像は3.0なので大きくずれている → シーク
        let all = "{\"request_id\":1,\"error\":\"success\",\"data\":3.0}\n{\"request_id\":2,\"error\":\"success\"}\n";
        let mut sent = Vec::new();
        let mut v = VideoSync::new(MpvIpc::new(Cursor::new(all.as_bytes().to_vec()), &mut sent), 0.5);
        v.paused = false; // すでに再生中
        let c = v.tick(10.0, true).unwrap();
        assert_eq!(c, Correction::Seek(10.5));
        assert_eq!(sent_commands(&sent)[1], json!(["seek", 10.5, "absolute+exact"]));
    }

    #[test]
    fn mpv_args_are_video_only_and_use_the_ipc_server() {
        let a = mpv_args("a.mp4", "PIPE", 1.5);
        assert!(a.contains(&"--no-audio".to_string()));
        assert!(a.contains(&"--input-ipc-server=PIPE".to_string()));
        assert!(a.contains(&"--start=1.5".to_string()));
        assert_eq!(a.last().unwrap(), "a.mp4");
    }
}

/// プレーヤーの再生に映像ウィンドウ(mpv)を追従させる。GUI・CLI共通。
/// 再生中の曲が映像コンテナ(MP4/MKV…)のときだけmpvを開き、曲が変わる・止まる・映像でなくなると閉じる。
/// ユーザーがmpvのウィンドウを閉じたら、その曲の間は開き直さない。
pub struct VideoFollower {
    mpv_path: String,
    current: Option<(String, Mpv)>,
    dismissed: Option<String>,
    /// mpvへ足す引数(例: `--wid=<ウィンドウID>`で別窓でなくアプリの窓へ描画させる)。
    pub extra_args: Vec<String>,
    /// 埋め込み時の描画範囲(左・右・上・下の余白の割合)。変わったらmpvへ送る。
    pub margins: [f64; 4],
    applied_margins: [f64; 4],
    pub last_error: String,
}

impl VideoFollower {
    pub fn new(mpv_path: impl Into<String>) -> Self {
        Self { mpv_path: mpv_path.into(), current: None, dismissed: None, extra_args: Vec::new(), margins: [0.0; 4], applied_margins: [0.0; 4], last_error: String::new() }
    }

    /// 映像ウィンドウが開いているか。
    pub fn is_open(&self) -> bool {
        self.current.is_some()
    }

    fn close(&mut self) {
        if let Some((_, mut m)) = self.current.take() {
            m.sync.ipc.quit();
            let _ = m.child.wait();
        }
    }

    /// 100ms周期などで呼ぶ。`enabled`がfalseなら映像を閉じる。
    pub fn step(&mut self, st: &crate::player::Status, enabled: bool) {
        use crate::player::PlayState;
        let want = match (&st.path, st.state, enabled) {
            (Some(p), PlayState::Playing | PlayState::Paused, true) if crate::media::probe_kind_only(p) == crate::media::MediaKind::VideoContainer => Some(p.clone()),
            _ => None,
        };
        if want.as_ref() != self.dismissed.as_ref() {
            self.dismissed = None;
        }
        if self.current.as_ref().map(|(p, _)| p) != want.as_ref() {
            self.close();
        }
        let Some(path) = want else { return };
        if self.current.is_none() && self.dismissed.as_deref() != Some(path.as_str()) {
            match {
                let extra: Vec<&str> = self.extra_args.iter().map(String::as_str).collect();
                spawn_mpv(&self.mpv_path, &path, st.position_secs, 0.0, 10.0, &extra)
            } {
                Ok(m) => {
                    self.applied_margins = [0.0; 4];
                    self.current = Some((path.clone(), m));
                    self.last_error.clear();
                }
                Err(e) => {
                    self.last_error = format!("{e}(mpvのパスは環境変数 OPEN_BAR_MPV で指定 / set OPEN_BAR_MPV)");
                    self.dismissed = Some(path);
                    return;
                }
            }
        }
        if let Some((_, m)) = self.current.as_mut() {
            if self.margins != self.applied_margins {
                let g = self.margins;
                if m.sync.ipc.set_margins(g[0], g[1], g[2], g[3]).is_ok() {
                    self.applied_margins = g;
                }
            }
        }
        if let Some((p, m)) = self.current.as_mut() {
            if m.sync.tick(st.position_secs, st.state == PlayState::Playing).is_err() {
                // mpvが閉じられた(ユーザーがウィンドウを閉じた等)
                let p = p.clone();
                self.close();
                self.dismissed = Some(p);
            }
        }
    }
}

impl Drop for VideoFollower {
    fn drop(&mut self) {
        self.close();
    }
}
