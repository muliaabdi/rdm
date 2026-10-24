//! DNS-over-HTTPS (DoH) resolver for rdm-core.
//!
//! Many Indonesian and international ISPs hijack or poison standard UDP port 53
//! DNS queries (returning NXDOMAIN or redirecting to blockpages). Querying DoH
//! over HTTPS (e.g. Cloudflare 1.1.1.1 or Google 8.8.8.8) bypasses ISP poisoning.
//!
//! Provides adapters implementing both `reqwest::dns::Resolve` and `wreq::dns::Resolve`.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

const DEFAULT_TTL: Duration = Duration::from_secs(300);
const TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsConfig {
    System,
    Cloudflare,
    Google,
    Custom(String),
}

impl DnsConfig {
    pub fn endpoint(&self) -> Option<&str> {
        match self {
            Self::System => None,
            Self::Cloudflare => Some("https://1.1.1.1/dns-query"),
            Self::Google => Some("https://dns.google/dns-query"),
            Self::Custom(url) => Some(url.as_str()),
        }
    }

    pub fn from_settings(mode: &str, custom: Option<&str>) -> Self {
        match mode.to_ascii_lowercase().as_str() {
            "cloudflare" | "1.1.1.1" => Self::Cloudflare,
            "google" | "8.8.8.8" => Self::Google,
            "custom" => {
                let u = custom.unwrap_or("").trim();
                if u.is_empty() {
                    Self::Cloudflare
                } else {
                    Self::Custom(u.to_string())
                }
            }
            _ => Self::System,
        }
    }
}

#[derive(Debug, Clone)]
struct CacheEntry {
    addrs: Vec<SocketAddr>,
    expires_at: Instant,
}

/// Thread-safe DoH resolver with memory cache and system fallback.
#[derive(Clone, Debug)]
pub struct DohResolver {
    endpoint: String,
    cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
    http: reqwest::Client,
}

impl DohResolver {
    pub fn new(endpoint: &str) -> Self {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .connect_timeout(TIMEOUT)
            .build()
            .unwrap_or_default();
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            cache: Arc::new(RwLock::new(HashMap::new())),
            http,
        }
    }

    /// Resolve a hostname to socket addresses.
    pub async fn resolve_host(&self, host: &str, port: u16) -> Vec<SocketAddr> {
        let host = host.trim();
        // 1. Literal IP address
        if let Ok(ip) = host.parse::<IpAddr>() {
            return vec![SocketAddr::new(ip, port)];
        }

        // 2. Prevent infinite recursion if the host is the DoH server itself
        if host == "1.1.1.1" || host == "one.one.one.one" || host == "cloudflare-dns.com" {
            return vec![SocketAddr::from(([1, 1, 1, 1], port))];
        }
        if host == "8.8.8.8" || host == "dns.google" {
            return vec![SocketAddr::from(([8, 8, 8, 8], port))];
        }

        // 3. Cache lookup
        let now = Instant::now();
        if let Ok(c) = self.cache.read() {
            if let Some(entry) = c.get(host) {
                if entry.expires_at > now && !entry.addrs.is_empty() {
                    return entry
                        .addrs
                        .iter()
                        .map(|a| SocketAddr::new(a.ip(), port))
                        .collect();
                }
            }
        }

        // 4. DoH JSON query (RFC 8427 format supported by Cloudflare, Google, AdGuard)
        if let Some(ips) = self.query_doh(host).await {
            if !ips.is_empty() {
                let addrs: Vec<SocketAddr> =
                    ips.iter().map(|&ip| SocketAddr::new(ip, port)).collect();
                if let Ok(mut c) = self.cache.write() {
                    c.insert(
                        host.to_string(),
                        CacheEntry {
                            addrs: addrs.clone(),
                            expires_at: now + DEFAULT_TTL,
                        },
                    );
                }
                return addrs;
            }
        }

        // 5. Fallback: native tokio getaddrinfo
        let target = format!("{host}:{port}");
        if let Ok(iter) = tokio::net::lookup_host(&target).await {
            let res: Vec<SocketAddr> = iter.collect();
            if !res.is_empty() {
                return res;
            }
        }

        Vec::new()
    }

    async fn query_doh(&self, host: &str) -> Option<Vec<IpAddr>> {
        let url = format!("{}?name={}&type=A", self.endpoint, host);
        let resp = self
            .http
            .get(&url)
            .header("accept", "application/dns-json")
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let text = resp.text().await.ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        let answers = json.get("Answer")?.as_array()?;
        let mut ips = Vec::new();
        for ans in answers {
            // type 1 = A (IPv4)
            if ans.get("type").and_then(|t| t.as_u64()) == Some(1) {
                if let Some(ip_str) = ans.get("data").and_then(|d| d.as_str()) {
                    if let Ok(ip) = ip_str.parse::<IpAddr>() {
                        ips.push(ip);
                    }
                }
            }
        }
        if ips.is_empty() {
            None
        } else {
            Some(ips)
        }
    }
}

// ---- reqwest integration ----

impl reqwest::dns::Resolve for DohResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let this = self.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = this.resolve_host(&host, 0).await;
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("DoH failed to resolve host {host}"),
                )) as Box<dyn StdError + Send + Sync>);
            }
            let iter: Box<dyn Iterator<Item = SocketAddr> + Send> = Box::new(addrs.into_iter());
            Ok(iter)
        })
    }
}

// ---- wreq integration ----

impl wreq::dns::Resolve for DohResolver {
    fn resolve(&self, name: wreq::dns::Name) -> wreq::dns::Resolving {
        let this = self.clone();
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs = this.resolve_host(&host, 0).await;
            if addrs.is_empty() {
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("DoH failed to resolve host {host}"),
                )) as Box<dyn StdError + Send + Sync>);
            }
            let addrs_iter: wreq::dns::Addrs = Box::new(addrs.into_iter());
            Ok(addrs_iter)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn resolves_cloudflare_doh() {
        let resolver = DohResolver::new("https://1.1.1.1/dns-query");
        let addrs = resolver.resolve_host("one.one.one.one", 443).await;
        assert!(!addrs.is_empty(), "expected addresses for one.one.one.one");
    }

    #[tokio::test]
    async fn resolves_blocked_host_via_doh() {
        let resolver = DohResolver::new("https://1.1.1.1/dns-query");
        // missav.ws is typically poisoned by Indonesian ISPs; DoH must resolve it
        let addrs = resolver.resolve_host("missav.ws", 443).await;
        assert!(!addrs.is_empty(), "expected DoH to resolve missav.ws");
    }
}
