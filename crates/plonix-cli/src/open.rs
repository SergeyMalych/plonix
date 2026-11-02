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
    trust_guidance_for(System::current(), &tilde(&home.ca_cert()), &tilde(&home.root))
}

/// The systems the trust wording differs on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum System {
    Mac,
    Windows,
    Other,
}

impl System {
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            System::Mac
        } else if cfg!(windows) {
            System::Windows
        } else {
            System::Other
        }
    }

    /// Where `plonix ca trust` puts the certificate, and what the system asks.
    pub fn trust_line(self, ca: &str) -> String {
        match self {
            System::Mac => format!("Adding {ca} to your login keychain as a trusted root. macOS will ask you to confirm."),
            System::Windows => format!("Adding {ca} to your Windows certificates as a trusted root. Windows will ask you to confirm."),
            System::Other => format!("Trusting {ca}."),
        }
    }

    /// Which apps accept HTTPS through Plonix once it is trusted.
    pub fn trusted_apps(self) -> &'static str {
        match self {
            System::Mac => "Safari, Chrome, curl and other apps",
            System::Windows => "Edge, Chrome, curl and other apps",
            System::Other => "Apps that use the system certificates",
        }
    }
}

pub fn trust_guidance_for(system: System, ca: &str, home: &str) -> String {
    let (apps, store, undo) = match system {
        System::Mac => (
            "Safari, curl, your everyday browser",
            "your login keychain as a trusted root\n(macOS asks for your password or Touch ID)",
            "Keychain Access",
        ),
        System::Windows => (
            "Edge, curl, your everyday browser",
            "your Windows certificates as a trusted root\n(Windows asks you to confirm)",
            "Manage user certificates (certmgr.msc), under Trusted Root Certification Authorities,",
        ),
        System::Other => ("curl, your everyday browser", "the trusted certificates of this computer", "your system's certificate settings"),
    };
    format!(
        "To capture HTTPS from other apps too ({apps}),
trust the Plonix certificate once:

    plonix ca trust

This adds {ca} to {store}.
The certificate was generated on this computer and its private key
never leaves {home}.
Undo it any time in {undo} by deleting \"Plonix CA\".
"
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


#[cfg(test)]
mod trust_tests {
    use super::*;

    #[test]
    fn trust_wording_follows_the_system() {
        let win = trust_guidance_for(System::Windows, "~/.plonix/ca.pem", "~/.plonix");
        assert!(win.contains("Windows certificates") && win.contains("certmgr.msc") && win.contains("Edge"), "{win}");
        assert!(!win.contains("keychain") && !win.contains("macOS") && !win.contains("Safari"), "{win}");
        let mac = trust_guidance_for(System::Mac, "~/.plonix/ca.pem", "~/.plonix");
        assert!(mac.contains("login keychain") && mac.contains("Keychain Access"), "{mac}");
        assert!(System::Windows.trust_line("ca.pem").contains("Windows will ask"));
        assert!(!System::Windows.trusted_apps().contains("Safari"));
    }
}
