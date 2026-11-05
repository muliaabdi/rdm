/* ---------- dialogs ---------- */
let lastFocus = null;
function openModal(html) {
  lastFocus = document.activeElement;
  $("dlg").classList.remove("wide", "hasbody", "settings");
  $("dlg").innerHTML = html;
  $("modal").classList.add("open");
  const f = $("dlg").querySelector("input, select, button:not(.pri)");
  if (f) setTimeout(() => f.focus(), 60);
}
function closeModal() {
  $("modal").classList.remove("open");
  detailsFor = null;
  queueAssignNext = null;
  if (lastFocus && lastFocus.focus) lastFocus.focus();
  lastFocus = null;
}
$("modal").onclick = (e) => { if (e.target.id === "modal") closeModal(); };
window.addEventListener("keydown", (e) => {
  if (e.key !== "Escape") return;
  if ($("ctx").classList.contains("open")) { closeCtx(); return; }
  if ($("modal").classList.contains("open")) closeModal();
});

const YT_HOSTS = /(^|\.)(youtube\.com|youtu\.be|vimeo\.com|dailymotion\.com|twitch\.tv|instagram\.com|tiktok\.com|twitter\.com|x\.com|facebook\.com|fb\.watch|reddit\.com|soundcloud\.com|bilibili\.com|nicovideo\.jp|rumble\.com|odysee\.com)$/i;
function isPageUrl(u) {
  try {
    const x = new URL(u);
    if (YT_HOSTS.test(x.hostname)) return true;
    return !/\.[a-z0-9]{2,5}$/i.test(x.pathname);
  } catch { return false; }
}

$("b-add").onclick = () => {
  queueAssignNext = queueSel !== "all" ? queueSel : null;
  openModal(`<h3>${t("dlg_add_title")}</h3>
    <textarea id="m-url" rows="1" placeholder="${esc(t("ph_url"))}" spellcheck="false" autocomplete="off" autocapitalize="off"></textarea>
    <div id="m-hint" class="mhint"></div>
    <div id="m-quality"></div>
    <div class="btns">
      <button id="m-detect">${t("btn_detect")}</button>
      <button id="m-cancel">${t("btn_cancel")}</button>
      <button id="m-ok" class="pri">${t("btn_download")}</button>
    </div>`);
  $("m-cancel").onclick = closeModal;
  const hint = (txt) => { $("m-hint").textContent = txt || ""; };
  // batch: one URL per line ("#" = comment); same rules as `rdm add -`
  const parsedUrls = () => {
    const seen = new Set();
    const out = [];
    for (let raw of $("m-url").value.split("\n")) {
      const line = raw.trim();
      if (!line || line.startsWith("#")) continue;
      if (!/^https?:\/\//i.test(line)) continue;
      if (!seen.has(line)) { seen.add(line); out.push(line); }
    }
    return out;
  };
  const refreshHint = () => {
    const multi = $("m-url").value.includes("\n");
    $("m-detect").style.display = multi ? "none" : "";
    if (multi) {
      const n = parsedUrls().length;
      hint(n ? t("urls_parsed", { n }) : t("urls_none"));
    } else {
      hint(isPageUrl($("m-url").value.trim()) ? t("hint_page") : "");
    }
  };
  $("m-url").addEventListener("input", refreshHint);
  refreshHint();

  const renderQuality = (m) => {
    if (m.gallery) {
      // picture gallery (gallery-dl): one choice — take every file
      openModal(`<h3>${t("dlg_quality_title")}</h3>
        <div class="mhint">${esc(m.title || m.url || "")}</div>
        <select id="m-q"><option value="gallery">${esc(t("opt_gallery", { n: m.files || 0 }))}</option></select>
        <div class="btns"><button id="m-qcancel">${t("btn_cancel")}</button><button id="m-qok" class="pri">${t("btn_download")}</button></div>`);
      $("m-qcancel").onclick = closeModal;
      $("m-qok").onclick = async () => {
        try { await invoke("add_page", { url: m.url, height: 1080, audioOnly: false }); }
        catch (e) { toast(trErr(e)); }
        closeModal();
      };
      return;
    }
    const fmts = (m.formats || []).filter((f) => f.vcodec !== "none" && f.height);
    const heights = [...new Set(fmts.map((f) => f.height))].sort((a, b) => b - a);
    const opts = heights.map((h) => {
      const f = fmts.find((x) => x.height === h);
      const sz = f && f.filesize ? " (" + fmt(f.filesize) + ")" : "";
      return `<option value="${h}">${h}p${sz}</option>`;
    }).join("");
    const hasAudio = (m.formats || []).some((f) => f.acodec !== "none");
    openModal(`<h3>${t("dlg_quality_title")}</h3>
      <div class="mhint">${esc(m.title || m.url || "")}</div>
      <select id="m-q">${opts}${hasAudio ? `<option value="audio">${t("opt_audio")}</option>` : ""}</select>
      <div class="btns"><button id="m-qcancel">${t("btn_cancel")}</button><button id="m-qok" class="pri">${t("btn_download")}</button></div>`);
    $("m-qcancel").onclick = closeModal;
    $("m-qok").onclick = async () => {
      const val = $("m-q").value;
      const audio = val === "audio";
      const h = audio ? 0 : Number(val) || 1080;
      try { await invoke("add_page", { url: m.url, height: h, audioOnly: audio }); }
      catch (e) { toast(trErr(e)); }
      closeModal();
    };
  };

  const detect = async () => {
    const url = $("m-url").value.trim();
    if (!url || url.includes("\n")) return;
    hint(t("hint_detecting"));
    window.__onFormats = (m) => {
      window.__onFormats = null;
      if (!m || m.__err) { hint(t("hint_fail", { e: (m && m.__err) || "?" })); return; }
      renderQuality(m);
    };
    try { await invoke("probe_url", { url }); } catch (e) { hint(t("hint_fail", { e: e })); }
  };
  $("m-detect").onclick = detect;

  const go = async () => {
    const urls = parsedUrls();
    if (urls.length > 1) {
      // batch: one job per line, sequential so the queue fills in order
      let ok = 0;
      for (const u of urls) {
        try { await invoke("add_url", { url: u }); ok++; } catch (e) { toast(trErr(e)); }
      }
      if (ok > 1) toast(t("batch_added", { n: ok }));
      closeModal();
      return;
    }
    const url = urls[0] || $("m-url").value.trim();
    if (!url) return;
    if (isPageUrl(url) && !$("m-quality").innerHTML) { return detect(); }
    try { await invoke("add_url", { url }); } catch (e) { toast(trErr(e)); }
    closeModal();
  };
  $("m-ok").onclick = go;
  // single-line field behaves like the old input (Enter = go);
  // once multi-line, Enter must insert a newline instead
  $("m-url").addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !$("m-url").value.includes("\n")) {
      e.preventDefault();
      go();
    }
  });
  setTimeout(() => $("m-url").focus(), 50);
};

$("b-resume").onclick = () => selected && invoke("job_action", { action: "resume", id: selected }).catch((e) => toast(trErr(e)));
$("b-stop").onclick = () => selected && invoke("job_action", { action: "pause", id: selected }).catch((e) => toast(trErr(e)));
$("b-delete").onclick = () => selected && confirmDelete(selected);
$("b-restart").onclick = () => selected && invoke("job_action", { action: "restart", id: selected }).catch((e) => toast(trErr(e)));
$("b-prio-up").onclick = () => bumpPrio(+1);
$("b-prio-dn").onclick = () => bumpPrio(-1);
$("b-queue").onclick = () => selected && openQueueDlg(selected);
function bumpPrio(d) {
  const j = selected && jobs.get(selected);
  if (!j) return;
  const p = Math.max(-100, Math.min(100, (typeof j.priority === "number" ? j.priority : 0) + d));
  jobSet(selected, { priority: p });
}

/* ---------- per-job tuning: speed limit / priority / queue ---------- */
function jobSet(id, v) {
  invoke("job_set", { id, v }).catch((e) => toast(trErr(e)));
}
// when Add URL runs while a queue view is open, the new job joins that queue
function consumeQueueAssign(id) {
  if (!queueAssignNext) return;
  const q = queueAssignNext;
  queueAssignNext = null;
  jobSet(id, { queue: q });
}
function openLimitDlg(id) {
  const j = jobs.get(id);
  const cur = j && j.speed_limit ? j.speed_limit : 0;
  openModal(`<h3>${t("dlg_limit_title")}</h3>
    <input id="m-lim" type="number" min="0" max="1048576" step="1" value="${cur}" inputmode="numeric" autofocus />
    <div class="mhint">${t("dlg_limit_hint")}<br />${t("dlg_limit_global")}</div>
    <div class="btns">
      <button id="m-lcancel">${t("btn_cancel")}</button>
      <button id="m-lok" class="pri">${t("btn_save")}</button>
    </div>`);
  $("m-lcancel").onclick = closeModal;
  const go = () => {
    const k = Math.max(0, Math.min(1 << 20, Math.floor(Number($("m-lim").value) || 0)));
    jobSet(id, { speed_limit: k });
    closeModal();
  };
  $("m-lok").onclick = go;
  $("m-lim").addEventListener("keydown", (e) => e.key === "Enter" && go());
}
function openPrioDlg(id) {
  const j = jobs.get(id);
  const cur = j && typeof j.priority === "number" ? j.priority : 0;
  const opts = [[5, "pri_high"], [0, "pri_normal"], [-5, "pri_low"]];
  openModal(`<h3>${t("dlg_prio_title")}</h3>
    <div class="prio-opts">${opts.map(([p, key]) =>
      `<button type="button" data-p="${p}" class="prio ${p === cur ? "cur" : ""}">${t(key)}</button>`).join("")}</div>
    <div class="mhint">${t("pri_hint")}</div>
    <div class="btns"><button id="m-pcancel">${t("btn_cancel")}</button></div>`);
  $("m-pcancel").onclick = closeModal;
  $("dlg").querySelectorAll(".prio").forEach((b) => {
    b.onclick = () => { jobSet(id, { priority: +b.dataset.p }); closeModal(); };
  });
}
function openQueueDlg(id) {
  const j = jobs.get(id);
  if (!j) return;
  const cur = j.queue || "main";
  const qs = queueCounts();
  const custom = [...qs.keys()].filter((q) => q !== "main").sort((a, b) => a.localeCompare(b));
  const row = (q, label) => `<button type="button" class="qopt${cur === q ? " cur" : ""}" data-q="${esc(q)}">
      <span class="nm">${esc(label)}</span>
      <span class="cnt">${qs.get(q) || 0}</span>
      <svg class="ck" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M4.5 12.5l5 5 10-11"/></svg>
    </button>`;
  openModal(`<h3>${t("q_move_title")}</h3>
    <div class="qopts">
      ${row("main", t("queue_main"))}
      ${custom.map((q) => row(q, q)).join("")}
    </div>
    <div class="qnew">
      <input id="m-qnew" placeholder="${esc(t("q_new_ph"))}" maxlength="32" spellcheck="false" autocomplete="off" autocapitalize="off" />
      <button id="m-qnew-ok">${t("q_move")}</button>
    </div>
    <div class="mhint">${t("dlg_queue_hint")}</div>
    <div class="btns"><button id="m-qclose">${t("btn_close")}</button></div>`);
  const move = (q) => { jobSet(id, { queue: q }); toast(t("q_moved", { q })); closeModal(); };
  $("dlg").querySelectorAll(".qopt").forEach((b) => { b.onclick = () => move(b.dataset.q); });
  $("m-qclose").onclick = closeModal;
  const go = () => { const q = $("m-qnew").value.trim(); if (q) move(q); };
  $("m-qnew-ok").onclick = go;
  $("m-qnew").addEventListener("keydown", (e) => e.key === "Enter" && go());
}
function openQueueMenu(q, x, y) {
  const rows = [...jobs.values()].filter((j) => (j.queue || "main") === q);
  const running = rows.filter((j) => j.status === "running");
  const resumable = rows.filter((j) => ["paused", "failed", "cancelled", "queued"].includes(j.status));
  const items = [
    { ic: "resume", label: t("q_resume_all"), en: resumable.length > 0,
      fn: () => resumable.forEach((j) => invoke("job_action", { action: "resume", id: j.id }).catch((e) => toast(trErr(e)))) },
    { ic: "stop", label: t("q_pause_all"), en: running.length > 0,
      fn: () => running.forEach((j) => invoke("job_action", { action: "pause", id: j.id }).catch((e) => toast(trErr(e)))) },
    "sep",
    { ic: "flag", label: t("q_rename"), en: true, fn: () => openRenameQueue(q) },
    { ic: "layers", label: t("q_to_main"), en: q !== "main",
      fn: () => {
        rows.forEach((j) => jobSet(j.id, { queue: "main" }));
        toast(t("q_moved_main", { n: rows.length }));
      } },
  ];
  $("ctx").innerHTML = items.map((it) => it === "sep"
    ? '<div class="sep"></div>'
    : `<button type="button" role="menuitem" ${it.en ? "" : "disabled"}>${CTX_ICONS[it.ic]}<span>${esc(it.label)}</span></button>`
  ).join("");
  const btns = [...$("ctx").querySelectorAll("button")];
  btns.forEach((b, k) => {
    const it = items.filter((x2) => x2 !== "sep")[k];
    b.onclick = () => { closeCtx(); it.fn(); };
  });
  $("ctx").classList.add("open");
  const r = $("ctx").getBoundingClientRect();
  $("ctx").style.left = Math.max(4, Math.min(x, window.innerWidth - r.width - 6)) + "px";
  $("ctx").style.top = Math.max(4, Math.min(y, window.innerHeight - r.height - 6)) + "px";
  const first = btns.find((b) => !b.disabled);
  if (first) first.focus();
}
function openRenameQueue(q) {
  openModal(`<h3>${t("q_rename_title")}</h3>
    <input id="m-qren" value="${esc(q)}" maxlength="32" spellcheck="false" autocomplete="off" autocapitalize="off" autofocus />
    <div class="mhint">${t("q_rename_hint")}</div>
    <div class="btns">
      <button id="m-qrcancel">${t("btn_cancel")}</button>
      <button id="m-qrok" class="pri">${t("btn_save")}</button>
    </div>`);
  $("m-qrcancel").onclick = closeModal;
  const go = () => {
    const nv = $("m-qren").value.trim();
    closeModal();
    if (!nv || nv === q) return;
    [...jobs.values()].filter((j) => (j.queue || "main") === q).forEach((j) => jobSet(j.id, { queue: nv }));
    queueSel = nv;
  };
  $("m-qrok").onclick = go;
  $("m-qren").addEventListener("keydown", (e) => e.key === "Enter" && go());
  setTimeout(() => { const i = $("m-qren"); if (i) { i.focus(); i.select(); } }, 60);
}

// Details opens by double-click, Enter or the row context menu
$("rows").addEventListener("keydown", (e) => e.key === "Enter" && document.activeElement.classList.contains("row") && openDetails(+document.activeElement.dataset.id));

/* ---------- file-type groups for the intercept filter ---------- */
const FTYPES = [
  ["images", "ft_images", "jpg jpeg png gif webp bmp svg ico tiff avif heic"],
  ["videos", "ft_videos", "mp4 mkv avi mov webm flv wmv m4v mpg mpeg ts 3gp"],
  ["audio", "ft_audio", "mp3 wav flac aac ogg opus m4a wma mid"],
  ["docs", "ft_docs", "pdf doc docx xls xlsx ppt pptx txt rtf odt ods odp csv epub mobi"],
  ["apps", "ft_apps", "exe msi apk deb rpm appimage flatpak snap dmg pkg jar"],
  ["arch", "ft_arch", "zip rar 7z tar gz bz2 xz zst lz lzma cab iso img"],
  ["others", "ft_others", "torrent bin dat m3u8 mpd nfo srt"],
];
const EXT2CAT = new Map(FTYPES.flatMap(([id, , str]) => str.split(" ").map((x) => [x, id])));
// folder-category keys must match rdm_core::category_of (settings.categories);
// distinct from the sidebar CATS above (list filters). [id, i18n key]
const FOLDER_CATS = [
  ["videos", "ft_videos"],
  ["audio", "ft_audio"],
  ["images", "ft_images"],
  ["documents", "ft_docs"],
  ["apps", "ft_apps"],
  ["archives", "ft_arch"],
  ["others", "ft_others"],
];

$("b-options").onclick = async () => {
  const s = await invoke("get_settings");
  const auto = await invoke("get_autostart").catch(() => !!s.autostart);
  const list = s.intercept_ext || [];
  const allOn = list.length === 0;
  // show the effective allow-list: known-category extensions too, otherwise a
  // lone "pdf" would vanish (its category needs every doc ext to check)
  const extras = allOn ? "" : list.join(", ");
  // category-folder overrides: only the ones actually customized are listed;
  // default behavior puts every file straight into the main folder
  const catdirs = {};
  for (const [id] of FOLDER_CATS) {
    const v = ((s.categories || {})[id] || "").trim();
    if (v) catdirs[id] = v;
  }
  openModal(`<h3>${t("dlg_settings_title")}</h3>
    <div class="tabs" role="tablist">
      <button type="button" role="tab" aria-selected="true" data-pane="p-dl" class="on">${t("tab_downloads")}</button>
      <button type="button" role="tab" aria-selected="false" data-pane="p-br">${t("tab_browser")}</button>
      <button type="button" role="tab" aria-selected="false" data-pane="p-ui">${t("tab_interface")}</button>
    </div>
    <div class="dlgbody">
    <div class="tabpane on" id="p-dl" role="tabpanel">
    <div class="fgrp">
      <label for="o-dir">${t("fld_dir")}</label>
      <div class="fldpath">
        <input id="o-dir" value="${esc(s.dir || "")}" placeholder="${esc(t("ph_dir"))}" spellcheck="false">
        <button type="button" id="o-browse" title="${t("btn_browse")}" aria-label="${t("btn_browse")}"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3.5 6.3c0-.7.6-1.3 1.3-1.3h4l2 2.2h8.4c.7 0 1.3.6 1.3 1.3v8.2c0 .7-.6 1.3-1.3 1.3H4.8c-.7 0-1.3-.6-1.3-1.3z"/><path d="M3.5 19h12.2l4.8-8.4"/></svg></button>
      </div>
    </div>
    <div class="frow c3">
      <label class="fgrp"><span>${t("fld_segments")}</span><input id="o-seg" type="number" min="1" max="32" value="${s.segments || 8}"></label>
      <label class="fgrp"><span>${t("fld_parallel")}</span><input id="o-par" type="number" min="1" max="16" value="${s.max_parallel || 3}"></label>
      <label class="fgrp"><span>${t("fld_speed")} (KiB/s)</span><input id="o-speed" type="number" min="0" step="64" value="${s.speed_limit || 0}"></label>
    </div>
    <div class="fgrp">
      <label for="o-dns">${t("fld_dns")}</label>
      <div class="dnsgrp${s.dns_mode === "custom" ? " custom" : ""}" id="o-dns-grp">
        <select id="o-dns">
          <option value="cloudflare"${(s.dns_mode || "cloudflare") === "cloudflare" ? " selected" : ""}>${t("opt_dns_cf")}</option>
          <option value="system"${s.dns_mode === "system" ? " selected" : ""}>${t("opt_dns_sys")}</option>
          <option value="google"${s.dns_mode === "google" ? " selected" : ""}>${t("opt_dns_gg")}</option>
          <option value="custom"${s.dns_mode === "custom" ? " selected" : ""}>${t("opt_dns_custom")}</option>
        </select>
        <input id="o-dns-custom" placeholder="${esc(t("ph_dns_custom"))}" value="${esc(s.dns_custom || "")}" spellcheck="false">
      </div>
    </div>
    <div class="sec">${t("fld_cats")}</div>
    <p class="cathint">${t("hint_cats")}</p>
    <div id="ovlist"></div>
    <div id="ovadd"></div>
    </div>
    <div class="tabpane" id="p-br" role="tabpanel">
    <div class="fgrp">
      <span>${t("fld_ftype")}</span>
      <div class="ftypes${allOn ? " all" : ""}" id="ft-box">
        <label class="fchk"><input type="checkbox" id="ft-all"${allOn ? " checked" : ""}><span>${t("ft_all")}</span></label>
        ${FTYPES.map(([id, key, str]) => `<label class="fchk"><input type="checkbox" id="ft-${id}"${str.split(" ").every((x) => list.includes(x)) ? " checked" : ""}${allOn ? " disabled" : ""}><span>${t(key)}</span></label>`).join("")}
      </div>
    </div>
    <div class="fgrp">
      <label for="o-int">${t("fld_extra")}</label>
      <input id="o-int" placeholder="${esc(t("ph_intercept"))}" value="${esc(extras)}" spellcheck="false" autocomplete="off">
    </div>
    </div>
    <div class="tabpane" id="p-ui" role="tabpanel">
    <div class="frow c2">
      <label class="fgrp"><span>${t("fld_lang")}</span><select id="o-lang">
        <option value="en"${lang === "en" ? " selected" : ""}>English</option>
        <option value="id"${lang === "id" ? " selected" : ""}>Indonesia</option></select></label>
      <label class="fgrp"><span>${t("fld_theme")}</span><select id="o-theme">
        <option value="system"${(localStorage.rdm_theme || "system") === "system" ? " selected" : ""}>${t("opt_system")}</option>
        <option value="light"${localStorage.rdm_theme === "light" ? " selected" : ""}>${t("opt_light")}</option>
        <option value="dark"${localStorage.rdm_theme === "dark" ? " selected" : ""}>${t("opt_dark")}</option></select></label>
    </div>
    <div class="sec">${t("sec_behavior")}</div>
    <label class="tgl"><span>${t("fld_autostart")}</span><input id="o-auto" type="checkbox"${auto ? " checked" : ""}></label>
    <label class="tgl"><span>${t("fld_notify")}</span><input id="o-notify" type="checkbox"${s.notify !== false ? " checked" : ""}></label>
    <label class="tgl"><span>${t("fld_clipboard")}</span><input id="o-clip" type="checkbox"${s.clipboard_watch !== false ? " checked" : ""}></label>
    <label class="tgl"><span>${t("fld_ytdlp")}</span><input id="o-ytdlp" type="checkbox"${s.ytdlp_update !== false ? " checked" : ""}></label>
    <label class="tgl"><span>${t("fld_sha256")}</span><input id="o-sha" type="checkbox"${s.sha256 === true ? " checked" : ""}></label>
    </div>
    </div>
    <div class="btns"><button id="o-cancel">${t("btn_cancel")}</button><button id="o-ok" class="pri">${t("btn_save")}</button></div>`);
  $("dlg").classList.add("hasbody", "settings");
  document.querySelectorAll("#dlg .tabs button").forEach((b) => {
    b.onclick = () => {
      document.querySelectorAll("#dlg .tabs button").forEach((x) => {
        x.classList.toggle("on", x === b);
        x.setAttribute("aria-selected", x === b ? "true" : "false");
      });
      document.querySelectorAll("#dlg .tabpane").forEach((p) => p.classList.toggle("on", p.id === b.dataset.pane));
    };
  });
  $("o-browse").onclick = async () => {
    try {
      const dir = await invoke("plugin:dialog|open", { options: { directory: true, multiple: false } });
      if (typeof dir === "string" && dir) $("o-dir").value = dir;
    } catch {}
  };
  const renderOv = () => {
    $("ovlist").innerHTML = FOLDER_CATS.filter(([id]) => id in catdirs).map(([id, key]) => `
      <div class="ovrow"><span>${t(key)}</span>
        <input class="ovin" data-id="${id}" value="${esc(catdirs[id])}" placeholder="${esc(t("ph_cat_dir"))}" spellcheck="false" autocomplete="off">
        <button type="button" class="ovdel" data-id="${id}" title="${t("btn_remove")}">✕</button></div>`).join("");
    $("ovlist").querySelectorAll(".ovin").forEach((inp) => {
      inp.oninput = () => { catdirs[inp.dataset.id] = inp.value.trim(); };
    });
    $("ovlist").querySelectorAll(".ovdel").forEach((b) => {
      b.onclick = () => { delete catdirs[b.dataset.id]; renderOv(); renderAdd(); };
    });
  };
  const renderAdd = () => {
    const left = FOLDER_CATS.filter(([id]) => !(id in catdirs));
    if (!left.length) {
      $("ovadd").innerHTML = "";
      return;
    }
    $("ovadd").innerHTML = `<select id="ov-sel"><option value="">${esc(t("ph_pick_cat"))}</option>${left
      .map(([id, key]) => `<option value="${id}">${t(key)}</option>`)
      .join("")}</select>`;
    $("ov-sel").onchange = () => {
      const id = $("ov-sel").value;
      if (!id) return;
      catdirs[id] = "";
      renderOv();
      renderAdd();
      const inp = $("ovlist").querySelector(`.ovin[data-id="${id}"]`);
      if (inp) inp.focus();
    };
  };
  renderOv();
  renderAdd();
  $("o-cancel").onclick = closeModal;
  $("o-dns").onchange = () => {
    const custom = $("o-dns").value === "custom";
    $("o-dns-grp").classList.toggle("custom", custom);
    if (custom) $("o-dns-custom").focus();
  };
  $("ft-all").onchange = () => {
    const on = $("ft-all").checked;
    $("ft-box").classList.toggle("all", on);
    for (const [id] of FTYPES) $("ft-" + id).disabled = on;
  };
  $("o-int").oninput = () => {
    if ($("o-int").value.trim() && $("ft-all").checked) {
      $("ft-all").checked = false;
      $("ft-all").onchange();
    }
  };
  $("o-ok").onclick = async () => {
    const next = $("o-lang").value;
    let ints = [];
    if (!$("ft-all").checked)
      for (const [id, , str] of FTYPES) if ($("ft-" + id).checked) ints.push(...str.split(" "));
    ints.push(...(($("o-int").value || "")
      .split(/[,;\s]+/)
      .map((x) => x.replace(/^\./, "").toLowerCase())
      .filter((x) => /^[a-z0-9]{1,8}$/.test(x))));
    ints = [...new Set(ints)];
    if (!$("ft-all").checked && ints.length === 0) toast(t("ft_empty_all"));
    const auto = $("o-auto").checked;
    // drop overrides the user added but left empty
    const catsClean = Object.fromEntries(Object.entries(catdirs).filter(([, v]) => v));
    await invoke("set_settings", {
      // keep keys this dialog does not edit (unknown/future settings)
      v: { ...s, dir: $("o-dir").value.trim() || null, segments: Number($("o-seg").value) || 8,
           max_parallel: Math.min(16, Math.max(1, Number($("o-par").value) || 3)),
           speed_limit: Math.max(0, Number($("o-speed").value) || 0),
           dns_mode: $("o-dns").value, dns_custom: $("o-dns-custom").value.trim(),
           categories: catsClean, ytdlp_update: $("o-ytdlp").checked, theme: $("o-theme").value,
            sha256: $("o-sha").checked,
           lang: next, intercept_ext: ints, autostart: auto, notify: $("o-notify").checked,
           clipboard_watch: $("o-clip").checked },
    });
    clipWatch = $("o-clip").checked;
    setTheme($("o-theme").value);
    await invoke("set_autostart", { enabled: auto }).catch((e) => toast(trErr(e)));
    closeModal();
    if (next !== lang) setLang(next);
  };
};

(async () => {
  try {
    const s = await invoke("get_settings");
    if (s && s.lang === "id") lang = "id";
    if (s && s.theme) setTheme(s.theme);
    clipWatch = !s || s.clipboard_watch !== false;
    // keep the autostart entry pointing at this build
    if (s && s.autostart) invoke("set_autostart", { enabled: true }).catch(() => {});
  } catch (e) { diag("lang load: " + e); }
  applyI18n();
})();

invoke("list_jobs").catch(() => {});
setInterval(() => invoke("list_jobs").catch(() => {}), 5000);
if (detailWin) initDetailWindow();

/* ---------- clipboard watch: offer to download copied links ---------- */
const clipSeen = new Set();
let clipLast = "";
let clipWatch = true;
function clipOffer(url) {
  clipSeen.add(url);
  let el = $("clipoff");
  if (!el) {
    el = document.createElement("div");
    el.id = "clipoff"; el.setAttribute("role", "dialog");
    el.innerHTML = `<div class="co-txt"></div><div class="co-btns">
      <button class="co-dl"></button><button class="co-x">✕</button></div>`;
    document.body.appendChild(el);
    el.querySelector(".co-x").onclick = () => el.classList.remove("on");
  }
  el.querySelector(".co-txt").textContent = url;
  el.title = url;
  const dl = el.querySelector(".co-dl");
  dl.textContent = t("btn_download");
  dl.onclick = () => {
    el.classList.remove("on");
    invoke("add_url", { url }).catch((e) => toast(trErr(e)));
  };
  el.classList.add("on");
  clearTimeout(el._t);
  el._t = setTimeout(() => el.classList.remove("on"), 10000);
}
async function clipTick() {
  if (!clipWatch || detailWin || document.hidden) return;
  let txt = "";
  try { txt = (await invoke("plugin:clipboard-manager|read_text") || "").trim(); }
  catch { return; }
  if (!/^https?:\/\/\S+$/i.test(txt) || txt === clipLast) return;
  clipLast = txt;
  if (clipSeen.has(txt)) return;
  if ([...jobs.values()].some((j) => j.url === txt)) { clipSeen.add(txt); return; }
  clipOffer(txt);
}
setInterval(clipTick, 1000);

