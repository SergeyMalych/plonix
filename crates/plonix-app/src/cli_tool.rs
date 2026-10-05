//! Plonix › Install Command Line Tool…: puts the `plonix` command on PATH.
//!
//! Plonix.app carries the command line tool inside it, next to the app's
//! own binary (Contents/MacOS/plonix-cli, bundled as a sidecar). Installing
//! links /usr/local/bin/plonix to it, so the command always matches the app,
//! updates included. macOS asks for an administrator password only when
//! /usr/local/bin cannot be written as the user.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

use crate::updates::pressed;

/// Where the command goes. On PATH in every macOS shell by default.
const LINK: &str = "/usr/local/bin/plonix";
/// The sidecar's name next to the app binary (build.rs, tauri.conf.json).
const TOOL: &str = "plonix-cli";

pub const INSTALL_ID: &str = "cli-install";
pub const UNINSTALL_ID: &str = "cli-uninstall";

/// Handles the two menu items. True when `id` was one of them.
pub fn on_menu_event(app: &AppHandle, id: &str) -> bool {
    let run: fn(&AppHandle) = match id {
        INSTALL_ID => install,
        UNINSTALL_ID => uninstall,
        _ => return false,
    };
    // The dialogs block, so they stay off the main thread.
    let app = app.clone();
    if let Err(e) = std::thread::Builder::new().name("plonix-cli-tool".into()).spawn(move || run(&app)) {
        tracing::warn!("could not start the command line tool task: {e}");
    }
    true
}

/// The tool inside this copy of the app, if it has a real one (development
/// builds carry a stand-in script, see build.rs).
fn bundled() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("Plonix could not find its own location: {e}"))?;
    let tool = exe.with_file_name(TOOL);
    let mut head = [0u8; 2];
    let real = std::fs::File::open(&tool).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head != b"#!";
    if !real {
        return Err("This copy of Plonix was built without the command line tool.\n\n\
                    Install it from the source instead: cargo install --path crates/plonix-cli"
            .into());
    }
    Ok(tool)
}

/// What is at /usr/local/bin/plonix now.
#[derive(Debug, PartialEq)]
enum Existing {
    Nothing,
    /// A link into a Plonix app (this one or another copy).
    Ours(PathBuf),
    /// Anything else: a file, or a link somewhere else.
    Other,
}

fn existing(link: &Path) -> Existing {
    let Ok(meta) = std::fs::symlink_metadata(link) else { return Existing::Nothing };
    if !meta.file_type().is_symlink() {
        return Existing::Other;
    }
    match std::fs::read_link(link) {
        Ok(to) if is_app_tool(&to) => Existing::Ours(to),
        _ => Existing::Other,
    }
}

fn is_app_tool(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == TOOL) && path.to_string_lossy().contains(".app/Contents/MacOS/")
}

/// Running from the disk image or from a randomized "translocated" copy: a
/// link to it would break once the image is ejected or the app is moved.
fn temporary_location(path: &Path) -> bool {
    let p = path.to_string_lossy();
    p.starts_with("/Volumes/") || p.contains("/AppTranslocation/")
}

fn install(app: &AppHandle) {
    let tool = match bundled() {
        Ok(t) => t,
        Err(msg) => return warn(app, "No command line tool in this build", &msg),
    };
    if temporary_location(&tool) {
        return warn(
            app,
            "Move Plonix to Applications first",
            "Plonix is running from the disk image or a temporary location, so the plonix command would stop working \
             once it is gone.\n\nDrag Plonix to your Applications folder, open it from there, and choose \
             Install Command Line Tool… again.",
        );
    }
    match existing(Path::new(LINK)) {
        Existing::Ours(to) if to == tool => {
            return info(app, "The plonix command is installed", &format!("{LINK} already runs the tool inside this copy of Plonix."));
        }
        Existing::Other => {
            let replace = app
                .dialog()
                .message(format!("{LINK} already exists and was not installed by Plonix. Replace it with the plonix command from this app?"))
                .title("Replace the existing plonix command?")
                .kind(MessageDialogKind::Warning)
                .buttons(MessageDialogButtons::OkCancelCustom("Replace".into(), "Cancel".into()))
                .blocking_show_with_result();
            if !pressed(&replace, "Replace", true) {
                return;
            }
        }
        _ => {}
    }
    match link(&tool) {
        Ok(()) => info(
            app,
            "Installed the plonix command",
            &format!(
                "{LINK} now runs the command line tool inside Plonix, and is updated with the app.\n\n\
                 Open a new Terminal window and try: plonix --help"
            ),
        ),
        Err(Failure::Cancelled) => {}
        Err(Failure::Error(e)) => warn(app, "Could not install the plonix command", &format!("{e}\n\nNothing was changed.")),
    }
}

fn uninstall(app: &AppHandle) {
    match existing(Path::new(LINK)) {
        Existing::Nothing => info(app, "The plonix command is not installed", &format!("There is nothing at {LINK}.")),
        Existing::Other => warn(app, "Left the plonix command alone", &format!("{LINK} was not installed by Plonix, so Plonix does not remove it.")),
        Existing::Ours(_) => match unlink() {
            Ok(()) => info(app, "Removed the plonix command", &format!("{LINK} is gone. Plonix itself is not affected.")),
            Err(Failure::Cancelled) => {}
            Err(Failure::Error(e)) => warn(app, "Could not remove the plonix command", &e),
        },
    }
}

enum Failure {
    /// The user closed the password prompt.
    Cancelled,
    Error(String),
}

/// Links /usr/local/bin/plonix to `tool`, as the user if the folder is
/// writable, else with the administrator prompt.
fn link(tool: &Path) -> Result<(), Failure> {
    let direct = (|| {
        if std::fs::symlink_metadata(LINK).is_ok() {
            std::fs::remove_file(LINK)?;
        }
        std::os::unix::fs::symlink(tool, LINK)
    })();
    match direct {
        Ok(()) => Ok(()),
        Err(_) => as_admin(&format!("mkdir -p /usr/local/bin && ln -sfn {} {LINK}", sh_quote(&tool.to_string_lossy())), "install"),
    }
}

fn unlink() -> Result<(), Failure> {
    match std::fs::remove_file(LINK) {
        Ok(()) => Ok(()),
        Err(_) => as_admin(&format!("rm -f {LINK}"), "remove"),
    }
}

/// Runs a shell command with administrator rights, behind the standard
/// macOS password prompt. `verb` says what for: install or remove.
fn as_admin(cmd: &str, verb: &str) -> Result<(), Failure> {
    let script = format!(
        "do shell script \"{}\" with prompt \"Plonix wants to {verb} the plonix command in /usr/local/bin.\" with administrator privileges",
        applescript_escape(cmd)
    );
    let out = Command::new("/usr/bin/osascript").arg("-e").arg(&script).output().map_err(|e| Failure::Error(format!("could not ask for permission: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let err = String::from_utf8_lossy(&out.stderr);
    // -128: the user pressed Cancel.
    if err.contains("-128") {
        return Err(Failure::Cancelled);
    }
    Err(Failure::Error(err.trim().to_string()))
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn info(app: &AppHandle, title: &str, message: &str) {
    app.dialog().message(message).title(title).kind(MessageDialogKind::Info).blocking_show();
}

fn warn(app: &AppHandle, title: &str, message: &str) {
    app.dialog().message(message).title(title).kind(MessageDialogKind::Warning).blocking_show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_links_into_an_app_count_as_ours() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("plonix");
        assert_eq!(existing(&link), Existing::Nothing);
        let tool = PathBuf::from("/Applications/Plonix.app/Contents/MacOS/plonix-cli");
        std::os::unix::fs::symlink(&tool, &link).unwrap();
        assert_eq!(existing(&link), Existing::Ours(tool));
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("/opt/homebrew/bin/plonix", &link).unwrap();
        assert_eq!(existing(&link), Existing::Other);
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, b"#!/bin/sh\n").unwrap();
        assert_eq!(existing(&link), Existing::Other);
    }

    #[test]
    fn shell_and_applescript_quoting() {
        let q = sh_quote("/Users/o'neil/Apps/Plonix.app/Contents/MacOS/plonix-cli");
        assert_eq!(q, "'/Users/o'\\''neil/Apps/Plonix.app/Contents/MacOS/plonix-cli'");
        assert_eq!(applescript_escape(r#"ln "a\b""#), r#"ln \"a\\b\""#);
        assert!(temporary_location(Path::new("/Volumes/Plonix/Plonix.app/Contents/MacOS/plonix-cli")));
        assert!(!temporary_location(Path::new("/Applications/Plonix.app/Contents/MacOS/plonix-cli")));
    }
}
