//! Text size: Settings › Appearance › Text size, and View › Bigger Text (⌘=),
//! Smaller Text (⌘−) and Actual Text Size (⌘0).
//!
//! The size is saved with the look, so every window follows it, the Start
//! screen and new windows included. Each window is zoomed natively, which
//! scales text and layout together and keeps the pointer, pop-ups and drags
//! in step with what is drawn. A window's smallest size grows with the text,
//! so every screen keeps the room it is laid out for.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use plonix_core::paths::Home;
use plonix_core::settings;
use serde_json::json;
use tauri::{AppHandle, LogicalSize, Manager, WebviewWindow};

pub const BIGGER_ID: &str = "text-bigger";
pub const SMALLER_ID: &str = "text-smaller";
pub const ACTUAL_ID: &str = "text-actual";

static HOME: OnceLock<Home> = OnceLock::new();
/// Window label → the text size it is zoomed to.
static SHOWN: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
/// The smallest window sizes, in points at 100%.
pub const PROJECT_MIN: (f64, f64) = (900.0, 560.0);
pub const LENS_MIN: (f64, f64) = (480.0, 360.0);
pub const LAUNCHER_MIN: (f64, f64) = (640.0, 460.0);

fn shown() -> &'static Mutex<HashMap<String, u32>> {
    SHOWN.get_or_init(Mutex::default)
}

/// Watches the saved size, which any window or a terminal may change, and
/// zooms every window to it.
pub fn follow(app: AppHandle, home: Home) {
    let _ = HOME.set(home.clone());
    // Window label → the text size its smallest size was set for.
    let mut sized: HashMap<String, u32> = HashMap::new();
    loop {
        let size = settings::text(&home);
        apply(&app, size, None);
        let windows = app.webview_windows();
        sized.retain(|label, _| windows.contains_key(label));
        for (label, w) in windows {
            if sized.get(&label) != Some(&size) {
                fit_size(&w, size);
                sized.insert(label, size);
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Grows the window's smallest size by the text size, up to the screen, and
/// the window with it when it is smaller. Runs off the main thread: it asks
/// the window and the screen for their sizes.
fn fit_size(w: &WebviewWindow, size: u32) {
    let label = w.label();
    let (base_w, base_h) = if label == crate::LAUNCHER {
        LAUNCHER_MIN
    } else if label.starts_with("lens-") {
        LENS_MIN
    } else {
        PROJECT_MIN
    };
    let zoom = f64::from(size).max(100.0) / 100.0;
    let (mut min_w, mut min_h) = (base_w * zoom, base_h * zoom);
    if let Ok(Some(m)) = w.current_monitor() {
        let area = m.work_area().size.to_logical::<f64>(m.scale_factor());
        // Leave room for the title bar.
        min_w = min_w.min(area.width).max(base_w);
        min_h = min_h.min(area.height - 40.0).max(base_h);
    }
    let _ = w.set_min_size(Some(LogicalSize::new(min_w, min_h)));
    if let (Ok(inner), Ok(scale)) = (w.inner_size(), w.scale_factor()) {
        let now = inner.to_logical::<f64>(scale);
        if now.width < min_w || now.height < min_h {
            let _ = w.set_size(LogicalSize::new(now.width.max(min_w), now.height.max(min_h)));
        }
    }
}

/// Zooms a window to the saved size once its page has loaded: a new window,
/// or one that moved to another page.
pub fn fit(w: &WebviewWindow) {
    let Some(home) = HOME.get() else { return };
    let size = settings::text(home);
    if w.set_zoom(f64::from(size) / 100.0).is_ok() {
        shown().lock().unwrap().insert(w.label().to_string(), size);
    }
}

/// Handles the three View items. True when `id` was one of them.
pub fn on_menu_event(app: &AppHandle, id: &str, focused: Option<&WebviewWindow>) -> bool {
    let step = match id {
        BIGGER_ID => 1,
        SMALLER_ID => -1,
        ACTUAL_ID => 0,
        _ => return false,
    };
    let Some(home) = HOME.get() else { return true };
    let size = settings::step_text(settings::text(home), step);
    if let Err(e) = settings::set_look(home, &json!({ "text": size })) {
        tracing::warn!("could not save the text size: {e:#}");
        return true;
    }
    apply(app, size, focused.map(|w| (w.label(), step)));
    true
}

/// Zooms every window not yet at `size`, and tells each page so its Settings
/// show it. `told` is the window whose keys asked for `step`: it says what
/// happened, also when the size was already at its end.
fn apply(app: &AppHandle, size: u32, told: Option<(&str, i32)>) {
    let windows = app.webview_windows();
    let mut shown = shown().lock().unwrap();
    shown.retain(|label, _| windows.contains_key(label));
    for (label, w) in windows {
        let step = told.filter(|(l, _)| *l == label).map(|(_, s)| s);
        if shown.get(&label) == Some(&size) && step.is_none() {
            continue;
        }
        if w.set_zoom(f64::from(size) / 100.0).is_ok() {
            shown.insert(label, size);
        }
        let script = match step {
            Some(step) => format!("window.plonixTextSize && plonixTextSize({size}, {step})"),
            None => format!("window.plonixTextSize && plonixTextSize({size})"),
        };
        let _ = w.eval(script);
    }
}
