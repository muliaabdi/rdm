// Video overlay: IDM-style "Download with RDM" tooltip above <video>/<audio>.
// Runs in all frames; hover a playable media element to reveal.

(function () {
  if (window.__rdmVideoOverlay) return;
  window.__rdmVideoOverlay = true;

  let btn = null;
  let hideTimer = null;
  let current = null;
  let pageModeNow = false;

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

  function destroy() {
    if (btn) btn.remove();
    btn = null;
    current = null;
    window.removeEventListener("scroll", place, true);
    window.removeEventListener("resize", place);
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
  }

  function show(el, url, isPage) {
    clearTimeout(hideTimer);
    pageModeNow = !!isPage;
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
      </style>
      <span class="in">
        <svg viewBox="0 0 24 24" fill="none" stroke="#7db3f5" stroke-width="2.4"><path d="M12 4v11m0 0l-4-4m4 4l4-4M5 20h14"/></svg>
        <b id="__rdm_lbl">Download with RDM</b>
      </span>`;
      btn.addEventListener("click", (e) => {
        e.stopPropagation();
        e.preventDefault();
        const u = urlOf(current || {});
        const b = btn.shadowRoot.querySelector("#__rdm_lbl");
        if (pageModeNow) {
          b.textContent = "…";
          chrome.runtime.sendMessage({ t: "page-offer", url: location.href }, () => {
            const err = chrome.runtime.lastError;
            b.textContent = err ? "ERR" : "✓";
          });
          clearTimeout(hideTimer);
          hideTimer = setTimeout(destroy, 1200);
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
      document.documentElement.append(btn);
      window.addEventListener("scroll", place, true);
      window.addEventListener("resize", place);
    }
    current = el;
    const lbl = btn.shadowRoot.querySelector("#__rdm_lbl");
    if (lbl) {
      if (pageModeNow) {
        // keep the pill tiny on video pages: icon + "RDM"; the full text is the hover tooltip
        lbl.textContent = "RDM";
        btn.title = "Download this page video with RDM";
      } else {
        lbl.textContent = "Download with RDM";
        btn.title = "";
      }
    }
    btn.style.opacity = "1";
    place();
  }

  function scheduleHide() {
    clearTimeout(hideTimer);
    hideTimer = setTimeout(destroy, 350);
  }

  // Resolve the media element for a hovered node. Players (JWPlayer, video.js,
  // custom HTML5 players) usually stack transparent control layers on top of
  // the <video>, so the hover target is a wrapper - look for the media inside
  // a player-ish ancestor too. Shadow-DOM hosts retarget to the host element,
  // which `querySelector` can still see through for slotted content.
  function mediaAt(el) {
    if (!el || el.nodeType !== 1 || !el.closest) return null;
    const direct = el.closest("video, audio");
    if (direct) return direct;
    const host = el.closest('[class*="player" i],[id*="player" i],[class*="video" i],[id*="video" i],[class*="jw" i],[data-player]');
    if (host) return host.querySelector("video, audio");
    return null;
  }

  document.addEventListener("mouseover", (e) => {
    const el = mediaAt(e.target);
    if (!el) {
      if (btn && !btn.contains(e.target)) scheduleHide();
      return;
    }
    if (el.srcObject && !urlOf(el)) {
      // live camera / WebRTC stream, not a downloadable file
      if (btn && !btn.contains(e.target)) scheduleHide();
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
