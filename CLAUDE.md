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
