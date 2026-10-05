//! The license and terms of use, accepted once per terms version.
//!
//! The Start screen and the CLI ask before anything else. The answer is kept
//! in `$PLONIX_HOME/terms.json`; a new [`VERSION`] asks again. Scripts and CI
//! pass `--accept-terms` or set `PLONIX_ACCEPT_TERMS=1` instead: that lets
//! the command run without recording an answer, so it never turns on usage
//! statistics either.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::paths::{Home, write_atomic};

/// Bump when TERMS.md changes in a way people should see again.
pub const VERSION: u32 = 1;

/// The terms of use, as shown on the first-launch screen.
pub const TERMS: &str = include_str!("../../../TERMS.md");

/// The Apache License 2.0, Plonix's license.
pub const LICENSE: &str = include_str!("../../../LICENSE");

/// Where the terms and the privacy notes are published.
pub const TERMS_URL: &str = "https://github.com/SergeyMalych/plonix/blob/main/TERMS.md";
pub const PRIVACY_URL: &str = "https://github.com/SergeyMalych/plonix/blob/main/docs/privacy.md";

/// What `terms.json` holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acceptance {
    pub version: u32,
    /// Unix seconds.
    pub accepted_at: i64,
}

fn path(home: &Home) -> std::path::PathBuf {
    home.root.join("terms.json")
}

/// The recorded answer, if any.
pub fn acceptance(home: &Home) -> Option<Acceptance> {
    serde_json::from_slice(&std::fs::read(path(home)).ok()?).ok()
}

/// Whether the current terms were accepted in this Plonix home.
pub fn accepted(home: &Home) -> bool {
    acceptance(home).is_some_and(|a| a.version >= VERSION)
}

/// Records that the current terms were accepted.
pub fn accept(home: &Home) -> Result<()> {
    home.ensure()?;
    let a = Acceptance { version: VERSION, accepted_at: crate::model::now_ms() / 1000 };
    write_atomic(&path(home), &serde_json::to_vec_pretty(&a)?)
}

/// True when `PLONIX_ACCEPT_TERMS` is set (to anything but 0 or empty), for
/// scripts, CI and tests.
pub fn accepted_by_env() -> bool {
    std::env::var("PLONIX_ACCEPT_TERMS").is_ok_and(|v| !matches!(v.trim(), "" | "0" | "false" | "no"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_once_per_version() {
        let home = Home { root: tempfile::tempdir().unwrap().keep() };
        assert!(!accepted(&home));
        accept(&home).unwrap();
        assert!(accepted(&home));
        write_atomic(&path(&home), br#"{"version":0,"accepted_at":1}"#).unwrap();
        assert!(!accepted(&home), "an older version asks again");
        assert!(TERMS.contains(&format!("Version {VERSION}")), "bump VERSION and the version line in TERMS.md together");
        assert!(LICENSE.contains("Apache License"));
    }
}
