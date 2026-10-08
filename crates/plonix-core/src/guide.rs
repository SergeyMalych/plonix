//! How to use each item in the Plonix Market: where it shows up, the steps to
//! get it working, and a screenshot of it at work.
//!
//! Guides ship inside Plonix (`store/guides.json` and `store/guides/*.jpg`),
//! not in the signed Market list, so better help reaches people with the app
//! and never changes what a signature covers. Only Plonix's own items have
//! one; the Market shows it on the item's page.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

pub const DATA: &str = include_str!("../../../store/guides.json");

/// Screenshots by name, served to the window at `/ui/guide/<name>.jpg`.
pub const SHOTS: &[(&str, &[u8])] = &[
    ("secret-sweep", include_bytes!("../../../store/guides/secret-sweep.jpg")),
    ("js-endpoints", include_bytes!("../../../store/guides/js-endpoints.jpg")),
    ("subdomain-discovery", include_bytes!("../../../store/guides/subdomain-discovery.jpg")),
    ("parameter-probe", include_bytes!("../../../store/guides/parameter-probe.jpg")),
    ("security-headers", include_bytes!("../../../store/guides/security-headers.jpg")),
    ("saved-users", include_bytes!("../../../store/guides/saved-users.jpg")),
    ("access-check", include_bytes!("../../../store/guides/access-check.jpg")),
    ("programs", include_bytes!("../../../store/guides/programs.jpg")),
];

/// How to use one Market item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guide {
    /// Where in Plonix it shows up, in a few words.
    #[serde(rename = "where")]
    pub place: String,
    /// The screen that place is on, to open it in a click.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<String>,
    pub steps: Vec<String>,
    /// A screenshot in [`SHOTS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shot: Option<String>,
    /// How to use it from the command line or Claude Code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Doc {
    plonix_guides: u32,
    items: BTreeMap<String, Guide>,
}

fn parse(text: &str) -> Result<BTreeMap<String, Guide>, String> {
    let d: Doc = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if d.plonix_guides != 1 {
        return Err(format!("plonix_guides {} is not one this Plonix reads", d.plonix_guides));
    }
    for (name, g) in &d.items {
        if g.steps.is_empty() || g.steps.iter().any(|s| s.trim().is_empty()) {
            return Err(format!("{name}: every guide has steps, none empty"));
        }
        if let Some(s) = &g.shot
            && shot(s).is_none()
        {
            return Err(format!("{name}: no screenshot named {s}"));
        }
    }
    Ok(d.items)
}

fn all() -> &'static BTreeMap<String, Guide> {
    static D: OnceLock<BTreeMap<String, Guide>> = OnceLock::new();
    D.get_or_init(|| parse(DATA).expect("store/guides.json is checked by the tests"))
}

/// The guide for one of Plonix's own Market items.
pub fn get(name: &str) -> Option<&'static Guide> {
    all().get(name)
}

/// A screenshot's bytes (JPEG), by name.
pub fn shot(name: &str) -> Option<&'static [u8]> {
    SHOTS.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_market_item_has_a_guide() {
        let guides = parse(DATA).unwrap();
        let index = crate::registry::parse(crate::market::SNAPSHOT[0].1.as_bytes()).unwrap();
        let tools = ["saved-users", "access-check", "callbacks", "programs"];
        for name in index.packages.iter().map(|p| p.name.as_str()).chain(tools) {
            assert!(guides.contains_key(name), "{name} has no guide in store/guides.json");
        }
        for (name, bytes) in SHOTS {
            assert!(bytes.starts_with(&[0xff, 0xd8]), "{name} is a JPEG");
            assert!(bytes.len() < 200_000, "{name} is small enough to ship in the app");
        }
    }

    #[test]
    fn bad_guides_are_refused() {
        let ok = r#"{"plonix_guides":1,"items":{"x":{"where":"Scope","steps":["Do it."]}}}"#;
        assert!(parse(ok).is_ok());
        assert!(parse(&ok.replace(r#"["Do it."]"#, "[]")).unwrap_err().contains("steps"));
        assert!(parse(&ok.replace(r#""steps""#, r#""shot":"nope","steps""#)).unwrap_err().contains("no screenshot"));
        assert!(parse(&ok.replace("1,", "2,")).is_err());
    }
}
