// open-bar Web版: ランディング(インストール版の自動起動・インストーラーのダウンロード)と、ブラウザ内プレーヤー。
// Landing (auto-launch of the installed app, installer download) + an in-browser player.
"use strict";

const $ = (id) => document.getElementById(id);
const REPO = "aon-co-jp/open-bar";

// ---------------- インストーラーのダウンロード先 ----------------
async function setupDownload() {
  const ua = navigator.userAgent;
  const os = /Windows/i.test(ua) ? "win" : /Mac/i.test(ua) ? "mac" : /Linux|X11/i.test(ua) ? "linux" : "other";
  try {
    const r = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, { headers: { Accept: "application/vnd.github+json" } });
    if (!r.ok) throw new Error(r.status);
    const rel = await r.json();
    const find = (re) => (rel.assets || []).find((a) => re.test(a.name));
    let a = null, label = "";
    if (os === "win") { a = find(/x64-setup\.exe$/); label = "Windows"; }
    else if (os === "mac") { a = find(/aarch64\.dmg$/) || find(/x64\.dmg$/); label = "macOS"; }
    else if (os === "linux") { a = find(/\.AppImage$/) || find(/\.deb$/); label = "Linux"; }
    if (a) {
      const btn = $("dl-main");
      btn.href = a.browser_download_url;
      btn.textContent = `${label}用インストーラーをダウンロード / Download for ${label} (${rel.tag_name})`;
    }
    const others = (rel.assets || []).filter((x) => /\.(exe|msi|dmg|AppImage|deb|rpm)$/.test(x.name)).map((x) => `<a href="${x.browser_download_url}">${x.name}</a>`).join(" ・ ");
    $("dl-version").innerHTML = `最新版 / Latest: <b>${rel.tag_name}</b> ・ ${others}`;
  } catch (e) {
    $("dl-version").textContent = "";
  }
}

// ---------------- この端末のトークンと起動状態(プレゼンス) ----------------
// トークンはこのブラウザだけが持つランダム文字列(アカウント無し)。インストール版を`openbar://launch?token=…`で起動すると、
// アプリが同じトークンで「起動中」を知らせ、このページがそれを表示する。
function getToken() {
  try {
    let t = localStorage.getItem("openbar-token");
    if (!t || !/^[A-Za-z0-9_-]{16,64}$/.test(t)) {
      const b = new Uint8Array(18);
      crypto.getRandomValues(b);
      t = btoa(String.fromCharCode(...b)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=/g, "");
      localStorage.setItem("openbar-token", t);
    }
    return t;
  } catch (e) {
    // localStorageが使えない環境: このページを開いている間だけ有効なトークン
    if (!window.__obarTmpToken) {
      const b = new Uint8Array(18);
      crypto.getRandomValues(b);
      window.__obarTmpToken = btoa(String.fromCharCode(...b)).replace(/\+/g, "-").replace(/\//g, "_").replace(/=/g, "");
    }
    return window.__obarTmpToken;
  }
}

const UA = navigator.userAgent;
const IS_TABLET = /iPad|Tablet/i.test(UA) || (/Android/i.test(UA) && !/Mobile/i.test(UA)) || (/Macintosh/i.test(UA) && navigator.maxTouchPoints > 1);
const IS_PHONE = !IS_TABLET && /iPhone|Android.*Mobile|Mobile/i.test(UA);
const DEVICE = IS_TABLET ? "tablet" : IS_PHONE ? "phone" : "pc";
const DEVICE_LABEL = { pc: ["PC版", "PC app"], phone: ["スマホ版", "Phone app"], tablet: ["タブレット版", "Tablet app"] };
const API = new URL("api/presence", location.href).pathname;

async function webBeat() {
  try {
    await fetch(API, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ token: getToken(), device: "web", name: "Web版", app_version: "web", state: P && P.state === "playing" ? "playing" : "idle" }) });
  } catch (e) { /* オフライン等は無視 */ }
}

async function refreshRunning() {
  const box = $("run-list");
  let devices = [];
  let apiOk = true;
  try {
    const r = await fetch(`${API}?token=${encodeURIComponent(getToken())}`, { cache: "no-store" });
    const d = await r.json();
    devices = d.devices || [];
  } catch (e) { apiOk = false; }
  const mine = devices.find((x) => x.device === DEVICE);
  const web = devices.find((x) => x.device === "web");
  const [ja, en] = DEVICE_LABEL[DEVICE];
  const row = (label, on, detail) => `<div style="margin:.15rem 0"><span class="badge" style="background:${on ? "var(--ok)" : "var(--line)"};color:${on ? "#fff" : "inherit"}">${on ? "起動中 / Running" : "未起動 / Not running"}</span> <b>${label}</b> <span style="color:var(--dim)">${detail || ""}</span></div>`;
  let html = "";
  if (DEVICE === "pc") {
    html += row(`${ja} / ${en}`, !!mine, mine ? `${mine.name} · v${mine.app_version}${mine.state === "playing" ? " · 再生中 / playing" : ""}` : "インストールして起動すると、ここに表示されます / Shows here once installed and started");
  } else {
    html += row(`${ja} / ${en}`, !!mine, mine ? mine.name : "このOS向けのアプリは準備中です / An app for this OS is not available yet");
  }
  html += row("Web版 / Web player", !!web, web ? "このページ / this page" : "");
  if (!apiOk) html += `<div class="msg">状態を取得できませんでした(オフライン?) / Could not fetch the status (offline?)</div>`;
  box.innerHTML = html;
}

// ---------------- インストール版の自動起動(openbar://) ----------------
function tryLaunch(showResult = true) {
  return new Promise((resolve) => {
    let launched = false;
    const mark = () => { launched = true; };
    window.addEventListener("blur", mark, { once: true });
    const vis = () => { if (document.hidden) launched = true; };
    document.addEventListener("visibilitychange", vis);
    const f = document.createElement("iframe");
    f.style.display = "none";
    f.src = `openbar://launch?token=${encodeURIComponent(getToken())}`;
    document.body.appendChild(f);
    setTimeout(() => {
      document.removeEventListener("visibilitychange", vis);
      f.remove();
      const st = $("launch-status");
      if (showResult) {
        if (launched) {
          st.textContent = "✓ インストール版を起動しました(または許可待ちです)。 / Launched the installed app (or waiting for your permission).";
          st.className = "status ok";
        } else {
          st.textContent = "インストール版が見つかりません。下のWeb版で再生できます。 / The installed app was not found. You can play in the web player below.";
          st.className = "status warn";
        }
      }
      resolve(launched);
    }, 2500);
  });
}

function setupLaunch() {
  const noAuto = $("no-auto");
  try { noAuto.checked = localStorage.getItem("openbar-no-auto") === "1"; } catch (e) { /* 保存できなくても続行 */ }
  noAuto.addEventListener("change", () => { try { localStorage.setItem("openbar-no-auto", noAuto.checked ? "1" : "0"); } catch (e) { /* noop */ } });
  $("launch-btn").addEventListener("click", () => tryLaunch(true));
  const params = new URLSearchParams(location.search);
  if (noAuto.checked || params.has("web")) {
    $("launch-status").textContent = "自動起動はオフです。「インストール版を起動」を押すと起動します。 / Auto-launch is off. Press the launch button to start the app.";
    return;
  }
  if (DEVICE !== "pc") {
    $("launch-status").textContent = "スマホ・タブレット向けのアプリは準備中です。下のWeb版で再生できます。 / Phone and tablet apps are not available yet. Use the web player below.";
    $("launch-btn").style.display = "none";
    return;
  }
  tryLaunch(true);
}

// ---------------- Web版プレーヤー ----------------
const fmtTime = (s) => {
  if (!isFinite(s) || s < 0) s = 0;
  s = Math.floor(s);
  const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
};
const esc = (t) => t.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

const P = {
  items: [],
  cur: -1,
  ctx: null,
  gain: null,
  state: "stopped", // stopped | playing | paused
  worker: null,
  sources: [], // 予約済みのソースノード
  startCtx: 0, // 再生位置0秒に相当するctx時刻
  offset: 0, // 再生開始位置(秒)
  dsd: null, // { total, outRate, channels, nextFirst, scheduledUntil, pending }
  reqId: 0,
  timer: null,
};

function ensureCtx() {
  if (!P.ctx) {
    P.ctx = new (window.AudioContext || window.webkitAudioContext)();
    P.gain = P.ctx.createGain();
    P.gain.gain.value = $("vol").value / 100;
    P.gain.connect(P.ctx.destination);
  }
  return P.ctx;
}

function msg(t) { $("msg").textContent = t || ""; }

function render() {
  const body = $("list");
  body.innerHTML = "";
  P.items.forEach((it, i) => {
    const tr = document.createElement("tr");
    if (i === P.cur && P.state !== "stopped") tr.className = "playing";
    tr.innerHTML = `<td class="n">${i === P.cur && P.state !== "stopped" ? "►" : i + 1}</td><td>${esc(it.file.name)}</td><td class="f">${esc(it.info || "")}</td><td class="t">${it.duration ? fmtTime(it.duration) : ""}</td>`;
    tr.addEventListener("dblclick", () => play(i, 0));
    tr.addEventListener("click", () => { if (P.state === "stopped") { P.cur = i; } });
    body.appendChild(tr);
  });
}

function isDsd(name) { return /\.dsf$/i.test(name); }

async function addFiles(files) {
  for (const file of files) {
    P.items.push({ file, info: isDsd(file.name) ? "DSD" : "PCM", duration: 0 });
  }
  render();
  if (P.cur < 0 && P.items.length) P.cur = 0;
}

function stopSources() {
  for (const s of P.sources) { try { s.onended = null; s.stop(); } catch (e) { /* 停止済み */ } }
  P.sources = [];
}

function stopAll() {
  stopSources();
  if (P.timer) { clearInterval(P.timer); P.timer = null; }
  P.dsd = null;
  P.state = "stopped";
  $("play").textContent = "▶";
  render();
}

function position() {
  if (P.state === "stopped") return 0;
  return Math.max(0, P.offset + (P.ctx.currentTime - P.startCtx));
}

async function play(i, offsetSec) {
  if (i < 0 || i >= P.items.length) return;
  const ctx = ensureCtx();
  if (ctx.state === "suspended") await ctx.resume();
  stopAll();
  P.cur = i;
  const it = P.items[i];
  msg("読み込み中… / Loading…");
  try {
    if (isDsd(it.file.name)) await playDsd(it, offsetSec);
    else await playPcm(it, offsetSec);
    msg("");
  } catch (e) {
    msg(`再生できません(${it.file.name}): ${e.message || e} / Cannot play: ${e.message || e}`);
    stopAll();
    return;
  }
  P.state = "playing";
  $("play").textContent = "⏸";
  render();
  if (!P.timer) P.timer = setInterval(tick, 200);
}

async function playPcm(it, offsetSec) {
  if (!it.buffer) {
    const data = await it.file.arrayBuffer();
    it.buffer = await P.ctx.decodeAudioData(data);
    it.duration = it.buffer.duration;
    it.info = `PCM ${(it.buffer.sampleRate / 1000).toFixed(1)} kHz ${it.buffer.numberOfChannels}ch`;
  }
  const src = P.ctx.createBufferSource();
  src.buffer = it.buffer;
  src.connect(P.gain);
  src.onended = () => { if (P.sources.includes(src) && P.state === "playing") next(); };
  P.offset = offsetSec;
  P.startCtx = P.ctx.currentTime;
  src.start(0, offsetSec);
  P.sources = [src];
}

function dsdWorker() {
  if (!P.worker) {
    P.worker = new Worker("dsd-worker.js");
    P.worker.onmessage = (ev) => onWorker(ev.data);
  }
  return P.worker;
}

const pendingOpen = new Map();
function onWorker(m) {
  if (m.type === "opened" || m.type === "error") {
    const p = pendingOpen.get(m.id);
    if (p) { pendingOpen.delete(m.id); m.type === "error" ? p.reject(new Error(m.message)) : p.resolve(m); }
    if (m.type === "error" && P.dsd) msg(m.message);
  } else if (m.type === "chunk") {
    const d = P.dsd;
    if (!d || m.id !== d.pendingId) return;
    d.pendingId = 0;
    const frames = m.chans[0].length;
    const buf = P.ctx.createBuffer(d.channels, frames, d.outRate);
    m.chans.forEach((c, ch) => buf.copyToChannel(c, ch));
    const src = P.ctx.createBufferSource();
    src.buffer = buf;
    src.connect(P.gain);
    const at = Math.max(d.scheduledUntil, P.ctx.currentTime + 0.05);
    src.start(at);
    P.sources.push(src);
    d.scheduledUntil = at + frames / d.outRate;
    d.nextFirst = m.first + frames;
    if (d.nextFirst >= d.total && P.state === "playing") {
      src.onended = () => { if (P.state === "playing" && d.nextFirst >= d.total) next(); };
    }
  }
}

async function playDsd(it, offsetSec) {
  const buffer = await it.file.arrayBuffer();
  const id = ++P.reqId;
  const opened = await new Promise((resolve, reject) => {
    pendingOpen.set(id, { resolve, reject });
    dsdWorker().postMessage({ type: "open", id, buffer }, [buffer]);
  });
  it.duration = opened.totalFrames / opened.outRate;
  it.info = `DSD${Math.round(opened.dsdRate / 44100)} → PCM ${(opened.outRate / 1000).toFixed(1)} kHz ${opened.channels}ch`;
  const first = Math.floor(offsetSec * opened.outRate);
  P.dsd = { total: opened.totalFrames, outRate: opened.outRate, channels: opened.channels, nextFirst: first, scheduledUntil: P.ctx.currentTime + 0.1, pendingId: 0 };
  P.offset = offsetSec;
  P.startCtx = P.ctx.currentTime + 0.1;
  pumpDsd();
}

function pumpDsd() {
  const d = P.dsd;
  if (!d || d.pendingId || d.nextFirst >= d.total) return;
  if (d.scheduledUntil - P.ctx.currentTime > 12) return; // 先読みは約12秒
  const count = Math.min(Math.floor(d.outRate * 4), d.total - d.nextFirst);
  d.pendingId = ++P.reqId;
  dsdWorker().postMessage({ type: "read", id: d.pendingId, first: d.nextFirst, count });
}

function tick() {
  if (P.state === "stopped") return;
  const it = P.items[P.cur];
  const pos = position();
  const dur = it ? it.duration : 0;
  $("pos").textContent = fmtTime(pos);
  $("dur").textContent = fmtTime(dur);
  if (dur > 0 && !seekDragging) $("seek").value = Math.round((pos / dur) * 1000);
  $("now").textContent = it ? `${P.state === "paused" ? "一時停止 / Paused" : "再生中 / Playing"}: ${it.file.name} (${it.info})` : "";
  pumpDsd();
}

function next() { if (P.cur + 1 < P.items.length) play(P.cur + 1, 0); else stopAll(); }
function prev() { if (P.cur > 0) play(P.cur - 1, 0); }

let seekDragging = false;
function setup() {
  setupDownload();
  setupLaunch();
  refreshRunning();
  webBeat();
  setInterval(refreshRunning, 5000);
  setInterval(webBeat, 10000);
  $("files").addEventListener("change", (e) => addFiles(e.target.files));
  const drop = $("drop");
  ["dragenter", "dragover"].forEach((ev) => drop.addEventListener(ev, (e) => { e.preventDefault(); drop.classList.add("over"); }));
  ["dragleave", "drop"].forEach((ev) => drop.addEventListener(ev, (e) => { e.preventDefault(); drop.classList.remove("over"); }));
  drop.addEventListener("drop", (e) => addFiles(e.dataTransfer.files));
  $("play").addEventListener("click", async () => {
    if (P.state === "playing") { await P.ctx.suspend(); P.state = "paused"; $("play").textContent = "▶"; render(); }
    else if (P.state === "paused") { await P.ctx.resume(); P.state = "playing"; $("play").textContent = "⏸"; render(); }
    else if (P.items.length) play(Math.max(P.cur, 0), 0);
  });
  $("stop").addEventListener("click", stopAll);
  $("next").addEventListener("click", next);
  $("prev").addEventListener("click", prev);
  $("vol").addEventListener("input", () => { if (P.gain) P.gain.gain.value = $("vol").value / 100; });
  const seek = $("seek");
  seek.addEventListener("pointerdown", () => (seekDragging = true));
  seek.addEventListener("change", () => {
    seekDragging = false;
    const it = P.items[P.cur];
    if (it && it.duration && P.state !== "stopped") play(P.cur, (seek.value / 1000) * it.duration);
  });
  render();
}

window.__obar = { addFiles, P, play, tryLaunch, getToken, refreshRunning, DEVICE };
setup();
