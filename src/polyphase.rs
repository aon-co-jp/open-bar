//! 整数倍アップサンプラ(ポリフェーズFIR)。44.1kHz→352.8kHz(8倍)のように、元レートの整数倍へ上げる変換専用。
//!
//! 汎用のsinc補間(rubato)は出力1サンプルごとに補間を挟むため重く、実機で音切れの原因になった(標準でも実時間の1.5倍)。
//! 整数倍ならポリフェーズ(位相ごとの短いFIR)で補間なしに計算でき、同じ品質で数倍速い。
//! 設計: Kaiser窓付きsincのローパス(カットオフは元のナイキストに対する割合)。全長N=L×M、遅延(N-1)/2は先頭で捨てて補正する。

/// 整数倍アップサンプラ(1チャンネル分の状態を持つ)。
pub struct Upsampler {
    l: usize,
    m: usize,
    /// 位相p(0..L)ごとの係数。畳み込みを内積にするため時間反転して並べる(長さM)。
    phases: Vec<Vec<f32>>,
    /// 直近M-1個の入力(先頭は0で初期化)。
    hist: Vec<f32>,
    /// 先頭で捨てる出力フレーム数(フィルターの遅延)。
    skip: usize,
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-14 * sum {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

/// プロトタイプFIR(長さN=L×M、DC利得=1になるよう正規化済み)を作る。`cutoff`は元のナイキストに対する割合(例: 0.95)。
pub fn design_prototype(l: usize, m: usize, cutoff: f64, beta: f64) -> Vec<f64> {
    let n = l * m;
    let mid = (n as f64 - 1.0) / 2.0;
    let fc = cutoff * 0.5 / l as f64; // 高いレートで正規化したカットオフ(周期/サンプル)
    let i0b = bessel_i0(beta);
    let mut h: Vec<f64> = (0..n)
        .map(|i| {
            let x = i as f64 - mid;
            let sinc = if x == 0.0 { 2.0 * fc } else { (2.0 * std::f64::consts::PI * fc * x).sin() / (std::f64::consts::PI * x) };
            let r = x / (mid + 0.5);
            sinc * bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b
        })
        .collect();
    // ゼロ挿入で下がる分(1/L)を補う: 各位相の係数和が1になるよう全体を正規化する
    let sum: f64 = h.iter().sum();
    h.iter_mut().for_each(|v| *v *= l as f64 / sum);
    h
}

impl Upsampler {
    /// `l`倍、位相あたり`m`タップ、カットオフ(元のナイキスト比)、Kaiserのβ。
    pub fn new(l: usize, m: usize, cutoff: f64, beta: f64) -> Upsampler {
        assert!(l >= 2 && m >= 2);
        let h = design_prototype(l, m, cutoff, beta);
        let phases: Vec<Vec<f32>> = (0..l).map(|p| (0..m).map(|k| h[(m - 1 - k) * l + p] as f32).collect()).collect();
        Upsampler { l, m, phases, hist: vec![0.0; m - 1], skip: (l * m - 1) / 2 }
    }

    pub fn ratio(&self) -> usize {
        self.l
    }

    /// 入力`x`(1チャンネル)を処理し、`x.len() * L`個(遅延ぶんの先頭を除く)の出力を返す。
    pub fn process(&mut self, x: &[f32]) -> Vec<f32> {
        let m = self.m;
        let mut buf = Vec::with_capacity(self.hist.len() + x.len());
        buf.extend_from_slice(&self.hist);
        buf.extend_from_slice(x);
        let mut out = Vec::with_capacity(x.len() * self.l);
        for t in 0..x.len() {
            let window = &buf[t..t + m]; // buf[t..t+m] = 直近M個の入力(古い→新しい)
            for ph in &self.phases {
                // 内積(コンパイラが自動でSIMD化しやすい単純なループ)
                let mut acc = 0.0f32;
                for (c, v) in ph.iter().zip(window) {
                    acc += c * v;
                }
                out.push(acc);
            }
        }
        // 履歴を更新(末尾のM-1個)
        let keep = m - 1;
        self.hist = buf[buf.len() - keep..].to_vec();
        // フィルターの遅延ぶんを先頭から捨てる
        if self.skip > 0 {
            let drop = self.skip.min(out.len());
            out.drain(..drop);
            self.skip -= drop;
        }
        out
    }

    /// 入力の終わりに呼ぶ: 遅延ぶんの0を流し込んで、残りの出力(末尾)を取り出す。
    pub fn flush(&mut self) -> Vec<f32> {
        let zeros = vec![0.0f32; self.m / 2 + 1];
        self.process(&zeros)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: f64, freq: f64, n: usize, amp: f32) -> Vec<f32> {
        (0..n).map(|i| amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin() as f32).collect()
    }

    fn goertzel(x: &[f32], rate: f64, freq: f64) -> f64 {
        let (mut s, mut c) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            let ph = 2.0 * std::f64::consts::PI * freq * i as f64 / rate;
            s += *v as f64 * ph.sin();
            c += *v as f64 * ph.cos();
        }
        2.0 * (s * s + c * c).sqrt() / x.len() as f64
    }

    #[test]
    fn dc_gain_is_one_and_the_prototype_is_symmetric() {
        let h = design_prototype(8, 64, 0.95, 9.0);
        let per_phase_sum: f64 = h.iter().sum::<f64>() / 8.0;
        assert!((per_phase_sum - 1.0).abs() < 1e-9, "各位相の直流利得は1: {per_phase_sum}");
        for i in 0..h.len() / 2 {
            assert!((h[i] - h[h.len() - 1 - i]).abs() < 1e-12, "リニア位相(対称)");
        }
    }

    #[test]
    fn output_length_and_tone_amplitude_are_preserved_across_streamed_blocks() {
        let x = tone(44_100.0, 1_000.0, 44_100, 0.5);
        let mut up = Upsampler::new(8, 64, 0.95, 9.0);
        let mut y = Vec::new();
        for block in x.chunks(2048) {
            y.extend(up.process(block));
        }
        y.extend(up.flush());
        assert!(y.len() >= x.len() * 8, "遅延を補正しても入力×8以上の長さが出る: {}", y.len());
        let seg = &y[y.len() / 4..y.len() * 3 / 4];
        // 信号は位相ずれなく元の1kHzのまま(遅延補正済み)。振幅は0.5
        let a = goertzel(seg, 352_800.0, 1_000.0);
        assert!((a - 0.5).abs() < 0.001, "振幅: {a}");
        // 遅延補正の確認: 出力の先頭付近が入力の位相と一致(x[0]=0付近から立ち上がる)
        assert!(y[0].abs() < 0.05 && y[8 * 3].abs() > 0.0, "先頭は入力と同じ位置から始まる");
    }

    #[test]
    fn image_rejection_and_passband_meet_the_targets() {
        let db = |a: f64| 20.0 * a.max(1e-12).log10();
        for (name, m, cutoff, min_image_db) in [("標準", 128usize, 0.95f64, 90.0f64), ("シャープ", 512, 0.99, 70.0)] {
            let x = tone(44_100.0, 1_000.0, 44_100, 0.5);
            let mut up = Upsampler::new(8, m, cutoff, 9.0);
            let mut y = up.process(&x);
            y.extend(up.flush());
            let seg = &y[y.len() / 4..y.len() * 3 / 4];
            let image = db(goertzel(seg, 352_800.0, 43_100.0) / 0.5);
            let x20 = tone(44_100.0, 20_000.0, 44_100, 0.5);
            let mut up2 = Upsampler::new(8, m, cutoff, 9.0);
            let mut y20 = up2.process(&x20);
            y20.extend(up2.flush());
            let seg20 = &y20[y20.len() / 4..y20.len() * 3 / 4];
            let pass20 = db(goertzel(seg20, 352_800.0, 20_000.0) / 0.5);
            eprintln!("{name}: M={m} 鏡像(43.1kHz)={image:.1}dB 20kHzゲイン={pass20:.2}dB");
            assert!(image < -min_image_db, "{name}: 鏡像の抑圧が不足 {image}");
            assert!(pass20.abs() < 0.3, "{name}: 20kHzが平坦でない {pass20}");
        }
    }

    #[test]
    fn it_is_much_faster_than_real_time() {
        let x = tone(44_100.0, 1_000.0, 44_100, 0.5);
        let mut up = Upsampler::new(8, 128, 0.95, 9.0);
        let t0 = std::time::Instant::now();
        let _ = up.process(&x);
        let one_channel = t0.elapsed().as_secs_f64();
        // ステレオ2ch分でも1秒の音声を十分速く(実時間の何倍か)
        let rtf = 1.0 / (one_channel * 2.0);
        eprintln!("ポリフェーズ標準(M=128): ステレオ実時間の {rtf:.1} 倍");
        assert!(rtf > 4.0, "実時間の4倍以上で動くはず: {rtf}");
    }
}
