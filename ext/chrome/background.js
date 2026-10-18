// RDM Integration - MV3 background service worker.
// Context menu on links/media -> offer to rdm-host (native messaging).

const HOST_NAME = "com.rdm.host";
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
  const referer = info.pageUrl || (tab && tab.url) || "";
  const cookie = await cookieHeader(url);
  const message = {
    t: "offer",
    url,
    filename: nameFromUrl(url),
    referer,
    cookie,
    ua: navigator.userAgent,
    segments: 8,
  };
  ensurePort().postMessage(message);
});
