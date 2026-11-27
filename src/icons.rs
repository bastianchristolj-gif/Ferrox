//! Vraies icônes des sites (raccourcis de la page d'accueil, sites les plus visités).
//!
//! Beaucoup de sites n'ont pas de `/favicon.ico` à la racine (Discord, Epic Games…) :
//! on lit donc les icônes déclarées dans la page (`<link rel="icon">`,
//! `apple-touch-icon`…), on choisit la plus nette, on la télécharge directement
//! depuis le site (aucun service tiers) et on la garde en cache 30 jours.

use base64::Engine;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const MAX_PAGE: u64 = 1024 * 1024;
const MAX_ICON: u64 = 300 * 1024;
const TTL: Duration = Duration::from_secs(30 * 86_400);

pub fn host_key(url: &str) -> String {
    crate::blocker::host_of(url).unwrap_or("").trim_start_matches("www.").to_ascii_lowercase()
}

fn cache_path(host: &str) -> Option<PathBuf> {
    let ok = !host.is_empty() && host.len() < 200 && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    ok.then(|| crate::extensions::ext_dir().with_file_name("icons").join(format!("{host}.txt")))
}

/// Icône en cache : (data URL, encore fraîche ?).
pub fn cached(host: &str) -> Option<(String, bool)> {
    let p = cache_path(host)?;
    let data = std::fs::read_to_string(&p).ok()?;
    if !data.starts_with("data:image/") {
        return None;
    }
    let fresh = std::fs::metadata(&p)
        .and_then(|m| m.modified())
        .map(|t| SystemTime::now().duration_since(t).unwrap_or_default() < TTL)
        .unwrap_or(false);
    Some((data, fresh))
}

pub fn store(host: &str, data_url: &str) {
    if let Some(p) = cache_path(host) {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, data_url);
    }
}

// ------------------------------------------------------------------ analyse

fn attr<'a>(tag: &'a str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find(name) {
        let i = from + pos;
        from = i + name.len();
        // le nom doit être un attribut entier (précédé d'un espace, suivi de « = »)
        if i == 0 || !lower.as_bytes()[i - 1].is_ascii_whitespace() {
            continue;
        }
        let rest = tag[i + name.len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        let val = match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next()?,
            _ => rest.split(|c: char| c.is_whitespace() || c == '>').next()?,
        };
        return Some(val.replace("&amp;", "&").trim().to_string());
    }
    None
}

/// Résout une adresse relative par rapport à la page.
pub fn resolve(base: &str, href: &str) -> Option<String> {
    let href = href.trim();
    if href.starts_with("data:image/") {
        return Some(href.to_string());
    }
    if href.starts_with("https://") || href.starts_with("http://") {
        return Some(href.to_string());
    }
    let scheme_end = base.find("://")? + 3;
    let host_end = base[scheme_end..].find('/').map(|i| scheme_end + i).unwrap_or(base.len());
    let origin = &base[..host_end];
    if let Some(rest) = href.strip_prefix("//") {
        return Some(format!("https://{rest}"));
    }
    if href.starts_with('/') {
        return Some(format!("{origin}{href}"));
    }
    if href.is_empty() || href.starts_with('#') || href.contains(':') {
        return None;
    }
    let path = &base[host_end..];
    let path = path.split(['?', '#']).next().unwrap_or("");
    let dir = path.rfind('/').map(|i| &path[..=i]).unwrap_or("/");
    Some(format!("{origin}{dir}{href}"))
}

/// Icônes déclarées par la page, de la plus nette à la moins nette.
pub fn parse_icons(html: &str, base: &str) -> Vec<String> {
    let head = &html[..html.len().min(400_000)];
    let lower = head.to_ascii_lowercase();
    let end = lower.find("</head>").unwrap_or(lower.len());
    let mut found: Vec<(u32, String)> = Vec::new();
    let mut from = 0;
    while let Some(pos) = lower[from..end].find("<link") {
        let i = from + pos;
        let Some(close) = lower[i..end].find('>') else { break };
        let tag = &head[i..i + close];
        from = i + close;
        let rel = attr(tag, "rel").unwrap_or_default().to_ascii_lowercase();
        if !rel.split_whitespace().any(|r| r == "icon" || r.starts_with("apple-touch-icon")) {
            continue; // (mask-icon = silhouette monochrome : ignorée)
        }
        let Some(url) = attr(tag, "href").and_then(|h| resolve(base, &h)) else { continue };
        let size = attr(tag, "sizes")
            .and_then(|s| s.split(['x', 'X']).next().and_then(|n| n.trim().parse::<u32>().ok()))
            .unwrap_or(0);
        let svg = url.to_ascii_lowercase().contains(".svg") || attr(tag, "type").is_some_and(|t| t.contains("svg"));
        let score = if svg {
            220
        } else if rel.contains("apple-touch-icon") {
            if size > 0 { size.min(256) } else { 180 }
        } else if size > 0 {
            size.min(256)
        } else {
            32
        };
        found.push((score, url));
    }
    found.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out: Vec<String> = Vec::new();
    for (_, u) in found {
        if !out.contains(&u) {
            out.push(u);
        }
    }
    out
}

fn sniff(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0, 0, 1, 0, ..] => Some("image/x-icon"),
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
            head.contains("<svg").then_some("image/svg+xml")
        }
    }
}

fn get(agent: &ureq::Agent, url: &str, limit: u64) -> Option<Vec<u8>> {
    let mut r = agent.get(url).call().ok()?;
    r.body_mut().with_config().limit(limit).read_to_vec().ok()
}

/// Trouve et télécharge la meilleure icône d'un site. Bloquant (thread secondaire).
pub fn fetch(page_url: &str) -> Option<String> {
    if !page_url.starts_with("https://") {
        return None;
    }
    let agent = crate::extensions::http_agent();
    let mut candidates = Vec::new();
    if let Some(html) = get(&agent, page_url, MAX_PAGE) {
        candidates = parse_icons(&String::from_utf8_lossy(&html), page_url);
    }
    let origin_end = page_url[8..].find('/').map(|i| i + 8).unwrap_or(page_url.len());
    let origin = &page_url[..origin_end];
    candidates.push(format!("{origin}/apple-touch-icon.png"));
    candidates.push(format!("{origin}/favicon.ico"));
    for c in candidates.into_iter().take(8) {
        if c.starts_with("data:image/") {
            if c.len() < 200_000 {
                return Some(c);
            }
            continue;
        }
        if !c.starts_with("https://") {
            continue;
        }
        if let Some(bytes) = get(&agent, &c, MAX_ICON) {
            if bytes.len() >= 16 {
                if let Some(mime) = sniff(&bytes) {
                    return Some(format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(&bytes)));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_urls() {
        let b = "https://store.epicgames.com/fr/browse?q=1";
        assert_eq!(resolve(b, "/favicon.ico").as_deref(), Some("https://store.epicgames.com/favicon.ico"));
        assert_eq!(resolve(b, "//cdn.x.com/i.png").as_deref(), Some("https://cdn.x.com/i.png"));
        assert_eq!(resolve(b, "img/i.png").as_deref(), Some("https://store.epicgames.com/fr/img/i.png"));
        assert_eq!(resolve(b, "javascript:alert(1)"), None);
    }

    #[test]
    fn picks_sharpest_icon() {
        let html = r#"<html><head>
          <link rel="mask-icon" href="/mask.svg">
          <link rel='icon' href="/assets/favicon.ico">
          <LINK REL="icon" TYPE="image/png" SIZES="32x32" HREF="/f32.png">
          <link href="https://cdn.discordapp.com/apple.png?v=1&amp;x=2" rel="apple-touch-icon" sizes="180x180">
          </head><body><link rel="icon" href="/ignored-in-body.png"></body></html>"#;
        let v = parse_icons(html, "https://discord.com/app");
        assert_eq!(v[0], "https://cdn.discordapp.com/apple.png?v=1&x=2");
        assert!(v.contains(&"https://discord.com/f32.png".to_string()));
        assert!(v.contains(&"https://discord.com/assets/favicon.ico".to_string()));
        assert!(!v.iter().any(|u| u.contains("mask") || u.contains("ignored")));
    }

    #[test]
    fn sniffs_types() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(sniff(b"\x00\x00\x01\x00...."), Some("image/x-icon"));
        assert_eq!(sniff(b"<?xml version='1.0'?><svg xmlns=..."), Some("image/svg+xml"));
        assert_eq!(sniff(b"<html>not an image</html>"), None);
    }
}

#[cfg(test)]
mod net_tests {
    /// Téléchargement réel (réseau) : `cargo test icons -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_fetch() {
        for url in ["https://github.com", "https://crates.io", "https://www.rust-lang.org"] {
            match super::fetch(url) {
                Some(d) => println!("{url} → {} ({} octets)", &d[..d.find(';').unwrap_or(20)], d.len()),
                None => println!("{url} → aucune icône (réseau bloqué ?)"),
            }
        }
    }
}
