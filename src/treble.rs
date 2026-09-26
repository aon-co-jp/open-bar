//! 高音補正(MP3向け) / Treble restoration (for MP3 sources)
//!
//! MP3は低ビットレートだと高域(だいたい16kHz以上)をばっさり切り落とすことが多く、
//! こもった/こもって聞こえる原因になる。ここでは失われた倍音そのものを復元するのではなく、
//! 残っている高域(カットオフ付近〜可聴域上限)を穏やかにブーストするハイシェルフEQで
//! 「こもり」を軽減する、実用的な近似(教科書的な2次シェルフフィルタ)。
//!
//! MP3 at low bitrates typically discards content above ~16 kHz, which sounds muffled.
//! This does not reconstruct the lost harmonics; it's a practical approximation that
//! gently boosts the remaining high frequencies (a textbook 2nd-order high-shelf EQ)
//! to reduce that muffled impression.

/// 直接形I biquad(1チャンネル分の状態)。
#[derive(Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    /// RBJ Audio EQ Cookbookのハイシェルフ係数(Q=0.707固定、shelf_freq_hz以上をgain_dbだけ持ち上げる)。
    fn high_shelf(sample_rate: f32, shelf_freq_hz: f32, gain_db: f32) -> Biquad {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * (shelf_freq_hz / sample_rate).min(0.49);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let q = 0.707_f32;
        let alpha = sin_w0 / (2.0 * q);
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;

        let b0 = a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cos_w0);
        let a2 = (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha;

        Biquad { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0, x1: 0.0, x2: 0.0, y1: 0.0, y2: 0.0 }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2 - self.a1 * self.y1 - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// チャンネルごとに独立した状態を持つ高音補正フィルタ。
pub struct TrebleRestore {
    stages: Vec<Biquad>,
}

/// 補正を始める周波数(Hz)。128kbps前後のMP3で高域が落ち始める帯域を狙う。
pub const SHELF_FREQ_HZ: f32 = 13_000.0;
/// 持ち上げ量(dB)。強くしすぎるとサ行が耳障りになるため控えめ。
pub const GAIN_DB: f32 = 6.0;

impl TrebleRestore {
    pub fn new(sample_rate_hz: u32, channels: usize) -> TrebleRestore {
        let bq = Biquad::high_shelf(sample_rate_hz as f32, SHELF_FREQ_HZ, GAIN_DB);
        TrebleRestore { stages: vec![bq; channels.max(1)] }
    }

    /// インターリーブされた`samples`(chごとに1サンプルずつ交互)へその場でフィルタをかける。
    /// `channels`は`new()`に渡したチャンネル数と常に一致する前提(1トラックの再生中に
    /// 出力チャンネル数が変わることは無い)。
    pub fn process_interleaved(&mut self, samples: &mut [f32], channels: usize) {
        debug_assert_eq!(self.stages.len(), channels.max(1), "TrebleRestoreはnew()時のチャンネル数のまま使う");
        let ch_n = self.stages.len();
        for (i, s) in samples.iter_mut().enumerate() {
            *s = self.stages[i % ch_n].process(*s);
        }
    }
}

/// パスの拡張子がMP3かどうか(高音補正は既定でMP3ソースのときだけ有効にする判定に使う)。
pub fn is_mp3_path(path: &str) -> bool {
    std::path::Path::new(path).extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("mp3")).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: f32, freq: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate).sin()).collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn boosts_high_frequencies_and_leaves_bass_alone() {
        let rate = 44_100.0;
        let n = 8_192;

        let bass_in = tone(rate, 200.0, n);
        let treble_in = tone(rate, 18_000.0, n);

        let mut f_bass = TrebleRestore::new(44_100, 1);
        let mut f_treble = TrebleRestore::new(44_100, 1);
        let mut bass_out = bass_in.clone();
        let mut treble_out = treble_in.clone();
        f_bass.process_interleaved(&mut bass_out, 1);
        f_treble.process_interleaved(&mut treble_out, 1);

        // 立ち上がりの過渡応答を除いた後半で比較する
        let seg = n / 2..n;
        let bass_gain_db = 20.0 * (rms(&bass_out[seg.clone()]) / rms(&bass_in[seg.clone()])).log10();
        let treble_gain_db = 20.0 * (rms(&treble_out[seg.clone()]) / rms(&treble_in[seg.clone()])).log10();

        assert!(bass_gain_db.abs() < 0.5, "低音はほぼ変化なしのはず: {bass_gain_db}dB");
        assert!(treble_gain_db > GAIN_DB - 1.0, "高音は設計通り持ち上がるはず: {treble_gain_db}dB (狙い {GAIN_DB}dB)");
    }

    #[test]
    fn is_mp3_path_matches_extension_case_insensitively() {
        assert!(is_mp3_path("song.mp3"));
        assert!(is_mp3_path("SONG.MP3"));
        assert!(!is_mp3_path("song.flac"));
        assert!(!is_mp3_path("song"));
    }
}
