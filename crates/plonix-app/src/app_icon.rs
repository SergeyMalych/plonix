//! The app icon follows the window style (Settings › Appearance › Style).
//!
//! Studio swaps in the red, yellow and blue mark while the app runs: the Dock
//! icon on macOS, each window's taskbar icon elsewhere. Finder, the installer
//! and the Dock after quitting keep the bundled Classic icon, which is fixed
//! when the app is built.

use std::collections::HashMap;
use std::time::Duration;

use plonix_core::paths::Home;
use tauri::AppHandle;

/// The Studio mark, 512 px PNG, for the macOS Dock.
#[cfg(target_os = "macos")]
const STUDIO_PNG: &[u8] = include_bytes!("../icons/studio/icon.png");
/// The same mark as 256×256 RGBA, for window icons.
#[cfg(not(target_os = "macos"))]
const STUDIO_RGBA: &[u8] = include_bytes!("../icons/studio/icon-256.rgba");

/// Watches the saved style and keeps the icon in step, new windows included.
pub fn follow(app: AppHandle, home: Home) {
    // Window label → whether it shows the Studio icon.
    let mut shown: HashMap<String, bool> = HashMap::new();
    let mut dock: Option<bool> = None;
    loop {
        let studio = plonix_core::settings::style(&home) == "studio";
        if dock != Some(studio) {
            set_dock(&app, studio);
            dock = Some(studio);
        }
        set_windows(&app, studio, &mut shown);
        std::thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(target_os = "macos")]
fn set_dock(app: &AppHandle, studio: bool) {
    let _ = app.run_on_main_thread(move || {
        use objc2::{AllocAnyThread, MainThreadMarker};
        use objc2_app_kit::{NSApplication, NSImage};
        use objc2_foundation::NSData;

        let Some(mtm) = MainThreadMarker::new() else { return };
        let ns_app = NSApplication::sharedApplication(mtm);
        // No image puts back the bundle's own icon.
        let image = studio.then(|| NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(STUDIO_PNG))).flatten();
        unsafe { ns_app.setApplicationIconImage(image.as_deref()) };
    });
}

#[cfg(not(target_os = "macos"))]
fn set_dock(_app: &AppHandle, _studio: bool) {}

#[cfg(target_os = "macos")]
fn set_windows(_app: &AppHandle, _studio: bool, _shown: &mut HashMap<String, bool>) {}

#[cfg(not(target_os = "macos"))]
fn set_windows(app: &AppHandle, studio: bool, shown: &mut HashMap<String, bool>) {
    use tauri::Manager;
    use tauri::image::Image;

    let windows = app.webview_windows();
    shown.retain(|label, _| windows.contains_key(label));
    for (label, w) in windows {
        // A new window starts with the bundle icon, which is Classic.
        if shown.get(&label).copied().unwrap_or(false) == studio {
            shown.insert(label, studio);
            continue;
        }
        let icon = if studio { Some(Image::new(STUDIO_RGBA, 256, 256)) } else { app.default_window_icon().cloned() };
        if let Some(icon) = icon
            && w.set_icon(icon).is_ok()
        {
            shown.insert(label, studio);
        }
    }
}
