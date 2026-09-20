# open-bar 開発メモ(CLAUDE.md)

開発方針・ルールの正本は [`open-raid-z/CLAUDE.md`](https://github.com/aon-co-jp/open-raid-z)。

## このリポジトリの役割

foobar2000をリスペクトした高音質・高画質優先のプレーヤー(Rust)。音声(PCM/DSD/Opus/WAV/MP3…)と動画(MP4/MKV…)を自由に組み合わせ、DSD/MQAはハードウェアが対応していればそのまま送り、非対応なら自動でPCMへ。Windows/macOS/Linux(将来Android)。

## 方針(ユーザー指示 2026-09-20)

- リポジトリ名は`open-bar`(GitHub: aon-co-jp/open-bar、公開)。DSD/MQA周りは`open-mqa`・`open-mqa-dsd`と連携。
- 音質・画質最優先。`open-cpu`/`open-directx`/`open-cuda`/`aruaru-llm`は、実測で効く箇所にだけ使う(投機的な結線はしない)。
- 「MP4動画+DSD音声」など自由な組み合わせに対応し、`make-disk`と一緒に開発する(`Combo`形式を共有)。
- MQAは復号・再実装しない(特許・非公開)。MQA対応DACへのビットパーフェクト素通しのみ。
- 世界中の言語で検索・GitHub調査して設計し、TEST・BUG修正を繰り返す。README/CLAUDE/PORTINGは日英併記。

## HANDOFF

- **2026-09-20 初版**: `src/{media,pcm,mqa,plan,combo}.rs`+CLI。`cargo test`は単体9件+実ファイルE2E 4件が通過(ffmpegで作った各形式・MP4/MKV/WebMの音声・実DSD256・MQAタグ)。WebM内Opusはsymphoniaが読めずffmpegフォールバックで解決。**音は出せない**(音声出力は未実装)。
- **次回の再開点**: (1) 音声出力: まずcpalで共有モードのPCM再生→WASAPI排他→DoP→ASIO DSD。(2) ギャップレス/ReplayGain。(3) Tauri UI。(4) libmpvでの映像+音声主導同期。(5) `open-mqa-dsd`へΔΣ変調器を移植し、DSD出力(PCM→DSD)も。(6) `make-disk`から`Combo`(`.obar.json`)を書き出す。
- 調査メモ: 先行Rustプレーヤーは Aqloss(WASAPI排他、DSD非対応)/Moosik(DSD一級、DoP+ASIO)/soul-player/dsd-rust(DoPのみ、macOS)。MQAのソフト展開(コア展開)を持つ再生アプリはあるが、ライセンス製品でありopen-barは扱わない。
