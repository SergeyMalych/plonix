//! The system's own Open and Save dialogs, when Plonix runs as an app.
//!
//! The engine has no windows of its own. The desktop app installs its
//! dialogs here at start-up, and routes that work with files (HAR export
//! and import) use them to ask the user where to save or what to open. In a
//! web browser there are none: the page downloads and uploads instead.

use std::path::PathBuf;
use std::sync::OnceLock;

/// A file type a dialog offers: a name and its extensions (without dots).
pub type Filter<'a> = (&'a str, &'a [&'a str]);

pub trait FileDialogs: Send + Sync {
    /// Asks where to save a file. `None` when the user cancels.
    fn save(&self, title: &str, file_name: &str, filters: &[Filter<'_>]) -> Option<PathBuf>;
    /// Asks for a file to open. `None` when the user cancels.
    fn open(&self, title: &str, filters: &[Filter<'_>]) -> Option<PathBuf>;
}

static DIALOGS: OnceLock<Box<dyn FileDialogs>> = OnceLock::new();

/// Installs the app's dialogs. Only the first call counts.
pub fn install(dialogs: Box<dyn FileDialogs>) {
    let _ = DIALOGS.set(dialogs);
}

/// The installed dialogs, if this process has any.
pub fn get() -> Option<&'static dyn FileDialogs> {
    DIALOGS.get().map(|d| d.as_ref())
}
