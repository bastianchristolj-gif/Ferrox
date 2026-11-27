//! Paramètres persistants (JSON dans le dossier de config de l'utilisateur).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Raccourci de la page Nouvel onglet.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Tile {
    pub name: String,
    pub url: String,
}

fn default_tiles() -> Vec<Tile> {
    [
        ("YouTube", "https://www.youtube.com"),
        ("Twitch", "https://www.twitch.tv"),
        ("Steam", "https://store.steampowered.com"),
        ("Reddit", "https://www.reddit.com"),
        ("Discord", "https://discord.com/app"),
        ("Epic Games", "https://store.epicgames.com"),
        ("Wikipédia", "https://fr.wikipedia.org"),
        ("GitHub", "https://github.com"),
    ]
    .iter()
    .map(|(n, u)| Tile { name: n.to_string(), url: u.to_string() })
    .collect()
}

pub fn sanitize_tiles(tiles: Vec<Tile>) -> Vec<Tile> {
    tiles
        .into_iter()
        .filter(|t| t.url.starts_with("https://") && t.url.len() <= 400 && crate::blocker::host_of(&t.url).is_some())
        .map(|t| {
            let name: String = t.name.trim().chars().filter(|c| !c.is_control()).take(40).collect();
            let name = if name.is_empty() { crate::blocker::host_of(&t.url).unwrap_or("").trim_start_matches("www.").to_string() } else { name };
            Tile { name, url: t.url }
        })
        .take(16)
        .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Thème (teinte des fonds) : voir THEMES.
    pub theme: String,
    /// Couleur d'accent de l'interface (hex).
    pub accent: String,
    /// Animations de l'interface.
    pub animations: bool,
    /// Fond animé (grille) sur la page Nouvel onglet.
    pub wallpaper: bool,
    /// Affiche la RAM utilisée dans la barre d'outils.
    pub show_ram: bool,
    /// Raccourcis de la page Nouvel onglet.
    pub home_tiles: Vec<Tile>,
    /// Affiche les icônes des sites (chargées depuis les sites eux-mêmes).
    pub site_icons: bool,
    /// Enregistre l'historique de navigation.
    pub history_enabled: bool,
    // --- Onglets / démarrage ---
    /// Rouvre les onglets de la session précédente au démarrage.
    pub restore_session: bool,
    /// Fermer le dernier onglet quitte le navigateur.
    pub quit_on_last_tab: bool,
    /// Les nouveaux onglets s'ouvrent à côté de l'onglet actif.
    pub new_tab_next: bool,
    /// Zoom par défaut des pages (en %).
    pub default_zoom: u32,

    // --- Contenu / système ---
    /// Autorise la lecture automatique des vidéos avec le son.
    pub allow_autoplay: bool,
    /// Accélération matérielle (GPU).
    pub hardware_accel: bool,
    /// Suspend les onglets cachés depuis 30 s (sauf s'ils jouent du son).
    pub suspend_bg: bool,
    /// Mode économie : moins de processus et rendu allégé (redémarrage requis).
    pub eco_mode: bool,
    /// Dossier de téléchargement ("" = dossier système).
    pub download_dir: String,

    // --- Saisie automatique ---
    pub autofill: bool,
    pub save_passwords: bool,

    /// Moteur de recherche ; `{}` est remplacé par la requête encodée.
    pub search_url: String,
    pub home_url: String,

    // --- Sécurité / vie privée ---
    pub adblock: bool,
    pub https_only: bool,
    pub block_permissions: bool,
    /// Désactive WebGL / WebAudio (réduit l'empreinte numérique).
    pub anti_fingerprint: bool,
    /// Envoie les signaux Do-Not-Track / Global Privacy Control.
    pub send_dnt: bool,
    /// Efface cookies et données de navigation à la fermeture.
    pub clear_on_exit: bool,
    /// Ferrox a déjà proposé uBlock Origin Lite (installation automatique au 1er lancement).
    pub ubo_offered: bool,
    /// Nettoyage unique des extensions (ne garder que uBlock Origin Lite) déjà effectué.
    pub ext_cleanup_done: bool,

    // --- "Control" : limiteurs de ressources ---
    /// Limite de RAM (Mo) — au-delà, les onglets en arrière-plan sont mis en veille.
    pub ram_limit_mb: u64,
    pub ram_limiter: bool,
    /// Nombre maximum d'onglets actifs en mémoire simultanément.
    pub max_live_tabs: usize,
    /// Mise en veille d'un onglet inactif après N minutes (0 = jamais).
    pub hibernate_after_min: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "plasma".into(),
            accent: "#fa1e4e".into(),
            animations: true,
            wallpaper: true,
            show_ram: true,
            home_tiles: default_tiles(),
            site_icons: true,
            history_enabled: true,
            restore_session: false,
            quit_on_last_tab: true,
            new_tab_next: true,
            default_zoom: 100,
            allow_autoplay: false,
            hardware_accel: true,
            suspend_bg: true,
            eco_mode: false,
            download_dir: String::new(),
            autofill: true,
            save_passwords: true,
            search_url: SEARCH_ENGINES[0].into(),
            home_url: "ferrox://home".into(),
            adblock: true,
            https_only: true,
            block_permissions: true,
            anti_fingerprint: false,
            send_dnt: true,
            clear_on_exit: false,
            ubo_offered: false,
            ext_cleanup_done: false,
            ram_limit_mb: 1536,
            ram_limiter: true,
            max_live_tabs: 6,
            hibernate_after_min: 10,
        }
    }
}

/// Moteurs de recherche proposés (le premier est celui par défaut).
pub const SEARCH_ENGINES: &[&str] = &["https://search.brave.com/search?q={}", "https://www.google.com/search?q={}"];

/// Thèmes disponibles (définis visuellement dans ui/index.html).
pub const THEMES: &[&str] = &["plasma", "nebula", "glacier", "toxic", "magma", "ghost"];

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("ferrox")
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("ferrox")
        .join("profile")
}

fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .map(Settings::sanitized)
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = config_dir();
        if std::fs::create_dir_all(&dir).is_ok() {
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let tmp = dir.join("settings.json.tmp");
                // Écriture atomique : fichier temporaire puis renommage.
                if std::fs::write(&tmp, json).is_ok() {
                    let _ = std::fs::rename(&tmp, settings_path());
                }
            }
        }
    }

    /// Valide les valeurs venant du disque ou de l'interface.
    pub fn sanitized(mut self) -> Self {
        let d = Settings::default();
        if !THEMES.contains(&self.theme.as_str()) {
            self.theme = d.theme.clone();
        }
        let hex_ok = self.accent.len() == 7
            && self.accent.starts_with('#')
            && self.accent[1..].chars().all(|c| c.is_ascii_hexdigit());
        if !hex_ok {
            self.accent = d.accent.clone();
        }
        // Seuls Brave Search et Google sont proposés : tout autre moteur repasse sur Brave.
        if !SEARCH_ENGINES.contains(&self.search_url.as_str()) {
            self.search_url = d.search_url.clone();
        }
        if !(self.home_url.starts_with("https://") || self.home_url == "ferrox://home") {
            self.home_url = d.home_url.clone();
        }
        self.default_zoom = self.default_zoom.clamp(50, 250);
        self.home_tiles = sanitize_tiles(std::mem::take(&mut self.home_tiles));
        if !self.download_dir.is_empty() && !std::path::Path::new(&self.download_dir).is_dir() {
            self.download_dir.clear();
        }
        self.ram_limit_mb = self.ram_limit_mb.clamp(256, 32_768);
        self.max_live_tabs = self.max_live_tabs.clamp(1, 50);
        self.hibernate_after_min = self.hibernate_after_min.min(240);
        self
    }
}

// ------------------------------------------------------------------ session

#[derive(Serialize, Deserialize, Default)]
pub struct Session {
    pub tabs: Vec<String>,
    pub active: usize,
}

fn session_path() -> PathBuf {
    config_dir().join("session.json")
}

impl Session {
    pub fn load() -> Self {
        std::fs::read_to_string(session_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = config_dir();
        if std::fs::create_dir_all(&dir).is_ok() {
            if let Ok(json) = serde_json::to_string(self) {
                let tmp = dir.join("session.json.tmp");
                if std::fs::write(&tmp, json).is_ok() {
                    let _ = std::fs::rename(&tmp, session_path());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_brave_and_google() {
        let d = Settings::default();
        assert_eq!(d.search_url, "https://search.brave.com/search?q={}");
        for (input, expected) in [
            ("https://duckduckgo.com/?q={}", "https://search.brave.com/search?q={}"),
            ("https://www.bing.com/search?q={}", "https://search.brave.com/search?q={}"),
            ("https://www.google.com/search?q={}", "https://www.google.com/search?q={}"),
            ("https://search.brave.com/search?q={}", "https://search.brave.com/search?q={}"),
        ] {
            let s = Settings { search_url: input.into(), ..Settings::default() }.sanitized();
            assert_eq!(s.search_url, expected, "{input}");
        }
    }
}
