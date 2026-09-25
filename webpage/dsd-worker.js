// DSD(DSF)→PCM変換のWebWorker。デスクトップ版(open-mqa-dsd)と同じ設計: Kaiser窓sincのFIR間引き、8タップずつ256通りの表引き。
// Web Worker that decimates DSD (DSF) to PCM — same design as the desktop build (open-mqa-dsd): Kaiser-windowed sinc FIR, byte-table lookup.

function besselI0(x) {
  let sum = 1, term = 1, k = 1;
  while (term > 1e-12 * sum) { term *= (x / (2 * k)) ** 2; sum += term; k += 1; }
  return sum;
}

function designLowpass(taps, fc, beta) {
  const m = (taps - 1) / 2, i0b = besselI0(beta);
  const h = new Float64Array(taps);
  let sum = 0;
  for (let n = 0; n < taps; n++) {
    const x = n - m;
    const sinc = x === 0 ? 2 * fc : Math.sin(2 * Math.PI * fc * x) / (Math.PI * x);
    const r = x / m;
    h[n] = sinc * besselI0(beta * Math.sqrt(Math.max(0, 1 - r * r))) / i0b;
    sum += h[n];
  }
  for (let n = 0; n < taps; n++) h[n] /= sum;
  return h;
}

class Decimator {
  constructor(dsdRate, outRate, cutoffHz = 40000) {
    if (dsdRate % outRate !== 0) throw new Error("出力レートがDSDレートを割り切れません");
    this.r = dsdRate / outRate;
    if (this.r % 8 !== 0) throw new Error("間引き率が8の倍数ではありません");
    const taps = Math.floor((Math.max(this.r * 12, 256) + 15) / 16) * 16;
    const fc = Math.min(cutoffHz, outRate * 0.45) / dsdRate;
    const h = designLowpass(taps, fc, 9.0);
    this.half = taps / 2;
    this.groups = taps / 8;
    this.table = new Float32Array(this.groups * 256);
    for (let g = 0; g < this.groups; g++) {
      for (let byte = 0; byte < 256; byte++) {
        let s = 0;
        for (let i = 0; i < 8; i++) s += ((byte >> (7 - i)) & 1) ? h[8 * g + i] : -h[8 * g + i];
        this.table[g * 256 + byte] = s;
      }
    }
  }
  // bits: 1チャンネル分(MSBファースト・時間順)。出力フレーム[first, first+count)を返す。
  range(bits, first, count) {
    const out = new Float32Array(count);
    for (let n = 0; n < count; n++) {
      const start = (first + n) * this.r - this.half; // 常にバイト境界
      let acc = 0;
      for (let g = 0; g < this.groups; g++) {
        const bit = start + g * 8;
        if (bit < 0) continue;
        const bi = bit >> 3;
        if (bi >= bits.length) continue;
        acc += this.table[g * 256 + bits[bi]];
      }
      out[n] = acc;
    }
    return out;
  }
}

// DSF → チャンネルごとの時間順・MSBファーストのバイト列
function parseDsf(buf) {
  const dv = new DataView(buf);
  const tag = (o) => String.fromCharCode(dv.getUint8(o), dv.getUint8(o + 1), dv.getUint8(o + 2), dv.getUint8(o + 3));
  if (buf.byteLength < 92 || tag(0) !== "DSD " || tag(28) !== "fmt " || tag(80) !== "data") throw new Error("DSFではありません");
  const channels = dv.getUint32(52, true), rate = dv.getUint32(56, true), bitsPer = dv.getUint32(60, true);
  const sampleBits = Number(dv.getBigUint64(64, true)), block = dv.getUint32(72, true);
  const dataSize = Number(dv.getBigUint64(84, true)) - 12;
  const bytes = new Uint8Array(buf, 92, Math.min(dataSize, buf.byteLength - 92));
  const valid = Math.ceil(sampleBits / 8);
  const rev = new Uint8Array(256);
  for (let b = 0; b < 256; b++) { let r = 0; for (let i = 0; i < 8; i++) if (b & (1 << i)) r |= 0x80 >> i; rev[b] = r; }
  const out = [];
  for (let c = 0; c < channels; c++) out.push(new Uint8Array(valid));
  const group = block * channels;
  for (let g = 0, pos = 0; g * group < bytes.length; g++, pos += block) {
    for (let c = 0; c < channels; c++) {
      const start = g * group + c * block;
      const len = Math.max(0, Math.min(block, bytes.length - start, valid - pos));
      for (let i = 0; i < len; i++) out[c][pos + i] = bitsPer === 1 ? rev[bytes[start + i]] : bytes[start + i];
    }
  }
  return { channels, rate, sampleBits, bits: out };
}

let stream = null, dec = null, outRate = 0;

self.onmessage = (ev) => {
  const m = ev.data;
  try {
    if (m.type === "open") {
      stream = parseDsf(m.buffer);
      // 出力レート: 176.4kHz以下で、DSDレートを8の倍数で割り切れる最大のもの(デスクトップ版の共有モードと同じ)
      let r = 8;
      while (stream.rate / r > 176400) r *= 2;
      outRate = stream.rate / r;
      dec = new Decimator(stream.rate, outRate);
      const total = Math.floor(stream.bits[0].length * 8 / dec.r);
      self.postMessage({ type: "opened", id: m.id, channels: stream.channels, dsdRate: stream.rate, outRate, totalFrames: total });
    } else if (m.type === "read") {
      const chans = stream.bits.map((b) => dec.range(b, m.first, m.count));
      self.postMessage({ type: "chunk", id: m.id, first: m.first, chans }, chans.map((c) => c.buffer));
    }
  } catch (e) {
    self.postMessage({ type: "error", id: m.id, message: String(e.message || e) });
  }
};
