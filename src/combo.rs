//! 映像と音声の自由な組み合わせ(例: MP4動画+DSD音声)の再生セット。
//!
//! MP4はDSDを入れる規格上の場所が無く、MKVでも標準のDSDコーデックが無い。そこで映像ファイルと音声ファイルを
//! **別ファイルのまま**組み合わせ、音声側を時間の基準(マスタークロック)にして映像を同期させる。
//! セットは`<名前>.obar.json`(JSON)で保存でき、`make-disk`が「動画+DSD音声」を書き出すときにも同じ形式を使う。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Combo {
    /// 映像ファイル(mp4/mkv/webm/mov/avi)。
    pub video: String,
    /// 音声ファイル(dsf/dff/wav/flac/opus/mp3/...)。映像内蔵の音声は使わず、こちらを鳴らす。
    pub audio: String,
    /// 音声を映像より何ミリ秒遅らせるか(負なら早める)。
    #[serde(default)]
    pub audio_offset_ms: i64,
}

#[derive(Debug, PartialEq)]
pub enum ComboError {
    Missing(String),
    NotVideo(String),
    NotAudio(String),
}

impl std::fmt::Display for ComboError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ComboError::Missing(p) => write!(f, "ファイルがありません: {p}"),
            ComboError::NotVideo(p) => write!(f, "映像ファイルではありません: {p}"),
            ComboError::NotAudio(p) => write!(f, "音声ファイルではありません: {p}"),
        }
    }
}

impl Combo {
    pub fn validate(&self) -> Result<(), ComboError> {
        use crate::media::{probe, MediaKind};
        for p in [&self.video, &self.audio] {
            if !std::path::Path::new(p).exists() {
                return Err(ComboError::Missing(p.clone()));
            }
        }
        if probe(&self.video).kind != MediaKind::VideoContainer {
            return Err(ComboError::NotVideo(self.video.clone()));
        }
        // 音声側は、DSD・PCM系のどちらでもよい(映像コンテナの音声トラックも可)。
        match probe(&self.audio).kind {
            MediaKind::Dsd | MediaKind::Audio | MediaKind::VideoContainer => Ok(()),
            MediaKind::Unknown => Err(ComboError::NotAudio(self.audio.clone())),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("Combo is serializable")
    }

    pub fn from_json(s: &str) -> Result<Combo, serde_json::Error> {
        serde_json::from_str(s)
    }
}

/// `movie.dsd256.dsf`のように名前に複数の`.`があっても、最初の`.`より前を「名前」とみなす。
fn first_stem(name: &str) -> String {
    name.split('.').next().unwrap_or(name).to_string()
}

/// フォルダ内で、同じ名前(拡張子違い)の映像と音声を自動で組み合わせる(例: `movie.mp4` + `movie.dsd256.dsf`)。
/// 音声はDSD > FLAC/WAV > その他の順に優先する。
pub fn auto_pair(dir: &str) -> Vec<Combo> {
    use crate::media::{probe, MediaKind};
    let mut videos: Vec<(String, String)> = Vec::new();
    let mut audios: Vec<(String, String, u8)> = Vec::new(); // (名前, パス, 優先度)
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    for e in rd.flatten() {
        let path = e.path().to_string_lossy().to_string();
        let name = e.file_name().to_string_lossy().to_string();
        match probe(&path).kind {
            MediaKind::VideoContainer => videos.push((first_stem(&name), path)),
            MediaKind::Dsd => audios.push((first_stem(&name), path, 0)),
            MediaKind::Audio => {
                let pr = if name.ends_with(".flac") || name.ends_with(".wav") { 1 } else { 2 };
                audios.push((first_stem(&name), path, pr));
            }
            MediaKind::Unknown => {}
        }
    }
    videos.sort();
    let mut out = Vec::new();
    for (vstem, vpath) in videos {
        let mut cands: Vec<&(String, String, u8)> = audios.iter().filter(|a| a.0 == vstem).collect();
        cands.sort_by_key(|a| (a.2, a.1.clone()));
        if let Some(a) = cands.first() {
            out.push(Combo { video: vpath, audio: a.1.clone(), audio_offset_ms: 0 });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trips_with_default_offset() {
        let c = Combo::from_json(r#"{"video":"a.mp4","audio":"a.dsf"}"#).unwrap();
        assert_eq!(c, Combo { video: "a.mp4".into(), audio: "a.dsf".into(), audio_offset_ms: 0 });
        assert_eq!(Combo::from_json(&c.to_json()).unwrap(), c);
    }

    #[test]
    fn validate_reports_missing_and_wrong_kinds() {
        let c = Combo { video: "Z:/nope.mp4".into(), audio: "Z:/nope.dsf".into(), audio_offset_ms: 0 };
        assert!(matches!(c.validate(), Err(ComboError::Missing(_))));
        let dir = std::env::temp_dir().join(format!("open_bar_combo_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (v, a) = (dir.join("v.txt"), dir.join("a.wav"));
        std::fs::write(&v, "x").unwrap();
        std::fs::write(&a, "x").unwrap();
        let c = Combo { video: v.to_string_lossy().into(), audio: a.to_string_lossy().into(), audio_offset_ms: 0 };
        assert!(matches!(c.validate(), Err(ComboError::NotVideo(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn auto_pair_matches_names_and_prefers_dsd() {
        let dir = std::env::temp_dir().join(format!("open_bar_pair_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("movie.mp4"), "x").unwrap();
        std::fs::write(dir.join("movie.flac"), "x").unwrap();
        // 先頭4バイトが"DSD "ならDSD扱い(中身の検証は再生時に行う)
        std::fs::write(dir.join("movie.dsd256.dsf"), b"DSD 0000").unwrap();
        std::fs::write(dir.join("other.mp4"), "x").unwrap();
        let pairs = auto_pair(dir.to_str().unwrap());
        assert_eq!(pairs.len(), 1, "音声の無いother.mp4は組み合わせない: {pairs:?}");
        assert!(pairs[0].audio.ends_with("movie.dsd256.dsf"), "DSDを優先: {pairs:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
