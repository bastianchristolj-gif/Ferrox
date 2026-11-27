//! Historique de navigation (fichier local `history.jsonl`, une visite par ligne).
//!
//! - Les onglets privés et les pages internes ne sont jamais enregistrés.
//! - Les ajouts sont écrits en fin de fichier (rapide) ; les suppressions
//!   réécrivent le fichier de façon atomique.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_ENTRIES: usize = 50_000;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Entry {
    pub id: u64,
    pub url: String,
    pub title: String,
    /// Secondes depuis l'époque Unix.
    pub ts: u64,
}

pub struct History {
    entries: Vec<Entry>,
    next_id: u64,
    path: PathBuf,
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn host(url: &str) -> String {
    crate::blocker::host_of(url).unwrap_or("").trim_start_matches("www.").to_ascii_lowercase()
}

impl History {
    pub fn load() -> Self {
        Self::load_from(crate::config::config_dir().join("history.jsonl"))
    }

    pub fn load_from(path: PathBuf) -> Self {
        // Une même visite peut apparaître plusieurs fois (titre mis à jour) : la dernière ligne l'emporte.
        let mut entries: Vec<Entry> = Vec::new();
        let mut index: HashMap<u64, usize> = HashMap::new();
        for e in std::fs::read_to_string(&path).unwrap_or_default().lines().filter_map(|l| serde_json::from_str::<Entry>(l).ok()) {
            match index.get(&e.id) {
                Some(&i) => entries[i] = e,
                None => {
                    index.insert(e.id, entries.len());
                    entries.push(e);
                }
            }
        }
        entries.retain(|e| !e.url.is_empty());
        if entries.len() > MAX_ENTRIES {
            entries.drain(..entries.len() - MAX_ENTRIES);
        }
        let next_id = entries.iter().map(|e| e.id).max().unwrap_or(0) + 1;
        let mut h = Self { entries, next_id, path };
        // Compacte le fichier au démarrage s'il a beaucoup grossi.
        if h.entries.len() > 1000 {
            h.rewrite();
        }
        h
    }

    fn append(&self, e: &Entry) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            if let Ok(line) = serde_json::to_string(e) {
                let _ = writeln!(f, "{line}");
            }
        }
    }

    fn rewrite(&mut self) {
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("jsonl.tmp");
        let mut out = String::with_capacity(self.entries.len() * 120);
        for e in &self.entries {
            if let Ok(l) = serde_json::to_string(e) {
                out.push_str(&l);
                out.push('\n');
            }
        }
        if std::fs::write(&tmp, out).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    /// Enregistre une visite. Une même adresse revisitée dans la minute ne crée
    /// pas de doublon (rechargements, redirections).
    pub fn visit(&mut self, url: &str, title: &str) {
        if !(url.starts_with("https://") || url.starts_with("http://")) || url.len() > 4096 {
            return;
        }
        let ts = now();
        if let Some(last) = self.entries.iter_mut().rev().take(20).find(|e| e.url == url) {
            if ts.saturating_sub(last.ts) < 60 {
                let t: String = title.chars().take(300).collect();
                if !t.is_empty() && t != last.title {
                    last.title = t;
                    let e = last.clone();
                    self.append(&e); // au rechargement, la dernière ligne d'un id l'emporte
                }
                return;
            }
        }
        let e = Entry { id: self.next_id, url: url.into(), title: title.chars().take(300).collect(), ts };
        self.next_id += 1;
        self.append(&e);
        self.entries.push(e);
        if self.entries.len() > MAX_ENTRIES + 5000 {
            self.entries.drain(..self.entries.len() - MAX_ENTRIES);
            self.rewrite();
        }
    }

    /// Recherche (titre ou adresse), du plus récent au plus ancien.
    pub fn query(&self, q: &str, offset: usize, limit: usize) -> (Vec<&Entry>, usize) {
        let q = q.trim().to_lowercase();
        let it = self.entries.iter().rev().filter(|e| {
            q.is_empty() || e.title.to_lowercase().contains(&q) || e.url.to_lowercase().contains(&q)
        });
        let all: Vec<&Entry> = it.collect();
        let total = all.len();
        (all.into_iter().skip(offset).take(limit).collect(), total)
    }

    /// Sites les plus visités ces 30 derniers jours : (hôte, url, titre, visites).
    pub fn top_sites(&self, n: usize) -> Vec<(String, String, String, usize)> {
        let since = now().saturating_sub(30 * 86400);
        let mut map: HashMap<String, (String, String, usize, u64)> = HashMap::new();
        for e in self.entries.iter().filter(|e| e.ts >= since) {
            let h = host(&e.url);
            if h.is_empty() {
                continue;
            }
            let v = map.entry(h).or_insert_with(|| (e.url.clone(), e.title.clone(), 0, 0));
            v.2 += 1;
            if e.ts >= v.3 {
                v.3 = e.ts;
            }
        }
        let mut v: Vec<_> = map.into_iter().map(|(h, (u, t, c, _))| (h.clone(), format!("https://{h}"), if t.is_empty() { u } else { t }, c)).collect();
        v.sort_by(|a, b| b.3.cmp(&a.3).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }

    pub fn delete_ids(&mut self, ids: &[u64]) {
        let before = self.entries.len();
        self.entries.retain(|e| !ids.contains(&e.id));
        if self.entries.len() != before {
            self.rewrite();
        }
    }

    pub fn delete_host(&mut self, h: &str) {
        let h = h.trim_start_matches("www.").to_ascii_lowercase();
        self.entries.retain(|e| host(&e.url) != h);
        self.rewrite();
    }

    /// Supprime les visites des `secs` dernières secondes (0 = tout).
    pub fn delete_since(&mut self, secs: u64) {
        if secs == 0 {
            self.entries.clear();
        } else {
            let since = now().saturating_sub(secs);
            self.entries.retain(|e| e.ts < since);
        }
        self.rewrite();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> History {
        let p = std::env::temp_dir().join(format!("ferrox-hist-{}-{}.jsonl", std::process::id(), now()));
        let _ = std::fs::remove_file(&p);
        History::load_from(p)
    }

    #[test]
    fn visit_query_delete() {
        let mut h = tmp();
        h.visit("https://www.youtube.com/watch?v=1", "Vidéo 1");
        h.visit("https://www.youtube.com/watch?v=1", "Vidéo 1 bis"); // < 60 s : pas de doublon
        h.visit("https://rust-lang.org/", "Rust");
        h.visit("file:///etc/passwd", "x"); // refusé
        assert_eq!(h.len(), 2);
        let (r, total) = h.query("vidéo", 0, 10);
        assert_eq!(total, 1);
        assert_eq!(r[0].title, "Vidéo 1 bis");
        assert_eq!(h.top_sites(5)[0].3, 1);
        h.delete_host("youtube.com");
        assert_eq!(h.len(), 1);
        // rechargement depuis le disque
        let h2 = History::load_from(h.path.clone());
        assert_eq!(h2.len(), 1);
        h.delete_since(0);
        assert_eq!(History::load_from(h.path.clone()).len(), 0);
        let _ = std::fs::remove_file(&h.path);
    }
}
