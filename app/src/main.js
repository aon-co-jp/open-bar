// バンドラー無しの素のJS。Tauriのグローバル(withGlobalTauri)を使う(make-diskと同じ方針)。
const invoke = window.__TAURI__.core.invoke;
const dialogOpen = window.__TAURI__.dialog.open;

const $ = (id) => document.getElementById(id);
/** @type {{path:string, info:any}[]} */
let items = [];
let currentIndex = null;
let dragging = false;
let lastState = "stopped";

const fmtTime = (s) => {
  if (!isFinite(s) || s < 0) s = 0;
  s = Math.floor(s);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
};
const baseName = (p) => p.replace(/\\/g, "/").split("/").pop();
const esc = (t) => t.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));

function describe(info) {
  if (!info) return "";
  const parts = [];
  if (info.kind === "dsd") {
    const mult = info.sample_rate_hz ? Math.round(info.sample_rate_hz / 44100) : 0;
    parts.push(`<span class="badge dsd">DSD${mult || ""}</span>`);
  } else if (info.kind === "video_container") {
    parts.push('<span class="badge">動画 / video</span>');
  }
  if (info.is_mqa) parts.push('<span class="badge mqa">MQA</span>');
  if (info.kind !== "dsd" && info.sample_rate_hz) {
    const khz = info.sample_rate_hz / 1000;
    parts.push(`${khz % 1 === 0 ? khz : khz.toFixed(1)} kHz` + (info.bits_per_sample ? ` / ${info.bits_per_sample}bit` : ""));
  } else if (info.kind === "dsd" && info.sample_rate_hz) {
    parts.push(`${(info.sample_rate_hz / 1e6).toFixed(4)} MHz`);
  }
  if (info.channels) parts.push(`${info.channels}ch`);
  return parts.join(" ");
}

function render() {
  const body = $("playlist-body");
  body.innerHTML = "";
  items.forEach((it, i) => {
    const tr = document.createElement("tr");
    if (i === currentIndex) tr.className = "playing";
    tr.innerHTML = `<td class="c-num">${i === currentIndex ? "►" : i + 1}</td><td class="c-file" title="${esc(it.path)}">${esc(baseName(it.path))}</td><td class="c-fmt">${describe(it.info)}</td><td class="c-dur">${it.info && it.info.duration_secs ? fmtTime(it.info.duration_secs) : ""}</td>`;
    tr.addEventListener("dblclick", () => playIndex(i));
    body.appendChild(tr);
  });
  $("empty").style.display = items.length ? "none" : "block";
}

async function syncPlaylist() {
  await invoke("player_set_playlist", { files: items.map((i) => i.path) });
}

async function addPaths(paths) {
  for (const path of paths) {
    let info = null;
    try {
      info = await invoke("probe_file", { path });
    } catch (e) {
      /* 情報が取れなくても一覧には載せる */
    }
    items.push({ path, info });
  }
  await syncPlaylist();
  render();
}

async function playIndex(i) {
  if (i < 0 || i >= items.length) return;
  await syncPlaylist();
  await invoke("player_play", { index: i });
}

$("add-files-btn").addEventListener("click", async () => {
  const selected = await dialogOpen({
    multiple: true,
    filters: [
      { name: "音声・動画 / Audio & video", extensions: ["wav", "flac", "mp3", "aac", "m4a", "ogg", "opus", "aiff", "alac", "mka", "wv", "dsf", "dff", "mp4", "mkv", "webm", "mov", "avi"] },
      { name: "すべて / All", extensions: ["*"] },
    ],
  });
  if (!selected) return;
  await addPaths(Array.isArray(selected) ? selected : [selected]);
});

$("add-folder-btn").addEventListener("click", async () => {
  const dir = await dialogOpen({ directory: true });
  if (!dir) return;
  const files = await invoke("scan_folder", { path: dir });
  await addPaths(files);
});

$("clear-btn").addEventListener("click", async () => {
  await invoke("player_stop");
  items = [];
  currentIndex = null;
  await syncPlaylist();
  render();
});

async function togglePlay() {
  if (lastState === "playing") await invoke("player_pause");
  else if (lastState === "paused") await invoke("player_resume");
  else if (items.length) await playIndex(currentIndex ?? 0);
}
$("play-btn").addEventListener("click", togglePlay);
$("stop-btn").addEventListener("click", () => invoke("player_stop"));
$("next-btn").addEventListener("click", () => invoke("player_next"));
$("prev-btn").addEventListener("click", () => invoke("player_prev"));
window.addEventListener("keydown", (e) => {
  if (e.code === "Space" && e.target === document.body) {
    e.preventDefault();
    togglePlay();
  }
});

const volEl = $("volume");
volEl.addEventListener("input", () => invoke("player_set_volume", { volume: volEl.value / 100 }));

const seekEl = $("seek");
seekEl.addEventListener("pointerdown", () => (dragging = true));
seekEl.addEventListener("change", async () => {
  const st = await invoke("player_status");
  await invoke("player_seek", { secs: (seekEl.value / 1000) * st.duration_secs });
  dragging = false;
});

// ---- 再生モード(A/B/E/D)の選択と、再生中の大きな表示 ----
let modes = [];
async function initModes() {
  modes = await invoke("player_modes");
  const box = $("mode-buttons");
  box.innerHTML = "";
  for (const m of modes) {
    const b = document.createElement("button");
    b.dataset.mode = m.id;
    b.title = `${m.desc_ja}
${m.desc_en}`;
    b.innerHTML = `<span class="l">${m.letter}: ${esc(m.title_ja)}</span><span class="e">${esc(m.title_en)}</span>`;
    b.addEventListener("click", () => invoke("player_set_mode", { mode: m.id }));
    box.appendChild(b);
  }
}

function renderBanner(st) {
  const a = st.active_mode;
  const playing = st.state !== "stopped";
  document.querySelectorAll("#mode-buttons button").forEach((b) => b.classList.toggle("selected", b.dataset.mode === st.selected_mode));
  const banner = $("mode-banner");
  banner.classList.toggle("bitperfect", !!a.bit_perfect);
  banner.classList.toggle("shared", !a.bit_perfect);
  $("mode-title-ja").textContent = `${a.letter}  ${a.title_ja}` + (playing ? "" : "(選択中 / selected)");
  $("mode-title-en").textContent = a.title_en;
  $("mode-route").textContent = playing ? `${st.route_ja}  /  ${st.route_en}` : "";
  const vol100 = Math.round(st.volume * 100) >= 100;
  const volNote = playing && a.bit_perfect && !vol100 ? "音量が100%未満のため、デジタルで音量処理をしています(ビットパーフェクトではありません)。/ Volume is below 100%, so digital volume is applied (not bit-perfect)." : "";
  $("mode-note").textContent = [st.note, volNote].filter(Boolean).join("  ");
  $("mode-desc").textContent = `${a.desc_ja}  ${a.desc_en}`;
}

async function poll() {
  try {
    const st = await invoke("player_status");
    lastState = st.state;
    currentIndex = st.state === "stopped" ? null : st.index;
    $("play-btn").textContent = st.state === "playing" ? "⏸" : "▶";
    const name = st.path ? baseName(st.path) : "";
    $("status-main").textContent = st.state === "playing" ? `再生中 / Playing: ${name}` : st.state === "paused" ? `一時停止 / Paused: ${name}` : "停止 / Stopped";
    renderBanner(st);
    $("status-route").textContent = st.device ? `${st.device}` : "";
    $("status-msg").textContent = st.message || "";
    $("pos-label").textContent = fmtTime(st.position_secs);
    $("dur-label").textContent = fmtTime(st.duration_secs);
    if (!dragging && st.duration_secs > 0) seekEl.value = Math.round((st.position_secs / st.duration_secs) * 1000);
    if (st.state === "stopped") seekEl.value = 0;
    // 再生中の行の強調を更新
    document.querySelectorAll("#playlist-body tr").forEach((tr, i) => {
      const playing = i === currentIndex;
      tr.classList.toggle("playing", playing);
      tr.firstChild.textContent = playing ? "►" : i + 1;
    });
  } catch (e) {
    $("status-msg").textContent = String(e);
  }
}
setInterval(poll, 250);
render();
initModes();

// 起動時にコマンドライン引数のファイルがあれば、追加して先頭から再生する。
(async () => {
  try {
    const files = await invoke("startup_files");
    if (files.length) {
      await addPaths(files);
      await playIndex(0);
    }
  } catch (e) {
    $("status-msg").textContent = String(e);
  }
})();
