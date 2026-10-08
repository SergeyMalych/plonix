//! The block list: packages the Plonix maintainers have pulled.
//!
//! When something on the Market turns out to be harmful (an extension that
//! misbehaves, a hijacked repository, a skill that steers agents somewhere
//! it should not), the maintainers add it to `store/blocked.json`, signed
//! with the same key as the Plonix Market. Plonix reads it whenever it opens
//! the Market and keeps the last verified copy, so it also applies offline
//! and when Plonix starts.
//!
//! A blocked package cannot be installed or added, from any Market, a file,
//! a folder or a repository. One that is already installed is switched off
//! (an extension) or removed (anything else), with the reason saved.
//!
//! ```json
//! { "plonix_blocked": 1,
//!   "entries": [ { "name": "bad-ext", "kind": "extension", "reason": "Sends tokens to its author." },
//!                { "sha256": "…64 hex…", "reason": "A tampered copy of a real package." } ] }
//! ```
//!
//! An entry names a package, a file (`sha256`) or both. With both, only that
//! file of that package is blocked.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::detect::clean;
use crate::paths::write_private;
use crate::registry::{self, Kind, Location, TrustedKey};

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_BYTES: usize = 512 * 1024;
const MAX_ENTRIES: usize = 2000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Blocklist {
    pub plonix_blocked: u32,
    #[serde(default)]
    pub entries: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    pub reason: String,
}

impl Entry {
    fn matches(&self, kind: Kind, name: &str, sha256: &str) -> bool {
        self.kind.is_none_or(|k| k == kind)
            && self.name.as_deref().is_none_or(|n| n == name)
            && self.sha256.as_deref().is_none_or(|s| s.eq_ignore_ascii_case(sha256))
    }
}

impl Blocklist {
    /// Why a package is blocked, if it is. `sha256` may be empty when the
    /// file is not known yet; then only entries naming the package apply.
    pub fn blocked(&self, kind: Kind, name: &str, sha256: &str) -> Option<&str> {
        self.entries
            .iter()
            .filter(|e| !sha256.is_empty() || e.sha256.is_none())
            .find(|e| e.matches(kind, name, sha256))
            .map(|e| e.reason.as_str())
    }

    /// The message shown when something blocked is refused or switched off.
    pub fn message(name: &str, reason: &str) -> String {
        format!("The Plonix maintainers blocked {}: {}", clean(name, 64), clean(reason, 300))
    }
}

pub fn parse(bytes: &[u8]) -> Result<Blocklist, String> {
    if bytes.len() > MAX_BYTES {
        return Err("the block list is too large".into());
    }
    let list: Blocklist = serde_json::from_slice(bytes).map_err(|e| format!("not a valid block list: {}", clean(&e.to_string(), 200)))?;
    if list.plonix_blocked != FORMAT_VERSION {
        return Err(format!("block list format {} is not supported", list.plonix_blocked));
    }
    if list.entries.len() > MAX_ENTRIES {
        return Err(format!("the block list has more than {MAX_ENTRIES} entries"));
    }
    for (i, e) in list.entries.iter().enumerate() {
        if e.name.is_none() && e.sha256.is_none() {
            return Err(format!("entries[{i}]: name a package, a file (sha256), or both"));
        }
        if let Some(s) = &e.sha256
            && (s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(format!("entries[{i}]: sha256 must be 64 hex characters"));
        }
        if e.reason.trim().is_empty() || e.reason.len() > 300 {
            return Err(format!("entries[{i}]: give a reason of at most 300 characters"));
        }
    }
    Ok(list)
}

/// Where the block list sits next to a Market index.
pub fn location_next_to(index: &Location) -> Option<Location> {
    registry::resolve(index, "blocked.json").ok()
}

fn saved_path(home_root: &Path) -> PathBuf {
    home_root.join("market").join("blocked.json")
}

fn official() -> Vec<TrustedKey> {
    vec![TrustedKey { key: registry::OFFICIAL_KEY.into(), publisher: registry::OFFICIAL_PUBLISHER.into() }]
}

/// Checks a block list and its signature against the Plonix Market key.
pub fn verify(bytes: &[u8], sig: &[u8]) -> Result<Blocklist, String> {
    registry::verify(bytes, sig, &official())?;
    parse(bytes)
}

/// Fetches the block list next to the official index, and keeps it when it
/// verifies. A list that cannot be fetched or verified changes nothing: the
/// last good copy stays in force.
pub fn refresh(home_root: &Path, index: &Location) -> Result<Blocklist, String> {
    let loc = location_next_to(index).ok_or("no place for a block list")?;
    let bytes = registry::fetch(&loc, MAX_BYTES).map_err(|e| format!("{e:#}"))?;
    let sig = registry::fetch(&registry::signature_location(&loc), registry::MAX_SIGNATURE_BYTES).map_err(|e| format!("{e:#}"))?;
    let list = verify(&bytes, &sig)?;
    save(home_root, &bytes, &sig).map_err(|e| format!("{e:#}"))?;
    Ok(list)
}

/// Keeps a verified list and its signature.
pub fn save(home_root: &Path, bytes: &[u8], sig: &[u8]) -> anyhow::Result<()> {
    let path = saved_path(home_root);
    std::fs::create_dir_all(path.parent().expect("has a parent"))?;
    write_private(&path.with_extension("json.sig"), sig)?;
    write_private(&path, bytes)
}

/// The block list in force: the last verified copy, checked again.
pub fn load(home_root: &Path) -> Blocklist {
    let path = saved_path(home_root);
    let (Ok(bytes), Ok(sig)) = (std::fs::read(&path), std::fs::read(path.with_extension("json.sig"))) else {
        return Blocklist::default();
    };
    verify(&bytes, &sig).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_match_by_name_file_or_both() {
        let sha = "a".repeat(64);
        let list = parse(
            format!(
                r#"{{"plonix_blocked":1,"entries":[
                    {{"name":"bad-ext","kind":"extension","reason":"Sends tokens away."}},
                    {{"sha256":"{sha}","reason":"A tampered copy."}},
                    {{"name":"notes","sha256":"{}","reason":"One bad release."}}]}}"#,
                "b".repeat(64)
            )
            .as_bytes(),
        )
        .unwrap();
        assert_eq!(list.blocked(Kind::Extension, "bad-ext", ""), Some("Sends tokens away."));
        assert_eq!(list.blocked(Kind::Skill, "bad-ext", ""), None);
        assert_eq!(list.blocked(Kind::Skill, "anything", &sha), Some("A tampered copy."));
        assert_eq!(list.blocked(Kind::Skill, "notes", &"b".repeat(64)), Some("One bad release."));
        assert_eq!(list.blocked(Kind::Skill, "notes", &"c".repeat(64)), None);
        // Before the file is known, a file-only entry cannot match.
        assert_eq!(list.blocked(Kind::Skill, "notes", ""), None);
    }

    #[test]
    fn refuses_entries_that_say_nothing() {
        assert!(parse(br#"{"plonix_blocked":1,"entries":[{"reason":"x"}]}"#).is_err());
        assert!(parse(br#"{"plonix_blocked":1,"entries":[{"name":"x","reason":" "}]}"#).is_err());
        assert!(parse(br#"{"plonix_blocked":2,"entries":[]}"#).is_err());
        assert!(parse(br#"{"plonix_blocked":1,"entries":[{"name":"x","reason":"r","extra":1}]}"#).is_err());
    }

    #[test]
    fn only_a_list_signed_by_the_plonix_key_counts() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = br#"{"plonix_blocked":1,"entries":[{"name":"bad-ext","reason":"r"}]}"#;
        let (private, _) = registry::generate_key().unwrap();
        let sig = serde_json::to_vec(&registry::sign(bytes, &private).unwrap()).unwrap();
        assert!(verify(bytes, &sig).is_err());
        // A saved copy that does not verify is ignored.
        save(dir.path(), bytes, &sig).unwrap();
        assert!(load(dir.path()).entries.is_empty());
    }
}
