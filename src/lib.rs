//! open-bar: foobar2000をリスペクトした高音質・高画質プレーヤーの中核(2026-09-20、初版)。
//!
//! - [`media`]: ファイルの種類判別と情報取得(PCM系・DSD・動画コンテナ)。
//! - [`pcm`]: symphonia(WAV/FLAC/MP3/AAC/ALAC/Vorbis/AIFF/MKV・MP4の音声)と純RustのOpusでのデコード。
//! - [`mqa`]: MQAファイルの判別(タグベース)。MQAは**復号せず**、MQA対応DACへビットパーフェクトで素通しする判断に使う。
//! - [`plan`]: 出力先ハードウェアの能力(DSDネイティブ/DoP/PCM上限/MQAデコーダ)から、DSDをそのまま送るか
//!   DoPにするか自動でPCMへ変換するかを決める再生計画。
//! - [`combo`]: 「MP4動画+DSD音声」のような、映像と音声を別ファイルで自由に組み合わせる再生セット。
//!
//! - [`exclusive`](Windows): WASAPI排他モードのビットパーフェクト出力(PCM・DoP)。
//! - [`player`]: 対話的なプレーヤー(再生/一時停止/停止/シーク/音量/自動送り)。UIから使う。
//! - [`output`]: 音声出力(第1段階=cpal共有モード+高品質リサンプル)。WASAPI排他/DoP/ASIOと動画表示は次の段階(README参照)。
pub mod combo;
#[cfg(windows)]
pub mod exclusive;
pub mod media;
pub mod mqa;
pub mod output;
pub mod pcm;
pub mod plan;
pub mod player;
pub mod playlist;
pub use open_mqa_dsd as dsd;
