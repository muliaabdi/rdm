// RDM popup: live job list via dedicated native host port.

const jobs = new Map(); // id -> {filename,size,done,status,error}
const el = () => document.getElementById("jobs");
let port = null;

function fmt(n) {
  if (n == null) return "?";
  if (n >= 1 << 30) return (n / (1 << 30)).toFixed(1) + " GB";
  if (n >= 1 << 20) return (n / (1 << 20)).toFixed(1) + " MB";
  if (n >= 1 << 10) return (n / (1 << 10)).toFixed(0) + " KB";
  return n + " B";
}

function render() {
  const box = el();
  if (!jobs.size) {
    box.innerHTML = '<div class="empty">tidak ada job</div>';
    return;
  }
  const rows = [...jobs.values()]
    .sort((a, b) => b.id - a.id)
    .map((j) => {
      const pct = j.size ? Math.min(100, Math.round((j.done / j.size) * 100)) : 0;
      const cls = j.status === "completed" ? "done" : j.status === "failed" ? "failed" : "";
      const label =
        j.status === "completed" ? "selesai" :
        j.status === "failed" ? "gagal" :
        j.status === "cancelled" ? "dibatalkan" :
        j.status === "running" ? `${fmt(j.done)} / ${fmt(j.size)} · ${pct}%` : j.status;
      return `<div class="job ${cls}">
        <div class="row"><span class="name" title="${esc(j.filename)}">${esc(j.filename)}</span><span class="pct">${pct}%</span></div>
        <div class="bar"><i style="width:${pct}%"></i></div>
        <div class="st">${label}${j.error ? ` · <span class="err">${esc(j.error)}</span>` : ""}</div>
      </div>`;
    })
    .join("");
  box.innerHTML = rows;
}

function esc(s) {
  return String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
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

function connect() {
  port = chrome.runtime.connectNative("com.rdm.host");
  port.onMessage.addListener((m) => {
    switch (m.t) {
      case "jobs":
        jobs.clear();
        for (const j of m.jobs || []) apply(j);
        break;
      case "accepted": apply({ id: m.id, status: "running" }); break;
      case "started": apply({ id: m.id, size: m.size }); break;
      case "progress": apply({ id: m.id, done: m.done, size: m.size, status: "running" }); break;
      case "done": apply({ id: m.id, done: m.size, size: m.size, status: "completed" }); break;
      case "failed": apply({ id: m.id, status: "failed", error: m.error }); break;
      case "cancelled": apply({ id: m.id, status: "cancelled" }); break;
    }
  });
  port.onDisconnect.addListener(() => {
    if (chrome.runtime.lastError) console.warn("rdm popup:", chrome.runtime.lastError.message);
  });
  port.postMessage({ t: "list" });
}

const interceptEl = document.getElementById("intercept");
chrome.storage.local.get({ intercept: true }, (s) => (interceptEl.checked = !!s.intercept));
interceptEl.onchange = () => chrome.storage.local.set({ intercept: interceptEl.checked });

document.getElementById("refresh").onclick = () => {
  if (port) port.postMessage({ t: "list" });
};

connect();
