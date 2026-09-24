//! 対話的なプレーヤー(再生・一時停止・停止・シーク・音量・プレイリスト自動送り)。UIから使う。
//!
//! 現状は**共有モード**(cpal)のみ。トラックごとにデコード→デバイスのレートへ変換→ストリームを開く方式で、
//! 曲の切り替わりに短い隙間(数十ms)が入る(ギャップレス連結は`playlist::load_gapless`のCLI再生のみ)。
//! 排他モード/DoPのUI連携と、切り替わりの隙間の解消は次の段階。

use crate::output::{map_channels_pub, resample, OutputError};
use crate::pcm::{decode_file, Pcm};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayState {
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub state: PlayState,
    pub index: Option<usize>,
    pub path: Option<String>,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub device: String,
    pub device_rate_hz: u32,
    pub source_rate_hz: u32,
    pub source_kind: String,
    pub volume: f32,
    /// 直近のエラーや注意(なければ空)。
    pub message: String,
}

enum Cmd {
    SetPlaylist(Vec<String>),
    Play(usize),
    Pause,
    Resume,
    Stop,
    Seek(f64),
    Next,
    Prev,
    Quit,
}

struct Track {
    pos: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    ended: Arc<AtomicBool>,
    total_frames: u64,
    rate: u32,
}

struct Shared {
    status: Status,
    track: Option<Track>,
}

pub struct Player {
    tx: Sender<Cmd>,
    shared: Arc<Mutex<Shared>>,
    volume: Arc<AtomicU32>,
}

impl Player {
    pub fn new() -> Player {
        let (tx, rx) = channel::<Cmd>();
        let volume = Arc::new(AtomicU32::new(0.8f32.to_bits()));
        let shared = Arc::new(Mutex::new(Shared {
            status: Status { state: PlayState::Stopped, index: None, path: None, position_secs: 0.0, duration_secs: 0.0, device: String::new(), device_rate_hz: 0, source_rate_hz: 0, source_kind: String::new(), volume: 0.8, message: String::new() },
            track: None,
        }));
        let (sh, vol) = (shared.clone(), volume.clone());
        std::thread::spawn(move || worker(rx, sh, vol));
        Player { tx, shared, volume }
    }

    pub fn set_playlist(&self, files: Vec<String>) {
        let _ = self.tx.send(Cmd::SetPlaylist(files));
    }
    pub fn play(&self, index: usize) {
        let _ = self.tx.send(Cmd::Play(index));
    }
    pub fn pause(&self) {
        let _ = self.tx.send(Cmd::Pause);
    }
    pub fn resume(&self) {
        let _ = self.tx.send(Cmd::Resume);
    }
    pub fn stop(&self) {
        let _ = self.tx.send(Cmd::Stop);
    }
    pub fn seek(&self, secs: f64) {
        let _ = self.tx.send(Cmd::Seek(secs));
    }
    pub fn next(&self) {
        let _ = self.tx.send(Cmd::Next);
    }
    pub fn prev(&self) {
        let _ = self.tx.send(Cmd::Prev);
    }
    pub fn set_volume(&self, v: f32) {
        self.volume.store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    /// 現在の状態(位置は再生中のストリームから読む)。
    pub fn status(&self) -> Status {
        let g = self.shared.lock().unwrap();
        let mut s = g.status.clone();
        if let Some(t) = &g.track {
            s.position_secs = (t.pos.load(Ordering::Relaxed).min(t.total_frames)) as f64 / t.rate.max(1) as f64;
        }
        s.volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
        s
    }
}

impl Default for Player {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Quit);
    }
}

fn set_msg(shared: &Arc<Mutex<Shared>>, m: impl Into<String>) {
    shared.lock().unwrap().status.message = m.into();
}

/// ファイルをデコードして、共有モードのデバイスで鳴らせる形(デバイスのレート・チャンネル数)へ整える。
fn prepare(path: &str, dev_rate: u32, dev_ch: usize) -> Result<(Vec<f32>, f64, u32, String), String> {
    let info = crate::media::probe(path);
    let (pcm, kind): (Pcm, &str) = if info.kind == crate::media::MediaKind::Dsd {
        let s = open_mqa_dsd::read_dsd_file(path).map_err(|e| e.to_string())?;
        // 共有モードはDoP不可。機器が受けやすい176.4kHz以下のPCMへ変換する
        let rate = crate::plan::dsd_pcm_candidates(s.rate_hz).into_iter().find(|r| *r <= 176_400).unwrap_or(44_100);
        let (v, r) = open_mqa_dsd::dsd_to_pcm(&s, open_mqa_dsd::DsdToPcm { out_rate_hz: rate, cutoff_hz: 40_000.0 }).map_err(|e| e.to_string())?;
        (Pcm { sample_rate: r, channels: s.channels.len(), samples: v, bits_per_sample: None }, "DSD→PCM")
    } else {
        (decode_file(path).map_err(|e| e.to_string())?, if info.is_mqa { "MQA(通常のPCMとして)" } else { "PCM" })
    };
    let duration = pcm.duration_secs();
    let src_rate = pcm.sample_rate;
    let samples = resample(&pcm.samples, pcm.channels, pcm.sample_rate, dev_rate).map_err(|e: OutputError| e.to_string())?;
    let samples = map_channels_pub(&samples, pcm.channels, dev_ch);
    Ok((samples, duration, src_rate, kind.to_string()))
}

fn build<T>(device: &cpal::Device, config: &StreamConfig, data: Arc<Vec<f32>>, ch: usize, track: &Track, volume: Arc<AtomicU32>) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let (pos, paused, ended, total) = (track.pos.clone(), track.paused.clone(), track.ended.clone(), track.total_frames);
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _| {
                let n = out.len() / ch;
                if paused.load(Ordering::Relaxed) {
                    out.iter_mut().for_each(|o| *o = T::from_sample(0.0));
                    return;
                }
                let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                let start = pos.load(Ordering::Relaxed) as usize;
                for f in 0..n {
                    for c in 0..ch {
                        let i = (start + f) * ch + c;
                        out[f * ch + c] = T::from_sample(if i < data.len() { data[i] * vol } else { 0.0 });
                    }
                }
                let np = (start + n) as u64;
                pos.store(np, Ordering::Relaxed);
                if np >= total {
                    ended.store(true, Ordering::Relaxed);
                }
            },
            |e| eprintln!("出力ストリームのエラー: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

fn worker(rx: std::sync::mpsc::Receiver<Cmd>, shared: Arc<Mutex<Shared>>, volume: Arc<AtomicU32>) {
    let mut playlist: Vec<String> = Vec::new();
    let mut stream: Option<cpal::Stream> = None;
    let mut current: Option<usize> = None;
    let host = cpal::default_host();
    loop {
        // トラックの自然終了を見て自動送り
        let ended = shared.lock().unwrap().track.as_ref().is_some_and(|t| t.ended.load(Ordering::Relaxed));
        let cmd = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let cmd = match (cmd, ended) {
            (Some(c), _) => Some(c),
            (None, true) => match current {
                Some(i) if i + 1 < playlist.len() => Some(Cmd::Play(i + 1)),
                _ => Some(Cmd::Stop),
            },
            (None, false) => None,
        };
        let Some(cmd) = cmd else { continue };
        match cmd {
            Cmd::Quit => return,
            Cmd::SetPlaylist(v) => playlist = v,
            Cmd::Stop => {
                stream = None;
                let mut g = shared.lock().unwrap();
                g.track = None;
                g.status.state = PlayState::Stopped;
                g.status.position_secs = 0.0;
            }
            Cmd::Pause => {
                if let Some(t) = &shared.lock().unwrap().track {
                    t.paused.store(true, Ordering::Relaxed);
                }
                let mut g = shared.lock().unwrap();
                if g.track.is_some() {
                    g.status.state = PlayState::Paused;
                }
            }
            Cmd::Resume => {
                if let Some(t) = &shared.lock().unwrap().track {
                    t.paused.store(false, Ordering::Relaxed);
                }
                let mut g = shared.lock().unwrap();
                if g.track.is_some() {
                    g.status.state = PlayState::Playing;
                }
            }
            Cmd::Seek(secs) => {
                if let Some(t) = &shared.lock().unwrap().track {
                    let frame = ((secs.max(0.0) * t.rate as f64) as u64).min(t.total_frames.saturating_sub(1));
                    t.pos.store(frame, Ordering::Relaxed);
                }
            }
            Cmd::Next => {
                if let Some(i) = current {
                    if i + 1 < playlist.len() {
                        current = Some(i + 1);
                        start_track(&host, &shared, &volume, &playlist, i + 1, &mut stream);
                    }
                }
            }
            Cmd::Prev => {
                if let Some(i) = current {
                    let j = i.saturating_sub(1);
                    current = Some(j);
                    start_track(&host, &shared, &volume, &playlist, j, &mut stream);
                }
            }
            Cmd::Play(i) => {
                if i < playlist.len() {
                    current = Some(i);
                    start_track(&host, &shared, &volume, &playlist, i, &mut stream);
                }
            }
        }
    }
}

fn start_track(host: &cpal::Host, shared: &Arc<Mutex<Shared>>, volume: &Arc<AtomicU32>, playlist: &[String], i: usize, stream: &mut Option<cpal::Stream>) {
    *stream = None;
    {
        let mut g = shared.lock().unwrap();
        g.track = None;
        g.status.state = PlayState::Stopped;
    }
    let path = &playlist[i];
    let Some(device) = host.default_output_device() else {
        set_msg(shared, "出力デバイスがありません");
        return;
    };
    let default = match device.default_output_config() {
        Ok(c) => c,
        Err(e) => {
            set_msg(shared, format!("デバイスの設定を取得できません: {e}"));
            return;
        }
    };
    let (dev_rate, dev_ch) = (default.sample_rate().0, default.channels() as usize);
    set_msg(shared, "読み込み中...");
    let (samples, duration, src_rate, kind) = match prepare(path, dev_rate, dev_ch) {
        Ok(x) => x,
        Err(e) => {
            set_msg(shared, format!("再生できません({path}): {e}"));
            return;
        }
    };
    let total = (samples.len() / dev_ch) as u64;
    let track = Track { pos: Arc::new(AtomicU64::new(0)), paused: Arc::new(AtomicBool::new(false)), ended: Arc::new(AtomicBool::new(false)), total_frames: total, rate: dev_rate };
    let config = StreamConfig { channels: dev_ch as u16, sample_rate: cpal::SampleRate(dev_rate), buffer_size: cpal::BufferSize::Default };
    let data = Arc::new(samples);
    let built = match default.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config, data, dev_ch, &track, volume.clone()),
        SampleFormat::I16 => build::<i16>(&device, &config, data, dev_ch, &track, volume.clone()),
        SampleFormat::I32 => build::<i32>(&device, &config, data, dev_ch, &track, volume.clone()),
        other => Err(format!("未対応のデバイス形式: {other:?}")),
    };
    match built {
        Ok(s) => {
            if let Err(e) = s.play() {
                set_msg(shared, format!("再生を開始できません: {e}"));
                return;
            }
            *stream = Some(s);
            let mut g = shared.lock().unwrap();
            g.status.state = PlayState::Playing;
            g.status.index = Some(i);
            g.status.path = Some(path.clone());
            g.status.duration_secs = duration;
            g.status.device = device.name().unwrap_or_default();
            g.status.device_rate_hz = dev_rate;
            g.status.source_rate_hz = src_rate;
            g.status.source_kind = kind;
            g.status.message = String::new();
            g.track = Some(track);
        }
        Err(e) => set_msg(shared, format!("ストリームを開けません: {e}")),
    }
}
