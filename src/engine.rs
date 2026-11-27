//! Couche moteur : Chromium Embedded Framework (CEF).
//!
//! - Chaque vue (interface ou onglet) est un navigateur CEF « fenêtre enfant » de la fenêtre
//!   principale de Ferrox.
//! - CEF tourne sur son propre thread d'interface (`multi_threaded_message_loop`) ; la boucle
//!   d'événements de Ferrox (tao) reste sur le thread principal. Toutes les opérations sur les
//!   navigateurs sont postées sur le thread d'interface de CEF ([`on_ui`]) et tous les rappels
//!   de CEF repartent vers Ferrox sous forme d'événements ([`emit`]).
//! - Canal page → Ferrox : dans le processus de rendu, une fonction native est passée en
//!   argument au script d'initialisation de Ferrox, exécuté avant tout script du site. Elle
//!   n'est jamais exposée dans `window` : le site ne peut pas l'atteindre.

use crate::UserEvent;
use cef::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use tao::event_loop::EventLoopProxy;

// ---------------------------------------------------------------------------
// Liens avec la boucle d'événements de Ferrox
// ---------------------------------------------------------------------------

static PROXY: OnceLock<Mutex<EventLoopProxy<UserEvent>>> = OnceLock::new();
/// Fenêtre principale (parent des vues et des boîtes de dialogue).
static MAIN_HWND: AtomicUsize = AtomicUsize::new(0);
/// Navigateurs CEF encore ouverts (pour s'arrêter proprement).
static LIVE: AtomicUsize = AtomicUsize::new(0);

pub fn set_proxy(p: EventLoopProxy<UserEvent>, hwnd: isize) {
    let _ = PROXY.set(Mutex::new(p));
    MAIN_HWND.store(hwnd as usize, Ordering::Relaxed);
}

pub fn emit(ev: UserEvent) {
    if let Some(p) = PROXY.get().and_then(|p| p.lock().ok()) {
        let _ = p.send_event(ev);
    }
}

pub fn live_browsers() -> usize {
    LIVE.load(Ordering::SeqCst)
}

fn cs(s: &str) -> CefString {
    CefString::from(s)
}

fn us(s: &CefStringUserfree) -> String {
    CefString::from(s).to_string()
}

fn opt(s: Option<&CefString>) -> String {
    s.map(|s| s.to_string()).unwrap_or_default()
}

/// Lit une liste de chaînes appartenant à CEF (sans la libérer).
fn list_strings(list: &mut CefStringList) -> Vec<String> {
    let raw: *mut sys::_cef_string_list_t = list.into();
    if raw.is_null() {
        return Vec::new();
    }
    // SAFETY: liste valide pendant le rappel ; chaque valeur copiée est libérée après lecture.
    unsafe {
        let n = sys::cef_string_list_size(raw).min(64);
        (0..n)
            .filter_map(|i| {
                let mut v: sys::cef_string_t = std::mem::zeroed();
                if sys::cef_string_list_value(raw, i, &mut v) == 0 {
                    return None;
                }
                let s = CefString::from(std::ptr::from_ref(&v)).to_string();
                sys::cef_string_utf16_clear(&mut v);
                Some(s)
            })
            .collect()
    }
}

type Job = Box<dyn FnOnce() + Send>;

wrap_task! {
    struct UiJob {
        job: Arc<Mutex<Option<Job>>>,
    }

    impl Task {
        fn execute(&self) {
            let job = self.job.lock().ok().and_then(|mut j| j.take());
            if let Some(job) = job {
                job();
            }
        }
    }
}

/// Exécute `f` sur le thread d'interface de CEF.
pub fn on_ui(f: impl FnOnce() + Send + 'static) {
    let mut task = UiJob::new(Arc::new(Mutex::new(Some(Box::new(f)))));
    post_task(ThreadId::UI, Some(&mut task));
}

// ---------------------------------------------------------------------------
// Démarrage
// ---------------------------------------------------------------------------

/// Options du moteur fixées au démarrage (redémarrage requis pour les changer).
#[derive(Clone, Default)]
pub struct BootOptions {
    pub eco_mode: bool,
    pub allow_autoplay: bool,
    pub hardware_accel: bool,
    pub anti_fingerprint: bool,
}

fn switches(o: &BootOptions) -> Vec<(&'static str, Option<&'static str>)> {
    let mut v = vec![
        // Les onglets d'un même site partagent un processus : moins de RAM.
        ("process-per-site", None),
        // Aucun signalement envoyé à des tiers.
        ("no-pings", None),
        ("disable-domain-reliability", None),
        ("disable-breakpad", None),
        ("disable-component-update", None),
    ];
    if o.eco_mode {
        v.push(("renderer-process-limit", Some("4")));
        v.push(("enable-low-end-device-mode", None));
    }
    if o.allow_autoplay {
        v.push(("autoplay-policy", Some("no-user-gesture-required")));
    }
    if !o.hardware_accel {
        v.push(("disable-gpu", None));
        v.push(("disable-gpu-compositing", None));
    }
    if o.anti_fingerprint {
        v.push(("disable-webgl", None));
        v.push(("disable-3d-apis", None));
    }
    v
}

wrap_app! {
    struct FxApp {
        boot: Option<Arc<BootOptions>>,
    }

    impl App {
        fn on_before_command_line_processing(&self, process_type: Option<&CefString>, command_line: Option<&mut CommandLine>) {
            // Processus principal uniquement : les sous-processus héritent des options.
            let (Some(boot), Some(cl)) = (self.boot.as_ref(), command_line) else { return };
            if !opt(process_type).is_empty() {
                return;
            }
            for (name, value) in switches(boot) {
                match value {
                    Some(v) => cl.append_switch_with_value(Some(&cs(name)), Some(&cs(v))),
                    None => cl.append_switch(Some(&cs(name))),
                }
            }
        }

        fn render_process_handler(&self) -> Option<RenderProcessHandler> {
            Some(FxRender::new(0))
        }
    }
}

/// Point d'entrée commun à tous les processus. Renvoie `Some(code)` pour un sous-processus
/// (rendu, GPU, réseau…) qui a terminé, `None` dans le processus principal une fois CEF démarré.
pub fn start(args: &MainArgs, sandbox_info: *mut u8, boot: Option<BootOptions>, cache: PathBuf) -> Option<i32> {
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let mut app = FxApp::new(boot.map(Arc::new));
    let code = execute_process(Some(args), Some(&mut app), sandbox_info);
    if code >= 0 {
        return Some(code);
    }
    let root = cache.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| cache.clone());
    let settings = Settings {
        no_sandbox: (!cfg!(feature = "sandbox")) as _,
        multi_threaded_message_loop: 1,
        cache_path: cs(&cache.to_string_lossy()),
        root_cache_path: cs(&root.to_string_lossy()),
        persist_session_cookies: 1,
        locale: cs("fr"),
        accept_language_list: cs("fr-FR,fr,en-US,en"),
        log_severity: LogSeverity::DISABLE,
        background_color: 0xFF12_1019,
        ..Default::default()
    };
    if initialize(Some(args), Some(&settings), Some(&mut app), sandbox_info) != 1 {
        crate::fatal("Impossible de démarrer le moteur Chromium (CEF).\n\nRéinstallez Ferrox puis relancez-le.");
    }
    None
}

pub fn stop() {
    shutdown();
}

// ---------------------------------------------------------------------------
// Processus de rendu : script d'initialisation + canal natif
// ---------------------------------------------------------------------------

/// Script d'initialisation de chaque navigateur, reçu à sa création (identifiant → script).
static RENDER_INIT: LazyLock<Mutex<HashMap<i32, String>>> = LazyLock::new(Default::default);

wrap_v8_handler! {
    struct NativeSend {
        browser: Browser,
    }

    impl V8Handler {
        fn execute(&self, _name: Option<&CefString>, _object: Option<&mut V8Value>, arguments: Option<&[Option<V8Value>]>, _retval: Option<&mut Option<V8Value>>, _exception: Option<&mut CefString>) -> i32 {
            let Some(Some(arg)) = arguments.and_then(|a| a.first()) else { return 1 };
            if arg.is_string() == 0 {
                return 1;
            }
            let body = us(&arg.string_value());
            if body.len() > 64 * 1024 {
                return 1;
            }
            if let (Some(frame), Some(mut msg)) = (self.browser.main_frame(), process_message_create(Some(&cs("fx")))) {
                if let Some(list) = msg.argument_list() {
                    list.set_string(0, Some(&cs(&body)));
                }
                frame.send_process_message(ProcessId::BROWSER, Some(&mut msg));
            }
            1
        }
    }
}

wrap_render_process_handler! {
    struct FxRender {
        _unused: u8,
    }

    impl RenderProcessHandler {
        fn on_browser_created(&self, browser: Option<&mut Browser>, extra_info: Option<&mut DictionaryValue>) {
            let (Some(b), Some(extra)) = (browser, extra_info) else { return };
            let init = us(&extra.string(Some(&cs("fx_init"))));
            if let Ok(mut m) = RENDER_INIT.lock() {
                m.insert(b.identifier(), init);
            }
        }

        fn on_browser_destroyed(&self, browser: Option<&mut Browser>) {
            if let (Some(b), Ok(mut m)) = (browser, RENDER_INIT.lock()) {
                m.remove(&b.identifier());
            }
        }

        fn on_context_created(&self, browser: Option<&mut Browser>, frame: Option<&mut Frame>, context: Option<&mut V8Context>) {
            let (Some(browser), Some(frame), Some(context)) = (browser, frame, context) else { return };
            if frame.is_main() == 0 {
                return;
            }
            // Le processus de rendu signale son identifiant : RAM par onglet.
            if let Some(mut msg) = process_message_create(Some(&cs("fxpid"))) {
                if let Some(list) = msg.argument_list() {
                    list.set_int(0, std::process::id() as i32);
                }
                frame.send_process_message(ProcessId::BROWSER, Some(&mut msg));
            }
            let init = RENDER_INIT.lock().ok().and_then(|m| m.get(&browser.identifier()).cloned()).unwrap_or_default();
            if init.is_empty() {
                return;
            }
            // Le script est une fonction qui reçoit la fonction native en argument.
            let mut ret: Option<V8Value> = None;
            let mut exc: Option<V8Exception> = None;
            if context.eval(Some(&cs(&init)), Some(&cs("ferrox://init")), 1, Some(&mut ret), Some(&mut exc)) == 0 {
                return;
            }
            let Some(func) = ret else { return };
            let mut handler = NativeSend::new(browser.clone());
            let Some(native) = v8_value_create_function(Some(&cs("send")), Some(&mut handler)) else { return };
            let _ = func.execute_function_with_context(Some(context), None, Some(&[Some(native)]));
        }
    }
}

// ---------------------------------------------------------------------------
// Vues
// ---------------------------------------------------------------------------

pub type Msg = Arc<dyn Fn(&str, &str) + Send + Sync>;
pub type Filter = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Comportements fournis par Ferrox pour une vue.
#[derive(Clone)]
pub struct Hooks {
    /// Message reçu de la page : (contenu, adresse du document).
    pub on_message: Msg,
    /// Navigation du cadre principal autorisée ?
    pub allow_nav: Filter,
    /// Requête réseau à bloquer ?
    pub block_request: Option<Filter>,
    /// Nouvelle fenêtre demandée par la page (jamais ouverte telle quelle).
    pub on_popup: Arc<dyn Fn(String) + Send + Sync>,
    /// Dossier de téléchargement.
    pub download_dir: Arc<Mutex<Option<PathBuf>>>,
    /// Refuser toutes les demandes d'autorisation (caméra, micro, position…).
    pub block_perms: Arc<AtomicBool>,
}

/// Rectangle en pixels physiques, relatif à la fenêtre principale.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[derive(Default)]
struct Inner {
    browser: Mutex<Option<Browser>>,
    dead: AtomicBool,
    bounds: Mutex<Bounds>,
    visible: AtomicBool,
}

impl Inner {
    fn hwnd(&self) -> Option<sys::HWND> {
        let b = self.browser.lock().ok()?.clone()?;
        let h = b.host()?.window_handle();
        (!h.0.is_null()).then_some(h)
    }

    /// Applique position et visibilité à la fenêtre enfant du navigateur.
    fn apply(&self) {
        let Some(h) = self.hwnd() else { return };
        let r = self.bounds.lock().map(|b| *b).unwrap_or_default();
        let visible = self.visible.load(Ordering::SeqCst);
        win::place(h, r, visible);
        if let Some(host) = self.browser.lock().ok().and_then(|b| b.clone()).and_then(|b| b.host()) {
            host.was_hidden((!visible) as i32);
        }
    }
}

/// Une vue web. La détruire ferme le navigateur (et libère son processus de rendu).
pub struct View {
    inner: Arc<Inner>,
}

impl Drop for View {
    fn drop(&mut self) {
        self.inner.dead.store(true, Ordering::SeqCst);
        let inner = self.inner.clone();
        on_ui(move || {
            if let Some(host) = inner.browser.lock().ok().and_then(|b| b.clone()).and_then(|b| b.host()) {
                // Cache tout de suite la fenêtre : la fermeture peut prendre un instant.
                if let Some(h) = inner.hwnd() {
                    win::place(h, Bounds::default(), false);
                }
                host.close_browser(1);
            }
        });
    }
}

pub struct ViewOptions {
    pub id: u64,
    pub url: String,
    /// Fonction JavaScript `(send) => {…}` exécutée au début de chaque document.
    pub init: String,
    pub private: bool,
    pub bounds: Bounds,
    pub visible: bool,
    pub zoom: u32,
    /// Interface de Ferrox (pas un onglet).
    pub ui: bool,
}

fn zoom_level(percent: u32) -> f64 {
    // Chromium : facteur = 1.2 ^ niveau
    ((percent.max(25) as f64) / 100.0).ln() / 1.2f64.ln()
}

static PRIVATE_CTX: Mutex<Option<RequestContext>> = Mutex::new(None);

impl View {
    /// Vue déjà fermée (aucun navigateur).
    pub fn closed() -> View {
        let inner = Arc::new(Inner::default());
        inner.dead.store(true, Ordering::SeqCst);
        View { inner }
    }

    pub fn new(o: ViewOptions, hooks: Hooks) -> View {
        let inner = Arc::new(Inner::default());
        if let Ok(mut b) = inner.bounds.lock() {
            *b = o.bounds;
        }
        inner.visible.store(o.visible, Ordering::SeqCst);
        let view = View { inner: inner.clone() };
        let parent = MAIN_HWND.load(Ordering::Relaxed);
        on_ui(move || {
            if inner.dead.load(Ordering::SeqCst) {
                return;
            }
            let r = o.bounds;
            let mut info = WindowInfo::default().set_as_child(
                sys::HWND(parent as *mut _),
                &Rect { x: r.x, y: r.y, width: r.w.max(1), height: r.h.max(1) },
            );
            if !o.visible {
                info.style &= !win::WS_VISIBLE;
            }
            info.runtime_style = RuntimeStyle::ALLOY;
            let settings = BrowserSettings {
                background_color: 0xFF12_1019,
                javascript_close_windows: State::DISABLED,
                ..Default::default()
            };
            let mut extra = dictionary_value_create();
            if let Some(e) = extra.as_mut() {
                e.set_string(Some(&cs("fx_init")), Some(&cs(&o.init)));
            }
            let mut client = FxClient::new(o.id, o.ui, inner.clone(), hooks, o.zoom);
            let mut ctx = if o.private {
                let mut g = PRIVATE_CTX.lock().ok();
                let slot = g.as_deref_mut();
                match slot {
                    Some(slot) => {
                        if slot.is_none() {
                            // Chemin vide : contexte en mémoire uniquement (navigation privée).
                            *slot = request_context_create_context(Some(&RequestContextSettings::default()), None);
                        }
                        slot.clone()
                    }
                    None => None,
                }
            } else {
                None
            };
            browser_host_create_browser(Some(&info), Some(&mut client), Some(&cs(&o.url)), Some(&settings), extra.as_mut(), ctx.as_mut());
        });
        view
    }

    fn with_browser(&self, f: impl FnOnce(Browser) + Send + 'static) {
        let inner = self.inner.clone();
        on_ui(move || {
            if let Some(b) = inner.browser.lock().ok().and_then(|b| b.clone()) {
                f(b);
            }
        });
    }

    pub fn load_url(&self, url: &str) {
        let url = url.to_string();
        self.with_browser(move |b| {
            if let Some(f) = b.main_frame() {
                f.load_url(Some(&cs(&url)));
            }
        });
    }

    pub fn eval(&self, js: &str) {
        let js = js.to_string();
        self.with_browser(move |b| {
            if let Some(f) = b.main_frame() {
                f.execute_java_script(Some(&cs(&js)), Some(&cs("ferrox://ui")), 1);
            }
        });
    }

    pub fn set_bounds(&self, r: Bounds) {
        if let Ok(mut b) = self.inner.bounds.lock() {
            if *b == r {
                return;
            }
            *b = r;
        }
        let inner = self.inner.clone();
        on_ui(move || inner.apply());
    }

    pub fn set_visible(&self, on: bool) {
        if self.inner.visible.swap(on, Ordering::SeqCst) == on {
            return;
        }
        let inner = self.inner.clone();
        on_ui(move || inner.apply());
    }

    pub fn focus(&self) {
        self.with_browser(|b| {
            if let Some(h) = b.host() {
                h.set_focus(1);
            }
        });
    }

    pub fn zoom(&self, percent: u32) {
        self.with_browser(move |b| {
            if let Some(h) = b.host() {
                h.set_zoom_level(zoom_level(percent));
            }
        });
    }

    pub fn reload(&self) {
        self.with_browser(|b| b.reload());
    }

    pub fn go_back(&self) {
        self.with_browser(|b| b.go_back());
    }

    pub fn go_forward(&self) {
        self.with_browser(|b| b.go_forward());
    }

    /// Adresse actuelle du document principal (si le navigateur est déjà créé).
    pub fn url(&self) -> Option<String> {
        let b = self.inner.browser.lock().ok()?.clone()?;
        Some(us(&b.main_frame()?.url()))
    }
}

/// Efface cookies et cache du profil principal.
pub fn clear_browsing_data() {
    on_ui(|| {
        if let Some(ctx) = request_context_get_global_context() {
            if let Some(cm) = ctx.cookie_manager(None) {
                cm.delete_cookies(None, None, None);
            }
            ctx.clear_http_cache(None);
        }
    });
}

// ---------------------------------------------------------------------------
// Rappels du navigateur (thread d'interface / réseau de CEF)
// ---------------------------------------------------------------------------

/// Téléchargements déjà signalés comme terminés.
static DOWNLOADS_DONE: LazyLock<Mutex<HashSet<u32>>> = LazyLock::new(Default::default);

wrap_client! {
    struct FxClient {
        id: u64,
        ui: bool,
        inner: Arc<Inner>,
        hooks: Hooks,
        zoom: u32,
    }

    impl Client {
        fn display_handler(&self) -> Option<DisplayHandler> {
            Some(FxDisplay::new(self.id, self.ui))
        }

        fn life_span_handler(&self) -> Option<LifeSpanHandler> {
            Some(FxLifeSpan::new(self.id, self.inner.clone(), self.hooks.clone(), self.zoom))
        }

        fn load_handler(&self) -> Option<LoadHandler> {
            if self.ui { None } else { Some(FxLoad::new(self.id)) }
        }

        fn request_handler(&self) -> Option<RequestHandler> {
            Some(FxRequest::new(self.id, self.hooks.clone()))
        }

        fn download_handler(&self) -> Option<DownloadHandler> {
            if self.ui { None } else { Some(FxDownload::new(self.hooks.clone())) }
        }

        fn permission_handler(&self) -> Option<PermissionHandler> {
            Some(FxPermission::new(self.hooks.clone()))
        }

        fn context_menu_handler(&self) -> Option<ContextMenuHandler> {
            Some(FxMenu::new(0))
        }

        fn on_process_message_received(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, _source: ProcessId, message: Option<&mut ProcessMessage>) -> i32 {
            let (Some(frame), Some(msg)) = (frame, message) else { return 0 };
            if frame.is_main() == 0 {
                return 1;
            }
            let Some(args) = msg.argument_list() else { return 1 };
            match us(&msg.name()).as_str() {
                "fx" => {
                    let body = us(&args.string(0));
                    let url = us(&frame.url());
                    (self.hooks.on_message)(&body, &url);
                }
                "fxpid" if !self.ui => {
                    let pid = args.int(0);
                    if pid > 0 {
                        emit(UserEvent::TabPid(self.id, pid as u32));
                    }
                }
                _ => {}
            }
            1
        }
    }
}

wrap_display_handler! {
    struct FxDisplay {
        id: u64,
        ui: bool,
    }

    impl DisplayHandler {
        fn on_address_change(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, url: Option<&CefString>) {
            if self.ui || frame.is_none_or(|f| f.is_main() == 0) {
                return;
            }
            emit(UserEvent::Address(self.id, opt(url)));
        }

        fn on_title_change(&self, _browser: Option<&mut Browser>, title: Option<&CefString>) {
            if !self.ui {
                emit(UserEvent::Title(self.id, opt(title).chars().take(300).collect()));
            }
        }

        fn on_favicon_urlchange(&self, _browser: Option<&mut Browser>, icon_urls: Option<&mut CefStringList>) {
            if self.ui {
                return;
            }
            let Some(list) = icon_urls else { return };
            if let Some(u) = list_strings(list).into_iter().find(|u| u.starts_with("https://")) {
                if u.len() < 1000 && !u.contains(['"', '<', '>', ' ', '\\']) {
                    emit(UserEvent::Favicon(self.id, u));
                }
            }
        }

        fn on_fullscreen_mode_change(&self, _browser: Option<&mut Browser>, fullscreen: i32) {
            if !self.ui {
                emit(UserEvent::Fullscreen(self.id, fullscreen != 0));
            }
        }
    }
}

wrap_life_span_handler! {
    struct FxLifeSpan {
        id: u64,
        inner: Arc<Inner>,
        hooks: Hooks,
        zoom: u32,
    }

    impl LifeSpanHandler {
        fn on_before_popup(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _popup_id: i32, target_url: Option<&CefString>, _target_frame_name: Option<&CefString>, _target_disposition: WindowOpenDisposition, _user_gesture: i32, _popup_features: Option<&PopupFeatures>, _window_info: Option<&mut WindowInfo>, _client: Option<&mut Option<Client>>, _settings: Option<&mut BrowserSettings>, _extra_info: Option<&mut Option<DictionaryValue>>, _no_javascript_access: Option<&mut i32>) -> i32 {
            // Jamais de fenêtre surgissante : Ferrox décide (nouvel onglet ou refus).
            (self.hooks.on_popup)(opt(target_url));
            1
        }

        fn on_after_created(&self, browser: Option<&mut Browser>) {
            let Some(b) = browser else { return };
            LIVE.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut slot) = self.inner.browser.lock() {
                *slot = Some(b.clone());
            }
            if self.inner.dead.load(Ordering::SeqCst) {
                if let Some(h) = b.host() {
                    h.close_browser(1);
                }
                return;
            }
            if self.zoom != 100 {
                if let Some(h) = b.host() {
                    h.set_zoom_level(zoom_level(self.zoom));
                }
            }
            self.inner.apply();
            emit(UserEvent::Created(self.id));
        }

        fn do_close(&self, _browser: Option<&mut Browser>) -> i32 {
            0
        }

        fn on_before_close(&self, _browser: Option<&mut Browser>) {
            if let Ok(mut slot) = self.inner.browser.lock() {
                *slot = None;
            }
            LIVE.fetch_sub(1, Ordering::SeqCst);
            emit(UserEvent::BrowserClosed);
        }
    }
}

wrap_load_handler! {
    struct FxLoad {
        id: u64,
    }

    impl LoadHandler {
        fn on_loading_state_change(&self, browser: Option<&mut Browser>, is_loading: i32, can_go_back: i32, can_go_forward: i32) {
            let url = browser.and_then(|b| b.main_frame()).map(|f| us(&f.url())).unwrap_or_default();
            emit(UserEvent::Load(self.id, is_loading != 0, url, can_go_back != 0, can_go_forward != 0));
        }
    }
}

wrap_request_handler! {
    struct FxRequest {
        id: u64,
        hooks: Hooks,
    }

    impl RequestHandler {
        fn on_before_browse(&self, _browser: Option<&mut Browser>, frame: Option<&mut Frame>, request: Option<&mut Request>, _user_gesture: i32, _is_redirect: i32) -> i32 {
            let (Some(frame), Some(req)) = (frame, request) else { return 0 };
            if frame.is_main() == 0 {
                return 0;
            }
            // 1 = navigation annulée
            (!(self.hooks.allow_nav)(&us(&req.url()))) as i32
        }

        fn resource_request_handler(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, _request: Option<&mut Request>, _is_navigation: i32, _is_download: i32, _request_initiator: Option<&CefString>, _disable_default_handling: Option<&mut i32>) -> Option<ResourceRequestHandler> {
            self.hooks.block_request.clone().map(|f| FxResource::new(self.id, f))
        }
    }
}

wrap_resource_request_handler! {
    struct FxResource {
        id: u64,
        block: Filter,
    }

    impl ResourceRequestHandler {
        fn on_before_resource_load(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, request: Option<&mut Request>, _callback: Option<&mut Callback>) -> ReturnValue {
            match request {
                Some(r) if (self.block)(&us(&r.url())) => ReturnValue::CANCEL,
                _ => ReturnValue::CONTINUE,
            }
        }
    }
}

wrap_download_handler! {
    struct FxDownload {
        hooks: Hooks,
    }

    impl DownloadHandler {
        fn can_download(&self, _browser: Option<&mut Browser>, url: Option<&CefString>, _request_method: Option<&CefString>) -> i32 {
            let u = opt(url).to_ascii_lowercase();
            (u.starts_with("https://") || u.starts_with("http://") || u.starts_with("blob:") || u.starts_with("data:")) as i32
        }

        fn on_before_download(&self, _browser: Option<&mut Browser>, item: Option<&mut DownloadItem>, suggested_name: Option<&CefString>, callback: Option<&mut BeforeDownloadCallback>) -> i32 {
            let (Some(item), Some(cb)) = (item, callback) else { return 0 };
            let name = opt(suggested_name)
                .chars()
                .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
                .collect::<String>();
            let name = name.trim().trim_matches('.').to_string();
            let name = if name.is_empty() { "telechargement".to_string() } else { name };
            match self.hooks.download_dir.lock().ok().and_then(|d| d.clone()) {
                Some(dir) => {
                    let _ = std::fs::create_dir_all(&dir);
                    cb.cont(Some(&cs(&dir.join(&name).to_string_lossy())), 0);
                }
                None => cb.cont(Some(&cs(&name)), 1),
            }
            emit(UserEvent::Download(us(&item.url()), false));
            1
        }

        fn on_download_updated(&self, _browser: Option<&mut Browser>, item: Option<&mut DownloadItem>, _callback: Option<&mut DownloadItemCallback>) {
            let Some(item) = item else { return };
            if item.is_complete() != 0 {
                let first = DOWNLOADS_DONE.lock().map(|mut s| s.insert(item.id())).unwrap_or(false);
                if first {
                    emit(UserEvent::Download(us(&item.url()), true));
                }
            }
        }
    }
}

wrap_permission_handler! {
    struct FxPermission {
        hooks: Hooks,
    }

    impl PermissionHandler {
        fn on_request_media_access_permission(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, requesting_origin: Option<&CefString>, requested_permissions: u32, callback: Option<&mut MediaAccessCallback>) -> i32 {
            let Some(cb) = callback.map(|c| c.clone()) else { return 0 };
            if self.hooks.block_perms.load(Ordering::Relaxed) {
                cb.cancel();
                return 1;
            }
            let origin = opt(requesting_origin);
            std::thread::spawn(move || {
                if win::ask(&format!("{origin}\n\nsouhaite utiliser votre caméra et/ou votre micro.\n\nAutoriser ?")) {
                    cb.cont(requested_permissions);
                } else {
                    cb.cancel();
                }
            });
            1
        }

        fn on_show_permission_prompt(&self, _browser: Option<&mut Browser>, _prompt_id: u64, requesting_origin: Option<&CefString>, _requested_permissions: u32, callback: Option<&mut PermissionPromptCallback>) -> i32 {
            let Some(cb) = callback.map(|c| c.clone()) else { return 0 };
            if self.hooks.block_perms.load(Ordering::Relaxed) {
                cb.cont(PermissionRequestResult::DENY);
                return 1;
            }
            let origin = opt(requesting_origin);
            std::thread::spawn(move || {
                let ok = win::ask(&format!("{origin}\n\ndemande une autorisation (position, notifications…).\n\nAutoriser ?"));
                cb.cont(if ok { PermissionRequestResult::ACCEPT } else { PermissionRequestResult::DENY });
            });
            1
        }
    }
}

wrap_context_menu_handler! {
    struct FxMenu {
        _unused: u8,
    }

    impl ContextMenuHandler {
        fn on_before_context_menu(&self, _browser: Option<&mut Browser>, _frame: Option<&mut Frame>, params: Option<&mut ContextMenuParams>, model: Option<&mut MenuModel>) {
            // Ferrox dessine son propre menu ; le menu natif ne reste que dans les champs de saisie.
            let editable = params.is_some_and(|p| p.is_editable() != 0);
            if let (false, Some(m)) = (editable, model) {
                m.clear();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Win32
// ---------------------------------------------------------------------------

mod win {
    use super::{Bounds, MAIN_HWND};
    use std::sync::atomic::Ordering;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_TOP, IDYES, MB_ICONQUESTION, MB_YESNO, MessageBoxW, SW_HIDE, SW_SHOWNA, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE,
        SetWindowPos, ShowWindow,
    };
    pub use windows_sys::Win32::UI::WindowsAndMessaging::WS_VISIBLE;

    pub fn place(h: cef::sys::HWND, r: Bounds, visible: bool) {
        let h = h.0 as _;
        // SAFETY: fenêtre enfant d'un navigateur CEF encore ouvert ; appels asynchrones.
        unsafe {
            if visible {
                SetWindowPos(h, HWND_TOP, r.x, r.y, r.w.max(1), r.h.max(1), SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS);
                ShowWindow(h, SW_SHOWNA);
            } else {
                ShowWindow(h, SW_HIDE);
            }
        }
    }

    /// Question Oui / Non (bloquante : à appeler hors des threads de CEF et de Ferrox).
    pub fn ask(text: &str) -> bool {
        let w = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
        let (t, title) = (w(text), w("Ferrox — autorisation"));
        let parent = MAIN_HWND.load(Ordering::Relaxed) as _;
        // SAFETY: chaînes UTF-16 terminées par 0, valides pendant l'appel.
        unsafe { MessageBoxW(parent, t.as_ptr(), title.as_ptr(), MB_YESNO | MB_ICONQUESTION) == IDYES }
    }
}

/// Libère la capture de la souris prise par une vue (avant de déplacer / redimensionner la
/// fenêtre principale depuis l'interface). À appeler sur le thread d'interface de CEF.
pub fn release_capture() {
    // SAFETY: aucun argument ; agit sur le thread appelant.
    unsafe {
        windows_sys::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
    }
}

/// Version de Chromium embarquée (ex. « 158.0.8127.89 »).
pub fn chromium_version() -> String {
    let v = String::from_utf8_lossy(sys::CEF_VERSION);
    v.trim_end_matches('\0').rsplit("chromium-").next().unwrap_or("").to_string()
}
