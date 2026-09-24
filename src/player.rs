//! 対話的なプレーヤー(再生・一時停止・停止・シーク・音量・プレイリスト自動送り)。UIから使う。
//!
//! 構成: ワーカースレッドがコマンドを受け、トラックごとに「デコード→(別スレッドで)デバイスのレートへ逐次リサンプル→
//! リングバッファ→cpalのコールバック」で鳴らす。リサンプルを先読みの逐次処理にしたので、384kHzのように高いデバイスレートでも
//! 再生は約1秒で始まり、メモリはリング(約8秒分)だけ。シークはリサンプラを作り直して元PCMの位置から再開する。
//!
//! 現状は**共有モード**のみ。曲の切り替わりに短い隙間(数十ms)が入る。排他モード/DoPのUI連携・DSDの逐次変換は次の段階。

use crate::output::map_channels_pub;
use crate::pcm::{decode_file, Pcm};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use serde::Serialize;
use std::collections::VecDeque;
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

/// 1トラック分の再生状態(コールバックと生産スレッドで共有)。
struct Track {
    ring: Mutex<VecDeque<f32>>,
    paused: AtomicBool,
    ended: AtomicBool,
    /// 生産スレッドの停止要求(トラック終了・停止・切り替え時)。
    stop: AtomicBool,
    /// 元PCMを最後まで読み終えたか。
    producer_done: AtomicBool,
    /// シークの世代番号。増えたら生産側はリサンプラを作り直して`seek_src_frame`から再開する。
    seek_gen: AtomicU64,
    seek_src_frame: AtomicU64,
    /// 現在の世代でコールバックが消費したデバイスフレーム数。
    consumed: AtomicU64,
    /// 現在の世代の開始位置(デバイスフレーム)。位置 = (base + consumed) / dev_rate。
    base: AtomicU64,
    dev_rate: u32,
    dev_ch: usize,
    total_secs: f64,
}

struct Shared {
    status: Status,
    track: Option<Arc<Track>>,
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

    /// 現在の状態(位置は消費したフレーム数から計算する)。
    pub fn status(&self) -> Status {
        let g = self.shared.lock().unwrap();
        let mut s = g.status.clone();
        if let Some(t) = &g.track {
            let frames = t.base.load(Ordering::Relaxed) + t.consumed.load(Ordering::Relaxed);
            s.position_secs = (frames as f64 / t.dev_rate.max(1) as f64).min(t.total_secs);
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

/// ファイルをデコードして元のレートのPCMにする(DSDは共有モード向けにPCMへ変換)。
fn decode_source(path: &str) -> Result<(Pcm, String), String> {
    let info = crate::media::probe(path);
    if info.kind == crate::media::MediaKind::Dsd {
        let s = open_mqa_dsd::read_dsd_file(path).map_err(|e| e.to_string())?;
        // 共有モードはDoP不可。機器が受けやすい176.4kHz以下のPCMへ変換する
        let rate = crate::plan::dsd_pcm_candidates(s.rate_hz).into_iter().find(|r| *r <= 176_400).unwrap_or(44_100);
        let (v, r) = open_mqa_dsd::dsd_to_pcm(&s, open_mqa_dsd::DsdToPcm { out_rate_hz: rate, cutoff_hz: 40_000.0 }).map_err(|e| e.to_string())?;
        Ok((Pcm { sample_rate: r, channels: s.channels.len(), samples: v, bits_per_sample: None }, "DSD→PCM".to_string()))
    } else {
        let kind = if info.is_mqa { "MQA(通常のPCMとして)" } else { "PCM" };
        Ok((decode_file(path).map_err(|e| e.to_string())?, kind.to_string()))
    }
}

const RING_SECS: f64 = 8.0;
const CHUNK: usize = 2048;

fn make_resampler(from: u32, to: u32, ch: usize) -> Option<SincFixedIn<f32>> {
    if from == to {
        return None;
    }
    let params = SincInterpolationParameters { sinc_len: 256, f_cutoff: 0.95, interpolation: SincInterpolationType::Cubic, oversampling_factor: 256, window: WindowFunction::BlackmanHarris2 };
    SincFixedIn::<f32>::new(to as f64 / from as f64, 2.0, params, CHUNK, ch).ok()
}

/// 生産スレッド: 元PCMを`CHUNK`フレームずつリサンプル→チャンネル変換→リングへ。リングが満杯なら待つ。
fn producer(track: Arc<Track>, src: Arc<Pcm>) {
    let ch = src.channels;
    let src_frames = (src.samples.len() / ch) as u64;
    let mut gen_seen = track.seek_gen.load(Ordering::Acquire);
    let mut pos: u64 = 0;
    let mut rs = make_resampler(src.sample_rate, track.dev_rate, ch);
    let capacity = (RING_SECS * track.dev_rate as f64) as usize * track.dev_ch;
    // リサンプラの遅延を吐き出して末尾まで届けるための残りフレーム数
    loop {
        if track.stop.load(Ordering::Relaxed) {
            return;
        }
        let g = track.seek_gen.load(Ordering::Acquire);
        if g != gen_seen {
            gen_seen = g;
            pos = track.seek_src_frame.load(Ordering::Acquire).min(src_frames);
            rs = make_resampler(src.sample_rate, track.dev_rate, ch);
            track.producer_done.store(false, Ordering::Release);
            let mut ring = track.ring.lock().unwrap();
            ring.clear();
            track.consumed.store(0, Ordering::Release);
            track.base.store((pos as f64 * track.dev_rate as f64 / src.sample_rate as f64) as u64, Ordering::Release);
        }
        if pos >= src_frames {
            track.producer_done.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_millis(20));
            continue;
        }
        if track.ring.lock().unwrap().len() >= capacity {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        let n = (CHUNK as u64).min(src_frames - pos) as usize;
        let block = &src.samples[pos as usize * ch..(pos as usize + n) * ch];
        let out: Vec<f32> = match rs.as_mut() {
            None => map_channels_pub(block, ch, track.dev_ch),
            Some(r) => {
                let planar: Vec<Vec<f32>> = (0..ch).map(|c| block.iter().skip(c).step_by(ch).copied().collect()).collect();
                let refs: Vec<&[f32]> = planar.iter().map(|v| v.as_slice()).collect();
                let res = if n == CHUNK { r.process(&refs, None) } else { r.process_partial(Some(&refs), None) };
                match res {
                    Ok(o) => {
                        let frames = o[0].len();
                        let mut inter = Vec::with_capacity(frames * ch);
                        for i in 0..frames {
                            for c in &o {
                                inter.push(c[i]);
                            }
                        }
                        map_channels_pub(&inter, ch, track.dev_ch)
                    }
                    Err(_) => Vec::new(),
                }
            }
        };
        pos += n as u64;
        // シークが入っていたら、この塊は古い位置のものなので捨てる
        if track.seek_gen.load(Ordering::Acquire) != gen_seen {
            continue;
        }
        track.ring.lock().unwrap().extend(out);
    }
}

fn build<T>(device: &cpal::Device, config: &StreamConfig, track: Arc<Track>, volume: Arc<AtomicU32>) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let ch = track.dev_ch;
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _| {
                let n = out.len() / ch;
                let silent = |out: &mut [T]| out.iter_mut().for_each(|o| *o = T::from_sample(0.0));
                if track.paused.load(Ordering::Relaxed) {
                    silent(out);
                    return;
                }
                let vol = f32::from_bits(volume.load(Ordering::Relaxed));
                let Ok(mut ring) = track.ring.try_lock() else {
                    silent(out); // 生産側がリングを更新中(ごく短時間)
                    return;
                };
                let avail = ring.len() / ch;
                let take = n.min(avail);
                for f in 0..take {
                    for c in 0..ch {
                        out[f * ch + c] = T::from_sample(ring.pop_front().unwrap_or(0.0) * vol);
                    }
                }
                for o in out[take * ch..].iter_mut() {
                    *o = T::from_sample(0.0);
                }
                track.consumed.fetch_add(take as u64, Ordering::Relaxed);
                if take < n && ring.is_empty() && track.producer_done.load(Ordering::Acquire) {
                    track.ended.store(true, Ordering::Release);
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
    let mut src_holder: Option<Arc<Pcm>> = None;
    let host = cpal::default_host();
    loop {
        let ended = shared.lock().unwrap().track.as_ref().is_some_and(|t| t.ended.load(Ordering::Acquire));
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
            Cmd::Quit => {
                stop_track(&shared, &mut stream);
                return;
            }
            Cmd::SetPlaylist(v) => playlist = v,
            Cmd::Stop => {
                stop_track(&shared, &mut stream);
                shared.lock().unwrap().status.state = PlayState::Stopped;
            }
            Cmd::Pause => {
                let mut g = shared.lock().unwrap();
                if let Some(t) = &g.track {
                    t.paused.store(true, Ordering::Relaxed);
                    g.status.state = PlayState::Paused;
                }
            }
            Cmd::Resume => {
                let mut g = shared.lock().unwrap();
                if let Some(t) = &g.track {
                    t.paused.store(false, Ordering::Relaxed);
                    g.status.state = PlayState::Playing;
                }
            }
            Cmd::Seek(secs) => {
                let g = shared.lock().unwrap();
                if let (Some(t), Some(src)) = (&g.track, &src_holder) {
                    let frame = ((secs.max(0.0) * src.sample_rate as f64) as u64).min((src.samples.len() / src.channels).saturating_sub(1) as u64);
                    t.seek_src_frame.store(frame, Ordering::Release);
                    t.seek_gen.fetch_add(1, Ordering::AcqRel);
                }
            }
            Cmd::Next => {
                if let Some(i) = current {
                    if i + 1 < playlist.len() {
                        current = Some(i + 1);
                        start_track(&host, &shared, &volume, &playlist, i + 1, &mut stream, &mut src_holder);
                    }
                }
            }
            Cmd::Prev => {
                if let Some(i) = current {
                    let j = i.saturating_sub(1);
                    current = Some(j);
                    start_track(&host, &shared, &volume, &playlist, j, &mut stream, &mut src_holder);
                }
            }
            Cmd::Play(i) => {
                if i < playlist.len() {
                    current = Some(i);
                    start_track(&host, &shared, &volume, &playlist, i, &mut stream, &mut src_holder);
                }
            }
        }
    }
}

fn stop_track(shared: &Arc<Mutex<Shared>>, stream: &mut Option<cpal::Stream>) {
    *stream = None;
    let mut g = shared.lock().unwrap();
    if let Some(t) = g.track.take() {
        t.stop.store(true, Ordering::Relaxed);
    }
    g.status.position_secs = 0.0;
}

fn start_track(host: &cpal::Host, shared: &Arc<Mutex<Shared>>, volume: &Arc<AtomicU32>, playlist: &[String], i: usize, stream: &mut Option<cpal::Stream>, src_holder: &mut Option<Arc<Pcm>>) {
    stop_track(shared, stream);
    shared.lock().unwrap().status.state = PlayState::Stopped;
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
    let (src, kind) = match decode_source(path) {
        Ok(x) => x,
        Err(e) => {
            set_msg(shared, format!("再生できません({path}): {e}"));
            return;
        }
    };
    let src = Arc::new(src);
    let track = Arc::new(Track {
        ring: Mutex::new(VecDeque::new()),
        paused: AtomicBool::new(false),
        ended: AtomicBool::new(false),
        stop: AtomicBool::new(false),
        producer_done: AtomicBool::new(false),
        seek_gen: AtomicU64::new(0),
        seek_src_frame: AtomicU64::new(0),
        consumed: AtomicU64::new(0),
        base: AtomicU64::new(0),
        dev_rate,
        dev_ch,
        total_secs: src.duration_secs(),
    });
    let (t2, s2) = (track.clone(), src.clone());
    std::thread::spawn(move || producer(t2, s2));
    let config = StreamConfig { channels: dev_ch as u16, sample_rate: cpal::SampleRate(dev_rate), buffer_size: cpal::BufferSize::Default };
    let built = match default.sample_format() {
        SampleFormat::F32 => build::<f32>(&device, &config, track.clone(), volume.clone()),
        SampleFormat::I16 => build::<i16>(&device, &config, track.clone(), volume.clone()),
        SampleFormat::I32 => build::<i32>(&device, &config, track.clone(), volume.clone()),
        other => Err(format!("未対応のデバイス形式: {other:?}")),
    };
    match built {
        Ok(s) => {
            if let Err(e) = s.play() {
                track.stop.store(true, Ordering::Relaxed);
                set_msg(shared, format!("再生を開始できません: {e}"));
                return;
            }
            *stream = Some(s);
            *src_holder = Some(src.clone());
            let mut g = shared.lock().unwrap();
            g.status.state = PlayState::Playing;
            g.status.index = Some(i);
            g.status.path = Some(path.clone());
            g.status.duration_secs = src.duration_secs();
            g.status.device = device.name().unwrap_or_default();
            g.status.device_rate_hz = dev_rate;
            g.status.source_rate_hz = src.sample_rate;
            g.status.source_kind = kind;
            g.status.message = String::new();
            g.track = Some(track);
        }
        Err(e) => {
            track.stop.store(true, Ordering::Relaxed);
            set_msg(shared, format!("ストリームを開けません: {e}"));
        }
    }
}
