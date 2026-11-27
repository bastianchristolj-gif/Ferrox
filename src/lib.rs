//! Ferrox — navigateur web léger et sécurisé, interface "gaming", écrit en Rust.
//!
//! Architecture :
//! - une fenêtre native (tao) ;
//! - une vue "interface" (onglets, barre d'adresse, barre latérale, panneaux)
//!   qui couvre toute la fenêtre ;
//! - une vue par onglet, posée par-dessus dans la zone de contenu.
//!   Le moteur est Chromium, embarqué avec Ferrox (CEF) : tous les processus du navigateur
//!   sont des processus Ferrox, isolés dans le bac à sable de Chromium.
//! - Les onglets en arrière-plan sont mis en veille (vue détruite, URL conservée).

mod blocker;
mod config;
mod engine;
mod history;
mod icons;
mod monitor;

use blocker::Blocker;
use config::Settings;
use engine::{Bounds, Hooks, View, ViewOptions};
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    window::{Window, WindowBuilder},
};

/// Hauteur barre d'onglets (36) + barre de navigation (38).
const TOP_H: u32 = 74;
/// Cadre autour du contenu (liseré d'accent + zone de redimensionnement).
const BORDER: u32 = 3;
const PANEL_W: u32 = 320;
const TICK: Duration = Duration::from_secs(2);

const UI_HTML: &str = include_str!("../ui/index.html");
const HOME_HTML: &str = include_str!("../ui/home.html");
const HOME_URL: &str = "ferrox://home";
const SETTINGS_URL: &str = "ferrox://settings";
const EXT_URL: &str = "ferrox://extensions";
const HISTORY_URL: &str = "ferrox://history";

/// Pages internes dessinées par l'interface elle-même (aucune vue web).
fn internal_page(url: &str) -> Option<&'static str> {
    match url {
        SETTINGS_URL => Some("settings"),
        EXT_URL => Some("extensions"),
        HISTORY_URL => Some("history"),
        _ => None,
    }
}

/// Document HTML chargé par Ferrox (interface, page d'accueil).
fn data_url(html: &str) -> String {
    use base64::Engine;
    format!("data:text/html;charset=utf-8;base64,{}", base64::engine::general_purpose::STANDARD.encode(html))
}

// ---------------------------------------------------------------------------
// Événements internes
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum UserEvent {
    Ui(String),
    Title(u64, String),
    /// (onglet, en cours de chargement ?, adresse, précédent possible, suivant possible)
    Load(u64, bool, String, bool, bool),
    /// Nouvelle adresse du document principal (y compris sans rechargement).
    Address(u64, String),
    OpenTab(String),
    Blocked(u64, u32),
    Key(u64, String),
    NavBlocked(u64, String),
    HttpBlocked(u64, String),
    Download(String, bool),
    Favicon(u64, String),
    NetBlocked(u64),
    Icon(String, Option<String>),
    HomeTiles(u64, Vec<config::Tile>),
    Fullscreen(u64, bool),
    OpenTabWith(String, bool),
    SearchTab(String),
    /// Le navigateur de la vue a été créé par le moteur.
    Created(u64),
    /// Processus de rendu qui affiche l'onglet.
    TabPid(u64, u32),
    /// Un navigateur du moteur s'est fermé.
    BrowserClosed,
}

/// Commandes envoyées par l'interface (JSON). Tout champ inattendu est rejeté.
#[derive(Deserialize, Debug)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
enum UiCmd {
    Ready,
    Navigate { input: String },
    NewTab { private: bool },
    Close { id: u64 },
    Switch { id: u64 },
    Sleep { id: u64 },
    Back,
    Forward,
    Reload,
    Home,
    Panel { name: Option<String> },
    Settings { settings: Settings },
    AllowHttp { url: String },
    ClearData,
    Drag,
    Minimize,
    Maximize,
    Quit,
    Resize { dir: String },
    OpenPage { name: String },
    // Page Extensions (non disponible avec CEF) : acceptées pour afficher un message.
    #[allow(dead_code)]
    ExtInstall { input: String },
    #[allow(dead_code)]
    ExtRemove { id: String },
    #[allow(dead_code)]
    ExtEnable { id: String, on: bool },
    #[allow(dead_code)]
    ExtOpen { id: String, page: String },
    OpenUrl { url: String },
    ClosePopup,
    Key { k: String },
    Zoom { delta: i32 },
    Restart,
    ResetSettings,
    OpenDownloads,
    HistoryQuery { q: String, offset: usize },
    HistoryDelete { ids: Vec<u64> },
    HistoryDeleteHost { host: String },
    HistoryClear { range: String },
}

// ---------------------------------------------------------------------------
// Onglets
// ---------------------------------------------------------------------------

struct Tab {
    id: u64,
    title: String,
    url: String,
    wv: Option<View>,
    last_active: Instant,
    private: bool,
    loading: bool,
    blocked: u32,
    favicon: Option<String>,
    zoom: u32,
    can_back: bool,
    can_fwd: bool,
    /// Processus de rendu de l'onglet (RAM par onglet).
    pid: Option<u32>,
    /// Requêtes bloquées au niveau réseau sur la page actuelle.
    blocked_net: u32,
}

/// Drapeaux partagés avec les rappels du moteur (autres threads).
#[derive(Clone)]
struct Shared {
    blocker: Arc<Blocker>,
    script: Arc<String>,
    adblock: Arc<AtomicBool>,
    https_only: Arc<AtomicBool>,
    block_perms: Arc<AtomicBool>,
    http_allowed: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    download_dir: Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
}

/// Que faire une fois tous les navigateurs du moteur fermés.
#[derive(Clone, Copy, PartialEq)]
enum Exit {
    Quit,
    Restart,
}

struct App {
    window: Window,
    ui: View,
    tabs: Vec<Tab>,
    active: Option<u64>,
    next_id: u64,
    settings: Settings,
    shared: Shared,
    proxy: EventLoopProxy<UserEvent>,
    panel: Option<String>,
    monitor: monitor::Monitor,
    usage: monitor::Usage,
    blocked_total: u64,
    notice: Option<serde_json::Value>,
    /// (en cours ?, message) affiché sur la page Extensions.
    ext_status: Option<(bool, String)>,
    /// Rayon des coins arrondis par Windows 11 (0 ailleurs).
    corner_radius: u32,
    /// Clé secrète du canal interface → Ferrox.
    ui_token: String,
    /// Vraies icônes des sites (hôte → data URL), pour l'accueil et l'interface.
    icon_map: std::collections::HashMap<String, String>,
    icons_pending: std::collections::HashSet<String>,
    /// Onglets fermés récemment (Ctrl+Maj+T).
    closed: Vec<String>,
    /// Plein écran (vidéo ou F11) : le contenu occupe toute la fenêtre.
    fullscreen: bool,
    /// Réglages lus au démarrage qui exigent un redémarrage pour changer.
    boot_flags: (bool, bool, bool, bool),
    session_dirty: bool,
    history: history::History,
    focused: bool,
    ticks: u32,
    last_state: std::cell::RefCell<String>,
    /// Fermeture en cours (on attend que le moteur ait fermé ses navigateurs).
    exiting: Option<(Exit, Instant)>,
}

/// Point d'entrée Windows, appelé par Ferrox.exe (lanceur `bootstrap.exe` de CEF) pour chacun
/// des processus du navigateur. Le lanceur fournit les informations du bac à sable.
///
/// # Safety
/// Appelé une seule fois par processus, par le lanceur de CEF, avec des arguments valides.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn RunWinMain(
    instance: cef::sys::HINSTANCE,
    _command_line: *const u8,
    _command_show: i32,
    sandbox_info: *mut u8,
) -> i32 {
    run(cef::MainArgs { instance }, sandbox_info)
}

fn is_subprocess() -> bool {
    std::env::args().skip(1).any(|a| a.starts_with("--type="))
}

fn run(main_args: cef::MainArgs, sandbox_info: *mut u8) -> i32 {
    if is_subprocess() {
        // Processus de rendu, GPU, réseau… : aucun accès aux réglages ni aux fichiers de Ferrox.
        return engine::start(&main_args, sandbox_info, None, Default::default()).unwrap_or(0);
    }

    let settings = Settings::load();
    let settings_boot = settings.clone();
    let cache = config::data_dir().join("chromium");
    if settings.clear_on_exit {
        // Données de navigation effacées : le profil est supprimé avant le démarrage du moteur.
        let _ = std::fs::remove_dir_all(&cache);
    }
    let _ = std::fs::create_dir_all(&cache);
    let boot = engine::BootOptions {
        eco_mode: settings.eco_mode,
        allow_autoplay: settings.allow_autoplay,
        hardware_accel: settings.hardware_accel,
        anti_fingerprint: settings.anti_fingerprint,
    };
    if let Some(code) = engine::start(&main_args, sandbox_info, Some(boot), cache) {
        return code;
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    let window = {
        use tao::platform::windows::WindowBuilderExtWindows;
        WindowBuilder::new()
            .with_title("Ferrox")
            .with_decorations(false) // barre de titre intégrée aux onglets
            .with_inner_size(LogicalSize::new(1280.0, 800.0))
            .with_min_inner_size(LogicalSize::new(640.0, 400.0))
            .with_window_icon(app_icon())
            .with_undecorated_shadow(true) // ombre native Windows malgré l'absence de bordure
            // Grande icône (barre des tâches, Alt+Tab) fixée par Ferrox lui-même : elle ne dépend
            // pas du cache d'icônes de Windows.
            .with_taskbar_icon(taskbar_icon())
            .build(&event_loop)
            .unwrap_or_else(|e| fatal(&format!("Impossible de créer la fenêtre : {e}")))
    };
    {
        use tao::platform::windows::WindowExtWindows;
        engine::set_proxy(proxy.clone(), window.hwnd());
    }

    // Clé de l'interface : remise au script de l'interface après son chargement ; toute
    // commande sans elle est ignorée (aucun autre script ne peut piloter Ferrox).
    let ui_token = secret();
    let ui = {
        let tok = format!("{ui_token}|");
        let hooks = Hooks {
            on_message: Arc::new(move |body: &str, url: &str| {
                // Uniquement depuis le document de l'interface chargé par Ferrox.
                if !url.starts_with("data:text/html") {
                    return;
                }
                if let Some(cmd) = body.strip_prefix(tok.as_str()) {
                    // Déplacer / redimensionner la fenêtre : la vue rend d'abord la souris.
                    if cmd.contains(r#""cmd":"drag""#) || cmd.contains(r#""cmd":"resize""#) {
                        engine::release_capture();
                    }
                    engine::emit(UserEvent::Ui(cmd.to_string()));
                } else if body == r#"{"cmd":"ready"}"# {
                    // seul message accepté sans clé : il déclenche la remise de la clé
                    engine::emit(UserEvent::Ui(body.to_string()));
                }
            }),
            // L'interface ne doit jamais naviguer ailleurs.
            allow_nav: Arc::new(|url: &str| url.starts_with("data:text/html") || url.starts_with("about:")),
            block_request: None,
            on_popup: Arc::new(|_| {}),
            download_dir: Default::default(),
            block_perms: Arc::new(AtomicBool::new(true)),
        };
        View::new(
            ViewOptions {
                id: 0,
                url: data_url(UI_HTML),
                init: UI_INIT.into(),
                private: false,
                bounds: full_bounds(&window),
                visible: true,
                zoom: 100,
                ui: true,
            },
            hooks,
        )
    };

    let blocker = Arc::new(Blocker::load());
    let shared = Shared {
        script: Arc::new(blocker.content_script()),
        blocker,
        adblock: Arc::new(AtomicBool::new(settings.adblock)),
        https_only: Arc::new(AtomicBool::new(settings.https_only)),
        block_perms: Arc::new(AtomicBool::new(settings.block_permissions)),
        http_allowed: Default::default(),
        download_dir: Arc::new(std::sync::Mutex::new(download_dir_of(&settings))),
    };

    let mut app = App {
        window,
        ui,
        tabs: Vec::new(),
        active: None,
        next_id: 1,
        settings,
        shared,
        proxy,
        panel: None,
        monitor: monitor::Monitor::new(),
        usage: Default::default(),
        blocked_total: 0,
        ext_status: None,
        corner_radius: 0,
        ui_token,
        icon_map: Default::default(),
        icons_pending: Default::default(),
        closed: Vec::new(),
        fullscreen: false,
        boot_flags: boot_flags(&settings_boot),
        session_dirty: false,
        history: history::History::load(),
        focused: true,
        ticks: 0,
        last_state: Default::default(),
        notice: None,
        exiting: None,
    };

    // Windows 11 : coins arrondis + bordure native à la couleur d'accent (suit l'arrondi).
    if winstyle::round_corners(&app.window) {
        app.corner_radius = 8;
    }
    winstyle::border_color(&app.window, &app.settings.accent);

    app.refresh_icons();
    let start = std::env::args().skip(1).find(|a| !a.starts_with("--")).map(|a| app.resolve_input(&a));
    let session = config::Session::load();
    if let Some(url) = start {
        app.new_tab(url, false);
    } else if app.settings.restore_session && !session.tabs.is_empty() {
        // Les onglets restaurés restent « endormis » jusqu'à ce qu'on les ouvre : démarrage instantané.
        for url in session.tabs.iter().take(100) {
            let url = if url == HOME_URL || internal_page(url).is_some() || url.starts_with("https://") || url.starts_with("http://") {
                url.clone()
            } else {
                continue;
            };
            app.push_tab(url, false);
        }
        let idx = session.active.min(app.tabs.len().saturating_sub(1));
        match app.tabs.get(idx).map(|t| t.id) {
            Some(id) => app.switch(id),
            None => app.new_tab(HOME_URL.into(), false),
        }
    } else {
        let home = app.resolve_input(&app.settings.home_url.clone());
        app.new_tab(home, false);
    }

    let mut next_tick = Instant::now() + TICK;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(next_tick);
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => app.quit(),
            Event::WindowEvent { event: WindowEvent::Resized(_), .. } => app.layout(),
            Event::WindowEvent { event: WindowEvent::ScaleFactorChanged { .. }, .. } => app.layout(),
            Event::WindowEvent { event: WindowEvent::Focused(f), .. } => app.focused = f,
            Event::UserEvent(ev) => app.on_event(ev),
            Event::NewEvents(tao::event::StartCause::ResumeTimeReached { .. }) => {
                next_tick = Instant::now() + if app.exiting.is_some() { Duration::from_millis(100) } else { TICK };
                *control_flow = ControlFlow::WaitUntil(next_tick);
                app.tick();
            }
            _ => {}
        }
    });
}

fn boot_flags(s: &Settings) -> (bool, bool, bool, bool) {
    (s.allow_autoplay, s.hardware_accel, s.eco_mode, s.anti_fingerprint)
}

/// Script d'initialisation de l'interface : expose la fonction native d'envoi à la seule page
/// de l'interface (document de Ferrox, aucun contenu tiers).
const UI_INIT: &str = "(function (send) { Object.defineProperty(window, '__fxPost', { value: send, configurable: false, writable: false, enumerable: false }); })";

mod winstyle {
    use tao::platform::windows::WindowExtWindows;
    use windows_sys::Win32::Graphics::Dwm::{
        DWMWA_BORDER_COLOR, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
    };

    fn set(w: &tao::window::Window, attr: i32, value: u32) -> bool {
        // SAFETY: HWND valide de notre fenêtre, pointeur vers un u32 de 4 octets.
        unsafe { DwmSetWindowAttribute(w.hwnd() as _, attr as u32, &value as *const u32 as _, 4) == 0 }
    }

    /// Vrai si Windows a accepté (Windows 11) ; faux sous Windows 10.
    pub fn round_corners(w: &tao::window::Window) -> bool {
        set(w, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND as u32)
    }

    pub fn border_color(w: &tao::window::Window, hex: &str) {
        let c = |i: usize| u32::from_str_radix(hex.get(i..i + 2).unwrap_or("00"), 16).unwrap_or(0);
        // COLORREF = 0x00BBGGRR
        set(w, DWMWA_BORDER_COLOR, c(1) | (c(3) << 8) | (c(5) << 16));
    }
}

/// Quitte le processus de façon garantie (chien de garde si la sortie normale reste bloquée).
fn exit_now() -> ! {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_millis(1500));
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};
        // SAFETY: termine notre propre processus ; les fichiers ont déjà été enregistrés.
        unsafe { TerminateProcess(GetCurrentProcess(), 0) };
    });
    std::process::exit(0)
}

/// Affiche une erreur bloquante (boîte de dialogue) puis quitte.
pub(crate) fn fatal(msg: &str) -> ! {
    eprintln!("{msg}");
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (text, title) = (w(msg), w("Ferrox"));
    // SAFETY: chaînes UTF-16 terminées par 0, valides pendant l'appel.
    unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
    std::process::exit(1)
}

fn download_dir_of(s: &Settings) -> Option<std::path::PathBuf> {
    if s.download_dir.is_empty() { dirs::download_dir() } else { Some(s.download_dir.clone().into()) }
}

/// Icône de la barre des tâches : le renard Ferrox (128×128 RGBA, généré depuis assets/logo/fox.svg).
fn taskbar_icon() -> Option<tao::window::Icon> {
    const RGBA: &[u8] = include_bytes!("../assets/logo/icon128.rgba");
    tao::window::Icon::from_rgba(RGBA.to_vec(), 128, 128).ok()
}

fn app_icon() -> Option<tao::window::Icon> {
    const RGBA: &[u8] = include_bytes!("../assets/logo/icon64.rgba");
    tao::window::Icon::from_rgba(RGBA.to_vec(), 64, 64).ok()
}

fn logical_size(window: &Window) -> (u32, u32) {
    let s = window.inner_size().to_logical::<u32>(window.scale_factor());
    (s.width, s.height)
}

/// Rectangle logique → pixels physiques (les fenêtres des vues sont en pixels physiques).
fn to_bounds(window: &Window, x: u32, y: u32, w: u32, h: u32) -> Bounds {
    let s = window.scale_factor();
    let p = |v: u32| (v as f64 * s).round() as i32;
    Bounds { x: p(x), y: p(y), w: p(w).max(1), h: p(h).max(1) }
}

fn full_bounds(window: &Window) -> Bounds {
    let s = window.inner_size();
    Bounds { x: 0, y: 0, w: s.width.max(1) as i32, h: s.height.max(1) as i32 }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 128 bits aléatoires (graine aléatoire du système via `RandomState`), en hexadécimal.
fn secret() -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let mut out = String::new();
    for _ in 0..2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(N.fetch_add(1, Ordering::Relaxed));
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// Script injecté au début de chaque document d'un onglet, avant tout script du site.
/// C'est une fonction qui reçoit la fonction native d'envoi : elle n'est jamais visible dans
/// `window`, le site ne peut ni l'intercepter ni connaître la clé de l'onglet.
const EXT_UNSUPPORTED: &str = "Les extensions du Chrome Web Store ne sont pas disponibles avec le moteur Chromium embarqué (CEF). Le bloqueur de pubs et de pisteurs de Ferrox reste actif.";

const MENU_SCRIPT: &str = include_str!("../ui/menu.js");

fn content_init(token: &str, blocker: Option<&str>, dnt: bool, accent: &str) -> String {
    format!(
        r#"(function (__native) {{
  'use strict';
  const __T = '{token}';
  const __apply = Reflect.apply;
  const __post = (m) => __apply(__native, null, [m]);
  const __fxSend = (m) => {{ try {{ if (__post) __post(__T + '|' + m); }} catch (_) {{}} }};
  {blocker}
  {privacy}
  {menu}
}})"#,
        blocker = blocker.unwrap_or(""),
        // couleur déjà validée (#rrggbb) par les paramètres
        menu = MENU_SCRIPT.replace("__ACCENT__", accent),
        privacy = PRIVACY_SCRIPT.replace("__DNT__", if dnt { "true" } else { "false" }),
    )
}

fn is_shortcut(k: &str) -> bool {
    matches!(k, "t" | "w" | "l" | "r" | "n" | "T" | "h" | "next" | "prev" | "back" | "fwd" | "zin" | "zout" | "z0" | "f11" | "esc")
        || (k.len() == 1 && (b'1'..=b'9').contains(&k.as_bytes()[0]))
}

fn tab_title(url: &str) -> String {
    match url {
        HOME_URL => "Nouvel onglet".into(),
        SETTINGS_URL => "Paramètres".into(),
        EXT_URL => "Extensions".into(),
        HISTORY_URL => "Historique".into(),
        _ => url.into(),
    }
}

fn is_local_host(host: &str) -> bool {
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".localhost")
}

impl App {
    // ------------------------------------------------------------------ layout
    fn content_rect(&self) -> Bounds {
        let (w, h) = logical_size(&self.window);
        if self.fullscreen {
            return full_bounds(&self.window);
        }
        let b = if self.window.is_maximized() { 0 } else { BORDER };
        let panel = if self.panel.is_some() { PANEL_W } else { 0 };
        to_bounds(&self.window, b, TOP_H, w.saturating_sub(2 * b + panel), h.saturating_sub(TOP_H + b))
    }

    fn layout(&mut self) {
        self.ui.set_bounds(full_bounds(&self.window));
        let r = self.content_rect();
        for t in &self.tabs {
            if let Some(wv) = &t.wv {
                wv.set_bounds(r);
            }
        }
    }

    // ---------------------------------------------------------------- onglets
    fn idx(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn resolve_input(&self, input: &str) -> String {
        let s = input.trim();
        if s.is_empty() || s == HOME_URL {
            return HOME_URL.into();
        }
        if internal_page(s).is_some() {
            return s.into();
        }
        let lower = s.to_ascii_lowercase();
        if lower.starts_with("https://") {
            return s.into();
        }
        if lower.starts_with("http://") {
            let host = blocker::host_of(s).unwrap_or("");
            if self.settings.https_only && !is_local_host(host) {
                return format!("https://{}", &s[7..]);
            }
            return s.into();
        }
        let looks_like_host = !s.contains(char::is_whitespace)
            && (s.contains('.') || s.starts_with("localhost"))
            && !s.starts_with('.')
            && !s.ends_with('.');
        if looks_like_host {
            let host = s.split(['/', ':']).next().unwrap_or("");
            let scheme = if is_local_host(host) { "http" } else { "https" };
            return format!("{scheme}://{s}");
        }
        self.settings.search_url.replace("{}", &percent_encode(s))
    }

    fn home_html(&self) -> String {
        // accent validé (#rrggbb) et URL de recherche encodée en JSON : pas d'injection.
        let search = serde_json::to_string(&self.settings.search_url).unwrap_or_else(|_| "\"\"".into());
        let mut classes = Vec::new();
        if !self.settings.animations {
            classes.push("noanim");
        }
        if !self.settings.wallpaper {
            classes.push("nowall");
        }
        if !self.settings.site_icons {
            classes.push("noicons");
        }
        // JSON inséré dans un <script> : on échappe « < » pour qu'aucun texte ne puisse fermer la balise.
        let js = |v: serde_json::Value| v.to_string().replace('<', "\\u003c");
        let top: Vec<_> = self
            .history
            .top_sites(8)
            .into_iter()
            .map(|(h, url, title, n)| serde_json::json!({"host": h, "url": url, "title": title, "n": n}))
            .collect();
        HOME_HTML
            .replace("__ACCENT__", &self.settings.accent)
            .replace("__SEARCH__", &search.replace('<', "\\u003c"))
            .replace("__TILES__", &js(serde_json::json!(self.settings.home_tiles)))
            .replace("__TOP__", &js(serde_json::json!(if self.settings.history_enabled { top } else { vec![] })))
            .replace("__ICONS__", if self.settings.site_icons { "true" } else { "false" })
            .replace("__ICONMAP__", &js(serde_json::json!(if self.settings.site_icons { self.icon_map.clone() } else { Default::default() })))
            .replace("__BODY__", &classes.join(" "))
    }

    fn new_tab(&mut self, url: String, private: bool) {
        let id = self.push_tab(url, private);
        if self.settings.new_tab_next {
            // Place l'onglet juste après l'onglet actif.
            if let (Some(a), Some(last)) = (self.active_idx(), self.tabs.len().checked_sub(1)) {
                if a + 1 < last {
                    let t = self.tabs.remove(last);
                    self.tabs.insert(a + 1, t);
                }
            }
        }
        self.switch(id);
    }

    /// Ajoute un onglet (non chargé) en fin de liste et renvoie son id.
    fn push_tab(&mut self, url: String, private: bool) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.session_dirty = true;
        self.tabs.push(Tab {
            id,
            title: tab_title(&url),
            url,
            wv: None,
            last_active: Instant::now(),
            private,
            loading: false,
            blocked: 0,
            favicon: None,
            zoom: self.settings.default_zoom,
            can_back: false,
            can_fwd: false,
            pid: None,
            blocked_net: 0,
        });
        id
    }

    fn create_webview(&mut self, idx: usize) {
        let (id, url, private) = {
            let t = &self.tabs[idx];
            (t.id, t.url.clone(), t.private)
        };
        if internal_page(&url).is_some() {
            return;
        }
        let sh = self.shared.clone();
        let p = self.proxy.clone();
        let is_home = url == HOME_URL;
        let load = if is_home { data_url(&self.home_html()) } else { url.clone() };
        let adblock = self.settings.adblock;
        self.tabs[idx].pid = None;

        // Canal privé page → Ferrox : clé secrète propre à l'onglet, gardée dans une
        // fermeture JavaScript que les scripts du site ne peuvent pas lire.
        let token = secret();
        let init = content_init(&token, adblock.then(|| sh.script.as_str()), self.settings.send_dnt, &self.settings.accent);

        let allow_nav = {
            let (sh, p) = (sh.clone(), p.clone());
            move |url: &str| -> bool {
                let lower = url.to_ascii_lowercase();
                if lower.starts_with("about:") || lower.starts_with("blob:") || lower.starts_with("data:") {
                    // (les sites ne peuvent pas ouvrir de document data: en haut de page : Chromium l'interdit)
                    return true;
                }
                if !(lower.starts_with("https://") || lower.starts_with("http://")) {
                    // file://, javascript:, chrome://, schémas d'applications… refusés.
                    return false;
                }
                if sh.adblock.load(Ordering::Relaxed) && sh.blocker.is_blocked_url(url) {
                    let _ = p.send_event(UserEvent::NavBlocked(id, url.to_string()));
                    return false;
                }
                if lower.starts_with("http://") && sh.https_only.load(Ordering::Relaxed) {
                    let host = blocker::host_of(url).unwrap_or("").to_ascii_lowercase();
                    let allowed = sh.http_allowed.lock().map(|s| s.contains(&host)).unwrap_or(false);
                    if !is_local_host(&host) && !allowed {
                        let _ = p.send_event(UserEvent::HttpBlocked(id, url.to_string()));
                        return false;
                    }
                }
                true
            }
        };

        // Blocage au niveau réseau (scripts, images, iframes… de domaines de pistage / publicité).
        let block_request: Option<engine::Filter> = adblock.then(|| {
            let (sh, p) = (sh.clone(), p.clone());
            Arc::new(move |url: &str| -> bool {
                if !sh.adblock.load(Ordering::Relaxed) || !url.starts_with("http") {
                    return false;
                }
                let blocked = sh.blocker.is_blocked_url(url);
                if blocked {
                    let _ = p.send_event(UserEvent::NetBlocked(id));
                }
                blocked
            }) as engine::Filter
        });

        let on_popup = {
            let (sh, p) = (sh.clone(), p.clone());
            move |url: String| {
                let lower = url.to_ascii_lowercase();
                let ok = (lower.starts_with("https://") || lower.starts_with("http://"))
                    && url.len() <= 8192
                    && !(sh.adblock.load(Ordering::Relaxed) && sh.blocker.is_blocked_url(&url));
                if ok {
                    let _ = p.send_event(UserEvent::OpenTab(url));
                }
            }
        };

        // Canal minimal depuis les pages web : seulement des compteurs / raccourcis.
        let on_message = {
            let p = p.clone();
            move |body: &str, uri: &str| {
                // Message sans la clé de l'onglet → ignoré (forgé par un site).
                let Some(b) = body.strip_prefix(token.as_str()).and_then(|r| r.strip_prefix('|')) else { return };
                if let Some(json) = b.strip_prefix("h:") {
                    // Raccourcis de la page d'accueil : accepté uniquement depuis la page d'accueil
                    // (document chargé par Ferrox), jamais depuis un site.
                    if is_home && b.len() <= 16 * 1024 && uri.starts_with("data:text/html") {
                        if let Ok(tiles) = serde_json::from_str::<Vec<config::Tile>>(json) {
                            let _ = p.send_event(UserEvent::HomeTiles(id, tiles));
                        }
                    }
                    return;
                }
                if b.len() > 4096 + 8 {
                    return;
                }
                if let Some(n) = b.strip_prefix("b:").and_then(|n| n.parse::<u32>().ok()) {
                    let _ = p.send_event(UserEvent::Blocked(id, n.min(100_000)));
                } else if let Some(k) = b.strip_prefix("k:") {
                    if is_shortcut(k) {
                        let _ = p.send_event(UserEvent::Key(id, k.to_string()));
                    }
                } else if let Some(u) = b.strip_prefix("f:") {
                    // Icône du site : HTTPS uniquement, caractères sûrs uniquement.
                    if u.starts_with("https://") && u.len() < 1000 && !u.contains(['"', '\\', '<', '>', ' ', '\n']) {
                        let _ = p.send_event(UserEvent::Favicon(id, u.to_string()));
                    }
                } else if let Some(u) = b.strip_prefix("op:") {
                    // menu clic droit : « ouvrir dans un onglet privé »
                    if u.starts_with("https://") || u.starts_with("http://") {
                        let _ = p.send_event(UserEvent::OpenTabWith(u.to_string(), true));
                    }
                } else if let Some(u) = b.strip_prefix("o:") {
                    if u.starts_with("https://") || u.starts_with("http://") {
                        let _ = p.send_event(UserEvent::OpenTab(u.to_string()));
                    }
                } else if let Some(q) = b.strip_prefix("s:") {
                    let q: String = q.chars().filter(|c| !c.is_control()).take(400).collect();
                    if !q.trim().is_empty() {
                        let _ = p.send_event(UserEvent::SearchTab(q));
                    }
                }
            }
        };

        let hooks = Hooks {
            on_message: Arc::new(on_message),
            allow_nav: Arc::new(allow_nav),
            block_request,
            on_popup: Arc::new(on_popup),
            download_dir: sh.download_dir.clone(),
            block_perms: sh.block_perms.clone(),
        };
        let wv = View::new(
            ViewOptions {
                id,
                url: load,
                init,
                private,
                bounds: self.content_rect(),
                visible: self.active == Some(id),
                zoom: self.tabs[idx].zoom,
                ui: false,
            },
            hooks,
        );
        self.tabs[idx].wv = Some(wv);
    }

    fn switch(&mut self, id: u64) {
        let Some(idx) = self.idx(id) else { return };
        if self.active != Some(id) {
            self.set_fullscreen(false);
            self.session_dirty = true;
        }
        self.active = Some(id);
        self.notice = None;
        if self.tabs[idx].wv.is_none() {
            self.create_webview(idx);
        }
        if internal_page(&self.tabs[idx].url).is_some() {
            // Page interne : c'est l'interface qui affiche la page et reçoit le clavier.
            self.ui.focus();
        }
        let r = self.content_rect();
        for t in &mut self.tabs {
            if let Some(wv) = &t.wv {
                if t.id == id {
                    wv.set_bounds(r);
                    wv.set_visible(true);
                    wv.focus();
                    t.last_active = Instant::now();
                } else {
                    // Vue cachée : Chromium réduit son activité (rendu, minuteries).
                    wv.set_visible(false);
                }
            }
        }
        self.enforce_limits();
        self.push_state();
    }

    fn close(&mut self, id: u64) {
        let Some(idx) = self.idx(id) else { return };
        if self.tabs.len() == 1 && self.settings.quit_on_last_tab {
            // Dernier onglet fermé → on quitte (la session garde cet onglet).
            self.quit();
            return;
        }
        let tab = self.tabs.remove(idx);
        self.session_dirty = true;
        if !tab.private && tab.url != HOME_URL {
            self.closed.push(tab.url.clone());
            if self.closed.len() > 25 {
                self.closed.remove(0);
            }
        }
        drop(tab.wv); // ferme le navigateur et libère la mémoire
        if self.tabs.is_empty() {
            self.new_tab(HOME_URL.into(), false);
            return;
        }
        if self.active == Some(id) {
            let ni = idx.min(self.tabs.len() - 1);
            let nid = self.tabs[ni].id;
            self.switch(nid);
        } else {
            self.push_state();
        }
    }

    fn sleep_tab(&mut self, idx: usize) {
        if self.tabs[idx].private {
            return; // un onglet privé mis en veille perdrait sa session
        }
        if let Some(wv) = self.tabs[idx].wv.take() {
            if let Some(u) = wv.url() {
                if u.starts_with("http") {
                    self.tabs[idx].url = u;
                }
            }
            drop(wv);
            self.tabs[idx].loading = false;
            self.tabs[idx].pid = None;
        }
    }

    /// Limiteurs façon "Control" : nombre d'onglets vivants, inactivité, RAM.
    fn enforce_limits(&mut self) {
        let active = self.active;
        let mut live: Vec<(Instant, usize)> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| t.wv.is_some() && Some(t.id) != active && !t.private)
            .map(|(i, t)| (t.last_active, i))
            .collect();
        live.sort();

        let mut to_sleep = Vec::new();
        let max_bg = self.settings.max_live_tabs.saturating_sub(1);
        if live.len() > max_bg {
            to_sleep.extend(live.iter().take(live.len() - max_bg).map(|(_, i)| *i));
        }
        if self.settings.hibernate_after_min > 0 {
            let limit = Duration::from_secs(self.settings.hibernate_after_min * 60);
            to_sleep.extend(live.iter().filter(|(t, _)| t.elapsed() > limit).map(|(_, i)| *i));
        }
        if self.settings.ram_limiter && self.usage.ram_mb > self.settings.ram_limit_mb {
            if let Some((_, i)) = live.first() {
                to_sleep.push(*i);
            }
        }
        to_sleep.sort();
        to_sleep.dedup();
        for i in to_sleep {
            self.sleep_tab(i);
        }
    }

    fn active_idx(&self) -> Option<usize> {
        self.active.and_then(|id| self.idx(id))
    }

    fn active_wv(&self) -> Option<&View> {
        self.active_idx().and_then(|i| self.tabs[i].wv.as_ref())
    }

    fn navigate_active(&mut self, url: String) {
        let Some(i) = self.active_idx() else { return };
        self.notice = None;
        if url == HOME_URL || internal_page(&url).is_some() {
            // Recrée la vue (page d'accueil) ou bascule sur une page interne.
            self.tabs[i].wv = None;
            self.tabs[i].title = tab_title(&url);
            self.tabs[i].url = url;
            self.tabs[i].loading = false;
            let id = self.tabs[i].id;
            self.switch(id);
            return;
        }
        self.tabs[i].url = url.clone();
        match &self.tabs[i].wv {
            Some(wv) => wv.load_url(&url),
            None => {
                let id = self.tabs[i].id;
                self.switch(id);
            }
        }
        self.push_state();
    }

    // -------------------------------------------------------------- événements
    fn on_event(&mut self, ev: UserEvent) {
        match ev {
            UserEvent::Ui(json) => match serde_json::from_str::<UiCmd>(&json) {
                Ok(cmd) => self.on_ui(cmd),
                Err(e) => eprintln!("commande UI rejetée : {e}"),
            },
            UserEvent::Title(id, t) => {
                if let Some(i) = self.idx(id) {
                    if !t.trim().is_empty() {
                        self.tabs[i].title = t.clone();
                    }
                    let cur = self.tabs[i].url.clone();
                    if cur.starts_with("http") && !self.tabs[i].private && self.settings.history_enabled {
                        self.history.visit(&cur, &t);
                    }
                    self.push_state();
                }
            }
            UserEvent::Address(id, url) => {
                // Sites « à page unique » (YouTube…) : l'adresse change sans rechargement.
                if let Some(i) = self.idx(id) {
                    if url.starts_with("http") && url != self.tabs[i].url {
                        self.tabs[i].url = url;
                        self.session_dirty = true;
                        self.push_state();
                    }
                }
            }
            UserEvent::Created(id) => {
                // La fenêtre du navigateur existe : on la (re)place et on (re)donne le focus.
                if self.active == Some(id) {
                    if let Some(wv) = self.active_wv() {
                        wv.focus();
                    }
                }
            }
            UserEvent::TabPid(id, pid) => {
                if let Some(i) = self.idx(id) {
                    self.tabs[i].pid = Some(pid);
                }
            }
            UserEvent::BrowserClosed => {
                if self.exiting.is_some() && engine::live_browsers() == 0 {
                    self.finish_exit();
                }
            }
            UserEvent::HomeTiles(id, tiles) => {
                if self.idx(id).is_some_and(|i| self.tabs[i].url == HOME_URL) {
                    self.settings.home_tiles = config::sanitize_tiles(tiles);
                    self.settings.save();
                    self.refresh_icons();
                }
            }
            UserEvent::Icon(host, data) => {
                self.icons_pending.remove(&host);
                if let Some(d) = data {
                    icons::store(&host, &d);
                    let js = |v: &str| serde_json::to_string(v).unwrap_or_default();
                    let call = format!("window.fxIcon && fxIcon({}, {})", js(&host), js(&d));
                    // pages d'accueil ouvertes + interface
                    for t in self.tabs.iter().filter(|t| t.url == HOME_URL) {
                        if let Some(wv) = &t.wv {
                            wv.eval(&call);
                        }
                    }
                    self.icon_map.insert(host, d);
                    self.send_icons();
                }
            }
            UserEvent::Load(id, started, url, back, fwd) => {
                if let Some(i) = self.idx(id) {
                    let t = &mut self.tabs[i];
                    let was = t.loading;
                    t.loading = started;
                    t.can_back = back;
                    t.can_fwd = fwd;
                    if started && !was {
                        t.blocked = 0;
                        t.blocked_net = 0;
                        if blocker::host_of(&url) != blocker::host_of(&t.url) {
                            t.favicon = None;
                        }
                    }
                    if url.starts_with("http") {
                        t.url = url;
                        self.session_dirty = true;
                    }
                    if !started && was && !t.private && self.settings.history_enabled && t.url.starts_with("http") {
                        let (u, title) = (t.url.clone(), t.title.clone());
                        let title = if title.starts_with("http") { String::new() } else { title };
                        self.history.visit(&u, &title);
                    }
                    self.push_state();
                }
            }
            UserEvent::OpenTab(url) => {
                let private = self.active_idx().is_some_and(|i| self.tabs[i].private);
                self.new_tab(url, private);
            }
            UserEvent::Blocked(id, n) => {
                if let Some(i) = self.idx(id) {
                    let old = self.tabs[i].blocked;
                    if n > old {
                        self.blocked_total += (n - old) as u64;
                        self.tabs[i].blocked = n;
                        self.push_state();
                    }
                }
            }
            UserEvent::NavBlocked(id, url) => {
                self.blocked_total += 1;
                if self.active == Some(id) {
                    self.notice = Some(serde_json::json!({"kind": "blocked", "url": url}));
                }
                self.push_state();
            }
            UserEvent::HttpBlocked(id, url) => {
                if self.active == Some(id) {
                    self.notice = Some(serde_json::json!({"kind": "http", "url": url}));
                    self.push_state();
                }
            }
            UserEvent::Download(url, done) => {
                let kind = if done { "download_done" } else { "download" };
                self.notice = Some(serde_json::json!({"kind": kind, "url": url}));
                self.push_state();
            }
            UserEvent::Key(id, k) => {
                if self.active == Some(id) {
                    self.shortcut(&k);
                }
            }
            UserEvent::Favicon(id, url) => {
                if let Some(i) = self.idx(id) {
                    if self.tabs[i].favicon.as_deref() != Some(url.as_str()) {
                        self.tabs[i].favicon = Some(url);
                        self.push_state();
                    }
                }
            }
            UserEvent::Fullscreen(id, on) => {
                if self.active == Some(id) {
                    self.set_fullscreen(on);
                }
            }
            UserEvent::OpenTabWith(url, private) => self.new_tab(url, private),
            UserEvent::SearchTab(q) => {
                let url = self.settings.search_url.replace("{}", &percent_encode(q.trim()));
                let private = self.active_idx().is_some_and(|i| self.tabs[i].private);
                self.new_tab(url, private);
            }
            UserEvent::NetBlocked(id) => {
                self.blocked_total += 1;
                if let Some(i) = self.idx(id) {
                    self.tabs[i].blocked_net += 1;
                    self.push_state();
                }
            }
        }
    }

    fn active_id(&self) -> Option<u64> {
        self.active
    }

    /// Raccourcis clavier (venant de l'interface ou des pages).
    fn shortcut(&mut self, k: &str) {
        match k {
            "t" => self.new_tab(HOME_URL.into(), false),
            "n" => self.new_tab(HOME_URL.into(), true),
            "w" => {
                if let Some(id) = self.active_id() {
                    self.close(id);
                }
            }
            "T" => {
                if let Some(url) = self.closed.pop() {
                    self.new_tab(url, false);
                }
            }
            "r" => {
                if let Some(wv) = self.active_wv() {
                    wv.reload();
                }
            }
            "back" => {
                if let Some(wv) = self.active_wv() {
                    wv.go_back();
                }
            }
            "fwd" => {
                if let Some(wv) = self.active_wv() {
                    wv.go_forward();
                }
            }
            "l" => {
                self.ui.focus();
                self.ui.eval("window.fx && fx.focusUrl()");
            }
            "next" | "prev" => self.cycle(k == "next"),
            "zin" => self.zoom(10),
            "zout" => self.zoom(-10),
            "z0" => self.zoom(0),
            "h" => {
                if let Some(t) = self.tabs.iter().find(|t| t.url == HISTORY_URL) {
                    let id = t.id;
                    self.switch(id);
                } else {
                    self.new_tab(HISTORY_URL.into(), false);
                }
            }
            "f11" => {
                let on = !self.fullscreen;
                self.set_fullscreen(on);
            }
            "esc" => {
                if self.fullscreen {
                    self.set_fullscreen(false);
                }
            }
            d if d.len() == 1 && d.as_bytes()[0].is_ascii_digit() && d != "0" => {
                // Ctrl+1…8 : onglet n ; Ctrl+9 : dernier onglet.
                let n = (d.as_bytes()[0] - b'1') as usize;
                let idx = if n == 8 { self.tabs.len() - 1 } else { n };
                if let Some(t) = self.tabs.get(idx) {
                    let id = t.id;
                    self.switch(id);
                }
            }
            _ => {}
        }
    }

    fn zoom(&mut self, delta: i32) {
        let Some(i) = self.active_idx() else { return };
        let z = if delta == 0 { 100 } else { (self.tabs[i].zoom as i32 + delta).clamp(30, 300) as u32 };
        self.tabs[i].zoom = z;
        if let Some(wv) = &self.tabs[i].wv {
            wv.zoom(z);
        }
        self.push_state();
    }

    fn set_fullscreen(&mut self, on: bool) {
        if self.fullscreen == on {
            return;
        }
        self.fullscreen = on;
        self.window.set_fullscreen(on.then_some(tao::window::Fullscreen::Borderless(None)));
        self.layout();
        self.push_state();
    }

    fn ui_eval(&self, js: &str) {
        self.ui.eval(&format!("window.fx && {js}"));
    }

    fn send_history(&self, q: &str, offset: usize) {
        let (items, total) = self.history.query(q, offset, 150);
        let v = serde_json::json!({ "q": q, "offset": offset, "total": total, "items": items, "count": self.history.len() });
        self.ui_eval(&format!("fx.history({v})"));
    }

    fn save_session(&mut self) {
        let tabs: Vec<&Tab> = self.tabs.iter().filter(|t| !t.private).collect();
        let active = tabs.iter().position(|t| Some(t.id) == self.active).unwrap_or(0);
        config::Session { tabs: tabs.iter().map(|t| t.url.clone()).collect(), active }.save();
        self.session_dirty = false;
    }

    /// Ferme Ferrox : enregistre, cache la fenêtre, ferme les navigateurs du moteur puis arrête
    /// le moteur (voir [`App::finish_exit`]).
    fn quit(&mut self) {
        self.begin_exit(Exit::Quit);
    }

    fn begin_exit(&mut self, how: Exit) {
        if self.exiting.is_some() {
            return;
        }
        self.settings.save();
        self.save_session();
        // La fenêtre disparaît tout de suite, même si le moteur met du temps à s'arrêter.
        self.window.set_visible(false);
        if self.settings.clear_on_exit {
            // Le profil est aussi supprimé au prochain démarrage, avant le lancement du moteur.
            engine::clear_browsing_data();
        }
        self.exiting = Some((how, Instant::now()));
        for t in &mut self.tabs {
            t.wv = None;
        }
        // La vue de l'interface est fermée en dernier.
        let ui = std::mem::replace(&mut self.ui, View::closed());
        drop(ui);
        if engine::live_browsers() == 0 {
            self.finish_exit();
        }
    }

    /// Tous les navigateurs sont fermés (ou le délai est écoulé) : arrêt du moteur et sortie.
    fn finish_exit(&mut self) -> ! {
        let how = self.exiting.map(|(h, _)| h).unwrap_or(Exit::Quit);
        engine::stop();
        if how == Exit::Restart {
            if let Ok(exe) = std::env::current_exe() {
                let _ = std::process::Command::new(exe).spawn();
            }
        }
        exit_now()
    }

    fn cycle(&mut self, forward: bool) {
        let Some(i) = self.active_idx() else { return };
        let n = self.tabs.len();
        let ni = if forward { (i + 1) % n } else { (i + n - 1) % n };
        let id = self.tabs[ni].id;
        self.switch(id);
    }

    fn on_ui(&mut self, cmd: UiCmd) {
        match cmd {
            UiCmd::Ready => {
                // (re)donne la clé à l'interface ; elle ne l'accepte qu'une fois
                self.ui.eval(&format!("window.fx && fx.setToken('{}')", self.ui_token));
                self.last_state.borrow_mut().clear();
                self.push_state();
                self.send_icons();
            }
            UiCmd::Navigate { input } => {
                let url = self.resolve_input(&input);
                self.navigate_active(url);
            }
            UiCmd::NewTab { private } => self.new_tab(HOME_URL.into(), private),
            UiCmd::Close { id } => self.close(id),
            UiCmd::Switch { id } => self.switch(id),
            UiCmd::Sleep { id } => {
                if Some(id) != self.active {
                    if let Some(i) = self.idx(id) {
                        self.sleep_tab(i);
                        self.push_state();
                    }
                }
            }
            UiCmd::Back => self.shortcut("back"),
            UiCmd::Forward => self.shortcut("fwd"),
            UiCmd::Key { k } => {
                if is_shortcut(&k) {
                    self.shortcut(&k);
                }
            }
            UiCmd::Zoom { delta } => self.zoom(delta.clamp(-50, 50)),
            UiCmd::ClosePopup => {}
            UiCmd::OpenUrl { url } => {
                if url.starts_with("https://") || url.starts_with("http://") {
                    self.new_tab(url, false);
                }
            }
            UiCmd::HistoryQuery { q, offset } => self.send_history(&q, offset),
            UiCmd::HistoryDelete { ids } => {
                self.history.delete_ids(&ids);
                self.ui_eval("fx.historyChanged()");
            }
            UiCmd::HistoryDeleteHost { host } => {
                if !host.is_empty() {
                    self.history.delete_host(&host);
                }
                self.ui_eval("fx.historyChanged()");
            }
            UiCmd::HistoryClear { range } => {
                let secs = match range.as_str() {
                    "hour" => 3600,
                    "day" => 86_400,
                    "week" => 7 * 86_400,
                    "month" => 28 * 86_400,
                    "all" => 0,
                    _ => return,
                };
                self.history.delete_since(secs);
                self.ui_eval("fx.historyChanged()");
            }
            UiCmd::Restart => self.begin_exit(Exit::Restart),
            UiCmd::ResetSettings => {
                let (ubo, clean) = (self.settings.ubo_offered, self.settings.ext_cleanup_done);
                self.settings = Settings { ubo_offered: ubo, ext_cleanup_done: clean, ..Settings::default() };
                self.apply_settings();
            }
            UiCmd::OpenDownloads => {
                if let Some(dir) = download_dir_of(&self.settings) {
                    let _ = std::fs::create_dir_all(&dir);
                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                }
            }
            UiCmd::Reload => {
                if let Some(wv) = self.active_wv() {
                    wv.reload();
                }
            }
            UiCmd::Home => self.navigate_active(self.resolve_input(&self.settings.home_url.clone())),
            UiCmd::Panel { name } => {
                let name = name.filter(|n| matches!(n.as_str(), "control" | "shield" | "extensions"));
                self.panel = if self.panel == name { None } else { name };
                self.layout();
                self.push_state();
            }
            UiCmd::Settings { settings } => {
                // Le drapeau interne n'est jamais modifiable depuis l'interface.
                let (ubo, clean) = (self.settings.ubo_offered, self.settings.ext_cleanup_done);
                self.settings = Settings { ubo_offered: ubo, ext_cleanup_done: clean, ..settings }.sanitized();
                self.apply_settings();
            }
            UiCmd::AllowHttp { url } => {
                if let Some(host) = blocker::host_of(&url) {
                    if let Ok(mut s) = self.shared.http_allowed.lock() {
                        s.insert(host.to_ascii_lowercase());
                    }
                    if url.to_ascii_lowercase().starts_with("http://") {
                        self.navigate_active(url);
                    }
                }
            }
            UiCmd::Drag => {
                let _ = self.window.drag_window();
            }
            UiCmd::Minimize => self.window.set_minimized(true),
            UiCmd::Maximize => {
                self.window.set_maximized(!self.window.is_maximized());
                self.layout();
                self.push_state();
            }
            UiCmd::Quit => self.quit(),
            UiCmd::Resize { dir } => {
                use tao::window::ResizeDirection as D;
                let d = match dir.as_str() {
                    "n" => D::North, "s" => D::South, "e" => D::East, "w" => D::West,
                    "ne" => D::NorthEast, "nw" => D::NorthWest, "se" => D::SouthEast, "sw" => D::SouthWest,
                    _ => return,
                };
                let _ = self.window.drag_resize_window(d);
            }
            UiCmd::OpenPage { name } => {
                let url = match name.as_str() {
                    "settings" => SETTINGS_URL,
                    "extensions" => EXT_URL,
                    "history" => HISTORY_URL,
                    _ => return,
                };
                if let Some(t) = self.tabs.iter().find(|t| t.url == url) {
                    let id = t.id;
                    self.switch(id);
                } else {
                    self.new_tab(url.into(), false);
                }
            }
            UiCmd::ExtInstall { .. } | UiCmd::ExtRemove { .. } | UiCmd::ExtEnable { .. } | UiCmd::ExtOpen { .. } => {
                self.ext_set_status(false, EXT_UNSUPPORTED.into());
            }
            UiCmd::ClearData => {
                // Le contexte (profil) est partagé par toutes les vues.
                engine::clear_browsing_data();
                self.notice = Some(serde_json::json!({"kind": "cleared", "url": ""}));
                self.push_state();
            }
        }
    }

    // ---------------------------------------------------------------- réglages
    fn apply_settings(&mut self) {
        winstyle::border_color(&self.window, &self.settings.accent);
        self.shared.adblock.store(self.settings.adblock, Ordering::Relaxed);
        self.shared.https_only.store(self.settings.https_only, Ordering::Relaxed);
        self.shared.block_perms.store(self.settings.block_permissions, Ordering::Relaxed);
        if let Ok(mut d) = self.shared.download_dir.lock() {
            *d = download_dir_of(&self.settings);
        }
        self.settings.save();
        self.enforce_limits();
        self.push_state();
    }

    fn ext_set_status(&mut self, busy: bool, msg: String) {
        self.ext_status = Some((busy, msg));
        self.push_state();
    }

    /// Charge depuis le cache (ou télécharge en arrière-plan) les icônes des raccourcis
    /// et des sites les plus visités.
    fn refresh_icons(&mut self) {
        if !self.settings.site_icons {
            return;
        }
        // (url, service de secours autorisé ?) : raccourcis oui, sites de l'historique non.
        let mut urls: Vec<(String, bool)> = self.settings.home_tiles.iter().map(|t| (t.url.clone(), self.settings.icon_service)).collect();
        urls.extend(self.history.top_sites(8).into_iter().map(|(_, url, _, _)| (url, false)));
        let mut todo = Vec::new();
        for (url, service) in urls {
            let host = icons::host_key(&url);
            if host.is_empty() || self.icons_pending.contains(&host) {
                continue;
            }
            match icons::cached(&host) {
                Some((data, fresh)) => {
                    self.icon_map.insert(host.clone(), data);
                    if !fresh {
                        todo.push((host, url, service));
                    }
                }
                None => todo.push((host, url, service)),
            }
        }
        for (h, _, _) in &todo {
            self.icons_pending.insert(h.clone());
        }
        if !todo.is_empty() {
            let p = self.proxy.clone();
            std::thread::spawn(move || {
                for (host, url, service) in todo {
                    let _ = p.send_event(UserEvent::Icon(host, icons::fetch(&url, service)));
                }
            });
        }
    }

    fn send_icons(&self) {
        let map = if self.settings.site_icons { serde_json::json!(self.icon_map) } else { serde_json::json!({}) };
        self.ui_eval(&format!("fx.icons({map})"));
    }

    fn tick(&mut self) {
        if let Some((_, since)) = self.exiting {
            // Le moteur n'a pas tout fermé à temps : on s'arrête quand même.
            if engine::live_browsers() == 0 || since.elapsed() > Duration::from_secs(3) {
                self.finish_exit();
            }
            return;
        }
        self.ticks = self.ticks.wrapping_add(1);
        if self.session_dirty && self.settings.restore_session {
            self.save_session();
        }
        // Fenêtre en arrière-plan ou réduite : mesures 3× moins fréquentes.
        if !self.focused && self.ticks % 3 != 0 {
            return;
        }
        self.usage = self.monitor.sample();
        self.enforce_limits();
        self.push_state();
    }

    fn push_state(&self) {
        // RAM par onglet : processus de rendu qui affiche son cadre principal
        // (si plusieurs onglets partagent un processus, sa mémoire est répartie).
        let pid_of = |t: &Tab| -> Option<u32> {
            t.pid
        };
        let mut shares: std::collections::HashMap<u32, usize> = Default::default();
        for t in self.tabs.iter().filter(|t| t.wv.is_some()) {
            if let Some(pid) = pid_of(t) {
                *shares.entry(pid).or_default() += 1;
            }
        }
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .map(|t| {
                let (mem, shared) = match pid_of(t).filter(|_| t.wv.is_some()) {
                    Some(pid) => {
                        let n = shares.get(&pid).copied().unwrap_or(1).max(1);
                        let bytes = self.usage.per_pid.get(&pid).map(|m| m.0).unwrap_or(0);
                        (Some(bytes / n as u64 / (1024 * 1024)), n > 1)
                    }
                    None => (None, false),
                };
                serde_json::json!({
                    "id": t.id,
                    "title": t.title,
                    "url": if t.url == HOME_URL { "" } else { t.url.as_str() },
                    "page": internal_page(&t.url),
                    "active": Some(t.id) == self.active,
                    "loading": t.loading,
                    "sleeping": t.wv.is_none() && internal_page(&t.url).is_none(),
                    "private": t.private,
                    "blocked": t.blocked + t.blocked_net,
                    "favicon": t.favicon,
                    "zoom": t.zoom,
                    "can_back": t.can_back,
                    "can_fwd": t.can_fwd,
                    "mem": mem,
                    "suspended": false,
                    "shared": shared,
                })
            })
            .collect();
        let state = serde_json::json!({
            "tabs": tabs,
            "panel": self.panel,
            "settings": self.settings,
            // Panneau Control fermé : seule la RAM (arrondie à 5 Mo) est affichée → l'interface
            // n'est redessinée que si ce chiffre change.
            "usage": if self.panel.as_deref() != Some("control") {
                serde_json::json!({"ram": self.usage.ram_mb / 5 * 5, "cpu": 0, "procs": 0, "sys_total": 0, "sys_used": 0, "cats": [], "per_tab": false})
            } else { serde_json::json!({
                "ram": self.usage.ram_mb, "cpu": self.usage.cpu_pct, "procs": self.usage.procs,
                "sys_total": self.usage.sys_total_mb, "sys_used": self.usage.sys_used_mb,
                "cats": self.usage.categories,
                "per_tab": true,
            }) },
            "history_count": self.history.len(),
            "blocked_total": self.blocked_total,
            "blocklist": self.shared.blocker.len(),
            "notice": self.notice,
            "layout": {
                "top": TOP_H,
                "border": if self.window.is_maximized() { 0 } else { BORDER },
                "panel": PANEL_W,
                "radius": if self.window.is_maximized() { 0 } else { self.corner_radius },
            },
            "maximized": self.window.is_maximized(),
            "exts": [],
            "fullscreen": self.fullscreen,
            "popup": false,
            "closed": self.closed.len(),
            "restart_needed": boot_flags(&self.settings) != self.boot_flags,
            "ext_supported": false,
            "ext_status": self.ext_status.as_ref().map(|(busy, msg)| serde_json::json!({"busy": busy, "msg": msg})),
            "about": {
                "version": env!("CARGO_PKG_VERSION"),
                "engine": format!("Chromium {} (CEF), intégré à Ferrox", engine::chromium_version()),
                "downloads": dirs::download_dir().map(|d| d.display().to_string()).unwrap_or_default(),
                "config": config::config_dir().display().to_string(),
            },
        });
        // serde_json produit un littéral JS sûr : aucune injection possible.
        let js = format!("window.fx && fx.render({state})");
        // Rien n'a changé → on n'envoie rien (l'interface ne se redessine pas).
        if *self.last_state.borrow() == js {
            return;
        }
        self.ui.eval(&js);
        *self.last_state.borrow_mut() = js;
    }
}

/// Signaux de vie privée + raccourcis clavier relayés depuis les pages.
const PRIVACY_SCRIPT: &str = r#"(() => {
  if (__DNT__) {
    try { Object.defineProperty(Navigator.prototype, 'globalPrivacyControl', { get: () => true, configurable: true }); } catch (_) {}
    try { Object.defineProperty(Navigator.prototype, 'doNotTrack', { get: () => '1', configurable: true }); } catch (_) {}
  }
  if (window.top !== window) return;
  const send = __fxSend;
  addEventListener('keydown', (e) => {
    if (!e.isTrusted) return; // ignore les faux événements créés par la page
    const k = e.key.toLowerCase();
    let c = null;
    if (e.ctrlKey && !e.altKey && !e.metaKey) {
      if (k === 'tab') c = e.shiftKey ? 'prev' : 'next';
      else if (e.shiftKey && k === 't') c = 'T';
      else if (e.shiftKey && k === 'n') c = 'n';
      else if (!e.shiftKey && ['t', 'w', 'l', 'r', 'h'].includes(k)) c = k;
      else if (!e.shiftKey && /^[1-9]$/.test(k)) c = k;
      else if (k === '+' || k === '=') c = 'zin';
      else if (k === '-') c = 'zout';
      else if (k === '0') c = 'z0';
    } else if (e.altKey && !e.ctrlKey && k === 'arrowleft') c = 'back';
    else if (e.altKey && !e.ctrlKey && k === 'arrowright') c = 'fwd';
    else if (k === 'f5' && !e.ctrlKey) c = 'r';
    else if (k === 'f11') c = 'f11';
    else if (k === 'escape' && !document.fullscreenElement) { send('k:esc'); return; }
    if (c) { e.preventDefault(); e.stopPropagation(); send('k:' + c); }
  }, true);
  addEventListener('mousedown', (e) => { if (e.isTrusted) send('x'); }, true);
  // Raccourcis de la page d'accueil (document chargé par Ferrox uniquement).
  if (location.protocol === 'data:') {
    document.addEventListener('fx-tiles', (e) => {
      if (typeof e.detail === 'string' && e.detail.length < 16000) send('h:' + e.detail);
    });
  }
  const icon = () => {
    const l = document.querySelector('link[rel~="icon"][href], link[rel="shortcut icon"][href], link[rel="apple-touch-icon"][href]');
    let u = l ? l.href : (location.protocol === 'https:' ? location.origin + '/favicon.ico' : '');
    if (u && u.startsWith('https://') && u.length < 1000) send('f:' + u);
  };
  if (document.readyState === 'complete') icon(); else addEventListener('load', icon);
})();"#;

#[cfg(test)]
mod tests {
    use super::percent_encode;

    #[test]
    fn encode() {
        assert_eq!(percent_encode("rust lang&x"), "rust+lang%26x");
        assert_eq!(percent_encode("é"), "%C3%A9");
    }
}
