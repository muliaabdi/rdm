/* ---------- events from daemon ---------- */
function apply(v) {
  const j = jobs.get(v.id) || { id: v.id, filename: "?", done: 0, size: null, status: "running", speed: 0 };
  let rebuild = false;
  for (const k of ["filename", "out_path", "size", "done", "status", "error", "url", "attempt", "attempts", "dir", "queue", "priority", "speed_limit", "sha256", "verified"]) {
    if (v[k] != null && j[k] !== v[k]) { j[k] = v[k]; if (k === "status" || k === "queue" || k === "priority") rebuild = true; }
  }
  if (v.missing != null) j.missing = v.missing;
  if (v.segments) j.segs = v.segments;
  j.speed = j.status === "running" ? sampleSpeed(v.id, j.done) : (speeds.delete(v.id), 0);
  jobs.set(v.id, j);
  renderCats(); renderRows();
}
let connState = "wait";
function setConn(state) {
  connState = state;
  const el = $("s-conn");
  el.className = state;
  $("s-conn-t").textContent =
    t(state === "ok" ? "connected" : state === "bad" ? "disconnected" : "connecting");
}
listen("rdm", (ev) => {
  const m = ev.payload;
  switch (m.t) {
    case "disconnected": connected = false; setConn("bad"); break;
    case "jobs":
      if (!connected) { connected = true; diag("jobs n=" + (m.jobs || []).length); setConn("ok"); }
      const seen = new Set((m.jobs || []).map((x) => x.id));
      for (const id of [...jobs.keys()]) if (!seen.has(id)) { jobs.delete(id); speeds.delete(id); segPrev.delete(id); jobHist.delete(id); }
      for (const j of m.jobs || []) apply(j);
      // opened late (e.g. auto-launched by a browser offer): highlight the newest running job
      if (selected == null || !jobs.has(selected)) {
        const r = [...jobs.values()].filter((j) => j.status === "running").sort((a, b) => b.id - a.id)[0];
        if (r) { selected = r.id; renderRows(); }
      }
      break;
    case "accepted": apply({ id: m.id, status: "running", done: 0 }); selected = m.id; consumeQueueAssign(m.id); renderRows(); break;
    case "confirm":
      apply({ id: m.id, filename: m.filename, out_path: m.out_path, status: "confirming", done: 0 });
      selected = m.id;
      consumeQueueAssign(m.id);
      renderRows();
      openConfirm(m);
      break;
    case "show": focusWindow(); break;
    case "show-job":
      // background offer: the Rust side opens the compact detail popup; the
      // main window just tracks the selection quietly (never raise/focus)
      apply({ id: m.id });
      if (!detailWin) { selected = m.id; renderRows(); }
      break;
    case "tray-select": if (jobs.has(m.id)) { selected = m.id; renderRows(); } break;
    case "formats": if (window.__onFormats) window.__onFormats(m); break;
    case "error": if (window.__onFormats) { const f = window.__onFormats; window.__onFormats = null; f({ __err: m.error }); } else toast(trErr(m.error || "daemon error")); break;
    case "started": {
      const patch = { id: m.id, size: m.size };
      if (m.filename) { patch.filename = m.filename; patch.out_path = m.out_path; }
      apply(patch);
      consumeQueueAssign(m.id);
      if (m.filename) renderRows();
      break;
    }
    case "status": apply({ id: m.id, status: m.status, attempt: m.attempt, attempts: m.max }); renderRows(); break;
    case "job":
      apply({ id: m.id, queue: m.queue, priority: m.priority, speed_limit: m.speed_limit });
      break;
    case "progress": apply({ id: m.id, done: m.done, size: m.size, status: "running" }); break;
    case "detail":
      if (!detailSeen) { detailSeen = true; diag("detail segments received"); }
      apply({ id: m.id, segments: m.segments, status: "running" });
      break;
    case "done": apply({ id: m.id, done: m.size, size: m.size, status: "completed", sha256: m.sha256, verified: m.verified }); notifyDone(jobs.get(m.id)); break;
    case "failed": apply({ id: m.id, status: "failed", error: m.error }); notifyFail(jobs.get(m.id), m.error); break;
    case "paused": apply({ id: m.id, status: "paused" }); break;
    case "cancelled": apply({ id: m.id, status: "cancelled" }); break;
    case "deleted": jobs.delete(m.id); speeds.delete(m.id); segPrev.delete(m.id); jobHist.delete(m.id); if (selected === m.id) selected = null;
      renderCats(); renderRows(); break;
  }
  if (detailWin) renderDetailCard();
});

/* ---------- compact detail popup (background offers) ---------- */
function initDetailWindow() {
  if (document.getElementById("detail-root")) return;
  document.documentElement.dataset.mode = "detail";
  const root = document.createElement("div");
  root.id = "detail-root";
  root.innerHTML = `
    <div class="dc-head">
      <div class="dv-ico" id="dc-ico"></div>
      <div class="dc-titles">
        <div class="dc-name" id="dc-name">…</div>
        <div class="dc-sub"><span id="dc-cat"></span><span class="dot">·</span><span class="st" id="dc-st"></span></div>
      </div>
      <button class="dc-x" id="dc-x" data-i18n-title="btn_close">✕</button>
    </div>
    <div>
      <div class="dv-track"><div class="dv-fill" id="dc-fill"></div></div>
      <div class="dv-meta-line"><span id="dc-pct">0%</span><span id="dc-done"></span></div>
    </div>
    <div class="kvgrid c3">
      <div class="kv"><b id="dc-speed"></b><small data-i18n="col_speed">Speed</small></div>
      <div class="kv"><b id="dc-eta"></b><small data-i18n="col_eta">Time left</small></div>
      <div class="kv"><b id="dc-size"></b><small data-i18n="col_size">Size</small></div>
    </div>
    <canvas class="dc-graph" id="dc-graph"></canvas>
    <div class="dv-path">
      <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 7.5c0-.8.6-1.5 1.4-1.5H9l2 2.2h8.6c.8 0 1.4.7 1.4 1.5v8.8c0 .8-.6 1.5-1.4 1.5H4.4c-.8 0-1.4-.7-1.4-1.5z"/></svg>
      <span id="dc-file"></span>
      <button class="dv-btn-copy" id="dc-copy-path" data-i18n="btn_copy" data-i18n-title="btn_copy" title="Copy">Copy</button>
    </div>
    <div class="dc-err" id="dc-err"></div>
    <div class="dc-actions">
      <button id="dc-open" data-i18n="ctx_open_folder">Open folder</button>
      <button id="dc-toggle"></button>
      <button id="dc-close" class="pri" data-i18n="btn_close">Close</button>
    </div>`;
  document.body.appendChild(root);

  const closeWin = () => {
    try { window.__TAURI__.window.getCurrentWindow().close(); } catch {}
  };
  $("dc-x").onclick = closeWin;
  $("dc-close").onclick = closeWin;
  $("dc-copy-path").onclick = () => {
    const j = jobs.get(detailId);
    if (j && j.out_path) {
      navigator.clipboard.writeText(j.out_path).then(() => {
        const btn = $("dc-copy-path");
        if (btn) {
          btn.textContent = t("btn_copied");
          setTimeout(() => { btn.textContent = t("btn_copy"); }, 1500);
        }
      }).catch(() => {});
    }
  };
  $("dc-open").onclick = () => {
    const j = jobs.get(detailId);
    if (j) invoke("open_path", { path: j.out_path || "", folder: true }).catch((e) => toast(trErr(e)));
  };
  $("dc-toggle").onclick = () => {
    const j = jobs.get(detailId);
    if (!j) return;
    const action = j.status === "running" ? "pause" : "resume";
    invoke("job_action", { action, id: detailId }).catch((e) => toast(trErr(e)));
  };
  const cv = $("dc-graph");
  new ResizeObserver(() => paintGraph(cv, jobHist.get(detailId) || [])).observe(cv);

  listen("detail-open", (ev) => {
    detailId = ev.payload.id;
    apply({ id: detailId });
    renderDetailCard();
    invoke("list_jobs").catch(() => {});
  });
  // the window may be created after the offer arrived: pull the pending id
  invoke("detail_current").then((id) => {
    if (id) {
      detailId = id;
      apply({ id: detailId });
      renderDetailCard();
    }
    invoke("list_jobs").catch(() => {});
  }).catch(() => {});
}

function renderDetailCard() {
  if (detailId == null || !$("dc-name")) return;
  const j = jobs.get(detailId);
  if (!j) return;
  $("dc-name").textContent = j.filename || "…";
  $("dc-name").title = j.filename || "";
  $("dc-ico").textContent = icoOf(j).ch;
  $("dc-cat").textContent = t(CATS.find(([id]) => id === catOf(j))[1]);
  const st = $("dc-st");
  st.className = "st " + j.status;
  st.textContent = statusLabel(j);
  const pct = pctOf(j);
  const sz = estSize(j);
  $("dc-fill").style.width = pct + "%";
  $("dc-fill").style.background = fillColor(j);
  if (j.status === "running") $("dc-fill").classList.add("pulse");
  else $("dc-fill").classList.remove("pulse");
  $("dc-pct").textContent = pct.toFixed(0) + "%";
  $("dc-done").textContent = fmt(j.done) + (sz ? " / " + (j.size ? fmt(j.size) : "~" + fmt(sz)) : "");
  $("dc-size").textContent = sz ? (j.size ? fmt(j.size) : "~" + fmt(sz)) : "?";
  $("dc-speed").textContent = j.status === "running" ? fmtSpeed(curSpeed(j)) : "";
  $("dc-eta").textContent = j.status === "running" ? fmtEta(etaOf(j)) : "";
  $("dc-file").textContent = j.out_path || "-";
  $("dc-file").title = j.out_path || "";
  const err = $("dc-err");
  if (j.error && (j.status === "failed" || j.status === "cancelled")) {
    err.textContent = j.error;
    err.title = j.error;
    err.className = "dc-err on";
  } else { err.className = "dc-err"; err.textContent = ""; err.removeAttribute("title"); }
  const tog = $("dc-toggle");
  const idle = j.status === "completed" || j.status === "resolving";
  tog.hidden = idle;
  if (!idle) tog.textContent = j.status === "running" ? t("stop") : t("resume");
  paintGraph($("dc-graph"), jobHist.get(detailId) || []);
}

setInterval(() => {
  const now = performance.now();
  for (const j of jobs.values()) {
    if (j.status === "running") {
      j.speed = sampleSpeed(j.id, j.done);
      let h = jobHist.get(j.id);
      if (!h) { h = []; jobHist.set(j.id, h); }
      // per-segment speed from consecutive detail snapshots (~400 ms cadence),
      // EMA-smoothed so the stacked bands do not strobe; the smoothed sum is
      // also what the card + graph label show, so both read the same number
      let seg = null;
      if (j.size && j.segs && j.segs.length && j.segs.length <= 32) {
        const cur = j.segs.map((s) => s.done);
        const p = segPrev.get(j.id);
        const dt = p ? (now - p.t) / 1000 : 0;
        if (p && dt >= 0.2) {
          seg = cur.map((d, i) => {
            const inst = Math.max(0, (d - (p.arr[i] ?? 0)) / dt);
            const prev = p.ema ? (p.ema[i] ?? null) : null;
            return prev == null ? inst : prev * 0.45 + inst * 0.55;
          });
        }
        segPrev.set(j.id, { t: now, arr: cur, ema: seg });
      }
      h.push({ v: seg ? seg.reduce((a, b) => a + b, 0) : (j.speed || 0), seg });
      if (h.length > 120) h.shift();
    } else if (segPrev.has(j.id)) {
      segPrev.delete(j.id);
    }
  }
  renderRows();
}, 500);

