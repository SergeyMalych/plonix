//! Updates, on the user's terms.
//!
//! Plonix never installs anything by itself. The flow has three separate
//! choices, and closing any dialog means "no":
//!
//! 1. Whether to look for updates at all. The first launch asks once; after
//!    that the choice lives in Plonix › Check Automatically (when Plonix
//!    starts, daily, weekly, or never). Plonix › Check for Updates… always
//!    works, whatever is picked there.
//! 2. Whether to download a newer version that was found. Downloading only
//!    fetches and verifies the signed package; nothing on disk changes.
//! 3. Whether to install it. Installing replaces the app and restarts it.
//!
//! The choice is kept in `$PLONIX_HOME/updates.json`. The window's pages have
//! no access to any of this: the checks, downloads and installs run here, in
//! the app, behind native dialogs.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use plonix_core::paths::Home;
use serde::{Deserialize, Serialize};
use tauri::menu::{CheckMenuItem, IsMenuItem, MenuItem, Submenu};
use tauri::{AppHandle, Manager, Wry};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult};
use tauri_plugin_updater::{Update, UpdaterExt};

pub const CHECK_NOW_ID: &str = "update-check";
const CADENCE_PREFIX: &str = "update-cadence-";
/// Gives the window a moment to appear before the first-launch question.
const FIRST_QUESTION_DELAY: Duration = Duration::from_secs(2);
/// How often a running app looks at whether an automatic check is due.
const TICK: Duration = Duration::from_secs(60 * 60);
const RELEASES_PAGE: &str = "https://github.com/SergeyMalych/plonix/releases";
/// Longest stretch of release notes shown in a dialog.
const NOTES_LIMIT: usize = 800;

/// When Plonix looks for a newer version by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cadence {
    OnLaunch,
    Daily,
    Weekly,
    /// Only when the user picks Check for Updates….
    Never,
}

impl Cadence {
    const ALL: [Cadence; 4] = [Cadence::OnLaunch, Cadence::Daily, Cadence::Weekly, Cadence::Never];

    fn label(self) -> &'static str {
        match self {
            Cadence::OnLaunch => "When Plonix Starts",
            Cadence::Daily => "Daily",
            Cadence::Weekly => "Weekly",
            Cadence::Never => "Never (Only When I Ask)",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Cadence::OnLaunch => "on-launch",
            Cadence::Daily => "daily",
            Cadence::Weekly => "weekly",
            Cadence::Never => "never",
        }
    }

    fn menu_id(self) -> String {
        format!("{CADENCE_PREFIX}{}", self.key())
    }

    fn from_menu_id(id: &str) -> Option<Cadence> {
        let key = id.strip_prefix(CADENCE_PREFIX)?;
        Cadence::ALL.into_iter().find(|c| c.key() == key)
    }
}

/// What the user chose, as stored in `updates.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    /// `None` until the user has answered the first-launch question.
    #[serde(default)]
    pub check: Option<Cadence>,
    /// Unix seconds of the last automatic check.
    #[serde(default)]
    pub last_check: Option<u64>,
    /// A version the user said to skip; automatic checks stay quiet about it.
    #[serde(default)]
    pub skipped_version: Option<String>,
}

impl Prefs {
    pub fn file(home: &Home) -> PathBuf {
        home.root.join("updates.json")
    }

    /// Reads the stored choice; a missing or unreadable file means nothing
    /// was chosen yet.
    pub fn load(path: &Path) -> Prefs {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }

    /// Whether an automatic check should run now. `at_launch` is true only
    /// for the check right after the app starts.
    pub fn check_due(&self, now: u64, at_launch: bool) -> bool {
        let every = match self.check {
            None | Some(Cadence::Never) => return false,
            Some(Cadence::OnLaunch) => return at_launch,
            Some(Cadence::Daily) => 24 * 60 * 60,
            Some(Cadence::Weekly) => 7 * 24 * 60 * 60,
        };
        self.last_check.is_none_or(|last| now.saturating_sub(last) >= every)
    }
}

/// Update state shared by the menu and the background checker.
pub struct Updates {
    prefs_file: Option<PathBuf>,
    prefs: Mutex<Prefs>,
    /// Set while a check, download or install is in progress.
    busy: AtomicBool,
    menu_items: Mutex<Vec<(Cadence, CheckMenuItem<Wry>)>>,
}

impl Updates {
    pub fn new() -> Updates {
        let prefs_file = Home::resolve(None).ok().map(|h| Prefs::file(&h));
        let prefs = prefs_file.as_deref().map(Prefs::load).unwrap_or_default();
        Updates { prefs_file, prefs: Mutex::new(prefs), busy: AtomicBool::new(false), menu_items: Mutex::new(Vec::new()) }
    }

    fn prefs(&self) -> Prefs {
        self.prefs.lock().unwrap().clone()
    }

    fn change(&self, f: impl FnOnce(&mut Prefs)) {
        let mut prefs = self.prefs.lock().unwrap();
        f(&mut prefs);
        if let Some(path) = &self.prefs_file
            && let Err(e) = prefs.save(path)
        {
            tracing::warn!("could not save the update choice: {e:#}");
        }
    }

    fn set_cadence(&self, cadence: Cadence) {
        self.change(|p| p.check = Some(cadence));
        self.sync_menu();
    }

    /// Ticks the menu item that matches the stored choice.
    fn sync_menu(&self) {
        let current = self.prefs().check.unwrap_or(Cadence::Never);
        for (cadence, item) in self.menu_items.lock().unwrap().iter() {
            let _ = item.set_checked(*cadence == current);
        }
    }
}

/// The update items for the app menu: Check for Updates… and the
/// Check Automatically choices.
pub fn menu_items(app: &AppHandle) -> tauri::Result<(MenuItem<Wry>, Submenu<Wry>)> {
    let updates = app.state::<Updates>();
    let current = updates.prefs().check.unwrap_or(Cadence::Never);
    let check_now = MenuItem::with_id(app, CHECK_NOW_ID, "Check for Updates…", true, None::<&str>)?;
    let mut items = Vec::new();
    for cadence in Cadence::ALL {
        items.push((cadence, CheckMenuItem::with_id(app, cadence.menu_id(), cadence.label(), true, cadence == current, None::<&str>)?));
    }
    let refs: Vec<&dyn IsMenuItem<Wry>> = items.iter().map(|(_, i)| i as &dyn IsMenuItem<Wry>).collect();
    let auto = Submenu::with_items(app, "Check for Updates Automatically", true, &refs)?;
    *updates.menu_items.lock().unwrap() = items;
    Ok((check_now, auto))
}

/// Handles a menu click; returns false when the item is not an update item.
pub fn on_menu_event(app: &AppHandle, id: &str) -> bool {
    if id == CHECK_NOW_ID {
        let app = app.clone();
        spawn(move || run_check(&app, true));
        return true;
    }
    if let Some(cadence) = Cadence::from_menu_id(id) {
        app.state::<Updates>().set_cadence(cadence);
        return true;
    }
    false
}

/// Starts the background side: the first-launch question, then automatic
/// checks while the app runs, as often as the user chose.
pub fn start(app: &AppHandle) {
    let app = app.clone();
    spawn(move || {
        let updates = app.state::<Updates>();
        if updates.prefs().check.is_none() {
            std::thread::sleep(FIRST_QUESTION_DELAY);
            ask_first_time(&app);
        }
        let mut at_launch = true;
        loop {
            if updates.prefs().check_due(now(), at_launch) {
                run_check(&app, false);
            }
            at_launch = false;
            std::thread::sleep(TICK);
        }
    });
}

fn spawn(f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name("plonix-updates".into()).spawn(f) {
        tracing::warn!("could not start the update task: {e}");
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The one-time question. Closing the dialog means "only when I ask".
fn ask_first_time(app: &AppHandle) {
    let result = app
        .dialog()
        .message(
            "Plonix can look for a newer version on its own, once a day.\n\n\
             It only looks. Nothing is downloaded or installed unless you say so, \
             and you can change this any time in the Plonix menu.",
        )
        .title("Check for Plonix updates automatically?")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom("Check Daily".into(), "Only When I Ask".into()))
        .blocking_show_with_result();
    let cadence = if pressed(&result, "Check Daily", true) { Cadence::Daily } else { Cadence::Never };
    app.state::<Updates>().set_cadence(cadence);
}

/// True when the dialog's first button (`label`) was pressed. Platforms report
/// a custom button either by its label or as the standard button it stands in for.
fn pressed(result: &MessageDialogResult, label: &str, first: bool) -> bool {
    match result {
        MessageDialogResult::Custom(text) => text == label,
        MessageDialogResult::Ok | MessageDialogResult::Yes => first,
        MessageDialogResult::No => !first,
        MessageDialogResult::Cancel => false,
    }
}

/// Clears the busy flag however a check ends.
struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Looks for a newer version and, if there is one, offers it. `asked` is true
/// when the user picked Check for Updates…, so the outcome is always shown;
/// automatic checks only speak up when there is something new.
fn run_check(app: &AppHandle, asked: bool) {
    let updates = app.state::<Updates>();
    if updates.busy.swap(true, Ordering::SeqCst) {
        if asked {
            info(app, "Already checking", "Plonix is already checking for updates.");
        }
        return;
    }
    let _guard = BusyGuard(&updates.busy);

    let found = tauri::async_runtime::block_on(async { app.updater()?.check().await });
    if !asked {
        updates.change(|p| p.last_check = Some(now()));
    }
    let current = app.package_info().version.to_string();
    let update = match found {
        Ok(Some(update)) => update,
        Ok(None) | Err(tauri_plugin_updater::Error::ReleaseNotFound) => {
            if asked {
                info(app, "You're up to date", &format!("Plonix {current} is the newest version."));
            }
            return;
        }
        Err(e) => {
            tracing::warn!("update check failed: {e}");
            if asked {
                warn(app, "Couldn't check for updates", &format!("{e}"));
            }
            return;
        }
    };
    if !asked && updates.prefs().skipped_version.as_deref() == Some(update.version.as_str()) {
        tracing::info!("Plonix {} is available; skipped by the user", update.version);
        return;
    }
    offer(app, &update, &current);
}

/// Asks whether to download, then whether to install. Each step needs a click.
fn offer(app: &AppHandle, update: &Update, current: &str) {
    let version = &update.version;
    let mut message = format!("Plonix {version} is available. You have {current}.");
    if let Some(notes) = update.body.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
        message.push_str("\n\n");
        message.push_str(&shorten(notes, NOTES_LIMIT));
    }
    message.push_str("\n\nDownloading doesn't change anything yet. You'll be asked again before it's installed.");
    let result = app
        .dialog()
        .message(message)
        .title("A new version of Plonix is available")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::YesNoCancelCustom("Download…".into(), "Skip This Version".into(), "Not Now".into()))
        .blocking_show_with_result();
    if pressed(&result, "Skip This Version", false) {
        app.state::<Updates>().change(|p| p.skipped_version = Some(version.clone()));
        return;
    }
    if !pressed(&result, "Download…", true) {
        return;
    }

    let bytes = match download(app, update) {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!("update download failed: {e:#}");
            warn(app, "The update didn't download", &format!("{e:#}\n\nNothing was changed."));
            return;
        }
    };

    let install = app
        .dialog()
        .message(format!(
            "Plonix {version} is downloaded and its signature checks out.\n\n\
             Installing replaces this copy of Plonix and restarts it. Captured traffic \
             and projects are kept."
        ))
        .title(format!("Install Plonix {version}?"))
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::OkCancelCustom("Install and Restart".into(), "Not Now".into()))
        .blocking_show_with_result();
    if !pressed(&install, "Install and Restart", true) {
        return;
    }
    match update.install(&bytes) {
        Ok(()) => app.request_restart(),
        Err(e) => {
            tracing::warn!("update install failed: {e}");
            warn(app, "The update wasn't installed", &format!("{e}\n\nPlonix {current} is still in place."));
        }
    }
}

/// Downloads and verifies the package, showing progress in the window title.
fn download(app: &AppHandle, update: &Update) -> Result<Vec<u8>> {
    if !can_verify(app) {
        anyhow::bail!(
            "This copy of Plonix has no update signing key, so it can't verify and install updates itself. \
             Download Plonix {} from {RELEASES_PAGE} instead.",
            update.version
        );
    }
    let win = app.get_webview_window("main");
    let set_title = |title: &str| {
        if let Some(win) = &win {
            let _ = win.set_title(title);
        }
    };
    let mut received = 0usize;
    let mut shown = None;
    let result = tauri::async_runtime::block_on(update.download(
        |chunk, total| {
            received += chunk;
            if let Some(total) = total.filter(|t| *t > 0) {
                let percent = (received as u64 * 100 / total).min(100);
                if shown != Some(percent) {
                    shown = Some(percent);
                    set_title(&format!("Plonix — downloading update {percent}%"));
                }
            }
        },
        || {},
    ));
    set_title("Plonix");
    result.context("downloading the update")
}

/// Whether this build carries the public key that update packages are checked
/// against. Builds made outside the release workflow may not.
fn can_verify(app: &AppHandle) -> bool {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u.get("pubkey"))
        .and_then(|k| k.as_str())
        .is_some_and(|k| !k.trim().is_empty())
}

fn info(app: &AppHandle, title: &str, message: &str) {
    app.dialog().message(message).title(title).kind(MessageDialogKind::Info).blocking_show();
}

fn warn(app: &AppHandle, title: &str, message: &str) {
    app.dialog().message(message).title(title).kind(MessageDialogKind::Warning).blocking_show();
}

/// Cuts text to about `limit` characters, at a line or word break.
fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let cut: String = text.chars().take(limit).collect();
    let end = cut.rfind('\n').or_else(|| cut.rfind(' ')).filter(|&i| i > limit / 2).unwrap_or(cut.len());
    format!("{}…", cut[..end].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60;

    #[test]
    fn nothing_is_checked_until_the_user_chooses() {
        let prefs = Prefs::default();
        assert!(!prefs.check_due(1_000_000, true));
        assert!(!prefs.check_due(1_000_000, false));
    }

    #[test]
    fn never_means_only_when_asked() {
        let prefs = Prefs { check: Some(Cadence::Never), ..Default::default() };
        assert!(!prefs.check_due(1_000_000, true));
    }

    #[test]
    fn on_launch_checks_only_at_launch() {
        let prefs = Prefs { check: Some(Cadence::OnLaunch), last_check: Some(0), ..Default::default() };
        assert!(prefs.check_due(10, true));
        assert!(!prefs.check_due(100 * DAY, false));
    }

    #[test]
    fn daily_and_weekly_wait_their_interval() {
        let daily = Prefs { check: Some(Cadence::Daily), last_check: Some(10 * DAY), ..Default::default() };
        assert!(!daily.check_due(10 * DAY + DAY - 1, true));
        assert!(daily.check_due(11 * DAY, false));

        let weekly = Prefs { check: Some(Cadence::Weekly), last_check: Some(10 * DAY), ..Default::default() };
        assert!(!weekly.check_due(16 * DAY, true));
        assert!(weekly.check_due(17 * DAY, false));

        let first = Prefs { check: Some(Cadence::Weekly), ..Default::default() };
        assert!(first.check_due(5, false));
    }

    #[test]
    fn choice_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        assert_eq!(Prefs::load(&path), Prefs::default());

        let prefs = Prefs { check: Some(Cadence::Weekly), last_check: Some(42), skipped_version: Some("0.2.0".into()) };
        prefs.save(&path).unwrap();
        assert_eq!(Prefs::load(&path), prefs);
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"weekly\""), "{raw}");
    }

    #[test]
    fn unreadable_file_means_not_chosen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("updates.json");
        std::fs::write(&path, "not json").unwrap();
        assert_eq!(Prefs::load(&path).check, None);
    }

    #[test]
    fn menu_ids_round_trip() {
        for cadence in Cadence::ALL {
            assert_eq!(Cadence::from_menu_id(&cadence.menu_id()), Some(cadence));
        }
        assert_eq!(Cadence::from_menu_id("update-cadence-hourly"), None);
        assert_eq!(Cadence::from_menu_id("go-traffic"), None);
    }

    #[test]
    fn closing_a_dialog_is_never_a_yes() {
        assert!(!pressed(&MessageDialogResult::Cancel, "Download…", true));
        assert!(!pressed(&MessageDialogResult::Cancel, "Skip This Version", false));
        assert!(pressed(&MessageDialogResult::Custom("Download…".into()), "Download…", true));
        assert!(!pressed(&MessageDialogResult::Custom("Not Now".into()), "Download…", true));
        assert!(pressed(&MessageDialogResult::Yes, "Download…", true));
        assert!(pressed(&MessageDialogResult::No, "Skip This Version", false));
    }

    #[test]
    fn long_notes_are_shortened() {
        assert_eq!(shorten("short", 10), "short");
        let long = "word ".repeat(400);
        let cut = shorten(&long, 100);
        assert!(cut.chars().count() <= 101);
        assert!(cut.ends_with('…'));
    }
}
