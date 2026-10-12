//! ループバック橋渡し(Windows): OSが出力している音(YouTube・ブラウザ・音楽アプリなど)を丸ごと受け取り、
//! 高品質のsincアップサンプル(+32/24bit)をかけて、別のDAC(排他モード)へ送り直す。
//!
//! 使い方の前提: 「Windowsの既定の出力」を**DACではない出力**(例: HDMIモニターの音声、仮想ケーブル)にし、
//! その出力の音をここで受け取って、本物のDACへ排他モードで出す。DACを既定の出力にしたまま排他モードで開くと、
//! 他のアプリの音がDACへ届かなくなるため、受け口と出口は別デバイスにする。
//!
//! 限界(正直な開示):
//! - 受け取りから出力までに約0.3秒の遅延が入る。音楽は問題ないが、映像と音がずれるので動画には向かない。
//! - DRMで保護された音声(OSの保護された経路)は、受け取れない(無音になる)ことがある。保護を外す処理はしない。
//! - 受け取る音はOSのミキサーの形式(多くは48kHz/44.1kHzの32bit浮動小数点)であり、元の音源より高い情報は無い。
//!   高くなるのは出力のレートと精度で、変換の質を上げる処理である。

use crate::exclusive::{find_render_device, list_render_devices, quantize, write_sample_24, ExclusiveDevice};
use crate::output::OutputError;
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wasapi::{initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

const CH: usize = 2;
/// 受け取りバッファの目標量(秒)。これを中心に、時計のずれをリサンプル比の微調整で吸収する。
const TARGET_SECS: f64 = 0.25;
const CHUNK: usize = 1024;

#[derive(Debug, Clone, Serialize)]
pub struct BridgeStatus {
    pub running: bool,
    pub capture_device: String,
    pub output_device: String,
    pub in_rate_hz: u32,
    pub out_rate_hz: u32,
    pub out_bits: u16,
    pub container_bits: u16,
    /// 受け取りバッファの量(秒)。目標付近で安定していれば正常。
    pub buffer_secs: f64,
    /// 出力側で素材が足りず無音を出したフレーム数(0が正常)。
    pub underrun_frames: u64,
    /// 直近に受け取った音の最大振幅(0〜1)。0のままなら音が届いていない(再生していない、またはDRMで無音にされている)。
    pub in_peak: f32,
    pub message: String,
}

pub struct Bridge {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<BridgeStatus>>,
    peak: Arc<std::sync::atomic::AtomicU32>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

fn werr<E: std::fmt::Display>(e: E) -> OutputError {
    OutputError::Stream(e.to_string())
}

/// 元のレートに対する出力レートの候補(高い順)。44.1kHz系は352.8kHzまで、それ以外は384kHzまで。
pub fn output_rate_candidates(src_rate: u32) -> Vec<u32> {
    let family_44 = src_rate % 44_100 == 0;
    let ups: &[u32] = if family_44 { &[352_800, 176_400, 88_200] } else { &[384_000, 192_000, 96_000] };
    let mut v: Vec<u32> = ups.iter().copied().filter(|r| *r >= src_rate).collect();
    if !v.contains(&src_rate) {
        v.push(src_rate);
    }
    v
}

impl Bridge {
    /// 名前の一部で受け取りデバイス(ループバック)と出力デバイス(排他)を指定して橋渡しを始める。
    /// `max_rate`は出力レートの上限(0なら制限なし)。`bits`は24か32(32を受け付けない機器は24へ落とす)。
    pub fn start(capture_name: &str, output_name: &str, max_rate: u32, bits: u16) -> Result<Bridge, OutputError> {
        if capture_name.eq_ignore_ascii_case(output_name) {
            return Err(OutputError::Config("受け取りと出力に同じデバイスは指定できません(音が自分に戻ってしまいます)".into()));
        }
        // 受け取り側の形式を先に調べ、出力を実際に開けるレートを決める(開けなければここで失敗させる)。
        initialize_mta().ok().map_err(werr)?;
        let enumerator = DeviceEnumerator::new().map_err(werr)?;
        let cap_dev = find_render_device(&enumerator, capture_name)?;
        let cap_name = cap_dev.get_friendlyname().unwrap_or_default();
        let in_rate = cap_dev.get_iaudioclient().map_err(werr)?.get_mixformat().map_err(werr)?.get_samplespersec();
        let out_dev_name = find_render_device(&enumerator, output_name)?.get_friendlyname().unwrap_or_default();
        if cap_name == out_dev_name {
            return Err(OutputError::Config("受け取りと出力が同じデバイスです".into()));
        }

        let mut chosen: Option<(u32, u16)> = None;
        let want_bits = if bits >= 32 { 32 } else { 24 };
        for r in output_rate_candidates(in_rate).into_iter().filter(|r| max_rate == 0 || *r <= max_rate) {
            if crate::exclusive::probe_on(output_name, r, CH, want_bits).is_ok() {
                chosen = Some((r, want_bits));
                break;
            }
            if want_bits > 24 && crate::exclusive::probe_on(output_name, r, CH, 24).is_ok() {
                chosen = Some((r, 24));
                break;
            }
        }
        let (out_rate, out_bits) = chosen.ok_or_else(|| OutputError::Config(format!("「{out_dev_name}」は排他モードで{}Hz以上を受け付けません(他のアプリが使用中の可能性があります)", in_rate)))?;

        let stop = Arc::new(AtomicBool::new(false));
        let ring: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::new()));
        let underruns = Arc::new(AtomicU64::new(0));
        let status = Arc::new(Mutex::new(BridgeStatus {
            running: true,
            capture_device: cap_name.clone(),
            output_device: out_dev_name.clone(),
            in_rate_hz: in_rate,
            out_rate_hz: out_rate,
            out_bits,
            container_bits: 0,
            buffer_secs: 0.0,
            underrun_frames: 0,
            in_peak: 0.0,
            message: String::new(),
        }));
        let peak = Arc::new(std::sync::atomic::AtomicU32::new(0));

        let mut threads = Vec::new();
        {
            let (stop, ring, status, name, peak) = (stop.clone(), ring.clone(), status.clone(), cap_name.clone(), peak.clone());
            threads.push(std::thread::spawn(move || {
                if let Err(e) = capture_loop(&name, in_rate, &stop, &ring, &peak) {
                    status.lock().unwrap().message = format!("受け取りが止まりました: {e}");
                    stop.store(true, Ordering::Relaxed);
                }
            }));
        }
        {
            let (stop, ring, status, underruns) = (stop.clone(), ring.clone(), status.clone(), underruns.clone());
            let out_name = out_dev_name.clone();
            threads.push(std::thread::spawn(move || {
                if let Err(e) = render_loop(&out_name, in_rate, out_rate, out_bits, &stop, &ring, &status, &underruns) {
                    status.lock().unwrap().message = format!("出力が止まりました: {e}");
                    stop.store(true, Ordering::Relaxed);
                }
            }));
        }
        Ok(Bridge { stop, status, peak, threads })
    }

    pub fn status(&self) -> BridgeStatus {
        let mut s = self.status.lock().unwrap().clone();
        s.running = !self.stop.load(Ordering::Relaxed);
        s.in_peak = f32::from_bits(self.peak.load(Ordering::Relaxed));
        s
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// 出力デバイスの一覧(UIの選択肢用)。
pub fn devices() -> Result<Vec<(String, bool)>, OutputError> {
    list_render_devices()
}

/// 受け取り: 出力デバイスのループバックを共有モードで開き、2ch・32bit浮動小数点でリングへ積む。
fn capture_loop(name: &str, rate: u32, stop: &AtomicBool, ring: &Mutex<VecDeque<f32>>, peak: &std::sync::atomic::AtomicU32) -> Result<(), OutputError> {
    initialize_mta().ok().map_err(werr)?;
    let enumerator = DeviceEnumerator::new().map_err(werr)?;
    let device = find_render_device(&enumerator, name)?;
    let mut client = device.get_iaudioclient().map_err(werr)?;
    let fmt = WaveFormat::new(32, 32, &SampleType::Float, rate as usize, CH, None);
    let (_def, min_period) = client.get_device_period().map_err(werr)?;
    // Directionをキャプチャにすると、出力デバイスはループバックとして開かれる。autoconvertで2chへ変換してもらう。
    client
        .initialize_client(&fmt, &Direction::Capture, &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min_period })
        .map_err(werr)?;
    let event = client.set_get_eventhandle().map_err(werr)?;
    let cap = client.get_audiocaptureclient().map_err(werr)?;
    client.start_stream().map_err(werr)?;
    let cap_limit = (rate as usize) * CH * 2; // 約2秒を上限(溜まりすぎたら古いほうを捨てる)
    let mut bytes: VecDeque<u8> = VecDeque::new();
    while !stop.load(Ordering::Relaxed) {
        // 再生中の音が無いとイベントが来ないので、タイムアウトは無音として扱い、ループを続ける。
        let _ = event.wait_for_event(100);
        cap.read_from_device_to_deque(&mut bytes).map_err(werr)?;
        if bytes.len() >= 4 {
            let mut r = ring.lock().unwrap();
            while bytes.len() >= 4 {
                let b = [bytes.pop_front().unwrap(), bytes.pop_front().unwrap(), bytes.pop_front().unwrap(), bytes.pop_front().unwrap()];
                let v = f32::from_le_bytes(b);
                if v.abs() > f32::from_bits(peak.load(Ordering::Relaxed)) {
                    peak.store(v.abs().to_bits(), Ordering::Relaxed);
                }
                r.push_back(v);
            }
            while r.len() > cap_limit {
                r.pop_front();
            }
        }
    }
    client.stop_stream().ok();
    Ok(())
}

/// 出力: リングから取り出してsinc変換し、排他モードのDACへ書く。リングの量に応じて変換比をわずかに調整し、
/// 受け取り側とDAC側の時計のずれで、バッファが空になる/溢れるのを防ぐ。
#[allow(clippy::too_many_arguments)]
fn render_loop(
    out_name: &str,
    in_rate: u32,
    out_rate: u32,
    bits: u16,
    stop: &AtomicBool,
    ring: &Mutex<VecDeque<f32>>,
    status: &Mutex<BridgeStatus>,
    underruns: &AtomicU64,
) -> Result<(), OutputError> {
    let dev = ExclusiveDevice::open_on(Some(out_name), out_rate, CH, bits)?;
    status.lock().unwrap().container_bits = (dev.container_bytes * 8) as u16;
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::Blackman2,
    };
    let base_ratio = out_rate as f64 / in_rate as f64;
    let mut rs = SincFixedIn::<f32>::new(base_ratio, 1.05, params, CHUNK, CH).map_err(|e| OutputError::Resample(e.to_string()))?;
    let target = (TARGET_SECS * in_rate as f64) as usize * CH;
    let mut primed = false;
    let mut out_q: VecDeque<f32> = VecDeque::new();
    let cbytes = dev.container_bytes;
    let (_n, _el) = dev.run(stop, |n, cbytes_run, chn, out| {
        debug_assert_eq!(cbytes, cbytes_run);
        // 必要な分(n フレーム)を作るまで、CHUNKフレームずつ入力を変換する
        while out_q.len() < n * chn {
            let (have, chunk): (usize, Option<Vec<f32>>) = {
                let mut r = ring.lock().unwrap();
                let have = r.len();
                if !primed && have < target {
                    (have, None)
                } else if have >= CHUNK * CH {
                    primed = true;
                    (have, Some(r.drain(..CHUNK * CH).collect()))
                } else {
                    (have, None)
                }
            };
            match chunk {
                Some(c) => {
                    // バッファが目標より多ければ少し速く消費(比を下げる)、少なければ遅く(比を上げる)。±0.2%まで。
                    let err = (have as f64 - target as f64) / target as f64;
                    let rel = (1.0 - err * 0.002).clamp(0.998, 1.002);
                    let _ = rs.set_resample_ratio_relative(rel, true);
                    let planar: Vec<Vec<f32>> = (0..CH).map(|ch| c.iter().skip(ch).step_by(CH).copied().collect()).collect();
                    if let Ok(o) = rs.process(&planar, None) {
                        for i in 0..o[0].len() {
                            for ch in &o {
                                out_q.push_back(ch[i]);
                            }
                        }
                    }
                }
                None => {
                    // 素材が足りない(音が無い/開始直後): 足りない分は無音。
                    let missing = n * chn - out_q.len();
                    out_q.extend(std::iter::repeat(0.0).take(missing));
                    if primed {
                        underruns.fetch_add((missing / chn) as u64, Ordering::Relaxed);
                    }
                }
            }
        }
        for _ in 0..n {
            for _c in 0..chn {
                let v = out_q.pop_front().unwrap_or(0.0);
                let s = quantize(v, bits as u32);
                if bits >= 32 {
                    if cbytes == 4 {
                        out.extend_from_slice(&s.to_le_bytes());
                    } else {
                        write_sample_24(out, s >> 8, cbytes);
                    }
                } else {
                    write_sample_24(out, s, cbytes);
                }
            }
        }
        if let Ok(mut st) = status.lock() {
            st.buffer_secs = ring.lock().map(|r| r.len() as f64 / (in_rate as f64 * CH as f64)).unwrap_or(0.0);
            st.underrun_frames = underruns.load(Ordering::Relaxed);
        }
        true
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_candidates_follow_the_family() {
        assert_eq!(output_rate_candidates(44_100), vec![352_800, 176_400, 88_200, 44_100]);
        assert_eq!(output_rate_candidates(48_000), vec![384_000, 192_000, 96_000, 48_000]);
        assert_eq!(output_rate_candidates(192_000), vec![384_000, 192_000]);
    }

    /// 実機テスト: 受け取り側の出力デバイスへ440Hzの音を共有モードで出し、橋渡しがそれを受け取って
    /// DAC(排他)へ高レート/32bitで送り直せることを確かめる。
    /// `OPEN_BAR_CAPTURE=PHL OPEN_BAR_OUTPUT=LOXJIE cargo test --release loopback_real -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn loopback_real_device_bridge() {
        use wasapi::{DeviceEnumerator, SampleType, WaveFormat};
        let cap = std::env::var("OPEN_BAR_CAPTURE").expect("OPEN_BAR_CAPTURE");
        let out = std::env::var("OPEN_BAR_OUTPUT").expect("OPEN_BAR_OUTPUT");
        println!("devices: {:?}", devices().unwrap());
        // 受け取り側のデバイスへ音を出すスレッド(共有モード)
        let stop_tone = Arc::new(AtomicBool::new(false));
        let st = stop_tone.clone();
        let cap2 = cap.clone();
        let tone = std::thread::spawn(move || {
            initialize_mta().ok().unwrap();
            let en = DeviceEnumerator::new().unwrap();
            let dev = find_render_device(&en, &cap2).unwrap();
            let mut c = dev.get_iaudioclient().unwrap();
            let mix = c.get_mixformat().unwrap();
            let rate = mix.get_samplespersec() as usize;
            let fmt = WaveFormat::new(32, 32, &SampleType::Float, rate, 2, None);
            let (_d, min) = c.get_device_period().unwrap();
            c.initialize_client(&fmt, &Direction::Render, &StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: min }).unwrap();
            let ev = c.set_get_eventhandle().unwrap();
            let r = c.get_audiorenderclient().unwrap();
            c.start_stream().unwrap();
            let mut ph = 0.0f64;
            while !st.load(Ordering::Relaxed) {
                let n = c.get_available_space_in_frames().unwrap() as usize;
                let mut buf = Vec::with_capacity(n * 8);
                for _ in 0..n {
                    let v = (ph.sin() * 0.3) as f32;
                    ph += 2.0 * std::f64::consts::PI * 440.0 / rate as f64;
                    buf.extend_from_slice(&v.to_le_bytes());
                    buf.extend_from_slice(&v.to_le_bytes());
                }
                r.write_to_device(n, &buf, None).unwrap();
                let _ = ev.wait_for_event(200);
            }
            c.stop_stream().ok();
        });
        std::thread::sleep(Duration::from_millis(500));
        let b = Bridge::start(&cap, &out, 0, 32).expect("bridge start");
        std::thread::sleep(Duration::from_secs(8));
        let s = b.status();
        println!("{s:#?}");
        b.stop();
        stop_tone.store(true, Ordering::Relaxed);
        tone.join().unwrap();
        assert!(s.in_peak > 0.05, "音が受け取れていない: {}", s.in_peak);
        assert!(s.out_rate_hz >= s.in_rate_hz);
        assert!(s.message.is_empty(), "{}", s.message);
        assert!(s.buffer_secs > 0.05 && s.buffer_secs < 1.0, "バッファが安定していない: {}", s.buffer_secs);
    }

    #[test]
    fn refuses_same_device_for_capture_and_output() {
        let r = Bridge::start("DAC", "dac", 0, 32);
        assert!(r.is_err());
    }
}
