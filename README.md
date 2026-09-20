# open-bar

[English](#english) / 日本語

[foobar2000](https://www.foobar2000.org/)(カスタマイズ性と高音質再生で知られる無料プレーヤー)をリスペクトした、**高音質・高画質優先のプレーヤー**をRustで作るプロジェクト。PCM・DSD・Opus・WAV・MP3などの音声と、MP4・MKVなどの動画を、**自由な組み合わせ**(例: MP4動画+DSD音声)で扱い、ハードウェアが対応していればDSD/MQAをそのまま送り、対応していなければ自動でPCMに変換して再生する。

## 現状(2026-09-20、初版 = 再生エンジンの中核)

**音声の再生は動きます(CLI)。** 共有モードとWASAPI排他モードのPCM再生は、実機のUSB DAC(MUSE HiFi M3Ultra)で確認しました。DoPは実装済みですが、**DoP対応DACでの実機確認はまだ**です(非対応DACだと大音量のノイズになるため、こちらでは試していません)。ASIOネイティブDSD(Steinbergライセンスが必要なSDK)、画面(UI)、動画表示は次の段階です。

| 機能 | 状態 |
|---|---|
| 音声デコード: WAV/FLAC/MP3/AAC(M4A)/ALAC/Vorbis/AIFF/Opus、MKV・MP4・WebM内の音声 | ✅ 実ファイルで確認(1kHz正弦波の振幅・長さを全形式で検証) |
| DSD: DSF・DSDIFF(非圧縮)の読み込み、DSD→PCM変換、DoP化 | ✅ (`open-mqa-dsd`) 実DSD256でprobe/plan確認 |
| 再生計画: DSDネイティブ/DoP/PCM自動変換、PCMの上限内での縮小 | ✅ 単体テスト |
| 音声出力(共有モード): cpal+高品質sincリサンプル | ✅ 実機(デバイスが実時間で消費することを確認: `open-bar play`) |
| 音声出力(WASAPI排他、ビットパーフェクト) | ✅ 実機のUSB DACで44.1kHz/24bitを確認(`--exclusive`) |
| DoP出力(排他モード) | ⚠ 実装済み・バイト配置は単体テスト。**DoP対応DACでの実機確認は未実施**(`--dop`) |
| ギャップレス連結・ReplayGain(`queue --rg`) | ✅ 実ファイルで隙間なし・-6dBで振幅半分を確認 |
| ASIOネイティブDSD | ❌ 未実装(ASIO SDKのライセンスが必要) |
| MQA: タグでの判別と、MQA対応DACへのビットパーフェクト素通しの判断 | ✅ 判別・計画のみ(**復号はしない**) |
| 映像+音声の自由な組み合わせ(`.obar.json`、同名ファイルの自動ペアリング) | ✅ 形式・検証・ペアリング(同期再生は次段階) |
| WebM内Opusなどsymphoniaが読めない形式 | ffmpegがあればフォールバック(実ファイルで確認) |
| DST圧縮のDSDIFF | 未対応(明示エラー) |

## MQAについて(正直な開示)

**MQAは復号せず、再実装もしません**(特許・営業秘密で保護された、ロスレスではない非公開技術)。MQA対応DACでMQAを鳴らすには、ソフトは**音量調整・EQ・リサンプル無しのビットパーフェクトのまま素通し**すればよく、展開はDAC側です。open-barは、MQA(タグで判別)をこの条件が満たせるときだけ素通しし、満たせなければ「MQAの展開は行われない通常のPCM」として鳴らし、その理由を表示します。

## 使い方(CLI)

```
open-bar probe <file>                          ファイル情報(JSON)
open-bar plan <file> --dsd none|dop|native --max-rate 192000 [--mqa-dac]
open-bar decode <file> <out.wav>               24bit WAVへ(DSDは自動PCM化)
open-bar play <file> [--volume V] [--seconds N]  既定デバイスで再生(共有モード。DSDは自動PCM化)
open-bar play <file> --exclusive [--bits 16|24]  WASAPI排他(Windows、ビットパーフェクト)
open-bar play <file.dsf> --dop                  DoP(DoP対応DACのみ!非対応だとノイズ)
open-bar queue [--rg] <file>...                 ギャップレス連続再生(--rg=ReplayGain)
open-bar pair <folder>                         同名の映像+音声を自動で組み合わせ
```

## 設計メモ(調査に基づく)

- **デコード**: [symphonia](https://docs.rs/symphonia/)(純Rust)。ただしsymphoniaにDSDデコーダは無く、MKV/WebM内のOpusも読めない → DSDは`open-mqa-dsd`、OpusはRust実装の`opus-pure`(`open-mqa`経由)、残りはffmpegフォールバック。
- **出力(次段階)**: 先行するRust製プレーヤー([Aqloss](https://github.com/themkoi/Aqloss)、[Moosik](https://github.com/HenloAmHorse/Moosik)、[soul-player](https://github.com/soulaudio/soul-player)、[dsd-rust](https://github.com/xenide/dsd-rust))を参考に、WASAPI排他(DoP)、ASIOネイティブDSD、macOSはDoPのみ。共有モードではDoP・MQAが壊れるため、排他モードを必須にする。
- **映像(次段階)**: MP4/MKVのコーデックは多様なため、[libmpv](https://lib.rs/crates/tauri-plugin-libmpv)を使う方針(Tauriへの直接埋め込みは難所のため、まずは外部プロセス+JSON IPCで同期を確立)。音声を時間の基準にして映像を追従させる。
- **エコシステム連携**: `open-cpu`(SIMD検出、DSD→PCM間引きの高速化に使用予定)、`open-cuda`/`open-directx`(FIR間引き・映像処理の並列化候補。ΔΣ変調は直列フィードバックでGPU向きでない)、`aruaru-llm`(ライブラリのタグ整理・検索など補助用途の候補)。いずれも「実測で効くところにだけ」使い、投機的な結線はしない。

## ロードマップ

1. ✅ 音声出力(共有・WASAPI排他・DoP)+ギャップレス+ReplayGain(済)。ASIO DSDは未実装
2. UI(Tauri、foobar2000風のプレイリスト/カスタマイズ)
3. 映像(libmpv)と音声主導の同期(`Combo`)
4. `make-disk`との連携(「動画+DSD音声」セットの書き出し)、ライブラリ管理

## 関連

[open-mqa](https://github.com/aon-co-jp/open-mqa) / [open-mqa-dsd](https://github.com/aon-co-jp/open-mqa-dsd) / [make-disk](https://github.com/aon-co-jp/make-disk) / [open-cpu](https://github.com/aon-co-jp/open-cpu)

<a id="english"></a>
## English

A **quality-first audio/video player** in Rust, in the spirit of [foobar2000](https://www.foobar2000.org/): PCM, DSD, Opus, WAV, MP3 and more, plus MP4/MKV video, in **free combinations** (e.g. MP4 video + DSD audio). If the hardware supports DSD/MQA the stream is sent as is; otherwise DSD is automatically converted to PCM.

**Status (2026-09-20): audio playback works from the CLI.** Shared-mode and WASAPI-exclusive PCM playback are verified on a real USB DAC (MUSE HiFi M3Ultra); gapless queues and ReplayGain work. DoP is implemented but **not yet verified on a DoP-capable DAC** (sending DoP to a non-DoP DAC produces loud noise, so it was not tried). ASIO native DSD (needs Steinberg's SDK licence), the GUI and video display are next.

Verified with real files: WAV/FLAC/MP3/AAC/Vorbis/Opus/MKA plus audio inside MP4/MKV/WebM (1 kHz tone amplitude and duration checked per format); DSF/DSDIFF reading, DSD→PCM and DoP via `open-mqa-dsd` (real DSD256 probed/planned); playback planning (native DSD / DoP / automatic PCM fallback); MQA detection by tag. **MQA is never decoded or reimplemented** (patented, proprietary, not lossless): on an MQA-capable DAC the player only needs to pass the stream bit-perfectly (no volume/EQ/resampling) and the DAC unfolds it; otherwise it plays as ordinary PCM and says why. DST-compressed DSDIFF is refused explicitly. Opus in WebM needs ffmpeg as a fallback.

Roadmap: verify DoP on hardware, UI (Tauri), video via libmpv with audio-master sync, ASIO DSD, make-disk integration.
