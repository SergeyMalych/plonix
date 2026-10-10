//! The app icon follows the window style (Settings › Appearance › Style).
//!
//! The bundle carries the Studio mark, the default. Classic swaps in the
//! indigo mark while the app runs: the Dock icon on macOS, each window's
//! taskbar icon elsewhere. Finder, the installer and the Dock after quitting
//! keep the bundled Studio icon, which is fixed when the app is built.

use std::collections::HashMap;
use std::time::Duration;

use plonix_core::paths::Home;
use tauri::AppHandle;

/// The Classic mark, 512 px PNG, for the macOS Dock.
#[cfg(target_os = "macos")]
const CLASSIC_PNG: &[u8] = include_bytes!("../icons/classic/icon.png");
/// The same mark as 256×256 RGBA, for window icons.
#[cfg(not(target_os = "macos"))]
const CLASSIC_RGBA: &[u8] = include_bytes!("../icons/classic/icon-256.rgba");

/// Watches the saved style and keeps the icon in step, new windows included.
pub fn follow(app: AppHandle, home: Home) {
    // Window label → whether it shows the Classic icon.
    let mut shown: HashMap<String, bool> = HashMap::new();
    let mut dock: Option<bool> = None;
    loop {
        let classic = plonix_core::settings::style(&home) == "classic";
        if dock != Some(classic) {
            set_dock(&app, classic);
            dock = Some(classic);
        }
        set_windows(&app, classic, &mut shown);
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(target_os = "macos")]
fn set_dock(app: &AppHandle, classic: bool) {
    let _ = app.run_on_main_thread(move || {
        use objc2::{AllocAnyThread, MainThreadMarker};
        use objc2_app_kit::{NSApplication, NSImage};
        use objc2_foundation::NSData;

        let Some(mtm) = MainThreadMarker::new() else { return };
        let ns_app = NSApplication::sharedApplication(mtm);
        // No image puts back the bundle's own icon.
        let image = classic.then(|| NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(CLASSIC_PNG))).flatten();
        unsafe { ns_app.setApplicationIconImage(image.as_deref()) };
    });
}

#[cfg(not(target_os = "macos"))]
fn set_dock(_app: &AppHandle, _classic: bool) {}

#[cfg(target_os = "macos")]
fn set_windows(_app: &AppHandle, _classic: bool, _shown: &mut HashMap<String, bool>) {}

#[cfg(not(target_os = "macos"))]
fn set_windows(app: &AppHandle, classic: bool, shown: &mut HashMap<String, bool>) {
    use tauri::Manager;
    use tauri::image::Image;

    let windows = app.webview_windows();
    shown.retain(|label, _| windows.contains_key(label));
    for (label, w) in windows {
        // A new window starts with the bundle icon, which is Studio.
        if shown.get(&label).copied().unwrap_or(false) == classic {
            shown.insert(label, classic);
            continue;
        }
        let icon = if classic { Some(Image::new(CLASSIC_RGBA, 256, 256)) } else { app.default_window_icon().cloned() };
        if let Some(icon) = icon
            && w.set_icon(icon).is_ok()
        {
            shown.insert(label, classic);
        }
    }
}
