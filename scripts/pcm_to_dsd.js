// 一度限りの変換スクリプト(テスト用フィクスチャ生成専用、リポジトリへは同梱しない)。
// 44.1kHz/16bit stereo WAVを読み、DSD64(2,822,400Hz)の.dsfへ変換する。
// アップサンプル: 線形補間(64倍)。変調: 2次デルタシグマ(教科書的なCRFB型)。
// DSFのビット順は「LSBファースト」(open-mqa-dsd/src/dsd.rsのコメント通り)。
const fs = require('fs');

const inPath = process.argv[2];
const outPath = process.argv[3];

function readWavPcm16Stereo(path) {
  const buf = fs.readFileSync(path);
  if (buf.toString('ascii', 0, 4) !== 'RIFF' || buf.toString('ascii', 8, 12) !== 'WAVE') {
    throw new Error('not a RIFF/WAVE file');
  }
  let pos = 12;
  let fmt = null, dataOff = -1, dataLen = 0;
  while (pos + 8 <= buf.length) {
    const id = buf.toString('ascii', pos, pos + 4);
    const size = buf.readUInt32LE(pos + 4);
    const body = pos + 8;
    if (id === 'fmt ') {
      fmt = { audioFormat: buf.readUInt16LE(body), channels: buf.readUInt16LE(body + 2), sampleRate: buf.readUInt32LE(body + 4), bitsPerSample: buf.readUInt16LE(body + 14) };
    } else if (id === 'data') {
      dataOff = body; dataLen = size;
    }
    pos = body + size + (size % 2);
  }
  if (!fmt || dataOff < 0) throw new Error('missing fmt/data chunk');
  if (fmt.channels !== 2 || fmt.bitsPerSample !== 16) throw new Error(`expected 16bit stereo, got ${fmt.channels}ch/${fmt.bitsPerSample}bit`);
  const n = dataLen / 4; // frames (2ch * 2bytes)
  const left = new Float64Array(n), right = new Float64Array(n);
  for (let i = 0; i < n; i++) {
    left[i] = buf.readInt16LE(dataOff + i * 4) / 32768;
    right[i] = buf.readInt16LE(dataOff + i * 4 + 2) / 32768;
  }
  return { sampleRate: fmt.sampleRate, left, right, frames: n };
}

// 2次デルタシグマ変調(CRFB型、素朴な実装)。xs: -1..1のFloat64Array(アップサンプル後)。戻り値: Uint8Array(0/1)。
function deltaSigma2(xs) {
  const bits = new Uint8Array(xs.length);
  let integ1 = 0, integ2 = 0, fb = 1;
  for (let i = 0; i < xs.length; i++) {
    const e1 = xs[i] - fb;
    integ1 += e1;
    const e2 = integ1 - fb;
    integ2 += e2;
    const bit = integ2 >= 0 ? 1 : -1;
    bits[i] = bit > 0 ? 1 : 0;
    fb = bit;
  }
  return bits;
}

function upsampleLinear(xs, factor) {
  const out = new Float64Array(xs.length * factor);
  for (let i = 0; i < xs.length; i++) {
    const a = xs[i];
    const b = i + 1 < xs.length ? xs[i + 1] : xs[i];
    for (let k = 0; k < factor; k++) {
      out[i * factor + k] = a + (b - a) * (k / factor);
    }
  }
  return out;
}

// bits(0/1のUint8Array) -> バイト列(LSBファースト、8bit単位でパディング)
function packBitsLsbFirst(bits) {
  const nBytes = Math.ceil(bits.length / 8);
  const out = Buffer.alloc(nBytes);
  for (let i = 0; i < bits.length; i++) {
    if (bits[i]) out[i >> 3] |= (1 << (i & 7));
  }
  return out;
}

function main() {
  const wav = readWavPcm16Stereo(inPath);
  if (wav.sampleRate !== 44100) throw new Error(`expected 44100Hz source, got ${wav.sampleRate}`);
  const FACTOR = 64; // DSD64
  const dsdRate = wav.sampleRate * FACTOR;

  const upL = upsampleLinear(wav.left, FACTOR);
  const upR = upsampleLinear(wav.right, FACTOR);
  const bitsL = deltaSigma2(upL);
  const bitsR = deltaSigma2(upR);

  const BLOCK = 4096; // ブロックサイズ(バイト/チャンネル)
  const sampleBitsPerChannel = bitsL.length; // = bitsR.length
  const bytesPerChannelRaw = Math.ceil(sampleBitsPerChannel / 8);
  const blocksNeeded = Math.ceil(bytesPerChannelRaw / BLOCK);
  const bytesPerChannelPadded = blocksNeeded * BLOCK;

  const rawL = packBitsLsbFirst(bitsL);
  const rawR = packBitsLsbFirst(bitsR);
  const chL = Buffer.alloc(bytesPerChannelPadded); rawL.copy(chL);
  const chR = Buffer.alloc(bytesPerChannelPadded); rawR.copy(chR);

  const dataBody = Buffer.alloc(bytesPerChannelPadded * 2);
  for (let b = 0; b < blocksNeeded; b++) {
    chL.copy(dataBody, b * BLOCK * 2, b * BLOCK, (b + 1) * BLOCK);
    chR.copy(dataBody, b * BLOCK * 2 + BLOCK, b * BLOCK, (b + 1) * BLOCK);
  }

  const headerLen = 92;
  const dataChunkSize = 12 + dataBody.length; // DSF仕様: dataチャンクの12バイトヘッダ込みのサイズ
  const fileSize = headerLen + dataBody.length;

  const head = Buffer.alloc(headerLen);
  head.write('DSD ', 0, 'ascii');
  head.writeBigUInt64LE(28n, 4);
  head.writeBigUInt64LE(BigInt(fileSize), 12);
  head.writeBigUInt64LE(0n, 20);
  head.write('fmt ', 28, 'ascii');
  head.writeBigUInt64LE(52n, 32);
  head.writeUInt32LE(1, 40); // format version
  head.writeUInt32LE(0, 44); // format id (DSD raw)
  head.writeUInt32LE(2, 48); // channel type = stereo
  head.writeUInt32LE(2, 52); // channel num
  head.writeUInt32LE(dsdRate, 56);
  head.writeUInt32LE(1, 60); // bits per sample
  head.writeBigUInt64LE(BigInt(sampleBitsPerChannel), 64);
  head.writeUInt32LE(BLOCK, 72);
  head.writeUInt32LE(0, 76);
  head.write('data', 80, 'ascii');
  head.writeBigUInt64LE(BigInt(dataChunkSize), 84);

  fs.writeFileSync(outPath, Buffer.concat([head, dataBody]));
  console.log(`wrote ${outPath}: ${fileSize} bytes, DSD${FACTOR} (${dsdRate}Hz), ${(sampleBitsPerChannel / dsdRate).toFixed(2)}s`);
}

main();
