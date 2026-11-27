//! Bloqueur de publicités et de traqueurs.
//!
//! Deux niveaux :
//! 1. **Navigation** (Rust) : toute navigation vers un domaine bloqué est annulée.
//! 2. **Contenu** (script injecté dans chaque page/frame) : les requêtes
//!    `fetch`, `XMLHttpRequest`, `sendBeacon`, et les éléments `<script>`,
//!    `<img>`, `<iframe>`… pointant vers un domaine bloqué sont neutralisés,
//!    et les emplacements publicitaires courants sont masqués par CSS.
//!
//! Une liste personnalisée peut être ajoutée dans
//! `~/.config/ferrox/blocklist.txt` (un domaine par ligne, format "hosts" accepté).

use std::collections::HashSet;

const BUILTIN: &[&str] = &[
    // Régies publicitaires
    "doubleclick.net", "googlesyndication.com", "googleadservices.com", "adservice.google.com",
    "pagead2.googlesyndication.com", "adnxs.com", "adsrvr.org", "advertising.com", "criteo.com",
    "criteo.net", "taboola.com", "outbrain.com", "pubmatic.com", "rubiconproject.com",
    "openx.net", "casalemedia.com", "smartadserver.com", "amazon-adsystem.com", "adform.net",
    "media.net", "moatads.com", "yieldmo.com", "sharethrough.com", "teads.tv", "33across.com",
    "bidswitch.net", "contextweb.com", "indexww.com", "lijit.com", "sovrn.com", "spotxchange.com",
    "zedo.com", "popads.net", "propellerads.com", "adcash.com", "exoclick.com", "mgid.com",
    "revcontent.com", "adroll.com", "quantserve.com", "serving-sys.com", "2mdn.net",
    "adsafeprotected.com", "doubleverify.com", "imasdk.googleapis.com",
    // Traqueurs / analytics
    "google-analytics.com", "googletagmanager.com", "googletagservices.com", "analytics.google.com",
    "scorecardresearch.com", "hotjar.com", "hotjar.io", "mixpanel.com", "segment.io", "segment.com",
    "amplitude.com", "fullstory.com", "mouseflow.com", "crazyegg.com", "clarity.ms", "newrelic.com",
    "nr-data.net", "chartbeat.com", "chartbeat.net", "parsely.com", "bluekai.com", "krxd.net",
    "demdex.net", "omtrdc.net", "everesttech.net", "tapad.com", "rlcdn.com", "agkn.com",
    "mathtag.com", "bounceexchange.com", "branch.io", "app-measurement.com", "connect.facebook.net",
    "pixel.facebook.com", "ads.linkedin.com", "px.ads.linkedin.com", "analytics.twitter.com",
    "ads-twitter.com", "static.ads-twitter.com", "analytics.tiktok.com", "ads.tiktok.com",
    "bat.bing.com", "ads.yahoo.com", "sb.scorecardresearch.com", "pixel.quantserve.com",
    "stats.g.doubleclick.net", "track.hubspot.com", "mc.yandex.ru",
    "cdn.taboola.com", "trc.taboola.com", "widgets.outbrain.com", "matomo.cloud",
    // Cryptominers
    "coinhive.com", "coin-hive.com", "cryptoloot.pro", "minero.cc", "webmine.pro",
];

/// Taille max de la liste embarquée dans le script de page (perf).
const MAX_JS_DOMAINS: usize = 20_000;

pub struct Blocker {
    domains: HashSet<String>,
}

impl Blocker {
    pub fn load() -> Self {
        let mut domains: HashSet<String> = BUILTIN
            .iter()
            .map(|d| d.split('/').next().unwrap_or(d).to_string())
            .collect();
        let custom = crate::config::config_dir().join("blocklist.txt");
        if let Ok(text) = std::fs::read_to_string(custom) {
            for line in text.lines() {
                let line = line.split('#').next().unwrap_or("").trim();
                // Format "hosts" : "0.0.0.0 domaine.com"
                let d = line.split_whitespace().last().unwrap_or("");
                if is_valid_domain(d) {
                    domains.insert(d.to_ascii_lowercase());
                }
            }
        }
        Self { domains }
    }

    /// Domaines bloqués (pour les filtres réseau du moteur).
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    pub fn domains(&self) -> Vec<String> {
        let mut v: Vec<String> = self.domains.iter().cloned().collect();
        v.sort();
        v
    }

    pub fn len(&self) -> usize {
        self.domains.len()
    }

    /// Vrai si l'hôte (ou un de ses domaines parents) est bloqué.
    pub fn is_blocked_host(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let mut h = host.as_str();
        loop {
            if self.domains.contains(h) {
                return true;
            }
            match h.find('.') {
                Some(i) => h = &h[i + 1..],
                None => return false,
            }
        }
    }

    pub fn is_blocked_url(&self, url: &str) -> bool {
        host_of(url).is_some_and(|h| self.is_blocked_host(h))
    }

    /// Script injecté au démarrage de chaque document.
    pub fn content_script(&self) -> String {
        let list: Vec<&String> = self.domains.iter().take(MAX_JS_DOMAINS).collect();
        let json = serde_json::to_string(&list).unwrap_or_else(|_| "[]".into());
        CONTENT_SCRIPT.replace("__DOMAINS__", &json)
    }
}

pub fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit('@').next()?; // ignore user:pass@
    let host = if host.starts_with('[') {
        host.split(']').next().map(|h| h.trim_start_matches('['))?
    } else {
        host.split(':').next()?
    };
    (!host.is_empty()).then_some(host)
}

fn is_valid_domain(d: &str) -> bool {
    d.len() > 3
        && d.len() < 254
        && d.contains('.')
        && d != "0.0.0.0"
        && d != "127.0.0.1"
        && d != "localhost"
        && d.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

const CONTENT_SCRIPT: &str = r#"(() => {
  'use strict';
  const D = new Set(__DOMAINS__);
  let blocked = 0, timer = 0;
  const report = () => {
    if (timer) return;
    timer = setTimeout(() => { timer = 0;
      if (window.top === window) __fxSend('b:' + Math.min(blocked, 100000));
    }, 400);
  };
  const isBad = (u) => {
    try {
      let h = new URL(u, location.href).hostname.toLowerCase();
      while (h) { if (D.has(h)) return true; const i = h.indexOf('.'); if (i < 0) return false; h = h.slice(i + 1); }
    } catch (_) {}
    return false;
  };
  const hit = () => { blocked++; report(); };

  const ofetch = window.fetch;
  if (ofetch) window.fetch = function (input, init) {
    const u = typeof input === 'string' ? input : (input && input.url);
    if (u && isBad(u)) { hit(); return Promise.reject(new TypeError('Blocked by Ferrox')); }
    return ofetch.apply(this, arguments);
  };
  const oopen = XMLHttpRequest.prototype.open;
  XMLHttpRequest.prototype.open = function (m, u) {
    if (u && isBad(String(u))) { hit(); u = 'data:,'; arguments[1] = u; }
    return oopen.apply(this, arguments);
  };
  if (navigator.sendBeacon) {
    const ob = navigator.sendBeacon.bind(navigator);
    navigator.sendBeacon = (u, d) => isBad(u) ? (hit(), true) : ob(u, d);
  }
  const wrap = (proto, prop) => {
    const desc = Object.getOwnPropertyDescriptor(proto, prop);
    if (!desc || !desc.set) return;
    Object.defineProperty(proto, prop, { ...desc, set(v) {
      if (v && isBad(String(v))) { hit(); return desc.set.call(this, 'about:blank'); }
      return desc.set.call(this, v);
    }});
  };
  [[HTMLScriptElement, 'src'], [HTMLImageElement, 'src'], [HTMLIFrameElement, 'src'],
   [HTMLLinkElement, 'href'], [HTMLSourceElement, 'src']].forEach(([c, p]) => c && wrap(c.prototype, p));
  const osa = Element.prototype.setAttribute;
  Element.prototype.setAttribute = function (n, v) {
    if ((n === 'src' || n === 'href') && /^(SCRIPT|IMG|IFRAME|LINK|SOURCE)$/.test(this.tagName) && isBad(String(v))) { hit(); v = 'about:blank'; }
    return osa.call(this, n, v);
  };
  const sweep = (n) => {
    if (n.nodeType !== 1) return;
    const u = n.src || n.href;
    if (u && /^(SCRIPT|IMG|IFRAME|EMBED|OBJECT)$/.test(n.tagName) && isBad(u)) { hit(); n.remove(); }
  };
  new MutationObserver((ms) => { for (const m of ms) for (const n of m.addedNodes) sweep(n); })
    .observe(document, { childList: true, subtree: true });

  const css = 'ins.adsbygoogle,[id^="google_ads"],[id^="div-gpt-ad"],[class*="ad-banner"],[class^="ad-slot"],.adsbox,.ad-container,.ad-placeholder,.sponsored-content,[data-ad-slot],[data-ad-client],iframe[src*="doubleclick"],.OUTBRAIN,.trc_related_container{display:none!important}';
  const addCss = () => { const s = document.createElement('style'); s.textContent = css; (document.head || document.documentElement).appendChild(s); };
  if (document.documentElement) addCss(); else document.addEventListener('DOMContentLoaded', addCss);
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parsing() {
        assert_eq!(host_of("https://a.b.com/x?y"), Some("a.b.com"));
        assert_eq!(host_of("https://user:pw@evil.com:8080/"), Some("evil.com"));
        assert_eq!(host_of("http://[::1]:80/"), Some("::1"));
        assert_eq!(host_of("about:blank"), None);
    }

    #[test]
    fn subdomains_blocked() {
        let b = Blocker::load();
        assert!(b.is_blocked_host("stats.g.doubleclick.net"));
        assert!(b.is_blocked_host("www.google-analytics.com"));
        assert!(!b.is_blocked_host("example.com"));
        assert!(!b.is_blocked_host("net"));
    }
}
