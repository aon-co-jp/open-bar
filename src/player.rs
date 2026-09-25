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

use crate::output::{integer_ratio, map_channels_pub, poly_design, sinc_params, ResampleFilter};
use crate::polyphase::Upsampler;
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
    pub auto_version: bool,
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
    SetAutoVersion(bool),
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
    /// 停止・切り替えの直前に、クリックノイズを避けて音量をなめらかに0へ下げる(約20ms)。
    fadeout: AtomicBool,
    faded: AtomicBool,
    fade_gain: AtomicU32,
    /// DoPのペイロードを運んでいる(マーカーは出力スレッドが付け、音量処理は一切しない)。
    dop: bool,
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
        auto_version: true,
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
    /// 同じ曲の別形式(例: `曲.wav`・`曲.dsd256.dsf`・`曲.dsd64.dsf`)をモードに合わせて自動選択するか(既定: する)。
    pub fn set_auto_version(&self, on: bool) {
        let _ = self.tx.send(Cmd::SetAutoVersion(on));
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
/// 開始・シーク直後のフェードイン、停止・切り替え直前のフェードアウトの長さ(秒)。
const FADE_SECS: f64 = 0.02;
const CHUNK: usize = 2048;

/// 変換器: 整数倍ならポリフェーズ(高速)、そうでなければ汎用のsinc補間(rubato)。
enum Rs {
    None,
    Sinc(Box<SincFixedIn<f32>>),
    Poly(Vec<Upsampler>),
}

fn make_resampler(from: u32, to: u32, ch: usize, filter: ResampleFilter) -> Rs {
    if from == to {
        return Rs::None;
    }
    if let Some(l) = integer_ratio(from, to) {
        let (m, cutoff) = poly_design(filter);
        return Rs::Poly((0..ch).map(|_| Upsampler::new(l, m, cutoff, 9.0)).collect());
    }
    match SincFixedIn::<f32>::new(to as f64 / from as f64, 2.0, sinc_params(filter), CHUNK, ch) {
        Ok(r) => Rs::Sinc(Box::new(r)),
        Err(_) => Rs::None,
    }
}

/// 生産スレッド: ソースを`CHUNK`フレームずつ読み→(必要なら)リサンプル→チャンネル変換→リングへ。リングが満杯なら待つ。
fn producer(track: Arc<Track>, src: Arc<dyn Source>) {
    let ch = src.channels();
    let src_frames = src.total_frames();
    let src_rate = src.rate_hz();
    let mut gen_seen = track.seek_gen.load(Ordering::Acquire);
    let mut pos: u64 = 0;
    let mut rs = make_resampler(src_rate, track.out_rate, ch, track.filter);
    let mut flushed = false;
    let capacity = (RING_SECS * track.out_rate as f64) as usize * track.out_ch;
    let fade_frames = (FADE_SECS * track.out_rate as f64) as usize;
    let mut fade_left = if track.dop { 0 } else { fade_frames }; // 開始・シーク直後はなめらかに立ち上げる(波形の途中から始まるとクリック音になる)
    loop {
        if track.stop.load(Ordering::Relaxed) {
            return;
        }
        let g = track.seek_gen.load(Ordering::Acquire);
        if g != gen_seen {
            gen_seen = g;
            pos = track.seek_src_frame.load(Ordering::Acquire).min(src_frames);
            rs = make_resampler(src_rate, track.out_rate, ch, track.filter);
            flushed = false;
            track.producer_done.store(false, Ordering::Release);
            let mut ring = track.ring.lock().unwrap();
            ring.clear();
            track.consumed.store(0, Ordering::Release);
            track.primed.store(false, Ordering::Release);
            fade_left = if track.dop { 0 } else { fade_frames };
            track.base.store((pos as f64 * track.out_rate as f64 / src_rate as f64) as u64, Ordering::Release);
        }
        if pos >= src_frames {
            // ポリフェーズはフィルターの遅延ぶんの末尾が残っているので、最後に一度だけ吐き出す
            if !flushed {
                flushed = true;
                if let Rs::Poly(ups) = &mut rs {
                    let tails: Vec<Vec<f32>> = ups.iter_mut().map(|u| u.flush()).collect();
                    let frames = tails.iter().map(|t| t.len()).min().unwrap_or(0);
                    let mut inter = Vec::with_capacity(frames * ch);
                    for i in 0..frames {
                        for t in &tails {
                            inter.push(t[i]);
                        }
                    }
                    track.ring.lock().unwrap().extend(map_channels_pub(&inter, ch, track.out_ch));
                }
            }
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
        let out: Vec<f32> = match &mut rs {
            Rs::None => map_channels_pub(&block, ch, track.out_ch),
            Rs::Poly(ups) => {
                let outs: Vec<Vec<f32>> = ups
                    .iter_mut()
                    .enumerate()
                    .map(|(c, u)| {
                        let mono: Vec<f32> = block.iter().skip(c).step_by(ch).copied().collect();
                        u.process(&mono)
                    })
                    .collect();
                let frames = outs.iter().map(|o| o.len()).min().unwrap_or(0);
                let mut inter = Vec::with_capacity(frames * ch);
                for i in 0..frames {
                    for o in &outs {
                        inter.push(o[i]);
                    }
                }
                map_channels_pub(&inter, ch, track.out_ch)
            }
            Rs::Sinc(r) => {
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
        let mut out = out;
        if fade_left > 0 {
            let ch_out = track.out_ch;
            let frames = out.len() / ch_out;
            for f in 0..frames.min(fade_left) {
                let g = 1.0 - (fade_left - f) as f32 / fade_frames as f32;
                for c in 0..ch_out {
                    out[f * ch_out + c] *= g;
                }
            }
            fade_left = fade_left.saturating_sub(frames);
        }
        track.ring.lock().unwrap().extend(out);
    }
}

// ---- 出力バックエンド ----

/// 現在再生中のトラック(なければ無音)。共有モードの出力ストリームは開きっぱなしで、ここを差し替えて曲やモードを切り替える。
type Slot = Arc<Mutex<Option<Arc<Track>>>>;

/// 開きっぱなしの共有モード出力。曲の切り替え・停止のたびにデバイスを開閉すると、DAC側でミュート解除のノイズ(プツッ)が出るため、
/// ストリームは維持して無音を流し続ける。
struct SharedOut {
    _stream: cpal::Stream,
    slot: Slot,
    rate: u32,
    ch: usize,
    name: String,
}

fn build_cpal<T>(device: &cpal::Device, config: &StreamConfig, slot: Slot, ch: usize, volume: Arc<AtomicU32>) -> Result<cpal::Stream, String>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    device
        .build_output_stream(
            config,
            move |out: &mut [T], _| {
                let n = out.len() / ch;
                let silent = |out: &mut [T]| out.iter_mut().for_each(|o| *o = T::from_sample(0.0));
                // 再生中のトラックを取得(差し替え中のごく短い間は無音)
                let track = match slot.try_lock() {
                    Ok(g) => g.clone(),
                    Err(_) => {
                        silent(out);
                        return;
                    }
                };
                let Some(track) = track else {
                    silent(out);
                    return;
                };
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
                let fading = track.fadeout.load(Ordering::Relaxed);
                let step = 1.0 / (FADE_SECS * track.out_rate as f64) as f32;
                let mut gain = f32::from_bits(track.fade_gain.load(Ordering::Relaxed));
                for (i, (o, v)) in out.iter_mut().zip(ring.drain(..take * ch)).enumerate() {
                    if fading && i % ch == 0 && gain > 0.0 {
                        gain = (gain - step).max(0.0);
                    }
                    *o = T::from_sample(v * vol * gain);
                }
                if fading {
                    track.fade_gain.store(gain.to_bits(), Ordering::Relaxed);
                    if gain <= 0.0 {
                        track.faded.store(true, Ordering::Release);
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

/// DoPの出力バイト列を作る(純粋関数。排他スレッドから使い、単体テストできる)。`next_payload`が尽きたら(`None`)DSD無音で埋める。
/// マーカーは`marker_even`から始めて**1フレームごとに反転**し、呼び出しをまたいでも位相が連続する。
pub fn dop_fill(marker_even: &mut bool, mut next_payload: impl FnMut() -> Option<u16>, frames: usize, channels: usize, container_bytes: usize, out: &mut Vec<u8>) {
    use crate::source::{dop_word, DOP_IDLE_PAYLOAD};
    for _ in 0..frames {
        for _ in 0..channels {
            let payload = next_payload().unwrap_or(DOP_IDLE_PAYLOAD);
            crate::exclusive_bytes::write_sample_24(out, dop_word(*marker_even, payload), container_bytes);
        }
        *marker_even = !*marker_even;
    }
}

/// 排他モードの出力スレッド: リングからフレームを取り、整数(16/24bit)へ変換してWASAPIへ書く。
/// - PCM: 音量が100%ならビットパーフェクト。100%未満のときだけデジタルで下げる(聴き比べの音量合わせ用)。
/// - DoP: リングにはペイロード(DSD 2バイト)が載っており、**ここで連続した位相のマーカー(0x05/0xFA交互)を付ける**。
///   一時停止・シーク・音切れ・開始前の隙間は、DSD無音(0x69)のペイロードで埋めて位相を保つ(0のPCMを混ぜるとDACが
///   DSDの判定を外してノイズになる)。DoPでは音量処理を一切しない。
#[cfg(windows)]
fn exclusive_thread(track: Arc<Track>, bits: u16, shared: Arc<Mutex<Shared>>, volume: Arc<AtomicU32>) {
    use crate::exclusive::{quantize, write_sample_24, ExclusiveDevice};
    use crate::source::dop_payload_from_f32;
    let dev = match ExclusiveDevice::open(track.out_rate, track.out_ch, bits) {
        Ok(d) => d,
        Err(e) => {
            set_msg(&shared, format!("排他モードを開けません: {e}"));
            track.stop.store(true, Ordering::Relaxed);
            return;
        }
    };
    let ch = track.out_ch;
    let mut marker_even = true; // DoPのマーカー位相(全チャンネル共通、フレームごとに反転)
    let r = dev.run(&track.stop, |n, cbytes, chn, out| {
        let paused = track.paused.load(Ordering::Relaxed);
        let vol = if track.dop { 1.0 } else { f32::from_bits(volume.load(Ordering::Relaxed)) };
        let fading = track.fadeout.load(Ordering::Relaxed) && !track.dop;
        let step = 1.0 / (FADE_SECS * track.out_rate as f64) as f32;
        let mut gain = f32::from_bits(track.fade_gain.load(Ordering::Relaxed));
        let mut ring = track.ring.lock().unwrap();
        let ring_frames = ring.len() / ch;
        if !paused && !track.primed.load(Ordering::Acquire) && (ring_frames as f64 >= PRIME_SECS * track.out_rate as f64 || track.producer_done.load(Ordering::Acquire)) {
            track.primed.store(true, Ordering::Release);
        }
        // 先読みが溜まるまでは無音(DoPはDSD無音で位相を保つ)
        let avail = if paused || !track.primed.load(Ordering::Acquire) { 0 } else { ring_frames };
        let take = n.min(avail);
        let mut samples = ring.drain(..take * ch);
        for f in 0..n {
            if track.dop {
                dop_fill(&mut marker_even, || if f < take { samples.next().map(dop_payload_from_f32) } else { None }, 1, chn, cbytes, out);
            } else {
                for _c in 0..chn {
                    if fading && _c == 0 && gain > 0.0 {
                        gain = (gain - step).max(0.0);
                    }
                    let v = if f < take { samples.next().unwrap_or(0.0) * vol * gain } else { 0.0 };
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
        }
        drop(samples);
        if fading {
            track.fade_gain.store(gain.to_bits(), Ordering::Relaxed);
            if gain <= 0.0 {
                track.faded.store(true, Ordering::Release);
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
    let mut ctx = Ctx { playlist: Vec::new(), current: None, mode: PlayMode::Shared, filter: ResampleFilter::Standard, auto_version: true, shared_out: None, src_holder: None, host: cpal::default_host(), shared: shared.clone(), volume };
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
            Cmd::SetAutoVersion(on) => {
                ctx.auto_version = on;
                shared.lock().unwrap().status.auto_version = on;
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
    /// 同じ曲の別形式(PCM/DSD256/DSD64など)がプレイリストにあるとき、モードに合わせて自動で選ぶ。
    auto_version: bool,
    shared_out: Option<SharedOut>,
    src_holder: Option<Arc<dyn Source>>,
    host: cpal::Host,
    shared: Arc<Mutex<Shared>>,
    volume: Arc<AtomicU32>,
}

/// 「同じ曲」の名前(最初の`.`より前、大文字小文字は区別しない)。`Track04.wav`・`Track04.dsd256.dsf`・`track04.dsd64.dsf`は同じ曲。
pub fn version_stem(path: &str) -> String {
    let name = path.replace('\\', "/").rsplit('/').next().unwrap_or(path).to_string();
    name.split('.').next().unwrap_or(&name).to_lowercase()
}

/// 同じ曲の候補の中から、モードに合う形式を選ぶ(純粋関数)。`candidates`は(プレイリスト位置, DSDか, ビットレート/サンプルレート)。
/// - DoP(D): DSDのうち、`dop_ok`(DoPのPCMレート=DSDレート/16を機器が受け付けるか)を満たすもの。レートが高い順(DSD256 → DSD64)。
///   受けられるDSDが無ければ`None`(呼び出し側がPCMへ落とす)。
/// - A/B/E: PCM版(あれば)。無ければ`None`(=そのDSDをPCM化して再生)。PCMが複数なら先頭。
pub fn pick_version(mode: PlayMode, candidates: &[(usize, bool, u32)], dop_ok: &dyn Fn(u32) -> bool) -> Option<usize> {
    if mode == PlayMode::Dop {
        let mut dsd: Vec<&(usize, bool, u32)> = candidates.iter().filter(|c| c.1).collect();
        dsd.sort_by(|a, b| b.2.cmp(&a.2));
        dsd.into_iter().find(|c| dop_ok(c.2)).map(|c| c.0)
    } else {
        candidates.iter().find(|c| !c.1).map(|c| c.0)
    }
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
        // クリック音を避けるため、まず約20msかけて音量を0へ下げてから止める(DoPは音量処理できないので即停止)
        let t = self.shared.lock().unwrap().track.clone();
        if let Some(t) = t {
            if !t.dop && !t.paused.load(Ordering::Relaxed) && t.primed.load(Ordering::Acquire) {
                t.fadeout.store(true, Ordering::Release);
                let t0 = std::time::Instant::now();
                while !t.faded.load(Ordering::Acquire) && t0.elapsed() < Duration::from_millis(120) {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        }
        // 共有モードのストリームは開いたまま、再生中のトラックだけ外す(デバイスの開閉によるノイズを避ける)
        if let Some(so) = &self.shared_out {
            *so.slot.lock().unwrap() = None;
        }
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

    /// 自動選択(`auto_version`)が有効なら、同じ曲の別形式のうちモードに合うものへ切り替える。(選んだ位置, 説明)を返す。
    fn resolve_version(&self, i: usize) -> (usize, String) {
        if !self.auto_version {
            return (i, String::new());
        }
        let stem = version_stem(&self.playlist[i]);
        let mut cands: Vec<(usize, bool, u32)> = Vec::new();
        for (idx, p) in self.playlist.iter().enumerate() {
            if version_stem(p) != stem {
                continue;
            }
            let info = crate::media::probe(p);
            let is_dsd = info.kind == crate::media::MediaKind::Dsd;
            cands.push((idx, is_dsd, info.sample_rate_hz.unwrap_or(0)));
        }
        if cands.len() < 2 {
            return (i, String::new());
        }
        #[cfg(windows)]
        let dop_ok = |dsd_rate: u32| crate::exclusive::probe(dsd_rate / 16, 2, 24).is_ok();
        #[cfg(not(windows))]
        let dop_ok = |_: u32| false;
        match pick_version(self.mode, &cands, &dop_ok) {
            Some(j) if j != i => {
                let c = cands.iter().find(|c| c.0 == j).copied().unwrap_or((j, false, 0));
                let what = if c.1 { format!("DSD{}", c.2 / 44_100) } else { format!("PCM {}", khz(c.2)) };
                (j, format!("モードに合わせて同じ曲の「{what}」版を自動で選びました。 / Auto-picked the {what} version of this track to match the mode."))
            }
            Some(j) => (j, String::new()),
            None => {
                // DoPで送れるDSDが無い(またはPCM版が無い): 選んだ位置のまま(DoP不可なら後段でPCM化にフォールバック)
                let msg = if self.mode == PlayMode::Dop { "この機器がDoPで受けられるDSD版が見つからないため、選んだファイルをPCM化して再生します。 / No DSD version this DAC accepts via DoP; playing the chosen file converted to PCM.".to_string() } else { String::new() };
                (i, msg)
            }
        }
    }

    fn start_track(&mut self, i: usize, start_secs: f64) {
        self.stop_track();
        self.shared.lock().unwrap().status.state = PlayState::Stopped;
        let (i, auto_note) = self.resolve_version(i);
        self.current = Some(i);
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
        let mut note = auto_note;
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
        // 変換が実時間に間に合わないフィルター(重いシャープなど)は、音切れの原因になるので標準へ落とす。
        let mut effective_filter = self.filter;
        if src.rate_hz() != out_rate {
            let rtf = crate::output::filter_realtime_factor(src.rate_hz(), out_rate, src.channels(), effective_filter);
            if rtf < 3.0 && effective_filter != ResampleFilter::Standard {
                let std_rtf = crate::output::filter_realtime_factor(src.rate_hz(), out_rate, src.channels(), ResampleFilter::Standard);
                note.push_str(&format!(" このPCでは選んだフィルターが実時間に間に合わない(実時間の{rtf:.1}倍)ため、標準フィルターで再生します。 / The chosen filter is too heavy for this PC ({rtf:.1}x real time); using Standard."));
                let _ = std_rtf;
                effective_filter = ResampleFilter::Standard;
            }
        }
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
            filter: effective_filter,
            fadeout: AtomicBool::new(false),
            faded: AtomicBool::new(false),
            fade_gain: AtomicU32::new(1.0f32.to_bits()),
            dop: src.kind() == "DoP",
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
                self.shared_out = None; // 排他モードの間は共有ストリームを閉じる
                let (t3, sh, vol) = (track.clone(), self.shared.clone(), self.volume.clone());
                std::thread::spawn(move || exclusive_thread(t3, bits, sh, vol));
                device_label = default_device_name();
            }
        } else if let (Some(device), Some(fmt)) = (dev, sample_format) {
            // 既存の共有ストリームが同じデバイス・形式なら使い回し、違えば作り直す
            let reuse = self.shared_out.as_ref().is_some_and(|so| so.rate == out_rate && so.ch == out_ch && so.name == dev_name);
            if !reuse {
                self.shared_out = None;
                let slot: Slot = Arc::new(Mutex::new(None));
                let config = StreamConfig { channels: out_ch as u16, sample_rate: cpal::SampleRate(out_rate), buffer_size: cpal::BufferSize::Default };
                let built = match fmt {
                    SampleFormat::F32 => build_cpal::<f32>(&device, &config, slot.clone(), out_ch, self.volume.clone()),
                    SampleFormat::I16 => build_cpal::<i16>(&device, &config, slot.clone(), out_ch, self.volume.clone()),
                    SampleFormat::I32 => build_cpal::<i32>(&device, &config, slot.clone(), out_ch, self.volume.clone()),
                    other => Err(format!("未対応のデバイス形式: {other:?}")),
                };
                match built {
                    Ok(stream) => {
                        if let Err(e) = stream.play() {
                            track.stop.store(true, Ordering::Relaxed);
                            set_msg(&self.shared, format!("再生を開始できません: {e}"));
                            return;
                        }
                        self.shared_out = Some(SharedOut { _stream: stream, slot, rate: out_rate, ch: out_ch, name: dev_name.clone() });
                    }
                    Err(e) => {
                        track.stop.store(true, Ordering::Relaxed);
                        set_msg(&self.shared, format!("ストリームを開けません: {e}"));
                        return;
                    }
                }
            }
            if let Some(so) = &self.shared_out {
                *so.slot.lock().unwrap() = Some(track.clone());
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

    #[test]
    fn dop_markers_stay_continuous_across_data_gaps_and_calls() {
        let mut phase = true;
        let mut out = Vec::new();
        // 3フレームぶんのデータ(2ch)→ その後データが尽きて2フレームは無音で埋める → 次の呼び出しでも位相が続く
        let data = [0x1111u16, 0x2222, 0x3333, 0x4444, 0x5555, 0x6666];
        let mut it = data.iter().copied();
        dop_fill(&mut phase, || it.next(), 5, 2, 4, &mut out);
        let mut out2 = Vec::new();
        dop_fill(&mut phase, || None, 2, 2, 4, &mut out2);
        out.extend(out2);
        // 4バイト/サンプル(左詰め24bit): [0, 下位, 上位, マーカー]。フレームごとに0x05,0xFA,0x05,…
        let markers: Vec<u8> = out.chunks(4).map(|w| w[3]).collect();
        let per_frame: Vec<u8> = markers.chunks(2).map(|f| f[0]).collect();
        assert_eq!(per_frame, vec![0x05, 0xFA, 0x05, 0xFA, 0x05, 0xFA, 0x05], "7フレーム通して交互(呼び出しをまたいでも崩れない)");
        assert!(markers.chunks(2).all(|f| f[0] == f[1]), "左右のマーカーは同じ位相");
        // データ部: 先頭は0x1111、尽きた後はDSD無音0x6969
        assert_eq!(&out[..4], &[0x00, 0x11, 0x11, 0x05]);
        let last = &out[out.len() - 4..];
        assert_eq!(last, &[0x00, 0x69, 0x69, 0x05], "7フレーム目(偶数位相)のマーカーは0x05");
    }

    #[test]
    fn version_stems_group_the_same_track_across_formats() {
        assert_eq!(version_stem("F:/a/Track04.wav"), "track04");
        assert_eq!(version_stem("C:\\x\\track04.dsd256.dsf"), "track04");
        assert_eq!(version_stem("Track04.dsd64.dsf"), "track04");
        assert_ne!(version_stem("Track05.wav"), version_stem("Track04.wav"));
    }

    #[test]
    fn mode_picks_pcm_or_the_best_dop_capable_dsd_version() {
        // (位置, DSDか, レート): 0=PCM 44.1k、1=DSD256、2=DSD64
        let c = [(0usize, false, 44_100u32), (1, true, 11_289_600), (2, true, 2_822_400)];
        let all_ok = |_: u32| true;
        let only64 = |r: u32| r == 2_822_400;
        let none = |_: u32| false;
        for m in [PlayMode::Exclusive, PlayMode::Shared, PlayMode::ExclusiveUpsample] {
            assert_eq!(pick_version(m, &c, &all_ok), Some(0), "A/B/EはPCM版");
        }
        assert_eq!(pick_version(PlayMode::Dop, &c, &all_ok), Some(1), "DoPはまずDSD256");
        assert_eq!(pick_version(PlayMode::Dop, &c, &only64), Some(2), "DSD256を受けられない機器ではDSD64へ自動で切り替わる");
        assert_eq!(pick_version(PlayMode::Dop, &c, &none), None, "DoPで受けられるDSDが無ければ選ばない");
        assert_eq!(pick_version(PlayMode::Exclusive, &c[1..], &all_ok), None, "PCM版が無ければ選ばない(DSDをPCM化)");
    }
}
