//! The Plonix store: a JSON index of community rule packs and extensions.
//!
//! An index is a static file that can be hosted anywhere (a GitHub repo, a
//! company intranet, a local directory). Each entry names a package, its
//! version, where to download it and the SHA-256 of the exact bytes. The
//! client refuses anything that does not hash to the listed value, so a
//! compromised download host cannot swap a package without also changing
//! the index.
//!
//! ```json
//! {
//!   "plonix_index": 1,
//!   "name": "Plonix community store",
//!   "packages": [
//!     { "name": "web-servers", "kind": "rules", "version": "1.0.0",
//!       "description": "...", "author": "...",
//!       "url": "packs/web-servers.json", "sha256": "…64 hex…" }
//!   ]
//! }
//! ```
//!
//! A relative `url` is resolved against the index's own location.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::detect::{check_text, clean};
use crate::rulepack::{check_pack_name, check_version};

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PACKAGES: usize = 5000;

/// The community index in the Plonix repository.
pub const DEFAULT_INDEX: &str = "https://raw.githubusercontent.com/SergeyMalych/plonix/main/store/index.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub plonix_index: u32,
    #[serde(default)]
    pub name: String,
    pub packages: Vec<Package>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// A detection rule pack (declarative, installable today).
    Rules,
    /// A filter pack: named Traffic filters (declarative, installable today).
    Filters,
    /// A sandboxed extension (see docs/extensions.md; not installable yet).
    Extension,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: String,
    pub kind: Kind,
    pub version: String,
    pub description: String,
    pub author: String,
    pub url: String,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
}

/// Parses and validates an index from untrusted bytes.
pub fn parse(bytes: &[u8]) -> Result<Index, String> {
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(format!("index is larger than {MAX_INDEX_BYTES} bytes"));
    }
    let index: Index = serde_json::from_slice(bytes).map_err(|e| format!("not a valid store index: {}", clean(&e.to_string(), 300)))?;
    if index.plonix_index != FORMAT_VERSION {
        return Err(format!("index format {} is not supported (this Plonix reads format {FORMAT_VERSION})", index.plonix_index));
    }
    check_text(&index.name, 100, true).map_err(|e| format!("name: {e}"))?;
    if index.packages.len() > MAX_PACKAGES {
        return Err(format!("index lists more than {MAX_PACKAGES} packages"));
    }
    let mut seen = std::collections::HashSet::new();
    for (i, p) in index.packages.iter().enumerate() {
        let at = |e: String| format!("packages[{i}] ({}): {e}", clean(&p.name, 64));
        check_pack_name(&p.name).map_err(|e| at(format!("name: {e}")))?;
        if !seen.insert((p.name.clone(), p.kind)) {
            return Err(at("listed twice".into()));
        }
        check_version(&p.version).map_err(|e| at(format!("version: {e}")))?;
        check_text(&p.description, 300, false).map_err(|e| at(format!("description: {e}")))?;
        check_text(&p.author, 100, false).map_err(|e| at(format!("author: {e}")))?;
        check_text(&p.homepage, 200, true).map_err(|e| at(format!("homepage: {e}")))?;
        check_text(&p.url, 500, false).map_err(|e| at(format!("url: {e}")))?;
        if p.sha256.len() != 64 || !p.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return Err(at("sha256: must be 64 lowercase hex characters".into()));
        }
    }
    Ok(index)
}

/// Where an index or package is read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    File(PathBuf),
    Url(String),
}

impl std::fmt::Display for Location {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Location::File(p) => write!(f, "{}", p.display()),
            Location::Url(u) => f.write_str(u),
        }
    }
}

/// Accepts a local path or a URL. URLs must be `https://`, except plain
/// `http://` to this machine (for testing an index you are writing).
pub fn location(s: &str) -> Result<Location, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("https://") {
        url_host(rest)?;
        return Ok(Location::Url(s.to_string()));
    }
    if let Some(rest) = s.strip_prefix("http://") {
        let host = url_host(rest)?;
        if matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]") {
            return Ok(Location::Url(s.to_string()));
        }
        return Err(format!("refusing plain http:// for {host}: use https:// so the download cannot be tampered with"));
    }
    if let Some(path) = s.strip_prefix("file://") {
        return Ok(Location::File(PathBuf::from(path)));
    }
    if s.contains("://") {
        return Err(format!("unsupported URL scheme in `{}`; use https:// or a local path", clean(s, 100)));
    }
    if s.is_empty() {
        return Err("empty location".into());
    }
    Ok(Location::File(PathBuf::from(s)))
}

fn url_host(rest: &str) -> Result<String, String> {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || authority.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("URL has no valid host".into());
    }
    let host = if authority.starts_with('[') {
        authority.split_inclusive(']').next().unwrap_or("").to_string()
    } else {
        authority.split(':').next().unwrap_or("").to_string()
    };
    Ok(host.to_ascii_lowercase())
}

/// Resolves a package `url` from an index at `base`. Relative URLs are
/// relative to the index; they may not climb out of its directory.
pub fn resolve(base: &Location, url: &str) -> Result<Location, String> {
    if url.contains("://") {
        return location(url);
    }
    if url.starts_with('/') || url.contains('\\') || url.split('/').any(|seg| seg == "..") || url.contains('?') || url.contains('#') {
        return Err(format!("package url `{}` must be absolute https:// or relative to the index without `..`", clean(url, 100)));
    }
    match base {
        Location::File(p) => Ok(Location::File(p.parent().unwrap_or(Path::new(".")).join(url))),
        Location::Url(u) => {
            let dir = match u.rfind('/') {
                Some(i) if i >= "https://".len() => &u[..=i],
                _ => return Err("index URL has no path".into()),
            };
            Ok(Location::Url(format!("{dir}{url}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locations() {
        assert!(matches!(location("https://example.com/i.json"), Ok(Location::Url(_))));
        assert!(matches!(location("http://127.0.0.1:8000/i.json"), Ok(Location::Url(_))));
        assert!(location("http://example.com/i.json").unwrap_err().contains("https"));
        assert!(location("ftp://example.com/x").is_err());
        assert!(location("https://user@evil.com/x").is_err());
        assert_eq!(location("./packs/a.json"), Ok(Location::File("./packs/a.json".into())));
    }

    #[test]
    fn relative_resolution() {
        let base = location("https://raw.example.com/store/index.json").unwrap();
        assert_eq!(resolve(&base, "packs/a.json").unwrap(), Location::Url("https://raw.example.com/store/packs/a.json".into()));
        assert!(resolve(&base, "../secrets.json").is_err());
        assert!(resolve(&base, "/etc/passwd").is_err());
        assert!(resolve(&base, "http://evil.com/a.json").is_err());
        let base = Location::File("/srv/store/index.json".into());
        assert_eq!(resolve(&base, "packs/a.json").unwrap(), Location::File("/srv/store/packs/a.json".into()));
    }

    #[test]
    fn index_validation() {
        let good = format!(
            r#"{{"plonix_index":1,"packages":[{{"name":"a","kind":"rules","version":"1.0.0","description":"d","author":"x","url":"packs/a.json","sha256":"{}"}}]}}"#,
            "a".repeat(64)
        );
        assert_eq!(parse(good.as_bytes()).unwrap().packages.len(), 1);
        assert!(parse(good.replace("\"rules\"", "\"binary\"").as_bytes()).is_err());
        assert!(parse(good.replace(&"a".repeat(64), "abc").as_bytes()).unwrap_err().contains("sha256"));
        assert!(parse(good.replace("\"name\":\"a\"", "\"name\":\"../a\"").as_bytes()).is_err());
    }

    /// The checked-in community index must match the packs next to it, or
    /// every install from it would fail verification.
    #[test]
    fn repository_index_matches_its_packs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../store");
        let bytes = std::fs::read(root.join("index.json")).unwrap();
        let index = parse(&bytes).unwrap();
        let base = Location::File(root.join("index.json"));
        for p in &index.packages {
            let Location::File(path) = resolve(&base, &p.url).unwrap() else { panic!("expected a relative url for {}", p.name) };
            let data = std::fs::read(&path).unwrap();
            assert_eq!(crate::rulepack::sha256_hex(&data), p.sha256, "{}: sha256 in store/index.json is stale", p.name);
            let (name, version) = match p.kind {
                Kind::Rules => crate::rulepack::parse(&data).map(|x| (x.doc.name, x.doc.version)).map_err(|e| e.to_string()),
                Kind::Filters => crate::filterpack::parse(&data).map(|x| (x.doc.name, x.doc.version)),
                Kind::Extension => continue,
            }
            .unwrap();
            assert_eq!((name.as_str(), version.as_str()), (p.name.as_str(), p.version.as_str()));
        }
    }
}
