# tests/fixtures/

**このフォルダの音声ファイル(WAV/DSF/.mka)自体はgitへコミットしない。** 10曲で
CD版+DSD64版+open-audio版を合わせて約3.4GBになり、コミットするとリポジトリが
際限なく肥大化するため(`make-disk/installer/README.md`の「バイナリはGitHub
Releasesが正本」という既存方針に倣う)。コミットするのは以下の3つだけ:

- [`TRACKS.json`](TRACKS.json) — 曲目・演奏者・録音年・ジャンル・再生時間・ライセンスの一覧
- [`../../scripts/fetch-test-fixtures.sh`](../../scripts/fetch-test-fixtures.sh) — ここに書かれた
  音源をInternet Archive(archive.org)から取得し、DSD64(.dsf)とopen-audio(.mka)を
  ローカルで生成するスクリプト(要: curl・node・ffmpeg)
- [`../../scripts/pcm_to_dsd.js`](../../scripts/pcm_to_dsd.js) — WAV(44.1kHz/16bit/stereo)を
  DSD64のDSFへ変換する2次デルタシグマ変調エンコーダ(一度限りの用途で書いたものだが、
  再現性のため正式にコミットしている)

`bash scripts/fetch-test-fixtures.sh`を実行すると、このフォルダに10曲ぶんの
`<name>.wav`(CD相当、原盤そのまま)・`_dsd/<name>_dsd64.dsf`(SACD相当のDSD64)・
`_openaudio/<name>.open-audio.mka`(open-av音声専用プロファイル、WAV+DSD添付+
マニフェスト)が生成される。`real_files.rs`のE2Eテストはこれらが無ければ自動で
スキップする(既存の`F:/tmp/cd_dsd/Track04.dsf`と同じ「無ければスキップ」方針)。

## 権利について / About rights

10曲すべて **Creative Commons Public Domain Mark 1.0**
(<http://creativecommons.org/publicdomain/mark/1.0/>)。

**「著作権の許可を取得した」のではなく、そもそも独占的な著作権が存在しない状態
(パブリックドメイン)であることを、Internet Archiveの"Great 78 Project"等が
確認した上で公開しているものを利用している。** 録音年は1901〜1925年で、作曲
(原曲)・演奏・原盤(サウンドレコーディング)いずれの権利も著作権保護期間が
満了しているか、権利者不明のままパブリックドメインとして扱われている。

This is not a claim that permission was obtained from a rights holder — it is the
opposite: these recordings are in the public domain (no exclusive copyright exists),
as verified and published by the Internet Archive's "Great 78 Project" and related
collections. All ten were recorded 1901–1925; both the underlying composition and
the specific sound recording ("phonogram") rights have expired or are treated as
public domain.

## 曲目一覧 / Track list

| # | タイトル / Title | 演奏 / Performer | 年 / Year | ジャンル / Genre | 長さ / Duration | 出典 / Source |
|---|---|---|---|---|---|---|
| 1 | Rienzi: Erstehe, hohe Roma, neu (Wagner) | Erik Schmedes (tenor) | 1905 | opera | 2:24 | <https://archive.org/details/ErikSchmedesRienzi342414> |
| 2 | Berliner 42226 | Gustav Waschow (baritone) | 1901 | opera | 1:41 | <https://archive.org/details/GustavWaschowBerliner42226> |
| 3 | Carmen: Habanera (Bizet) | Jeanne Marié de l'Isle (mezzo-soprano) | 1905 | opera | 3:34 | <https://archive.org/details/HabaneraDeLIsleCarmen> |
| 4 | Die lustigen Weiber von Windsor: Garten-Quartett (Nicolai) | Pickelmann/Leux/Kuttner/Hieber | 1909 | opera | 2:49 | <https://archive.org/details/GartenQuartettLustigeWeiber24435> |
| 5 | Play Me Slow | Fletcher Henderson and his Orchestra | 1920s | jazz/orchestra | 3:13 | <https://archive.org/details/PlayMeSlow292D> |
| 6 | Invincible Eagle March (Sousa) | Vess Ossman (banjo, with orchestra) | 1901 | orchestra/march | 3:01 | <https://archive.org/details/InvincibleEagleMarchVessOssman> |
| 7 | Die Zauberflöte: Gli angui d'inferno (Mozart) | Regina Pacini (soprano) | 1905 | opera | 2:40 | <https://archive.org/details/IlFlautoMagicoGliAnguiDinferno> |
| 8 | Estrellita (unpublished take) | Toti Dal Monte (soprano) | 1925 | opera/vocal | 2:34 | <https://archive.org/details/estrellitaunpublished> |
| 9 | Lucia di Lammermoor: Duet (Donizetti) | Frieda Hempel, Hermann Jadlowker | 1911 | opera | 3:32 | <https://archive.org/details/JadlowkerHempelLuciaDuett> |
| 10 | Ernani (Verdi) | Tancredi Pasero (bass) | 1927 | opera | 2:58 | <https://archive.org/details/ErnaniPaseroB1305> |

合計 / total: 約27分 / ~27 minutes

## DSD64エンコーダについて(正直な開示) / About the DSD64 encoder (honest disclosure)

`scripts/pcm_to_dsd.js`は、線形補間による64倍アップサンプル+教科書的な2次
デルタシグマ変調(CRFB型)という簡易な実装で、市販のマスタリング用DSD
エンコーダと同等の音質は謳わない。**テスト用フィクスチャとして「本物の
DSF形式のファイルを再生パイプラインに通せること」を検証する目的**であり、
`open-mqa-dsd`の自前パーサー(`parse_dsf`/`dsd_to_pcm`)とffmpegの両方で
デコードでき、再生時間・サンプル数が一致することを確認済み(2026-09-26)。

`pcm_to_dsd.js` is a simple implementation (64x linear-interpolation upsampling +
a textbook 2nd-order CRFB-style delta-sigma modulator), not a claim of
mastering-grade DSD encoding quality. Its purpose is to produce **genuine,
correctly-structured DSF files** to exercise the playback pipeline as test
fixtures. Verified to decode correctly (matching duration and sample count)
through both this project's own `open-mqa-dsd` parser and ffmpeg (2026-09-26).
