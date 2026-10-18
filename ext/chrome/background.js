// RDM Integration - MV3 background service worker.
// M2: header-based interception (webRequest) + floating download button + panel feed.

const HOST_NAME = "com.rdm.host";
const MIN_SIZE = 1024 * 1024;
const SEEN_TTL = 10 * 60 * 1000;

let port = null;

function ensurePort() {
  if (!port) {
    port = chrome.runtime.connectNative(HOST_NAME);
    port.onMessage.addListener((m) => console.log("rdm:", m));
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

async function offer(url, filename, referer) {
  const cookie = await cookieHeader(url);
  ensurePort().postMessage({
    t: "offer",
    url,
    filename: filename || nameFromUrl(url),
    referer: referer || "",
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
      title: "Unduh dengan RDM",
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
  }
  return false;
});

// ---- interception: response header heuristics ----

const CANDIDATE_TYPE = /^(video\/|audio\/|application\/(zip|x-rar-compressed|x-7z-compressed|octet-stream|x-iso9660-image|pdf))/i;

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
      if (seenRecently(details.url)) return;

      const name = cdFilename(cd) || nameFromUrl(details.url);
      showFab(details.tabId, details.url, name, len).catch(() => {});
    } catch (e) {
      console.warn("rdm: onHeadersReceived", e);
    }
  },
  { urls: ["<all_urls>"], types: ["main_frame", "sub_frame", "xmlhttprequest", "media", "other"] },
  ["responseHeaders"]
);

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
    b.textContent = "✓ dikirim ke RDM";
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
