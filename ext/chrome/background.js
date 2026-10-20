// RDM Integration - MV3 background service worker.
// M2: header-based interception (webRequest) + floating download button + panel feed.

const HOST_NAME = "com.rdm.host";
const MIN_SIZE = 1024 * 1024;
const SEEN_TTL = 10 * 60 * 1000;

let port = null;
let intercept = true; // takeover: cancel native downloads, route to RDM
let interceptExt = null; // null/[] = every type; array = extension allow-list

// diagnostics -> daemon journal (journalctl --user -u rdm-daemon -t ext)
function dlog(msg) {
  try { ensurePort().postMessage({ t: "log", msg }); } catch {}
}

chrome.storage.local.get({ intercept: true, interceptExt: null }, (s) => {
  intercept = !!s.intercept;
  interceptExt = Array.isArray(s.interceptExt) ? s.interceptExt : null;
});
chrome.storage.onChanged.addListener((ch, area) => {
  if (area !== "local") return;
  if (ch.intercept) intercept = !!ch.intercept.newValue;
  if (ch.interceptExt) interceptExt = Array.isArray(ch.interceptExt.newValue) ? ch.interceptExt.newValue : null;
});

function extOf(name, url) {
  let m = /\.([a-z0-9]{1,8})$/i.exec(name || "");
  if (!m) {
    try { m = /\.([a-z0-9]{1,8})$/i.exec(new URL(url).pathname); } catch {}
  }
  return m ? m[1].toLowerCase() : "";
}

// allow-list from RDM settings; when a filter is set, ONLY those extensions
// take over - extension-less URLs (google drive, signed CDN endpoints) stay
// with the browser instead of hijacking every unknown download
function extAllowed(name, url) {
  if (!Array.isArray(interceptExt) || !interceptExt.length) return true;
  const e = extOf(name, url);
  return !!e && interceptExt.includes(e);
}

function ensurePort() {
  if (!port) {
    port = chrome.runtime.connectNative(HOST_NAME);
    port.onMessage.addListener((m) => {
      if (m && m.t === "settings" && m.settings) {
        const v = Array.isArray(m.settings.intercept_ext) ? m.settings.intercept_ext : null;
        if (JSON.stringify(v) !== JSON.stringify(interceptExt)) {
          interceptExt = v;
          chrome.storage.local.set({ interceptExt: v });
        }
      }
    });
    port.onDisconnect.addListener(() => {
      if (chrome.runtime.lastError) console.warn("rdm: host disconnect:", chrome.runtime.lastError.message);
      port = null;
    });
  }
  return port;
}

async function cookieHeader(url) {
  try {
    const cs = await chrome.cookies.getAll({ url });
    return cs.map((c) => `${c.name}=${c.value}`).join("; ");
  } catch {
    return "";
  }
}

function nameFromUrl(url) {
  try {
    const p = new URL(url).pathname.split("/").filter(Boolean).pop() || "";
    return decodeURIComponent(p);
  } catch {
    return "";
  }
}

function cdFilename(cd) {
  const m = /filename\*?=(?:UTF-8'')?["']?([^"';]+)/i.exec(cd || "");
  return m ? decodeURIComponent(m[1]) : "";
}

async function offer(url, filename, referer, confirm) {
  const cookie = await cookieHeader(url);
  ensurePort().postMessage({
    t: "offer",
    url,
    filename: filename || nameFromUrl(url),
    referer: referer || "",
    cookie,
    ua: navigator.userAgent,
    segments: 8,
    confirm: !!confirm,
  });
}

// Page-level offer: resolve the page via yt-dlp and download the best media.
async function pageOffer(url, height, audio) {
  const cookie = await cookieHeader(url);
  ensurePort().postMessage({
    t: "offer-page",
    url,
    page: true,
    height: height || 1080,
    audio_only: !!audio,
    referer: url,
    cookie,
    ua: navigator.userAgent,
    segments: 8,
  });
}

// ---- context menu (M1) ----

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: "rdm-download",
      title: "Download with RDM",
      contexts: ["link", "video", "audio", "image"],
    });
  });
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  const url = info.linkUrl || info.srcUrl;
  if (!url || !/^https?:\/\//i.test(url)) return;
  await offer(url, nameFromUrl(url), info.pageUrl || (tab && tab.url) || "");
});

// ---- runtime messages: floating button clicks ----

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg && msg.t === "fab-offer" && msg.url) {
    offer(msg.url, msg.name || "", sender.tab ? sender.tab.url : "").catch(console.warn);
    sendResponse({ ok: true });
  } else if (msg && msg.t === "page-offer" && msg.url) {
    pageOffer(msg.url, msg.height, msg.audio).catch(console.warn);
    sendResponse({ ok: true });
  }
  return false;
});

// ---- interception: response header heuristics ----

const CANDIDATE_TYPE = /^(video\/|audio\/|application\/(zip|x-rar-compressed|x-7z-compressed|octet-stream|x-iso9660-image|pdf))/i;
// Types Chrome will NOT render inline: navigation to these always becomes a
// native download, which takeover handles. Inline-renderable (video/audio/pdf)
// still gets the floating button.
const NON_RENDERABLE = /^application\/(zip|x-rar-compressed|x-7z-compressed|octet-stream|x-iso9660-image)/i;

const seen = new Map(); // url -> ts

function seenRecently(url) {
  const now = Date.now();
  for (const [u, ts] of seen) {
    if (now - ts > SEEN_TTL) seen.delete(u);
  }
  if (seen.has(url)) return true;
  seen.set(url, now);
  return false;
}

chrome.webRequest.onHeadersReceived.addListener(
  (details) => {
    try {
      if (details.tabId < 0 || details.statusCode >= 300) return;
      const h = {};
      for (const { name, value } of details.responseHeaders || []) h[name.toLowerCase()] = value;
      const ct = h["content-type"] || "";
      const cd = h["content-disposition"] || "";
      const len = parseInt(h["content-length"] || "0", 10);

      const attachment = /attachment/i.test(cd);
      if (!attachment) {
        if (/text\/html/i.test(ct)) return; // page navigation
        if (!CANDIDATE_TYPE.test(ct)) return;
        if (len > 0 && len < MIN_SIZE) return;
      }
      // With takeover enabled, downloads Chrome will start natively are routed
      // via downloads.onCreated instead - no button needed for those.
      if (intercept && (attachment || NON_RENDERABLE.test(ct))) return;
      if (seenRecently(details.url)) return;

      const name = cdFilename(cd) || nameFromUrl(details.url);
      if (!extAllowed(name, details.url)) return; // respect the Files type filter
      showFab(details.tabId, details.url, name, len).catch(() => {});
    } catch (e) {
      console.warn("rdm: onHeadersReceived", e);
    }
  },
  { urls: ["<all_urls>"], types: ["main_frame", "sub_frame", "xmlhttprequest", "media", "other"] },
  ["responseHeaders"]
);

// ---- takeover: cancel native download, route to RDM ----

// Chrome often fires onCreated before the target filename is known
// (google drive's extension-less download URLs, signed CDN links).
// Park those and decide once onChanged reports the real filename.
const pendingName = new Map(); // download id -> {url, referrer, timer}

chrome.downloads.onChanged.addListener((delta) => {
  const p = pendingName.get(delta.id);
  if (!p) return;
  if (delta.filename && delta.filename.current) {
    pendingName.delete(delta.id);
    clearTimeout(p.timer);
    const name = delta.filename.current.split(/[\\/]/).pop() || "";
    dlog(`resolved id=${delta.id} name="${name}"`);
    if (!extAllowed(name, p.url)) return; // not in the allow-list: keep it native
    (async () => {
      try {
        await chrome.downloads.cancel(delta.id).catch(() => {});
        await chrome.downloads.erase({ id: delta.id }).catch(() => {});
        console.log("rdm: takeover (late name)", name);
        await offer(p.url, name, p.referrer || "", true);
      } catch (e) {
        console.warn("rdm: late takeover failed", e);
      }
    })();
  } else if (delta.state && delta.state.current !== "in_progress") {
    pendingName.delete(delta.id); // finished/interrupted before a name arrived
    clearTimeout(p.timer);
  }
});

chrome.downloads.onCreated.addListener(async (item) => {
  try {
    if (!intercept) return;
    if (item.state === "complete") return; // finished before we saw it
    const url = item.finalUrl || item.url;
    if (!/^https?:/i.test(url)) return;
    if (url.startsWith("blob:") || url.startsWith("data:")) return;

    const name = (item.filename || "").split(/[\\/]/).pop() || nameFromUrl(url);
    if (!extAllowed(name, url)) {
      dlog(`skip name="${name}" ext=${JSON.stringify(extOf(name, url))} allowed=${JSON.stringify(interceptExt)} url=${url.slice(0, 120)}`);
      // no extension anywhere yet (drive.usercontent.google.com & co):
      // wait for Chrome to resolve the filename, then decide
      if (!extOf(name, url) && !pendingName.has(item.id)) {
        const timer = setTimeout(() => pendingName.delete(item.id), 60000);
        pendingName.set(item.id, { url, referrer: item.referrer || "", timer });
        dlog(`park id=${item.id} awaiting filename`);
      }
      return; // not in the RDM allow-list: let the browser handle it
    }

    await chrome.downloads.cancel(item.id).catch(() => {});
    await chrome.downloads.erase({ id: item.id }).catch(() => {});

    console.log("rdm: takeover", name);
    await offer(url, name, item.referrer || "", true);
  } catch (e) {
    console.warn("rdm: takeover failed", e);
  }
});

// ---- floating button injection ----

async function showFab(tabId, url, name, len) {
  const tab = await chrome.tabs.get(tabId);
  if (!/^https?:/.test(tab.url || "")) return;
  await chrome.scripting.executeScript({
    target: { tabId },
    args: [url, name || "file", len],
    func: injectFab,
  });
}

function injectFab(url, name, len) {
  const old = document.getElementById("__rdm_fab");
  if (old) old.remove();
  const fmt = (n) =>
    n >= 1 << 30 ? (n / (1 << 30)).toFixed(1) + " GB"
    : n >= 1 << 20 ? (n / (1 << 20)).toFixed(1) + " MB"
    : n >= 1 << 10 ? (n / (1 << 10)).toFixed(0) + " KB"
    : n + " B";
  const host = document.createElement("div");
  host.id = "__rdm_fab";
  host.style.cssText = "all:initial;position:fixed;z-index:2147483647;right:24px;bottom:24px;";
  const root = host.attachShadow({ mode: "closed" });
  const b = document.createElement("button");
  b.textContent = "⬇ RDM · " + name + (len ? " (" + fmt(len) + ")" : "");
  b.style.cssText = [
    "all:initial",
    "font:13px/1.4 system-ui,sans-serif",
    "background:#1a73e8",
    "color:#fff",
    "border:0",
    "border-radius:8px",
    "padding:10px 14px",
    "box-shadow:0 4px 16px rgba(0,0,0,.35)",
    "cursor:pointer",
    "max-width:340px",
    "overflow:hidden",
    "text-overflow:ellipsis",
    "white-space:nowrap",
    "display:block",
  ].join(";");
  b.onmouseenter = () => (b.style.filter = "brightness(1.1)");
  b.onmouseleave = () => (b.style.filter = "none");
  b.onclick = () => {
    chrome.runtime.sendMessage({ t: "fab-offer", url, name, page: location.href });
    b.textContent = "✓ sent to RDM";
    setTimeout(() => host.remove(), 1200);
  };
  const dismiss = document.createElement("div");
  dismiss.textContent = "✕";
  dismiss.style.cssText = [
    "all:initial",
    "position:absolute",
    "top:-8px",
    "right:-8px",
    "width:18px",
    "height:18px",
    "border-radius:50%",
    "background:#444",
    "color:#fff",
    "font:11px/18px system-ui,sans-serif",
    "text-align:center",
    "cursor:pointer",
  ].join(";");
  dismiss.onclick = () => host.remove();
  const wrap = document.createElement("div");
  wrap.style.cssText = "all:initial;position:relative";
  wrap.append(b, dismiss);
  root.append(wrap);
  document.documentElement.append(host);
  setTimeout(() => host.remove(), 60_000);
}
