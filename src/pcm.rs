//! PCM系音声のデコード。symphoniaでWAV/FLAC/MP3/AAC/ALAC/Vorbis/AIFF/MKV・MP4内の音声を、Opusは純Rustで読む。
//! サンプルはインターリーブしたf32(24bitまで厳密。32bit整数は精度が落ちる点に注意)。

use std::fs::File;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PcmError {
    #[error("ファイルを開けません: {0}")]
    Io(#[from] std::io::Error),
    #[error("デコードできません: {0}")]
    Decode(String),
}

#[derive(Debug, Clone)]
pub struct Pcm {
    pub sample_rate: u32,
    pub channels: usize,
    /// インターリーブしたサンプル(±1.0が最大振幅)。
    pub samples: Vec<f32>,
    /// 元のビット深度(判る場合のみ)。
    pub bits_per_sample: Option<u32>,
}

impl Pcm {
    pub fn duration_secs(&self) -> f64 {
        self.samples.len() as f64 / self.channels.max(1) as f64 / self.sample_rate.max(1) as f64
    }
}

fn err<E: std::fmt::Display>(e: E) -> PcmError {
    PcmError::Decode(e.to_string())
}

fn is_ogg_opus(path: &str) -> bool {
    use std::io::Read;
    let mut buf = [0u8; 512];
    let n = File::open(path).and_then(|mut f| f.read(&mut buf)).unwrap_or(0);
    buf[..n].starts_with(b"OggS") && buf[..n].windows(8).any(|w| w == b"OpusHead")
}

/// symphoniaで読めない形式(例: MKV/WebM内のOpus)を、外部のffmpegで一時WAVへ変換して読む(フォールバック)。
/// ffmpegが無ければ元のエラーを返す。
fn decode_via_ffmpeg(path: &str, original: PcmError) -> Result<Pcm, PcmError> {
    let tmp = std::env::temp_dir().join(format!("open_bar_fallback_{}_{}.wav", std::process::id(), path.len()));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i", path, "-vn", "-c:a", "pcm_f32le", tmp.to_str().unwrap_or("")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !ok {
        return Err(original);
    }
    let r = decode_symphonia(tmp.to_str().unwrap_or(""));
    let _ = std::fs::remove_file(&tmp);
    r.or(Err(original))
}

pub fn decode_file(path: &str) -> Result<Pcm, PcmError> {
    match decode_native(path) {
        Ok(p) => Ok(p),
        Err(e @ PcmError::Decode(_)) => decode_via_ffmpeg(path, e),
        Err(e) => Err(e),
    }
}

fn decode_native(path: &str) -> Result<Pcm, PcmError> {
    if is_ogg_opus(path) {
        let bytes = std::fs::read(path)?;
        let d = open_mqa::opus::decode_opus(&bytes, 48_000).map_err(err)?;
        let ch = d.samples_per_channel.len();
        let n = d.num_frames();
        let mut samples = Vec::with_capacity(n * ch);
        for i in 0..n {
            for c in &d.samples_per_channel {
                samples.push(c[i] as f32 / 32768.0);
            }
        }
        return Ok(Pcm { sample_rate: 48_000, channels: ch, samples, bits_per_sample: Some(16) });
    }
    decode_symphonia(path)
}

fn decode_symphonia(path: &str) -> Result<Pcm, PcmError> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()).map_err(err)?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some())
        .ok_or_else(|| PcmError::Decode("音声トラックが見つかりません".into()))?
        .clone();
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(err)?;
    let (mut rate, mut channels) = (track.codec_params.sample_rate.unwrap_or(0), track.codec_params.channels.map(|c| c.count()).unwrap_or(0));
    let mut samples: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(err(e)),
        };
        if packet.track_id() != track.id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let spec = *buf.spec();
                rate = spec.rate;
                channels = spec.channels.count();
                let mut sb = SampleBuffer::<f32>::new(buf.capacity() as u64, spec);
                sb.copy_interleaved_ref(buf);
                samples.extend_from_slice(sb.samples());
            }
            Err(SymError::DecodeError(_)) => continue, // 壊れたパケットは飛ばして続行
            Err(e) => return Err(err(e)),
        }
    }
    if samples.is_empty() || rate == 0 || channels == 0 {
        return Err(PcmError::Decode("デコード結果が空です".into()));
    }
    Ok(Pcm { sample_rate: rate, channels, samples, bits_per_sample: track.codec_params.bits_per_sample })
}
