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
- **2026-09-20 続き**: `output.rs`(cpal共有+rubatoリサンプル、実機で実時間消費を確認)、`exclusive.rs`(WASAPI排他、実機USB DAC「MUSE HiFi M3Ultra」で44.1kHz/24bit再生確認、DoP実装・DoP実機は未確認)、`playlist.rs`(ギャップレス+ReplayGain)。`cargo test`は単体15+実ファイル5が通過。DSD256全曲(242秒)のPCM化は34.5秒(release)。**DoPは非対応DACだと大音量ノイズのため、ユーザー環境のDAC確認後にのみ実機試験する**。次: (1) DoP実機確認、(2) Tauri UI(プレイリスト/foobar2000風)、(3) libmpvで映像+音声主導同期、(4) 逐次デコードのギャップレス(メモリ削減)、(5) ASIO DSD(ライセンス要確認)。
- **2026-09-24**: `player.rs`(対話プレーヤー、逐次リサンプル+リング8秒、シーク/一時停止/自動送り、実デバイスでテスト通過)と`app/`(Tauri GUI、実アプリで再生・経路表示を画面で確認)を追加。コマンドライン引数のファイルを起動時に追加・再生(「プログラムから開く」用)。既知の限界: DSDは全曲変換が終わるまで再生が始まらない(DSD256で約35秒)、曲間に数十msの隙間、GUIは共有モードのみ(排他/DoPはCLI)。次: DSD逐次変換、排他/DoP UI、libmpvで映像、ASIO。
- **2026-09-25**: 再生モードを4つ実装(`player.rs`): A=排他ビットパーフェクト、B=共有+高品質アップサンプル、E=排他+アップサンプル(352.8k/384k)、D=DoP。再生中に同じ位置で切り替えて聴き比べられ、GUIは現在のモードを日英の大きなバナーで表示(ユーザー依頼)。`source.rs`(PCM/DSD→PCM逐次/DoPのソース)で**DSD256の再生開始が約35秒→1.6秒**に。排他は`exclusive.rs`の`ExclusiveDevice`(実機D40 PRO=xCORE USB Audio 2.0は32bitコンテナ+有効24bitで受理)。実機テスト: A(44.1k排他)/B(384k共有)/E(352.8k排他)の切替、DSD256逐次再生が通過。**ユーザーの聴取: 共有B(384kHzアップサンプル)が排他A・DSD(C)より良かったとの印象 → 排他アップサンプル(E)を追加**。DoPは非対応DACで危険なため、機器確認後にユーザー操作でのみ。既定音量を100%(排他がビットパーフェクトになる)に変更、100%未満ではデジタル音量(非ビットパーフェクト)と明示。次: 曲間ギャップレス、DoP実機、libmpv映像、ASIO。
- **2026-09-25 続き2**: 聴取結果(ユーザー): 音質は B(共有+384k)が最良、次いで D/E/A。D(DoP)はD40 PROでノイズ(非対応DACの可能性、または音切れでDoPが外れた可能性)。対策: 先読み(0.75秒)を溜めてから再生、リング枯渇の計測(`Status.underrun_frames`、B/E/Aとも10秒で0を実測)、DSDを最終レートへ直接変換(E)、アップサンプルフィルター3種(標準/シャープ/ソフト。実測: 鏡像抑圧 -142/-150/-142 dB、20kHzゲイン 0/0/-12.2 dB)、GUIのモードボタンと大パネルの不一致を修正(クリックで即座に選択モードを表示、実際と違えば理由を表示)。テスト: 単体21+実ファイル9が通過。
- **2026-09-25 続き3(リリース v0.1.0)**: ユーザーの聴取が判明: 今までのレポートはプレイリスト1(PCM)だけ。2(DSD256)ではノイズ無しで **D>E>B>A**。シャープ=音切れ・スローテンポ・ノイズ(バグ)→原因は重い補間の実時間割れ。対策: `polyphase.rs`(整数倍ポリフェーズFIR、標準10倍/シャープ4.5倍/ソフト15倍 実時間)、自動フォールバック(実時間3倍未満なら標準へ)、DoPのマーカーを出力側で連続付与(pause/seek/underrunはDSD無音0x69で埋める)、フェードイン/アウト、MMCSS+長め周期、同じ曲の別形式(PCM/DSD256/DSD64)をモードで自動選択、カットオフ可変。リリースワークフロー追加(`v*`タグ→Windows/macOS/Linux)。テスト: 単体28+実ファイル10が通過。既知の限界: 曲間の隙間、libmpv映像未実装、ASIO DSD未実装、macOS/Linuxは未検証(CI次第)。
- **2026-09-25 続き4(easy-web.tokyo/open-bar)**: Web版(`webpage/`: ランディング+ブラウザ内プレーヤー。PCMはWebAudio、DSFはWebWorkerでDSD→PCM(実DSD64を350msで再生開始・シーク確認))と、Rust製Webサーバー(`server/`、静的配信+プレゼンスAPI、単体6件)。インストール版の自動起動は`openbar://launch?token=…`(ページがトークンを付けて起動→アプリが同じトークンで約10秒ごとに心拍→ページが「起動中」表示)。表示はアクセス元の端末(トークン=ブラウザ)のアプリだけ(他端末・再生内容は見えない、曲名は送らない)。アプリ側: deep-link+single-instance+`presence.rs`(単体3件)。スマホ/タブレット版アプリは未提供(ページは「準備中」と表示)。VPSは`/root/repository/open-bar`+`open-bar-web.service`+open-web-serverの`domains.toml`に`/open-bar`を追加、easy-web.tokyoの紹介カード追加(open-easy-web-appのshell.rs)。
- **2026-09-27(高音補正・open-mqa-dsdコンテナ再生・バグ修正)**: MP3向け高音補正を2段構えで実装。
  (1) 既定: `src/treble.rs`(RBJハイシェルフEQ、13kHz以上を+6dB)をリアルタイムに掛ける近似。
  (2) AI版: `src/ai_treble.rs`が[`open-audio-sr`](https://github.com/aon-co-jp/open-audio-sr)
  (実在の拡散モデルAudioSR)を呼び、実際に高域を生成する。推論はCPUで数分〜数十分かかるため
  **再生開始はブロックしない**設計: まだAI強化版のキャッシュが無ければその場では作らず
  バックグラウンドで生成を開始し、今回の再生は(1)のリアルタイムEQにフォールバックする。次回
  同じ曲を再生するときにキャッシュ(`%LOCALAPPDATA%\open-bar\ai-treble-cache`、パス+更新日時+
  サイズがキー)があればそちらを再生し、このときはリアルタイムEQを重ねて掛けない。
  `open-audio-sr`のPython venvが無い/未設定の環境では`is_available()`がfalseになり、常に(1)へ
  フォールバックする(未設定でも問題なく動く)。あわせてopen-mqa-dsdコンテナ(.mka/.mkv、旧称
  「open-audio」)の再生・高音補正・DSD抽出の実装も反映済み(既存)。
  **副産物のバグ修正**: この機能追加のため`open-audio-sr`をgit依存として初めて他プロジェクトから
  使おうとしたところ、`main.rs`(HTTPサーバー)専用のRPoem/open-cpu依存(`.deps/`へのpath依存)が
  常時必須になっておりビルドできない不具合を発見、`open-audio-sr`側Cargo.tomlを修正して`server`
  featureへ隔離した(`open-audio-sr`のCLAUDE.mdにも記録)。`cargo test -p open-bar --lib`は
  33件成功(ai_treble含む)。
  **未実施**: 実機での音質検証(AI強化版の生成完了を実際に待って聴き比べる、開発機のPython import
  遅延のため未実施)。GUI(Tauri)側にAI強化版生成中/完了の状態表示は無い(既定のリアルタイムEQで
  常に鳴るため機能上は問題ないが、ユーザーへの見える化は次回課題)。GPU(open-cuda/open-directx)
  推論は`open-audio-sr`側が未実装のため、ここでも当然未対応。

- **2026-11-04(曲間ギャップレス)**: `player.rs`。今の曲の生産が終わったら(最後の数秒がリングに入った時点)、次の曲を先に読み込んで(`prefetch_next`)今のトラックの`next`に登録し、出力側(共有=`fill_shared`、排他/DoP=`exclusive_thread`)が**曲の終わりと同じバッファの中で**次のリングの先頭へ切り替える(`take_across`)。出力形式(排他の有無・レート・ビット・チャンネル・DoP・機器)が同じ時だけつなぐ(違えば従来どおり曲が終わってから開く)。切り替え後に制御側が`promote`で状態表示を付け替え、古い生産スレッドは`retired`で終了。従来の隙間の原因は、100ms周期の検知+ファイル読み込み+0.75秒の先読み待ちだった。テスト: `take_across`と、2トラックを共有コールバックでつないで境目に無音が入らないことを確認する単体テスト(`shared_callback_switches...`)が通過。**実機では未確認**(本機の共有デバイスは更新前から開けず、排他・DoPの切り替えも実機で未試験)。DoPで次の曲もDoPのときのマーカー位相は連続する設計だが未実機。次: 実機で曲間を聴いて確認、先読み開始をもっと早める(今は最後の約8秒)、ReplayGainの曲間の音量差。
- **2026-11-05(映像: mpv + 音声主導の同期)**: `src/video.rs`。mpvを`--no-audio --pause`で別プロセス起動し、JSON IPC(Windowsは名前付きパイプ、他はUnixソケット)で操作する(libmpvを直接リンクしない方針どおり。Tauriへの埋め込みは未着手)。音声(`player`)が時間の基準で、`VideoSync::tick`(100ms周期)が`time-pos`を読み、ずれ(映像−音声+音声オフセット)に応じて、40ms以内=何もしない、40〜250ms=再生速度を最大±5%で調整、250ms以上=シーク。再生開始時は音声の位置へシークしてから一時停止を解除する。CLI: `open-bar video <映像> <音声> [--mpv PATH] [--seconds N]`(`<combo.obar.json>`も可、mpvは`--mpv`か環境変数`OPEN_BAR_MPV`)。テスト: 単体5件(補正の計算・IPCのイベント読み飛ばし・エラー・コマンド列)と、**実際のmpv(zhongfly/mpv-winbuild 2026-11-04)での統合テスト**`tests/video_mpv.rs`(`OPEN_BAR_MPV`があるときだけ動く、`--vo=null`): 擬似の音声時計に8秒追従し、途中で音声を5秒飛ばしてもシークで追従、落ち着いた後のずれは±40ms以内を実測。実mpvで見つけたバグ: mpvは`--pause`起動なのに制御側が再生中と思い込んでいて映像が止まったまま(ずれが時間と共に増えた)→初期状態を一時停止中に修正。**未確認**: `open-bar video`のCLI全体(音声デバイスが本機で開けないため。音声はPlayerの既存経路)、実際の映像表示を目で見ること(テストは`--vo=null`)、Tauri GUIへの埋め込み、macOS/Linuxの実mpv。次: GUIへの映像ウィンドウ、`Combo`のオフセット調整UI。
  **2026-11-05 追記(未確認の確認)**: 実機(Windows)で `open-bar video <mp4> <flac> --mpv …` を最後まで実行し、音声が鳴り終わるまで走って正常終了(exit 0)、実mpvのウィンドウにテスト映像が表示されるのを画面キャプチャで確認。音声が始まらないときは無言で終わらず、理由を表示して失敗終了するよう修正。**なお未確認**: 目と耳での同期の体感、Tauri GUIへの埋め込み、macOS/Linuxの実mpv。
  **2026-11-05 続き(GUI組み込み・Linux実mpv)**: `video::VideoFollower`(再生中の曲が映像コンテナならmpvの窓を開き、音声に追従。曲が変わる・止まる・映像オフで閉じる。ユーザーが窓を閉じたらその曲の間は開き直さない)を追加し、Tauri GUIに組み込み(`video_set_enabled`/`video_status`、「映像を表示」チェック、mpvは環境変数`OPEN_BAR_MPV`か`PATH`)。実アプリでMP4を再生してmpvの窓が開き映像が映ることを画面で確認。**Linux実mpv(WSL Ubuntu、mpv 0.41.0)で統合テストを実行して見つけたバグ**: mpvはシーク直後に映像が0.2秒ほど止まるため、同じ補正が毎周期シークを連打して収束しなかった → シーク後0.6秒の猶予(その間は速度補正のみ)と、猶予明けのずれから止まる秒数を学習して次のシークで先へ飛ぶ補正を追加。開始時、mpvが読み込み前でシークが失敗しても一時停止のまま再試行するよう修正。修正後、Linux実mpvで3回連続成功(落ち着いた後のずれは0.25秒未満)、Windows実mpvでも成功。**2026-11-05 続き2(Windowsで窓の中へ埋め込み)**: Tauriの窓を透明(`transparent: true`)にし、mpvを`--wid=<窓のHWND>`で窓へ描画させる(裏に映像、前面のWebViewは`#video-area`だけ透明でほかは不透明にしてあるCSS)。映像の枠の位置・大きさはページが測って`video_set_rect`で送り、`VideoFollower`が`video-margin-ratio-*`としてmpvへ反映(窓のリサイズにも追従)。Windowsは既定で埋め込み、`OPEN_BAR_EMBED=0`で別窓、macOS/Linuxは別窓のまま。**Windows実機で確認**: 実アプリでMP4(映像のフレーム番号入りのテスト映像)を再生し、映像がツールバーの下の枠に収まること、フレーム番号が再生位置と一致すること(0:09→9、0:19→19、0:29→29)、窓のサイズを980×700/820×640/700×760に変えても追従することを画面で確認。1回だけ、サイズ変更の直後の撮影で窓が映らなかったことがあった(再現せず原因は未特定)。**未確認**: 目と耳での同期の体感、macOSの実mpv、Linux/macOSでの埋め込み(Wayland非対応。X11のみ見込み)。

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

## 依存の更新(2026-11-03)
`Cargo.lock`更新(rustls 0.23.46・rustls-webpki 0.103.16・webpki-roots 1.0.10 ほか、RUSTSEC-2026-0346〜0349・0381・0382解消)。thiserror 2、ureq 3(`tls`→`rustls`機能、`timeout`→`config().timeout_global()`)、wasapi 0.26。
**2026-11-04 続き**: 見送っていた3つも更新(symphonia 0.6.1・rubato 5.0・cpal 0.18)。`output.rs`に`new_sinc`/`sinc_process`(`SincFixedIn`相当のラッパー)を置き、`player.rs`・`loopback.rs`もそれを使う。symphonia 0.6はprobe/`AudioDecoderOptions`/`copy_to_slice_interleaved`へ、cpalは`description().name()`・`build_output_stream`の値渡し・`SampleRate`=u32へ書き換え。
**音質を変えていない根拠**: sinc長・窓・補間・f_cutoff(`Some(..)`)・ビット深度・排他モードは従来のまま。デコードは`gapless=false`でサンプル数を0.5と同じにした(0.6の既定はtrue)。同じ合成音で変更前後のリサンプル出力を比較: 整数倍(44.1→88.2/176.4/352.8k、全フィルター)は最大差 3.9e-7(浮動小数点の誤差)。**非整数倍(44.1→48/96/32k)は最大差0.02〜0.10**: rubato 5は出力先頭にフィルター遅延ぶんの無音が付くため、先頭の`(sinc_len/2-1)*ratio`フレームを丸めて捨てて0.15の位置に揃えており、端数(最大0.5出力フレーム)の位相が違う(ノイズ入りの合成音での差。聴感は未確認)。
**デコードの前後比較(実施済み)**: ffmpegで作った5秒のステレオ(FLAC/MP3/WAV24bit/AAC(m4a)/Vorbis)を、更新前(ce5bf22)と更新後の`open-bar decode`で24bit WAVにして比較。FLAC・MP3・WAVは**バイト単位で完全一致**、AACは最大差 1.2e-7(丸め)。Vorbisは更新後に先頭へ128フレーム(約2.9ms)余分に出ていたが、**`pcm.rs`でVorbisだけ`Track.delay`ぶん先頭を捨てて修正済み**(フレーム数・サンプルとも更新前と一致、最大差1.2e-7。回帰テスト`vorbis_decode_has_the_same_frame_count_and_start_as_before`)。
**未確認**: 聴感(実機で聴き比べていない)、macOS。
テスト: Windows/Linux(WSL)とも単体・E2Eの結果は更新前と同一(`tests/real_files.rs`の実機デバイス系はWindowsで2件、WSLで3件、`presence`の実サーバーテスト(WSL)が環境依存で失敗するのも更新前から同じ)。

## API 方針: REST 全廃・gRPC / Connect へ (2026-11-04)

### 日本語
- **REST は全廃する。新しい API は REST で作らない。** ブラウザや `curl` から呼ぶ API は **Connect の単純呼び出し**(`POST /<パッケージ>.<Service>/<Method>`、`content-type: application/json`、本文は JSON)、サービス間は **gRPC / gRPC-web**(RPoem の管理API `open_runo.admin.v1.Admin` と同じ流儀)。ストリーミングは、必要になってから。
- **既存の REST は、触るときに順に移す。** 移したら旧 REST の経路は消す。移行が終わるまで残るが、**新規には追加しない**。
- **JSON は RS-JSON に統一**する(読み込みは `from_str_secure` / `parse_secure` でサイズ・深さ・重複キーの上限つき、書き出しは `rust_json::fast`)。ブラウザの WASM に `prost` は入れない(サイズが増えるため)。
- **例外(自分たちの API ではなく、相手の規格が決めているもの)**: ACME、OAuth/OIDC のコールバック、第三者(GitHub など)からの Webhook、ヘルスチェック、静的ファイル配信、OTLP など。これらは規格どおりの HTTP を受ける。
- 参考実装: `RPoem`(`open_runo.admin.v1.Admin`)、`open-easy-web`(`server/src/connect.rs`、`proto/open_easy_web/admin/v1/admin.proto`)。
- ※ このリポジトリのコードが既に方針どおりかどうかは、各リポジトリの現状による。この節は方針の記録であり、コードの変更を意味しない。

### English
- **REST is being abolished. New APIs are not built as REST.** APIs called from browsers or `curl` use **Connect unary calls** (`POST /<package>.<Service>/<Method>`, `content-type: application/json`, JSON body); service-to-service calls use **gRPC / gRPC-web** (the same style as RPoem's admin API `open_runo.admin.v1.Admin`). Streaming only when it is actually needed.
- **Existing REST endpoints are migrated one by one, when they are touched.** The old REST route is removed once migrated. Until migration is finished they remain, but **no new ones are added**.
- **JSON is unified on RS-JSON**: reading with `from_str_secure` / `parse_secure` (size, depth and duplicate-key limits), writing with `rust_json::fast`. `prost` is not put into browser WASM (it would grow the binary).
- **Exceptions (shapes dictated by someone else's standard, not our own API)**: ACME, OAuth/OIDC callbacks, webhooks from third parties (e.g. GitHub), health checks, static file serving, OTLP, and so on. These accept plain HTTP as the standard requires.
- Reference implementations: `RPoem` (`open_runo.admin.v1.Admin`) and `open-easy-web` (`server/src/connect.rs`, `proto/open_easy_web/admin/v1/admin.proto`).
- Note: whether the code in a given repository already follows this policy depends on that repository. This section records the policy; it does not mean the code has been changed.

### Deutsch (KI-Übersetzung, redaktionell überarbeitet; noch nicht von Muttersprachlern geprüft)
- **REST wird vollständig abgeschafft; neue APIs entstehen nicht mehr als REST.** APIs, die aus dem Browser oder per `curl` aufgerufen werden, verwenden **unäre Connect-Aufrufe** (`POST /<Paket>.<Service>/<Methode>`, `content-type: application/json`, JSON-Body). Zwischen Diensten kommen **gRPC bzw. gRPC-web** zum Einsatz, im selben Stil wie die Admin-API von RPoem (`open_runo.admin.v1.Admin`). Streaming wird erst ergänzt, wenn es tatsächlich gebraucht wird.
- **Bestehende REST-Endpunkte werden schrittweise umgestellt, und zwar immer dann, wenn man ohnehin an ihnen arbeitet.** Sobald ein Endpunkt umgestellt ist, wird die alte REST-Route entfernt. Bis zum Abschluss der Migration bleiben die übrigen bestehen, es werden aber **keine neuen mehr hinzugefügt**.
- **JSON wird einheitlich über RS-JSON verarbeitet**: Gelesen wird mit `from_str_secure` bzw. `parse_secure` (mit Obergrenzen für Größe, Verschachtelungstiefe und doppelte Schlüssel), geschrieben mit `rust_json::fast`. `prost` wird nicht in das Browser-WASM aufgenommen, da es die Datei vergrößern würde.
- **Ausnahmen (Schnittstellen, deren Form ein fremder Standard vorgibt und die nicht zu unserer eigenen API gehören)**: ACME, OAuth/OIDC-Callbacks, Webhooks von Drittanbietern (z. B. GitHub), Health-Checks, die Auslieferung statischer Dateien, OTLP und Ähnliches. Diese nehmen, wie vom jeweiligen Standard verlangt, gewöhnliches HTTP entgegen.
- Referenzimplementierungen: `RPoem` (`open_runo.admin.v1.Admin`) und `open-easy-web` (`server/src/connect.rs`, `proto/open_easy_web/admin/v1/admin.proto`).
- Hinweis: Ob der Code eines einzelnen Repositorys diese Richtlinie bereits erfüllt, hängt vom jeweiligen Repository ab. Dieser Abschnitt dokumentiert lediglich die Richtlinie; er besagt nicht, dass der Code schon angepasst wurde.

### Русский (перевод выполнен с помощью ИИ и отредактирован; носителями языка пока не проверен)
- **От REST мы полностью отказываемся; новые API больше не создаются в стиле REST.** API, к которым обращаются из браузера или через `curl`, используют **унарные вызовы Connect** (`POST /<пакет>.<Service>/<Method>`, `content-type: application/json`, тело запроса — JSON). Для взаимодействия между сервисами применяются **gRPC и gRPC-web** — в том же стиле, что и административный API RPoem (`open_runo.admin.v1.Admin`). Потоковая передача данных добавляется лишь тогда, когда в ней действительно возникает необходимость.
- **Существующие REST-эндпоинты переводятся поэтапно — по мере того, как с ними приходится работать.** После перевода старый REST-маршрут удаляется. Пока миграция не завершена, оставшиеся эндпоинты сохраняются, но **новые не добавляются**.
- **Работа с JSON унифицирована на основе RS-JSON**: чтение — через `from_str_secure` или `parse_secure` (с ограничениями на размер, глубину вложенности и повторяющиеся ключи), запись — через `rust_json::fast`. `prost` в браузерный WASM не включается, поскольку это увеличило бы размер файла.
- **Исключения (интерфейсы, форму которых определяет чужой стандарт, а не наш собственный API)**: ACME, обратные вызовы OAuth/OIDC, вебхуки сторонних сервисов (например, GitHub), проверки работоспособности, раздача статических файлов, OTLP и тому подобное. Такие интерфейсы принимают обычный HTTP, как того требует соответствующий стандарт.
- Эталонные реализации: `RPoem` (`open_runo.admin.v1.Admin`) и `open-easy-web` (`server/src/connect.rs`, `proto/open_easy_web/admin/v1/admin.proto`).
- Примечание: соответствует ли код конкретного репозитория этой политике, зависит от самого репозитория. Данный раздел лишь фиксирует политику и не означает, что код уже изменён.

## API 方針の補足: 用語の整理と使い分け (2026-11-04)

### 日本語
**1つに統一するものではなく、別の層の話です。** 「REST の代わり」になるのは Connect・gRPC・GraphQL だけで、WASM は画面を作る技術です。

| 用語 | 何か | REST の代わりか |
|---|---|---|
| REST | URL と HTTP メソッドで操作を表す API の作り方 | 全廃する側 |
| **Connect** | `POST /サービス名/メソッド名` に JSON を送る形式。`curl` でもブラウザの `fetch` でも呼べる | はい(**採用**) |
| **gRPC** | 同じ「サービス/メソッド」の考え方を、バイナリ(protobuf)と HTTP/2 で行う形式。速く、ストリーミングも得意 | はい(**サービス同士に採用**) |
| gRPC-web | gRPC のブラウザ向けの変形版 | はい(必要なら) |
| GraphQL | 欲しい項目を問い合わせる形式 | 一部だけ(**問い合わせ(読み取り)専用**) |
| WASM | Rust をブラウザの中で動かす仕組み | いいえ(画面を作る技術) |

- **ブラウザの画面や `curl` から呼ぶ API は Connect**(POST、JSON を RS-JSON で読み書き)。ブラウザの WASM は素の gRPC を話せず、Connect なら `fetch` のままで、`prost` を足さずに済む(画面のサイズも増えない)。
- **サービス同士の通信(サーバー間、重い処理、ストリーミング)は gRPC。** 多くは Connect で足りるので、ストリーミングが必要になった所だけ gRPC に上げる。
- **GraphQL は、まとめて問い合わせたいときの読み取り専用。** 書き込み(操作)には使わない。
- **WASM は画面を作る技術として、そのまま使う。** 画面から呼ぶ API が Connect になっただけ。
- 外部の規格が形を決めるもの(ACME、OAuth のコールバック、第三者の Webhook、ヘルスチェック、静的ファイル、OTLP)は、規格どおりの HTTP のまま。
- **迷ったら Connect。**

### English
**These are not alternatives to unify; they sit at different layers.** Only Connect, gRPC and GraphQL can replace REST; WASM is a way to build the UI.

| Term | What it is | Replaces REST? |
|---|---|---|
| REST | API style that expresses operations through URLs and HTTP methods | The thing being abolished |
| **Connect** | `POST /Service/Method` with a JSON body; callable from `curl` and from a browser `fetch` | Yes (**adopted**) |
| **gRPC** | The same service/method idea over binary (protobuf) and HTTP/2; fast, good at streaming | Yes (**for service-to-service**) |
| gRPC-web | A browser-friendly variant of gRPC | Yes (if needed) |
| GraphQL | A query language: ask for exactly the fields you want | Partly (**read-only queries**) |
| WASM | Runs Rust inside the browser | No (a UI technology) |

- **APIs called from browser screens or `curl` use Connect** (POST, JSON read and written with RS-JSON). Browser WASM cannot speak plain gRPC; with Connect it stays a `fetch`, and `prost` is not needed (the binary does not grow).
- **Service-to-service calls (server to server, heavy work, streaming) use gRPC.** Connect is enough in most places; move a call to gRPC only where streaming is actually needed.
- **GraphQL is read-only**, for queries that gather several pieces of data. It is not used for writes (operations).
- **WASM stays as the way to build the UI.** The API it calls simply became Connect.
- Interfaces whose shape an outside standard dictates (ACME, OAuth callbacks, third-party webhooks, health checks, static files, OTLP) stay plain HTTP.
- **When in doubt, use Connect.**

### Deutsch (KI-Übersetzung, sprachlich überarbeitet; noch nicht von Muttersprachlern geprüft)
**Es geht nicht darum, eine einzige Technik auszuwählen – die Begriffe liegen auf unterschiedlichen Ebenen.** Als Ersatz für REST kommen nur Connect, gRPC und GraphQL in Frage; WASM dagegen ist eine Technik, mit der die Benutzeroberfläche gebaut wird.

| Begriff | Was ist das? | Ersatz für REST? |
|---|---|---|
| REST | Ein API-Stil, bei dem Operationen über URLs und HTTP-Methoden ausgedrückt werden | wird abgeschafft |
| **Connect** | `POST /Dienst/Methode` mit JSON im Body; aufrufbar mit `curl` ebenso wie mit `fetch` im Browser | ja (**gewählt**) |
| **gRPC** | Dasselbe Prinzip aus Dienst und Methode, aber über binäres protobuf und HTTP/2; schnell und gut für Streaming geeignet | ja (**zwischen Diensten**) |
| gRPC-web | Eine für den Browser angepasste Variante von gRPC | ja (bei Bedarf) |
| GraphQL | Abfragesprache, mit der man genau die benötigten Felder anfordert | teilweise (**nur für lesende Abfragen**) |
| WASM | Führt Rust-Code direkt im Browser aus | nein (Technik für die Oberfläche) |

- **APIs, die von Browseroberflächen oder per `curl` aufgerufen werden, verwenden Connect** (POST; JSON wird mit RS-JSON gelesen und geschrieben). WASM im Browser kann kein reines gRPC sprechen. Mit Connect bleibt es bei einem gewöhnlichen `fetch`, und `prost` ist nicht nötig – die WASM-Datei wird dadurch nicht größer.
- **Für die Kommunikation zwischen Diensten (Server zu Server, rechenintensive Aufgaben, Streaming) wird gRPC eingesetzt.** In den meisten Fällen reicht Connect aus; auf gRPC wird nur dort umgestellt, wo Streaming tatsächlich gebraucht wird.
- **GraphQL dient ausschließlich lesenden Abfragen**, wenn mehrere Informationen gebündelt abgefragt werden sollen. Für Schreibzugriffe (Operationen) wird es nicht verwendet.
- **WASM bleibt die Technik für die Oberfläche.** Geändert hat sich lediglich, dass die aufgerufene API nun auf Connect beruht.
- Schnittstellen, deren Form durch einen externen Standard festgelegt ist (ACME, OAuth-Callbacks, Webhooks von Drittanbietern, Health-Checks, statische Dateien, OTLP), bleiben gewöhnliches HTTP.
- **Im Zweifel: Connect.**

### Русский (перевод выполнен с помощью ИИ и литературно отредактирован; носителями языка пока не проверен)
**Это не варианты, из которых нужно выбрать один, а технологии разных уровней.** Заменить REST могут только Connect, gRPC и GraphQL; WASM же — это способ создания пользовательского интерфейса.

| Термин | Что это | Заменяет REST? |
|---|---|---|
| REST | Стиль построения API, при котором операции выражаются через URL и HTTP-методы | отменяется |
| **Connect** | `POST /Сервис/Метод` с телом в формате JSON; вызывается как из `curl`, так и из браузерного `fetch` | да (**выбран**) |
| **gRPC** | Тот же принцип «сервис — метод», но поверх двоичного формата protobuf и HTTP/2; работает быстро и хорошо подходит для потоковой передачи | да (**для связи между сервисами**) |
| gRPC-web | Адаптированный для браузера вариант gRPC | да (при необходимости) |
| GraphQL | Язык запросов: клиент запрашивает ровно те поля, которые ему нужны | частично (**только запросы на чтение**) |
| WASM | Позволяет выполнять код на Rust прямо в браузере | нет (технология для интерфейса) |

- **API, которые вызываются из браузерных интерфейсов или через `curl`, строятся на Connect** (POST; JSON читается и записывается через RS-JSON). Браузерный WASM не умеет работать с «чистым» gRPC. С Connect достаточно обычного `fetch`, а `prost` не требуется — размер WASM-файла не увеличивается.
- **Для обмена между сервисами (сервер — сервер, ресурсоёмкие задачи, потоковая передача) применяется gRPC.** В большинстве случаев достаточно Connect; на gRPC переходят только там, где потоковая передача действительно необходима.
- **GraphQL используется только для чтения**, когда нужно получить сразу несколько связанных данных. Для записи (операций) он не применяется.
- **WASM по-прежнему остаётся технологией для интерфейса.** Изменилось лишь то, что вызываемый им API теперь построен на Connect.
- Интерфейсы, форма которых задана внешним стандартом (ACME, обратные вызовы OAuth, вебхуки сторонних сервисов, проверки работоспособности, статические файлы, OTLP), остаются обычным HTTP.
- **Если есть сомнения — выбирайте Connect.**
