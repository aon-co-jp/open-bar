//! ファイルの種類判別と情報取得(再生前の下調べ)。

use serde::Serialize;
use std::fs::File;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// DSF / DSDIFF。
    Dsd,
    /// PCM系の音声(WAV/FLAC/MP3/AAC/ALAC/Vorbis/Opus/AIFF)。
    Audio,
    /// 映像を含み得るコンテナ(MP4/MKV/WebM/MOV/AVI)。音声トラックは`pcm`で読めるが、映像の表示は別段階。
    VideoContainer,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct MediaInfo {
    pub path: String,
    pub kind: MediaKind,
    /// DSDならビットレート、PCMならサンプルレート(Hz)。
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<usize>,
    pub bits_per_sample: Option<u32>,
    pub duration_secs: Option<f64>,
    pub is_mqa: bool,
}

const VIDEO_EXTS: [&str; 7] = ["mp4", "m4v", "mkv", "webm", "mov", "avi", "ts"];
const AUDIO_EXTS: [&str; 14] = ["wav", "flac", "mp3", "aac", "m4a", "ogg", "opus", "aiff", "aif", "alac", "mka", "wv", "mp2", "caf"];

pub fn probe(path: &str) -> MediaInfo {
    let ext = std::path::Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let mut info = MediaInfo { path: path.to_string(), kind: MediaKind::Unknown, sample_rate_hz: None, channels: None, bits_per_sample: None, duration_secs: None, is_mqa: false };
    if open_mqa_dsd::is_dsd_file(path) {
        info.kind = MediaKind::Dsd;
        if let Ok(s) = open_mqa_dsd::read_dsd_file(path) {
            info.sample_rate_hz = Some(s.rate_hz);
            info.channels = Some(s.channels.len());
            info.bits_per_sample = Some(1);
            info.duration_secs = Some(s.duration_secs());
        }
        return info;
    }
    info.kind = if VIDEO_EXTS.contains(&ext.as_str()) {
        MediaKind::VideoContainer
    } else if AUDIO_EXTS.contains(&ext.as_str()) {
        MediaKind::Audio
    } else {
        MediaKind::Unknown
    };
    info.is_mqa = ext == "flac" && crate::mqa::is_mqa_file(path);
    if info.kind == MediaKind::Unknown {
        return info;
    }
    if let Ok(file) = File::open(path) {
        let mss = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        hint.with_extension(&ext);
        if let Ok(p) = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()) {
            if let Some(t) = p.format.tracks().iter().find(|t| t.codec_params.sample_rate.is_some()) {
                let cp = &t.codec_params;
                info.sample_rate_hz = cp.sample_rate;
                info.channels = cp.channels.map(|c| c.count());
                info.bits_per_sample = cp.bits_per_sample;
                if let (Some(tb), Some(n)) = (cp.time_base, cp.n_frames) {
                    let t = tb.calc_time(n);
                    info.duration_secs = Some(t.seconds as f64 + t.frac);
                }
            }
        }
    }
    info
}
