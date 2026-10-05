//! Extensions: the manifest, the capability model, packages and the
//! installed library (see `docs/extensions.md`).
//!
//! An extension with code is a WebAssembly module run in the sandbox in
//! [`crate::sandbox`]. It is distributed as one package file
//! ([`parse_package`]): the manifest and the module, so a single SHA-256
//! pins both. Installed packages live in `$PLONIX_HOME/extensions`, pinned
//! the same way as rule packs and re-verified every time they are loaded.
//!
//! The capability list is deliberately missing things. There is no
//! capability for network access, file system access, changing scope, or
//! sending requests to hosts outside accepted scope, and the manifest parser
//! rejects anything not on the list. A capability that does not exist cannot
//! be granted by mistake.
//!
//! A `program` extension has no code of its own: it names one of the
//! scanner programs Plonix knows how to drive ([`crate::program`]), which
//! the user installs separately. Running it needs the sensitive
//! `run-program` capability, so the user says yes to that at install.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

use crate::detect::{check_text, clean};
use crate::paths::{Home, write_private};
use crate::rulepack::{check_pack_name, check_version, sha256_hex};
use crate::sandbox;
use crate::shelf::Shelf;

pub const FORMAT_VERSION: u32 = 1;
/// The package format ([`parse_package`]).
pub const PACKAGE_VERSION: u32 = 1;
/// Largest package file: the module in Base64 plus the manifest.
pub const MAX_PACKAGE_BYTES: usize = 6 * 1024 * 1024;
/// The file name of a manifest in an extension's source folder.
pub const MANIFEST_FILE: &str = "plonix-extension.json";
/// The file name ending of a package.
pub const PACKAGE_SUFFIX: &str = ".plonixext";
const MAX_INSTALLED: usize = 100;

/// What the runtime in this version can do. A manifest asking for anything
/// else is listed but not installable yet.
pub const RUNTIME_CAPABILITIES: &[Capability] = &[Capability::ReadTraffic, Capability::ReadOutOfScope, Capability::PassiveAnalysis, Capability::ProposeFindings];
/// What a `program` extension may ask for.
pub const PROGRAM_CAPABILITIES: &[Capability] = &[Capability::ReadTraffic, Capability::ReadOutOfScope, Capability::PassiveAnalysis, Capability::RunProgram];

/// What an extension may do. Every capability is mediated by the engine:
/// the extension never gets a raw socket, file handle or database handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Read captured exchanges, delivered by the engine. In-scope hosts only
    /// unless the user also grants `read-out-of-scope`.
    ReadTraffic,
    /// Also see traffic for hosts that are not accepted into scope.
    ReadOutOfScope,
    /// Read scope rules and suggestions (never change them).
    ReadScope,
    /// Contribute detection rules (same validation as rule packs).
    DetectionRules,
    /// Contribute named Traffic filters (same validation as filter packs).
    NamedFilters,
    /// Contribute panels to existing screens (the Traffic inspector, a
    /// host in the Map), drawn with Plonix's own components from a view
    /// tree the extension returns. No HTML, scripts or styles.
    UiPanels,
    /// Contribute one tab of its own to the sidebar, drawn the same way.
    UiTab,
    /// Return passive observations (tags, notes) for exchanges it was given.
    PassiveAnalysis,
    /// Propose findings. They are recorded as created by the extension and
    /// marked unconfirmed until a person confirms them.
    ProposeFindings,
    /// Ask the engine to send requests. The engine applies scope enforcement
    /// to every request exactly as it does for the CLI and agents, so this
    /// can only ever reach accepted hosts, and each request is recorded.
    ScopedRequests,
    /// Run the program a `program` extension names, installed by the user,
    /// over copies of captured requests and responses, on this computer.
    RunProgram,
}

impl Capability {
    pub fn describe(self) -> &'static str {
        match self {
            Capability::ReadTraffic => "read captured traffic for in-scope hosts",
            Capability::ReadOutOfScope => "read captured traffic for hosts outside scope",
            Capability::ReadScope => "read scope rules and suggestions",
            Capability::DetectionRules => "add technology detection rules",
            Capability::NamedFilters => "add named Traffic filters",
            Capability::UiPanels => "show panels inside Plonix screens, with Plonix's own components",
            Capability::UiTab => "add a sidebar tab, with Plonix's own components",
            Capability::PassiveAnalysis => "annotate traffic it was given",
            Capability::ProposeFindings => "propose findings (unconfirmed until you confirm)",
            Capability::ScopedRequests => "send requests to accepted hosts only (scope-enforced, recorded)",
            Capability::RunProgram => "run a program you installed on this Mac over copies of captured requests and responses",
        }
    }

    /// Capabilities that need an explicit "yes" at install time rather than
    /// being granted with the rest.
    pub fn sensitive(self) -> bool {
        matches!(self, Capability::ReadOutOfScope | Capability::ScopedRequests | Capability::RunProgram)
    }
}

/// How the extension runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// Declarative only: ships rule packs, no code.
    Declarative,
    /// A WebAssembly component run in the engine's sandbox (planned).
    Wasm,
    /// Runs a scanner program the user installed (see [`crate::program`]).
    Program,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub plonix_extension: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    pub runtime: Runtime,
    /// For `wasm`: the module file inside the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// Rule pack files inside the package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rule_packs: Vec<String>,
    /// Filter pack files inside the package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter_packs: Vec<String>,
    /// For `program`: which program, by its id in [`crate::program::PROGRAMS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    pub capabilities: Vec<Capability>,
}

pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    if bytes.len() > 64 * 1024 {
        return Err("manifest is larger than 64 KiB".into());
    }
    let m: Manifest = serde_json::from_slice(bytes).map_err(|e| format!("invalid extension manifest: {}", clean(&e.to_string(), 300)))?;
    check_manifest(m)
}

fn check_manifest(m: Manifest) -> Result<Manifest, String> {
    if m.plonix_extension != FORMAT_VERSION {
        return Err(format!("plonix_extension: format {} is not supported", m.plonix_extension));
    }
    check_pack_name(&m.name).map_err(|e| format!("name: {e}"))?;
    check_version(&m.version).map_err(|e| format!("version: {e}"))?;
    check_text(&m.description, 300, false).map_err(|e| format!("description: {e}"))?;
    check_text(&m.author, 100, false).map_err(|e| format!("author: {e}"))?;
    check_text(&m.homepage, 200, true).map_err(|e| format!("homepage: {e}"))?;
    for f in m.entry.iter().chain(&m.rule_packs).chain(&m.filter_packs) {
        check_package_path(f)?;
    }
    if m.runtime != Runtime::Program && m.program.is_some() {
        return Err("program: only a program extension names a program".into());
    }
    if m.runtime != Runtime::Program && m.capabilities.contains(&Capability::RunProgram) {
        return Err("capabilities: run-program is for program extensions".into());
    }
    match m.runtime {
        Runtime::Declarative => {
            if m.entry.is_some() {
                return Err("entry: a declarative extension has no code entry point".into());
            }
            if m.capabilities.iter().any(|c| !matches!(c, Capability::DetectionRules | Capability::NamedFilters)) {
                return Err("capabilities: a declarative extension can only ask for detection-rules and named-filters".into());
            }
        }
        Runtime::Wasm => {
            if !m.entry.as_deref().is_some_and(|e| e.ends_with(".wasm")) {
                return Err("entry: a wasm extension needs an entry ending in .wasm".into());
            }
        }
        Runtime::Program => {
            let Some(p) = m.program.as_deref() else { return Err("program: a program extension names the program it runs".into()) };
            if crate::program::get(p).is_none() {
                let known: Vec<&str> = crate::program::PROGRAMS.iter().map(|p| p.id).collect();
                return Err(format!("program: `{}` is not one Plonix can run ({})", clean(p, 40), known.join(", ")));
            }
            if m.entry.is_some() || !m.rule_packs.is_empty() || !m.filter_packs.is_empty() {
                return Err("a program extension has no files of its own".into());
            }
            if !m.capabilities.contains(&Capability::RunProgram) {
                return Err("capabilities: a program extension asks for run-program".into());
            }
        }
    }
    let mut caps = m.capabilities.clone();
    caps.sort();
    caps.dedup();
    if caps.len() != m.capabilities.len() {
        return Err("capabilities: listed twice".into());
    }
    if caps.contains(&Capability::ReadOutOfScope) && !caps.contains(&Capability::ReadTraffic) {
        return Err("capabilities: read-out-of-scope needs read-traffic".into());
    }
    Ok(m)
}

/// Paths inside a package: relative, no `..`, no hidden tricks.
fn check_package_path(p: &str) -> Result<(), String> {
    let ok = !p.is_empty()
        && p.len() <= 200
        && !p.starts_with('/')
        && p.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
    if ok { Ok(()) } else { Err(format!("`{}` must be a relative path inside the package", clean(p, 100))) }
}

impl Capability {
    /// The id as written in a manifest: `read-traffic`.
    pub fn id(self) -> String {
        serde_json::to_value(self).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        serde_json::from_value(serde_json::Value::String(s.trim().to_ascii_lowercase())).map_err(|_| format!("unknown capability `{}`", clean(s, 40)))
    }
}

/// Whether this version of Plonix can run the extension: a WebAssembly
/// analyzer that reads traffic and asks only for what the runtime has.
pub fn installable(m: &Manifest) -> Result<(), String> {
    if m.runtime == Runtime::Program {
        let missing: Vec<String> = m.capabilities.iter().filter(|c| !PROGRAM_CAPABILITIES.contains(c)).map(|c| c.id()).collect();
        if !missing.is_empty() {
            return Err(format!("{} asks for {}, which a program extension cannot have", m.name, missing.join(", ")));
        }
        if !m.capabilities.contains(&Capability::ReadTraffic) || !m.capabilities.contains(&Capability::PassiveAnalysis) {
            return Err(format!("{} must ask for read-traffic and passive-analysis", m.name));
        }
        return Ok(());
    }
    if m.runtime == Runtime::Declarative {
        return Err(format!(
            "{} is a declarative extension; install its rule and filter packs with `plonix rules add` and `plonix filters add` for now",
            m.name
        ));
    }
    let missing: Vec<String> = m.capabilities.iter().filter(|c| !RUNTIME_CAPABILITIES.contains(c)).map(|c| c.id()).collect();
    if !missing.is_empty() {
        return Err(format!("{} needs {}, which this version of Plonix cannot run yet", m.name, missing.join(", ")));
    }
    if !m.capabilities.contains(&Capability::ReadTraffic) || !m.capabilities.iter().any(|c| matches!(c, Capability::PassiveAnalysis | Capability::ProposeFindings)) {
        return Err(format!(
            "{} must ask for read-traffic and passive-analysis or propose-findings: analyzers are the only kind of extension this version runs",
            m.name
        ));
    }
    if !m.rule_packs.is_empty() || !m.filter_packs.is_empty() {
        return Err(format!("{}: rule and filter packs inside a code extension are not supported yet", m.name));
    }
    Ok(())
}

// ---- packages -------------------------------------------------------------------

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageDoc {
    plonix_extension_package: u32,
    manifest: Manifest,
    /// Files by their path in the package, in Base64: exactly the entry
    /// module, or none for a program extension.
    #[serde(default)]
    files: BTreeMap<String, String>,
}

/// A parsed extension package: its manifest and module, both checked.
#[derive(Clone)]
pub struct Package {
    pub manifest: Manifest,
    pub module: Vec<u8>,
    /// SHA-256 of the whole package file.
    pub sha256: String,
}

impl std::fmt::Debug for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Package").field("name", &self.manifest.name).field("version", &self.manifest.version).field("sha256", &self.sha256).finish()
    }
}

/// Parses a package file and checks everything in it short of running it:
/// the manifest, that this Plonix can run it, and the module against the
/// host API and the capabilities it asks for.
pub fn parse_package(bytes: &[u8]) -> Result<Package, String> {
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(format!("the package is larger than {} MiB", MAX_PACKAGE_BYTES / 1024 / 1024));
    }
    let doc: PackageDoc = serde_json::from_slice(bytes).map_err(|e| format!("invalid extension package: {}", clean(&e.to_string(), 300)))?;
    if doc.plonix_extension_package != PACKAGE_VERSION {
        return Err(format!("plonix_extension_package: format {} is not supported", doc.plonix_extension_package));
    }
    let manifest = check_manifest(doc.manifest)?;
    installable(&manifest)?;
    if manifest.runtime == Runtime::Program {
        if !doc.files.is_empty() {
            return Err("files: a program extension carries no files".into());
        }
        return Ok(Package { manifest, module: vec![], sha256: sha256_hex(bytes) });
    }
    let entry = manifest.entry.clone().unwrap_or_default();
    let Some(encoded) = doc.files.get(&entry).filter(|_| doc.files.len() == 1) else {
        return Err(format!("files: the package must hold exactly its entry, {}", clean(&entry, 100)));
    };
    let module = B64.decode(encoded.as_bytes()).map_err(|_| format!("files: {} is not valid Base64", clean(&entry, 100)))?;
    sandbox::compile(&module, &manifest.capabilities).map_err(|e| format!("{}: {e}", manifest.name))?;
    Ok(Package { manifest, module, sha256: sha256_hex(bytes) })
}

/// Builds a package file from a manifest and its module.
pub fn pack(manifest_bytes: &[u8], module: &[u8]) -> Result<Vec<u8>, String> {
    let manifest = parse_manifest(manifest_bytes)?;
    let entry = manifest.entry.clone().ok_or("the manifest has no entry module")?;
    let doc = PackageDoc { plonix_extension_package: PACKAGE_VERSION, manifest, files: BTreeMap::from([(entry, B64.encode(module))]) };
    let mut bytes = serde_json::to_vec_pretty(&doc).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    parse_package(&bytes)?;
    Ok(bytes)
}

/// An extension from a local path: a package file as it is, or a source
/// folder (or its `plonix-extension.json`) packed with the module it names.
pub fn read_source(path: &Path) -> Result<Vec<u8>> {
    let file = if path.is_dir() { path.join(MANIFEST_FILE) } else { path.to_path_buf() };
    let meta = std::fs::metadata(&file).map_err(|e| anyhow!("reading {}: {e}", file.display()))?;
    if meta.len() > MAX_PACKAGE_BYTES as u64 {
        bail!("{} is larger than {} MiB", file.display(), MAX_PACKAGE_BYTES / 1024 / 1024);
    }
    let bytes = std::fs::read(&file).map_err(|e| anyhow!("reading {}: {e}", file.display()))?;
    let is_manifest = serde_json::from_slice::<serde_json::Value>(&bytes).ok().is_some_and(|v| v.get("plonix_extension").is_some());
    if !is_manifest {
        return Ok(bytes);
    }
    let m = parse_manifest(&bytes).map_err(|e| anyhow!("{}: {e}", file.display()))?;
    let entry = m.entry.as_deref().ok_or_else(|| anyhow!("{}: the manifest has no entry module", m.name))?;
    let module_path = file.parent().unwrap_or(Path::new(".")).join(entry);
    let module = std::fs::read(&module_path).map_err(|e| anyhow!("reading {}: {e}", module_path.display()))?;
    pack(&bytes, &module).map_err(|e| anyhow!(e))
}

// ---- the installed library -------------------------------------------------------

/// What the user decided at install time.
#[derive(Debug, Clone, Default)]
pub struct Consent {
    /// Sensitive capabilities the user said yes to.
    pub grant: Vec<Capability>,
    /// The user saw what an update asks for beyond what it had, and agreed.
    pub approve_new: bool,
}

/// An extension's runtime state, kept next to the pinned packages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtState {
    /// What the user granted, a subset of what the manifest asks for.
    pub granted: Vec<Capability>,
    pub enabled: bool,
    /// Why Plonix turned it off, shown until the user turns it back on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_at: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StateFile {
    extensions: BTreeMap<String, ExtState>,
}

/// An installed extension, for listings.
#[derive(Debug, Clone, Serialize)]
pub struct Info {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub homepage: String,
    pub source: String,
    pub sha256: String,
    pub requested: Vec<Capability>,
    #[serde(flatten)]
    pub state: ExtState,
    /// The file on disk is still the one that was verified.
    pub intact: bool,
    /// For a program extension: the program and whether it is installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<ProgramStatus>,
}

/// The program a program extension runs, and whether this computer has it.
#[derive(Debug, Clone, Serialize)]
pub struct ProgramStatus {
    pub id: String,
    pub found: bool,
    /// How to install it.
    pub install: String,
    pub homepage: String,
}

impl ProgramStatus {
    pub fn of(m: &Manifest) -> Option<Self> {
        let p = crate::program::get(m.program.as_deref()?)?;
        Some(Self { id: p.id.into(), found: crate::program::locate(p.id).is_some(), install: p.install.into(), homepage: p.homepage.into() })
    }
}

/// An enabled extension, verified and compiled, ready to run.
#[derive(Clone)]
pub struct Loaded {
    pub name: String,
    pub version: String,
    pub granted: Vec<Capability>,
    pub runner: Runner,
}

/// How a loaded extension runs.
#[derive(Clone)]
pub enum Runner {
    Wasm(sandbox::Compiled),
    /// The id of the program, in [`crate::program::PROGRAMS`].
    Program(String),
}

#[derive(Default, Clone)]
pub struct LoadedSet {
    pub extensions: Vec<Loaded>,
    pub problems: Vec<String>,
}

/// Installed extensions: pinned packages (see [`Shelf`]) and `state.json`.
pub struct ExtensionLibrary {
    dir: PathBuf,
    shelf: Shelf,
}

/// A value that changes whenever installed extensions or their state change.
pub type Stamp = (Option<std::time::SystemTime>, Option<std::time::SystemTime>);

impl ExtensionLibrary {
    pub fn new(home: &Home) -> Self {
        Self::at(&home.root.join("extensions"))
    }

    pub fn at(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf(), shelf: Shelf::new(dir, "extension", "extensions", MAX_INSTALLED) }
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    fn read_state(&self) -> StateFile {
        std::fs::read(self.state_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn write_state(&self, s: &StateFile) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("state.json.tmp");
        write_private(&tmp, &serde_json::to_vec_pretty(s)?)?;
        std::fs::rename(&tmp, self.state_path())?;
        Ok(())
    }

    pub fn stamp(&self) -> Stamp {
        (self.shelf.stamp(), std::fs::metadata(self.state_path()).and_then(|m| m.modified()).ok())
    }

    /// Installs a package. Capabilities that are not sensitive are granted
    /// with the install; sensitive ones only when `consent` says yes. An
    /// update asking for capabilities it did not have needs `approve_new`.
    pub fn install(&self, bytes: &[u8], source: &str, expected_sha256: Option<&str>, consent: &Consent) -> Result<(Package, Option<String>)> {
        Shelf::check_sha(bytes, expected_sha256, "extension")?;
        let pkg = parse_package(bytes).map_err(|e| anyhow!(e))?;
        let name = pkg.manifest.name.clone();
        let mut state = self.read_state();
        let old = state.extensions.get(&name).filter(|_| self.shelf.installed_version(&name).is_some()).cloned();
        if let Some(old) = &old {
            let new: Vec<String> = pkg.manifest.capabilities.iter().filter(|c| !old.granted.contains(c) && !c.sensitive()).map(|c| c.id()).collect();
            if !new.is_empty() && !consent.approve_new {
                bail!(
                    "{name} {} asks for more than the installed version: {}. Nothing changed. Approve it from its page in the Market, \
                     or with `plonix extensions add <file> --yes`",
                    pkg.manifest.version,
                    new.join(", ")
                );
            }
        }
        let keep = |c: &Capability| !c.sensitive() || consent.grant.contains(c) || old.as_ref().is_some_and(|o| o.granted.contains(c));
        let granted: Vec<Capability> = pkg.manifest.capabilities.iter().copied().filter(keep).collect();
        let previous = self.shelf.put(&name, &pkg.manifest.version, bytes, source)?;
        state.extensions.insert(name, ExtState { granted, enabled: true, disabled_reason: None, disabled_at: None });
        self.write_state(&state)?;
        Ok((pkg, previous))
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        let existed = self.shelf.remove(name)?;
        let mut state = self.read_state();
        if state.extensions.remove(name).is_some() {
            self.write_state(&state)?;
        }
        Ok(existed)
    }

    /// Turns an extension on or off. Turning it on clears the reason Plonix
    /// turned it off.
    pub fn set_enabled(&self, name: &str, on: bool) -> Result<ExtState> {
        if self.shelf.installed_version(name).is_none() {
            bail!("no extension named `{}` is installed (see `plonix extensions`)", clean(name, 64));
        }
        let mut state = self.read_state();
        let s = state.extensions.entry(name.to_string()).or_default();
        s.enabled = on;
        if on {
            s.disabled_reason = None;
            s.disabled_at = None;
        }
        let out = s.clone();
        self.write_state(&state)?;
        Ok(out)
    }

    /// Turns an extension off because it misbehaved, saying why.
    pub fn disable(&self, name: &str, reason: &str) -> Result<()> {
        let mut state = self.read_state();
        let Some(s) = state.extensions.get_mut(name) else { return Ok(()) };
        s.enabled = false;
        s.disabled_reason = Some(clean(reason, 300));
        s.disabled_at = Some(crate::model::now_ms());
        self.write_state(&state)
    }

    pub fn installed_version(&self, name: &str) -> Option<String> {
        self.shelf.installed_version(name)
    }

    pub fn installed(&self) -> Vec<crate::shelf::Installed> {
        self.shelf.installed()
    }

    /// Every installed extension with its state.
    pub fn list(&self) -> Vec<Info> {
        let state = self.read_state();
        let (verified, _) = self.shelf.verified();
        self.shelf
            .installed()
            .into_iter()
            .map(|i| {
                let manifest = verified.iter().find(|v| v.name == i.name).and_then(|v| parse_package(&v.bytes).ok()).map(|p| p.manifest);
                Info {
                    version: i.entry.version.clone(),
                    description: manifest.as_ref().map(|m| m.description.clone()).unwrap_or_else(|| "Not loaded: the file changed since it was installed.".into()),
                    author: manifest.as_ref().map(|m| m.author.clone()).unwrap_or_else(|| "unknown".into()),
                    homepage: manifest.as_ref().map(|m| m.homepage.clone()).unwrap_or_default(),
                    program: manifest.as_ref().and_then(ProgramStatus::of),
                    requested: manifest.map(|m| m.capabilities).unwrap_or_default(),
                    state: state.extensions.get(&i.name).cloned().unwrap_or_default(),
                    source: i.entry.source.clone(),
                    sha256: i.entry.sha256.clone(),
                    intact: i.intact,
                    name: i.name,
                }
            })
            .collect()
    }

    pub fn info(&self, name: &str) -> Option<Info> {
        self.list().into_iter().find(|i| i.name == name)
    }

    /// Enabled extensions whose files still match what was verified at
    /// install, compiled with only what the user granted linked in.
    pub fn load(&self) -> LoadedSet {
        let state = self.read_state();
        let (verified, mut problems) = self.shelf.verified();
        let mut out = vec![];
        for v in verified {
            let Some(st) = state.extensions.get(&v.name).filter(|s| s.enabled) else { continue };
            let loaded = parse_package(&v.bytes).and_then(|p| {
                let granted: Vec<Capability> = p.manifest.capabilities.iter().copied().filter(|c| st.granted.contains(c)).collect();
                let runner = match p.manifest.program {
                    Some(program) => Runner::Program(program),
                    None => Runner::Wasm(sandbox::compile(&p.module, &granted)?),
                };
                Ok(Loaded { name: v.name.clone(), version: p.manifest.version, granted, runner })
            });
            match loaded {
                Ok(l) => out.push(l),
                Err(e) => problems.push(format!("extension {}: {e}", v.name)),
            }
        }
        LoadedSet { extensions: out, problems }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(runtime: &str, caps: &str, extra: &str) -> String {
        format!(
            r#"{{"plonix_extension":1,"name":"jwt-tools","version":"0.1.0","description":"d","author":"a",
                "runtime":"{runtime}","capabilities":[{caps}]{extra}}}"#
        )
    }

    #[test]
    fn capabilities_are_a_closed_list() {
        for forbidden in ["\"network\"", "\"filesystem\"", "\"exec\"", "\"modify-scope\"", "\"send-anywhere\""] {
            let m = manifest("wasm", forbidden, r#","entry":"x.wasm""#);
            assert!(parse_manifest(m.as_bytes()).unwrap_err().contains("unknown variant"), "{forbidden} was accepted");
        }
        let ok = manifest("wasm", r#""read-traffic","propose-findings""#, r#","entry":"plugin.wasm""#);
        let m = parse_manifest(ok.as_bytes()).unwrap();
        assert!(installable(&m).is_ok());
        let tab = manifest("wasm", r#""read-traffic","ui-tab""#, r#","entry":"plugin.wasm""#);
        assert!(installable(&parse_manifest(tab.as_bytes()).unwrap()).unwrap_err().contains("ui-tab"), "the runtime has no UI yet");
    }

    #[test]
    fn declarative_extensions_cannot_ask_for_more() {
        let m = manifest("declarative", r#""read-traffic""#, "");
        assert!(parse_manifest(m.as_bytes()).is_err());
        let m = manifest("declarative", r#""ui-tab""#, "");
        assert!(parse_manifest(m.as_bytes()).is_err(), "UI needs code, so the sandbox");
        let m = manifest("declarative", r#""detection-rules","named-filters""#, r#","rule_packs":["rules/a.json"],"filter_packs":["filters/a.json"]"#);
        assert!(parse_manifest(m.as_bytes()).is_ok());
    }

    #[test]
    fn program_extensions_name_a_known_program() {
        let caps = r#""read-traffic","passive-analysis","run-program""#;
        let m = parse_manifest(manifest("program", caps, r#","program":"trufflehog""#).as_bytes()).unwrap();
        assert!(installable(&m).is_ok());
        assert!(Capability::RunProgram.sensitive());
        assert!(parse_manifest(manifest("program", caps, r#","program":"sh""#).as_bytes()).unwrap_err().contains("not one Plonix can run"));
        assert!(parse_manifest(manifest("program", caps, "").as_bytes()).is_err(), "it must name one");
        let no_run = manifest("program", r#""read-traffic","passive-analysis""#, r#","program":"trufflehog""#);
        assert!(parse_manifest(no_run.as_bytes()).is_err(), "running it needs the capability");
        let wasm = manifest("wasm", r#""read-traffic","run-program""#, r#","entry":"x.wasm""#);
        assert!(parse_manifest(wasm.as_bytes()).is_err(), "wasm code cannot run programs");
        let more = manifest("program", r#""read-traffic","passive-analysis","run-program","scoped-requests""#, r#","program":"trufflehog""#);
        assert!(installable(&parse_manifest(more.as_bytes()).unwrap()).is_err());
        let pkg = format!(r#"{{"plonix_extension_package":1,"manifest":{}}}"#, manifest("program", caps, r#","program":"trufflehog""#));
        let p = parse_package(pkg.as_bytes()).unwrap();
        assert!(p.module.is_empty());
    }

    #[test]
    fn package_paths_stay_inside() {
        for bad in ["../x.wasm", "/etc/x.wasm", "a/../../x.wasm", "a//x.wasm"] {
            let m = manifest("wasm", r#""read-traffic""#, &format!(r#","entry":"{bad}""#));
            assert!(parse_manifest(m.as_bytes()).is_err(), "{bad}");
        }
    }
}
