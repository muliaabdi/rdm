/* ---------- state ---------- */
const jobs = new Map();          // id -> job {id,url,filename,out_path,size,done,status,error,segs,speed}
const speeds = new Map();        // id -> [{t,done}]
const jobHist = new Map();       // id -> [{v, seg}] speed samples (running jobs), seg = per-segment B/s
const segPrev = new Map();       // id -> {t, arr:[done per segment index]} previous detail snapshot
let selected = null;
let cat = "all";
let connected = false;
let detailSeen = false;
let detailId = null;    // job shown in the compact detail window

const CATS = [
  ["all", "cat_all"],
  ["compressed", "cat_compressed"],
  ["documents", "cat_documents"],
  ["music", "cat_music"],
  ["video", "cat_video"],
  ["programs", "cat_programs"],
  ["other", "cat_other"],
];
const EXT = {
  compressed: ["zip","rar","7z","tar","gz","bz2","xz","iso"],
  documents: ["pdf","doc","docx","xls","xlsx","ppt","pptx","txt","md","epub"],
  music: ["mp3","flac","wav","ogg","m4a","aac","opus"],
  video: ["mp4","mkv","avi","webm","mov","flv","m4v","ts","m4s"],
  programs: ["exe","msi","deb","rpm","appimage","apk","bin","dmg"],
};
function catOf(j) {
  const m = /\.([a-z0-9]+)$/i.exec(j.filename || "");
  const e = m ? m[1].toLowerCase() : "";
  for (const [k, list] of Object.entries(EXT)) if (list.includes(e)) return k;
  return "other";
}

/* ---------- helpers ---------- */
function fmt(n) {
  if (n == null) return "?";
  const u = ["B", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < u.length - 1) { n /= 1024; i++; }
  return (i === 0 ? n : n.toFixed(n >= 100 ? 0 : 1)) + " " + u[i];
}
function fmtSpeed(bps) {
  if (!bps || bps < 1) return "0 B/s";
  return fmt(bps) + "/s";
}
function fmtEta(sec) {
  if (sec == null || !isFinite(sec) || sec < 0) return "∞";
  if (sec < 60) return Math.ceil(sec) + t("u_s");
  if (sec < 3600) return Math.floor(sec / 60) + t("u_m") + " " + Math.ceil(sec % 60) + t("u_s");
  return Math.floor(sec / 3600) + t("u_h") + " " + Math.ceil((sec % 3600) / 60) + t("u_m");
}
function esc(s) {
  return String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" }[c]));
}
function segGradient(segs, size) {
  // Alternating green stripes per completed segment; theme-aware pair
  if (!segs || !size) return "var(--fill-ok)";
  const dark = document.documentElement.dataset.theme === "dark";
  const full = dark ? "#5cbd6e" : "#2f9e44", alt = dark ? "#4fae60" : "#268a3c";
  const parts = [];
  segs.forEach((s, i) => {
    const a = (s.start / size) * 100, b = (s.done / Math.max(1, s.end - s.start + 1)) * 100;
    const c = i % 2 ? alt : full;
    if (b > 0) parts.push(`${c} ${a.toFixed(2)}%`, `${c} ${(a + ((b / 100) * ((s.end - s.start + 1) / size) * 100)).toFixed(2)}%`);
  });
  if (!parts.length) parts.push("transparent 0%", "transparent 0%");
  return `linear-gradient(90deg,${parts.join(",")})`;
}
function icoFor(j) {
  const m = /\.([a-z0-9]+)$/i.exec(j.filename || "");
  return `<span class="fico">${esc((m ? m[1] : "?").slice(0, 4))}</span>`;
}

/* ---------- speed sampling ---------- */
function sampleSpeed(id, done) {
  const now = performance.now();
  let h = speeds.get(id);
  if (!h) { h = []; speeds.set(id, h); }
  h.push({ t: now, done });
  while (h.length > 2 && now - h[0].t > 6000) h.shift();
  if (h.length < 2) return 0;
  const a = h[0], b = h[h.length - 1];
  const dt = (b.t - a.t) / 1000;
  if (dt < 0.3) return 0;
  return Math.max(0, (b.done - a.done) / dt);
}

