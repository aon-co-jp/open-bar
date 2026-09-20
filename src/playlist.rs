//! プレイリスト: ギャップレス連結とReplayGain。
//!
//! - **ギャップレス**: 全トラックをデコードして共通のレート・チャンネル数の1本のPCMへ連結する(トラック間に無音が入らない)。
//!   レートが違うトラックは、先頭トラックのレートへ高品質リサンプルする。全曲をメモリに載せる単純方式のため、
//!   長い/多数のトラックでは大量のメモリを使う(逐次デコードは今後)。
//! - **ReplayGain**: タグ`REPLAYGAIN_TRACK_GAIN`(dB)を読み、音量を揃える。**ビットパーフェクト再生ではゲインを掛けない**。

use crate::output::{resample, OutputError};
use crate::pcm::{decode_file, Pcm, PcmError};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlaylistError {
    #[error(transparent)]
    Decode(#[from] PcmError),
    #[error(transparent)]
    Resample(#[from] OutputError),
    #[error("プレイリストが空です")]
    Empty,
}

/// ファイル先頭のタグ領域から`REPLAYGAIN_TRACK_GAIN`(dB)を探す(FLAC/Ogg/MP3(TXXX)/MKAのVorbisコメント形式に対応)。
pub fn read_track_gain_db(path: &str) -> Option<f64> {
    use std::io::Read;
    let mut buf = vec![0u8; 262_144];
    let n = std::fs::File::open(path).and_then(|mut f| f.read(&mut buf)).ok()?;
    parse_gain(&buf[..n])
}

pub fn parse_gain(bytes: &[u8]) -> Option<f64> {
    let key = b"REPLAYGAIN_TRACK_GAIN";
    let pos = bytes.windows(key.len()).position(|w| w.eq_ignore_ascii_case(key))?;
    let rest = &bytes[pos + key.len()..];
    // "=" または NUL区切りの後の数値("-6.50 dB")
    let start = rest.iter().position(|b| b.is_ascii_digit() || *b == b'-' || *b == b'+')?;
    let text: String = rest[start..].iter().take(12).take_while(|b| b.is_ascii_digit() || matches!(**b, b'-' | b'+' | b'.')).map(|b| *b as char).collect();
    text.parse().ok()
}

/// dBを振幅倍率へ。
pub fn gain_to_amplitude(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

/// 複数ファイルをギャップレスな1本のPCMへ連結する。`replay_gain`がtrueなら各トラックへタグのゲインを適用する。
pub fn load_gapless(paths: &[String], replay_gain: bool) -> Result<Pcm, PlaylistError> {
    let mut iter = paths.iter();
    let first = iter.next().ok_or(PlaylistError::Empty)?;
    let mut out = decode_file(first)?;
    if replay_gain {
        apply_gain(&mut out, read_track_gain_db(first));
    }
    for p in iter {
        let mut t = decode_file(p)?;
        if replay_gain {
            apply_gain(&mut t, read_track_gain_db(p));
        }
        let samples = if t.sample_rate != out.sample_rate { resample(&t.samples, t.channels, t.sample_rate, out.sample_rate)? } else { t.samples };
        // チャンネル数が違う場合は、少ない方に合わせず先頭トラックの数へ揃える(モノラル→複製、多い分は切り捨て)
        let samples = if t.channels != out.channels { remap(&samples, t.channels, out.channels) } else { samples };
        out.samples.extend_from_slice(&samples);
    }
    Ok(out)
}

fn apply_gain(p: &mut Pcm, db: Option<f64>) {
    if let Some(db) = db {
        let g = gain_to_amplitude(db);
        p.samples.iter_mut().for_each(|s| *s *= g);
    }
}

fn remap(samples: &[f32], from: usize, to: usize) -> Vec<f32> {
    let frames = samples.len() / from;
    let mut out = Vec::with_capacity(frames * to);
    for f in 0..frames {
        for c in 0..to {
            out.push(if from == 1 { samples[f] } else if c < from { samples[f * from + c] } else { 0.0 });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_tag_is_parsed_from_vorbis_comment_style_bytes() {
        assert_eq!(parse_gain(b"xx\x00REPLAYGAIN_TRACK_GAIN=-6.50 dB\x00yy"), Some(-6.5));
        assert_eq!(parse_gain(b"replaygain_track_gain=+3.2 dB"), Some(3.2));
        assert_eq!(parse_gain(b"nothing here"), None);
        assert!((gain_to_amplitude(-6.0206) - 0.5).abs() < 1e-3);
    }

    #[test]
    fn remap_duplicates_mono_and_truncates_extra_channels() {
        assert_eq!(remap(&[0.1, 0.2], 1, 2), vec![0.1, 0.1, 0.2, 0.2]);
        assert_eq!(remap(&[1.0, 2.0, 3.0, 4.0], 2, 1), vec![1.0, 3.0]);
    }
}
