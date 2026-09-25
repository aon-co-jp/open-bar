//! WASAPI排他モードの出力(Windows)。OSのミキサー・リサンプラ・音量処理を通さず、デバイスへビットパーフェクトで送る。
//! PCM(16/24bit、任意レート)とDoP(DSD over PCM、24bit)に対応する。DoP・MQA素通しにはこれが必須。
//!
//! **DoPの安全上の注意**: DoP非対応のDACへDoPを送ると、DSDのビット列がそのままPCMとして鳴り、大音量のノイズになる。
//! DoP対応DACであることを確かめてから使うこと(CLIでも明示指定のときだけ動く)。

use crate::output::OutputError;
use std::sync::atomic::{AtomicBool, Ordering};
/// 出力スレッドを「Pro Audio」(MMCSS)+最高優先度にする。Windowsが他の処理を優先して音が一瞬途切れるのを防ぐ。
mod rt {
    #[link(name = "avrt")]
    extern "system" {
        fn AvSetMmThreadCharacteristicsW(task: *const u16, index: *mut u32) -> isize;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentThread() -> isize;
        fn SetThreadPriority(h: isize, p: i32) -> i32;
    }
    pub fn boost_current_thread() {
        let name: Vec<u16> = "Pro Audio\0".encode_utf16().collect();
        let mut idx = 0u32;
        unsafe {
            AvSetMmThreadCharacteristicsW(name.as_ptr(), &mut idx);
            SetThreadPriority(GetCurrentThread(), 15); // THREAD_PRIORITY_TIME_CRITICAL
        }
    }
}

use wasapi::{initialize_mta, AudioClient, AudioRenderClient, DeviceEnumerator, Direction, Handle, SampleType, StreamMode, WaveFormat};

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

pub use crate::exclusive_bytes::write_sample_24;

/// f32(±1.0)を`bits`ビットの整数へ丸める。ソースが16/24bit整数由来(または DoPワード)なら値は厳密に整数へ戻る。
pub fn quantize(v: f32, bits: u32) -> i32 {
    let scale = (1i64 << (bits - 1)) as f32;
    let max = scale - 1.0;
    (v * scale).round().clamp(-scale, max) as i32
}

/// 開いた排他モードのデバイス(COMオブジェクトはスレッドをまたげないので、使うスレッドで`open`する)。
pub struct ExclusiveDevice {
    pub device_name: String,
    pub rate_hz: u32,
    pub channels: usize,
    pub container_bytes: usize,
    client: AudioClient,
    render: AudioRenderClient,
    event: Handle,
}

impl ExclusiveDevice {
    /// 既定の出力デバイスを排他モードで開く(まだ再生は始めない)。
    pub fn open(rate_hz: u32, channels: usize, want_bits: u16) -> Result<ExclusiveDevice, OutputError> {
        initialize_mta().ok().map_err(werr)?;
        let enumerator = DeviceEnumerator::new().map_err(werr)?;
        let device = enumerator.get_default_device(&Direction::Render).map_err(werr)?;
        let name = device.get_friendlyname().unwrap_or_else(|_| "(名前不明)".into());
        let mut client = device.get_iaudioclient().map_err(werr)?;
        // USB DACは32bitコンテナ+有効24bitだけ受けるものが多いので、候補を順に試す。
        let candidates: Vec<(usize, usize)> = if want_bits >= 24 { vec![(32, 24), (24, 24), (32, 32)] } else { vec![(16, 16), (32, 24), (24, 24), (32, 16)] };
        let mut chosen = None;
        let mut last_err = String::new();
        for (container, valid) in candidates {
            let desired = WaveFormat::new(container, valid, &SampleType::Int, rate_hz as usize, channels, None);
            match client.is_supported_exclusive_with_quirks(&desired) {
                Ok(f) => {
                    chosen = Some(f);
                    break;
                }
                Err(e) => last_err = e.to_string(),
            }
        }
        let fmt = chosen.ok_or_else(|| OutputError::Config(format!("排他モードで{rate_hz}Hz/{want_bits}bit/{channels}chを受け付けません: {last_err}")))?;
        let block = fmt.get_blockalign() as usize;
        let container_bytes = block / channels;
        let (_def, min_period) = client.get_device_period().map_err(werr)?;
        // 周期は長め(最低でも約10ms)にして、Windowsのスケジューリングの揺れで音が途切れにくくする(遅延は気にしない用途)。
        let wanted = (3 * min_period / 2).max(100_000);
        let period = client.calculate_aligned_period_near(wanted, Some(128), &fmt).map_err(werr)?;
        client
            .initialize_client(&fmt, &Direction::Render, &StreamMode::EventsExclusive { period_hns: period })
            .map_err(|e| OutputError::Stream(format!("排他モードを開始できません(他のアプリが使用中、または排他モードが禁止されています): {e}")))?;
        let event = client.set_get_eventhandle().map_err(werr)?;
        let render = client.get_audiorenderclient().map_err(werr)?;
        Ok(ExclusiveDevice { device_name: name, rate_hz, channels, container_bytes, client, render, event })
    }

    /// 再生ループ。`fill(フレーム数, コンテナ幅バイト, ch, 出力バイト列)`が`out`へそのフレーム数ぶんを追加し、
    /// まだ続くなら`true`、これで最後なら`false`を返す。`stop`が立つか、最後まで書き終えたら戻る。
    pub fn run<F>(&self, stop: &AtomicBool, mut fill: F) -> Result<(u64, f64), OutputError>
    where
        F: FnMut(usize, usize, usize, &mut Vec<u8>) -> bool,
    {
        rt::boost_current_thread();
        let block = self.container_bytes * self.channels;
        let mut written: u64 = 0;
        let mut more = true;
        let mut write = |n: usize, more: &mut bool, written: &mut u64| -> Result<(), OutputError> {
            let mut buf = Vec::with_capacity(n * block);
            *more = fill(n, self.container_bytes, self.channels, &mut buf);
            self.render.write_to_device(n, &buf, None).map_err(werr)?;
            *written += n as u64;
            Ok(())
        };
        let first = self.client.get_available_space_in_frames().map_err(werr)? as usize;
        write(first, &mut more, &mut written)?;
        let t0 = std::time::Instant::now();
        self.client.start_stream().map_err(werr)?;
        while more && !stop.load(Ordering::Relaxed) {
            if self.event.wait_for_event(1000).is_err() {
                self.client.stop_stream().ok();
                return Err(OutputError::Stream("デバイスがデータを要求しません(タイムアウト)".into()));
            }
            let n = self.client.get_available_space_in_frames().map_err(werr)? as usize;
            write(n, &mut more, &mut written)?;
        }
        if !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(200)); // 末尾のバッファが鳴り終わるのを待つ
        }
        let elapsed = t0.elapsed().as_secs_f64();
        self.client.stop_stream().map_err(werr)?;
        Ok((written, elapsed))
    }
}

/// この形式で排他モードを開けるかを確かめる(実際には再生しない)。開けたらコンテナのビット幅を返す。
pub fn probe(rate_hz: u32, channels: usize, want_bits: u16) -> Result<u16, OutputError> {
    ExclusiveDevice::open(rate_hz, channels, want_bits).map(|d| (d.container_bytes * 8) as u16)
}

fn play_fixed<F>(rate_hz: u32, channels: usize, want_bits: u16, total_frames: u64, mut sample: F) -> Result<ExclusiveReport, OutputError>
where
    F: FnMut(u64, usize, usize, usize, &mut Vec<u8>),
{
    let dev = ExclusiveDevice::open(rate_hz, channels, want_bits)?;
    let stop = AtomicBool::new(false);
    let mut pos: u64 = 0;
    let (written, elapsed) = dev.run(&stop, |n, cbytes, ch, out| {
        sample(pos, n, cbytes, ch, out);
        pos += n as u64;
        pos < total_frames
    })?;
    Ok(ExclusiveReport { device: dev.device_name.clone(), rate_hz, container_bits: (dev.container_bytes * 8) as u16, channels: channels as u16, frames_written: written.min(total_frames), elapsed_secs: elapsed })
}

/// PCM(インターリーブf32、`bits`は16/24)を排他モードで再生する。
pub fn play_exclusive_pcm(samples: &[f32], channels: usize, rate_hz: u32, bits: u16, volume: f32) -> Result<ExclusiveReport, OutputError> {
    let total = (samples.len() / channels) as u64;
    let bits = bits.clamp(16, 24);
    play_fixed(rate_hz, channels, bits, total, |start, n, cbytes, ch, out| {
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
    play_fixed(pcm_rate_hz, ch, 24, total, |start, n, cbytes, chn, out| {
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
    fn quantize_rounds_clamps_and_round_trips_integers_exactly() {
        assert_eq!(quantize(0.5, 24), 4_194_304);
        assert_eq!(quantize(2.0, 16), 32_767);
        assert_eq!(quantize(-1.0, 16), -32_768);
        // 24bit整数 → f32(/2^23) → 整数: 全範囲の代表値で厳密に元へ戻る(ビットパーフェクトの前提)
        for k in [-8_388_608i32, -8_388_607, -1, 0, 1, 0x05_69_69, 8_388_607, (0xFA_12_34i32) << 8 >> 8] {
            assert_eq!(quantize(k as f32 / 8_388_608.0, 24), k, "{k}");
        }
        for k in [-32_768i32, -1, 0, 1, 12_345, 32_767] {
            assert_eq!(quantize(k as f32 / 32_768.0, 16), k, "{k}");
        }
    }
}
