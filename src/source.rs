//! 再生ソース: ランダムアクセスできるPCM供給元。プレーヤーはこれを先読みしながら読み、シークもできる。
//!
//! - [`PcmSource`]: デコード済みPCM(全体をメモリに持つ)。
//! - [`DsdPcmSource`]: DSDを**必要な範囲だけ**逐次にPCMへ変換する(全曲の変換を待たずに再生を始められる)。
//! - [`DopSource`]: DSDをDoPの24bitワード(±1.0に正規化、整数値は厳密)として供給する。ビットパーフェクトの排他出力用。

use crate::pcm::Pcm;
use open_mqa_dsd::{Decimator, DsdStream, DsdToPcm};

pub trait Source: Send + Sync {
    fn rate_hz(&self) -> u32;
    fn channels(&self) -> usize;
    fn total_frames(&self) -> u64;
    /// `start`フレームから最大`frames`フレームを、インターリーブしたf32で返す(範囲外は返さない)。
    fn read(&self, start: u64, frames: usize) -> Vec<f32>;
    /// 種別の表示名(例: `PCM`、`DSD→PCM`、`DoP`)。
    fn kind(&self) -> String;
    fn duration_secs(&self) -> f64 {
        self.total_frames() as f64 / self.rate_hz().max(1) as f64
    }
}

pub struct PcmSource {
    pcm: Pcm,
    kind: String,
}

impl PcmSource {
    pub fn new(pcm: Pcm, kind: impl Into<String>) -> Self {
        PcmSource { pcm, kind: kind.into() }
    }
}

impl Source for PcmSource {
    fn rate_hz(&self) -> u32 {
        self.pcm.sample_rate
    }
    fn channels(&self) -> usize {
        self.pcm.channels
    }
    fn total_frames(&self) -> u64 {
        (self.pcm.samples.len() / self.pcm.channels.max(1)) as u64
    }
    fn read(&self, start: u64, frames: usize) -> Vec<f32> {
        let ch = self.pcm.channels;
        let total = self.total_frames();
        if start >= total {
            return Vec::new();
        }
        let n = (frames as u64).min(total - start) as usize;
        self.pcm.samples[start as usize * ch..(start as usize + n) * ch].to_vec()
    }
    fn kind(&self) -> String {
        self.kind.clone()
    }
}

pub struct DsdPcmSource {
    stream: DsdStream,
    dec: Decimator,
    out_rate: u32,
}

impl DsdPcmSource {
    pub fn new(stream: DsdStream, cfg: DsdToPcm) -> Result<Self, open_mqa_dsd::DsdError> {
        let dec = Decimator::new(stream.rate_hz, cfg)?;
        Ok(DsdPcmSource { stream, dec, out_rate: cfg.out_rate_hz })
    }
}

impl Source for DsdPcmSource {
    fn rate_hz(&self) -> u32 {
        self.out_rate
    }
    fn channels(&self) -> usize {
        self.stream.channels.len()
    }
    fn total_frames(&self) -> u64 {
        self.stream.channels.first().map(|c| self.dec.output_len(c) as u64).unwrap_or(0)
    }
    fn read(&self, start: u64, frames: usize) -> Vec<f32> {
        let total = self.total_frames();
        if start >= total {
            return Vec::new();
        }
        let n = (frames as u64).min(total - start) as usize;
        let planar: Vec<Vec<f32>> = std::thread::scope(|s| {
            let hs: Vec<_> = self
                .stream
                .channels
                .iter()
                .map(|bits| {
                    let dec = &self.dec;
                    s.spawn(move || {
                        let mut v = Vec::with_capacity(n);
                        dec.process_range(bits, start as usize, n, &mut v);
                        v
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().expect("decimator thread panicked")).collect()
        });
        let ch = planar.len();
        let mut out = Vec::with_capacity(n * ch);
        for i in 0..n {
            for c in &planar {
                out.push(c[i]);
            }
        }
        out
    }
    fn kind(&self) -> String {
        "DSD→PCM".to_string()
    }
}

/// DoPワード(24bit符号付き)を±1.0へ正規化する係数。2^23で割るので、整数→f32→整数の往復が厳密にできる。
pub const DOP_SCALE: f32 = 8_388_608.0;

pub struct DopSource {
    stream: DsdStream,
}

impl DopSource {
    pub fn new(stream: DsdStream) -> Self {
        DopSource { stream }
    }

    /// `idx`番目のDoPワード(チャンネル`c`)を符号付き24bit整数で返す。マーカーは0x05/0xFAを交互、末尾はDSD無音(0x69)で埋める。
    pub fn word(&self, c: usize, idx: usize) -> i32 {
        let bytes = &self.stream.channels[c];
        let marker: i32 = if idx % 2 == 0 { 0x05 } else { 0xFA };
        let b0 = bytes.get(2 * idx).copied().unwrap_or(0x69) as i32;
        let b1 = bytes.get(2 * idx + 1).copied().unwrap_or(0x69) as i32;
        ((marker << 16) | (b0 << 8) | b1) << 8 >> 8 // 24bitの符号拡張
    }
}

impl Source for DopSource {
    fn rate_hz(&self) -> u32 {
        self.stream.rate_hz / 16
    }
    fn channels(&self) -> usize {
        self.stream.channels.len()
    }
    fn total_frames(&self) -> u64 {
        self.stream.channels.first().map(|c| c.len().div_ceil(2) as u64).unwrap_or(0)
    }
    fn read(&self, start: u64, frames: usize) -> Vec<f32> {
        let total = self.total_frames();
        if start >= total {
            return Vec::new();
        }
        let n = (frames as u64).min(total - start) as usize;
        let ch = self.channels();
        let mut out = Vec::with_capacity(n * ch);
        for f in 0..n {
            for c in 0..ch {
                out.push(self.word(c, start as usize + f) as f32 / DOP_SCALE);
            }
        }
        out
    }
    fn kind(&self) -> String {
        "DoP".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream() -> DsdStream {
        DsdStream { rate_hz: 2_822_400, channels: vec![vec![0xAA, 0x55, 0x12, 0x34, 0xFF], vec![0x01, 0x02, 0x03, 0x04, 0x05]], sample_bits: 40 }
    }

    #[test]
    fn dop_source_words_round_trip_exactly_through_f32() {
        let s = DopSource::new(stream());
        assert_eq!(s.rate_hz(), 176_400);
        assert_eq!(s.total_frames(), 3, "5バイト→DoP 3フレーム(最後はDSD無音で埋める)");
        let v = s.read(0, 10);
        assert_eq!(v.len(), 6);
        // f32→整数(排他出力の変換と同じ)で、元のDoPワードに厳密に戻る
        let back: Vec<i32> = v.iter().map(|x| (x * DOP_SCALE).round() as i32).collect();
        assert_eq!(back[0], 0x05_AA_55);
        assert_eq!(back[1], 0x05_01_02);
        assert_eq!(back[2], (0xFA_12_34i32) << 8 >> 8, "マーカー0xFAは符号付き24bitで負の値");
        assert_eq!(back[4], 0x05_FF_69, "端数バイトはDSD無音0x69で埋まる");
        assert!(v.iter().all(|x| (-1.0..1.0).contains(x)));
    }

    #[test]
    fn dsd_pcm_source_reads_the_same_samples_regardless_of_chunking() {
        let mut ch = Vec::new();
        for i in 0..40_000u32 {
            ch.push((i.wrapping_mul(2654435761) >> 13) as u8);
        }
        let st = DsdStream { rate_hz: 2_822_400, channels: vec![ch.clone(), ch], sample_bits: 320_000 };
        let src = DsdPcmSource::new(st, DsdToPcm::default_for(2_822_400)).unwrap();
        let all = src.read(0, src.total_frames() as usize);
        let mut parts = Vec::new();
        let mut at = 0u64;
        for size in [100usize, 1, 999, 4000, 7] {
            parts.extend(src.read(at, size));
            at += size as u64;
        }
        assert_eq!(&all[..parts.len()], &parts[..], "シークしても逐次でも同じサンプル");
        assert_eq!(src.channels(), 2);
    }

    #[test]
    fn pcm_source_reads_ranges_and_stops_at_the_end() {
        let s = PcmSource::new(Pcm { sample_rate: 44_100, channels: 2, samples: (0..20).map(|x| x as f32).collect(), bits_per_sample: Some(16) }, "PCM");
        assert_eq!(s.total_frames(), 10);
        assert_eq!(s.read(8, 5), vec![16.0, 17.0, 18.0, 19.0]);
        assert!(s.read(10, 5).is_empty());
    }
}
