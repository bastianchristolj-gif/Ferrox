//! Ferrox — navigateur web léger et sécurisé, interface "gaming", écrit en Rust.
//!
//! Architecture :
//! - une fenêtre native (tao) ;
//! - une WebView "interface" (onglets, barre d'adresse, barre latérale, panneaux)
//!   qui couvre toute la fenêtre ;
//! - une WebView par onglet, posée par-dessus dans la zone de contenu.
//!   Le moteur de rendu est celui du système (WebKitGTK / WebView2 / WKWebView) :
//!   aucun Chromium embarqué → binaire de quelques Mo et RAM réduite.
//! - Les onglets en arrière-plan sont mis en veille (WebView détruite, URL conservée).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod blocker;
mod config;
mod extensions;
mod history;
mod monitor;

use blocker::Blocker;
use config::Settings;
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
use wry::{
    BackgroundThrottlingPolicy, NewWindowResponse, PageLoadEvent, PermissionKind,
    PermissionResponse, Rect, WebContext, WebView, WebViewBuilder,
    dpi::{LogicalPosition, LogicalSize as WLogicalSize},
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

/// Pages internes dessinées par l'interface elle-même (aucune WebView web).
fn internal_page(url: &str) -> Option<&'static str> {
    match url {
        SETTINGS_URL => Some("settings"),
        EXT_URL => Some("extensions"),
        HISTORY_URL => Some("history"),
        _ => None,
    }
}

fn valid_ext_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| (b'a'..=b'p').contains(&b))
}

// ---------------------------------------------------------------------------
// Événements internes
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
enum UserEvent {
    Ui(String),
    Title(u64, String),
    Load(u64, bool, String),
    OpenTab(String),
    Blocked(u64, u32),
    Key(u64, String),
    NavBlocked(u64, String),
    HttpBlocked(u64, String),
    Download(String, bool),
    ExtDownloaded(Result<std::path::PathBuf, String>),
    ExtAdded(Result<String, String>),
    ExtList(Vec<(String, String, bool)>),
    ExtChanged(Option<String>, bool),
    Favicon(u64, String),
    Renderers(Vec<(u32, Vec<u32>)>),
    HomeTiles(u64, Vec<config::Tile>),
    Fullscreen(u64, bool),
    ClosePopup,
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
    ExtInstall { input: String },
    ExtRemove { id: String },
    ExtEnable { id: String, on: bool },
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
    wv: Option<WebView>,
    last_active: Instant,
    private: bool,
    loading: bool,
    blocked: u32,
    favicon: Option<String>,
    zoom: u32,
    can_back: bool,
    can_fwd: bool,
    /// Identifiant du cadre principal (Windows) pour mesurer la RAM de l'onglet.
    frame_id: Option<u32>,
    /// Onglet suspendu par le moteur (Windows).
    suspended: bool,
}

/// Drapeaux partagés avec les callbacks des WebViews (qui doivent être 'static).
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

struct App {
    window: Window,
    #[cfg(target_os = "linux")]
    fixed: gtk::Fixed,
    ui: WebView,
    ctx: WebContext,
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
    exts: Vec<extensions::ExtInfo>,
    /// (en cours ?, message) affiché sur la page Extensions.
    ext_status: Option<(bool, String)>,
    /// Rayon des coins arrondis par Windows 11 (0 ailleurs).
    corner_radius: u32,
    /// Onglets fermés récemment (Ctrl+Maj+T).
    closed: Vec<String>,
    /// Plein écran (vidéo ou F11) : le contenu occupe toute la fenêtre.
    fullscreen: bool,
    /// Fenêtre d'extension affichée sous le bouton Extensions (Windows).
    popup: Option<WebView>,
    /// Réglages lus au démarrage qui exigent un redémarrage pour changer.
    boot_flags: (bool, bool, bool),
    session_dirty: bool,
    ubo_checked: bool,
    history: history::History,
    /// Processus de rendu → cadres principaux (Windows).
    renderers: Vec<(u32, Vec<u32>)>,
    focused: bool,
    ticks: u32,
    last_state: std::cell::RefCell<String>,
}

fn main() {
    // Sandbox des processus web WebKitGTK (bubblewrap) si disponible.
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_FORCE_SANDBOX").is_none() && which("bwrap") {
        // SAFETY: appelé au tout début de main, avant la création de tout thread.
        unsafe { std::env::set_var("WEBKIT_FORCE_SANDBOX", "1") };
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let settings = Settings::load();
    let settings_boot = settings.clone();
    // Arguments du moteur (identiques pour toutes les vues, fixés au démarrage).
    let _ = BROWSER_ARGS.set(browser_args(&settings));

    let window = WindowBuilder::new()
        .with_title("Ferrox")
        .with_decorations(false) // barre de titre intégrée aux onglets
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_min_inner_size(LogicalSize::new(640.0, 400.0))
        .with_window_icon(app_icon());
    #[cfg(target_os = "windows")]
    let window = {
        use tao::platform::windows::WindowBuilderExtWindows;
        window.with_undecorated_shadow(true) // ombre native Windows malgré l'absence de bordure
    };
    let window = window
        .build(&event_loop)
        .unwrap_or_else(|e| fatal(&format!("Impossible de créer la fenêtre : {e}")));

    #[cfg(target_os = "linux")]
    let fixed = {
        use gtk::prelude::*;
        use tao::platform::unix::WindowExtUnix;
        let fixed = gtk::Fixed::new();
        let vbox = window.default_vbox().expect("vbox GTK");
        vbox.pack_start(&fixed, true, true, 0);
        fixed.show_all();
        fixed
    };

    let _ = std::fs::create_dir_all(config::data_dir());
    // Un seul contexte (et donc un seul processus réseau) pour l'interface et les onglets.
    let mut ctx = WebContext::new(Some(config::data_dir()));

    let ui = {
        let p = proxy.clone();
        let builder = WebViewBuilder::new_with_web_context(&mut ctx)
            .with_html(UI_HTML)
            .with_bounds(full_rect(&window))
            .with_background_color((18, 16, 25, 255))
            .with_devtools(cfg!(debug_assertions))
            .with_hotkeys_zoom(false)
            .with_ipc_handler(move |req| {
                let body = req.into_body();
                if body.len() <= 64 * 1024 {
                    let _ = p.send_event(UserEvent::Ui(body));
                }
            })
            // L'interface ne doit jamais naviguer ailleurs.
            .with_navigation_handler(|url| url.starts_with("about:") || url.starts_with("data:"))
            .with_new_window_req_handler(|_, _| NewWindowResponse::Deny);
        #[cfg(target_os = "windows")]
        let builder = {
            use wry::WebViewBuilderExtWindows;
            builder.with_default_context_menus(false).with_browser_accelerator_keys(false)
        };
        build(builder, &window, #[cfg(target_os = "linux")] &fixed).unwrap_or_else(|e| {
            fatal(&format!(
                "Impossible de démarrer le moteur web.\n\n{e}\n\n\
                 Sous Windows, installez « Microsoft Edge WebView2 Runtime » \
                 (https://go.microsoft.com/fwlink/p/?LinkId=2124703) puis relancez Ferrox."
            ))
        })
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
        #[cfg(target_os = "linux")]
        fixed,
        ui,
        ctx,
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
        exts: Vec::new(),
        ext_status: None,
        corner_radius: 0,
        closed: Vec::new(),
        fullscreen: false,
        popup: None,
        boot_flags: (settings_boot.allow_autoplay, settings_boot.hardware_accel, settings_boot.eco_mode),
        session_dirty: false,
        ubo_checked: false,
        history: history::History::load(),
        renderers: Vec::new(),
        focused: true,
        ticks: 0,
        last_state: Default::default(),
        notice: None,
    };

    #[cfg(target_os = "windows")]
    {
        // Windows 11 : coins arrondis + bordure native à la couleur d'accent (suit l'arrondi).
        if winstyle::round_corners(&app.window) {
            app.corner_radius = 8;
        }
        winstyle::border_color(&app.window, &app.settings.accent);
    }

    if app.settings.clear_on_exit {
        // Filet de sécurité : si l'effacement à la fermeture n'a pas eu le temps de finir.
        let _ = app.ui.clear_all_browsing_data();
    }
    #[cfg(target_os = "windows")]
    extensions::wv2::set_autofill(&app.ui, app.settings.autofill, app.settings.save_passwords);

    let start = std::env::args().nth(1).map(|a| app.resolve_input(&a));
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
            Event::WindowEvent { event: WindowEvent::Focused(f), .. } => app.focused = f,
            Event::UserEvent(ev) => app.on_event(ev),
            Event::NewEvents(tao::event::StartCause::ResumeTimeReached { .. }) => {
                next_tick = Instant::now() + TICK;
                *control_flow = ControlFlow::WaitUntil(next_tick);
                app.tick();
            }
            _ => {}
        }
    });
}

#[cfg(target_os = "windows")]
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

/// Affiche une erreur bloquante (boîte de dialogue sous Windows) puis quitte.
fn fatal(msg: &str) -> ! {
    eprintln!("{msg}");
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (text, title) = (w(msg), w("Ferrox"));
        // SAFETY: chaînes UTF-16 terminées par 0, valides pendant l'appel.
        unsafe { MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
    }
    std::process::exit(1)
}

#[cfg(target_os = "linux")]
fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

fn build(
    builder: WebViewBuilder<'_>,
    window: &Window,
    #[cfg(target_os = "linux")] fixed: &gtk::Fixed,
) -> wry::Result<WebView> {
    #[cfg(target_os = "linux")]
    {
        let _ = window;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(fixed)
    }
    #[cfg(target_os = "windows")]
    {
        use wry::WebViewBuilderExtWindows;
        // Toutes les vues partagent le même dossier de profil : WebView2 exige des
        // arguments identiques. On garde SmartScreen (anti-phishing) actif, que wry
        // désactive par défaut, et on coupe la télémétrie / les pings.
        builder
            .with_additional_browser_args(BROWSER_ARGS.get().map(String::as_str).unwrap_or(WEBVIEW2_ARGS))
            .with_browser_extensions_enabled(true)
            .build_as_child(window)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        builder.build_as_child(window)
    }
}

static BROWSER_ARGS: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Arguments du moteur WebView2 selon les réglages (lecture auto, GPU).
fn browser_args(s: &Settings) -> String {
    // Les onglets d'un même site partagent un processus : moins de RAM.
    let mut a = format!("{WEBVIEW2_ARGS} --process-per-site");
    if s.eco_mode {
        a.push_str(" --renderer-process-limit=4 --enable-low-end-device-mode");
    }
    if s.allow_autoplay {
        a.push_str(" --autoplay-policy=no-user-gesture-required");
    }
    if !s.hardware_accel {
        a.push_str(" --disable-gpu --disable-gpu-compositing");
    }
    a
}

fn download_dir_of(s: &Settings) -> Option<std::path::PathBuf> {
    if s.download_dir.is_empty() { dirs::download_dir() } else { Some(s.download_dir.clone().into()) }
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const WEBVIEW2_ARGS: &str = "--disable-features=msWebOOUI,msPdfOOUI,msEdgeShoppingAssistant,\
AutofillServerCommunication,msEdgeCollections --no-pings --disable-domain-reliability";

/// Icône de fenêtre (hexagone d'accent) générée en mémoire : aucun fichier à charger.
fn app_icon() -> Option<tao::window::Icon> {
    const N: u32 = 64;
    let mut rgba = Vec::with_capacity((N * N * 4) as usize);
    let c = N as f32 / 2.0;
    for y in 0..N {
        for x in 0..N {
            let (dx, dy) = ((x as f32 + 0.5 - c).abs(), (y as f32 + 0.5 - c).abs());
            let r = c - 2.0;
            // hexagone "pointe en haut"
            let inside = dx <= r * 0.866 && dy <= r - dx * 0.577;
            let edge = inside && !(dx <= r * 0.866 - 5.0 && dy <= r - 5.0 - dx * 0.577);
            // lettre F
            let (fx, fy) = (x as i32 - 24, y as i32 - 18);
            let letter = (0..6).contains(&fx) && (0..28).contains(&fy)
                || (0..20).contains(&fx) && (0..6).contains(&fy)
                || (0..15).contains(&fx) && (11..16).contains(&fy);
            let px = if letter && inside { [255, 255, 255, 255] }
                else if edge { [250, 30, 78, 255] }
                else if inside { [26, 10, 16, 255] }
                else { [0, 0, 0, 0] };
            rgba.extend_from_slice(&px);
        }
    }
    tao::window::Icon::from_rgba(rgba, N, N).ok()
}

fn logical_size(window: &Window) -> (u32, u32) {
    let s = window.inner_size().to_logical::<u32>(window.scale_factor());
    (s.width, s.height)
}

fn full_rect(window: &Window) -> Rect {
    let (w, h) = logical_size(window);
    Rect {
        position: LogicalPosition::new(0, 0).into(),
        size: WLogicalSize::new(w, h).into(),
    }
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
    fn content_rect(&self) -> Rect {
        let (w, h) = logical_size(&self.window);
        if self.fullscreen {
            return Rect { position: LogicalPosition::new(0, 0).into(), size: WLogicalSize::new(w, h).into() };
        }
        let b = if self.window.is_maximized() { 0 } else { BORDER };
        let panel = if self.panel.is_some() { PANEL_W } else { 0 };
        Rect {
            position: LogicalPosition::new(b, TOP_H).into(),
            size: WLogicalSize::new(
                w.saturating_sub(2 * b + panel).max(1),
                h.saturating_sub(TOP_H + b).max(1),
            )
            .into(),
        }
    }

    fn layout(&mut self) {
        self.close_popup();
        let _ = self.ui.set_bounds(full_rect(&self.window));
        let r = self.content_rect();
        for t in &self.tabs {
            if let Some(wv) = &t.wv {
                let _ = wv.set_bounds(r);
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
            frame_id: None,
            suspended: false,
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
        let rect = self.content_rect();
        let home = (url == HOME_URL).then(|| self.home_html());
        let adblock = self.settings.adblock;
        let zoom = self.tabs[idx].zoom;

        let mut builder = if private {
            // Même environnement (exigé par WebView2), mais session InPrivate éphémère.
            WebViewBuilder::new_with_web_context(&mut self.ctx).with_incognito(true)
        } else {
            WebViewBuilder::new_with_web_context(&mut self.ctx)
        };

        if adblock {
            builder = builder.with_initialization_script(sh.script.as_str());
        }
        builder = builder.with_initialization_script(PRIVACY_SCRIPT.replace("__DNT__", if self.settings.send_dnt { "true" } else { "false" }));

        let nav = {
            let (sh, p) = (sh.clone(), p.clone());
            move |url: String| -> bool {
                let lower = url.to_ascii_lowercase();
                if lower.starts_with("about:") || lower.starts_with("blob:") || lower.starts_with("data:") {
                    return true;
                }
                // Pages des extensions (popup, options) : Windows uniquement.
                if extensions::SUPPORTED && lower.starts_with("chrome-extension://") {
                    return true;
                }
                if !(lower.starts_with("https://") || lower.starts_with("http://")) {
                    // file://, javascript:, schémas d'applications… refusés.
                    return false;
                }
                if sh.adblock.load(Ordering::Relaxed) && sh.blocker.is_blocked_url(&url) {
                    let _ = p.send_event(UserEvent::NavBlocked(id, url));
                    return false;
                }
                if lower.starts_with("http://") && sh.https_only.load(Ordering::Relaxed) {
                    let host = blocker::host_of(&url).unwrap_or("").to_ascii_lowercase();
                    let allowed = sh.http_allowed.lock().map(|s| s.contains(&host)).unwrap_or(false);
                    if !is_local_host(&host) && !allowed {
                        let _ = p.send_event(UserEvent::HttpBlocked(id, url));
                        return false;
                    }
                }
                true
            }
        };

        let perms = sh.block_perms.clone();
        builder = builder
            .with_bounds(rect)
            .with_background_color((18, 16, 25, 255))
            .with_devtools(cfg!(debug_assertions))
            .with_autoplay(self.settings.allow_autoplay)
            .with_general_autofill_enabled(self.settings.autofill)
            .with_clipboard(true)
            .with_background_throttling(BackgroundThrottlingPolicy::Suspend)
            .with_navigation_handler(nav)
            .with_permission_handler(move |kind| {
                if !perms.load(Ordering::Relaxed) {
                    return PermissionResponse::Default;
                }
                let _: PermissionKind = kind;
                PermissionResponse::Deny
            })
            .with_new_window_req_handler({
                let (sh, p) = (sh.clone(), p.clone());
                move |url, _| {
                    let lower = url.to_ascii_lowercase();
                    let ok = (lower.starts_with("https://") || lower.starts_with("http://")
                        || (extensions::SUPPORTED && lower.starts_with("chrome-extension://")))
                        && !(sh.adblock.load(Ordering::Relaxed) && sh.blocker.is_blocked_url(&url));
                    if ok {
                        let _ = p.send_event(UserEvent::OpenTab(url));
                    }
                    NewWindowResponse::Deny
                }
            })
            .with_document_title_changed_handler({
                let p = p.clone();
                move |t| {
                    let _ = p.send_event(UserEvent::Title(id, t.chars().take(300).collect()));
                }
            })
            .with_on_page_load_handler({
                let p = p.clone();
                move |ev, url| {
                    let _ = p.send_event(UserEvent::Load(id, matches!(ev, PageLoadEvent::Started), url));
                }
            })
            // Canal minimal depuis les pages web : seulement des compteurs / raccourcis.
            .with_ipc_handler({
                let p = p.clone();
                move |req| {
                    let b = req.body();
                    if let Some(json) = b.strip_prefix("h:") {
                        // Raccourcis de la page d'accueil : accepté uniquement depuis la page d'accueil
                        // (document about:blank chargé par Ferrox), jamais depuis un site.
                        let uri = req.uri().to_string();
                        if b.len() <= 16 * 1024 && (uri.starts_with("about:blank") || uri.is_empty()) {
                            if let Ok(tiles) = serde_json::from_str::<Vec<config::Tile>>(json) {
                                let _ = p.send_event(UserEvent::HomeTiles(id, tiles));
                            }
                        }
                        return;
                    }
                    if b.len() > 1024 {
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
                        if u.starts_with("https://") && !u.contains(['"', '\\', '<', '>', ' ', '\n']) {
                            let _ = p.send_event(UserEvent::Favicon(id, u.to_string()));
                        }
                    } else if b == "x" {
                        let _ = p.send_event(UserEvent::ClosePopup);
                    }
                }
            })
            .with_download_started_handler({
                let p = p.clone();
                let dl_dir = sh.download_dir.clone();
                move |url, path| {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().replace(['/', '\\'], "_"))
                        .filter(|n| !n.is_empty() && n != "." && n != "..")
                        .unwrap_or_else(|| "telechargement".into());
                    if let Some(dir) = dl_dir.lock().ok().and_then(|d| d.clone()) {
                        *path = dir.join(name);
                    }
                    let _ = p.send_event(UserEvent::Download(url, false));
                    true
                }
            })
            .with_download_completed_handler({
                let p = p.clone();
                move |url, _, ok| {
                    let _ = p.send_event(UserEvent::Download(url, ok));
                }
            });

        if let Some(html) = home {
            builder = builder.with_html(html);
        }

        let Ok(wv) = build(builder, &self.window, #[cfg(target_os = "linux")] &self.fixed) else {
            return;
        };

        #[cfg(target_os = "linux")]
        harden_linux(&wv, &self.settings, { let p = p.clone(); move |on| { let _ = p.send_event(UserEvent::Fullscreen(id, on)); } });
        #[cfg(target_os = "windows")]
        {
            extensions::wv2::on_fullscreen(&wv, { let p = p.clone(); move |on| { let _ = p.send_event(UserEvent::Fullscreen(id, on)); } });
            extensions::wv2::on_favicon(&wv, { let p = p.clone(); move |u| {
                if u.starts_with("https://") && u.len() < 1000 && !u.contains(['"', '<', '>', ' ']) {
                    let _ = p.send_event(UserEvent::Favicon(id, u));
                }
            } });
            self.tabs[idx].frame_id = extensions::wv2::main_frame_id(&wv);
        }
        if zoom != 100 {
            let _ = wv.zoom(zoom as f64 / 100.0);
        }

        if url != HOME_URL {
            let _ = wv.load_url(&url);
        }
        self.tabs[idx].wv = Some(wv);
    }

    fn switch(&mut self, id: u64) {
        let Some(idx) = self.idx(id) else { return };
        if self.active != Some(id) {
            self.close_popup();
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
            let _ = self.ui.focus();
        }
        let r = self.content_rect();
        for t in &mut self.tabs {
            if let Some(wv) = &t.wv {
                if t.id == id {
                    let _ = wv.set_bounds(r);
                    let _ = wv.set_visible(true);
                    let _ = wv.focus();
                    t.last_active = Instant::now();
                    t.suspended = false;
                    #[cfg(target_os = "windows")]
                    extensions::wv2::set_background(wv, false);
                } else {
                    let _ = wv.set_visible(false);
                    #[cfg(target_os = "windows")]
                    extensions::wv2::set_background(wv, true);
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
        }
        let tab = self.tabs.remove(idx);
        self.session_dirty = true;
        if !tab.private && tab.url != HOME_URL {
            self.closed.push(tab.url.clone());
            if self.closed.len() > 25 {
                self.closed.remove(0);
            }
        }
        drop(tab.wv); // détruit la WebView et libère la mémoire
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
            if let Ok(u) = wv.url() {
                if u.starts_with("http") {
                    self.tabs[idx].url = u;
                }
            }
            drop(wv);
            self.tabs[idx].loading = false;
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

    fn active_wv(&self) -> Option<&WebView> {
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
            Some(wv) => {
                let _ = wv.load_url(&url);
            }
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
                    // Sites « à page unique » (YouTube…) : l'adresse change sans rechargement.
                    let cur = self.tabs[i].wv.as_ref().and_then(|w| w.url().ok());
                    if let Some(cur) = cur.filter(|u| u.starts_with("http")) {
                        if cur != self.tabs[i].url {
                            self.tabs[i].url = cur.clone();
                            self.session_dirty = true;
                            let nav = self.tabs[i].wv.as_ref().map(|w| (w.can_go_back().unwrap_or(false), w.can_go_forward().unwrap_or(false)));
                            if let Some((b, f)) = nav {
                                self.tabs[i].can_back = b;
                                self.tabs[i].can_fwd = f;
                            }
                        }
                        if !self.tabs[i].private && self.settings.history_enabled {
                            self.history.visit(&cur, &t);
                        }
                    }
                    self.push_state();
                }
            }
            UserEvent::Renderers(r) => self.renderers = r,
            UserEvent::HomeTiles(id, tiles) => {
                if self.idx(id).is_some_and(|i| self.tabs[i].url == HOME_URL) {
                    self.settings.home_tiles = config::sanitize_tiles(tiles);
                    self.settings.save();
                }
            }
            UserEvent::Load(id, started, url) => {
                if let Some(i) = self.idx(id) {
                    let t = &mut self.tabs[i];
                    t.loading = started;
                    if started {
                        t.blocked = 0;
                        if blocker::host_of(&url) != blocker::host_of(&t.url) {
                            t.favicon = None;
                        }
                    }
                    if url.starts_with("http") || url.starts_with("chrome-extension://") {
                        t.url = url;
                        self.session_dirty = true;
                    }
                    if let Some(wv) = &t.wv {
                        t.can_back = wv.can_go_back().unwrap_or(false);
                        t.can_fwd = wv.can_go_forward().unwrap_or(false);
                    }
                    if !started && !t.private && self.settings.history_enabled && t.url.starts_with("http") {
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
            UserEvent::ExtDownloaded(res) => match res {
                Ok(path) => self.ext_add(path),
                Err(e) => self.ext_set_status(false, e),
            },
            UserEvent::ExtAdded(res) => {
                match res {
                    Ok(name) => self.ext_set_status(false, format!("« {name} » installée ✔")),
                    Err(e) => self.ext_set_status(false, e),
                }
                self.ext_refresh();
            }
            UserEvent::ExtList(list) => {
                self.exts = list
                    .into_iter()
                    .map(|(id, name, enabled)| {
                        let mut info = extensions::info_from_disk(&id).unwrap_or_default();
                        info.id = id;
                        if info.name.is_empty() || info.name.starts_with("__MSG_") {
                            info.name = name;
                        }
                        info.enabled = enabled;
                        info
                    })
                    .collect();
                self.exts.sort_by_key(|e| e.name.to_lowercase());
                if !self.ubo_checked {
                    self.ubo_checked = true;
                    if !self.settings.ubo_offered {
                        // 1er lancement : installe uBlock Origin Lite (une seule fois, même si on la supprime ensuite).
                        self.settings.ubo_offered = true;
                        self.settings.save();
                        if !self.exts.iter().any(|e| e.id == extensions::UBOL_ID) {
                            self.ext_install(extensions::UBOL_ID);
                        }
                    }
                }
                self.push_state();
            }
            UserEvent::ExtChanged(removed, ok) => {
                if let Some(id) = removed.filter(|id| ok && valid_ext_id(id)) {
                    let _ = std::fs::remove_dir_all(extensions::ext_dir().join(&id));
                    self.ext_set_status(false, "Extension supprimée".into());
                } else if !ok {
                    self.ext_set_status(false, "L'opération a échoué".into());
                }
                self.ext_refresh();
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
            UserEvent::ClosePopup => self.close_popup(),
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
                    let _ = wv.reload();
                }
            }
            "back" => {
                if let Some(wv) = self.active_wv() {
                    let _ = wv.go_back();
                }
            }
            "fwd" => {
                if let Some(wv) = self.active_wv() {
                    let _ = wv.go_forward();
                }
            }
            "l" => {
                let _ = self.ui.focus();
                let _ = self.ui.evaluate_script("window.fx && fx.focusUrl()");
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
                self.close_popup();
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
            let _ = wv.zoom(z as f64 / 100.0);
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

    fn close_popup(&mut self) {
        if self.popup.take().is_some() {
            self.push_state();
        }
    }

    /// Ouvre la fenêtre (popup) d'une extension sous le bouton Extensions.
    fn open_ext_popup(&mut self, url: String) {
        self.close_popup();
        let (w, _) = logical_size(&self.window);
        let pw = 380u32;
        let right = if self.panel.is_some() { PANEL_W } else { 0 } + BORDER + 8;
        let rect = Rect {
            position: LogicalPosition::new(w.saturating_sub(pw + right), TOP_H + 6).into(),
            size: WLogicalSize::new(pw, 560).into(),
        };
        let p = self.proxy.clone();
        let p2 = self.proxy.clone();
        let builder = WebViewBuilder::new_with_web_context(&mut self.ctx)
            .with_bounds(rect)
            .with_background_color((24, 24, 27, 255))
            .with_devtools(cfg!(debug_assertions))
            .with_url(url)
            // Les liens ouverts depuis la popup vont dans un nouvel onglet.
            .with_navigation_handler(move |u| {
                if u.starts_with("chrome-extension://") || u.starts_with("about:") {
                    return true;
                }
                if u.starts_with("https://") || u.starts_with("http://") {
                    let _ = p.send_event(UserEvent::OpenTab(u));
                    let _ = p.send_event(UserEvent::ClosePopup);
                }
                false
            })
            .with_new_window_req_handler(move |u, _| {
                if u.starts_with("https://") || u.starts_with("http://") || u.starts_with("chrome-extension://") {
                    let _ = p2.send_event(UserEvent::OpenTab(u));
                }
                NewWindowResponse::Deny
            });
        if let Ok(wv) = build(builder, &self.window, #[cfg(target_os = "linux")] &self.fixed) {
            let _ = wv.focus();
            self.popup = Some(wv);
        }
        self.push_state();
    }

    fn ui_eval(&self, js: &str) {
        let _ = self.ui.evaluate_script(&format!("window.fx && {js}"));
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

    fn quit(&mut self) -> ! {
        self.settings.save();
        self.save_session();
        if self.settings.clear_on_exit {
            let _ = self.ui.clear_all_browsing_data();
            // laisse un court délai au moteur pour effacer (sinon : refait au prochain démarrage)
            std::thread::sleep(Duration::from_millis(400));
        }
        std::process::exit(0)
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
                self.last_state.borrow_mut().clear();
                self.push_state();
                self.ext_refresh();
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
            UiCmd::ClosePopup => self.close_popup(),
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
            UiCmd::Restart => {
                self.settings.save();
                self.save_session();
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe).spawn();
                }
                std::process::exit(0);
            }
            UiCmd::ResetSettings => {
                let ubo = self.settings.ubo_offered;
                self.settings = Settings { ubo_offered: ubo, ..Settings::default() };
                self.apply_settings();
            }
            UiCmd::OpenDownloads => {
                if let Some(dir) = download_dir_of(&self.settings) {
                    let _ = std::fs::create_dir_all(&dir);
                    #[cfg(target_os = "windows")]
                    let _ = std::process::Command::new("explorer").arg(&dir).spawn();
                    #[cfg(target_os = "linux")]
                    let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                    #[cfg(target_os = "macos")]
                    let _ = std::process::Command::new("open").arg(&dir).spawn();
                }
            }
            UiCmd::Reload => {
                if let Some(wv) = self.active_wv() {
                    let _ = wv.reload();
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
                let ubo = self.settings.ubo_offered;
                self.settings = Settings { ubo_offered: ubo, ..settings }.sanitized();
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
                if name == "extensions" {
                    self.ext_refresh();
                }
            }
            UiCmd::ExtInstall { input } => self.ext_install(&input),
            UiCmd::ExtRemove { id } => self.ext_remove(id),
            UiCmd::ExtEnable { id, on } => self.ext_enable(id, on),
            UiCmd::ExtOpen { id, page } => {
                // La page à ouvrir vient du manifeste lu sur le disque, jamais de l'interface.
                let target = self.exts.iter().find(|e| e.id == id).and_then(|e| match page.as_str() {
                    "popup" => e.popup.clone(),
                    "options" => e.options.clone(),
                    _ => None,
                });
                if let Some(rel) = target {
                    let url = format!("chrome-extension://{id}/{rel}");
                    if page == "popup" { self.open_ext_popup(url) } else { self.new_tab(url, false) }
                }
            }
            UiCmd::ClearData => {
                // Le contexte (profil) est partagé par toutes les vues.
                let _ = self.ui.clear_all_browsing_data();
                self.notice = Some(serde_json::json!({"kind": "cleared", "url": ""}));
                self.push_state();
            }
        }
    }

    // ------------------------------------------------------------- extensions
    fn apply_settings(&mut self) {
        #[cfg(target_os = "windows")]
        {
            winstyle::border_color(&self.window, &self.settings.accent);
            extensions::wv2::set_autofill(&self.ui, self.settings.autofill, self.settings.save_passwords);
        }
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

    fn ext_refresh(&mut self) {
        #[cfg(target_os = "windows")]
        {
            let p = self.proxy.clone();
            if let Err(e) = extensions::wv2::list(&self.ui, move |l| {
                let _ = p.send_event(UserEvent::ExtList(l));
            }) {
                self.ext_status = Some((false, e));
            }
        }
    }

    fn ext_install(&mut self, input: &str) {
        if !extensions::SUPPORTED {
            return self.ext_set_status(false, "Les extensions Chrome ne sont prises en charge que sous Windows (WebView2).".into());
        }
        let Some(id) = extensions::parse_store_id(input) else {
            return self.ext_set_status(false, "Lien ou ID invalide. Collez l'adresse de la page de l'extension sur le Chrome Web Store.".into());
        };
        self.ext_set_status(true, "Téléchargement depuis le Chrome Web Store…".into());
        let p = self.proxy.clone();
        std::thread::spawn(move || {
            let _ = p.send_event(UserEvent::ExtDownloaded(extensions::download_and_unpack(&id)));
        });
    }

    fn ext_add(&mut self, path: std::path::PathBuf) {
        let _ = &path;
        #[cfg(target_os = "windows")]
        {
            self.ext_set_status(true, "Installation…".into());
            let p = self.proxy.clone();
            if let Err(e) = extensions::wv2::add(&self.ui, &path, move |r| {
                let _ = p.send_event(UserEvent::ExtAdded(r));
            }) {
                self.ext_set_status(false, e);
            }
        }
    }

    fn ext_remove(&mut self, id: String) {
        let _ = &id;
        #[cfg(target_os = "windows")]
        {
            let p = self.proxy.clone();
            let id2 = id.clone();
            if let Err(e) = extensions::wv2::remove(&self.ui, id, move |ok| {
                let _ = p.send_event(UserEvent::ExtChanged(Some(id2), ok));
            }) {
                self.ext_set_status(false, e);
            }
        }
    }

    fn ext_enable(&mut self, id: String, on: bool) {
        let _ = (&id, on);
        #[cfg(target_os = "windows")]
        {
            let p = self.proxy.clone();
            if let Err(e) = extensions::wv2::enable(&self.ui, id, on, move |ok| {
                let _ = p.send_event(UserEvent::ExtChanged(None, ok));
            }) {
                self.ext_set_status(false, e);
            }
        }
    }

    fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        if self.session_dirty && self.settings.restore_session {
            self.save_session();
        }
        // Fenêtre en arrière-plan ou réduite : mesures 3× moins fréquentes.
        if !self.focused && self.ticks % 3 != 0 {
            return;
        }
        // Suspension des onglets cachés depuis plus de 30 s.
        #[cfg(target_os = "windows")]
        if self.settings.suspend_bg {
            let active = self.active;
            for t in self.tabs.iter_mut() {
                if Some(t.id) != active && !t.suspended && t.last_active.elapsed() > Duration::from_secs(30) {
                    if let Some(wv) = &t.wv {
                        t.suspended = extensions::wv2::try_suspend(wv);
                    }
                }
            }
        }
        self.usage = self.monitor.sample();
        #[cfg(target_os = "windows")]
        if self.panel.as_deref() == Some("control") {
            let p = self.proxy.clone();
            extensions::wv2::renderer_frames(&self.ui, move |r| {
                let _ = p.send_event(UserEvent::Renderers(r));
            });
        }
        self.enforce_limits();
        self.push_state();
    }

    fn push_state(&self) {
        // RAM par onglet : processus de rendu qui affiche son cadre principal
        // (si plusieurs onglets partagent un processus, sa mémoire est répartie).
        let pid_of = |t: &Tab| -> Option<u32> {
            let f = t.frame_id?;
            self.renderers.iter().find(|(_, frames)| frames.contains(&f)).map(|(pid, _)| *pid)
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
                    "blocked": t.blocked,
                    "favicon": t.favicon,
                    "zoom": t.zoom,
                    "can_back": t.can_back,
                    "can_fwd": t.can_fwd,
                    "mem": mem,
                    "suspended": t.suspended,
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
                "per_tab": cfg!(target_os = "windows"),
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
            "exts": self.exts,
            "fullscreen": self.fullscreen,
            "popup": self.popup.is_some(),
            "closed": self.closed.len(),
            "restart_needed": (self.settings.allow_autoplay, self.settings.hardware_accel, self.settings.eco_mode) != self.boot_flags
                && cfg!(target_os = "windows"),
            "ext_supported": extensions::SUPPORTED,
            "ext_status": self.ext_status.as_ref().map(|(busy, msg)| serde_json::json!({"busy": busy, "msg": msg})),
            "about": {
                "version": env!("CARGO_PKG_VERSION"),
                "engine": if cfg!(target_os = "windows") { "Microsoft Edge WebView2" } else if cfg!(target_os = "macos") { "WebKit (WKWebView)" } else { "WebKitGTK" },
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
        let _ = self.ui.evaluate_script(&js);
        *self.last_state.borrow_mut() = js;
    }
}

/// Réglages WebKitGTK supplémentaires (Linux).
#[cfg(target_os = "linux")]
fn harden_linux(wv: &WebView, s: &Settings, fullscreen: impl Fn(bool) + Clone + 'static) {
    use webkit2gtk::{CacheModel, HardwareAccelerationPolicy, SettingsExt, WebContextExt, WebViewExt, WebsiteDataManagerExt};
    use wry::WebViewExtUnix;
    let wk = wv.webview();
    // Vidéo en plein écran : Ferrox agrandit la vue à toute la fenêtre.
    let f2 = fullscreen.clone();
    wk.connect_enter_fullscreen(move |_| { fullscreen(true); false });
    wk.connect_leave_fullscreen(move |_| { f2(false); false });
    if let Some(st) = WebViewExt::settings(&wk) {
        if !s.hardware_accel {
            st.set_hardware_acceleration_policy(HardwareAccelerationPolicy::Never);
        }
        st.set_enable_dns_prefetching(false);
        st.set_javascript_can_open_windows_automatically(false);
        st.set_enable_page_cache(false); // économise la RAM
        st.set_enable_webgl(!s.anti_fingerprint);
        st.set_enable_webaudio(!s.anti_fingerprint);
        st.set_enable_media_stream(!s.block_permissions);
        st.set_enable_developer_extras(cfg!(debug_assertions));
        st.set_allow_file_access_from_file_urls(false);
        st.set_allow_universal_access_from_file_urls(false);
    }
    if let Some(ctx) = wk.context() {
        ctx.set_cache_model(CacheModel::DocumentBrowser);
        if let Some(dm) = ctx.website_data_manager() {
            dm.set_itp_enabled(true); // Intelligent Tracking Prevention
        }
    }
}

/// Signaux de vie privée + raccourcis clavier relayés depuis les pages.
const PRIVACY_SCRIPT: &str = r#"(() => {
  if (__DNT__) {
    try { Object.defineProperty(Navigator.prototype, 'globalPrivacyControl', { get: () => true, configurable: true }); } catch (_) {}
    try { Object.defineProperty(Navigator.prototype, 'doNotTrack', { get: () => '1', configurable: true }); } catch (_) {}
  }
  if (window.top !== window) return;
  const send = (m) => { try { window.ipc && window.ipc.postMessage(m); } catch (_) {} };
  addEventListener('keydown', (e) => {
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
  addEventListener('mousedown', () => send('x'), true);
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
