//! 音声出力(第1段階: 共有モード)。cpalで既定の出力デバイスへ再生し、デバイスが対応しないサンプルレートは
//! rubatoの高品質sincリサンプラで変換する。**排他モード(WASAPI)・DoP・ASIO DSDは次の段階**(README参照)。
//! 共有モードはOSのミキサーを通るため、ビットパーフェクトではない(DoP・MQA素通しには使えない)。

use crate::pcm::Pcm;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OutputError {
    #[error("出力デバイスがありません")]
    NoDevice,
    #[error("デバイスの設定を取得できません: {0}")]
    Config(String),
    #[error("再生できません: {0}")]
    Stream(String),
    #[error("リサンプルに失敗しました: {0}")]
    Resample(String),
}

#[derive(Debug, Clone)]
pub struct PlayOpts {
    /// 音量(0.0〜1.0)。1.0以外はビットパーフェクトではない。
    pub volume: f32,
    /// 先頭から再生する最大秒数(省略で全部)。
    pub max_seconds: Option<f64>,
}

impl Default for PlayOpts {
    fn default() -> Self {
        PlayOpts { volume: 1.0, max_seconds: None }
    }
}

#[derive(Debug, Clone)]
pub struct PlayReport {
    pub device: String,
    pub device_rate_hz: u32,
    pub device_channels: u16,
    pub resampled: bool,
    /// デバイスが実際に消費したフレーム数。
    pub frames_consumed: u64,
    pub elapsed_secs: f64,
}

/// アップサンプル(リサンプル)フィルターの選択。聴き比べ用に特性の違う3種類を用意する(いずれもリニア位相のsinc補間)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResampleFilter {
    /// 標準: 256タップ、通過帯域は原音のナイキストの95%まで。バランス型。
    #[default]
    Standard,
    /// シャープ: 1024タップ・カットオフ99%。通過帯域が平坦で急峻(プリリンギングの範囲は長くなる)。計算量は大きい。
    Sharp,
    /// ソフト: 緩やかなロールオフ(カットオフ90%)。急峻なフィルターより時間軸のにじみ(リンギング)が短い。
    Soft,
    /// カスタム: カットオフを原音のナイキストに対する百分率(80〜99)で指定する(256タップ)。標準(95)とソフト(90)の間を細かく試せる。
    Custom(u8),
}

pub fn sinc_params(f: ResampleFilter) -> SincInterpolationParameters {
    match f {
        ResampleFilter::Standard => SincInterpolationParameters { sinc_len: 256, f_cutoff: 0.95, interpolation: SincInterpolationType::Cubic, oversampling_factor: 256, window: WindowFunction::BlackmanHarris2 },
        // 1024タップ・512倍オーバーサンプルは重すぎて実時間に間に合わず、音切れの原因になった(実機の聴取で判明)ため、512タップにした
        ResampleFilter::Sharp => SincInterpolationParameters { sinc_len: 512, f_cutoff: 0.99, interpolation: SincInterpolationType::Cubic, oversampling_factor: 256, window: WindowFunction::BlackmanHarris2 },
        ResampleFilter::Custom(pct) => SincInterpolationParameters { sinc_len: 256, f_cutoff: (pct.clamp(80, 99) as f32) / 100.0, interpolation: SincInterpolationType::Cubic, oversampling_factor: 256, window: WindowFunction::BlackmanHarris2 },
        ResampleFilter::Soft => SincInterpolationParameters { sinc_len: 256, f_cutoff: 0.90, interpolation: SincInterpolationType::Cubic, oversampling_factor: 256, window: WindowFunction::Blackman2 },
    }
}

/// 整数倍アップサンプル用のポリフェーズ設計(位相あたりタップ数M、元のナイキストに対するカットオフ)。フィルターの種類ごと。
pub fn poly_design(f: ResampleFilter) -> (usize, f64) {
    match f {
        ResampleFilter::Standard => (128, 0.95),
        ResampleFilter::Sharp => (256, 0.99),
        ResampleFilter::Soft => (96, 0.90),
        ResampleFilter::Custom(pct) => (128, pct.clamp(80, 99) as f64 / 100.0),
    }
}

/// `from`→`to`が整数倍(2〜32倍)ならその倍率。
pub fn integer_ratio(from: u32, to: u32) -> Option<usize> {
    (from > 0 && to > from && to % from == 0 && (to / from) <= 32).then(|| (to / from) as usize)
}

/// このフィルターで`from`→`to`Hzの変換が、実時間の何倍の速さで進むかを実測する(1.0未満なら音切れする)。
pub fn filter_realtime_factor(from: u32, to: u32, channels: usize, filter: ResampleFilter) -> f64 {
    if from == to {
        return f64::INFINITY;
    }
    let frames = (from as usize) / 2; // 0.5秒ぶんの擬似信号
    let mut x = 0x2545F4914F6CDD1Du64;
    let samples: Vec<f32> = (0..frames * channels)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            ((x >> 40) as f32 / 8_388_608.0 - 1.0) * 0.3
        })
        .collect();
    let t0 = std::time::Instant::now();
    if let Some(l) = integer_ratio(from, to) {
        let (m, cutoff) = poly_design(filter);
        for c in 0..channels {
            let mono: Vec<f32> = samples.iter().skip(c).step_by(channels).copied().collect();
            let _ = crate::polyphase::Upsampler::new(l, m, cutoff, 9.0).process(&mono);
        }
    } else {
        let _ = resample_with(&samples, channels, from, to, filter);
    }
    0.5 / t0.elapsed().as_secs_f64().max(1e-6)
}

/// インターリーブしたf32を`from`Hz→`to`Hzへ高品質変換する(標準フィルター)。
pub fn resample(samples: &[f32], channels: usize, from: u32, to: u32) -> Result<Vec<f32>, OutputError> {
    resample_with(samples, channels, from, to, ResampleFilter::Standard)
}

/// フィルターを指定して変換する(sinc補間、長さは比に応じて変わる)。
pub fn resample_with(samples: &[f32], channels: usize, from: u32, to: u32, filter: ResampleFilter) -> Result<Vec<f32>, OutputError> {
    if from == to {
        return Ok(samples.to_vec());
    }
    let frames = samples.len() / channels;
    let params = sinc_params(filter);
    let chunk = 2048usize;
    let mut rs = SincFixedIn::<f32>::new(to as f64 / from as f64, 2.0, params, chunk, channels).map_err(|e| OutputError::Resample(e.to_string()))?;
    let planar: Vec<Vec<f32>> = (0..channels).map(|c| samples.iter().skip(c).step_by(channels).copied().collect()).collect();
    let mut out: Vec<Vec<f32>> = vec![Vec::new(); channels];
    let mut pos = 0;
    while pos + chunk <= frames {
        let block: Vec<&[f32]> = planar.iter().map(|c| &c[pos..pos + chunk]).collect();
        let r = rs.process(&block, None).map_err(|e| OutputError::Resample(e.to_string()))?;
        for (o, c) in out.iter_mut().zip(r) {
            o.extend(c);
        }
        pos += chunk;
    }
    if pos < frames {
        let block: Vec<&[f32]> = planar.iter().map(|c| &c[pos..]).collect();
        let r = rs.process_partial(Some(&block), None).map_err(|e| OutputError::Resample(e.to_string()))?;
        for (o, c) in out.iter_mut().zip(r) {
            o.extend(c);
        }
    }
    // フィルタの遅延分の末尾が残るので、理論長に切り詰める
    let want = (frames as f64 * to as f64 / from as f64).round() as usize;
    let n = out[0].len().min(want.max(1));
    let mut inter = Vec::with_capacity(n * channels);
    for i in 0..n {
        for c in &out {
            inter.push(c[i]);
        }
    }
    Ok(inter)
}

/// PCMのチャンネル数をデバイスのチャンネル数へ合わせる(モノラル→複製、多い分は切り捨て、足りない分は無音)。
pub fn map_channels_pub(samples: &[f32], from: usize, to: usize) -> Vec<f32> {
    map_channels(samples, from, to)
}

fn map_channels(samples: &[f32], from: usize, to: usize) -> Vec<f32> {
    if from == to {
        return samples.to_vec();
    }
    let frames = samples.len() / from;
    let mut out = Vec::with_capacity(frames * to);
    for f in 0..frames {
        for c in 0..to {
            let src = if from == 1 { 0 } else { c };
            out.push(if src < from { samples[f * from + src] } else { 0.0 });
        }
    }
    out
}

fn run_stream<T>(device: &cpal::Device, config: &StreamConfig, data: Arc<Vec<f32>>, total_frames: u64, ch: usize) -> Result<(u64, f64), OutputError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let pos = Arc::new(AtomicU64::new(0));
    let done = Arc::new(AtomicBool::new(false));
    let (pos_cb, done_cb, data_cb) = (pos.clone(), done.clone(), data.clone());
    let stream = device
        .build_output_stream(
            config,
            move |out: &mut [T], _| {
                let start = pos_cb.load(Ordering::Relaxed) as usize;
                let n = out.len() / ch;
                for f in 0..n {
                    for c in 0..ch {
                        let i = (start + f) * ch + c;
                        out[f * ch + c] = T::from_sample(if i < data_cb.len() { data_cb[i] } else { 0.0 });
                    }
                }
                let np = (start + n) as u64;
                pos_cb.store(np, Ordering::Relaxed);
                if np >= total_frames {
                    done_cb.store(true, Ordering::Relaxed);
                }
            },
            |e| eprintln!("出力ストリームのエラー: {e}"),
            None,
        )
        .map_err(|e| OutputError::Stream(e.to_string()))?;
    let t0 = std::time::Instant::now();
    stream.play().map_err(|e| OutputError::Stream(e.to_string()))?;
    while !done.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(10));
        if t0.elapsed().as_secs_f64() > total_frames as f64 / config.sample_rate.0 as f64 + 5.0 {
            return Err(OutputError::Stream("デバイスがデータを消費しません(タイムアウト)".into()));
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    drop(stream);
    Ok((pos.load(Ordering::Relaxed).min(total_frames), elapsed))
}

/// 既定の出力デバイスで再生する(再生が終わるまで戻らない)。
pub fn play_blocking(pcm: &Pcm, opts: &PlayOpts) -> Result<PlayReport, OutputError> {
    let host = cpal::default_host();
    let device = host.default_output_device().ok_or(OutputError::NoDevice)?;
    let name = device.name().unwrap_or_else(|_| "(名前不明)".into());
    let default = device.default_output_config().map_err(|e| OutputError::Config(e.to_string()))?;
    let dev_ch = default.channels();
    // 元のサンプルレートをそのまま受け付けるデバイス設定があればそれを使い、無ければ既定のレートへ変換する。
    let native_ok = device
        .supported_output_configs()
        .map_err(|e| OutputError::Config(e.to_string()))?
        .any(|c| c.channels() == dev_ch && c.min_sample_rate().0 <= pcm.sample_rate && pcm.sample_rate <= c.max_sample_rate().0 && c.sample_format() == default.sample_format());
    let dev_rate = if native_ok { pcm.sample_rate } else { default.sample_rate().0 };
    let mut samples = pcm.samples.clone();
    if let Some(max) = opts.max_seconds {
        let keep = (max * pcm.sample_rate as f64) as usize * pcm.channels;
        samples.truncate(keep.min(samples.len()));
    }
    if (opts.volume - 1.0).abs() > f32::EPSILON {
        samples.iter_mut().for_each(|s| *s *= opts.volume);
    }
    let resampled = dev_rate != pcm.sample_rate;
    let samples = resample(&samples, pcm.channels, pcm.sample_rate, dev_rate)?;
    let samples = map_channels(&samples, pcm.channels, dev_ch as usize);
    let total = (samples.len() / dev_ch as usize) as u64;
    let config = StreamConfig { channels: dev_ch, sample_rate: cpal::SampleRate(dev_rate), buffer_size: cpal::BufferSize::Default };
    let data = Arc::new(samples);
    let (frames, elapsed) = match default.sample_format() {
        SampleFormat::F32 => run_stream::<f32>(&device, &config, data, total, dev_ch as usize)?,
        SampleFormat::I16 => run_stream::<i16>(&device, &config, data, total, dev_ch as usize)?,
        SampleFormat::I32 => run_stream::<i32>(&device, &config, data, total, dev_ch as usize)?,
        other => return Err(OutputError::Config(format!("未対応のデバイス形式: {other:?}"))),
    };
    Ok(PlayReport { device: name, device_rate_hz: dev_rate, device_channels: dev_ch, resampled, frames_consumed: frames, elapsed_secs: elapsed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, secs: f64, amp: f32) -> Vec<f32> {
        (0..(rate as f64 * secs) as usize).map(|i| amp * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / rate as f32).sin()).collect()
    }

    fn amplitude(x: &[f32], rate: u32) -> f64 {
        let n = x.len().min(rate as usize / 2);
        let start = x.len() / 4; // フィルタの立ち上がりを避ける
        let seg = &x[start..start + n.min(x.len() - start)];
        let (mut s, mut c) = (0.0, 0.0);
        for (i, v) in seg.iter().enumerate() {
            let ph = 2.0 * std::f64::consts::PI * 1000.0 * (start + i) as f64 / rate as f64;
            s += *v as f64 * ph.sin();
            c += *v as f64 * ph.cos();
        }
        2.0 * (s * s + c * c).sqrt() / seg.len() as f64
    }

    #[test]
    fn resampling_keeps_the_tone_amplitude_and_scales_the_length() {
        let x = tone(48_000, 1.0, 0.5);
        let y = resample(&x, 1, 48_000, 44_100).unwrap();
        assert!((y.len() as f64 - 44_100.0).abs() < 8.0, "長さ: {}", y.len());
        assert!((amplitude(&y, 44_100) - 0.5).abs() < 0.01, "振幅: {}", amplitude(&y, 44_100));
        let z = resample(&x, 1, 48_000, 192_000).unwrap();
        assert!((z.len() as f64 - 192_000.0).abs() < 8.0);
        assert!((amplitude(&z, 192_000) - 0.5).abs() < 0.01);
    }

    #[test]
    fn same_rate_is_untouched_and_channels_are_mapped() {
        let x = vec![0.1, 0.2, 0.3];
        assert_eq!(resample(&x, 1, 48_000, 48_000).unwrap(), x);
        assert_eq!(map_channels(&[0.5, -0.5], 1, 2), vec![0.5, 0.5, -0.5, -0.5]);
        assert_eq!(map_channels(&[1.0, 2.0, 3.0, 4.0], 2, 4), vec![1.0, 2.0, 0.0, 0.0, 3.0, 4.0, 0.0, 0.0]);
        assert_eq!(map_channels(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 3, 2), vec![1.0, 2.0, 4.0, 5.0]);
    }

    /// フィルターごとの実測特性: 通過帯域(20kHz)のゲインと、1kHzの鏡像(43.1kHz)の抑圧量を測る。
    #[test]
    fn upsampling_filters_have_the_advertised_passband_and_image_rejection() {
        let goertzel = |x: &[f32], rate: u32, freq: f64| -> f64 {
            let (mut s, mut c) = (0.0, 0.0);
            for (i, v) in x.iter().enumerate() {
                let ph = 2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64;
                s += *v as f64 * ph.sin();
                c += *v as f64 * ph.cos();
            }
            2.0 * (s * s + c * c).sqrt() / x.len() as f64
        };
        let db = |a: f64| 20.0 * a.max(1e-12).log10();
        for filter in [ResampleFilter::Standard, ResampleFilter::Sharp, ResampleFilter::Soft] {
            let measure = |freq: f64, at: f64| -> f64 {
                let x: Vec<f32> = (0..88_200).map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq as f32 * i as f32 / 44_100.0).sin()).collect();
                let y = resample_with(&x, 1, 44_100, 352_800, filter).unwrap();
                let seg = &y[y.len() / 4..y.len() * 3 / 4]; // フィルターの立ち上がり・終端を避ける
                db(goertzel(seg, 352_800, at) / 0.5)
            };
            // 位相の連続性のため、セグメント開始位置ぶんの周波数はそのまま(振幅のみ比較)
            let pass20 = measure(20_000.0, 20_000.0);
            let pass1 = measure(1_000.0, 1_000.0);
            let image = measure(1_000.0, 43_100.0);
            eprintln!("{filter:?}: 1kHz={pass1:.2}dB 20kHz={pass20:.2}dB 鏡像(43.1kHz)={image:.1}dB");
            assert!(pass1.abs() < 0.05, "{filter:?} 1kHzは平坦: {pass1}");
            assert!(image < -90.0, "{filter:?} 鏡像の抑圧が不足: {image}");
            match filter {
                ResampleFilter::Sharp => assert!(pass20.abs() < 0.1, "シャープは20kHzまで平坦: {pass20}"),
                ResampleFilter::Standard => assert!(pass20.abs() < 0.5, "標準も20kHzはほぼ平坦: {pass20}"),
                ResampleFilter::Soft => assert!(pass20 < -0.5, "ソフトは20kHzを緩やかに落とす: {pass20}"),
                ResampleFilter::Custom(_) => {}
            }
        }
    }
}
