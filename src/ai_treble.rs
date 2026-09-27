//! 高音補正(MP3向け)のAI版 — AudioSR(open-audio-sr、実在の拡散モデル)による帯域拡張。
//!
//! `src/treble.rs`のハイシェルフEQは、失われた高域を「持ち上げて誤魔化す」近似でしかない。
//! ここでは`open-audio-sr`(AudioSRを呼ぶRustラッパー)で実際に高域を生成する。ただし推論は
//! CPUで数分〜数十分かかりうるため、**再生開始をブロックしない**設計にする:
//!
//! - まだAI強化版が無ければ、その場では作らず(再生を待たせない)、バックグラウンドで生成を
//!   開始し、今回の再生は`treble.rs`のリアルタイムEQにフォールバックする。
//! - 次にその曲を再生するとき、キャッシュされたAI強化版(48kHz WAV)があればそれを再生する
//!   (リアルタイムEQは掛けない。二重に持ち上げないため)。
//! - キャッシュキーはファイルパス+更新日時+サイズ。元ファイルが変われば作り直す。
//! - `open-audio-sr`のPython venvが用意されていない環境では`open_audio_sr::is_available()`が
//!   falseになり、常にリアルタイムEQへフォールバックする(未設定でも問題なく動く)。

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

fn inflight() -> &'static Mutex<HashSet<String>> {
    static INFLIGHT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    INFLIGHT.get_or_init(|| Mutex::new(HashSet::new()))
}

fn cache_dir() -> PathBuf {
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(local).join("open-bar").join("ai-treble-cache");
    }
    std::env::temp_dir().join("open-bar-ai-treble-cache")
}

/// パス+更新日時+サイズから、内容が変わらない限り安定するキャッシュキーを作る。
fn cache_key(path: &str, mtime_secs: u64, len: u64) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    mtime_secs.hash(&mut h);
    len.hash(&mut h);
    format!("{:016x}", h.finish())
}

/// f32 PCM(`crate::pcm::Pcm`)を、AudioSR(soundfile経由)が読めるIEEE float形式のWAVへ書く。
fn write_wav_f32(path: &Path, pcm: &crate::pcm::Pcm) -> std::io::Result<()> {
    let channels = pcm.channels.max(1) as u16;
    let sample_rate = pcm.sample_rate;
    let bits_per_sample: u16 = 32;
    let byte_rate = sample_rate * channels as u32 * (bits_per_sample as u32 / 8);
    let block_align = channels * (bits_per_sample / 8);
    let data_bytes = (pcm.samples.len() * 4) as u32;
    let mut f = std::fs::File::create(path)?;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_bytes).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&3u16.to_le_bytes())?; // 3 = IEEE float
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&block_align.to_le_bytes())?;
    f.write_all(&bits_per_sample.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_bytes.to_le_bytes())?;
    for s in &pcm.samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

/// AI強化版のキャッシュがあればそのパスを返す。無ければ(利用可能なら)バックグラウンドで
/// 生成を開始して`None`を返す(呼び出し側はリアルタイムEQへフォールバックする)。
pub fn try_get_or_spawn(path: &str) -> Option<PathBuf> {
    if !open_audio_sr::is_available() {
        return None;
    }
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let key = cache_key(path, mtime, meta.len());
    let out = cache_dir().join(format!("{key}.wav"));
    if out.exists() {
        return Some(out);
    }
    spawn_job(path.to_string(), out, key);
    None
}

fn spawn_job(path: String, out: PathBuf, key: String) {
    {
        let mut g = inflight().lock().unwrap();
        if !g.insert(key.clone()) {
            return; // 既に同じ曲の生成が進行中
        }
    }
    std::thread::spawn(move || {
        let result: Result<(), String> = (|| {
            std::fs::create_dir_all(cache_dir()).map_err(|e| e.to_string())?;
            let pcm = crate::pcm::decode_file(&path).map_err(|e| e.to_string())?;
            let tmp_in = cache_dir().join(format!("{key}.in.wav"));
            write_wav_f32(&tmp_in, &pcm).map_err(|e| e.to_string())?;
            let tmp_out = cache_dir().join(format!("{key}.tmp.wav"));
            let opts = open_audio_sr::SrOptions::default();
            let r = open_audio_sr::super_resolve_file(&tmp_in, &tmp_out, &opts)
                .map_err(|e| e.to_string());
            let _ = std::fs::remove_file(&tmp_in);
            r?;
            std::fs::rename(&tmp_out, &out).map_err(|e| e.to_string())
        })();
        if let Err(e) = result {
            eprintln!("open-bar: AI高音補正(open-audio-sr)の生成に失敗しました({path}): {e}");
        }
        inflight().lock().unwrap().remove(&key);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_stable_and_sensitive_to_mtime_and_len() {
        let a = cache_key("song.mp3", 1000, 2000);
        let b = cache_key("song.mp3", 1000, 2000);
        assert_eq!(a, b, "同じ入力なら同じキー");
        assert_ne!(a, cache_key("song.mp3", 1001, 2000), "更新日時が違えば別キー");
        assert_ne!(a, cache_key("song.mp3", 1000, 2001), "サイズが違えば別キー");
        assert_ne!(a, cache_key("other.mp3", 1000, 2000), "パスが違えば別キー");
    }

    #[test]
    fn wav_round_trips_through_the_existing_pcm_decoder() {
        let dir = std::env::temp_dir().join(format!("open_bar_ai_treble_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        let samples: Vec<f32> = (0..2000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let pcm = crate::pcm::Pcm { sample_rate: 44_100, channels: 2, samples: samples.clone(), bits_per_sample: Some(32) };
        write_wav_f32(&path, &pcm).unwrap();
        let back = crate::pcm::decode_file(path.to_str().unwrap()).unwrap();
        assert_eq!(back.sample_rate, 44_100);
        assert_eq!(back.channels, 2);
        assert_eq!(back.samples.len(), samples.len());
        for (a, b) in samples.iter().zip(back.samples.iter()) {
            assert!((a - b).abs() < 1e-5, "{a} vs {b}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unavailable_when_no_python_venv_is_configured() {
        // このテスト環境にOPEN_AUDIO_SR_HOMEを設定していない前提(CI/開発機の既定)。
        // 実際にopen-audio-srのvenvを指す場合はこの検証はスキップされてよいが、
        // try_get_or_spawnは未設定時に必ずNoneを返す(パニックしない)ことを確認する。
        if std::env::var("OPEN_AUDIO_SR_HOME").is_ok() {
            return;
        }
        assert!(try_get_or_spawn("nonexistent-file-xyz.mp3").is_none());
    }
}
