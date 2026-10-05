//! Plonix as a desktop app.
//!
//! The app opens on the Start screen, which lists projects and creates new
//! ones. Each project opens in a window of its own, served by a session with
//! its own engine (proxy, API and database), so several projects run side by
//! side. Closing a project's window closes its session. All of it runs in the
//! app's own process; projects opened from a terminal (`plonix start -p …`)
//! show up on the Start screen too and open in a window like any other.
//!
//! Windows only ever show Plonix's own pages. Any other link opens in the
//! default browser. With Settings › Interface › "Open projects in: My web
//! browser", projects open in the default browser instead of a window.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(unix)]
mod cli_tool;
mod crashes;
mod updates;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use plonix_core::hub::{self, Hub, HubEvent};
use plonix_core::paths::Home;
use plonix_core::settings::InterfaceSettings;
use tauri::menu::{Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu};
use tauri::webview::PageLoadEvent;
use tauri::{AppHandle, Manager, RunEvent, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent, Wry};
use tauri_plugin_dialog::DialogExt;

const LAUNCHER: &str = "launcher";

/// The Start screen and every project session it opens.
struct Host {
    hub: Arc<Hub>,
    runtime: tokio::runtime::Runtime,
}

static HOST: OnceLock<Host> = OnceLock::new();
/// Set once the starting page has loaded, so errors can be shown on it.
static START_PAGE_READY: AtomicBool = AtomicBool::new(false);
/// Project windows: window label → project id.
static WINDOWS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn windows() -> &'static Mutex<HashMap<String, String>> {
    WINDOWS.get_or_init(Mutex::default)
}

/// Lets the UI adapt its wording when it runs inside the app.
const INIT_SCRIPT: &str = "window.__PLONIX_APP__ = true;";

/// Menu items that drive a project window: (id, label, accelerator, script).
const VIEW_ITEMS: [(&str, &str, &str, &str); 9] = [
    ("go-traffic", "Traffic", "CmdOrCtrl+1", "window.plonix && plonix.go('traffic')"),
    ("go-bench", "Bench", "CmdOrCtrl+2", "window.plonix && plonix.go('bench')"),
    ("go-scope", "Scope", "CmdOrCtrl+3", "window.plonix && plonix.go('scope')"),
    ("go-map", "Map", "CmdOrCtrl+4", "window.plonix && plonix.go('map')"),
    ("go-findings", "Findings", "CmdOrCtrl+5", "window.plonix && plonix.go('findings')"),
    ("go-agents", "Agents", "CmdOrCtrl+6", "window.plonix && plonix.go('agents')"),
    ("go-market", "Market", "CmdOrCtrl+7", "window.plonix && plonix.go('market')"),
    ("go-scans", "Scans", "CmdOrCtrl+8", "window.plonix && plonix.go('scans')"),
    ("go-programs", "Programs", "CmdOrCtrl+9", "window.plonix && plonix.go('programs')"),
];
const OPEN_TARGET_SCRIPT: &str = "window.plonix && plonix.openTarget()";
const TOGGLE_SIDEBAR_SCRIPT: &str = "window.plonix && plonix.toggleSidebar()";
const IMPORT_HAR_SCRIPT: &str = "window.plonix && plonix.importHar && plonix.importHar()";
const EXPORT_HAR_SCRIPT: &str = "window.plonix && plonix.exportHar && plonix.exportHar()";
const PROJECT_SETTINGS_SCRIPT: &str = "window.plonix && plonix.go('settings')";
const LAUNCHER_SETTINGS_SCRIPT: &str = "window.plonixLauncher && plonixLauncher.settings()";
const NEW_PROJECT_SCRIPT: &str = "window.plonixLauncher && plonixLauncher.newProject()";
/// The usual shortcut for showing and hiding a sidebar.
const TOGGLE_SIDEBAR_KEY: &str = if cfg!(target_os = "macos") { "Ctrl+Cmd+S" } else { "Ctrl+Shift+S" };

fn main() {
    // A crash, in the window or in any engine thread, writes a scrubbed
    // report to $PLONIX_HOME/crashes; the next launch asks what to do with it.
    if let Ok(home) = Home::resolve(None) {
        plonix_core::crash::install(&home, "app", crashes::saved);
    }

    // Headless entry points, before any window is created. The in-app "Ask
    // Claude" panel wires Claude Code to this same binary run as `<app> mcp`
    // (current_exe), so it must serve the read-only MCP server over stdio and
    // exit — never boot the GUI, which would open a second Plonix window.
    if std::env::args().nth(1).as_deref() == Some("mcp") {
        let code = match Home::resolve(None).and_then(plonix_core::mcp::serve) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("plonix mcp: {e:#}");
                1
            }
        };
        std::process::exit(code);
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("PLONIX_LOG").unwrap_or_else(|_| "plonix_core=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(updates::Updates::new())
        .menu(build_menu)
        .on_menu_event(|app, event| on_menu(app, event.id().as_ref()))
        .setup(|app| {
            plonix_core::dialogs::install(Box::new(Dialogs(app.handle().clone())));
            let handle = app.handle().clone();
            launcher_window(&handle)?;
            std::thread::Builder::new().name("plonix-start".into()).spawn(move || start(handle))?;
            // A report from last time is asked about first, then updates.
            let handle = app.handle().clone();
            std::thread::Builder::new().name("plonix-crashes".into()).spawn(move || {
                if let Ok(home) = Home::resolve(None) {
                    crashes::ask(&handle, &home);
                }
                updates::start(&handle);
            })?;
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to start Plonix");

    app.run(|_app, event| match event {
        RunEvent::Exit => {
            // Closes every session, which applies "keep only in-scope traffic".
            if let Some(host) = HOST.get() {
                host.runtime.block_on(host.hub.shutdown());
            }
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { has_visible_windows: false, .. } => show_launcher(_app),
        _ => {}
    });
}

/// The Start screen window. It shows a starting page until the hub runs.
fn launcher_window(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    if let Some(w) = app.get_webview_window(LAUNCHER) {
        return Ok(w);
    }
    let handle = app.clone();
    let w = WebviewWindowBuilder::new(app, LAUNCHER, WebviewUrl::App("index.html".into()))
        .title("Plonix")
        .inner_size(880.0, 620.0)
        .min_inner_size(640.0, 460.0)
        .initialization_script(INIT_SCRIPT)
        .on_navigation(move |url| launcher_navigation(&handle, url))
        .on_page_load(|_, payload| {
            if payload.event() == PageLoadEvent::Finished && !is_hub_page(payload.url()) {
                START_PAGE_READY.store(true, Ordering::SeqCst);
            }
        })
        .build()?;
    // Reopened after it was closed: go straight to the Start screen.
    if let Some(Ok(url)) = HOST.get().and_then(|h| h.hub.launch_url().ok()).map(|u| Url::parse(&u)) {
        let _ = w.navigate(url);
    }
    Ok(w)
}

fn show_launcher(app: &AppHandle) {
    match app.get_webview_window(LAUNCHER) {
        Some(w) => {
            let _ = w.show();
            let _ = w.set_focus();
        }
        None => {
            let _ = launcher_window(app);
        }
    }
}

/// The system's Open and Save dialogs, for engine routes that work with
/// files (HAR export and import). They are asked for from engine tasks, never
/// the main thread, so blocking until the user answers is fine.
struct Dialogs(AppHandle);

impl Dialogs {
    fn builder(&self, title: &str, filters: &[plonix_core::dialogs::Filter<'_>]) -> tauri_plugin_dialog::FileDialogBuilder<Wry> {
        let mut b = self.0.dialog().file().set_title(title);
        for (name, exts) in filters {
            b = b.add_filter(*name, exts);
        }
        // Over the project window the user is working in.
        if let Some(w) = self.0.webview_windows().into_values().find(|w| w.is_focused().unwrap_or(false)) {
            b = b.set_parent(&w);
        }
        b
    }
}

impl plonix_core::dialogs::FileDialogs for Dialogs {
    fn save(&self, title: &str, file_name: &str, filters: &[plonix_core::dialogs::Filter<'_>]) -> Option<std::path::PathBuf> {
        self.builder(title, filters).set_file_name(file_name).blocking_save_file()?.into_path().ok()
    }

    fn open(&self, title: &str, filters: &[plonix_core::dialogs::Filter<'_>]) -> Option<std::path::PathBuf> {
        self.builder(title, filters).blocking_pick_file()?.into_path().ok()
    }
}

/// Starts the Start screen server in this process and shows it.
fn start(app: AppHandle) {
    let started = (|| -> Result<String> {
        let home = Home::resolve(None)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .thread_name("plonix-engine")
            .build()
            .context("starting the engine runtime")?;
        let hub = runtime.block_on(hub::start(&home, None)).context("starting the Start screen")?;
        plonix_core::usage::record("app_launched");
        hub.announce()?;
        let url = hub.launch_url()?;
        let events = hub.events.subscribe();
        let _ = HOST.set(Host { hub, runtime });
        let handle = app.clone();
        std::thread::Builder::new().name("plonix-events".into()).spawn(move || follow_events(handle, events))?;
        Ok(url)
    })();
    let Some(win) = app.get_webview_window(LAUNCHER) else { return };
    match started.and_then(|u| Url::parse(&u).context("bad Start screen address")) {
        Ok(url) => {
            let _ = win.navigate(url);
        }
        Err(e) => show_error(&win, &format!("{e:#}")),
    }
}

/// Closes a project's window when its session ends elsewhere (`plonix stop`).
fn follow_events(app: AppHandle, mut events: tokio::sync::broadcast::Receiver<HubEvent>) {
    loop {
        match events.blocking_recv() {
            Ok(HubEvent::Closed { project_id }) => {
                let label = windows().lock().unwrap().iter().find(|(_, id)| **id == project_id).map(|(l, _)| l.clone());
                if let Some(w) = label.and_then(|l| app.get_webview_window(&l)) {
                    let _ = w.destroy();
                }
            }
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(_) => return,
        }
    }
}

fn show_error(win: &WebviewWindow, message: &str) {
    tracing::error!("{message}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !START_PAGE_READY.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let arg = serde_json::to_string(message).unwrap_or_else(|_| "\"\"".into());
    let _ = win.eval(format!("window.plonixError && plonixError({arg})"));
}

fn origin(url: &Url) -> String {
    format!("{}://{}:{}", url.scheme(), url.host_str().unwrap_or(""), url.port_or_known_default().unwrap_or(0))
}

fn is_loopback(url: &Url) -> bool {
    url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

fn is_hub_page(url: &Url) -> bool {
    HOST.get().is_some_and(|h| Url::parse(&h.hub.url()).is_ok_and(|hub| origin(&hub) == origin(url)))
}

fn is_start_page(url: &Url) -> bool {
    url.scheme() == "tauri" || url.host_str() == Some("tauri.localhost")
}

/// The Start screen window shows the starting page and the Start screen.
/// When it navigates to a project (a one-time sign-in link to a session's
/// engine), the project opens in its own window, or in the browser.
fn launcher_navigation(app: &AppHandle, url: &Url) -> bool {
    if is_start_page(url) || is_hub_page(url) {
        return true;
    }
    if is_loopback(url) && url.fragment().is_some_and(|f| f.starts_with("code=")) {
        let (app, link) = (app.clone(), url.to_string());
        std::thread::spawn(move || open_project_url(&app, &link));
        return false;
    }
    if matches!(url.scheme(), "http" | "https") {
        open_externally(url.as_str());
    }
    false
}

/// Opens a project's one-time sign-in link: in its window (focusing it if it
/// is already open), or in the default browser.
fn open_project_url(app: &AppHandle, link: &str) {
    let Some(host) = HOST.get() else { return };
    let Ok(url) = Url::parse(link) else { return };
    if InterfaceSettings::load(&host.hub.home).open_in_browser {
        open_externally(link);
        return;
    }
    let label = format!("project-{}", url.port().unwrap_or(0));
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.navigate(url);
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let api = origin(&url);
    let info = host
        .runtime
        .block_on(host.hub.project_for_api(&api))
        .or_else(|| plonix_core::session::running(&host.hub.home).into_iter().find(|i| i.api == api));
    let (title, project_id) = match &info {
        Some(i) => (format!("{} — Plonix", i.project), i.project_id.clone()),
        None => ("Plonix".to_string(), String::new()),
    };
    let own_origin = api.clone();
    let built = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(url))
        .title(&title)
        .inner_size(1320.0, 840.0)
        .min_inner_size(900.0, 560.0)
        .initialization_script(INIT_SCRIPT)
        .on_navigation(move |u| {
            if is_loopback(u) && origin(u) == own_origin {
                return true;
            }
            if matches!(u.scheme(), "http" | "https") {
                open_externally(u.as_str());
            }
            false
        })
        .build();
    match built {
        Ok(w) => {
            windows().lock().unwrap().insert(label.clone(), project_id.clone());
            w.on_window_event(move |event| {
                if let WindowEvent::Destroyed = event {
                    close_project(&label);
                }
            });
        }
        Err(e) => tracing::error!("could not open the project window: {e}"),
    }
}

/// Closing a project's window closes its session, if this app serves it.
fn close_project(label: &str) {
    let Some(project_id) = windows().lock().unwrap().remove(label) else { return };
    let Some(host) = HOST.get() else { return };
    let hub = host.hub.clone();
    host.runtime.spawn(async move {
        if hub.hosted().await.iter().any(|i| i.project_id == project_id)
            && let Err(e) = hub.close(&project_id).await
        {
            tracing::error!("closing project {project_id}: {e:#}");
        }
    });
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

fn focused(app: &AppHandle) -> Option<WebviewWindow> {
    app.webview_windows().into_values().find(|w| w.is_focused().unwrap_or(false))
}

fn on_menu(app: &AppHandle, id: &str) {
    if updates::on_menu_event(app, id) {
        return;
    }
    #[cfg(unix)]
    if cli_tool::on_menu_event(app, id) {
        return;
    }
    let win = focused(app);
    let in_project = win.as_ref().is_some_and(|w| w.label() != LAUNCHER);
    match id {
        "new-project" => {
            show_launcher(app);
            if let Some(w) = app.get_webview_window(LAUNCHER) {
                let _ = w.eval(NEW_PROJECT_SCRIPT);
            }
        }
        "show-projects" => show_launcher(app),
        "settings" => match win {
            Some(w) if in_project => {
                let _ = w.eval(PROJECT_SETTINGS_SCRIPT);
            }
            _ => {
                show_launcher(app);
                if let Some(w) = app.get_webview_window(LAUNCHER) {
                    let _ = w.eval(LAUNCHER_SETTINGS_SCRIPT);
                }
            }
        },
        "open-in-browser" => {
            if let Some(w) = win.filter(|_| in_project) {
                open_in_browser(&w);
            }
        }
        _ => {
            let script = match id {
                "open-target" => Some(OPEN_TARGET_SCRIPT),
                "toggle-sidebar" => Some(TOGGLE_SIDEBAR_SCRIPT),
                "import-har" => Some(IMPORT_HAR_SCRIPT),
                "export-har" => Some(EXPORT_HAR_SCRIPT),
                _ => VIEW_ITEMS.iter().find(|(item, ..)| *item == id).map(|(.., script)| *script),
            };
            if let (Some(script), Some(w)) = (script, win.filter(|_| in_project)) {
                let _ = w.eval(script);
            }
        }
    }
}

/// Opens the project in the focused window in the default browser as well.
fn open_in_browser(w: &WebviewWindow) {
    let Some(host) = HOST.get() else { return };
    let Ok(url) = w.url() else { return };
    let api = origin(&url);
    let token = match host.hub.home.load_or_create_token() {
        Ok(t) => t,
        Err(_) => return,
    };
    std::thread::spawn(move || {
        let resp = ureq::post(&format!("{api}/api/ui/launch"))
            .set("Authorization", &format!("Bearer {token}"))
            .set("X-Plonix-Client", "app")
            .timeout(Duration::from_secs(5))
            .send_json(serde_json::json!({}));
        if let Ok(v) = resp.and_then(|r| r.into_json::<serde_json::Value>().map_err(Into::into))
            && let Some(link) = v["url"].as_str()
        {
            open_externally(link);
        }
    });
}

/// The platform's standard menu, plus File › New Project…, Projects and
/// Open Target…, Import HAR… and Export Traffic as HAR…, Settings…, Install
/// Command Line Tool… (macOS), the update items, and the Plonix screens in View.
fn build_menu(app: &AppHandle) -> tauri::Result<Menu<Wry>> {
    let menu = Menu::default(app)?;
    let new_project = MenuItem::with_id(app, "new-project", "New Project…", true, Some("CmdOrCtrl+N"))?;
    let projects = MenuItem::with_id(app, "show-projects", "Projects…", true, Some("CmdOrCtrl+Shift+P"))?;
    let open = MenuItem::with_id(app, "open-target", "Open Target…", true, Some("CmdOrCtrl+O"))?;
    let import_har = MenuItem::with_id(app, "import-har", "Import HAR…", true, None::<&str>)?;
    let export_har = MenuItem::with_id(app, "export-har", "Export Traffic as HAR…", true, None::<&str>)?;
    let file = find_or_add_submenu(app, &menu, "File", 1)?;
    file.prepend_items(&[
        &new_project,
        &projects,
        &PredefinedMenuItem::separator(app)?,
        &open,
        &PredefinedMenuItem::separator(app)?,
        &import_har,
        &export_har,
        &PredefinedMenuItem::separator(app)?,
    ])?;

    let settings = MenuItem::with_id(app, "settings", "Settings…", true, Some("CmdOrCtrl+,"))?;
    if cfg!(target_os = "macos")
        && let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next()
    {
        #[cfg(unix)]
        {
            let install = MenuItem::with_id(app, cli_tool::INSTALL_ID, "Install Command Line Tool…", true, None::<&str>)?;
            let uninstall = MenuItem::with_id(app, cli_tool::UNINSTALL_ID, "Uninstall Command Line Tool…", true, None::<&str>)?;
            app_menu.insert_items(&[&PredefinedMenuItem::separator(app)?, &install, &uninstall], 1)?;
        }
        app_menu.insert_items(&[&PredefinedMenuItem::separator(app)?, &settings], 1)?;
    } else {
        file.append_items(&[&PredefinedMenuItem::separator(app)?, &settings])?;
    }
    add_update_items(app, &menu)?;

    let view = find_or_add_submenu(app, &menu, "View", 3)?;
    let mut items: Vec<MenuItem<Wry>> = Vec::new();
    for (id, label, accel, _) in VIEW_ITEMS {
        items.push(MenuItem::with_id(app, id, label, true, Some(accel))?);
    }
    let toggle = MenuItem::with_id(app, "toggle-sidebar", "Toggle Sidebar", true, Some(TOGGLE_SIDEBAR_KEY))?;
    let in_browser = MenuItem::with_id(app, "open-in-browser", "Open in Browser", true, Some("CmdOrCtrl+Shift+B"))?;
    let sep = PredefinedMenuItem::separator(app)?;
    let sep2 = PredefinedMenuItem::separator(app)?;
    let mut refs: Vec<&dyn tauri::menu::IsMenuItem<Wry>> = items.iter().map(|i| i as &dyn tauri::menu::IsMenuItem<Wry>).collect();
    refs.push(&sep);
    refs.push(&toggle);
    refs.push(&in_browser);
    if !view.items()?.is_empty() {
        refs.push(&sep2);
    }
    view.prepend_items(&refs)?;
    Ok(menu)
}

/// Check for Updates… and its automatic-check choices go right under About
/// in the app menu on macOS, and in Help elsewhere.
fn add_update_items(app: &AppHandle, menu: &Menu<Wry>) -> tauri::Result<()> {
    let (check_now, auto) = updates::menu_items(app)?;
    let sep = PredefinedMenuItem::separator(app)?;
    if cfg!(target_os = "macos")
        && let Some(MenuItemKind::Submenu(app_menu)) = menu.items()?.into_iter().next()
    {
        // The app menu starts with About Plonix.
        let at = 1.min(app_menu.items()?.len());
        app_menu.insert_items(&[&sep, &check_now, &auto], at)?;
        return Ok(());
    }
    let help = find_or_add_submenu(app, menu, "Help", usize::MAX)?;
    if !help.items()?.is_empty() {
        help.append(&sep)?;
    }
    help.append_items(&[&check_now, &auto])?;
    Ok(())
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
