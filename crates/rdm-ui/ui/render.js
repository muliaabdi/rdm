/* ---------- render ---------- */
let queueSel = "all"; // "all" or a queue label
let queueAssignNext = null; // queue view open when Add URL clicked: tag the next job
function visible() {
  const rows = [...jobs.values()]
    .filter((j) => cat === "all" || catOf(j) === cat)
    .filter((j) => queueSel === "all" || (j.queue || "main") === queueSel);
  // inside a queue the list mirrors the scheduler: priority first, FIFO after
  return queueSel === "all" ? rows.sort((a, b) => b.id - a.id)
    : rows.sort((a, b) => (b.priority || 0) - (a.priority || 0) || a.id - b.id);
}
function queueCounts() {
  const qs = new Map();
  for (const j of jobs.values()) {
    const q = j.queue || "main";
    qs.set(q, (qs.get(q) || 0) + 1);
  }
  return qs;
}
function qRun(q) {
  return [...jobs.values()].filter((j) => j.status === "running" && (q === "all" || (j.queue || "main") === q)).length;
}
function renderQueues() {
  const qs = queueCounts();
  const box = $("qlist");
  const custom = [...qs.entries()]
    .filter(([q]) => q !== "main")
    .sort((a, b) => a[0].localeCompare(b[0]))
    .map(([q, cnt]) => [q, q, cnt]);
  if (queueSel !== "all" && !qs.has(queueSel)) queueSel = "all";
  const items = [["all", t("queue_all"), jobs.size], ...(qs.has("main") ? [["main", t("queue_main"), qs.get("main")]] : []), ...custom];
  $("q-sec").hidden = custom.length === 0 && !qs.has("main");
  box.innerHTML = items.map(([id, label, cnt]) => {
    const run = id !== "all" && qRun(id) > 0;
    return `<div class="cat ${queueSel === id ? "sel" : ""}" data-q="${esc(id)}" role="button" tabindex="0"
      title="${esc(t("items_fmt", { n: cnt, m: qRun(id) }))}">
      ${esc(label)}${run ? '<span class="run-dot"></span>' : ""}
      <span class="cnt">${cnt}</span></div>`;
  }).join("");
  box.querySelectorAll(".cat").forEach((el) => {
    const pick = () => { queueSel = el.dataset.q; renderQueues(); renderRows(); };
    el.onclick = pick;
    el.onkeydown = (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); pick(); } };
    el.oncontextmenu = (e) => {
      const q = el.dataset.q;
      if (q === "all") return;
      e.preventDefault();
      openQueueMenu(q, e.clientX, e.clientY);
    };
  });
}
function renderCats() {
  const counts = { all: jobs.size };
  for (const j of jobs.values()) { const k = catOf(j); counts[k] = (counts[k] || 0) + 1; }
  $("catlist").innerHTML = CATS.map(([id, label]) =>
    `<div class="cat ${cat === id ? "sel" : ""}" data-cat="${id}" role="button" tabindex="0">
      ${t(label)}<span class="cnt">${counts[id] || 0}</span></div>`).join("");
  document.querySelectorAll("#catlist .cat").forEach((el) => {
    const pick = () => { cat = el.dataset.cat; renderCats(); renderRows(); };
    el.onclick = pick;
    el.onkeydown = (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); pick(); } };
  });
  renderQueues();
}
function estSize(j) {
  if (j.size) return j.size;
  if (j.segs && j.segs.length && j.done > 0) {
    const done = j.segs.filter((s) => s.done > 0).length;
    if (done > 0) return Math.round((j.done / done) * j.segs.length);
  }
  return null;
}
function etaOf(j) {
  if (j.status !== "running" || !j.speed) return null;
  const sz = estSize(j);
  if (!sz || sz <= j.done) return null;
  return (sz - j.done) / j.speed;
}
function selectRow(id) {
  selected = id;
  document.querySelectorAll(".row").forEach((el) => {
    const on = +el.dataset.id === id;
    el.classList.toggle("sel", on);
    el.setAttribute("aria-selected", on);
  });
  updateToolbar();
}
function rowHtml(j, idx) {
  const pct = pctOf(j);
  const sz = estSize(j);
  const fillColor = j.status === "failed" ? "var(--fill-red)"
    : j.status === "paused" ? "var(--fill-amber)"
    : j.status === "confirming" ? "transparent"
    : "var(--fill-ok)";
  const w = j.status === "completed" ? 100 : pct;
  const tip = esc(j.filename + (j.error ? `: ${j.error}` : ""));
  // queue label + priority markers: subtle chips next to the name
  const qchip = j.queue && j.queue !== "main" && queueSel === "all"
    ? `<span class="qtag" title="${esc(t("dlg_queue_title") + ": " + j.queue)}">${esc(j.queue)}</span>` : "";
  const pnum = typeof j.priority === "number" ? j.priority : 0;
  const pchip = pnum > 0
    ? `<span class="ptag up" title="${esc(t("ctx_priority") + ": " + t("pri_high"))}">▲</span>`
    : pnum < 0
      ? `<span class="ptag dn" title="${esc(t("ctx_priority") + ": " + t("pri_low"))}">▼</span>` : "";
  const limChip = j.speed_limit > 0
    ? `<span class="ptag lm" title="${esc(t("dlg_limit_title") + ": " + j.speed_limit + " KiB/s")}">◔</span>` : "";
  const okChip = j.status === "completed" && j.verified
    ? `<span class="ptag ok" title="${esc(t("ver_ok"))}">✓</span>` : "";
  const rank = queueSel !== "all" ? `<span class="rank">${idx + 1}</span>` : "";
  return `<div class="row ${selected === j.id ? "sel" : ""}" role="option" aria-selected="${selected === j.id}" tabindex="0" data-id="${j.id}" draggable="true">
    <div class="name" title="${tip}">${rank}${icoFor(j)}<span class="nm">${esc(j.filename)}</span>${qchip}${pchip}${limChip}${okChip}</div>
    <div class="num dim sz">${sz ? (j.size ? fmt(j.size) : "~" + fmt(sz)) : "?"}</div>
    <div class="pcol"><div class="cellbar ${j.status === "running" ? "pulse" : ""}"><div class="fill" style="width:${w}%;background:${fillColor}"></div></div><span class="pct">${w.toFixed(0)}%</span></div>
    <div class="st ${j.status}${j.missing ? " missing" : ""}">${statusLabel(j)}</div>
    <div class="num dim eta">${j.status === "running" ? fmtEta(etaOf(j)) : ""}</div>
    <div class="num spd">${j.status === "running" ? fmtSpeed(j.speed) : ""}</div>
  </div>`;
}
function renderRows() {
  const rows = visible();
  updateQhead();
  const box = $("rows");
  if (!rows.length) {
    box.innerHTML = `<div class="empty">
      <div class="plate"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3v10m-4-4 4 4 4-4"/><path d="M4 17v1.5C4 19.3 4.7 20 5.5 20h13c.8 0 1.5-.7 1.5-1.5V17"/></svg></div>
      <div class="ttl">${t(cat === "all" ? "empty_title" : "empty_category")}</div>
      <div class="empty-hint">${t("empty_hint")}</div>
      <button id="empty-add">${t("add_url")}</button></div>`;
    const ea = $("empty-add");
    if (ea) ea.onclick = () => $("b-add").click();
    box.dataset.ids = "";
    updateToolbar(); updateDetails(); renderStatus();
    return;
  }
  const ids = rows.map((j) => j.id).join(",");
  // full rebuild only when the row set / language / category changes; ticks
  // update fields in place so the progress bars animate instead of jumping
  if (box.dataset.ids === ids && box.dataset.lang === lang && box.dataset.cat === cat && box.dataset.queue === queueSel) {
    for (const j of rows) {
      const el = box.querySelector(`.row[data-id="${j.id}"]`);
      if (!el) continue;
      const on = selected === j.id;
      el.classList.toggle("sel", on);
      el.setAttribute("aria-selected", on);
      const pct = j.size ? Math.min(100, (j.done / j.size) * 100) : 0;
      const w = j.status === "completed" ? 100 : pct;
      el.querySelector(".fill").style.width = w + "%";
      el.querySelector(".pct").textContent = w.toFixed(0) + "%";
      const st = el.querySelector(".st");
      st.className = "st " + j.status + (j.missing ? " missing" : "");
      st.textContent = statusLabel(j);
      const szt = j.size ? fmt(j.size) : "?";
      const sz = el.querySelector(".sz");
      if (sz.textContent !== szt) sz.textContent = szt;
      el.querySelector(".eta").textContent = j.status === "running" ? fmtEta(etaOf(j)) : "";
      el.querySelector(".spd").textContent = j.status === "running" ? fmtSpeed(j.speed) : "";
      const tip = j.filename + (j.error ? `: ${j.error}` : "");
      const nm = el.querySelector(".nm");
      if (nm.textContent !== j.filename) nm.textContent = j.filename;
      const nameEl = el.querySelector(".name");
      if (nameEl.title !== tip) nameEl.title = tip;
    }
    updateToolbar(); updateDetails(); renderStatus();
    return;
  }
  box.dataset.ids = ids; box.dataset.lang = lang; box.dataset.cat = cat; box.dataset.queue = queueSel;
  box.innerHTML = rows.map(rowHtml).join("");
  const ae = document.activeElement;
  const activeId = ae && ae.classList && ae.classList.contains("row") ? +ae.dataset.id : null;
  document.querySelectorAll(".row").forEach((el) => {
    el.onclick = () => selectRow(+el.dataset.id);
    el.onfocus = () => selectRow(+el.dataset.id);
    el.ondblclick = () => openDetails(+el.dataset.id);
    el.oncontextmenu = (ev) => { ev.preventDefault(); selectRow(+el.dataset.id); openCtx(+el.dataset.id, ev.clientX, ev.clientY); };
  });
  if (activeId != null) {
    const el = box.querySelector(`.row[data-id="${activeId}"]`);
    if (el) el.focus({ preventScroll: true });
  }
  updateToolbar(); updateDetails(); renderStatus();
}
$("rows").addEventListener("click", (e) => { if (e.target === $("rows")) selectRow(null); });
$("rows").addEventListener("keydown", (e) => {
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  const items = [...document.querySelectorAll(".row")];
  const i = items.indexOf(document.activeElement);
  const n = items[e.key === "ArrowDown" ? i + 1 : i - 1];
  if (n) { e.preventDefault(); n.focus(); }
});

/* ---------- queue header + drag a job onto a queue ---------- */
function updateQhead() {
  const el = $("qhead");
  if (!el) return;
  if (queueSel === "all") { el.hidden = true; el.dataset.sig = ""; return; }
  const rows = [...jobs.values()].filter((j) => (j.queue || "main") === queueSel);
  const act = rows.filter((j) => j.status === "running");
  const resumable = rows.some((j) => ["paused", "failed", "cancelled", "queued"].includes(j.status));
  const tot = act.reduce((a, j) => a + (j.speed || 0), 0);
  // rebuild only when something visible changed, so hover state survives ticks
  const sig = [queueSel, lang, rows.length, act.length, resumable ? 1 : 0, Math.round(tot / 1024)].join("|");
  if (el.dataset.sig === sig) return;
  el.dataset.sig = sig;
  el.hidden = false;
  el.innerHTML = `
    <span class="qname">${esc(queueSel === "main" ? t("queue_main") : queueSel)}</span>
    <span class="qmeta">${t("items_fmt", { n: rows.length, m: act.length })}</span>
    <span class="qmeta">${fmtSpeed(tot)}</span>
    <span class="qgrow"></span>
    <button type="button" class="qbtn go" id="qh-resume"${resumable ? "" : " disabled"}>
      <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M7.5 4.8c0-.9 1-1.5 1.8-1L19 10.2c.8.5.8 1.6 0 2.1L9.3 18.9c-.8.5-1.8-.1-1.8-1z"/></svg>
      ${t("q_resume_all")}</button>
    <button type="button" class="qbtn" id="qh-pause"${act.length ? "" : " disabled"}>
      <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="6" y="5" width="4" height="14" rx="1"/><rect x="14" y="5" width="4" height="14" rx="1"/></svg>
      ${t("q_pause_all")}</button>`;
  $("qh-resume").onclick = () => rows.forEach((j) => {
    if (["paused", "failed", "cancelled", "queued"].includes(j.status))
      invoke("job_action", { action: "resume", id: j.id }).catch((e) => toast(trErr(e)));
  });
  $("qh-pause").onclick = () => act.forEach((j) =>
    invoke("job_action", { action: "pause", id: j.id }).catch((e) => toast(trErr(e))));
}
$("rows").addEventListener("dragstart", (e) => {
  const r = e.target.closest(".row");
  if (!r) return;
  e.dataTransfer.setData("text/rdm-job", r.dataset.id);
  e.dataTransfer.effectAllowed = "move";
});
$("qlist").addEventListener("dragover", (e) => {
  const q = e.target.closest(".cat[data-q]");
  if (!q || q.dataset.q === "all") return;
  e.preventDefault();
  e.dataTransfer.dropEffect = "move";
  q.classList.add("drop");
});
$("qlist").addEventListener("dragleave", (e) => {
  const q = e.target.closest(".cat[data-q]");
  if (q) q.classList.remove("drop");
});
$("qlist").addEventListener("drop", (e) => {
  const q = e.target.closest(".cat[data-q]");
  document.querySelectorAll("#qlist .cat.drop").forEach((el) => el.classList.remove("drop"));
  if (!q || q.dataset.q === "all") return;
  e.preventDefault();
  const id = +(e.dataTransfer.getData("text/rdm-job") || e.dataTransfer.getData("text/plain"));
  if (jobs.has(id)) { jobSet(id, { queue: q.dataset.q }); toast(t("q_moved", { q: q.dataset.q })); }
});
// no webview context menu anywhere; keep the native one inside text fields
// so right-click paste still works in the URL input
document.addEventListener("contextmenu", (e) => {
  if (e.target.closest("input, select, textarea")) return;
  e.preventDefault();
});

/* ---------- row context menu ---------- */
const CTX_ICONS = {
  details: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" aria-hidden="true"><path d="M5 20v-7m5 7V6m5 14v-10m5 10V9"/></svg>',
  stop: '<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><rect x="6" y="6" width="12" height="12" rx="2"/></svg>',
  resume: '<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M7.5 4.8c0-.9 1-1.5 1.8-1L19 10.2c.8.5.8 1.6 0 2.1L9.3 18.9c-.8.5-1.8-.1-1.8-1z"/></svg>',
  delete: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4 7h16M9.5 7V4.8c0-.4.4-.8.8-.8h3.4c.4 0 .8.4.8.8V7m-9.7 0l.9 12.3c.1.9.8 1.7 1.8 1.7h7c1 0 1.7-.8 1.8-1.7L17.2 7M10 11v6m4-6v6"/></svg>',
  openfile: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M14 3H6.8C6 3 5.3 3.7 5.3 4.5v15c0 .8.7 1.5 1.5 1.5h10.4c.8 0 1.5-.7 1.5-1.5V7.5z"/><path d="M14 3v4.5h4.7M9.4 12.4h5.2M9.4 16h5.2"/></svg>',
  openfolder: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3.5 6.5c0-.8.7-1.5 1.5-1.5h4l2 2.5h8c.8 0 1.5.7 1.5 1.5v9c0 .8-.7 1.5-1.5 1.5H5c-.8 0-1.5-.7-1.5-1.5z"/></svg>',
  gauge: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 15l4-6"/><path d="M5.5 19a8.5 8.5 0 1 1 13 0"/></svg>',
  flag: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 21V4"/><path d="M5 4h11l-2 4 2 4H5"/></svg>',
  layers: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 3l9 5-9 5-9-5z"/><path d="M3 13l9 5 9-5"/></svg>',
};
let ctxId = null;
function closeCtx() {
  $("ctx").classList.remove("open");
  ctxId = null;
}
function ctxAction(fn) {
  const id = ctxId;
  closeCtx();
  if (id != null) fn(id);
}
function openCtx(id, x, y) {
  const j = jobs.get(id);
  if (!j) return;
  ctxId = id;
  const running = j.status === "running";
  const canResume = ["paused", "failed", "cancelled", "queued"].includes(j.status);
  const items = [
    { ic: "details", label: t("details"), en: true,
      fn: (i) => openDetails(i) },
    { ic: "openfile", label: t("ctx_open_file"), en: j.status === "completed",
      fn: (i) => invoke("open_path", { path: jobs.get(i)?.out_path || "", folder: false }).catch((e) => toast(trErr(e))) },
    { ic: "openfolder", label: t("ctx_open_folder"), en: !!(j.out_path && j.status !== "confirming"),
      fn: (i) => invoke("open_path", { path: jobs.get(i)?.out_path || "", folder: true }).catch((e) => toast(trErr(e))) },
    running || canResume
      ? { ic: running ? "stop" : "resume", label: running ? t("stop") : t("resume"), en: true,
          fn: (i) => invoke("job_action", { action: running ? "pause" : "resume", id: i }).catch((e) => toast(trErr(e))) }
      : { ic: "resume", label: t("resume"), en: false, fn: () => {} },
    "sep",
    { ic: "gauge", label: t("ctx_limit"), en: j.status !== "confirming",
      fn: (i) => openLimitDlg(i) },
    { ic: "flag", label: t("ctx_priority"), en: j.status !== "confirming",
      fn: (i) => openPrioDlg(i) },
    { ic: "layers", label: t("ctx_queue"), en: j.status !== "confirming",
      fn: (i) => openQueueDlg(i) },
    "sep",
    { ic: "delete", label: t("delete"), en: true,
      fn: (i) => confirmDelete(i) },
  ];
  const html = items.map((it) => it === "sep"
    ? '<div class="sep"></div>'
    : `<button type="button" role="menuitem" ${it.en ? "" : "disabled"}>${CTX_ICONS[it.ic]}<span>${esc(it.label)}</span></button>`
  ).join("");
  const menu = $("ctx");
  menu.innerHTML = html;
  const btns = [...menu.querySelectorAll("button")];
  btns.forEach((b, k) => {
    const it = items.filter((x) => x !== "sep")[k];
    b.onclick = () => ctxAction(it.fn);
  });
  menu.classList.add("open");
  const r = menu.getBoundingClientRect();
  menu.style.left = Math.max(4, Math.min(x, window.innerWidth - r.width - 6)) + "px";
  menu.style.top = Math.max(4, Math.min(y, window.innerHeight - r.height - 6)) + "px";
  const first = btns.find((b) => !b.disabled);
  if (first) first.focus();
}
document.addEventListener("click", () => closeCtx());
$("rows").addEventListener("scroll", () => closeCtx(), true);
window.addEventListener("resize", closeCtx);
window.addEventListener("blur", () => closeCtx());
function updateToolbar() {
  const j = jobs.get(selected);
  $("b-resume").toggleAttribute("disabled", !(j && ["paused", "failed", "cancelled", "confirming", "queued"].includes(j.status)));
  $("b-stop").toggleAttribute("disabled", !(j && j.status === "running"));
  $("b-restart").toggleAttribute("disabled", !(j && j.status !== "running"));
  const prioOk = j && !["completed", "confirming"].includes(j.status);
  $("b-prio-up").toggleAttribute("disabled", !prioOk);
  $("b-prio-dn").toggleAttribute("disabled", !prioOk);
  $("b-queue").toggleAttribute("disabled", !(j && j.status !== "confirming"));
  $("b-delete").toggleAttribute("disabled", !j);
}
function statusLabel(j) {
  const pct = j.size ? Math.min(100, (j.done / j.size) * 100) : 0;
  return j.missing ? t("st_missing") :
    j.status === "retrying" ? t("st_retrying", { n: j.attempt || 1, m: j.attempts || 3 }) :
    j.status === "completed" ? t("st_completed") :
    j.status === "failed" ? t("st_failed") :
    j.status === "paused" ? t("st_paused") :
    j.status === "cancelled" ? t("st_cancelled") :
    j.status === "queued" ? t("st_queued") :
    j.status === "confirming" ? t("st_confirming") :
    j.status === "resolving" ? t("st_resolving") :
    j.status === "running" ? t("st_running") : j.status;
}

/* ---------- takeover confirmation (intercepted browser downloads) ---------- */
/* ---------- delete confirmation (keeps or removes partial files) ---------- */
function notifyDone(j) {
  if (!j) return;
  invoke("notify", { title: t("st_completed"), body: j.filename || "#" + j.id }).catch(() => {});
}
function notifyFail(j, err) {
  if (!j) return;
  invoke("notify", { title: t("st_failed"), body: (j.filename || "#" + j.id) + (err ? ": " + err : "") }).catch(() => {});
}
async function confirmDelete(id) {
  const j = jobs.get(id);
  const partial = j && j.status !== "completed" && j.status !== "confirming";
  // mirror the daemon's guard: nothing is removed for finished jobs, and a
  // folder is only removed when it is strictly below $HOME and not the
  // download base folder (page jobs store the base folder as out_path)
  let base = "";
  try { base = ((await invoke("get_settings")) || {}).dir || ""; } catch {}
  const out = (j && j.out_path) || "";
  const deletable = partial && out &&
    !(j.dir && (!base || out === base || base.startsWith(out)));
  let what = "";
  if (deletable) {
    what = `<div class="mhint" style="margin-top:2px">${t(j.dir ? "del_will_dir" : "del_will_file")}</div>
      <div class="delpath${j.dir ? " dir" : ""}">${esc(out)}</div>`;
  } else if (!partial && out) {
    what = `<div class="mhint" style="margin-top:2px">${t("del_kept")}</div>
      <div class="delpath">${esc(out)}</div>`;
  } else {
    what = `<div class="mhint" style="margin-top:2px">${t("del_none")}</div>`;
  }
  openModal(`<h3>${t("del_title")}</h3>
    <div class="dt-head"><div class="dt-name" title="${esc(j?.filename || "")}">${esc(j?.filename || "?")}</div></div>
    <div class="mhint">${t("del_hint")}</div>
    ${what}
    ${deletable ? `<div class="fld"><label>&nbsp;</label><label class="fchk"><input type="checkbox" id="dl-files" checked><span>${t("del_files")}</span></label></div>` : ""}
    <div class="btns">
      <button id="dl-cancel">${t("btn_cancel")}</button>
      <button id="dl-ok" class="pri">${t("btn_delete")}</button>
    </div>`);
  $("dl-cancel").onclick = closeModal;
  $("dl-ok").onclick = () => {
    const files = deletable && !!$("dl-files")?.checked;
    invoke("job_action", { action: "delete", id, files }).catch((e) => toast(trErr(e)));
    closeModal();
  };
}

function openConfirm(m) {
  openModal(`<h3>${t("confirm_title")}</h3>
    <div class="dt-head"><div class="dt-name" title="${esc(m.filename || "")}">${esc(m.filename || "?")}</div></div>
    <div class="dgrid">
      <div class="k">${t("col_url")}</div><div class="v" style="grid-column: 2 / -1" title="${esc(m.url)}">${esc(m.url)}</div>
      <div class="k">${t("col_file")}</div><div class="v" style="grid-column: 2 / -1" title="${esc(m.out_path || "")}">${esc(m.out_path || "-")}</div>
    </div>
    <div class="mhint">${t("confirm_hint")}</div>
    <div class="btns">
      <button id="cf-cancel">${t("btn_cancel")}</button>
      <button id="cf-ok" class="pri">${t("btn_download")}</button>
    </div>`);
  $("cf-cancel").onclick = () => {
    invoke("job_action", { action: "delete", id: m.id }).catch((e) => toast(trErr(e)));
    closeModal();
  };
  $("cf-ok").onclick = () => {
    invoke("job_action", { action: "resume", id: m.id }).catch((e) => toast(trErr(e)));
    closeModal();
  };
}

/* ---------- details dialog (in-app, double-click) ---------- */
let detailsFor = null;
function detailSeries() {
  return jobHist.get(detailsFor) || [];
}
/// File badge: short extension label.
function icoOf(j) {
  const m = /\.([a-z0-9]+)$/i.exec(j.filename || "");
  return { ch: (m ? m[1] : catOf(j)).slice(0, 4).toUpperCase() };
}
function pctOf(j) {
  if (j.size) return Math.min(100, (j.done / j.size) * 100);
  if (j.segs && j.segs.length) {
    const done = j.segs.filter((s) => s.done > 0).length;
    return Math.min(100, (done / j.segs.length) * 100);
  }
  return 0;
}
function fillColor(j) {
  return j.segs && j.status === "running" ? segGradient(j.segs, j.size)
    : j.status === "failed" ? "var(--fill-red)"
    : j.status === "paused" ? "var(--fill-amber)"
    : "var(--fill-ok)";
}
function openDetails(id) {
  const j = jobs.get(id);
  if (!j) return;
  detailsFor = id;
  selected = id;
  openModal(`<h3>${t("details_title")}</h3>
    <div class="dv">
      <div class="dv-head">
        <div class="dv-ico" id="dt-ico"></div>
        <div class="dv-titles">
          <div class="dv-name" id="dt-name"></div>
          <div class="dv-sub">
            <span id="dt-cat"></span><span class="dot">·</span>
            <span class="st" id="dt-st"></span>
          </div>
        </div>
      </div>
      <div>
        <div class="dv-track"><div class="dv-fill" id="dt-fill"></div></div>
        <div class="dv-meta-line"><span id="dt-done"></span><span class="live" id="dt-live"></span></div>
      </div>
      <div class="kvgrid">
        <div class="kv"><b id="dt-size"></b><small>${t("col_size")}</small></div>
        <div class="kv"><b id="dt-queue"></b><small>${t("dlg_queue_title")}</small></div>
        <div class="kv"><b id="dt-prio"></b><small>${t("ctx_priority")}</small></div>
        <div class="kv"><b id="dt-lim"></b><small>${t("dlg_limit_title")}</small></div>
      </div>
      <div class="dt-gtitle" style="margin:0">${t("speed_graph")}</div>
      <canvas class="dgraph" id="dt-graph"></canvas>
      <div class="dv-path">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M3 7.5c0-.8.6-1.5 1.4-1.5H9l2 2.2h8.6c.8 0 1.4.7 1.4 1.5v8.8c0 .8-.6 1.5-1.4 1.5H4.4c-.8 0-1.4-.7-1.4-1.5z"/></svg>
        <span id="dt-file"></span>
        <button type="button" class="dv-btn-copy" id="dt-copy">${t("btn_copy")}</button>
      </div>
      <div class="dv-sha" id="dt-sha" hidden></div>
      <div class="errline" id="dt-err"></div>
    </div>
    <div class="btns">
      <button id="dt-folder" class="lead">${t("ctx_open_folder")}</button>
      <button id="dt-toggle"></button>
      <button id="dt-close" class="pri">${t("btn_close")}</button>
    </div>`);
  $("dlg").classList.add("wide");
  $("dt-close").onclick = closeModal;
  $("dt-folder").onclick = () => {
    const jj = jobs.get(detailsFor);
    if (jj) invoke("open_path", { path: jj.out_path || "", folder: true }).catch((e) => toast(trErr(e)));
  };
  $("dt-copy").onclick = () => {
    const jj = jobs.get(detailsFor);
    if (jj && jj.out_path) {
      navigator.clipboard?.writeText(jj.out_path).then(() => {
        const btn = $("dt-copy");
        btn.textContent = t("btn_copied");
        setTimeout(() => { btn.textContent = t("btn_copy"); }, 1500);
      }).catch(() => {});
    }
  };
  $("dt-toggle").onclick = () => {
    const jj = jobs.get(detailsFor);
    if (!jj) return;
    const action = jj.status === "running" ? "pause" : "resume";
    invoke("job_action", { action, id: detailsFor }).catch((e) => toast(trErr(e)));
  };
  const cv = $("dt-graph");
  new ResizeObserver(() => paintGraph(cv, detailSeries())).observe(cv);
  updateDetails();
}
function updateDetails() {
  if (detailsFor == null || !$("dt-name")) return;
  const j = jobs.get(detailsFor);
  if (!j) { closeModal(); return; }
  const pct = pctOf(j);
  $("dt-name").textContent = j.filename || "?";
  $("dt-name").title = j.filename || "";
  $("dt-ico").textContent = icoOf(j).ch;
  $("dt-cat").textContent = t(CATS.find(([id]) => id === catOf(j))[1]);
  const st = $("dt-st");
  st.className = "st " + j.status;
  st.textContent = statusLabel(j);
  $("dt-fill").style.width = pct + "%";
  $("dt-fill").style.background = fillColor(j);
  $("dt-done").textContent = fmt(j.done) + (j.size ? " / " + fmt(j.size) : "") + " (" + pct.toFixed(0) + "%)";
  $("dt-live").textContent = j.status === "running" ? fmtSpeed(curSpeed(j)) + " · " + fmtEta(etaOf(j)) : "";
  const err = $("dt-err");
  if (j.error && (j.status === "failed" || j.status === "cancelled")) {
    err.textContent = t("error_label") + ": " + j.error;
    err.title = j.error;
    err.className = "errline on";
  } else { err.className = "errline"; err.textContent = ""; err.removeAttribute("title"); }
  $("dt-file").textContent = j.out_path || "-";
  $("dt-file").title = j.out_path || "";
  $("dt-size").textContent = j.size ? fmt(j.size) : "?";
  const pnum = typeof j.priority === "number" ? j.priority : 0;
  const plabel = pnum > 0 ? `${t("pri_high")} (+${pnum})` : pnum < 0 ? `${t("pri_low")} (${pnum})` : t("pri_normal");
  $("dt-queue").textContent = j.queue && j.queue !== "main" ? j.queue : t("queue_main");
  $("dt-prio").textContent = plabel;
  $("dt-lim").textContent = j.speed_limit > 0 ? j.speed_limit + " KiB/s" : t("lim_global");
  const shaEl = $("dt-sha");
  if (shaEl) {
    shaEl.hidden = !j.sha256;
    if (j.sha256) {
      const short = j.sha256.length > 20 ? j.sha256.slice(0, 20) + "…" : j.sha256;
      shaEl.textContent = t("sha_lbl") + ": " + short;
      shaEl.title = j.sha256;
      shaEl.onclick = () => {
        navigator.clipboard?.writeText(j.sha256).then(() => toast(t("ver_ok")), () => {});
      };
    }
  }
  const tog = $("dt-toggle");
  if (tog) {
    const idle = j.status === "completed" || j.status === "resolving";
    tog.hidden = idle; // nothing to pause/resume once finished or still resolving
    if (!idle) tog.textContent = j.status === "running" ? t("stop") : t("resume");
  }
  paintGraph($("dt-graph"), detailSeries());
}
function renderStatus() {
  const running = [...jobs.values()].filter((j) => j.status === "running");
  const tot = running.reduce((a, j) => a + (j.speed || 0), 0);
  $("s-items").textContent = t("items_fmt", { n: jobs.size, m: running.length });
  $("s-speed").textContent = t("total_fmt", { s: fmtSpeed(tot) });
}

/* ---------- graph ---------- */
/// The one number the speed card and the graph label agree on: the latest
/// smoothed sample (falling back to the rolling-average estimate).
function curSpeed(j) {
  const h = jobHist.get(j.id);
  return h && h.length ? h[h.length - 1].v : (j.speed || 0);
}
/// Distinct hue per segment index; 47 is coprime to 360 so up to ~30 bands
/// stay visually apart. Segment indices are stable across dynamic splits.
function segColor(i, a) {
  return `hsla(${(i * 47 + 8) % 360},60%,50%,${a})`;
}
function paintGraph(cv, samples) {
  if (!cv || !cv.getContext) return;
  lastGraph = [cv, samples];
  const ctx = cv.getContext("2d");
  const r = cv.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(40, Math.round(r.width * dpr));
  const h = Math.max(24, Math.round(r.height * dpr));
  if (cv.width !== w || cv.height !== h) { cv.width = w; cv.height = h; }
  ctx.clearRect(0, 0, w, h);
  const C = document.documentElement.dataset.theme === "dark"
    ? { grid: "#2b2f37", grid2: "#3c414d", line: "#4fae60", txt: "#9aa2ad" }
    : { grid: "#e8eaee", grid2: "#d3d7dd", line: "#268a3c", txt: "#5d6470" };
  const vals = samples.map((s) => s.v);
  const max = Math.max(1024, ...vals) * 1.15;
  ctx.strokeStyle = C.grid;
  ctx.lineWidth = 0.75 * dpr;
  const step = 34 * dpr;
  for (let x = step; x < w; x += step) { ctx.beginPath(); ctx.moveTo(x, 0); ctx.lineTo(x, h); ctx.stroke(); }
  for (let y = step; y < h; y += step) { ctx.beginPath(); ctx.moveTo(0, y); ctx.lineTo(w, y); ctx.stroke(); }
  ctx.strokeStyle = C.grid2;
  ctx.beginPath(); ctx.moveTo(0, h - 1); ctx.lineTo(w, h - 1); ctx.stroke();
  const peak = Math.max(0, ...vals);
  if (peak >= 1 && samples.length > 1) {
    const n = samples.length;
    const x = (i) => (i / (n - 1)) * w;
    const y = (v) => h - (v / max) * (h - 4 * dpr) - 2 * dpr;
    const maxSeg = samples.reduce((m, s) => Math.max(m, s.seg ? s.seg.length : 0), 0);
    if (maxSeg > 1) {
      // stacked bands: one colored area per segment, total = outline on top
      const cums = samples.map((sm) => {
        const a = [0];
        (sm.seg || []).forEach((v) => a.push(a[a.length - 1] + v));
        return a;
      });
      for (let s = 0; s < maxSeg; s++) {
        ctx.beginPath();
        for (let i = 0; i < n; i++) { const yy = y(cums[i][s + 1] || 0); i ? ctx.lineTo(x(i), yy) : ctx.moveTo(x(i), yy); }
        for (let i = n - 1; i >= 0; i--) ctx.lineTo(x(i), y(cums[i][s] || 0));
        ctx.closePath();
        ctx.fillStyle = segColor(s, 0.6);
        ctx.fill();
      }
      ctx.beginPath();
      samples.forEach((sm, i) => { const yy = y(sm.v); i ? ctx.lineTo(x(i), yy) : ctx.moveTo(x(i), yy); });
      ctx.strokeStyle = C.line; ctx.lineWidth = 1.5 * dpr;
      ctx.lineJoin = "round";
      ctx.stroke();
      ctx.fillStyle = C.txt;
      ctx.font = `${10 * dpr}px system-ui, sans-serif`;
      ctx.fillText(fmt(max) + "/s", 4 * dpr, 10 * dpr);
      const lbl = `${maxSeg} seg`;
      ctx.fillText(lbl, w - ctx.measureText(lbl).width - 4 * dpr, 10 * dpr);
    } else {
      ctx.beginPath();
      vals.forEach((v, i) => {
        const yy = y(v);
        i ? ctx.lineTo(x(i), yy) : ctx.moveTo(x(i), yy);
      });
      ctx.lineTo(w, h); ctx.lineTo(0, h); ctx.closePath();
      const grad = ctx.createLinearGradient(0, 0, 0, h);
      if (document.documentElement.dataset.theme === "dark") {
        grad.addColorStop(0, "rgba(79,174,96,.32)");
        grad.addColorStop(1, "rgba(79,174,96,.03)");
      } else {
        grad.addColorStop(0, "rgba(47,158,68,.28)");
        grad.addColorStop(1, "rgba(47,158,68,.03)");
      }
      ctx.fillStyle = grad;
      ctx.fill();
      ctx.beginPath();
      vals.forEach((v, i) => {
        const yy = y(v);
        i ? ctx.lineTo(x(i), yy) : ctx.moveTo(x(i), yy);
      });
      ctx.strokeStyle = C.line; ctx.lineWidth = 1.5 * dpr;
      ctx.lineJoin = "round";
      ctx.stroke();
      ctx.fillStyle = C.txt;
      ctx.font = `${10 * dpr}px system-ui, sans-serif`;
      // label = current speed, matching the card (max only scales the axis)
      ctx.fillText(fmt(vals.length ? vals[vals.length - 1] : 0) + "/s", 4 * dpr, 10 * dpr);
    }
  } else {
    ctx.fillStyle = C.txt;
    ctx.font = `${10 * dpr}px system-ui, sans-serif`;
    ctx.fillText(fmt(vals.length ? vals[vals.length - 1] : 0) + "/s", 4 * dpr, 10 * dpr);
  }
}

