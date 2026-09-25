//! 対話的なプレーヤー(再生・一時停止・停止・シーク・音量・モード切替・プレイリスト自動送り)。UIから使う。
//!
//! 構成: ワーカースレッドがコマンドを受け、トラックごとに「ソース(PCM/DSD→PCM/DoP)→(生産スレッドで)必要ならリサンプル→
//! リングバッファ→出力バックエンド(cpal共有 または WASAPI排他)」で鳴らす。先読みの逐次処理なので、DSDや高いレートでも
//! 再生は約1秒で始まる。シークはリサンプラを作り直してソースの位置から再開する。
//!
//! **再生モード**(A/Bの聴き比べ用に、再生中でも同じ位置で切り替えられる):
//! - `Shared`(B): Windowsのミキサー経由。デバイスの設定レートへ高品質変換。音量が効く。
//! - `Exclusive`(A): 排他モードで元のレートのままDACへ直送(ビットパーフェクト)。音量はDAC側。
//! - `ExclusiveUpsample`(E): 排他モードで、高品質sincで高いレート(352.8k/384k等)へ変換して直送。
//! - `Dop`(D): DSDをDoPでDACへ直送(**DoP対応DACのみ**。非対応だと大音量のノイズ)。
//!
//! 曲の切り替わりに短い隙間(数十ms)が入る(ギャップレスは次の段階)。

use crate::output::{map_channels_pub, sinc_params, ResampleFilter};
use crate::pcm::decode_file;
use crate::source::{DopSource, DsdPcmSource, PcmSource, Source};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use rubato::{Resampler, SincFixedIn};
use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlayMode {
    Shared,
    Exclusive,
    ExclusiveUpsample,
    Dop,
}

/// 再生モードの表示文(日本語・英語)。UIで大きく表示する。
#[derive(Debug, Clone, Serialize)]
pub struct ModeText {
    pub id: &'static str,
    pub letter: &'static str,
    pub title_ja: &'static str,
    pub title_en: &'static str,
    pub desc_ja: &'static str,
    pub desc_en: &'static str,
    pub bit_perfect: bool,
}

pub fn mode_text(mode: PlayMode) -> ModeText {
    match mode {
        PlayMode::Shared => ModeText {
            id: "shared",
            letter: "B",
            title_ja: "共有モード・高品質アップサンプル",
            title_en: "Shared mode · high-quality upsampling",
            desc_ja: "Windowsのミキサーを経由し、デバイスの設定レート(例: 384kHz)へ高品質のsinc補間で変換して再生します。アプリの音量が効きます。",
            desc_en: "Plays through the Windows mixer, converting to the device's configured rate (e.g. 384 kHz) with a high-quality sinc resampler. The app volume works.",
            bit_perfect: false,
        },
        PlayMode::Exclusive => ModeText {
            id: "exclusive",
            letter: "A",
            title_ja: "排他モード・ビットパーフェクト",
            title_en: "Exclusive mode · bit-perfect",
            desc_ja: "Windowsのミキサーを通さず、元のサンプルレートのままDACへ直接送ります(変換・音量処理なし)。音量はDAC側で調整してください(アプリの音量は100%のとき値を変えません)。",
            desc_en: "Bypasses the Windows mixer and sends the original sample rate straight to the DAC (no conversion, no volume processing). Adjust the volume on the DAC.",
            bit_perfect: true,
        },
        PlayMode::ExclusiveUpsample => ModeText {
            id: "exclusive_upsample",
            letter: "E",
            title_ja: "排他モード・高品質アップサンプル",
            title_en: "Exclusive mode · high-quality upsampling",
            desc_ja: "Windowsのミキサーを通さず、高品質のsinc補間で高いレート(例: 352.8kHz/384kHz)へ変換してDACへ直接送ります。音量はDAC側で調整してください。",
            desc_en: "Bypasses the Windows mixer and sends audio converted to a higher rate (e.g. 352.8/384 kHz) with a high-quality sinc resampler straight to the DAC. Adjust the volume on the DAC.",
            bit_perfect: false,
        },
        PlayMode::Dop => ModeText {
            id: "dop",
            letter: "D",
            title_ja: "DoP・DSDのままDACへ",
            title_en: "DoP · DSD as-is to the DAC",
            desc_ja: "DSDをDoP(DSD over PCM)に載せ、排他モードでDACへ直接送ります。DoP対応のDACでのみ使ってください(非対応だと大きなノイズになります)。DSD以外のファイルは、排他モード(A)で再生します。",
            desc_en: "Packs DSD as DoP (DSD over PCM) and sends it straight to the DAC in exclusive mode. Use only with a DoP-capable DAC (otherwise loud noise). Non-DSD files play in exclusive mode (A).",
            bit_perfect: true,
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub state: PlayState,
    pub index: Option<usize>,
    pub path: Option<String>,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub device: String,
    /// 出力(リング)のレート。共有=デバイスの設定レート、排他=実際にDACへ送るレート。
    pub out_rate_hz: u32,
    pub source_rate_hz: u32,
    pub source_kind: String,
    pub volume: f32,
    /// 選択中のモードと、実際に使っているモード(排他を開けなければ共有へ落ちる)。
    pub selected_mode: PlayMode,
    pub active_mode: ModeText,
    /// 実際の出力形式の説明(例: 「PCM 44.1 kHz → 384 kHz へ変換」)。
    pub route_ja: String,
    pub route_en: String,
    /// 選んだモードで再生できず別のモードへ切り替えた理由など(なければ空)。
    pub note: String,
    /// 直近のエラー(なければ空)。
    pub message: String,
    /// 再生中の音切れ(リング枯渇)フレーム数。0が正常。
    pub underrun_frames: u64,
    /// 選択中のアップサンプルフィルター。
    pub filter: ResampleFilter,
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
    SetMode(PlayMode),
    SetFilter(ResampleFilter),
    Quit,
}

/// 1トラック分の再生状態(出力バックエンドと生産スレッドで共有)。
struct Track {
    ring: Mutex<VecDeque<f32>>,
    paused: AtomicBool,
    ended: AtomicBool,
    /// 生産スレッド・出力スレッドの停止要求(トラック終了・停止・切り替え時)。
    stop: AtomicBool,
    producer_done: AtomicBool,
    /// シークの世代番号。増えたら生産側はリサンプラを作り直して`seek_src_frame`から再開する。
    seek_gen: AtomicU64,
    seek_src_frame: AtomicU64,
    consumed: AtomicU64,
    /// 再生中にリングが空で無音を出した回数(フレーム数)。0でなければ音切れ・ノイズの原因になり得る。
    underrun_frames: AtomicU64,
    /// 先読みバッファが溜まるまで(または曲の終わりまで)は無音を出して待つ。
    primed: AtomicBool,
    /// 現在の世代の開始位置(出力フレーム)。位置 = (base + consumed) / out_rate。
    base: AtomicU64,
    out_rate: u32,
    out_ch: usize,
    total_secs: f64,
    filter: ResampleFilter,
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

fn blank_status(mode: PlayMode) -> Status {
    Status {
        state: PlayState::Stopped,
        index: None,
        path: None,
        position_secs: 0.0,
        duration_secs: 0.0,
        device: String::new(),
        out_rate_hz: 0,
        source_rate_hz: 0,
        source_kind: String::new(),
        volume: 1.0,
        selected_mode: mode,
        active_mode: mode_text(mode),
        route_ja: String::new(),
        route_en: String::new(),
        note: String::new(),
        message: String::new(),
        underrun_frames: 0,
        filter: ResampleFilter::Standard,
    }
}

impl Player {
    pub fn new() -> Player {
        let (tx, rx) = channel::<Cmd>();
        let volume = Arc::new(AtomicU32::new(1.0f32.to_bits()));
        let shared = Arc::new(Mutex::new(Shared { status: blank_status(PlayMode::Shared), track: None }));
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
    /// 再生モードを切り替える。再生中なら同じ位置から新しいモードで再開する(A/Bの聴き比べ用)。
    pub fn set_mode(&self, mode: PlayMode) {
        let _ = self.tx.send(Cmd::SetMode(mode));
    }
    /// アップサンプルのフィルターを切り替える。再生中なら同じ位置から新しいフィルターで再開する。
    pub fn set_filter(&self, f: ResampleFilter) {
        let _ = self.tx.send(Cmd::SetFilter(f));
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
            s.position_secs = (frames as f64 / t.out_rate.max(1) as f64).min(t.total_secs);
        }
        s.volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
        if let Some(t) = &g.track {
            s.underrun_frames = t.underrun_frames.load(Ordering::Relaxed);
        }
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

fn khz(hz: u32) -> String {
    let k = hz as f64 / 1000.0;
    if (k - k.round()).abs() < 1e-9 {
        format!("{} kHz", k as u64)
    } else {
        format!("{k:.1} kHz")
    }
}

const RING_SECS: f64 = 8.0;
/// 再生を始める前に溜める量(秒)。DoPは1フレームでも欠けるとDAC側のDSD判定が外れてノイズになるため、余裕を持たせる。
const PRIME_SECS: f64 = 0.75;
const CHUNK: usize = 2048;

fn make_resampler(from: u32, to: u32, ch: usize, filter: ResampleFilter) -> Option<SincFixedIn<f32>> {
    if from == to {
        return None;
    }
    SincFixedIn::<f32>::new(to as f64 / from as f64, 2.0, sinc_params(filter), CHUNK, ch).ok()
}

/// 生産スレッド: ソースを`CHUNK`フレームずつ読み→(必要なら)リサンプル→チャンネル変換→リングへ。リングが満杯なら待つ。
fn producer(track: Arc<Track>, src: Arc<dyn Source>) {
    let ch = src.channels();
    let src_frames = src.total_frames();
    let src_rate = src.rate_hz();
    let mut gen_seen = track.seek_gen.load(Ordering::Acquire);
    let mut pos: u64 = 0;
    let mut rs = make_resampler(src_rate, track.out_rate, ch, track.filter);
    let capacity = (RING_SECS * track.out_rate as f64) as usize * track.out_ch;
    loop {
        if track.stop.load(Ordering::Relaxed) {
            return;
        }
        let g = track.seek_gen.load(Ordering::Acquire);
        if g != gen_seen {
            gen_seen = g;
            pos = track.seek_src_frame.load(Ordering::Acquire).min(src_frames);
            rs = make_resampler(src_rate, track.out_rate, ch, track.filter);
            track.producer_done.store(false, Ordering::Release);
            let mut ring = track.ring.lock().unwrap();
            ring.clear();
            track.consumed.store(0, Ordering::Release);
            track.primed.store(false, Ordering::Release);
            track.base.store((pos as f64 * track.out_rate as f64 / src_rate as f64) as u64, Ordering::Release);
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
        let block = src.read(pos, CHUNK);
        let n = block.len() / ch;
        if n == 0 {
            pos = src_frames;
            continue;
        }
        let out: Vec<f32> = match rs.as_mut() {
            None => map_channels_pub(&block, ch, track.out_ch),
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
                        map_channels_pub(&inter, ch, track.out_ch)
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

// ---- 出力バックエンド ----

fn build_cpal<T>(device: &cpal::Device, config: &StreamConfig, track: Arc<Track>, volume: Arc<AtomicU32>) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let ch = track.out_ch;
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
                if !track.primed.load(Ordering::Acquire) {
                    if avail as f64 >= PRIME_SECS * track.out_rate as f64 || track.producer_done.load(Ordering::Acquire) {
                        track.primed.store(true, Ordering::Release);
                    } else {
                        silent(out); // 先読みが溜まるまで待つ(開始直後・シーク直後の音切れを防ぐ)
                        return;
                    }
                }
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
                if take < n {
                    if ring.is_empty() && track.producer_done.load(Ordering::Acquire) {
                        track.ended.store(true, Ordering::Release);
                    } else {
                        track.underrun_frames.fetch_add((n - take) as u64, Ordering::Relaxed);
                    }
                }
            },
            |e| eprintln!("出力ストリームのエラー: {e}"),
            None,
        )
        .map_err(|e| e.to_string())
}

/// 排他モードの出力スレッド: リングからフレームを取り、整数(16/24bit)へ変換してWASAPIへ書く。音量処理はしない。
#[cfg(windows)]
fn exclusive_thread(track: Arc<Track>, bits: u16, shared: Arc<Mutex<Shared>>, volume: Arc<AtomicU32>) {
    use crate::exclusive::{quantize, write_sample_24, ExclusiveDevice};
    let dev = match ExclusiveDevice::open(track.out_rate, track.out_ch, bits) {
        Ok(d) => d,
        Err(e) => {
            set_msg(&shared, format!("排他モードを開けません: {e}"));
            track.stop.store(true, Ordering::Relaxed);
            return;
        }
    };
    let ch = track.out_ch;
    let r = dev.run(&track.stop, |n, cbytes, chn, out| {
        let paused = track.paused.load(Ordering::Relaxed);
        let vol = f32::from_bits(volume.load(Ordering::Relaxed));
        let mut ring = track.ring.lock().unwrap();
        let ring_frames = ring.len() / ch;
        if !paused && !track.primed.load(Ordering::Acquire) {
            if ring_frames as f64 >= PRIME_SECS * track.out_rate as f64 || track.producer_done.load(Ordering::Acquire) {
                track.primed.store(true, Ordering::Release);
            }
        }
        // 先読みが溜まるまでは無音(DoPでも、開始前の無音はマーカー無しのPCM無音で問題ない)
        let avail = if paused || !track.primed.load(Ordering::Acquire) { 0 } else { ring_frames };
        let take = n.min(avail);
        for f in 0..n {
            for _c in 0..chn {
                // 音量が100%ならビットパーフェクト(値を一切変えない)。100%未満のときだけデジタルで下げる(聴き比べの音量合わせ用)。
                let v = if f < take { ring.pop_front().unwrap_or(0.0) * vol } else { 0.0 };
                let s = quantize(v, bits as u32);
                if bits == 16 && cbytes == 2 {
                    out.extend_from_slice(&(s as i16).to_le_bytes());
                } else if bits == 16 {
                    write_sample_24(out, s << 8, cbytes);
                } else {
                    write_sample_24(out, s, cbytes);
                }
            }
        }
        track.consumed.fetch_add(take as u64, Ordering::Relaxed);
        if !paused && track.primed.load(Ordering::Acquire) && take < n {
            if ring.is_empty() && track.producer_done.load(Ordering::Acquire) {
                track.ended.store(true, Ordering::Release);
                return false;
            }
            track.underrun_frames.fetch_add((n - take) as u64, Ordering::Relaxed);
        }
        true
    });
    if let Err(e) = r {
        set_msg(&shared, format!("排他モードの出力エラー: {e}"));
        track.ended.store(true, Ordering::Release);
    }
}

fn worker(rx: std::sync::mpsc::Receiver<Cmd>, shared: Arc<Mutex<Shared>>, volume: Arc<AtomicU32>) {
    let mut ctx = Ctx { playlist: Vec::new(), current: None, mode: PlayMode::Shared, filter: ResampleFilter::Standard, stream: None, src_holder: None, host: cpal::default_host(), shared: shared.clone(), volume };
    loop {
        let ended = shared.lock().unwrap().track.as_ref().is_some_and(|t| t.ended.load(Ordering::Acquire));
        let cmd = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let cmd = match (cmd, ended) {
            (Some(c), _) => Some(c),
            (None, true) => match ctx.current {
                Some(i) if i + 1 < ctx.playlist.len() => Some(Cmd::Play(i + 1)),
                _ => Some(Cmd::Stop),
            },
            (None, false) => None,
        };
        let Some(cmd) = cmd else { continue };
        match cmd {
            Cmd::Quit => {
                ctx.stop_track();
                return;
            }
            Cmd::SetPlaylist(v) => ctx.playlist = v,
            Cmd::Stop => {
                ctx.stop_track();
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
            Cmd::Seek(secs) => ctx.seek(secs),
            Cmd::Next => {
                if let Some(i) = ctx.current {
                    if i + 1 < ctx.playlist.len() {
                        ctx.current = Some(i + 1);
                        ctx.start_track(i + 1, 0.0);
                    }
                }
            }
            Cmd::Prev => {
                if let Some(i) = ctx.current {
                    let j = i.saturating_sub(1);
                    ctx.current = Some(j);
                    ctx.start_track(j, 0.0);
                }
            }
            Cmd::Play(i) => {
                if i < ctx.playlist.len() {
                    ctx.current = Some(i);
                    ctx.start_track(i, 0.0);
                }
            }
            Cmd::SetFilter(f) => {
                ctx.filter = f;
                let (playing, pos) = {
                    let g = shared.lock().unwrap();
                    let pos = g.track.as_ref().map(|t| (t.base.load(Ordering::Relaxed) + t.consumed.load(Ordering::Relaxed)) as f64 / t.out_rate.max(1) as f64).unwrap_or(0.0);
                    (g.track.is_some(), pos)
                };
                shared.lock().unwrap().status.filter = f;
                if playing {
                    if let Some(i) = ctx.current {
                        ctx.start_track(i, pos);
                    }
                }
            }
            Cmd::SetMode(m) => {
                ctx.mode = m;
                let (playing, pos) = {
                    let g = shared.lock().unwrap();
                    let pos = g.track.as_ref().map(|t| (t.base.load(Ordering::Relaxed) + t.consumed.load(Ordering::Relaxed)) as f64 / t.out_rate.max(1) as f64).unwrap_or(0.0);
                    (g.track.is_some(), pos)
                };
                {
                    let mut g = shared.lock().unwrap();
                    g.status.selected_mode = m;
                    if !playing {
                        g.status.active_mode = mode_text(m);
                    }
                }
                if playing {
                    if let Some(i) = ctx.current {
                        ctx.start_track(i, pos); // 同じ位置から新しいモードで再開
                    }
                }
            }
        }
    }
}

struct Ctx {
    playlist: Vec<String>,
    current: Option<usize>,
    mode: PlayMode,
    filter: ResampleFilter,
    stream: Option<cpal::Stream>,
    src_holder: Option<Arc<dyn Source>>,
    host: cpal::Host,
    shared: Arc<Mutex<Shared>>,
    volume: Arc<AtomicU32>,
}

/// 排他モードで使うPCMのレート候補(元レートと同じ系列、高い順)。`upsample`なら352.8k/384kまで上げる。
fn exclusive_rates(src_rate: u32, upsample: bool) -> Vec<u32> {
    let family_44 = src_rate % 44_100 == 0;
    let ups: &[u32] = if family_44 { &[352_800, 176_400, 88_200] } else { &[384_000, 192_000, 96_000] };
    let mut v: Vec<u32> = Vec::new();
    if upsample {
        v.extend(ups.iter().copied().filter(|r| *r >= src_rate));
    }
    if !v.contains(&src_rate) {
        v.push(src_rate);
    }
    v
}

impl Ctx {
    fn stop_track(&mut self) {
        self.stream = None;
        let mut g = self.shared.lock().unwrap();
        if let Some(t) = g.track.take() {
            t.stop.store(true, Ordering::Relaxed);
        }
        g.status.position_secs = 0.0;
    }

    fn seek(&mut self, secs: f64) {
        let g = self.shared.lock().unwrap();
        if let (Some(t), Some(src)) = (&g.track, &self.src_holder) {
            let frame = ((secs.max(0.0) * src.rate_hz() as f64) as u64).min(src.total_frames().saturating_sub(1));
            t.seek_src_frame.store(frame, Ordering::Release);
            t.seek_gen.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// ファイルを開いてソースにする。`want_dop`ならDSDをDoPソースに、そうでなければPCM(DSDはDSD→PCM)。
    fn open_source(&self, path: &str, want_dop: bool, dsd_pcm_target: Option<u32>) -> Result<Arc<dyn Source>, String> {
        let info = crate::media::probe(path);
        if info.kind == crate::media::MediaKind::Dsd {
            let s = open_mqa_dsd::read_dsd_file(path).map_err(|e| e.to_string())?;
            if want_dop {
                return Ok(Arc::new(DopSource::new(s)));
            }
            // PCM化するときは、最終的な出力レートへ直接変換できるならそれを使う(2段階の変換を避けて音質を保つ)。
            // 指定が無ければ機器が受けやすい176.4kHz以下(DSD64→176.4k、DSD256→176.4k)。
            let cands = crate::plan::dsd_pcm_candidates(s.rate_hz);
            let rate = dsd_pcm_target.filter(|t| cands.contains(t)).or_else(|| cands.iter().copied().find(|r| *r <= 176_400)).unwrap_or(44_100);
            let src = DsdPcmSource::new(s, open_mqa_dsd::DsdToPcm { out_rate_hz: rate, cutoff_hz: 40_000.0 }).map_err(|e| e.to_string())?;
            return Ok(Arc::new(src));
        }
        let kind = if info.is_mqa { "MQA(通常のPCMとして)" } else { "PCM" };
        let pcm = decode_file(path).map_err(|e| e.to_string())?;
        Ok(Arc::new(PcmSource::new(pcm, kind)))
    }

    fn start_track(&mut self, i: usize, start_secs: f64) {
        self.stop_track();
        self.shared.lock().unwrap().status.state = PlayState::Stopped;
        let path = self.playlist[i].clone();
        set_msg(&self.shared, "読み込み中... / Loading...");
        let is_dsd = crate::media::probe(&path).kind == crate::media::MediaKind::Dsd;
        let want_dop = self.mode == PlayMode::Dop && is_dsd;
        // 排他アップサンプル(E)のDSDは、352.8kHzへ直接変換する(176.4kHz→352.8kHzの2段階変換にしない)
        let dsd_target = if self.mode == PlayMode::ExclusiveUpsample && is_dsd { Some(352_800) } else { None };
        let mut src = match self.open_source(&path, want_dop, dsd_target) {
            Ok(s) => s,
            Err(e) => {
                set_msg(&self.shared, format!("再生できません({path}): {e}"));
                return;
            }
        };
        let mut mode = self.mode;
        let mut note = String::new();
        // 排他系のモード: 実際に開ける形式を探す。開けなければ理由を記録して共有モードへ落とす。
        let mut exclusive_choice: Option<(u32, u16)> = None; // (レート, 有効ビット)
        #[cfg(windows)]
        if mode != PlayMode::Shared {
            let ch = src.channels();
            let mut found = false;
            let is_dop = src.kind() == "DoP";
            let bits: u16 = if is_dop { 24 } else { 24 };
            let rates = if is_dop { vec![src.rate_hz()] } else { exclusive_rates(src.rate_hz(), mode == PlayMode::ExclusiveUpsample) };
            for r in rates {
                if crate::exclusive::probe(r, ch, bits).is_ok() {
                    exclusive_choice = Some((r, bits));
                    found = true;
                    break;
                }
            }
            if !found && dsd_target.is_some() && !is_dop {
                // 352.8kHzを受け付けない機器: 176.4kHzのPCMに切り替えて、排他(候補は元のレート以上)を探し直す
                if let Ok(s) = self.open_source(&path, false, None) {
                    src = s;
                    for r in exclusive_rates(src.rate_hz(), mode == PlayMode::ExclusiveUpsample) {
                        if crate::exclusive::probe(r, ch, bits).is_ok() {
                            exclusive_choice = Some((r, bits));
                            found = true;
                            break;
                        }
                    }
                }
            }
            if !found {
                if is_dop {
                    // DoPを送れない機器: DSD→PCMに切り替えて、排他(A)で試す
                    note = format!("この機器はDoP({})を受け付けません。DSDをPCMへ変換して再生します。 / This device does not accept DoP ({}). Playing DSD converted to PCM.", khz(src.rate_hz()), khz(src.rate_hz()));
                    if let Ok(s) = self.open_source(&path, false, None) {
                        src = s;
                    }
                    mode = PlayMode::Exclusive;
                    for r in exclusive_rates(src.rate_hz(), false) {
                        if crate::exclusive::probe(r, ch, 24).is_ok() {
                            exclusive_choice = Some((r, 24));
                            break;
                        }
                    }
                }
                if exclusive_choice.is_none() {
                    note.push_str(&format!("排他モードで{}を受け付けません。共有モードで再生します。 / The device does not accept {} in exclusive mode. Playing in shared mode.", khz(src.rate_hz()), khz(src.rate_hz())));
                    mode = PlayMode::Shared;
                }
            }
        }
        #[cfg(not(windows))]
        if mode != PlayMode::Shared {
            note = "排他モードはWindowsのみです。共有モードで再生します。 / Exclusive mode is Windows-only. Playing in shared mode.".to_string();
            mode = PlayMode::Shared;
        }
        // 出力(リング)のレートとチャンネル数
        let (out_rate, out_ch, dev_name, sample_format, dev) = if let Some((r, _)) = exclusive_choice {
            (r, src.channels(), "排他デバイス".to_string(), None, None)
        } else {
            let Some(device) = self.host.default_output_device() else {
                set_msg(&self.shared, "出力デバイスがありません / No output device");
                return;
            };
            let default = match device.default_output_config() {
                Ok(c) => c,
                Err(e) => {
                    set_msg(&self.shared, format!("デバイスの設定を取得できません: {e}"));
                    return;
                }
            };
            (default.sample_rate().0, default.channels() as usize, device.name().unwrap_or_default(), Some(default.sample_format()), Some(device))
        };
        let track = Arc::new(Track {
            ring: Mutex::new(VecDeque::new()),
            paused: AtomicBool::new(false),
            ended: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            producer_done: AtomicBool::new(false),
            seek_gen: AtomicU64::new(0),
            seek_src_frame: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            underrun_frames: AtomicU64::new(0),
            primed: AtomicBool::new(false),
            base: AtomicU64::new(0),
            out_rate,
            out_ch,
            total_secs: src.duration_secs(),
            filter: self.filter,
        });
        if start_secs > 0.0 {
            let frame = ((start_secs * src.rate_hz() as f64) as u64).min(src.total_frames().saturating_sub(1));
            track.seek_src_frame.store(frame, Ordering::Release);
            track.seek_gen.store(1, Ordering::Release);
        }
        let (t2, s2) = (track.clone(), src.clone());
        std::thread::spawn(move || producer(t2, s2));
        let mut device_label = dev_name.clone();
        if let Some((_, bits)) = exclusive_choice {
            #[cfg(windows)]
            {
                let (t3, sh, vol) = (track.clone(), self.shared.clone(), self.volume.clone());
                std::thread::spawn(move || exclusive_thread(t3, bits, sh, vol));
                device_label = default_device_name();
            }
        } else if let (Some(device), Some(fmt)) = (dev, sample_format) {
            let config = StreamConfig { channels: out_ch as u16, sample_rate: cpal::SampleRate(out_rate), buffer_size: cpal::BufferSize::Default };
            let built = match fmt {
                SampleFormat::F32 => build_cpal::<f32>(&device, &config, track.clone(), self.volume.clone()),
                SampleFormat::I16 => build_cpal::<i16>(&device, &config, track.clone(), self.volume.clone()),
                SampleFormat::I32 => build_cpal::<i32>(&device, &config, track.clone(), self.volume.clone()),
                other => Err(format!("未対応のデバイス形式: {other:?}")),
            };
            match built {
                Ok(s) => {
                    if let Err(e) = s.play() {
                        track.stop.store(true, Ordering::Relaxed);
                        set_msg(&self.shared, format!("再生を開始できません: {e}"));
                        return;
                    }
                    self.stream = Some(s);
                }
                Err(e) => {
                    track.stop.store(true, Ordering::Relaxed);
                    set_msg(&self.shared, format!("ストリームを開けません: {e}"));
                    return;
                }
            }
        }
        self.src_holder = Some(src.clone());
        // 経路の説明
        let kind = src.kind();
        let src_rate = src.rate_hz();
        let (route_ja, route_en) = if kind == "DoP" {
            (format!("DSDをDoPで送出({} · 24bit) → DACがDSDとして再生", khz(out_rate)), format!("DSD sent as DoP ({} · 24-bit) → the DAC plays it as DSD", khz(out_rate)))
        } else if src_rate == out_rate {
            (format!("{kind} {} をそのまま送出", khz(src_rate)), format!("{kind} {} sent as-is", khz(src_rate)))
        } else {
            (format!("{kind} {} → {} へ高品質変換(sinc補間)", khz(src_rate), khz(out_rate)), format!("{kind} {} → converted to {} (sinc resampler)", khz(src_rate), khz(out_rate)))
        };
        let mut g = self.shared.lock().unwrap();
        g.status.state = PlayState::Playing;
        g.status.index = Some(i);
        g.status.path = Some(path);
        g.status.duration_secs = src.duration_secs();
        g.status.device = device_label;
        g.status.out_rate_hz = out_rate;
        g.status.source_rate_hz = src_rate;
        g.status.source_kind = kind;
        g.status.selected_mode = self.mode;
        g.status.active_mode = mode_text(mode);
        g.status.route_ja = route_ja;
        g.status.route_en = route_en;
        g.status.note = note;
        g.status.message = String::new();
        g.track = Some(track);
    }
}

#[cfg(windows)]
fn default_device_name() -> String {
    use wasapi::{initialize_mta, DeviceEnumerator, Direction};
    let _ = initialize_mta();
    DeviceEnumerator::new().ok().and_then(|e| e.get_default_device(&Direction::Render).ok()).and_then(|d| d.get_friendlyname().ok()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_bilingual_text_and_a_distinct_letter() {
        let modes = [PlayMode::Shared, PlayMode::Exclusive, PlayMode::ExclusiveUpsample, PlayMode::Dop];
        let mut letters = std::collections::HashSet::new();
        for m in modes {
            let t = mode_text(m);
            assert!(!t.title_ja.is_empty() && !t.title_en.is_empty() && !t.desc_ja.is_empty() && !t.desc_en.is_empty(), "{:?}", m);
            assert!(letters.insert(t.letter));
        }
        assert!(mode_text(PlayMode::Exclusive).bit_perfect && mode_text(PlayMode::Dop).bit_perfect);
        assert!(!mode_text(PlayMode::Shared).bit_perfect && !mode_text(PlayMode::ExclusiveUpsample).bit_perfect);
    }

    #[test]
    fn exclusive_rate_candidates_stay_in_the_source_rate_family() {
        assert_eq!(exclusive_rates(44_100, false), vec![44_100]);
        assert_eq!(exclusive_rates(44_100, true), vec![352_800, 176_400, 88_200, 44_100]);
        assert_eq!(exclusive_rates(48_000, true), vec![384_000, 192_000, 96_000, 48_000]);
        assert_eq!(exclusive_rates(96_000, true), vec![384_000, 192_000, 96_000], "元レートと同じ値は重複しない");
    }
}
