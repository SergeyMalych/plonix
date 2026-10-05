//! Trusting the Plonix CA system-wide.
//!
//! Chromium-based capture browsers trust the CA by its key pin and need none
//! of this. Firefox, Safari, curl and other apps check the system trust
//! store: on macOS that is the login keychain, which `plonix ca trust` and
//! the window's "Trust the Plonix certificate" both add the CA to. Firefox
//! follows the keychain because its capture profile sets
//! `security.enterprise_roots.enabled`.

use anyhow::Result;

use crate::paths::Home;

/// Whether this system can trust the CA from Plonix (macOS only).
pub fn supported() -> bool {
    cfg!(target_os = "macos")
}

/// Whether the system already trusts the CA: `None` when Plonix cannot tell.
pub fn is_trusted(home: &Home) -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        let cert = home.ca_cert();
        if !cert.is_file() {
            return Some(false);
        }
        // -l: the certificate is a CA; -L: no network lookups.
        let status = std::process::Command::new("/usr/bin/security")
            .args(["verify-cert", "-q", "-L", "-l", "-c"])
            .arg(&cert)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        Some(status.success())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = home;
        None
    }
}

/// Adds the CA to the login keychain as a trusted root. macOS asks the user
/// to confirm with their password or Touch ID; this waits for the answer.
#[cfg(target_os = "macos")]
pub fn trust(home: &Home) -> Result<()> {
    use anyhow::{Context, bail};
    let keychain = std::env::var_os("HOME")
        .map(|h| std::path::PathBuf::from(h).join("Library/Keychains/login.keychain-db"))
        .context("HOME is not set")?;
    let out = std::process::Command::new("/usr/bin/security")
        .args(["add-trusted-cert", "-r", "trustRoot", "-k"])
        .arg(&keychain)
        .arg(home.ca_cert())
        .stdin(std::process::Stdio::null())
        .output()
        .context("running /usr/bin/security")?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.trim();
        bail!("macOS did not add the certificate{}. Nothing was changed.", if why.is_empty() { String::new() } else { format!(" ({why})") });
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn trust(home: &Home) -> Result<()> {
    anyhow::bail!(
        "trusting the certificate from Plonix works on macOS only. On this system add {} to your trust store by hand \
         (e.g. Debian/Ubuntu: copy it to /usr/local/share/ca-certificates/plonix.crt and run update-ca-certificates).",
        home.ca_cert().display()
    )
}
