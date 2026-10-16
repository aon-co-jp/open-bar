# open-bar

[English](#english) / 日本語

[foobar2000](https://www.foobar2000.org/)(カスタマイズ性と高音質再生で知られる無料プレーヤー)をリスペクトした、**高音質・高画質優先のプレーヤー**をRustで作るプロジェクト。PCM・DSD・Opus・WAV・MP3などの音声と、MP4・MKVなどの動画を、**自由な組み合わせ**(例: MP4動画+DSD音声)で扱い、ハードウェアが対応していればDSD/MQAをそのまま送り、対応していなければ自動でPCMに変換して再生する。

## ダウンロード / Download

**最新インストーラー(Windows / macOS / Linux): https://github.com/aon-co-jp/open-bar/releases/latest**

## 現状(2026-09-20、初版 = 再生エンジンの中核)

**音声の再生は動きます(CLI)。** 共有モードとWASAPI排他モードのPCM再生は、実機のUSB DAC(MUSE HiFi M3Ultra)で確認しました。DoPは、**D40 PRO(xCORE USB Audio 2.0)で、DSDファイルを聴いて確認**していただきました(「DSD → PCM変換より音が良い」との感想)。非対応DACに送ると大音量のノイズになるので、対応DACでのみ使ってください。ASIOネイティブDSD(Steinbergライセンスが必要なSDK)、画面(UI)、動画表示は次の段階です。

| 機能 | 状態 |
|---|---|
| 音声デコード: WAV/FLAC/MP3/AAC(M4A)/ALAC/Vorbis/AIFF/Opus、MKV・MP4・WebM内の音声 | ✅ 実ファイルで確認(1kHz正弦波の振幅・長さを全形式で検証) |
| DSD: DSF・DSDIFF(非圧縮)の読み込み、DSD→PCM変換、DoP化 | ✅ (`open-mqa-dsd`) 実DSD256でprobe/plan確認 |
| 再生計画: DSDネイティブ/DoP/PCM自動変換、PCMの上限内での縮小 | ✅ 単体テスト |
| 音声出力(共有モード): cpal+高品質sincリサンプル | ✅ 実機(デバイスが実時間で消費することを確認: `open-bar play`) |
| 音声出力(WASAPI排他、ビットパーフェクト) | ✅ 実機のUSB DACで44.1kHz/24bitを確認(`--exclusive`) |
| DoP出力(排他モード) | ✅ 実機のUSB DAC(D40 PRO)で聴取確認。マーカー位相は出力側で連続して付与(一時停止・シーク・音切れでも崩れない、単体テスト) |
| ギャップレス連結・ReplayGain(`queue --rg`) | ✅ 実ファイルで隙間なし・-6dBで振幅半分を確認 |
| 再生モード切替(A排他ビットパーフェクト/B共有+アップサンプル/E排他+アップサンプル/D DoP)+現在のモードを日英で大きく表示 | ✅ 実機で切替・経路表示を確認(DoPの音は未確認) |
| DSDの逐次変換(全曲の変換を待たない) | ✅ DSD256で再生開始 約35秒→1.6秒 |
| 画面(GUI、Tauri): プレイリスト・再生/一時停止/停止/前後・シーク・音量・形式/経路表示(日英併記) | ✅ 実アプリで再生を確認(共有モード、逐次リサンプル)。排他/DoPはCLIのみ(UI連携は次) |
| ASIOネイティブDSD | ❌ 未実装(ASIO SDKのライセンスが必要) |
| MQA: タグでの判別と、MQA対応DACへのビットパーフェクト素通しの判断 | ✅ 判別・計画のみ(**復号はしない**) |
| 映像+音声の自由な組み合わせ(`.obar.json`、同名ファイルの自動ペアリング) | ✅ 形式・検証・ペアリング(同期再生は次段階) |
| WebM内Opusなどsymphoniaが読めない形式 | ffmpegがあればフォールバック(実ファイルで確認) |
| 高音補正(MP3向け): 既定はリアルタイムEQ、[`open-audio-sr`](https://github.com/aon-co-jp/open-audio-sr)(実在の拡散モデルAudioSR)が使えればAIで実際に高域を復元(バックグラウンド生成→次回再生からキャッシュを使用) | ✅ EQは単体テスト、AI版はopen-audio-sr側で実機検証済み・open-bar組み込みは単体テスト(`ai_treble`) |
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
2. ✅ UI(Tauri): 初版済み(`app/`、`cargo run -p open-bar-app`)。次: 排他/DoP切替、カスタマイズ、ドラッグ&ドロップ、DSDの逐次変換(現状は変換が終わるまで数十秒待つ)
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


## 聴き比べの結果(2026-09-25、ユーザーの聴取)

DSD256のファイルを聴くと、ノイズは無く、**D(DoP) > E(排他アップサンプル) > B(共有アップサンプル) > A(排他) の順に音が良い**との感想。アップサンプルは標準フィルターが最も良く、ソフト(20kHzで-12dB)は面白みに欠け、シャープは初版で音切れがあった(下記の高速化で修正)。

**性能の実測(音切れの原因と対策)**: 汎用sinc補間は標準フィルターでも実時間の約1.5倍で余裕が無く、音切れ・ノイズの原因だった。整数倍のアップサンプル(44.1k→352.8kなど)をポリフェーズFIRに置き換え、標準は実時間の約10倍、シャープは約4.5倍、ソフトは約15倍になった。実時間に間に合わないフィルターは標準へ自動で切り替える。

## JSON処理の使用方針: `parse_secure_sonic`(速い型+安全)と`sonic-rs`は使える所では全て使う(2026-10-14、ユーザー指示)
正本は[`RS-JSON`](https://github.com/aon-co-jp/RS-JSON)(`README.md`・`PORTING.md`)。

**方針**: JSONを読む所は、使える所では**全て** `rust-json`(RS-JSON)の `parse_secure_sonic`(`fast`機能、結果は`sonic_rs::Value`)を使う。
`sonic-rs` を直接使える所(型付き構造体への読み込みなど)も同様。使えない所は `parse_secure`(結果は`serde_json::Value`)を使う。
外部から来る信頼できない入力(HTTP本文・ネットワーク越しのファイル・アップロード)は、**必ずサイズ・深さ・重複キーの上限つき**で読む。

**実測(7 MB・4万件)**: `serde_json` 200 ms / `parse_secure` 217 ms / **`parse_secure_sonic` 83 ms(約2.4倍速い、防御込み)** / `sonic-rs`そのまま 33 ms(約6倍、防御なし)。
`serde_json::Value`に読む限りSIMDでもほぼ速くならない(188 ms)。**速さの源は、結果を連続した領域に詰め込む値の型**。

**使える所 / 使えない所**
- ✅ 読み取り中心で、JSONを大きく・頻繁に読む所(キャッシュJSON、設定、APIの応答、ログ、インデックス)。
- ✅ 外部からの入力の入口(速さより防御が目的。`parse_secure`でも可)。
- ⚠️ `serde_json::Value`を関数間で受け渡している既存コードは、型を`sonic_rs::Value`に直す書き換えが要る(`JsonValueTrait`/`JsonContainerTrait`のメソッド)。書き換えずに`serde_json::Value`へ読むと速くならない。
- ❌ WASM(`rust_json::light`、依存ゼロ)、PHP/JavaScript側のJSON処理(Rustの部品は使えない)。
- 注意: 依存先の`sonic-rs`は内部で`unsafe`を使う(自前コードに`unsafe`は無い)。小さなJSON(数KB)では体感差が出ない。速度目的の書き換えは、実測で効く所から。

**導入**: `Cargo.toml`に `rust-json = { path = "../RS-JSON", features = ["fast"] }`。使用例は`RS-JSON/README.md`。

**このリポジトリでの適用状況**: 未適用(方針のみ)。コードの切り替えは、JSONを大きく・頻繁に読む所から実測して順に行う。

## JSON handling policy: use `parse_secure_sonic` (fast type + secure) and `sonic-rs` everywhere they can be used (2026-10-14, user instruction)
The source of truth is [`RS-JSON`](https://github.com/aon-co-jp/RS-JSON) (`README.md`, `PORTING.md`).

**Policy**: wherever JSON is read, use `parse_secure_sonic` from `rust-json` (RS-JSON, the `fast` feature; the result is `sonic_rs::Value`) **everywhere it can be used**.
Use `sonic-rs` directly where possible too (e.g. reading into typed structs). Where it cannot be used, use `parse_secure` (result: `serde_json::Value`).
Untrusted input from outside (HTTP bodies, files over the network, uploads) **must always be read with size / depth / duplicate-key limits**.

**Measured (7 MB, 40,000 records)**: `serde_json` 200 ms / `parse_secure` 217 ms / **`parse_secure_sonic` 83 ms (about 2.4x faster, with the defences)** / raw `sonic-rs` 33 ms (about 6x, no defences).
Reading into `serde_json::Value` gives almost no speed-up even with SIMD (188 ms). **The speed comes from the value type that packs the result into contiguous memory.**

**Where it can / cannot be used**
- ✅ Read-heavy code that parses large or frequent JSON (cache JSON, config, API responses, logs, indexes).
- ✅ Entrances for outside input (the goal there is defence rather than speed; `parse_secure` is fine).
- ⚠️ Existing code that passes `serde_json::Value` between functions needs its type changed to `sonic_rs::Value` (methods from `JsonValueTrait` / `JsonContainerTrait`). Reading into `serde_json::Value` without that change is not faster.
- ❌ WASM (`rust_json::light`, zero dependencies) and the JSON handling on the PHP / JavaScript side (Rust components cannot be used there).
- Note: the `sonic-rs` dependency uses `unsafe` internally (our own code has none). For small JSON (a few KB) the difference is not noticeable; do speed-motivated rewrites where measurements show a benefit.

**Adoption**: in `Cargo.toml`, `rust-json = { path = "../RS-JSON", features = ["fast"] }`. Example in `RS-JSON/README.md`.

**Status in this repository**: Not yet applied (policy only). Code will be switched over place by place, starting where measurements show large or frequent JSON reads.

---

作者 / Author: 石 塚 正 浩（MASAHIRO ISHIZUKA） (https://github.com/aon-co-jp)
