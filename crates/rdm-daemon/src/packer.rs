//! Dean Edwards packer (`eval(function(p,a,c,k,e,d){...})`) unpacker.
//!
//! A few video hosts (lulustream / luluvido, ...) have no yt-dlp or
//! gallery-dl support at all, but embed the HLS playlist inside a packed
//! script. When both extractors fail, `extract` fetches the page, unpacks
//! the payload by plain token substitution and pulls the master playlist
//! URL out of the result — no JS runtime required.

use anyhow::Result;
use reqwest::header::{COOKIE, REFERER};
use std::time::Duration;

/// The host rejects plain curl-style requests with 403; a browser UA is
/// enough (no referer or cookie needed).
const BROWSER_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Locate the packed payload in `html` and return the unpacked script.
pub fn unpack(html: &str) -> Option<String> {
    let start = html.find("eval(function(p,a,c,k,e,d)")?;
    let rest = &html[start..];
    // payload starts right after the closing brace of the packer function
    let p_open = rest.find("}('")? + 3;
    // payload ends at the quote that opens the `,RADIX,COUNT,'` tail —
    // quotes escaped as \' inside the payload are skipped
    let mut p_end = None;
    let mut i = p_open;
    let b = rest.as_bytes();
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
            continue;
        }
        if b[i] == b'\'' && tail_is_radix_count(&rest[i + 2..]) {
            p_end = Some(i);
            break;
        }
        i += 1;
    }
    let p_end = p_end?;
    let payload = &rest[p_open..p_end];
    // `RADIX,COUNT,'TOKENS'.split('|')` — skip the `,'` first
    let tail = &rest[p_end + 2..];
    let c1 = tail.find(',')?;
    let radix: u32 = tail[..c1].parse().ok()?;
    let tail = &tail[c1 + 1..];
    let c2 = tail.find(',')?;
    let count: usize = tail[..c2].parse().ok()?;
    let tail = &tail[c2 + 1..];
    if !(2..=36).contains(&radix) || !tail.starts_with('\'') {
        return None;
    }
    let k_body = &tail[1..];
    let k_end = k_body.find('\'')?;
    if !k_body[k_end + 1..].starts_with(".split('") {
        return None;
    }
    let tokens: Vec<&str> = k_body[..k_end].split('|').collect();
    let count = count.min(tokens.len());
    let mut out = payload.to_string();
    // same order as the original packer: while(c--) — highest token first
    for n in (0..count).rev() {
        if tokens[n].is_empty() {
            continue;
        }
        let word = encode_base(n, radix);
        out = replace_word(&out, &word, tokens[n]);
    }
    Some(out)
}

/// `s` starts right after a quote; true when the text begins with
/// `,RADIX,COUNT,'` (numbers, commas, opening quote of the token list).
fn tail_is_radix_count(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    for group in 0..2 {
        let d = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == d {
            return false;
        }
        if group == 1 {
            return i + 1 < b.len() && b[i] == b',' && b[i + 1] == b'\'';
        }
        if i >= b.len() || b[i] != b',' {
            return false;
        }
        i += 1;
    }
    false
}

/// JS `\w`: [A-Za-z0-9_]. `$` is NOT a word character for `\b`.
fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Replace whole-word occurrences of `word` (JS `RegExp('\\b'+word+'\\b','g')`
/// semantics). Packed payloads are ASCII, so byte scanning is safe here.
fn replace_word(hay: &str, word: &str, repl: &str) -> String {
    if word.is_empty() {
        return hay.to_string();
    }
    let b = hay.as_bytes();
    let w = word.as_bytes();
    let mut out = String::with_capacity(hay.len());
    let mut last = 0usize;
    let mut search = 0usize;
    while let Some(rel) = hay[search..].find(word) {
        let s = search + rel;
        let e = s + w.len();
        let before_ok = s == 0 || !is_word_byte(b[s - 1]);
        let after_ok = e >= b.len() || !is_word_byte(b[e]);
        if before_ok && after_ok {
            out.push_str(&hay[last..s]);
            out.push_str(repl);
            last = e;
        } else {
            // not a whole word: copy through the first byte, keep scanning
            out.push_str(&hay[last..s + 1]);
            last = s + 1;
        }
        search = s + 1;
    }
    out.push_str(&hay[last..]);
    out
}

/// JS `Number.prototype.toString(radix)`, lowercase digits.
fn encode_base(mut n: usize, radix: u32) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".to_string();
    }
    let mut s = Vec::new();
    while n > 0 {
        s.push(DIGITS[n % radix as usize]);
        n /= radix as usize;
    }
    s.reverse();
    String::from_utf8(s).expect("ascii digits")
}

/// First (master preferred) playlist URL in the unpacked script.
pub fn find_m3u8(js: &str) -> Option<String> {
    let mut best: Option<String> = None;
    let mut i = 0usize;
    while let Some(rel) = js[i..].find("http") {
        let s = i + rel;
        i = s + 4;
        if !js[s..].starts_with("http://") && !js[s..].starts_with("https://") {
            continue;
        }
        let end = js[s..]
            .find(|c: char| c == '"' || c == '\'' || c.is_whitespace() || c == '<' || c == ')')
            .map(|p| s + p)
            .unwrap_or(js.len());
        let url = js[s..end].replace("\\/", "/");
        if url.contains(".m3u8") {
            if url.contains("master") {
                return Some(url);
            }
            best = best.or(Some(url));
        }
    }
    best
}

/// Page `<title>`, trimmed of the usual ` - Sitename` / `.mp4` suffixes.
pub fn page_title(html: &str) -> Option<String> {
    let t = html.find("<title")? + "<title".len();
    let t = html[t..].find('>')? + t + 1;
    let e = html[t..].find("</title")? + t;
    let raw = html[t..e].trim().replace("&amp;", "&");
    if raw.is_empty() {
        return None;
    }
    let clean = raw
        .split(" - ")
        .next()
        .unwrap_or(&raw)
        .trim()
        .trim_end_matches(".mp4")
        .trim();
    if clean.is_empty() {
        None
    } else {
        Some(clean.to_string())
    }
}

/// Fetch `page`, unpack its packed script and return the playlist URL plus
/// a filename hint. `Ok(None)` = nothing packed (or no playlist inside).
pub async fn extract(
    page: &str,
    ua: Option<&str>,
    cookie: Option<&str>,
) -> Result<Option<(String, String)>> {
    let client = reqwest::Client::builder()
        .user_agent(ua.filter(|u| !u.is_empty()).unwrap_or(BROWSER_UA))
        .timeout(Duration::from_secs(20))
        .build()?;
    let mut req = client.get(page).header(REFERER, page);
    if let Some(c) = cookie.filter(|c| !c.is_empty()) {
        req = req.header(COOKIE, c);
    }
    let html = req.send().await?.error_for_status()?.text().await?;
    let Some(js) = unpack(&html) else {
        return Ok(None);
    };
    let Some(m3u8) = find_m3u8(&js) else {
        return Ok(None);
    };
    let title = page_title(&html).unwrap_or_else(|| "video".to_string());
    Ok(Some((m3u8, title)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SYNTHETIC: &str = r#"<html><head><title>My Cool Video - Lulustream.mp4 - LuluStream</title></head><body><script type="text/javascript">eval(function(p,a,c,k,e,d){while(c--)if(k[c])p=p.replace(new RegExp('\\b'+c.toString(a)+'\\b','g'),k[c]);return p}('l(\'3\').4({5:[{2:"1://6.7/8/9_a/master.m3u8?t=x"}]})',16,10,'|https|file|jw|setup|sources|cdn|host|vid|name9'.split('|'),0,{}))</script></body></html>"#;

    // real payload captured from luluvido.com/e/zew39mrhpir6 (stream token
    // long expired — fixture only)
    const REAL: &str = r#"eval(function(p,a,c,k,e,d){while(c--)if(k[c])p=p.replace(new RegExp('\\b'+c.toString(a)+'\\b','g'),k[c]);return p}('l("39").8a({89:[{2b:"15://88.87.1x/86/85/84/82/81.80?t=7z-7y&s=3s&e=7x&f=3t&i=0.3&7w=0"}],7v:"15://7u.7t.1j/7s.7r",20:"13%",1z:"13%",7q:{1y:"/b/1k/1k-3z.21",19:"1k-3z"},7p:"7o",7n:"7m.46",7l:\'7k\',7j:"1d",h:[{2b:"/3y/3m.3y",7i:"7h 1e",7g:"1e"}],1e:{7f:1,7e:\'#7d\',7c:\'#7b\',7a:"79",77:30,76:\'13\',},\'75\':{"74":"73"},71:"70",6z:"15://2a.1j",3x:{2b:"15://2a.1j/b/1k/b-3x.n",6y:"15://2a.1j",u:"6x-6w",3f:"5"}});m 28,29;m 6v=0,6u=0;m b=l();m 3w=0,6t=0,6s=0,q=0;$.6r({6q:{\'6p-6o\':\'3d-6n\'}});b.o(\'2k\',c(x){g(5>0&&x.u>=5&&29!=1){29=1;$(\'r.6m\').6l(\'6k\')}g(x.u>=q+5||x.u<q){q=x.u;27.6j(\'26\',6i.6h(q),{6g:60*60*24*7})}});b.o(\'1g\',c(x){3w=x.u});b.o(\'3b\',c(x){3v(x)});b.o(\'6f\',c(){$(\'r.3u\').6e();27.3c(\'26\')});b.o(\'3l\',c(x){});c 3v(x){$(\'r.3u\').1u();$(\'#6d\').1u();g(28)1m;28=1;1i=0;g(6b.6a===69){1i=1}$.3q(\'/68?3k=67&3j=3i&66=3t-65-64-3s-63&62=1&61=&1i=\'+1i,c(3r){$(\'#5z\').5y(3r)});m q=27.3q(\'26\');g(q>0){l().1g(q)}}c 5x(){m h=b.1n(3p);3o.3n(h);g(h.y>1){2e(i=0;i<h.y;i++){g(h[i].19==3p){3o.3n(\'!!=\'+i);b.2c(i)}}}}b.o(\'5w\',c(){b.o(\'5v\',c(25){g(5u(\'3m\').5t(25.h[25.5s].5r)){l().3l(1d);l().5q(0);3h(\'/?3k=5p&3j=3i\')}});c 3h(1y){m $1h=$("<r />").21({u:"5o",20:"13%",1z:"13%",5n:0,3e:0,3g:5m,5l:"5k(10%, 10%, 10%, 0.4)","2j-5j":"5i"});$("<5h />").21({20:"60%",1z:"60%",3g:5g,"3f-3e":"5f"}).5e({\'5d\':1y,\'5c\':\'0\',\'5b\':\'3d\'}).3a($1h);$1h.5a(c(){$(59).3c();l().3b()});$1h.3a($(\'#39\'))}l().1t(\'<n 37="36://35.33.1x/31/n" 2z="a-n-z" 2y="0 0 2x 2w"><2v d="58.5 2u.6c-9.5 7.9-22.8 9.7-34.1 4.57 56.4 0 55 83.6 7.2 72.3 18.4 54.5-3.6 34.1 4.53 52 2t-17.7 14.3-32 32-2s 14.3 32 51 17.7-14.3 32-32 50-32-14.3-32-4z-11.5 9.6-1w 2r"/></n>\',"4y 10 2q",c(){l().1g(l().2p()+10)},"38");$("r[2o=38]").2m().2l(\'.a-z-1v\');l().1t(\'<n 37="36://35.33.1x/31/n" 2z="a-n-z" 2y="0 0 2x 2w"><2v d="4x.5 2u.4w.5 7.9 22.8 9.7 34.1 4.4v.4-16.6 18.4-4u-12.4-7.2-23.7-18.4-4t-24.5-3.6-34.1 4.4l-1w 4s 2t-17.7-14.3-32-32-4r 78.3 0 4q 17.7 14.3 32 32 2s-14.3 32-4p.5 9.6 1w 2r"/></n>\',"4o 10 2q",c(){m 1f=l().2p()-10;g(1f<0)1f=0;l().1g(1f)},"2n");$("r[2o=2n]").2m().2l(\'.a-z-1v\');$("r.a-z-1v").1u();$(\'.a-4n-2k\').4m($(\'.a-2j-4k\'))});b.o("p",c(1a){m h=b.1n();g(h.y<2)1m;$(\'.a-k-4j-4i\').4h(c(){$(\'#a-k-j-p\').1q(\'a-k-j-1b\');$(\'.a-j-p\').w(\'v-1r\',\'1c\')});b.1t("/4g/4f.n","4e 4d",c(){$(\'.a-2i\').4c(\'a-k-2h\');$(\'.a-k-1e, .a-k-4b\').w(\'v-1s\',\'1c\');g($(\'.a-2i\').4a(\'a-k-2h\')){$(\'.a-j-p\').w(\'v-1s\',\'1d\');$(\'.a-j-p\').w(\'v-1r\',\'1d\');$(\'.a-k-j-49\').1q(\'a-k-j-1b\');$(\'.a-k-j-p\').48(\'a-k-j-1b\')}47{$(\'.a-j-p\').w(\'v-1s\',\'1c\');$(\'.a-j-p\').w(\'v-1r\',\'1c\');$(\'.a-k-j-p\').1q(\'a-k-j-1b\')}},"45");b.o("44",c(1a){1p.43(\'1o\',1a.h[1a.42].19)});g(1p.2g(\'1o\')){41("2f(1p.2g(\'1o\'));",40)}});m 1l;c 2f(2d){m h=b.1n();g(h.y>1){2e(i=0;i<h.y;i++){g(h[i].19==2d){g(i==1l){1m}1l=i;b.2c(i)}}}}',36,299,'||||||||||jw|player|function||||if|tracks||submenu|settings|jwplayer|var|svg|on|audioTracks|lastt|div|||position|aria|attr||length|icon||||100||https||||name|event|active|false|true|captions|tt|seek|dd|adb|com|jw8|current_audio|return|getAudioTracks|default_audio|localStorage|removeClass|expanded|checked|addButton|hide|rewind|192|org|url|height|width|css||||tr|tt206okjlrydqt|ls|vvplay|vvad|lulustream|file|setCurrentAudioTrack|audio_name|for|audio_set|getItem|open|controls|text|time|insertAfter|detach|ff00|button|getPosition|sec|160z|32s32|241V96c0|440|path|512|320|viewBox|class||2000||w3||www|http|xmlns|ff11|vplayer|appendTo|play|remove|no|top|margin|zIndex|openIframeOverlay|206okjlrydqt|file_code|op|pause|empty|log|console|track_name|get|data|1792536961|22511703|video_ad|doPlay|prevt|logo|srt|theme|300|setTimeout|currentTrack|setItem|audioTrackChanged|dualSound||else|addClass|quality|hasClass|playbackRates|toggleClass|Track|Audio|dualy|images|mousedown|buttons|topbar|countdown||append|slider|Rewind|32V271l11|96V416c0|32S0|160L64|29s|29V96c0|4s18|6c9|M267|Forward|32V271l|32s|32V416c0|160L256|4l192|67s24|416V96C0|428|4S0|M52|this|click|scrolling|frameborder|src|prop|50px|1000001|iframe|center|align|rgba|background|1000000|left|absolute|upload_srt|setCurrentCaptions|id|track|test|RegExp|captionsChanged|ready|set_audio_track|html|fviews||referer|embed|5447ba5d5f2730a7e9d6ebb24ffb62cc|99|118|hash|view|dl|undefined|cRAds|window||over_player_msg|show|complete|ttl|round|Math|set|slow|fadeIn|video_ad_fadein|cache|Cache|Content|headers|ajaxSetup|v2done|tott|vastdone2|vastdone1|bar|control|link|aboutlink|LuluStream|abouttext||HD|2371|qualityLabels|fontOpacity|backgroundOpacity||Tahoma|fontFamily|303030|backgroundColor|FFFFFF|color|userFontScale|kind|Upload|label|androidhls|none|preload|2762|duration|uniform|stretching|skin|jpg|206okjlrydqt_xt|lulucdn|img|image|sp|28800|SzwPpbx7CqBTzbPRtwSBY|dU0E6nSQUSTFtjxXNSVr2|m3u8|master|206okjlrydqt_h||04502|02|hls2|tnmr|fbevr1wmczl7|sources|setup'.split('|')))"#;

    #[test]
    fn unpacks_synthetic() {
        let js = unpack(SYNTHETIC).expect("packed script found");
        // escaped quotes inside the payload stay escaped — the browser's
        // eval unescapes them; we only need the URLs
        assert!(js.contains(r"l(\'jw\')"), "js = {js}");
        let url = find_m3u8(&js).expect("m3u8 found");
        // `9_a` is one JS word, so token 9 must NOT be substituted inside it
        assert_eq!(url, "https://cdn.host/vid/9_a/master.m3u8?t=x");
        assert_eq!(page_title(SYNTHETIC).as_deref(), Some("My Cool Video"));
    }

    #[test]
    fn unpacks_real_luluvido_payload() {
        let js = unpack(REAL).expect("packed script found");
        assert!(js.contains(".setup({sources:"), "js = {js}");
        let url = find_m3u8(&js).expect("m3u8 found");
        assert_eq!(
            url,
            "https://fbevr1wmczl7.tnmr.org/hls2/02/04502/206okjlrydqt_h/master.m3u8?t=dU0E6nSQUSTFtjxXNSVr2-SzwPpbx7CqBTzbPRtwSBY&s=1792536961&e=28800&f=22511703&i=0.3&sp=0"
        );
    }

    // real payload captured from sexkbj.top/embed/wfy6g9kacpgd — same
    // player family, different token layout than the luluvido one
    #[test]
    fn unpacks_real_sexkbj_payload() {
        let html = include_str!("../tests/fixtures/sx_packed.js");
        let js = unpack(html).expect("packed script found");
        let url = find_m3u8(&js).expect("m3u8 found");
        assert!(url.contains("/hls2/"), "url = {url}");
        assert!(
            url.ends_with("master.m3u8") || url.contains("master.m3u8?"),
            "url = {url}"
        );
    }

    #[test]
    fn plain_html_is_none() {
        assert!(unpack("<html><body>nothing packed</body></html>").is_none());
        assert!(find_m3u8("var a = 'https://x/y.mp4';").is_none());
        assert!(page_title("<html><title>  </title></html>").is_none());
    }

    #[test]
    fn base_encoding_matches_js() {
        assert_eq!(encode_base(0, 36), "0");
        assert_eq!(encode_base(9, 36), "9");
        assert_eq!(encode_base(10, 36), "a");
        assert_eq!(encode_base(35, 36), "z");
        assert_eq!(encode_base(36, 36), "10");
        assert_eq!(encode_base(255, 16), "ff");
    }
}
