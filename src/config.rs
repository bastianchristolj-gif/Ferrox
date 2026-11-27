//! Paramètres persistants (JSON dans le dossier de config de l'utilisateur).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Couleur d'accent de l'interface (hex).
    pub accent: String,
    /// Moteur de recherche ; `{}` est remplacé par la requête encodée.
    pub search_url: String,
    pub home_url: String,

    // --- Sécurité / vie privée ---
    pub adblock: bool,
    pub https_only: bool,
    pub block_permissions: bool,
    /// Désactive WebGL / WebAudio (réduit l'empreinte numérique).
    pub anti_fingerprint: bool,

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
            accent: "#fa1e4e".into(),
            search_url: "https://duckduckgo.com/?q={}".into(),
            home_url: "ferrox://home".into(),
            adblock: true,
            https_only: true,
            block_permissions: true,
            anti_fingerprint: false,
            ram_limit_mb: 1536,
            ram_limiter: true,
            max_live_tabs: 6,
            hibernate_after_min: 10,
        }
    }
}

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
        let hex_ok = self.accent.len() == 7
            && self.accent.starts_with('#')
            && self.accent[1..].chars().all(|c| c.is_ascii_hexdigit());
        if !hex_ok {
            self.accent = d.accent.clone();
        }
        if !(self.search_url.starts_with("https://") && self.search_url.contains("{}")) {
            self.search_url = d.search_url.clone();
        }
        if !(self.home_url.starts_with("https://") || self.home_url == "ferrox://home") {
            self.home_url = d.home_url.clone();
        }
        self.ram_limit_mb = self.ram_limit_mb.clamp(256, 32_768);
        self.max_live_tabs = self.max_live_tabs.clamp(1, 50);
        self.hibernate_after_min = self.hibernate_after_min.min(240);
        self
    }
}
