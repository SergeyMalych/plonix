//! `plonix open <target>`: from one command to captured traffic.
//!
//! The capture browser itself lives in [`plonix_core::browser`], so the
//! Plonix app can open it too.

use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};
use plonix_core::paths::Home;

pub use plonix_core::browser::{Kind, detect, launch, parse_target};

/// Opens a URL in the user's default browser (not the capture browser, so the
/// Plonix window's own requests are never captured).
pub fn open_url(url: &str) -> Result<()> {
    if let Some(exe) = std::env::var_os("PLONIX_UI_BROWSER").filter(|v| !v.is_empty()) {
        plonix_core::browser::spawn_detached(Command::new(exe).arg(url))?;
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    let mut cmd = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut cmd = Command::new("xdg-open");
    let status = cmd.arg(url).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status()?;
    if !status.success() {
        bail!("{status}");
    }
    Ok(())
}

/// One-time guidance for trusting the CA outside the Plonix browser.
pub fn trust_guidance(home: &Home) -> String {
    format!(
        "To capture HTTPS from other apps too (Safari, curl, your everyday browser),
trust the Plonix certificate once:

    plonix ca trust

This adds {ca} to your login keychain as a trusted root
(macOS asks for your password or Touch ID). The certificate was generated on
this machine and its private key never leaves {home}.
Undo it any time in Keychain Access by deleting \"Plonix CA\".
",
        ca = tilde(&home.ca_cert()),
        home = tilde(&home.root)
    )
}

/// Shortens paths under the home directory to `~/...` for display.
pub fn tilde(path: &Path) -> String {
    match plonix_core::paths::user_home() {
        Some(h) if !h.as_os_str().is_empty() && path.starts_with(&h) => {
            format!("~/{}", path.strip_prefix(&h).unwrap_or(path).display())
        }
        _ => path.display().to_string(),
    }
}

