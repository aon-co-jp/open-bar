#!/usr/bin/env bash
# テスト用の実音源(オペラ/オーケストラ/ジャズ、CD相当WAV+DSD64+open-audio)を、
# archive.org(Internet Archive、Great 78 Project等)から取得し、DSD64とopen-audio(.mka)を
# その場で生成する(2026-09-26新設)。
#
# **ここで生成する音声ファイル自体はgitへコミットしない**(WAV+DSD+open-audioで1曲あたり
# 数百MB、10曲で3GB超になり、コミットするとリポジトリが際限なく肥大化するため——
# make-disk/installer/README.mdの「バイナリはGitHub Releasesが正本」という既存方針に倣う)。
# コミットするのはこのスクリプトと`TRACKS.json`(曲目・権利者・ライセンスの一覧)だけ。
#
# **権利について**: 10曲すべて、`TRACKS.json`の`license`が示すとおり
# Creative Commons Public Domain Mark 1.0(=著作権保護期間が満了した録音、archive.orgの
# "Great 78 Project"等が権利状況を確認した上で公開)。録音年は1901〜1920年代で、作曲(原曲)・
# 演奏・原盤(サウンドレコーディング)いずれの権利も存在しない、または権利者不明のまま
# パブリックドメインとして公開されている。「著作権許可を取得した」のではなく、
# **そもそも独占的な著作権が存在しない**という状態(ユーザー確認済み、2026-09-26)。
#
# 使い方: bash scripts/fetch-test-fixtures.sh
# (リポジトリルートから実行。要: curl, node, ffmpeg。ffmpegはmake-disk側の
# src-tauri/binaries/にあるものを使うか、PATH上のものを使う)
set -euo pipefail
cd "$(dirname "$0")/.."

DEST="tests/fixtures"
mkdir -p "$DEST"

FFMPEG="${OPEN_BAR_FFMPEG:-ffmpeg}"
if ! command -v "$FFMPEG" >/dev/null 2>&1 && [ -f "../make-disk/src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe" ]; then
  FFMPEG="../make-disk/src-tauri/binaries/ffmpeg-x86_64-pc-windows-msvc.exe"
fi

OPENAV="${OPEN_BAR_OPENAV:-}"
if [ -z "$OPENAV" ] && [ -f "../open-av/target/release/open-av.exe" ]; then
  OPENAV="../open-av/target/release/open-av.exe"
fi
if [ -z "$OPENAV" ] && [ -f "../open-av/target/release/open-av" ]; then
  OPENAV="../open-av/target/release/open-av"
fi

node -e '
const ids = require("./tests/fixtures/TRACKS.json").map(t => t.id);
console.log(ids.join("\n"));
' > /tmp/_track_ids.txt 2>/dev/null || node -e '
const ids = require("./tests/fixtures/TRACKS.json").map(t => t.id);
console.log(ids.join("\n"));
'

while IFS= read -r id; do
  [ -z "$id" ] && continue
  name=$(node -e "const t=require('./tests/fixtures/TRACKS.json').find(x=>x.id==='$id'); process.stdout.write(t.name)")
  wavFile=$(node -e "const t=require('./tests/fixtures/TRACKS.json').find(x=>x.id==='$id'); process.stdout.write(t.wavFile)")
  out_wav="$DEST/${name}.wav"

  if [ ! -f "$out_wav" ]; then
    echo "downloading $name ..."
    meta_url="https://archive.org/metadata/$id"
    dl_name=$(node -e "
      const https = require('https');
      https.get('$meta_url', res => {
        let body = '';
        res.on('data', d => body += d);
        res.on('end', () => {
          const d = JSON.parse(body);
          const f = d.files.find(x => x.format === 'Wave' || x.format === 'WAVE');
          process.stdout.write(f.name);
        });
      });
    ")
    encoded=$(node -e "process.stdout.write(encodeURIComponent(process.argv[1]))" "$dl_name")
    curl -sL --max-time 120 -o "$out_wav" "https://archive.org/download/$id/$encoded"
  fi

  dsf="$DEST/${name}_dsd64.dsf"
  if [ ! -f "$dsf" ]; then
    echo "encoding DSD64 for $name ..."
    tmp_norm="$DEST/${name}._norm.wav"
    "$FFMPEG" -y -v error -i "$out_wav" -ar 44100 -ac 2 -sample_fmt s16 "$tmp_norm"
    node scripts/pcm_to_dsd.js "$tmp_norm" "$dsf"
    rm -f "$tmp_norm"
  fi

  if [ -n "$OPENAV" ]; then
    mka="$DEST/${name}.open-audio.mka"
    if [ ! -f "$mka" ]; then
      echo "packing open-audio for $name ..."
      wavfb="$DEST/${name}._fallback.wav"
      "$FFMPEG" -y -v error -i "$out_wav" -ar 44100 -ac 2 -c:a pcm_s16le "$wavfb"
      manifest="$DEST/${name}.manifest.json"
      cat > "$manifest" << EOF
{
  "format": "open-audio",
  "version": "0.1",
  "title": "$name",
  "audio_tracks": [
    { "id": "dsd-main", "kind": "dsd", "role": "main", "rate_hz": 2822400, "channels": 2, "layout": "stereo", "title": "DSD64 stereo (main, SACD相当)", "file": "${name}_dsd64.dsf" },
    { "id": "fallback", "kind": "pcm", "role": "fallback", "rate_hz": 44100, "channels": 2, "layout": "stereo", "title": "WAV(ロスレスPCM、CD相当、非対応プレーヤー用)", "stream_index": 0 }
  ]
}
EOF
      OPEN_AV_FFMPEG="$FFMPEG" "$OPENAV" pack "$wavfb" "$manifest" "$mka" "$dsf"
      rm -f "$wavfb"
    fi
  else
    echo "note: open-av binary not found (build ../open-av with 'cargo build --release'), skipping open-audio packing for $name" >&2
  fi
done < /tmp/_track_ids.txt 2>/dev/null || true

echo "done. fixtures are in $DEST (gitignored, not committed)."
