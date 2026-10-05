//! Bug bounty platforms: where programs come from.
//!
//! A platform pack is declarative data, like the other Market packs: the
//! platform's API address, how it signs you in, and where in its JSON answers
//! the programs and their assets are. Plonix reads it with one fetcher that
//! only ever talks to the address the pack declares, over HTTPS, with the
//! token the user gave for that platform. A pack runs no code, so a new
//! platform can be added from the Market without a Plonix release.
//!
//! Tokens are kept in the macOS Keychain (in a private file elsewhere) and are
//! never handed to agents.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::detect::{check_text, clean};
use crate::model::now_ms;
use crate::paths::{Home, write_private};
use crate::bounty::{Asset, AssetKind, Program, Rules};
use crate::rulepack::{MAX_PACK_BYTES, check_pack_name, check_version, sha256_hex};
use crate::shelf::Shelf;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_INSTALLED_PACKS: usize = 50;
/// Largest API answer Plonix reads.
pub const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
/// Most pages Plonix follows for one list.
pub const MAX_PAGES: usize = 50;

/// The platform packs built into Plonix.
pub const BUILTIN: &[(&str, &str)] = &[("hackerone", include_str!("../../../store/platforms/hackerone.json"))];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformDoc {
    /// Format version, currently 1.
    pub plonix_platform: u32,
    pub name: String,
    pub version: String,
    /// Shown to the user: "HackerOne".
    pub title: String,
    pub description: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    /// The longer description on the platform's Market page: a few short paragraphs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub about: Vec<String>,
    /// The API's origin, `https://api.example.com`. Plonix sends nothing anywhere else.
    pub api: String,
    /// A program's page on the platform; `{handle}` is replaced.
    pub program_url: String,
    pub auth: AuthDef,
    pub programs: ListDef<ProgramFields>,
    /// The program's own page in the API, for its policy text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<DetailDef>,
    pub scopes: ListDef<ScopeFields>,
    /// The platform's asset types to Plonix's: `"WILDCARD": "wildcard"`.
    pub asset_kinds: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    /// HTTP Basic with a user name and a token.
    Basic,
    /// `Authorization: Bearer <token>`.
    Bearer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthDef {
    pub kind: AuthKind,
    /// Label for the user name field (Basic only): "API username".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub user_label: String,
    /// Label for the token field: "API token".
    pub secret_label: String,
    /// One line on where to get the token.
    pub help: String,
    /// The page where the user creates a token.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token_url: String,
}

/// A paged list in the API. Paths and pointers are relative to the API origin
/// and the JSON answer (RFC 6901 pointers such as `/data`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListDef<F> {
    /// `{handle}` is replaced by the program's handle.
    pub path: String,
    /// Where the array of items is.
    pub items: String,
    /// Where the next page's URL is, if the API pages.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub next: String,
    #[serde(default = "default_pages")]
    pub max_pages: usize,
    pub fields: F,
}

fn default_pages() -> usize {
    10
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramFields {
    pub handle: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bounty: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeFields {
    pub identifier: String,
    pub kind: String,
    /// A boolean: whether the asset may be tested and reported.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub in_scope: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bounty: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instruction: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub max_severity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetailDef {
    pub path: String,
    /// Where the policy text is.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub policy: String,
}

#[derive(Debug, Clone)]
pub struct PlatformPack {
    pub doc: PlatformDoc,
    pub sha256: String,
}

/// A platform as clients see it.
#[derive(Debug, Clone, Serialize)]
pub struct PlatformInfo {
    pub name: String,
    pub title: String,
    pub version: String,
    pub description: String,
    pub homepage: String,
    pub auth: AuthDef,
    pub builtin: bool,
    pub source: String,
    pub connected: bool,
}

fn check_pointer(p: &str) -> Result<(), String> {
    if p.is_empty() || (p.starts_with('/') && p.len() <= 200 && !p.chars().any(|c| c.is_control())) {
        Ok(())
    } else {
        Err(format!("`{}` must be a JSON pointer such as /data", clean(p, 60)))
    }
}

fn check_path(p: &str) -> Result<(), String> {
    if p.starts_with('/') && !p.starts_with("//") && p.len() <= 300 && !p.contains("..") && !p.chars().any(|c| c.is_control() || c.is_whitespace()) {
        Ok(())
    } else {
        Err(format!("`{}` must be a path on the API such as /v1/programs", clean(p, 60)))
    }
}

/// Whether `api` is an origin Plonix may call: HTTPS, or plain HTTP on this
/// computer (for testing a pack against a local stand-in).
fn check_origin(api: &str) -> Result<(), String> {
    let rest = api.strip_prefix("https://").or_else(|| api.strip_prefix("http://").filter(|r| r.starts_with("127.0.0.1") || r.starts_with("localhost")));
    match rest {
        Some(host) if !host.is_empty() && !host.contains(['/', '?', '#', '@']) && !host.chars().any(|c| c.is_whitespace() || c.is_control()) => Ok(()),
        _ => Err(format!("`{}` must be an https origin such as https://api.example.com, with no path", clean(api, 80))),
    }
}

/// Parses and validates a platform pack from untrusted bytes. Reports every problem.
pub fn parse(bytes: &[u8]) -> Result<PlatformPack, String> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(format!("pack is {} bytes; the limit is {MAX_PACK_BYTES}", bytes.len()));
    }
    let doc: PlatformDoc = serde_json::from_slice(bytes).map_err(|e| format!("not a valid platform pack: {}", clean(&e.to_string(), 300)))?;
    let mut errors = vec![];
    if doc.plonix_platform != FORMAT_VERSION {
        errors.push(format!("plonix_platform: format {} is not supported (this Plonix reads format {FORMAT_VERSION})", doc.plonix_platform));
    }
    if let Err(e) = check_pack_name(&doc.name) {
        errors.push(format!("name: {e}"));
    }
    if let Err(e) = check_version(&doc.version) {
        errors.push(format!("version: {e}"));
    }
    for (field, value, max, empty) in [
        ("title", &doc.title, 60, false),
        ("description", &doc.description, 300, false),
        ("author", &doc.author, 100, false),
        ("license", &doc.license, 64, true),
        ("homepage", &doc.homepage, 200, true),
        ("program_url", &doc.program_url, 300, false),
        ("auth.user_label", &doc.auth.user_label, 60, doc.auth.kind != AuthKind::Basic),
        ("auth.secret_label", &doc.auth.secret_label, 60, false),
        ("auth.help", &doc.auth.help, 300, false),
        ("auth.token_url", &doc.auth.token_url, 300, true),
    ] {
        if let Err(e) = check_text(value, max, empty) {
            errors.push(format!("{field}: {e}"));
        }
    }
    if doc.about.len() > 8 {
        errors.push("about: at most 8 paragraphs".into());
    }
    for (i, para) in doc.about.iter().enumerate() {
        if let Err(e) = check_text(para, 700, false) {
            errors.push(format!("about[{i}]: {e}"));
        }
    }
    if let Err(e) = check_origin(&doc.api) {
        errors.push(format!("api: {e}"));
    }
    if !doc.program_url.starts_with("https://") {
        errors.push("program_url: must start with https://".into());
    }
    let mut paths = vec![("programs.path", &doc.programs.path), ("scopes.path", &doc.scopes.path)];
    if let Some(d) = &doc.program {
        paths.push(("program.path", &d.path));
    }
    for (field, p) in paths {
        if let Err(e) = check_path(p) {
            errors.push(format!("{field}: {e}"));
        }
    }
    let f = &doc.programs.fields;
    let s = &doc.scopes.fields;
    let mut pointers = vec![
        ("programs.items", &doc.programs.items),
        ("programs.next", &doc.programs.next),
        ("programs.fields.handle", &f.handle),
        ("programs.fields.name", &f.name),
        ("programs.fields.bounty", &f.bounty),
        ("programs.fields.state", &f.state),
        ("scopes.items", &doc.scopes.items),
        ("scopes.next", &doc.scopes.next),
        ("scopes.fields.identifier", &s.identifier),
        ("scopes.fields.kind", &s.kind),
        ("scopes.fields.in_scope", &s.in_scope),
        ("scopes.fields.bounty", &s.bounty),
        ("scopes.fields.instruction", &s.instruction),
        ("scopes.fields.max_severity", &s.max_severity),
    ];
    if let Some(d) = &doc.program {
        pointers.push(("program.policy", &d.policy));
    }
    for (field, p) in pointers {
        if let Err(e) = check_pointer(p) {
            errors.push(format!("{field}: {e}"));
        }
    }
    for (field, p) in [("programs.fields.handle", &f.handle), ("programs.fields.name", &f.name), ("scopes.fields.identifier", &s.identifier), ("scopes.fields.kind", &s.kind)] {
        if p.is_empty() {
            errors.push(format!("{field}: required"));
        }
    }
    for (field, n) in [("programs.max_pages", doc.programs.max_pages), ("scopes.max_pages", doc.scopes.max_pages)] {
        if !(1..=MAX_PAGES).contains(&n) {
            errors.push(format!("{field}: between 1 and {MAX_PAGES}"));
        }
    }
    if doc.asset_kinds.len() > 100 {
        errors.push("asset_kinds: at most 100".into());
    }
    for (k, v) in &doc.asset_kinds {
        if check_text(k, 60, false).is_err() || !["web", "wildcard", "ip", "cidr", "mobile", "source", "other"].contains(&v.as_str()) {
            errors.push(format!("asset_kinds: `{}` must map to web, wildcard, ip, cidr, mobile, source or other", clean(k, 60)));
        }
    }
    if !errors.is_empty() {
        return Err(format!("invalid platform pack:\n  - {}", errors.join("\n  - ")));
    }
    Ok(PlatformPack { doc, sha256: sha256_hex(bytes) })
}

/// Platform packs installed in a Plonix home, plus the built-in ones.
pub struct PlatformLibrary {
    dir: PathBuf,
    shelf: Shelf,
    creds: Credentials,
}

impl PlatformLibrary {
    pub fn new(home: &Home) -> Self {
        let dir = home.root.join("platforms");
        Self { shelf: Shelf::new(&dir, "platform pack", "market", MAX_INSTALLED_PACKS), creds: Credentials::new(home), dir }
    }

    pub fn at(dir: &Path) -> Self {
        Self { shelf: Shelf::new(dir, "platform pack", "market", MAX_INSTALLED_PACKS), creds: Credentials::file(dir), dir: dir.to_path_buf() }
    }

    pub fn install(&self, bytes: &[u8], source: &str, expected_sha256: Option<&str>) -> Result<(PlatformPack, Option<String>)> {
        Shelf::check_sha(bytes, expected_sha256, "pack")?;
        let pack = parse(bytes).map_err(|e| anyhow!(e))?;
        if BUILTIN.iter().any(|(n, _)| *n == pack.doc.name) {
            bail!("`{}` is the name of a built-in platform; give the pack a different name", pack.doc.name);
        }
        let previous = self.shelf.put(&pack.doc.name, &pack.doc.version, bytes, source)?;
        Ok((pack, previous))
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        if BUILTIN.iter().any(|(n, _)| *n == name) {
            bail!("`{name}` is built in and cannot be removed");
        }
        let _ = self.creds.remove(name);
        self.forget_catalog(name);
        self.shelf.remove(name)
    }

    pub fn installed_version(&self, name: &str) -> Option<String> {
        self.shelf.installed_version(name)
    }

    pub fn installed(&self) -> Vec<crate::shelf::Installed> {
        self.shelf.installed()
    }

    /// Every platform in effect, built-in first. Problems are returned beside them.
    pub fn load(&self) -> (Vec<(PlatformPack, bool, String)>, Vec<String>) {
        let mut out: Vec<(PlatformPack, bool, String)> = vec![];
        let mut problems = vec![];
        for (name, text) in BUILTIN {
            match parse(text.as_bytes()) {
                Ok(p) => out.push((p, true, "built-in".into())),
                Err(e) => problems.push(format!("built-in platform {name}: {e}")),
            }
        }
        let (verified, more) = self.shelf.verified();
        problems.extend(more);
        for v in verified {
            match parse(&v.bytes) {
                Ok(p) if p.doc.name == v.name && !out.iter().any(|(o, ..)| o.doc.name == p.doc.name) => out.push((p, false, v.entry.source.clone())),
                Ok(_) => problems.push(format!("platform pack {}: name inside the file does not match", v.name)),
                Err(e) => problems.push(format!("platform pack {}: {e}", v.name)),
            }
        }
        (out, problems)
    }

    pub fn get(&self, name: &str) -> Option<PlatformPack> {
        self.load().0.into_iter().find(|(p, ..)| p.doc.name == name).map(|(p, ..)| p)
    }

    pub fn infos(&self) -> (Vec<PlatformInfo>, Vec<String>) {
        let (packs, problems) = self.load();
        let infos = packs
            .into_iter()
            .map(|(p, builtin, source)| PlatformInfo {
                connected: self.creds.get(&p.doc.name).is_some(),
                name: p.doc.name,
                title: p.doc.title,
                version: p.doc.version,
                description: p.doc.description,
                homepage: p.doc.homepage,
                auth: p.doc.auth,
                builtin,
                source,
            })
            .collect();
        (infos, problems)
    }

    pub fn credentials(&self) -> &Credentials {
        &self.creds
    }

    fn catalog_path(&self, name: &str) -> PathBuf {
        self.dir.join("catalog").join(format!("{name}.json"))
    }

    /// The programs last synced from a platform, if any.
    pub fn catalog(&self, name: &str) -> Option<Catalog> {
        check_pack_name(name).ok()?;
        serde_json::from_slice(&std::fs::read(self.catalog_path(name)).ok()?).ok()
    }

    pub fn save_catalog(&self, c: &Catalog) -> Result<()> {
        check_pack_name(&c.platform).map_err(|e| anyhow!(e))?;
        let path = self.catalog_path(&c.platform);
        std::fs::create_dir_all(path.parent().unwrap())?;
        write_private(&path, &serde_json::to_vec(c)?)
    }

    /// Drops a platform's synced programs. Private programs are in there, so
    /// they go when the token goes.
    pub fn forget_catalog(&self, name: &str) {
        if check_pack_name(name).is_ok() {
            let _ = std::fs::remove_file(self.catalog_path(name));
        }
    }
}

// ---- syncing every program ---------------------------------------------------

/// Programs fetched at once while syncing.
const SYNC_WORKERS: usize = 3;
/// Shortest gap between two requests to a platform, across all workers.
const MIN_REQUEST_GAP: Duration = Duration::from_millis(150);

/// Every program the user can work on at a platform, with its assets and
/// rules, kept on this computer so it can be searched without asking again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub platform: String,
    pub synced_at: i64,
    pub programs: Vec<CatalogEntry>,
    /// Programs that could not be read this time, with why.
    #[serde(default)]
    pub failed: Vec<SyncFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    /// The platform's word for whether it takes reports now, such as open or paused.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    pub program: Program,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncFailure {
    pub handle: String,
    pub name: String,
    pub error: String,
}

/// Where a sync is, for the Programs screen.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SyncStatus {
    pub running: bool,
    pub done: usize,
    pub total: usize,
    pub started_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether the error was the platform refusing the token.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub refused: bool,
}

static SYNCS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, SyncStatus>>> = std::sync::LazyLock::new(Default::default);

fn sync_key(home: &Home, name: &str) -> String {
    format!("{}\n{name}", home.root.display())
}

pub fn sync_status(home: &Home, name: &str) -> SyncStatus {
    SYNCS.lock().unwrap().get(&sync_key(home, name)).cloned().unwrap_or_default()
}

/// Starts syncing every program from a platform in the background, unless a
/// sync is already running. The result lands in the platform's catalog.
pub fn start_sync(home: &Home, name: &str) -> Result<SyncStatus, FetchError> {
    let lib = PlatformLibrary::new(home);
    let pack = lib.get(name).ok_or_else(|| FetchError::Other(format!("no platform `{}`; add it from the Market", clean(name, 60))))?;
    let cred = lib.credentials().get(name).ok_or_else(|| FetchError::NotConnected(pack.doc.title.clone()))?;
    let key = sync_key(home, name);
    {
        let mut all = SYNCS.lock().unwrap();
        let st = all.entry(key.clone()).or_default();
        if st.running {
            return Ok(st.clone());
        }
        *st = SyncStatus { running: true, started_at: now_ms(), ..Default::default() };
    }
    let owned = home.clone();
    std::thread::spawn(move || {
        let set = |f: &dyn Fn(&mut SyncStatus)| f(SYNCS.lock().unwrap().entry(key.clone()).or_default());
        let out = Client::new(&pack, cred).map_err(|e| FetchError::Other(e.to_string())).and_then(|c| c.sync(&|done, total| set(&|s| (s.done, s.total) = (done, total))));
        let out = out.and_then(|c| PlatformLibrary::new(&owned).save_catalog(&c).map_err(|e| FetchError::Other(format!("saving the programs: {e:#}"))));
        set(&|s| {
            s.running = false;
            if let Err(e) = &out {
                s.error = Some(e.to_string());
                s.refused = matches!(e, FetchError::Unauthorized(..));
            }
        });
    });
    Ok(sync_status(home, name))
}

// ---- credentials -----------------------------------------------------------

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Credential {
    #[serde(default)]
    pub user: String,
    pub secret: String,
}

#[cfg(test)]
impl Credential {
    fn default_for_test() -> Self {
        Credential { user: "u".into(), secret: "s".into() }
    }
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential").field("user", &self.user).field("secret", &"…").finish()
    }
}

/// Where platform tokens are kept: the macOS Keychain for the standard Plonix
/// home, a private file otherwise (other systems, test homes).
pub struct Credentials {
    keychain: bool,
    file: PathBuf,
}

const KEYCHAIN_ACCOUNT: &str = "plonix";

impl Credentials {
    pub fn new(home: &Home) -> Self {
        let standard = std::env::var_os("HOME").map(PathBuf::from).map(|h| h.join(".plonix")) == Some(home.root.clone());
        Self { keychain: cfg!(target_os = "macos") && standard && std::env::var_os("PLONIX_NO_KEYCHAIN").is_none(), file: home.root.join("platforms").join("credentials.json") }
    }

    pub fn file(dir: &Path) -> Self {
        Self { keychain: false, file: dir.join("credentials.json") }
    }

    fn service(name: &str) -> String {
        format!("Plonix platform {name}")
    }

    fn read_file(&self) -> BTreeMap<String, Credential> {
        std::fs::read(&self.file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn get(&self, name: &str) -> Option<Credential> {
        if self.keychain {
            let out = std::process::Command::new("/usr/bin/security").args(["find-generic-password", "-s", &Self::service(name), "-a", KEYCHAIN_ACCOUNT, "-w"]).output().ok()?;
            if !out.status.success() {
                return None;
            }
            let b64 = String::from_utf8(out.stdout).ok()?;
            let raw = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
            return serde_json::from_slice(&raw).ok();
        }
        self.read_file().remove(name)
    }

    pub fn set(&self, name: &str, cred: &Credential) -> Result<()> {
        check_pack_name(name).map_err(|e| anyhow!(e))?;
        if cred.secret.is_empty() || cred.secret.len() > 4096 || cred.secret.chars().any(|c| c.is_control()) || cred.user.len() > 200 || cred.user.chars().any(|c| c.is_control()) {
            bail!("the token must be one line of at most 4096 characters");
        }
        if self.keychain {
            // The token goes to `security` on standard input, never on its command line.
            let b64 = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(cred)?);
            let script = format!("add-generic-password -U -s \"{}\" -a {KEYCHAIN_ACCOUNT} -w {b64}\n", Self::service(name));
            let mut child = std::process::Command::new("/usr/bin/security")
                .arg("-i")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .context("could not run the Keychain tool")?;
            use std::io::Write;
            child.stdin.take().context("no stdin")?.write_all(script.as_bytes())?;
            let out = child.wait_with_output()?;
            if !out.status.success() || self.get(name).is_none() {
                bail!("could not save the token in the Keychain: {}", clean(String::from_utf8_lossy(&out.stderr).trim(), 200));
            }
            return Ok(());
        }
        let mut all = self.read_file();
        all.insert(name.to_string(), cred.clone());
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_private(&self.file, &serde_json::to_vec_pretty(&all)?)
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        if self.keychain {
            let st = std::process::Command::new("/usr/bin/security").args(["delete-generic-password", "-s", &Self::service(name), "-a", KEYCHAIN_ACCOUNT]).output()?;
            return Ok(st.status.success());
        }
        let mut all = self.read_file();
        let had = all.remove(name).is_some();
        if had {
            write_private(&self.file, &serde_json::to_vec_pretty(&all)?)?;
        }
        Ok(had)
    }
}

// ---- reading a platform's API ---------------------------------------------

/// A program in a platform's list, before its assets are fetched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProgramSummary {
    pub handle: String,
    pub name: String,
    pub bounty: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub state: String,
    pub url: String,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("{0} is not connected: add your token first")]
    NotConnected(String),
    #[error("{0} did not accept the token (HTTP {1}). Check it and connect again.")]
    Unauthorized(String, u16),
    #[error("{0}")]
    Other(String),
}

pub struct Client<'a> {
    doc: &'a PlatformDoc,
    cred: Credential,
    agent: ureq::Agent,
    /// When the next request may go out.
    next: std::sync::Mutex<std::time::Instant>,
}

impl<'a> Client<'a> {
    pub fn new(pack: &'a PlatformPack, cred: Credential) -> Result<Self> {
        let mut b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout(Duration::from_secs(60)).redirects(0).user_agent(&format!("Plonix/{}", env!("CARGO_PKG_VERSION")));
        if let Some(p) = std::env::var("HTTPS_PROXY").ok().or_else(|| std::env::var("https_proxy").ok()).filter(|p| !p.is_empty())
            && !pack.doc.api.starts_with("http://")
        {
            b = b.proxy(ureq::Proxy::new(&p).context("invalid HTTPS_PROXY")?);
        }
        Ok(Self { doc: &pack.doc, cred, agent: b.build(), next: std::sync::Mutex::new(std::time::Instant::now()) })
    }

    /// Fetches one JSON answer. Refuses any address outside the pack's API origin.
    fn get(&self, url: &str) -> Result<Value, FetchError> {
        let origin = &self.doc.api;
        if !(url.starts_with(origin.as_str()) && matches!(url.as_bytes().get(origin.len()), Some(b'/') | Some(b'?'))) {
            return Err(FetchError::Other(format!("{} pointed outside its API ({}); stopped", self.doc.title, clean(url, 80))));
        }
        let auth = match self.doc.auth.kind {
            AuthKind::Basic => format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", self.cred.user, self.cred.secret))),
            AuthKind::Bearer => format!("Bearer {}", self.cred.secret),
        };
        let mut tries = 0;
        let resp = loop {
            self.pace();
            tries += 1;
            match self.agent.get(url).set("Accept", "application/json").set("Authorization", &auth).call() {
                // Too many requests: wait as long as the platform asks (within reason) and try again.
                Err(ureq::Error::Status(429, r)) if tries < 4 => {
                    let wait = r.header("Retry-After").and_then(|v| v.trim().parse::<u64>().ok()).unwrap_or(10).clamp(1, 60);
                    std::thread::sleep(Duration::from_secs(wait));
                }
                other => break other,
            }
        };
        let resp = match resp {
            Ok(r) => r,
            Err(ureq::Error::Status(code @ (401 | 403), _)) => return Err(FetchError::Unauthorized(self.doc.title.clone(), code)),
            Err(ureq::Error::Status(code, _)) => return Err(FetchError::Other(format!("{} answered HTTP {code} for {}", self.doc.title, clean(&url[origin.len()..], 80)))),
            Err(ureq::Error::Transport(t)) => return Err(FetchError::Other(format!("could not reach {} ({t})", self.doc.title))),
        };
        let mut body = vec![];
        resp.into_reader().take(MAX_RESPONSE_BYTES + 1).read_to_end(&mut body).map_err(|e| FetchError::Other(format!("reading {}: {e}", self.doc.title)))?;
        if body.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(FetchError::Other(format!("{} sent more than {MAX_RESPONSE_BYTES} bytes; stopped", self.doc.title)));
        }
        serde_json::from_slice(&body).map_err(|_| FetchError::Other(format!("{} did not answer with JSON", self.doc.title)))
    }

    /// Keeps requests from all workers at least MIN_REQUEST_GAP apart.
    fn pace(&self) {
        let wait = {
            let mut next = self.next.lock().unwrap();
            let now = std::time::Instant::now();
            let at = (*next).max(now);
            *next = at + MIN_REQUEST_GAP;
            at - now
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    /// Fetches every program with its assets and rules. `progress` hears
    /// (done, total) as programs come in. A refused token stops the sync;
    /// other failures are noted per program and the rest carry on.
    pub fn sync(&self, progress: &(dyn Fn(usize, usize) + Sync)) -> Result<Catalog, FetchError> {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let list = self.programs()?;
        let total = list.len();
        progress(0, total);
        let next = AtomicUsize::new(0);
        let done = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let results: std::sync::Mutex<Vec<Option<Result<Program, FetchError>>>> = std::sync::Mutex::new((0..total).map(|_| None).collect());
        std::thread::scope(|s| {
            for _ in 0..SYNC_WORKERS.min(total.max(1)) {
                s.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, Ordering::SeqCst);
                        if i >= total || stop.load(Ordering::SeqCst) {
                            break;
                        }
                        let r = self.program(&list[i]);
                        if matches!(r, Err(FetchError::Unauthorized(..))) {
                            stop.store(true, Ordering::SeqCst);
                        }
                        results.lock().unwrap()[i] = Some(r);
                        progress(done.fetch_add(1, Ordering::SeqCst) + 1, total);
                    }
                });
            }
        });
        let mut catalog = Catalog { platform: self.doc.name.clone(), synced_at: now_ms(), programs: vec![], failed: vec![] };
        for (summary, r) in list.iter().zip(results.into_inner().unwrap()) {
            match r {
                Some(Ok(program)) => catalog.programs.push(CatalogEntry { state: summary.state.clone(), program }),
                Some(Err(e @ FetchError::Unauthorized(..))) => return Err(e),
                Some(Err(e)) => catalog.failed.push(SyncFailure { handle: summary.handle.clone(), name: summary.name.clone(), error: e.to_string() }),
                None => {}
            }
        }
        Ok(catalog)
    }

    /// Follows a paged list, returning every item.
    fn list<F>(&self, def: &ListDef<F>, handle: &str) -> Result<Vec<Value>, FetchError> {
        let mut url = format!("{}{}", self.doc.api, def.path.replace("{handle}", &encode(handle)));
        let mut items = vec![];
        for _ in 0..def.max_pages {
            let page = self.get(&url)?;
            match page.pointer(&def.items) {
                Some(Value::Array(a)) => items.extend(a.iter().cloned()),
                _ => return Err(FetchError::Other(format!("{} answered in a shape this platform pack does not expect", self.doc.title))),
            }
            match (!def.next.is_empty()).then(|| page.pointer(&def.next)).flatten() {
                Some(Value::String(next)) if !next.is_empty() => {
                    url = if next.starts_with('/') { format!("{}{next}", self.doc.api) } else { next.clone() };
                }
                _ => break,
            }
        }
        Ok(items)
    }

    pub fn programs(&self) -> Result<Vec<ProgramSummary>, FetchError> {
        let f = &self.doc.programs.fields;
        let mut out: Vec<ProgramSummary> = self
            .list(&self.doc.programs, "")?
            .iter()
            .filter_map(|it| {
                let handle = text(it, &f.handle)?;
                Some(ProgramSummary {
                    url: self.doc.program_url.replace("{handle}", &encode(&handle)),
                    name: text(it, &f.name).unwrap_or_else(|| handle.clone()),
                    bounty: flag(it, &f.bounty),
                    state: text(it, &f.state).unwrap_or_default(),
                    handle,
                })
            })
            .collect();
        out.sort_by_key(|p| p.name.to_lowercase());
        Ok(out)
    }

    /// A program known only by its handle (and the name the list showed).
    pub fn summary(&self, handle: &str, name: &str, bounty: bool) -> ProgramSummary {
        ProgramSummary {
            handle: handle.to_string(),
            name: if name.trim().is_empty() { handle.to_string() } else { clean(name.trim(), 200) },
            bounty,
            state: String::new(),
            url: self.doc.program_url.replace("{handle}", &encode(handle)),
        }
    }

    /// Fetches one program's assets and policy, and reads its rules from the policy.
    pub fn program(&self, summary: &ProgramSummary) -> Result<Program, FetchError> {
        let s = &self.doc.scopes.fields;
        let assets: Vec<Asset> = self
            .list(&self.doc.scopes, &summary.handle)?
            .iter()
            .filter_map(|it| {
                let identifier = text(it, &s.identifier)?;
                let raw_kind = text(it, &s.kind).unwrap_or_default();
                let kind = AssetKind::parse(self.doc.asset_kinds.get(&raw_kind).map(String::as_str).unwrap_or("other"));
                let kind = if kind == AssetKind::Web && identifier.trim_start().starts_with("*.") { AssetKind::Wildcard } else { kind };
                Some(Asset {
                    identifier: clean(&identifier, 500),
                    kind,
                    in_scope: if s.in_scope.is_empty() { true } else { flag(it, &s.in_scope) },
                    bounty: flag(it, &s.bounty),
                    instruction: text(it, &s.instruction).map(|t| clean(&t, 2000)).unwrap_or_default(),
                    max_severity: text(it, &s.max_severity).unwrap_or_default(),
                })
            })
            .take(crate::bounty::MAX_ASSETS)
            .collect();
        let policy = match &self.doc.program {
            Some(d) if !d.policy.is_empty() => {
                let page = self.get(&format!("{}{}", self.doc.api, d.path.replace("{handle}", &encode(&summary.handle))))?;
                text(&page, &d.policy).unwrap_or_default()
            }
            _ => String::new(),
        };
        let mut rules: Rules = crate::bounty::extract_rules(&policy);
        rules.not_accepted = crate::bounty::parse_policy(&policy).rules.not_accepted;
        rules.no_intrusive = true;
        crate::bounty::fill_placeholders(&mut rules, &self.cred.user);
        let id: String = summary.handle.chars().filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c)).take(100).collect();
        Ok(Program {
            id: if id.is_empty() { crate::bounty::slug(&summary.name) } else { id },
            name: clean(&summary.name, 200),
            platform: self.doc.name.clone(),
            url: summary.url.clone(),
            bounty: summary.bounty,
            assets,
            rules,
            synced_at: now_ms(),
        })
    }
}

fn text(v: &Value, ptr: &str) -> Option<String> {
    if ptr.is_empty() {
        return None;
    }
    match v.pointer(ptr)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn flag(v: &Value, ptr: &str) -> bool {
    !ptr.is_empty() && matches!(v.pointer(ptr), Some(Value::Bool(true)))
}

/// Percent-encodes a path segment.
fn encode(s: &str) -> String {
    s.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    #[test]
    fn builtin_platforms_are_valid() {
        let lib = PlatformLibrary::at(Path::new("/nonexistent"));
        let (packs, problems) = lib.load();
        assert!(problems.is_empty(), "{problems:?}");
        assert!(packs.iter().any(|(p, builtin, _)| p.doc.name == "hackerone" && *builtin));
    }

    #[test]
    fn rejects_packs_that_could_reach_elsewhere() {
        let good = BUILTIN[0].1;
        for (from, to, want) in [
            ("\"https://api.hackerone.com\"", "\"http://api.hackerone.com\"", "api:"),
            ("\"https://api.hackerone.com\"", "\"https://api.hackerone.com/v1\"", "api:"),
            ("\"/v1/hackers/programs/{handle}/structured_scopes", "\"//evil.example/x", "scopes.path"),
            ("\"plonix_platform\": 1", "\"plonix_platform\": 1, \"run\": \"x\"", "unknown field"),
        ] {
            assert!(good.contains(from), "{from}");
            let err = parse(good.replacen(from, to, 1).as_bytes()).unwrap_err();
            assert!(err.contains(want), "{want} not in {err}");
        }
    }

    #[test]
    fn credentials_in_a_private_file() {
        let dir = tempfile::tempdir().unwrap();
        let c = Credentials::file(dir.path());
        assert!(c.get("hackerone").is_none());
        c.set("hackerone", &Credential { user: "neo".into(), secret: "s3cret".into() }).unwrap();
        assert_eq!(c.get("hackerone").unwrap().secret, "s3cret");
        assert!(c.set("hackerone", &Credential { user: "neo".into(), secret: "a\nb".into() }).is_err());
        assert!(!format!("{:?}", c.get("hackerone").unwrap()).contains("s3cret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.path().join("credentials.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(c.remove("hackerone").unwrap());
        assert!(c.get("hackerone").is_none());
    }

    /// Serves canned JSON for a few paths and records each path with its Authorization header.
    fn serve(pages: impl FnOnce(&str) -> Vec<(String, String)>) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let pages = pages(&addr);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let seen2 = seen.clone();
        std::thread::spawn(move || {
            for stream in l.incoming() {
                let mut s = stream.unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
                let mut auth = String::new();
                loop {
                    let mut h = String::new();
                    if r.read_line(&mut h).unwrap() == 0 || h.trim().is_empty() {
                        break;
                    }
                    if h.to_ascii_lowercase().starts_with("authorization:") {
                        auth = h.trim().to_string();
                    }
                }
                seen2.lock().unwrap().push(format!("{path} {auth}"));
                let resp = match pages.iter().find(|(p, _)| *p == path) {
                    Some((_, b)) => format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}", b.len()),
                    None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                };
                let _ = s.write_all(resp.as_bytes());
            }
        });
        (addr, seen)
    }

    fn pack_at(addr: &str) -> PlatformPack {
        parse(BUILTIN[0].1.replace("https://api.hackerone.com", addr).as_bytes()).unwrap()
    }

    #[test]
    fn reads_programs_and_scopes_through_a_pack() {
        let (addr, seen) = serve(|addr| {
            vec![
                ("/v1/hackers/programs?page%5Bsize%5D=100".into(), format!(r#"{{"data":[{{"attributes":{{"handle":"acme","name":"Acme Cloud","offers_bounties":true,"submission_state":"open"}}}}],"links":{{"next":"{addr}/v1/hackers/programs?page%5Bnumber%5D=2"}}}}"#)),
                ("/v1/hackers/programs?page%5Bnumber%5D=2".into(), r#"{"data":[{"attributes":{"handle":"beta","name":"Beta VDP","offers_bounties":false}}],"links":{}}"#.into()),
                (
                    "/v1/hackers/programs/acme/structured_scopes?page%5Bsize%5D=100".into(),
                    r#"{"data":[
                        {"attributes":{"asset_identifier":"*.acme.example","asset_type":"WILDCARD","eligible_for_submission":true,"eligible_for_bounty":true,"max_severity":"critical"}},
                        {"attributes":{"asset_identifier":"status.acme.example","asset_type":"URL","eligible_for_submission":false,"eligible_for_bounty":false,"instruction":"Third party"}},
                        {"attributes":{"asset_identifier":"com.acme.app","asset_type":"GOOGLE_PLAY_APP_ID","eligible_for_submission":true,"eligible_for_bounty":true}}
                    ],"links":{}}"#
                        .into(),
                ),
                ("/v1/hackers/programs/acme".into(), r#"{"id":"1","type":"program","attributes":{"policy":"Keep it to 3 requests per second. Add X-Bug-Bounty: <your username> to requests. Automated scanners are not allowed."}}"#.into()),
            ]
        });
        let pack = pack_at(&addr);
        let client = Client::new(&pack, Credential { user: "neo".into(), secret: "tok".into() }).unwrap();
        let list = client.programs().unwrap();
        assert_eq!(list.iter().map(|p| p.handle.as_str()).collect::<Vec<_>>(), vec!["acme", "beta"]);
        assert!(list[0].bounty && !list[1].bounty);
        assert_eq!(list[0].url, "https://hackerone.com/acme");

        let p = client.program(&list[0]).unwrap();
        assert_eq!(p.platform, "hackerone");
        assert_eq!(p.assets.len(), 3);
        assert_eq!(p.assets[0].kind, AssetKind::Wildcard);
        assert!(!p.assets[1].in_scope);
        assert_eq!(p.assets[2].kind, AssetKind::Mobile);
        assert_eq!(p.rules.rate_per_second, Some(3.0));
        assert!(p.rules.no_automation && p.rules.no_intrusive);
        assert_eq!(p.rules.headers[0].value, "neo", "the placeholder takes the platform user name");
        let rules = p.scope_rules(0);
        assert_eq!(rules.len(), 2);

        let basic = format!("Authorization: Basic {}", base64::engine::general_purpose::STANDARD.encode("neo:tok"));
        assert!(seen.lock().unwrap().iter().all(|l| l.ends_with(&basic)), "{:?}", seen.lock().unwrap());
    }

    #[test]
    fn never_follows_a_link_outside_the_api() {
        let (addr, seen) = serve(|_| vec![("/v1/hackers/programs?page%5Bsize%5D=100".into(), r#"{"data":[],"links":{"next":"https://elsewhere.example/steal"}}"#.into())]);
        let pack = pack_at(&addr);
        let client = Client::new(&pack, Credential { user: "neo".into(), secret: "tok".into() }).unwrap();
        let err = client.programs().unwrap_err();
        assert!(err.to_string().contains("outside its API"), "{err}");
        assert_eq!(seen.lock().unwrap().len(), 1);

        let (addr, _) = serve(|_| vec![]);
        let pack = pack_at(&addr);
        let client = Client::new(&pack, Credential::default_for_test()).unwrap();
        assert!(matches!(client.programs(), Err(FetchError::Other(m)) if m.contains("HTTP 404")));
    }

    #[test]
    fn syncs_every_program_and_keeps_going_past_one_that_fails() {
        let (addr, _) = serve(|_| {
            vec![
                ("/v1/hackers/programs?page%5Bsize%5D=100".into(), r#"{"data":[{"attributes":{"handle":"acme","name":"Acme","offers_bounties":true,"submission_state":"open"}},{"attributes":{"handle":"gone","name":"Gone","submission_state":"paused"}}],"links":{}}"#.into()),
                ("/v1/hackers/programs/acme/structured_scopes?page%5Bsize%5D=100".into(), r#"{"data":[{"attributes":{"asset_identifier":"app.acme.example","asset_type":"URL","eligible_for_submission":true}}],"links":{}}"#.into()),
                ("/v1/hackers/programs/acme".into(), r#"{"attributes":{"policy":"At most 2 requests per second."}}"#.into()),
            ]
        });
        let pack = pack_at(&addr);
        let client = Client::new(&pack, Credential { user: "neo".into(), secret: "tok".into() }).unwrap();
        let seen = std::sync::Mutex::new(vec![]);
        let c = client.sync(&|d, t| seen.lock().unwrap().push((d, t))).unwrap();
        assert_eq!(c.programs.len(), 1);
        assert_eq!(c.programs[0].state, "open");
        assert_eq!(c.programs[0].program.rules.rate_per_second, Some(2.0));
        assert_eq!(c.failed.len(), 1);
        assert_eq!(c.failed[0].handle, "gone");
        assert_eq!(seen.lock().unwrap().last(), Some(&(2, 2)));

        let dir = tempfile::tempdir().unwrap();
        let lib = PlatformLibrary::at(dir.path());
        assert!(lib.catalog("hackerone").is_none());
        lib.save_catalog(&c).unwrap();
        assert_eq!(lib.catalog("hackerone").unwrap().programs[0].program.name, "Acme");
        assert!(lib.catalog("../hackerone").is_none());
        lib.forget_catalog("hackerone");
        assert!(lib.catalog("hackerone").is_none());
    }
}
