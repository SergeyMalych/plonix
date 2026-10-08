//! Tools: the smallest kind of Market package. A tool turns on a capability
//! that is already built into Plonix — a sidebar tab, a Bench control — that
//! ships switched off until someone installs it.
//!
//! A tool package is only data: a name and the id of the built-in feature it
//! switches on, chosen from a fixed list this build knows ([`FEATURES`]). It
//! carries no code and no settings of its own; installing it adds the
//! feature's id to the set Plonix reports as on, and removing it takes it
//! away. Installed tools live in `$PLONIX_HOME/tools`, pinned by checksum
//! like every other pack.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::detect::{check_text, clean};
use crate::paths::Home;
use crate::rulepack::{MAX_PACK_BYTES, check_pack_name, check_version, sha256_hex};
use crate::shelf::Shelf;

pub const FORMAT_VERSION: u32 = 1;
const MAX_INSTALLED: usize = 50;

/// A built-in feature a tool can switch on. The id is what a tool package
/// names; the title and summary are what the Market shows.
pub struct Feature {
    pub id: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
}

/// Every feature a tool may switch on. A tool naming anything else is
/// rejected, so a catalog cannot turn on something this build does not have.
pub const FEATURES: &[Feature] = &[
    Feature {
        id: "saved-users",
        title: "Saved users",
        summary: "Keep the cookies and tokens for each user of an application, and switch which one the Bench sends as.",
    },
    Feature {
        id: "access-check",
        title: "Access check",
        summary: "A screen that replays selected requests as each saved user, and once signed out, and lines up the responses.",
    },
    Feature {
        id: "callbacks",
        title: "Callbacks",
        summary: "A screen that hands out unique hosts to put in requests and lists every DNS lookup, HTTP request or mail that reaches one.",
    },
    Feature {
        id: "programs",
        title: "Programs",
        summary: "A screen that brings in a bug bounty or disclosure program, turns its assets into scope and keeps its rules while you test.",
    },
];

pub fn feature(id: &str) -> Option<&'static Feature> {
    FEATURES.iter().find(|f| f.id == id)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDoc {
    /// Format version, currently 1.
    pub plonix_tool: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    /// The id of the built-in feature this tool switches on.
    pub feature: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
}

#[derive(Debug, Clone)]
pub struct Tool {
    pub doc: ToolDoc,
    pub sha256: String,
}

/// Parses and validates a tool package from untrusted bytes.
pub fn parse(bytes: &[u8]) -> Result<Tool, String> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(format!("pack is {} bytes; the limit is {MAX_PACK_BYTES}", bytes.len()));
    }
    let doc: ToolDoc =
        serde_json::from_slice(bytes).map_err(|e| format!("not a valid tool: {}", clean(&e.to_string(), 300)))?;
    let mut errors = vec![];
    if doc.plonix_tool != FORMAT_VERSION {
        errors.push(format!("plonix_tool: format {} is not supported (this Plonix reads format {FORMAT_VERSION})", doc.plonix_tool));
    }
    if let Err(e) = check_pack_name(&doc.name) {
        errors.push(format!("name: {e}"));
    }
    if let Err(e) = check_version(&doc.version) {
        errors.push(format!("version: {e}"));
    }
    for (field, value, max, empty) in
        [("description", &doc.description, 300, false), ("author", &doc.author, 100, false), ("license", &doc.license, 64, true), ("homepage", &doc.homepage, 200, true)]
    {
        if let Err(e) = check_text(value, max, empty) {
            errors.push(format!("{field}: {e}"));
        }
    }
    if feature(&doc.feature).is_none() {
        errors.push(format!("feature: `{}` is not a built-in feature this Plonix knows", clean(&doc.feature, 40)));
    }
    if !errors.is_empty() {
        return Err(format!("invalid tool:\n  - {}", errors.join("\n  - ")));
    }
    Ok(Tool { doc, sha256: sha256_hex(bytes) })
}

/// Tools installed in a Plonix home, and the features they switch on.
pub struct ToolLibrary {
    shelf: Shelf,
}

impl ToolLibrary {
    pub fn new(home: &Home) -> Self {
        Self::at(&home.root.join("tools"))
    }

    pub fn at(dir: &Path) -> Self {
        Self { shelf: Shelf::new(dir, "tool", "market", MAX_INSTALLED) }
    }

    pub fn stamp(&self) -> Option<std::time::SystemTime> {
        self.shelf.stamp()
    }

    pub fn install(&self, bytes: &[u8], source: &str, expected_sha256: Option<&str>) -> Result<(Tool, Option<String>)> {
        Shelf::check_sha(bytes, expected_sha256, "tool")?;
        let tool = parse(bytes).map_err(|e| anyhow!(e))?;
        let previous = self.shelf.put(&tool.doc.name, &tool.doc.version, bytes, source)?;
        Ok((tool, previous))
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        self.shelf.remove(name)
    }

    pub fn installed_version(&self, name: &str) -> Option<String> {
        self.shelf.installed_version(name)
    }

    pub fn installed(&self) -> Vec<crate::shelf::Installed> {
        self.shelf.installed()
    }

    /// The ids of the built-in features switched on by the installed tools.
    pub fn enabled_features(&self) -> BTreeSet<String> {
        let (verified, problems) = self.shelf.verified();
        for p in &problems {
            tracing::warn!("tools: {p}");
        }
        verified
            .into_iter()
            .filter_map(|v| parse(&v.bytes).ok().filter(|t| t.doc.name == v.name))
            .map(|t| t.doc.feature)
            .filter(|f| feature(f).is_some())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACK: &str = r#"{"plonix_tool":1,"name":"saved-users","version":"1.0.0","description":"Switch users in the Bench","author":"Plonix contributors","feature":"saved-users"}"#;

    #[test]
    fn parses_and_rejects_unknown_feature() {
        assert_eq!(parse(PACK.as_bytes()).unwrap().doc.feature, "saved-users");
        let bad = PACK.replace("\"feature\":\"saved-users\"", "\"feature\":\"make-coffee\"");
        assert!(parse(bad.as_bytes()).unwrap_err().contains("not a built-in feature"));
        assert!(parse(PACK.replace("\"feature\"", "\"extra\":1,\"feature\"").as_bytes()).unwrap_err().contains("unknown field"));
    }

    #[test]
    fn install_enables_the_feature_and_remove_disables_it() {
        let dir = tempfile::tempdir().unwrap();
        let lib = ToolLibrary::at(dir.path());
        assert!(lib.enabled_features().is_empty());
        lib.install(PACK.as_bytes(), "./saved-users.json", None).unwrap();
        assert!(lib.enabled_features().contains("saved-users"));
        assert!(lib.remove("saved-users").unwrap());
        assert!(lib.enabled_features().is_empty());
    }

    #[test]
    fn a_tampered_tool_switches_nothing_on() {
        let dir = tempfile::tempdir().unwrap();
        let lib = ToolLibrary::at(dir.path());
        lib.install(PACK.as_bytes(), "x", None).unwrap();
        std::fs::write(dir.path().join("packs/saved-users.json"), PACK.replace("saved-users", "access-check")).unwrap();
        assert!(lib.enabled_features().is_empty());
    }
}
