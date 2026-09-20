//! 再生計画: 出力先ハードウェアの能力から「そのまま送る/DoPにする/自動でPCMへ変換する」を決める。
//!
//! 「ハードウェアが対応していればDSDはそのまま再生、していなければ自動的にPCM変換して再生」を、
//! 再生ソフト側の責務として明示的なロジックにしたもの。MQAは復号せず、MQA対応DACがあり、ビットパーフェクトが
//! 保てるときだけ素通し(展開はDAC)。それ以外は通常のPCMとして鳴らす(MQAの高域展開は行われない、と明記)。

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DsdSupport {
    /// DSD非対応(PCMのみ)。
    None,
    /// DoP(DSD over PCM)対応。WASAPI排他/ALSA等のPCM経路でDSDを運ぶ。
    Dop,
    /// ネイティブDSD対応(ASIO DSD / USB DSD Audio Class)。
    Native,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct DeviceCaps {
    /// 出力できるPCMの最大サンプルレート(Hz)。
    pub max_pcm_rate_hz: u32,
    /// 出力できるPCMの最大ビット深度。
    pub max_bits: u32,
    /// 排他モード(共有ミキサーを通さない)で開けるか。DoP・MQA素通しには必須。
    pub exclusive_mode: bool,
    pub dsd: DsdSupport,
    /// MQAデコーダ(展開)を持つDAC/機器か。
    pub mqa_decoder: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Pcm { rate_hz: u32, bits: u32 },
    Dsd { rate_hz: u32 },
    Mqa { rate_hz: u32, bits: u32 },
}

#[derive(Debug, Clone, Copy)]
pub struct PlayOptions {
    /// 音量調整・EQ・リサンプルなどのDSPを一切かけない(ビットパーフェクト)。
    pub bit_perfect: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum OutputPlan {
    Pcm { rate_hz: u32, bits: u32, resampled: bool, bit_depth_reduced: bool },
    DsdNative { rate_hz: u32 },
    DsdDop { pcm_rate_hz: u32 },
    /// DSDを内部でPCMへ変換して送る(DSD非対応の機器向けの自動フォールバック)。
    DsdToPcm { pcm_rate_hz: u32 },
    /// MQA対応DACへビットパーフェクトで素通し(展開はDAC側)。
    MqaPassthrough { rate_hz: u32, bits: u32 },
    /// MQAデコーダが無い/ビットパーフェクトを保てないため、通常のPCMとして再生(MQAの展開は行われない)。
    MqaAsPlainPcm { rate_hz: u32, bits: u32, reason: &'static str },
    Unplayable { reason: String },
}

/// DSDレートから、8の倍数の間引き率で得られるPCMレートの候補(大きい順、例: DSD64→[352800,176400,88200,44100])。
pub fn dsd_pcm_candidates(dsd_rate_hz: u32) -> Vec<u32> {
    let mut v = Vec::new();
    let mut r = 8u32;
    while dsd_rate_hz / r >= 44_100 && dsd_rate_hz % r == 0 {
        v.push(dsd_rate_hz / r);
        r *= 2;
    }
    v
}

pub fn plan(src: SourceKind, caps: DeviceCaps, opts: PlayOptions) -> OutputPlan {
    match src {
        SourceKind::Dsd { rate_hz } => {
            if caps.dsd == DsdSupport::Native {
                return OutputPlan::DsdNative { rate_hz };
            }
            let dop_rate = rate_hz / 16;
            if caps.dsd == DsdSupport::Dop && caps.exclusive_mode && caps.max_bits >= 24 && dop_rate <= caps.max_pcm_rate_hz && opts.bit_perfect {
                return OutputPlan::DsdDop { pcm_rate_hz: dop_rate };
            }
            match dsd_pcm_candidates(rate_hz).into_iter().find(|r| *r <= caps.max_pcm_rate_hz) {
                Some(r) => OutputPlan::DsdToPcm { pcm_rate_hz: r },
                None => OutputPlan::Unplayable { reason: format!("この機器の最大PCMレート{}HzではDSDをPCM化できません", caps.max_pcm_rate_hz) },
            }
        }
        SourceKind::Mqa { rate_hz, bits } => {
            if caps.mqa_decoder && caps.exclusive_mode && opts.bit_perfect {
                OutputPlan::MqaPassthrough { rate_hz, bits }
            } else {
                let reason = if !caps.mqa_decoder {
                    "MQAデコーダ対応の機器ではありません"
                } else if !caps.exclusive_mode {
                    "排他モードで開けないためビットパーフェクトを保てません"
                } else {
                    "ビットパーフェクト(DSP無し)が指定されていません"
                };
                match plan(SourceKind::Pcm { rate_hz, bits }, caps, opts) {
                    OutputPlan::Pcm { rate_hz, bits, .. } => OutputPlan::MqaAsPlainPcm { rate_hz, bits, reason },
                    other => other,
                }
            }
        }
        SourceKind::Pcm { rate_hz, bits } => {
            let mut rate = rate_hz;
            while rate > caps.max_pcm_rate_hz && rate % 2 == 0 && rate / 2 >= 8_000 {
                rate /= 2; // 同じ系列(44.1k/48k系)を保ったまま下げる
            }
            if rate > caps.max_pcm_rate_hz {
                return OutputPlan::Unplayable { reason: format!("{rate_hz}Hzをこの機器の最大{}Hz以下にできません", caps.max_pcm_rate_hz) };
            }
            let out_bits = bits.min(caps.max_bits);
            OutputPlan::Pcm { rate_hz: rate, bits: out_bits, resampled: rate != rate_hz, bit_depth_reduced: out_bits != bits }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(dsd: DsdSupport, max_rate: u32) -> DeviceCaps {
        DeviceCaps { max_pcm_rate_hz: max_rate, max_bits: 32, exclusive_mode: true, dsd, mqa_decoder: false }
    }
    const BP: PlayOptions = PlayOptions { bit_perfect: true };

    #[test]
    fn dsd_is_sent_as_is_when_the_hardware_supports_it() {
        assert_eq!(plan(SourceKind::Dsd { rate_hz: 11_289_600 }, caps(DsdSupport::Native, 384_000), BP), OutputPlan::DsdNative { rate_hz: 11_289_600 });
        // DoP: DSD256は705.6kHzのPCMが必要
        assert_eq!(plan(SourceKind::Dsd { rate_hz: 11_289_600 }, caps(DsdSupport::Dop, 768_000), BP), OutputPlan::DsdDop { pcm_rate_hz: 705_600 });
    }

    #[test]
    fn dsd_falls_back_to_pcm_automatically_when_unsupported_or_too_fast_for_dop() {
        assert_eq!(plan(SourceKind::Dsd { rate_hz: 2_822_400 }, caps(DsdSupport::None, 192_000), BP), OutputPlan::DsdToPcm { pcm_rate_hz: 176_400 });
        // DoPに対応していても、DSD256のDoP(705.6k)を出せない機器では352.8kのPCMへ
        assert_eq!(plan(SourceKind::Dsd { rate_hz: 11_289_600 }, caps(DsdSupport::Dop, 384_000), BP), OutputPlan::DsdToPcm { pcm_rate_hz: 352_800 });
        // 共有モード(DoPが壊れる)や非ビットパーフェクト指定でもPCMへ
        let shared = DeviceCaps { exclusive_mode: false, ..caps(DsdSupport::Dop, 768_000) };
        assert_eq!(plan(SourceKind::Dsd { rate_hz: 2_822_400 }, shared, BP), OutputPlan::DsdToPcm { pcm_rate_hz: 352_800 });
        assert!(matches!(plan(SourceKind::Dsd { rate_hz: 2_822_400 }, caps(DsdSupport::Dop, 768_000), PlayOptions { bit_perfect: false }), OutputPlan::DsdToPcm { .. }));
    }

    #[test]
    fn dsd_pcm_candidates_are_byte_aligned_44k_multiples() {
        assert_eq!(dsd_pcm_candidates(2_822_400), vec![352_800, 176_400, 88_200, 44_100]);
        assert_eq!(dsd_pcm_candidates(11_289_600), vec![1_411_200, 705_600, 352_800, 176_400, 88_200, 44_100]);
    }

    #[test]
    fn mqa_is_passed_through_only_with_a_capable_dac_and_bit_perfect_path() {
        let dac = DeviceCaps { mqa_decoder: true, ..caps(DsdSupport::None, 384_000) };
        assert_eq!(plan(SourceKind::Mqa { rate_hz: 44_100, bits: 24 }, dac, BP), OutputPlan::MqaPassthrough { rate_hz: 44_100, bits: 24 });
        assert!(matches!(plan(SourceKind::Mqa { rate_hz: 44_100, bits: 24 }, caps(DsdSupport::None, 384_000), BP), OutputPlan::MqaAsPlainPcm { .. }));
        assert!(matches!(plan(SourceKind::Mqa { rate_hz: 44_100, bits: 24 }, dac, PlayOptions { bit_perfect: false }), OutputPlan::MqaAsPlainPcm { .. }));
    }

    #[test]
    fn pcm_is_reduced_within_the_same_rate_family_only_when_needed() {
        let c = DeviceCaps { max_bits: 24, ..caps(DsdSupport::None, 192_000) };
        assert_eq!(plan(SourceKind::Pcm { rate_hz: 384_000, bits: 32 }, c, BP), OutputPlan::Pcm { rate_hz: 192_000, bits: 24, resampled: true, bit_depth_reduced: true });
        assert_eq!(plan(SourceKind::Pcm { rate_hz: 352_800, bits: 24 }, c, BP), OutputPlan::Pcm { rate_hz: 176_400, bits: 24, resampled: true, bit_depth_reduced: false });
        assert_eq!(plan(SourceKind::Pcm { rate_hz: 44_100, bits: 16 }, c, BP), OutputPlan::Pcm { rate_hz: 44_100, bits: 16, resampled: false, bit_depth_reduced: false });
    }
}
