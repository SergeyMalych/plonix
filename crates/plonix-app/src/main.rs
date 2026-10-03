//! Plonix as a desktop app.
//!
//! The app window shows the Plonix UI, live against an engine: the one already
//! running (started by `plonix start` or `plonix open`), or one the app starts
//! inside its own process. The app signs the window in itself with a one-time
//! launch code, so there is no link to open and no terminal step.
//!
//! The window only ever shows the engine's own pages. Any other link opens in
//! the default browser.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use plonix_core::Engine;
use plonix_core::engine::{self, EngineConfig};
use plonix_core::paths::{EngineInfo, Home};
use tauri::menu::{Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindowBuilder, Wry};

/// Project the embedded engine records into, same default as `plonix ui`.
const PROJECT: &str = "default";
const DEFAULT_PROXY_PORT: u16 = 8080;
const DEFAULT_API_PORT: u16 = 8090;

/// Origin of the engine the window is signed in to, e.g. `http://127.0.0.1:8090`.
static ENGINE_ORIGIN: OnceLock<String> = OnceLock::new();
/// Set once the starting page has loaded, so errors can be shown on it.
static START_PAGE_READY: AtomicBool = AtomicBool::new(false);

/// Lets the UI adapt its wording when it runs inside the app.
const INIT_SCRIPT: &str = "window.__PLONIX_APP__ = true;";

/// Menu items that drive the UI: (id, label, accelerator, script).
const VIEW_ITEMS: [(&str, &str, &str, &str); 5] = [
    ("go-traffic", "Traffic", "CmdOrCtrl+1", "window.plonix && plonix.go('traffic')"),
    ("go-bench", "Bench", "CmdOrCtrl+2", "window.plonix && plonix.go('bench')"),
    ("go-scope", "Scope", "CmdOrCtrl+3", "window.plonix && plonix.go('scope')"),
    ("go-map", "Map", "CmdOrCtrl+4", "window.plonix && plonix.go('map')"),
    ("go-findings", "Findings", "CmdOrCtrl+5", "window.plonix && plonix.go('findings')"),
];
const OPEN_TARGET_SCRIPT: &str = "window.plonix && plonix.openTarget()";
const TOGGLE_SIDEBAR_SCRIPT: &str = "window.plonix && plonix.toggleSidebar()";
/// The usual shortcut for showing and hiding a sidebar.
const TOGGLE_SIDEBAR_KEY: &str = if cfg!(target_os = "macos") { "Ctrl+Cmd+S" } else { "Ctrl+Shift+S" };

/// An engine running inside the app's process.
struct Embedded {
    engine: Arc<Engine>,
    home: Home,
    // Keeps the engine's tasks alive for as long as the app runs.
    _runtime: tokio::runtime::Runtime,
}

#[derive(Default)]
struct EngineSlot(Mutex<Option<Embedded>>);

/// The engine the window talks to.
struct Connection {
    api: String,
    token: String,
    embedded: Option<Embedded>,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PLONIX_LOG").unwrap_or_else(|_| "plonix_core=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let app = tauri::Builder::default()
        .manage(EngineSlot::default())
        .menu(build_menu)
        .on_menu_event(|app, event| {
            let id = event.id().as_ref();
            let script = if id == "open-target" {
                Some(OPEN_TARGET_SCRIPT)
            } else if id == "toggle-sidebar" {
                Some(TOGGLE_SIDEBAR_SCRIPT)
            } else {
                VIEW_ITEMS.iter().find(|(item, ..)| *item == id).map(|(.., script)| *script)
            };
            if let (Some(script), Some(win)) = (script, app.get_webview_window("main")) {
                let _ = win.eval(script);
            }
        })
        .setup(|app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Plonix")
                .inner_size(1320.0, 840.0)
                .min_inner_size(900.0, 560.0)
                .initialization_script(INIT_SCRIPT)
                .on_navigation(allow_navigation)
                .on_page_load(|_, payload| {
                    if payload.event() == PageLoadEvent::Finished && !is_engine_page(payload.url()) {
                        START_PAGE_READY.store(true, Ordering::SeqCst);
                    }
                })
                .build()?;
            let handle = app.handle().clone();
            std::thread::Builder::new().name("plonix-connect".into()).spawn(move || connect_window(handle))?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start Plonix");

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            let slot = app.state::<EngineSlot>();
            if let Some(embedded) = slot.0.lock().unwrap().take() {
                stop_embedded(embedded);
            }
        }
    });
}

/// Connects to an engine and points the window at it.
fn connect_window(app: AppHandle) {
    let result = Home::resolve(None).and_then(|home| connect(&home)).and_then(|conn| {
        let url = launch_url(&conn)?;
        Ok((conn, url))
    });
    let Some(win) = app.get_webview_window("main") else { return };
    match result {
        Ok((conn, url)) => {
            let _ = ENGINE_ORIGIN.set(conn.api.clone());
            if let Some(embedded) = conn.embedded {
                *app.state::<EngineSlot>().0.lock().unwrap() = Some(embedded);
            }
            match Url::parse(&url) {
                Ok(url) => {
                    let _ = win.navigate(url);
                }
                Err(e) => show_error(&win, &format!("bad window address {url}: {e}")),
            }
        }
        Err(e) => show_error(&win, &format!("{e:#}")),
    }
}

fn show_error(win: &tauri::WebviewWindow, message: &str) {
    tracing::error!("{message}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !START_PAGE_READY.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let arg = serde_json::to_string(message).unwrap_or_else(|_| "\"\"".into());
    let _ = win.eval(format!("window.plonixError && plonixError({arg})"));
}

/// Uses the engine that is already running, else starts one in this process.
fn connect(home: &Home) -> Result<Connection> {
    home.ensure()?;
    let token = home.load_or_create_token()?;
    if let Some(info) = home.read_engine_info()
        && engine_answers(&info.api, &token)
    {
        tracing::info!("using the running engine at {}", info.api);
        return Ok(Connection { api: info.api, token, embedded: None });
    }
    let embedded = start_embedded(home)?;
    let info = home.read_engine_info().context("the engine did not announce itself")?;
    Ok(Connection { api: info.api, token, embedded: Some(embedded) })
}

fn engine_answers(api: &str, token: &str) -> bool {
    ureq::get(&format!("{api}/api/status"))
        .set("Authorization", &format!("Bearer {token}"))
        .set("X-Plonix-Client", "app")
        .timeout(Duration::from_secs(2))
        .call()
        .is_ok()
}

/// Starts the engine on its own runtime and announces it in `engine.json`,
/// so `plonix` commands in a terminal use the same engine as the window.
fn start_embedded(home: &Home) -> Result<Embedded> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("plonix-engine")
        .build()
        .context("starting the engine runtime")?;
    let config = EngineConfig {
        home: home.clone(),
        project: PROJECT.into(),
        proxy_addr: SocketAddr::from(([127, 0, 0, 1], DEFAULT_PROXY_PORT)),
        proxy_port_fallback: true,
        api_addr: SocketAddr::from(([127, 0, 0, 1], free_or_any(DEFAULT_API_PORT))),
        insecure_upstream: false,
    };
    let running = runtime.block_on(engine::start(&config)).context("starting the engine")?;
    let info = EngineInfo {
        pid: std::process::id(),
        api: format!("http://{}", running.api_addr),
        proxy: running.proxy_addr.to_string(),
        project: PROJECT.into(),
        started_at: running.engine.started_at,
    };
    std::fs::write(home.engine_file(), serde_json::to_vec_pretty(&info)?)
        .with_context(|| format!("writing {}", home.engine_file().display()))?;
    tracing::info!("engine started: proxy {}, API {}", running.proxy_addr, running.api_addr);
    Ok(Embedded { engine: running.engine, home: home.clone(), _runtime: runtime })
}

fn stop_embedded(embedded: Embedded) {
    embedded.engine.shutdown.notify_waiters();
    // Only remove the file if it still describes this process.
    if embedded.home.read_engine_info().is_some_and(|i| i.pid == std::process::id()) {
        let _ = std::fs::remove_file(embedded.home.engine_file());
    }
}

/// Keeps a preferred port when it is free, else lets the OS pick one.
fn free_or_any(port: u16) -> u16 {
    if TcpListener::bind(("127.0.0.1", port)).is_ok() { port } else { 0 }
}

/// A one-time address that opens the UI signed in.
fn launch_url(conn: &Connection) -> Result<String> {
    let resp: serde_json::Value = ureq::post(&format!("{}/api/ui/launch", conn.api))
        .set("Authorization", &format!("Bearer {}", conn.token))
        .set("X-Plonix-Client", "app")
        .timeout(Duration::from_secs(5))
        .send_json(serde_json::json!({}))
        .context("signing the window in")?
        .into_json()?;
    resp["url"].as_str().map(String::from).context("the engine did not return a window address")
}

fn origin(url: &Url) -> String {
    format!("{}://{}:{}", url.scheme(), url.host_str().unwrap_or(""), url.port_or_known_default().unwrap_or(0))
}

fn is_engine_page(url: &Url) -> bool {
    ENGINE_ORIGIN.get().is_some_and(|o| Url::parse(o).is_ok_and(|e| origin(&e) == origin(url)))
}

/// The window shows the starting page and the engine's UI, nothing else.
fn allow_navigation(url: &Url) -> bool {
    let start_page = url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost");
    if start_page || is_engine_page(url) {
        return true;
    }
    if matches!(url.scheme(), "http" | "https") {
        open_externally(url.as_str());
    }
    false
}

fn open_externally(url: &str) {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = std::process::Command::new("xdg-open");
    if let Err(e) = plonix_core::browser::spawn_detached(cmd.arg(url)) {
        tracing::warn!("could not open {url}: {e}");
    }
}

/// The platform's standard menu, plus File › Open Target… and the Plonix
/// screens in View.
fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::default(app)?;
    let open = MenuItem::with_id(app, "open-target", "Open Target…", true, Some("CmdOrCtrl+O"))?;
    let file = find_or_add_submenu(app, &menu, "File", 1)?;
    file.prepend_items(&[&open, &PredefinedMenuItem::separator(app)?])?;

    let view = find_or_add_submenu(app, &menu, "View", 3)?;
    let mut items: Vec<MenuItem<Wry>> = Vec::new();
    for (id, label, accel, _) in VIEW_ITEMS {
        items.push(MenuItem::with_id(app, id, label, true, Some(accel))?);
    }
    let toggle = MenuItem::with_id(app, "toggle-sidebar", "Toggle Sidebar", true, Some(TOGGLE_SIDEBAR_KEY))?;
    let sep = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let mut refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = items.iter().map(|i| i as &dyn tauri::menu::IsMenuItem<Wry>).collect();
    refs.push(&sep);
    refs.push(&toggle);
    if !view.items()?.is_empty() {
        refs.push(&sep2);
    }
    view.prepend_items(&refs)?;
    Ok(menu)
}

fn find_or_add_submenu(app: &AppHandle, menu: &Menu<Wry>, title: &str, position: usize) -> tauri::Result<Submenu<Wry>> {
    for item in menu.items()? {
        if let MenuItemKind::Submenu(sub) = item
            && sub.text()? == title
        {
            return Ok(sub);
        }
    }
    let sub = Submenu::new(app, title, true)?;
    let position = position.min(menu.items()?.len());
    menu.insert(&sub, position)?;
    Ok(sub)
}
