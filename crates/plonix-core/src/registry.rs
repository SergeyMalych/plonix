//! The Plonix Market index: one signed catalog of everything modular.
//!
//! An index is a static file that can be hosted anywhere (a GitHub repo, a
//! company intranet, a local directory). Each entry is a package with a
//! kind (skill, rule pack, filter pack, bundle, extension), a version, where
//! to download it and the SHA-256 of the exact bytes. A package may require
//! others; a bundle is a package that is nothing but its requirements.
//!
//! An index is *validated* when it comes with a signature file next to it
//! (`index.json.sig`) made by a key Plonix trusts. The signature covers the
//! index bytes, and the index pins every package by SHA-256, so one check
//! covers the whole catalog: a download host cannot swap a package, and
//! nobody without the publisher's key can add or change one.
//!
//! ```json
//! {
//!   "plonix_index": 2,
//!   "name": "Plonix Market",
//!   "packages": [
//!     { "name": "web-servers", "kind": "rules", "version": "1.0.0",
//!       "description": "...", "author": "...",
//!       "url": "packs/web-servers.json", "sha256": "…64 hex…" },
//!     { "name": "api-kit", "kind": "bundle", "version": "1.0.0",
//!       "description": "...", "author": "...",
//!       "requires": ["api-inventory", "leaks"] }
//!   ]
//! }
//! ```
//!
//! A relative `url` is resolved against the index's own location.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::detect::{check_text, clean};
use crate::rulepack::{check_pack_name, check_version};

/// The newest index format this Plonix writes. Format 1 (no bundles,
/// skills or requirements) is still read.
pub const FORMAT_VERSION: u32 = 2;
pub const MAX_INDEX_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PACKAGES: usize = 5000;
/// A package may require at most this many others.
pub const MAX_REQUIRES: usize = 50;

/// The Market index in the Plonix repository.
pub const DEFAULT_INDEX: &str = "https://raw.githubusercontent.com/SergeyMalych/plonix/main/store/index.json";

/// The key that signs the Plonix Market index. Packages listed there are
/// reviewed by the Plonix maintainers before the index is signed.
pub const OFFICIAL_KEY: &str = "ed25519:r8Td25nYRpgJ3QwK/UapBC93gNVchFzwcSzA3g/Er/Q=";
pub const OFFICIAL_PUBLISHER: &str = "Plonix maintainers";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub plonix_index: u32,
    #[serde(default)]
    pub name: String,
    pub packages: Vec<Package>,
}

impl Index {
    pub fn get(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// An agent skill: a playbook agents discover through MCP.
    Skill,
    /// A detection rule pack (declarative).
    Rules,
    /// A filter pack: named Traffic filters (declarative).
    Filters,
    /// A detector pack: Mind Reader suggestions that hand off to another tab (declarative).
    Detectors,
    /// A list pack: named payload lists for the Bench (declarative).
    List,
    /// A set of other packages, installed together.
    Bundle,
    /// A sandboxed extension (see docs/extensions.md; not installable yet).
    Extension,
    /// A bug bounty platform: where programs and their scope come from (declarative).
    Platform,
    /// A tool: switches on a capability built into Plonix (see [`crate::tool`]).
    Tool,
}

impl Kind {
    pub const ALL: &[Kind] =
        &[Kind::Skill, Kind::Rules, Kind::Filters, Kind::Detectors, Kind::List, Kind::Bundle, Kind::Extension, Kind::Platform, Kind::Tool];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Skill => "skill",
            Kind::Rules => "rules",
            Kind::Filters => "filters",
            Kind::Detectors => "detectors",
            Kind::List => "list",
            Kind::Bundle => "bundle",
            Kind::Extension => "extension",
            Kind::Platform => "platform",
            Kind::Tool => "tool",
        }
    }

    /// Singular, for messages: "rule pack", "skill".
    pub fn noun(self) -> &'static str {
        match self {
            Kind::Skill => "skill",
            Kind::Rules => "rule pack",
            Kind::Filters => "filter pack",
            Kind::Detectors => "detector pack",
            Kind::List => "list pack",
            Kind::Bundle => "bundle",
            Kind::Extension => "extension",
            Kind::Platform => "platform",
            Kind::Tool => "tool",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub name: String,
    pub kind: Kind,
    pub version: String,
    pub description: String,
    pub author: String,
    /// Where the package file is. Bundles have none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    /// The longer description shown on the item's page: a few short paragraphs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub about: Vec<String>,
    /// Packages installed along with this one, by name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
}

/// Parses and validates an index from untrusted bytes.
pub fn parse(bytes: &[u8]) -> Result<Index, String> {
    if bytes.len() > MAX_INDEX_BYTES {
        return Err(format!("index is larger than {MAX_INDEX_BYTES} bytes"));
    }
    let mut raw: Value = serde_json::from_slice(bytes).map_err(|e| format!("not a valid Market index: {}", clean(&e.to_string(), 300)))?;
    // A newer Market can list kinds of package this Plonix does not know yet:
    // leave those out instead of refusing the whole Market.
    if let Some(Value::Array(packages)) = raw.get_mut("packages") {
        packages.retain(|p| p.get("kind").and_then(Value::as_str).is_none_or(|k| Kind::ALL.iter().any(|known| known.as_str() == k)));
    }
    let index: Index = serde_json::from_value(raw).map_err(|e| format!("not a valid Market index: {}", clean(&e.to_string(), 300)))?;
    if !(1..=FORMAT_VERSION).contains(&index.plonix_index) {
        return Err(format!("index format {} is not supported (this Plonix reads formats 1 to {FORMAT_VERSION})", index.plonix_index));
    }
    check_text(&index.name, 100, true).map_err(|e| format!("name: {e}"))?;
    if index.packages.len() > MAX_PACKAGES {
        return Err(format!("index lists more than {MAX_PACKAGES} packages"));
    }
    let mut seen = HashSet::new();
    for (i, p) in index.packages.iter().enumerate() {
        let at = |e: String| format!("packages[{i}] ({}): {e}", clean(&p.name, 64));
        check_pack_name(&p.name).map_err(|e| at(format!("name: {e}")))?;
        // Requirements name packages, so a name means one package.
        if !seen.insert(p.name.clone()) {
            return Err(at("listed twice".into()));
        }
        if index.plonix_index < 2 && (matches!(p.kind, Kind::Skill | Kind::Detectors | Kind::List | Kind::Bundle | Kind::Platform | Kind::Tool) || !p.requires.is_empty()) {
            return Err(at("skills, detector packs, list packs, platforms, tools, bundles and requirements need index format 2".into()));
        }
        check_version(&p.version).map_err(|e| at(format!("version: {e}")))?;
        check_text(&p.description, 300, false).map_err(|e| at(format!("description: {e}")))?;
        check_text(&p.author, 100, false).map_err(|e| at(format!("author: {e}")))?;
        check_text(&p.homepage, 200, true).map_err(|e| at(format!("homepage: {e}")))?;
        if p.about.len() > 8 {
            return Err(at("about: at most 8 paragraphs".into()));
        }
        for para in &p.about {
            check_text(para, 700, false).map_err(|e| at(format!("about: {e}")))?;
        }
        if p.kind == Kind::Bundle {
            if !p.url.is_empty() || !p.sha256.is_empty() {
                return Err(at("a bundle has no file: leave out url and sha256".into()));
            }
            if p.requires.is_empty() {
                return Err(at("a bundle must require at least one package".into()));
            }
        } else {
            check_text(&p.url, 500, false).map_err(|e| at(format!("url: {e}")))?;
            if p.sha256.len() != 64 || !p.sha256.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
                return Err(at("sha256: must be 64 lowercase hex characters".into()));
            }
        }
        if p.requires.len() > MAX_REQUIRES {
            return Err(at(format!("requires more than {MAX_REQUIRES} packages")));
        }
        let mut reqs = HashSet::new();
        for r in &p.requires {
            if r == &p.name {
                return Err(at("requires itself".into()));
            }
            if !reqs.insert(r) {
                return Err(at(format!("requires `{}` twice", clean(r, 64))));
            }
        }
    }
    for (i, p) in index.packages.iter().enumerate() {
        for r in &p.requires {
            if !seen.contains(r) {
                return Err(format!("packages[{i}] ({}): requires `{}`, which is not in the index", p.name, clean(r, 64)));
            }
        }
    }
    install_order_all(&index)?;
    Ok(index)
}

/// Fails if requirements form a cycle anywhere in the index.
fn install_order_all(index: &Index) -> Result<(), String> {
    for p in &index.packages {
        install_order(index, &p.name)?;
    }
    Ok(())
}

/// The packages to install for `name`, requirements first, `name` last.
pub fn install_order(index: &Index, name: &str) -> Result<Vec<String>, String> {
    fn visit(index: &Index, name: &str, path: &mut Vec<String>, done: &mut Vec<String>) -> Result<(), String> {
        if done.iter().any(|d| d == name) {
            return Ok(());
        }
        if path.iter().any(|p| p == name) {
            path.push(name.to_string());
            return Err(format!("requirements form a cycle: {}", path.join(" → ")));
        }
        let p = index.get(name).ok_or_else(|| format!("`{}` is not in the index", clean(name, 64)))?;
        path.push(name.to_string());
        for r in &p.requires {
            visit(index, r, path, done)?;
        }
        path.pop();
        done.push(name.to_string());
        Ok(())
    }
    let mut done = vec![];
    visit(index, name, &mut vec![], &mut done)?;
    Ok(done)
}

// ---- signatures --------------------------------------------------------------

/// The signature file published next to an index (`index.json.sig`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignatureFile {
    pub plonix_signature: u32,
    /// The public key that made the signature, `ed25519:<base64>`.
    pub key: String,
    /// Ed25519 signature of the exact index bytes, base64.
    pub signature: String,
}

pub const MAX_SIGNATURE_BYTES: usize = 4096;

/// A public key Plonix accepts index signatures from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedKey {
    pub key: String,
    pub publisher: String,
}

/// The official key plus any the user added in Settings › Market.
pub fn trusted_keys(extra: &[String]) -> Vec<TrustedKey> {
    let mut keys = vec![TrustedKey { key: OFFICIAL_KEY.into(), publisher: OFFICIAL_PUBLISHER.into() }];
    for k in extra {
        let k = k.trim();
        if parse_public_key(k).is_ok() && !keys.iter().any(|t| t.key == k) {
            keys.push(TrustedKey { key: k.into(), publisher: "a key you trust".into() });
        }
    }
    keys
}

pub fn parse_public_key(s: &str) -> Result<Vec<u8>, String> {
    let b = s.trim().strip_prefix("ed25519:").ok_or("a key looks like ed25519:<base64>")?;
    let raw = B64.decode(b).map_err(|_| "the key is not valid base64".to_string())?;
    if raw.len() != 32 {
        return Err("an ed25519 public key is 32 bytes".into());
    }
    Ok(raw)
}

/// Checks `sig` against the index bytes. Returns the key that signed them.
pub fn verify(index: &[u8], sig: &[u8], trusted: &[TrustedKey]) -> Result<TrustedKey, String> {
    if sig.len() > MAX_SIGNATURE_BYTES {
        return Err("signature file is too large".into());
    }
    let file: SignatureFile = serde_json::from_slice(sig).map_err(|e| format!("not a valid signature file: {}", clean(&e.to_string(), 200)))?;
    if file.plonix_signature != 1 {
        return Err(format!("signature format {} is not supported", file.plonix_signature));
    }
    let Some(key) = trusted.iter().find(|t| t.key == file.key.trim()) else {
        return Err(format!("signed by {}, which is not a key you trust (add it in Settings › Market to trust it)", clean(&file.key, 80)));
    };
    let raw = parse_public_key(&key.key)?;
    let signature = B64.decode(file.signature.trim()).map_err(|_| "the signature is not valid base64".to_string())?;
    UnparsedPublicKey::new(&ED25519, raw)
        .verify(index, &signature)
        .map_err(|_| "the signature does not match the index: it was changed after it was signed".to_string())?;
    Ok(key.clone())
}

/// A new signing key: the private key (PKCS#8, base64) and its public key.
pub fn generate_key() -> Result<(String, String), String> {
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&rng).map_err(|_| "could not generate a key".to_string())?;
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|_| "could not read the new key".to_string())?;
    Ok((B64.encode(pkcs8.as_ref()), format!("ed25519:{}", B64.encode(pair.public_key().as_ref()))))
}

/// Signs index bytes with a private key from [`generate_key`].
pub fn sign(index: &[u8], private_key: &str) -> Result<SignatureFile, String> {
    let pkcs8 = B64.decode(private_key.trim()).map_err(|_| "the private key is not valid base64".to_string())?;
    let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|_| "not an Ed25519 private key".to_string())?;
    Ok(SignatureFile {
        plonix_signature: 1,
        key: format!("ed25519:{}", B64.encode(pair.public_key().as_ref())),
        signature: B64.encode(pair.sign(index).as_ref()),
    })
}

/// Where the signature of an index at `index` is published.
pub fn signature_location(index: &Location) -> Location {
    match index {
        Location::File(p) => {
            let mut s = p.as_os_str().to_owned();
            s.push(".sig");
            Location::File(PathBuf::from(s))
        }
        Location::Url(u) => Location::Url(format!("{u}.sig")),
    }
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

// ---- fetching -----------------------------------------------------------------

/// Reads a local file or downloads a URL, refusing anything over `max`
/// bytes. Redirects may not leave https.
pub fn fetch(loc: &Location, max: usize) -> anyhow::Result<Vec<u8>> {
    use anyhow::{Context, anyhow, bail};
    let mut buf = Vec::new();
    match loc {
        Location::File(p) => {
            let f = std::fs::File::open(p).with_context(|| format!("reading {}", p.display()))?;
            f.take(max as u64 + 1).read_to_end(&mut buf).with_context(|| format!("reading {}", p.display()))?;
        }
        Location::Url(u) => {
            let mut b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout(Duration::from_secs(60)).redirects(3);
            // Downloads honour the usual proxy variables, except to this machine.
            let local = u.starts_with("http://");
            if let Some(p) = std::env::var("HTTPS_PROXY").ok().or_else(|| std::env::var("https_proxy").ok()).filter(|_| !local) {
                b = b.proxy(ureq::Proxy::new(p).context("invalid HTTPS_PROXY")?);
            }
            let resp = b.build().get(u).call().map_err(|e| match e {
                ureq::Error::Status(code, _) => anyhow!("{u}: HTTP {code}"),
                ureq::Error::Transport(t) => anyhow!("{u}: {t}"),
            })?;
            location(resp.get_url()).map_err(|e| anyhow!("{u} redirected to an unsafe location: {e}"))?;
            resp.into_reader().take(max as u64 + 1).read_to_end(&mut buf).with_context(|| format!("downloading {u}"))?;
        }
    }
    if buf.len() > max {
        bail!("{loc} is larger than {max} bytes; refusing it");
    }
    Ok(buf)
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
        // A kind this Plonix does not know yet is left out, not fatal.
        assert!(parse(good.replace("\"rules\"", "\"binary\"").as_bytes()).unwrap().packages.is_empty());
        assert!(parse(good.replace("\"rules\"", "7").as_bytes()).is_err());
        assert!(parse(good.replace(&"a".repeat(64), "abc").as_bytes()).unwrap_err().contains("sha256"));
        assert!(parse(good.replace("\"name\":\"a\"", "\"name\":\"../a\"").as_bytes()).is_err());
    }

    fn v2(packages: &str) -> String {
        format!(r#"{{"plonix_index":2,"packages":[{packages}]}}"#)
    }

    fn pkg(name: &str, kind: &str, requires: &str) -> String {
        let file = if kind == "bundle" { String::new() } else { format!(r#","url":"x/{name}","sha256":"{}""#, "a".repeat(64)) };
        format!(r#"{{"name":"{name}","kind":"{kind}","version":"1.0.0","description":"d","author":"x","requires":[{requires}]{file}}}"#)
    }

    #[test]
    fn bundles_and_requirements() {
        let ok = v2(&[pkg("a", "skill", ""), pkg("b", "filters", "\"a\""), pkg("kit", "bundle", "\"b\"")].join(","));
        let index = parse(ok.as_bytes()).unwrap();
        assert_eq!(install_order(&index, "kit").unwrap(), vec!["a", "b", "kit"]);

        let cycle = v2(&[pkg("a", "skill", "\"b\""), pkg("b", "skill", "\"a\"")].join(","));
        assert!(parse(cycle.as_bytes()).unwrap_err().contains("cycle"));
        let missing = v2(&pkg("a", "skill", "\"ghost\""));
        assert!(parse(missing.as_bytes()).unwrap_err().contains("not in the index"));
        let empty_bundle = v2(&pkg("kit", "bundle", ""));
        assert!(parse(empty_bundle.as_bytes()).unwrap_err().contains("at least one"));
        let same_name = v2(&[pkg("a", "skill", ""), pkg("a", "rules", "")].join(","));
        assert!(parse(same_name.as_bytes()).unwrap_err().contains("twice"));
        // Format 1 has no skills or bundles.
        assert!(parse(ok.replace("\"plonix_index\":2", "\"plonix_index\":1").as_bytes()).unwrap_err().contains("format 2"));
    }

    #[test]
    fn signatures() {
        let (private, public) = generate_key().unwrap();
        let index = b"{\"plonix_index\":2,\"packages\":[]}";
        let sig = serde_json::to_vec(&sign(index, &private).unwrap()).unwrap();
        let trusted = vec![TrustedKey { key: public.clone(), publisher: "test".into() }];
        assert_eq!(verify(index, &sig, &trusted).unwrap().key, public);
        assert!(verify(b"{\"plonix_index\":2,\"packages\":[ ]}", &sig, &trusted).unwrap_err().contains("changed"));
        assert!(verify(index, &sig, &trusted_keys(&[])).unwrap_err().contains("not a key you trust"));
        assert!(verify(index, b"garbage", &trusted).is_err());
        assert!(trusted_keys(&["not a key".into()]).len() == 1);
        assert_eq!(signature_location(&Location::Url("https://x.test/i.json".into())), Location::Url("https://x.test/i.json.sig".into()));
    }

    /// The checked-in index must be signed by the official key.
    #[test]
    fn repository_index_is_signed() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../store");
        let index = std::fs::read(root.join("index.json")).unwrap();
        let sig = std::fs::read(root.join("index.json.sig")).unwrap();
        verify(&index, &sig, &trusted_keys(&[])).expect("store/index.json must be re-signed after every change (see store/README.md)");
    }

    /// The checked-in community index must match the packs next to it, or
    /// every install from it would fail verification.
    #[test]
    fn repository_index_matches_its_packs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../store");
        let bytes = std::fs::read(root.join("index.json")).unwrap();
        let index = parse(&bytes).unwrap();
        let base = Location::File(root.join("index.json"));
        for p in index.packages.iter().filter(|p| p.kind != Kind::Bundle) {
            let Location::File(path) = resolve(&base, &p.url).unwrap() else { panic!("expected a relative url for {}", p.name) };
            let data = std::fs::read(&path).unwrap();
            assert_eq!(crate::rulepack::sha256_hex(&data), p.sha256, "{}: sha256 in store/index.json is stale", p.name);
            let (name, version) = match p.kind {
                Kind::Rules => crate::rulepack::parse(&data).map(|x| (x.doc.name, x.doc.version)).map_err(|e| e.to_string()),
                Kind::Filters => crate::filterpack::parse(&data).map(|x| (x.doc.name, x.doc.version)),
                Kind::Detectors => crate::detectorpack::parse(&data).map(|x| (x.doc.name, x.doc.version)),
                Kind::List => crate::listpack::parse(&data).map(|x| (x.doc.name, x.doc.version)),
                Kind::Tool => crate::tool::parse(&data).map(|x| (x.doc.name, x.doc.version)),
                Kind::Skill => crate::skill::parse(&data).map(|x| (x.name, x.version)),
                Kind::Extension => crate::market::extension_manifest(&data).map(|m| (m.name, m.version)),
                Kind::Platform => crate::platform::parse(&data).map(|x| (x.doc.name, x.doc.version)),
                Kind::Bundle => unreachable!(),
            }
            .unwrap();
            assert_eq!((name.as_str(), version.as_str()), (p.name.as_str(), p.version.as_str()));
        }
    }
}
