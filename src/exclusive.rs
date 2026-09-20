//! WASAPI排他モードの出力(Windows)。OSのミキサー・リサンプラ・音量処理を通さず、デバイスへビットパーフェクトで送る。
//! PCM(16/24/32bit、任意レート)とDoP(DSD over PCM、24bit)に対応する。DoP・MQA素通しにはこれが必須。
//!
//! **DoPの安全上の注意**: DoP非対応のDACへDoPを送ると、DSDのビット列がそのままPCMとして鳴り、大音量のノイズになる。
//! DoP対応DACであることを確かめてから使うこと(CLIでも明示指定のときだけ動く)。

use crate::output::OutputError;
use wasapi::{initialize_mta, DeviceEnumerator, Direction, SampleType, StreamMode, WaveFormat};

#[derive(Debug, Clone)]
pub struct ExclusiveReport {
    pub device: String,
    pub rate_hz: u32,
    /// デバイスが受け付けたコンテナのビット幅(16/24/32)。
    pub container_bits: u16,
    pub channels: u16,
    pub frames_written: u64,
    pub elapsed_secs: f64,
}

fn werr<E: std::fmt::Display>(e: E) -> OutputError {
    OutputError::Stream(e.to_string())
}

/// 24bit符号付き整数のサンプル(`i32`の下位24bit)を、デバイスのコンテナ幅(3または4バイト)へ書く。
/// 4バイトは左詰め(`sample << 8`)、3バイトはそのままの24bit。**ビットパーフェクト**(値は一切変えない)。
pub fn write_sample_24(out: &mut Vec<u8>, sample24: i32, container_bytes: usize) {
    match container_bytes {
        3 => out.extend_from_slice(&sample24.to_le_bytes()[..3]),
        4 => out.extend_from_slice(&(sample24 << 8).to_le_bytes()),
        _ => unreachable!("24bitデータの出力コンテナは3または4バイト"),
    }
}

/// f32(±1.0)を`bits`ビットの整数へ丸める(TPDFなし、単純丸め。ビットパーフェクトが目的ならPCMは整数のまま渡すこと)。
fn quantize(v: f32, bits: u32) -> i32 {
    let max = ((1i64 << (bits - 1)) - 1) as f32;
    (v.clamp(-1.0, 1.0) * max).round() as i32
}

/// 排他モードで開き、`fill`にフレームを作らせて最後まで再生する共通処理。
/// `fill(start_frame, nframes, container_bytes, channels, out)`は`out`へ`nframes`フレーム分のバイト列を追加する。
fn run_exclusive<F>(rate_hz: u32, channels: u16, want_bits: u16, total_frames: u64, mut fill: F) -> Result<ExclusiveReport, OutputError>
where
    F: FnMut(u64, usize, usize, usize, &mut Vec<u8>),
{
    initialize_mta().ok().map_err(werr)?;
    let enumerator = DeviceEnumerator::new().map_err(werr)?;
    let device = enumerator.get_default_device(&Direction::Render).map_err(werr)?;
    let name = device.get_friendlyname().unwrap_or_else(|_| "(名前不明)".into());
    let mut client = device.get_iaudioclient().map_err(werr)?;
    // 希望の形式(有効ビット=want_bits、まず24/32bitコンテナを試す)を、デバイスが受け付ける形へ調整する。
    let desired = WaveFormat::new(want_bits.max(24) as usize, want_bits as usize, &SampleType::Int, rate_hz as usize, channels as usize, None);
    let fmt = client.is_supported_exclusive_with_quirks(&desired).map_err(|e| OutputError::Config(format!("排他モードで{rate_hz}Hz/{want_bits}bit/{channels}chを受け付けません: {e}")))?;
    let block = fmt.get_blockalign() as usize;
    let container_bytes = block / channels as usize;
    let (_def, min_period) = client.get_device_period().map_err(werr)?;
    let period = client.calculate_aligned_period_near(3 * min_period / 2, Some(128), &fmt).map_err(werr)?;
    client.initialize_client(&fmt, &Direction::Render, &StreamMode::EventsExclusive { period_hns: period }).map_err(|e| OutputError::Stream(format!("排他モードを開始できません(他のアプリが使用中、または排他モードが禁止されています): {e}")))?;
    let event = client.set_get_eventhandle().map_err(werr)?;
    let render = client.get_audiorenderclient().map_err(werr)?;
    let mut pos: u64 = 0;
    let mut write = |n: usize, pos: &mut u64| -> Result<(), OutputError> {
        let mut buf = Vec::with_capacity(n * block);
        fill(*pos, n, container_bytes, channels as usize, &mut buf);
        render.write_to_device(n, &buf, None).map_err(werr)?;
        *pos += n as u64;
        Ok(())
    };
    let first = client.get_available_space_in_frames().map_err(werr)? as usize;
    write(first, &mut pos)?;
    let t0 = std::time::Instant::now();
    client.start_stream().map_err(werr)?;
    while pos < total_frames + first as u64 {
        event.wait_for_event(1000).map_err(|_| OutputError::Stream("デバイスがデータを要求しません(タイムアウト)".into()))?;
        let n = client.get_available_space_in_frames().map_err(werr)? as usize;
        write(n, &mut pos)?;
    }
    // 末尾のバッファが鳴り終わるのを待つ
    std::thread::sleep(std::time::Duration::from_millis(200));
    let elapsed = t0.elapsed().as_secs_f64();
    client.stop_stream().map_err(werr)?;
    Ok(ExclusiveReport { device: name, rate_hz, container_bits: (container_bytes * 8) as u16, channels, frames_written: pos.min(total_frames), elapsed_secs: elapsed })
}

/// PCM(インターリーブf32、`bits`は16/24/32)を排他モードで再生する。
pub fn play_exclusive_pcm(samples: &[f32], channels: usize, rate_hz: u32, bits: u16, volume: f32) -> Result<ExclusiveReport, OutputError> {
    let total = (samples.len() / channels) as u64;
    let bits = bits.clamp(16, 24); // 32bitは有効24bit(f32の仮数は24bit)として扱う
    run_exclusive(rate_hz, channels as u16, bits, total, |start, n, cbytes, ch, out| {
        for f in 0..n {
            for c in 0..ch {
                let i = (start as usize + f) * ch + c;
                let v = if i < samples.len() { samples[i] * volume } else { 0.0 };
                let s = quantize(v, bits as u32);
                if bits == 16 && cbytes == 2 {
                    out.extend_from_slice(&(s as i16).to_le_bytes());
                } else if bits == 16 {
                    write_sample_24(out, s << 8, cbytes); // 16bitを24bitコンテナの上位へ
                } else {
                    write_sample_24(out, s, cbytes);
                }
            }
        }
    })
}

/// DoPフレーム(チャンネルごとの`[マーカー, 上位, 下位]`列)を排他モードで送る。PCMレートはDSDレート/16。
pub fn play_dop(frames: &[Vec<[u8; 3]>], pcm_rate_hz: u32) -> Result<ExclusiveReport, OutputError> {
    let ch = frames.len();
    let total = frames.iter().map(|c| c.len()).min().unwrap_or(0) as u64;
    run_exclusive(pcm_rate_hz, ch as u16, 24, total, |start, n, cbytes, chn, out| {
        for f in 0..n {
            for c in 0..chn {
                let idx = start as usize + f;
                // 末尾の穴埋めはDSDの無音パターン(0x69)+交互のマーカー
                let fr = frames[c].get(idx).copied().unwrap_or([if idx % 2 == 0 { 0x05 } else { 0xFA }, 0x69, 0x69]);
                let v = ((fr[0] as i32) << 16 | (fr[1] as i32) << 8 | fr[2] as i32) << 8 >> 8; // 符号拡張した24bit
                write_sample_24(out, v, cbytes);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_24_is_written_bit_exactly_in_both_container_widths() {
        let mut b = Vec::new();
        write_sample_24(&mut b, 0x05_69_69 as i32, 3);
        assert_eq!(b, vec![0x69, 0x69, 0x05], "3バイト: リトルエンディアンの24bit");
        let mut b = Vec::new();
        write_sample_24(&mut b, 0x05_69_69, 4);
        assert_eq!(b, vec![0x00, 0x69, 0x69, 0x05], "4バイト: 左詰め(<<8)、下位バイトは0");
        // 負の値(マーカー0xFAは最上位ビットが立つ=負の24bit値)も壊れない
        let v = ((0xFAi32 << 16 | 0x12 << 8 | 0x34) << 8) >> 8;
        let mut b = Vec::new();
        write_sample_24(&mut b, v, 4);
        assert_eq!(b, vec![0x00, 0x34, 0x12, 0xFA]);
        let mut b = Vec::new();
        write_sample_24(&mut b, v, 3);
        assert_eq!(b, vec![0x34, 0x12, 0xFA]);
    }

    #[test]
    fn quantize_rounds_and_clamps() {
        assert_eq!(quantize(0.5, 24), 4_194_304);
        assert_eq!(quantize(2.0, 16), 32_767);
        assert_eq!(quantize(-1.0, 16), -32_767);
    }
}
