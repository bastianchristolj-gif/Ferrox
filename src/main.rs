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

/// Pages internes dessinées par l'interface elle-même (aucune WebView web).
fn internal_page(url: &str) -> Option<&'static str> {
    match url {
        SETTINGS_URL => Some("settings"),
        EXT_URL => Some("extensions"),
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
        notice: None,
    };

    let start = std::env::args().nth(1).map(|a| app.resolve_input(&a));
    app.new_tab(start.as_deref().unwrap_or(HOME_URL).to_string(), false);

    let mut next_tick = Instant::now() + TICK;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(next_tick);
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                app.settings.save();
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent { event: WindowEvent::Resized(_), .. } => app.layout(),
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
            .with_additional_browser_args(WEBVIEW2_ARGS)
            .with_browser_extensions_enabled(true)
            .build_as_child(window)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        builder.build_as_child(window)
    }
}

#[cfg(target_os = "windows")]
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

fn tab_title(url: &str) -> String {
    match url {
        HOME_URL => "Nouvel onglet".into(),
        SETTINGS_URL => "Paramètres".into(),
        EXT_URL => "Extensions".into(),
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
        HOME_HTML
            .replace("__ACCENT__", &self.settings.accent)
            .replace("__SEARCH__", &search)
            .replace("__BODY__", &classes.join(" "))
    }

    fn new_tab(&mut self, url: String, private: bool) {
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            title: tab_title(&url),
            url,
            wv: None,
            last_active: Instant::now(),
            private,
            loading: false,
            blocked: 0,
        });
        self.switch(id);
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

        let mut builder = if private {
            // Même environnement (exigé par WebView2), mais session InPrivate éphémère.
            WebViewBuilder::new_with_web_context(&mut self.ctx).with_incognito(true)
        } else {
            WebViewBuilder::new_with_web_context(&mut self.ctx)
        };

        if adblock {
            builder = builder.with_initialization_script(sh.script.as_str());
        }
        builder = builder.with_initialization_script(PRIVACY_SCRIPT);

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
            .with_autoplay(false)
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
                    if b.len() > 16 {
                        return;
                    }
                    if let Some(n) = b.strip_prefix("b:").and_then(|n| n.parse::<u32>().ok()) {
                        let _ = p.send_event(UserEvent::Blocked(id, n.min(100_000)));
                    } else if let Some(k) = b.strip_prefix("k:") {
                        if matches!(k, "t" | "w" | "l" | "r" | "n" | "next" | "prev") {
                            let _ = p.send_event(UserEvent::Key(id, k.to_string()));
                        }
                    }
                }
            })
            .with_download_started_handler({
                let p = p.clone();
                move |url, path| {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().replace(['/', '\\'], "_"))
                        .filter(|n| !n.is_empty() && n != "." && n != "..")
                        .unwrap_or_else(|| "telechargement".into());
                    if let Some(dir) = dirs::download_dir() {
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
        harden_linux(&wv, &self.settings);

        if url != HOME_URL {
            let _ = wv.load_url(&url);
        }
        self.tabs[idx].wv = Some(wv);
    }

    fn switch(&mut self, id: u64) {
        let Some(idx) = self.idx(id) else { return };
        self.active = Some(id);
        self.notice = None;
        if self.tabs[idx].wv.is_none() {
            self.create_webview(idx);
        }
        let r = self.content_rect();
        for t in &mut self.tabs {
            if let Some(wv) = &t.wv {
                if t.id == id {
                    let _ = wv.set_bounds(r);
                    let _ = wv.set_visible(true);
                    let _ = wv.focus();
                    t.last_active = Instant::now();
                } else {
                    let _ = wv.set_visible(false);
                }
            }
        }
        self.enforce_limits();
        self.push_state();
    }

    fn close(&mut self, id: u64) {
        let Some(idx) = self.idx(id) else { return };
        let tab = self.tabs.remove(idx);
        drop(tab.wv); // détruit la WebView et libère la mémoire
        if self.tabs.is_empty() {
            // Dernier onglet fermé → on quitte le navigateur.
            self.settings.save();
            std::process::exit(0);
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
                        self.tabs[i].title = t;
                    }
                    self.push_state();
                }
            }
            UserEvent::Load(id, started, url) => {
                if let Some(i) = self.idx(id) {
                    let t = &mut self.tabs[i];
                    t.loading = started;
                    if started {
                        t.blocked = 0;
                    }
                    if url.starts_with("http") {
                        t.url = url;
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
                if self.active != Some(id) {
                    return;
                }
                match k.as_str() {
                    "t" => self.new_tab(HOME_URL.into(), false),
                    "n" => self.new_tab(HOME_URL.into(), true),
                    "w" => self.close(id),
                    "r" => {
                        if let Some(wv) = self.active_wv() {
                            let _ = wv.reload();
                        }
                    }
                    "l" => {
                        let _ = self.ui.focus();
                        let _ = self.ui.evaluate_script("window.fx && fx.focusUrl()");
                    }
                    "next" | "prev" => self.cycle(k == "next"),
                    _ => {}
                }
            }
        }
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
            UiCmd::Back => {
                if let Some(wv) = self.active_wv() {
                    let _ = wv.go_back();
                }
            }
            UiCmd::Forward => {
                if let Some(wv) = self.active_wv() {
                    let _ = wv.go_forward();
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
                self.settings = settings.sanitized();
                self.shared.adblock.store(self.settings.adblock, Ordering::Relaxed);
                self.shared.https_only.store(self.settings.https_only, Ordering::Relaxed);
                self.shared.block_perms.store(self.settings.block_permissions, Ordering::Relaxed);
                self.settings.save();
                self.enforce_limits();
                self.push_state();
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
            UiCmd::Quit => {
                self.settings.save();
                std::process::exit(0);
            }
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
                    self.new_tab(format!("chrome-extension://{id}/{rel}"), false);
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
        self.usage = self.monitor.sample();
        self.enforce_limits();
        self.push_state();
    }

    fn push_state(&self) {
        let tabs: Vec<_> = self
            .tabs
            .iter()
            .map(|t| {
                serde_json::json!({
                    "id": t.id,
                    "title": t.title,
                    "url": if t.url == HOME_URL || internal_page(&t.url).is_some() { "" } else { t.url.as_str() },
                    "page": internal_page(&t.url),
                    "active": Some(t.id) == self.active,
                    "loading": t.loading,
                    "sleeping": t.wv.is_none() && internal_page(&t.url).is_none(),
                    "private": t.private,
                    "blocked": t.blocked,
                })
            })
            .collect();
        let state = serde_json::json!({
            "tabs": tabs,
            "panel": self.panel,
            "settings": self.settings,
            "usage": {"ram": self.usage.ram_mb, "cpu": self.usage.cpu_pct, "procs": self.usage.procs},
            "blocked_total": self.blocked_total,
            "blocklist": self.shared.blocker.len(),
            "notice": self.notice,
            "layout": {"top": TOP_H, "border": if self.window.is_maximized() { 0 } else { BORDER }, "panel": PANEL_W},
            "maximized": self.window.is_maximized(),
            "exts": self.exts,
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
        let _ = self.ui.evaluate_script(&format!("window.fx && fx.render({state})"));
    }
}

/// Réglages WebKitGTK supplémentaires (Linux).
#[cfg(target_os = "linux")]
fn harden_linux(wv: &WebView, s: &Settings) {
    use webkit2gtk::{CacheModel, SettingsExt, WebContextExt, WebViewExt, WebsiteDataManagerExt};
    use wry::WebViewExtUnix;
    let wk = wv.webview();
    if let Some(st) = WebViewExt::settings(&wk) {
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
  try { Object.defineProperty(Navigator.prototype, 'globalPrivacyControl', { get: () => true, configurable: true }); } catch (_) {}
  try { Object.defineProperty(Navigator.prototype, 'doNotTrack', { get: () => '1', configurable: true }); } catch (_) {}
  if (window.top !== window) return;
  addEventListener('keydown', (e) => {
    if (!e.ctrlKey || e.altKey || e.metaKey) return;
    const k = e.key.toLowerCase();
    let c = null;
    if (k === 'tab') c = e.shiftKey ? 'prev' : 'next';
    else if (!e.shiftKey && ['t','w','l','r'].includes(k)) c = k;
    else if (e.shiftKey && k === 'n') c = 'n';
    if (c && window.ipc) { e.preventDefault(); e.stopPropagation(); window.ipc.postMessage('k:' + c); }
  }, true);
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
