//! open-bar: foobar2000をリスペクトした高音質・高画質プレーヤーの中核(2026-09-20、初版)。
//!
//! - [`media`]: ファイルの種類判別と情報取得(PCM系・DSD・動画コンテナ)。
//! - [`pcm`]: symphonia(WAV/FLAC/MP3/AAC/ALAC/Vorbis/AIFF/MKV・MP4の音声)と純RustのOpusでのデコード。
//! - [`mqa`]: MQAファイルの判別(タグベース)。MQAは**復号せず**、MQA対応DACへビットパーフェクトで素通しする判断に使う。
//! - [`plan`]: 出力先ハードウェアの能力(DSDネイティブ/DoP/PCM上限/MQAデコーダ)から、DSDをそのまま送るか
//!   DoPにするか自動でPCMへ変換するかを決める再生計画。
//! - [`combo`]: 「MP4動画+DSD音声」のような、映像と音声を別ファイルで自由に組み合わせる再生セット。
//!
//! 音声デバイスへの実出力(WASAPI排他/ASIO)と動画の表示は次の段階(README参照)。
pub mod combo;
pub mod media;
pub mod mqa;
pub mod pcm;
pub mod plan;
pub use open_mqa_dsd as dsd;
