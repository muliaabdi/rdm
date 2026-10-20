// Video overlay: "Download with RDM" tooltip above <video>/<audio>.
// Runs in all frames; hover a playable media element to reveal.

(function () {
  if (window.__rdmVideoOverlay) return;
  window.__rdmVideoOverlay = true;

  let btn = null;
  let hideTimer = null;
  let current = null;
  let pageModeNow = false;
  let pageUrl = null;
  let menu = null;
  let menuOutside = null;
  let menuKey = null;

  function urlOf(el) {
    const u = (el && (el.currentSrc || el.src)) || "";
    return /^https?:\/\//i.test(u) ? u : "";
  }

  function nameOf(url) {
    try {
      return decodeURIComponent(new URL(url).pathname.split("/").filter(Boolean).pop() || "");
    } catch {
      return "";
    }
  }

  // For blob:/MSE media we offer "the page" — but on feed sites (x.com,
  // reddit) the hovered video belongs to one post, not the timeline.
  // Walk up from the media element and prefer that post's permalink.
  function contextualPageUrl(el) {
    const abs = (h) => {
      try {
        return new URL(h, location.origin).href.split("?")[0];
      } catch {
        return "";
      }
    };
    // direct enclosing post link beats everything (old reddit, embeds)
    const enclosing = el.closest && el.closest('a[href*="/comments/"]');
    if (enclosing) return abs(enclosing.getAttribute("href") || "") || location.href;
    let n = el;
    for (let i = 0; n && i < 60; i++, n = n.parentElement) {
      if (n.tagName === "A") {
        const h = (n.getAttribute("href") || "").split("?")[0];
        if (/^\/?[^/]*\/status\/\d+/.test(h) || /\/comments\//.test(h)) return abs(h) || location.href;
      } else if (n.tagName === "SHREDDIT-POST" || (n.hasAttribute && n.hasAttribute("permalink"))) {
        // reddit: the post component carries its permalink as an attribute
        const p = n.getAttribute("permalink");
        if (p) return abs(p) || location.href;
        const a = n.querySelector('a[slot="full-post-link"], a[slot="title"], a[href*="/comments/"]');
        if (a) return abs(a.getAttribute("href") || "") || location.href;
      } else if (n.tagName === "ARTICLE" || (n.matches && n.matches('[data-testid="tweet"]'))) {
        const a = n.querySelector('a[href*="/status/"], a[href*="/comments/"]');
        if (a) return abs(a.getAttribute("href") || "") || location.href;
      }
    }
    return location.href;
  }

  function destroy() {
    if (btn) btn.remove();
    btn = null;
    current = null;
    closeMenu();
    window.removeEventListener("scroll", place, true);
    window.removeEventListener("resize", place);
  }

  function closeMenu() {
    if (menu) menu.remove();
    menu = null;
    if (menuOutside) {
      document.removeEventListener("mousedown", menuOutside, true);
      menuOutside = null;
    }
    if (menuKey) {
      document.removeEventListener("keydown", menuKey, true);
      menuKey = null;
    }
  }

  // Resolution picker: click the pill on a page-mode video and choose the
  // quality from the formats the daemon extracts (yt-dlp) for that page.
  function openMenu() {
    closeMenu();
    menu = document.createElement("div");
    menu.style.cssText = [
      "all:initial",
      "position:fixed",
      "left:0",
      "top:0",
      "z-index:2147483647",
    ].join(";");
    const root = menu.attachShadow({ mode: "open" });
    root.innerHTML = `<style>
      .box {
        font:12px/1 system-ui,sans-serif; color:#fff;
        background:rgba(20,21,25,.96); border:1px solid #1a73e8; border-radius:10px;
        box-shadow:0 6px 18px rgba(0,0,0,.45); padding:4px; min-width:120px;
        max-height:224px; overflow-y:auto; user-select:none;
      }
      .hd { font-size:10px; color:#9aa4b2; padding:5px 10px 3px; letter-spacing:.04em; }
      .it {
        display:flex; align-items:center; gap:8px; padding:6px 10px; border-radius:7px;
        cursor:pointer; white-space:nowrap;
      }
      .it:hover { background:#2a4b7c }
      .it b { font-weight:600 }
      .it .tag { font-size:9.5px; color:#ffd479; border:1px solid rgba(255,212,121,.5);
                 border-radius:4px; padding:1px 4px; }
      .dim { color:#9aa4b2 }
    </style>
    <div class="box"><div class="hd">RDM — loading formats…</div></div>`;
    document.documentElement.append(menu);
    clearTimeout(hideTimer);

    const box = () => root.querySelector(".box");
    const url = pageUrl || location.href;
    // usable immediately: the two choices that never need a page extract.
    // The resolution list replaces them once (and if) extraction lands.
    const FALLBACK = [
      { label: "Best", height: 1080, audio: false, tag: "auto" },
      { label: "MP3", height: 0, audio: true, tag: "audio" },
    ];
    buildItems(FALLBACK, "RDM — pick quality");
    chrome.runtime.sendMessage({ t: "page-formats", url }, (resp) => {
      if (!menu) return;
      const err = chrome.runtime.lastError;
      if (err || !resp || !resp.ok) {
        // extraction failed or timed out: keep the two reliable choices
        buildItems(FALLBACK, "RDM — fallback");
        return;
      }
      const fmts = Array.isArray(resp.formats) ? resp.formats : [];
      const heights = [...new Set(
        fmts.filter((f) => f.vcodec && f.vcodec !== "none" && f.height)
            .map((f) => f.height)
      )].sort((a, b) => b - a);
      const items = heights.map((h) => ({
        label: h + "p",
        height: h,
        audio: false,
        tag: h === heights[0] ? "best" : "",
      }));
      if (fmts.some((f) => f.acodec && f.acodec !== "none")) {
        items.push({ label: "MP3", height: 0, audio: true, tag: "audio" });
      }
      if (!items.length) items.push({ label: "Best", height: 1080, audio: false, tag: "auto" });
      buildItems(items, resp.title ? clip(resp.title, 26) : "RDM — quality");
    });

    function clip(s, n) {
      return s.length > n ? s.slice(0, n - 1) + "…" : s;
    }

    function buildItems(items, title) {
      const b = box();
      b.innerHTML = `<div class="hd">${title.replace(/&/g, "&amp;").replace(/</g, "&lt;")}</div>` +
        items
          .map(
            (it, i) =>
              `<div class="it" data-i="${i}"><b>${it.label}</b>${
                it.tag ? `<span class="tag">${it.tag}</span>` : ""
              }${it.audio ? `<span class="dim">audio only</span>` : ""}</div>`
          )
          .join("");
      b.querySelectorAll(".it").forEach((el) => {
        el.addEventListener("click", (e) => {
          e.stopPropagation();
          e.preventDefault();
          const it = items[Number(el.dataset.i)];
          el.querySelector("b").textContent = "…";
          try {
            chrome.runtime.sendMessage(
              { t: "page-offer", url, height: it.height || 1080, audio: !!it.audio },
              () => {
                // orphaned script after an extension reload leaves this
                // pending forever; a hard error at least names the problem
                el.querySelector("b").textContent = chrome.runtime.lastError ? "ERR" : "✓";
                setTimeout(() => { closeMenu(); destroy(); }, 900);
              }
            );
          } catch (err) {
            el.querySelector("b").textContent = "ERR (reload page)";
          }
        });
      });
      placeMenu();
    }

    menuOutside = (e) => {
      if (menu && !menu.contains(e.target) && (!btn || !btn.contains(e.target))) closeMenu();
    };
    document.addEventListener("mousedown", menuOutside, true);
    menuKey = (e) => {
      if (e.key === "Escape") closeMenu();
    };
    document.addEventListener("keydown", menuKey, true);
    placeMenu();
  }

  function placeMenu() {
    if (!menu || !btn) return;
    const r = btn.getBoundingClientRect();
    const b = menu.shadowRoot && menu.shadowRoot.querySelector(".box");
    const w = (b && b.offsetWidth) || 130;
    const h = (b && b.offsetHeight) || 90;
    const x = Math.max(4, Math.min(r.right - w, innerWidth - w - 4));
    const y = r.bottom + 6 + h > innerHeight ? Math.max(4, r.top - h - 6) : r.bottom + 6;
    menu.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
  }

  function place() {
    if (!btn || !current) return;
    const r = current.getBoundingClientRect();
    if (r.width < 60 || r.height < 40 || r.bottom < 0 || r.top > innerHeight) {
      destroy();
      return;
    }
    const w = btn.offsetWidth || 150;
    const x = Math.max(4, Math.min(r.right - w - 8, innerWidth - w - 4));
    const y = Math.max(4, r.top + 8);
    btn.style.transform = `translate(${Math.round(x)}px, ${Math.round(y)}px)`;
    placeMenu();
  }

  function show(el, url, isPage) {
    clearTimeout(hideTimer);
    pageModeNow = !!isPage;
    pageUrl = isPage ? contextualPageUrl(el) : null;
    if (!btn) {
      btn = document.createElement("div");
      btn.id = "__rdm_video_tip";
      btn.style.cssText = [
        "all:initial",
        "position:fixed",
        "left:0",
        "top:0",
        "z-index:2147483647",
        "font:12px/1 system-ui,sans-serif",
        "background:rgba(20,21,25,.92)",
        "color:#fff",
        "border:1px solid #1a73e8",
        "border-radius:16px",
        "padding:6px 12px",
        "cursor:pointer",
        "display:flex",
        "align-items:center",
        "gap:6px",
        "box-shadow:0 4px 14px rgba(0,0,0,.4)",
        "white-space:nowrap",
        "user-select:none",
        "pointer-events:auto",
        "transition:opacity .15s",
      ].join(";");
      const root = btn.attachShadow({ mode: "open" });
      root.innerHTML = `<style>
        .in { display:flex; align-items:center; gap:6px; font:12px/1 system-ui,sans-serif; color:#fff; }
        .in b { font-weight:600 }
        .in:hover b { color:#9ec5ff }
        svg { width:14px; height:14px; flex:none }
        .mp3 { display:none; font:10px/1 system-ui,sans-serif; font-weight:600; color:#ffd479;
               border:1px solid rgba(255,212,121,.55); border-radius:4px; padding:2px 4px; cursor:pointer; }
        .mp3:hover { background:rgba(255,212,121,.15) }
      </style>
      <span class="in">
        <svg viewBox="0 0 24 24" fill="none" stroke="#7db3f5" stroke-width="2.4"><path d="M12 4v11m0 0l-4-4m4 4l4-4M5 20h14"/></svg>
        <b id="__rdm_lbl">Download with RDM</b>
        <span class="mp3" id="__rdm_mp3" title="Download audio only">MP3</span>
      </span>`;
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        e.preventDefault();
        const u = urlOf(current || {});
        const b = btn.shadowRoot.querySelector("#__rdm_lbl");
        if (pageModeNow) {
          // page video: open the resolution picker instead of sending blind
          if (menu) {
            closeMenu();
            return;
          }
          openMenu();
          return;
        }
        if (!u) return;
        b.textContent = "sending…";
        chrome.runtime.sendMessage({ t: "fab-offer", url: u, name: nameOf(u), page: location.href }, () => {
          const err = chrome.runtime.lastError;
          b.textContent = err ? "ERR: " + err.message.slice(0, 30) : "✓ sent to RDM";
        });
        clearTimeout(hideTimer);
        hideTimer = setTimeout(destroy, 1200);
      });
      btn.shadowRoot.querySelector("#__rdm_mp3").addEventListener("click", (e) => {
        e.stopPropagation();
        e.preventDefault();
        if (!pageModeNow) return;
        const m = btn.shadowRoot.querySelector("#__rdm_mp3");
        m.textContent = "…";
        chrome.runtime.sendMessage({ t: "page-offer", url: pageUrl || location.href, audio: true }, () => {
          m.textContent = chrome.runtime.lastError ? "ERR" : "✓";
        });
        clearTimeout(hideTimer);
        hideTimer = setTimeout(destroy, 1200);
      });
      document.documentElement.append(btn);
      window.addEventListener("scroll", place, true);
      window.addEventListener("resize", place);
    }
    current = el;
    const lbl = btn.shadowRoot.querySelector("#__rdm_lbl");
    const mp3 = btn.shadowRoot.querySelector("#__rdm_mp3");
    if (lbl) {
      // one label for both modes: direct files and page (yt-dlp) videos alike
      lbl.textContent = "Download with RDM";
      btn.title = pageModeNow ? "Pick a quality for this page video" : "";
    }
    if (mp3) mp3.style.display = pageModeNow ? "block" : "none";
    if (pageModeNow) {
      // warm the format list while the user reaches for the pill, so the menu
      // is usually instant (background caches the reply and de-dupes calls)
      try {
        chrome.runtime.sendMessage(
          { t: "page-formats", url: pageUrl || location.href },
          () => void chrome.runtime.lastError
        );
      } catch {}
    }
    btn.style.opacity = "1";
    place();
  }

  function scheduleHide() {
    clearTimeout(hideTimer);
    // grace period is longer with the menu open so moving pill -> menu
    // across the gap does not tear both down
    hideTimer = setTimeout(destroy, menu ? 600 : 350);
  }

  // Resolve the media element for a hovered node. Players (JWPlayer, video.js,
  // custom HTML5 players) usually stack transparent control layers on top of
  // the <video>, so the hover target is a wrapper - look for the media inside
  // a player-ish ancestor too. Shadow-DOM hosts (Reddit faceplate, etc.)
  // retarget events to the host element, so walk the composed path and the
  // hosts' own shadow roots as well.
  function mediaAt(el, path) {
    if (!el || el.nodeType !== 1 || !el.closest) return null;
    if (path && path.length) {
      for (const n of path) {
        if (n && n.matches && n.matches("video, audio")) return n;
      }
      for (const n of path) {
        if (n && n.shadowRoot) {
          const v = n.shadowRoot.querySelector("video, audio");
          if (v) return v;
        }
      }
    }
    const direct = el.closest("video, audio");
    if (direct) return direct;
    const host = el.closest('[class*="player" i],[id*="player" i],[class*="video" i],[id*="video" i],[class*="jw" i],[data-player],[data-testid*="video" i],[data-testid*="player" i],faceplate-video-player,shreddit-post');
    if (host) return host.querySelector("video, audio");
    return null;
  }

  document.addEventListener("mouseover", (e) => {
    const overRdm = (btn && btn.contains(e.target)) || (menu && menu.contains(e.target));
    if (overRdm) {
      // any mouseleave on the way in armed the hide timer; a menu is useless
      // if it evaporates while the user is still choosing
      clearTimeout(hideTimer);
      return;
    }
    const el = mediaAt(e.target, e.composedPath && e.composedPath());
    if (!el) {
      scheduleHide();
      return;
    }
    if (el.srcObject && !urlOf(el)) {
      // live camera / WebRTC stream, not a downloadable file
      scheduleHide();
      return;
    }
    const url = urlOf(el);
    if (!url) {
      // blob:/data: MSE streams cannot be re-fetched directly; offer the page
      // itself so the daemon can resolve it via yt-dlp (any site).
      show(el, "", true);
      return;
    }
    show(el, url, false);
  }, true);

  document.addEventListener("mouseleave", scheduleHide, true);
})();
