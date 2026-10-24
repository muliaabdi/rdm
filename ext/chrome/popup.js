// RDM popup: live jobs, speed/ETA, filters, takeover switch, connection
// diagnostics. Own native port, independent of the background worker.

const HOST = "com.rdm.host";
const jobs = new Map(); // id -> {filename,size,done,status,error}
const speeds = new Map(); // id -> {last,ts,ema}
let filter = "all";
let port = null;
let conn = { state: "connecting", note: "" }; // connecting | online | offline
let rttMs = null; // last list -> jobs round trip
let greetMs = null; // connect -> settings greeting
let seenFrame = false;

const $ = (id) => document.getElementById(id);
const ACTIVE = ["running", "resolving", "confirming", "queued", "paused"];
const ST_LABEL = {
  running: "Running", resolving: "Resolving", confirming: "Confirming",
  queued: "Queued", paused: "Paused", completed: "Done",
  failed: "Failed", cancelled: "Cancelled",
};

const ICON = {
  pause: '<svg width="11" height="11" viewBox="0 0 12 12" aria-hidden="true"><rect x="2" y="1.5" width="2.7" height="9" rx="0.9" fill="currentColor"/><rect x="7.3" y="1.5" width="2.7" height="9" rx="0.9" fill="currentColor"/></svg>',
  resume: '<svg width="11" height="11" viewBox="0 0 12 12" aria-hidden="true"><path d="M3 1.7v8.6c0 .5.6.8 1 .5l6.8-4.3c.4-.2.4-.8 0-1L4 1.2c-.4-.3-1 0-1 .5z" fill="currentColor"/></svg>',
  cancel: '<svg width="11" height="11" viewBox="0 0 12 12" aria-hidden="true"><path d="M2.6 2.6l6.8 6.8M9.4 2.6L2.6 9.4" stroke="currentColor" stroke-width="1.7" fill="none" stroke-linecap="round"/></svg>',
};

function fmtBytes(n) {
  if (n == null) return "?";
  if (n >= 1 << 30) return (n / (1 << 30)).toFixed(1) + " GB";
  if (n >= 1 << 20) return (n / (1 << 20)).toFixed(1) + " MB";
  if (n >= 1 << 10) return (n / (1 << 10)).toFixed(0) + " KB";
  return n + " B";
}
function fmtSpeed(bps) {
  if (bps >= 1 << 20) return (bps / (1 << 20)).toFixed(1) + " MB/s";
  if (bps >= 1 << 10) return (bps / (1 << 10)).toFixed(0) + " KB/s";
  return Math.round(bps) + " B/s";
}
function fmtEta(sec) {
  if (!isFinite(sec) || sec <= 0 || sec > 86400) return "";
  const h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = Math.floor(sec % 60);
  return h ? `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}` : `${m}:${String(s).padStart(2, "0")}`;
}
function esc(s) {
  return String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}
function bucket(st) {
  if (st === "completed") return "done";
  if (st === "failed") return "failed";
  if (ACTIVE.includes(st)) return "active";
  return "all"; // cancelled and unknown stay under All only
}

/* Speed is measured client-side from progress deltas (EMA, refreshed only
   while the popup is open; nothing is invented server-side). */
function noteProgress(id, done) {
  const now = performance.now();
  let s = speeds.get(id);
  if (!s) { s = { last: null, ts: 0, ema: 0 }; speeds.set(id, s); }
  if (s.last == null || done < s.last) {
    s.last = done; s.ts = now;
    return;
  }
  const dt = now - s.ts;
  if (dt >= 250) {
    const inst = ((done - s.last) * 1000) / dt;
    s.ema = s.ema ? s.ema * 0.3 + inst * 0.7 : inst;
    s.last = done;
    s.ts = now;
  }
}

function apply(v) {
  const j = jobs.get(v.id) || { id: v.id, filename: "?", done: 0, size: null, status: "running" };
  if (v.filename != null) j.filename = v.filename;
  if (v.size != null) j.size = v.size;
  if (v.done != null) j.done = v.done;
  if (v.status != null) j.status = v.status;
  if (v.error != null) j.error = v.error;
  jobs.set(v.id, j);
  render();
}

/* ---- rendering ---- */

function render() {
  renderConn();
  renderTabs();
  renderJobs();
}

function renderConn() {
  const pill = $("conn"), label = $("conn-label"), banner = $("offline");
  pill.className = "conn " + (conn.state === "online" ? "online" : conn.state === "offline" ? "offline" : "");
  label.textContent = conn.state === "online" ? "Connected" : conn.state === "offline" ? "Offline" : "Connecting";
  const off = conn.state === "offline";
  if (off) {
    banner.removeAttribute("hidden");
    $("offline-msg").textContent = conn.note
      ? conn.note.slice(0, 90)
      : "Is rdm-daemon running?";
  } else {
    banner.setAttribute("hidden", "");
  }
}

function renderTabs() {
  const c = { all: jobs.size, active: 0, done: 0, failed: 0 };
  for (const j of jobs.values()) c[bucket(j.status)]++;
  for (const k of ["all", "active", "done", "failed"]) $("c-" + k).textContent = c[k];
}

function renderJobs() {
  const box = $("jobs");
  if (!seenFrame) {
    box.innerHTML = '<div class="state"><span class="spin" aria-hidden="true"></span><b>Connecting to RDM</b>Waiting for the daemon to answer.</div>';
    return;
  }
  const list = [...jobs.values()].filter((j) => filter === "all" || bucket(j.status) === filter)
    .sort((a, b) => b.id - a.id);
  if (!list.length) {
    const msg = filter === "all"
      ? "<b>Nothing here yet</b>Intercepted downloads appear in this list."
      : `<b>No ${filter === "done" ? "completed" : filter} downloads</b>Switch to All to see every job.`;
    box.innerHTML = `<div class="state">${msg}</div>`;
    return;
  }
  box.innerHTML = list.map((j) => {
    const pct = j.size ? Math.min(100, Math.round((j.done / j.size) * 100)) : 0;
    const st = ST_LABEL[j.status] || j.status;
    const sp = speeds.get(j.id);
    const fresh = sp && sp.ts && Date.now() - sp.ts < 3500 && sp.ema > 1 && j.status === "running";
    const parts = [st];
    if (j.status === "running" || j.status === "paused") parts.push(`${fmtBytes(j.done)} / ${fmtBytes(j.size)} · ${pct}%`);
    else if (j.status === "completed") parts.push(fmtBytes(j.size ?? j.done));
    if (fresh) {
      parts.push(fmtSpeed(sp.ema));
      const eta = j.size > j.done ? fmtEta((j.size - j.done) / sp.ema) : "";
      if (eta) parts.push(eta + " left");
    }
    let act = "";
    if (["running", "resolving", "confirming"].includes(j.status)) {
      act += `<button class="act" data-a="pause" data-id="${j.id}" title="Pause" aria-label="Pause ${esc(j.filename)}">${ICON.pause}</button>`;
      act += `<button class="act danger" data-a="cancel" data-id="${j.id}" title="Cancel" aria-label="Cancel ${esc(j.filename)}">${ICON.cancel}</button>`;
    } else if (["paused", "queued", "failed", "cancelled"].includes(j.status)) {
      act += `<button class="act" data-a="resume" data-id="${j.id}" title="Resume" aria-label="Resume ${esc(j.filename)}">${ICON.resume}</button>`;
    }
    if (["completed", "failed", "cancelled"].includes(j.status)) {
      act += `<button class="act danger" data-a="delete" data-id="${j.id}" title="Delete job and file" aria-label="Delete ${esc(j.filename)} and its file">${ICON.cancel}</button>`;
    }
    const errLine = j.error ? `<div class="j-info err-t" title="${esc(j.error)}">${esc(j.error)}</div>` : "";
    return `<div class="job j-${esc(j.status)}">
      <div class="j-top">
        <span class="j-dot st-${esc(j.status)}" title="${st}" aria-hidden="true"></span>
        <span class="j-name" title="${esc(j.filename)}">${esc(j.filename)}</span>
        <span class="j-pct">${pct}%</span>
      </div>
      <div class="j-bar"><i style="width:${pct}%"></i></div>
      <div class="j-meta">
        <span class="j-info">${esc(parts.join(" · "))}</span>
        <span class="j-actions">${act}</span>
      </div>
      ${errLine}
    </div>`;
  }).join("");
}

// job actions: one delegated listener (the list re-renders constantly)
$("jobs").addEventListener("click", (ev) => {
  const b = ev.target.closest("button.act");
  if (!b) return;
  ev.stopPropagation();
  if (port) port.postMessage({ t: b.dataset.a, id: Number(b.dataset.id), files: true });
});

/* ---- connection ---- */

function setConn(state, note) {
  conn = { state, note: note ?? "" };
  renderConn();
}

function updDiag() {
  const online = conn.state === "online";
  $("d-host").textContent = online ? `Connected (${greetMs ?? "–"} ms)` : conn.state === "connecting" ? "testing…" : "Failed";
  $("d-host").className = online ? "d-ok" : conn.state === "offline" ? "d-err" : "";
  $("d-daemon").textContent = online ? "Responding" : conn.state === "connecting" ? "testing…" : "No reply";
  $("d-daemon").className = $("d-host").className;
  $("d-rtt").textContent = rttMs != null ? `${rttMs} ms` : "–";
  updTakeover();
  $("d-ver").textContent = "v" + chrome.runtime.getManifest().version;
}

function updTakeover() {
  chrome.storage.local.get({ intercept: true }, (s) => {
    $("d-take").textContent = s.intercept ? "On" : "Off";
    $("d-take").className = s.intercept ? "d-ok" : "";
    const sw = $("intercept");
    if (sw.checked !== !!s.intercept) sw.checked = !!s.intercept;
  });
}

function connect() {
  seenFrame = false;
  rttMs = null;
  setConn("connecting");
  renderJobs();
  const t0 = performance.now();
  try {
    port = chrome.runtime.connectNative(HOST);
  } catch (e) {
    setConn("offline", String(e && e.message ? e.message : e));
    updDiag();
    return;
  }
  let listAt = 0;
  port.onMessage.addListener((m) => {
    if (!m || !m.t) return;
    if (m.t === "settings") {
      // greeting proves the full chain: extension -> native host -> daemon
      greetMs = Math.round(performance.now() - t0);
      setConn("online");
      updDiag();
      return;
    }
    if (m.t === "jobs" && listAt) {
      rttMs = Math.round(performance.now() - listAt);
      listAt = 0;
      updDiag();
    }
    seenFrame = true;
    switch (m.t) {
      case "jobs":
        jobs.clear();
        for (const j of m.jobs || []) {
          speeds.set(j.id, { last: j.done ?? null, ts: performance.now(), ema: 0 });
          apply(j);
        }
        break;
      case "accepted": apply({ id: m.id, status: "running" }); break;
      case "started": apply({ id: m.id, size: m.size }); break;
      case "progress":
        noteProgress(m.id, m.done ?? 0);
        apply({ id: m.id, done: m.done, size: m.size, status: "running" });
        break;
      case "done": apply({ id: m.id, done: m.size, size: m.size, status: "completed" }); break;
      case "failed": apply({ id: m.id, status: "failed", error: m.error }); break;
      case "cancelled": apply({ id: m.id, status: "cancelled" }); break;
      case "paused": apply({ id: m.id, status: "paused" }); break;
      case "status": apply({ id: m.id, status: m.status }); break;
      case "deleted": jobs.delete(m.id); speeds.delete(m.id); render(); break;
    }
  });
  port.onDisconnect.addListener(() => {
    port = null;
    const why = chrome.runtime.lastError ? chrome.runtime.lastError.message : "";
    setConn("offline", why);
    updDiag();
  });
  listAt = performance.now();
  port.postMessage({ t: "list" });
}

/* ---- controls ---- */

const interceptEl = $("intercept");
chrome.storage.local.get({ intercept: true }, (s) => (interceptEl.checked = !!s.intercept));
interceptEl.onchange = () => {
  chrome.storage.local.set({ intercept: interceptEl.checked });
  updTakeover();
};

for (const b of $("tabs").querySelectorAll("button")) {
  b.onclick = () => {
    filter = b.dataset.f;
    for (const x of $("tabs").querySelectorAll("button")) x.setAttribute("aria-pressed", String(x === b));
    renderJobs();
  };
}

$("retry").onclick = connect;
$("retest").onclick = connect;
$("diag-toggle").onclick = () => {
  const d = $("diag");
  d.hidden = !d.hidden;
  $("diag-toggle").setAttribute("aria-expanded", String(!d.hidden));
  if (!d.hidden) updDiag();
};
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape" && !$("diag").hidden) $("diag-toggle").click();
});

// tick the ETA/speed readouts while jobs are running
setInterval(() => {
  for (const j of jobs.values()) if (j.status === "running") { renderJobs(); return; }
}, 1000);

connect();
