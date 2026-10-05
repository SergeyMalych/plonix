//! The Market: browse, install, update and remove everything modular in
//! Plonix (skills, rule packs, filter packs, bundles and extensions) from
//! one signed catalog.
//!
//! The catalog is a [`registry::Index`]. It is only used when its signature
//! verifies against a trusted key ([`registry::verify`]), and every package
//! is checked against the SHA-256 the index lists before it is installed.
//! Skills are text and packs are data. Extensions with code are WebAssembly
//! analyzers that only ever run in the sandbox ([`crate::sandbox`]), with the
//! capabilities the user granted; an extension this version cannot run yet
//! is listed but not installable.
//!
//! A copy of the Plonix Market index and its packages is compiled into
//! Plonix ([`SNAPSHOT`]). When the online index cannot be reached, the
//! Market shows that copy instead, so it works offline.
//!
//! Installed state lives where each kind already keeps it (`rules/`,
//! `filters/`, `skills/` in `$PLONIX_HOME`). Bundles are recorded in
//! `market/bundles.json` with the packages they added, so removing a bundle
//! removes what it brought in and nothing else.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::detect::clean;
use crate::extension::{self, Capability, Consent, ExtensionLibrary};
use crate::filterpack::{self, FilterLibrary};
use crate::listpack::{self, ListLibrary};
use crate::paths::{Home, write_private};
use crate::registry::{self, Index, Kind, Location, OFFICIAL_PUBLISHER, Package, TrustedKey};
use crate::rulepack::{self, Library, MAX_PACK_BYTES, newer, sha256_hex};
use crate::settings::{self, Field, Level, Section};
use crate::skill::{self, SkillLibrary};

/// The Plonix Market index and every file it points to, as shipped with
/// this build.
pub const SNAPSHOT: &[(&str, &str)] = &[
    ("index.json", include_str!("../../../store/index.json")),
    ("index.json.sig", include_str!("../../../store/index.json.sig")),
    ("packs/web-servers.json", include_str!("../../../store/packs/web-servers.json")),
    ("packs/frameworks.json", include_str!("../../../store/packs/frameworks.json")),
    ("packs/cms.json", include_str!("../../../store/packs/cms.json")),
    ("packs/edge.json", include_str!("../../../store/packs/edge.json")),
    ("packs/api-surface.json", include_str!("../../../store/packs/api-surface.json")),
    ("packs/admin-panels.json", include_str!("../../../store/packs/admin-panels.json")),
    ("filterpacks/common.json", include_str!("../../../store/filterpacks/common.json")),
    ("filterpacks/leaks.json", include_str!("../../../store/filterpacks/leaks.json")),
    ("lists/starter-lists.json", include_str!("../../../store/lists/starter-lists.json")),
    ("lists/extra-wordlists.json", include_str!("../../../store/lists/extra-wordlists.json")),
    ("skills/triage-host.md", include_str!("../../../store/skills/triage-host.md")),
    ("skills/explain-request.md", include_str!("../../../store/skills/explain-request.md")),
    ("skills/review-sign-in.md", include_str!("../../../store/skills/review-sign-in.md")),
    ("skills/check-scope.md", include_str!("../../../store/skills/check-scope.md")),
    ("skills/draft-finding.md", include_str!("../../../store/skills/draft-finding.md")),
    ("skills/api-inventory.md", include_str!("../../../store/skills/api-inventory.md")),
    ("extensions/graphql-explorer.json", include_str!("../../../store/extensions/graphql-explorer.json")),
    ("extensions/security-headers.plonixext", include_str!("../../../store/extensions/security-headers.plonixext")),
    ("extensions/secret-sweep.plonixext", include_str!("../../../store/extensions/secret-sweep.plonixext")),
    ("extensions/js-endpoints.plonixext", include_str!("../../../store/extensions/js-endpoints.plonixext")),
    ("extensions/subdomain-discovery.plonixext", include_str!("../../../store/extensions/subdomain-discovery.plonixext")),
    ("extensions/parameter-probe.plonixext", include_str!("../../../store/extensions/parameter-probe.plonixext")),
];

// ---- settings -------------------------------------------------------------------

pub const SETTINGS: &str = "market";

pub fn settings_section() -> Section {
    Section::new(SETTINGS, "Market", Level::Global)
        .describe("Where Plonix finds skills, rules, filters, bundles and extensions. Applies to all projects.")
        .order(40)
        .field(crate::profile::global_field())
        .field(
            Field::text("index", "Market address", "")
                .placeholder(registry::DEFAULT_INDEX)
                .help("Leave empty for the Plonix Market. A company can host its own: an https:// address or a folder path to an index.json."),
        )
        .field(
            Field::list("trusted_keys", "Also trust these publishers", &[])
                .placeholder("ed25519:…")
                .help("Public keys of other Markets you trust, one per line. A Market is only used when a trusted key signed it."),
        )
        .field(
            Field::toggle("allow_unsigned", "Allow Markets that are not signed", false)
                .help("Off by default. When on, a Market list without a trusted signature can be used, and everything from it is marked Not verified."),
        )
        .validator(|v| {
            let mut p = vec![];
            let index = v.get("index").and_then(Value::as_str).unwrap_or("").trim();
            if !index.is_empty()
                && let Err(e) = registry::location(index)
            {
                p.push(settings::Problem::new("index", e));
            }
            for k in v.get("trusted_keys").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
                if let Err(e) = registry::parse_public_key(k) {
                    p.push(settings::Problem::new("trusted_keys", format!("{}: {e}", clean(k, 40))));
                }
            }
            p
        })
}

/// The Market settings in effect.
#[derive(Debug, Clone, Default)]
pub struct MarketSettings {
    pub index: String,
    pub trusted_keys: Vec<String>,
    pub allow_unsigned: bool,
}

impl MarketSettings {
    pub fn load(home: &Home) -> Self {
        let v = settings::global(home, SETTINGS);
        Self {
            index: v.get("index").and_then(Value::as_str).unwrap_or("").trim().to_string(),
            allow_unsigned: v.get("allow_unsigned").and_then(Value::as_bool).unwrap_or(false),
            trusted_keys: v
                .get("trusted_keys")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
        }
    }
}

// ---- the catalog ------------------------------------------------------------------

/// Where a catalog came from.
#[derive(Debug, Clone)]
pub enum Origin {
    /// The copy compiled into this Plonix.
    Bundled,
    Remote(Location),
}

/// How far a catalog can be trusted.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Trust {
    /// Signed by a trusted key.
    Verified { key: String, publisher: String },
    /// No signature, accepted only because the user asked (`--allow-unsigned`).
    Unverified,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub origin: Origin,
    pub index: Index,
    pub trust: Trust,
    /// Why the online index was not used, when this is the bundled copy.
    pub offline_reason: Option<String>,
}

impl Catalog {
    pub fn location(&self) -> String {
        match &self.origin {
            Origin::Bundled => "built into Plonix".into(),
            Origin::Remote(l) => l.to_string(),
        }
    }

    pub fn verified(&self) -> bool {
        matches!(self.trust, Trust::Verified { .. })
    }

    /// Downloads a package and checks it against the index: checksum,
    /// format, name and version. Nothing is installed.
    pub fn fetch(&self, p: &Package) -> Result<Vec<u8>> {
        let bytes = match &self.origin {
            Origin::Bundled => SNAPSHOT
                .iter()
                .find(|(path, _)| *path == p.url)
                .map(|(_, text)| text.as_bytes().to_vec())
                .ok_or_else(|| anyhow!("{}: not in the copy built into Plonix", p.name))?,
            Origin::Remote(base) => {
                let src = registry::resolve(base, &p.url).map_err(|e| anyhow!("{}: {e}", p.name))?;
                registry::fetch(&src, max_bytes(p.kind)).with_context(|| format!("downloading {}", p.name))?
            }
        };
        let actual = sha256_hex(&bytes);
        if actual != p.sha256 {
            bail!("{}: checksum mismatch: the Market lists sha256 {}, the download is {actual}. Nothing was installed.", p.name, p.sha256);
        }
        let (name, version) = describe(p.kind, &bytes).map_err(|e| anyhow!("{}: {e}", p.name))?;
        if name != p.name || version != p.version {
            bail!("{}: the Market lists {} {} but the file is {name} {version}", p.name, p.name, p.version);
        }
        Ok(bytes)
    }
}

/// The largest file of a kind the Market downloads.
pub fn max_bytes(kind: Kind) -> usize {
    match kind {
        Kind::Extension => extension::MAX_PACKAGE_BYTES,
        _ => MAX_PACK_BYTES.max(skill::MAX_SKILL_BYTES),
    }
}

/// The name and version inside a package file, after full validation.
pub fn describe(kind: Kind, bytes: &[u8]) -> Result<(String, String), String> {
    match kind {
        Kind::Rules => rulepack::parse(bytes).map(|x| (x.doc.name, x.doc.version)).map_err(|e| e.to_string()),
        Kind::Filters => filterpack::parse(bytes).map(|x| (x.doc.name, x.doc.version)),
        Kind::List => listpack::parse(bytes).map(|x| (x.doc.name, x.doc.version)),
        Kind::Skill => skill::parse(bytes).map(|x| (x.name, x.version)),
        // A package with code, or a bare manifest: an extension listed before its code is published.
        Kind::Extension => match extension::parse_package(bytes) {
            Ok(p) => Ok((p.manifest.name, p.manifest.version)),
            Err(e) if is_package(bytes) => Err(e),
            Err(_) => extension::parse_manifest(bytes).map(|m| (m.name, m.version)),
        },
        Kind::Bundle => Err("a bundle has no file".into()),
    }
}

fn is_package(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes).ok().is_some_and(|v| v.get("plonix_extension_package").is_some())
}

/// What an extension in the Market asks for, read from its file: the
/// manifest of a package or of a listing.
pub fn extension_manifest(bytes: &[u8]) -> Result<extension::Manifest, String> {
    if is_package(bytes) { extension::parse_package(bytes).map(|p| p.manifest) } else { extension::parse_manifest(bytes) }
}

/// Whether an extension in the Market can be installed by this Plonix, and
/// why not in plain words.
pub fn extension_runnable(bytes: &[u8]) -> Result<(), String> {
    if is_package(bytes) {
        return extension::parse_package(bytes).map(|_| ());
    }
    let m = extension::parse_manifest(bytes)?;
    extension::installable(&m)?;
    Err(format!("the Market lists {} so you can see what is coming; its code is not published yet", m.name))
}

#[derive(Debug, Clone, Default)]
pub struct OpenOptions {
    /// Use this index instead of the one in Settings.
    pub index: Option<String>,
    /// Accept an index without a signature (CLI only, for Market authors).
    pub allow_unsigned: bool,
}

/// Which index to read: the option, then Settings, then the environment,
/// then the Plonix Market.
pub fn index_address(home: &Home, opts: &OpenOptions) -> String {
    opts.index
        .clone()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| Some(MarketSettings::load(home).index).filter(|s| !s.is_empty()))
        .or_else(|| std::env::var("PLONIX_MARKET_INDEX").ok().filter(|s| !s.trim().is_empty()))
        .or_else(|| std::env::var("PLONIX_STORE_INDEX").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| registry::DEFAULT_INDEX.to_string())
}

/// Opens the catalog: fetches the index and its signature and verifies them.
///
/// The Plonix Market itself falls back to the copy built into this Plonix
/// when the online index cannot be fetched or verified; the copy is signed
/// by the same key, so falling back never lowers the bar.
pub fn open(home: &Home, opts: &OpenOptions) -> Result<Catalog> {
    let address = index_address(home, opts);
    let trusted = registry::trusted_keys(&MarketSettings::load(home).trusted_keys);
    let loc = registry::location(&address).map_err(|e| anyhow!(e))?;
    match open_at(&loc, &trusted, opts.allow_unsigned || MarketSettings::load(home).allow_unsigned) {
        Ok(c) => Ok(c),
        Err(e) if address == registry::DEFAULT_INDEX => {
            let mut c = bundled(&trusted)?;
            c.offline_reason = Some(format!("{e:#}"));
            Ok(c)
        }
        Err(e) => Err(e),
    }
}

fn open_at(loc: &Location, trusted: &[TrustedKey], allow_unsigned: bool) -> Result<Catalog> {
    let bytes = registry::fetch(loc, registry::MAX_INDEX_BYTES).with_context(|| format!("fetching the Market index {loc}"))?;
    let index = registry::parse(&bytes).map_err(|e| anyhow!("Market index {loc}: {e}"))?;
    let sig = registry::fetch(&registry::signature_location(loc), registry::MAX_SIGNATURE_BYTES);
    let trust = match sig {
        Ok(sig) => {
            let key = registry::verify(&bytes, &sig, trusted).map_err(|e| anyhow!("Market index {loc} failed verification: {e}"))?;
            Trust::Verified { key: key.key, publisher: key.publisher }
        }
        Err(_) if allow_unsigned => Trust::Unverified,
        Err(e) => bail!(
            "Market index {loc} is not signed ({e:#}). Plonix only installs from a signed Market; \
             Market authors can test an unsigned index with --allow-unsigned."
        ),
    };
    Ok(Catalog { origin: Origin::Remote(loc.clone()), index, trust, offline_reason: None })
}

/// The copy of the Plonix Market built into this Plonix.
pub fn bundled(trusted: &[TrustedKey]) -> Result<Catalog> {
    let file = |name: &str| SNAPSHOT.iter().find(|(p, _)| *p == name).map(|(_, t)| t.as_bytes()).unwrap_or_default();
    let bytes = file("index.json");
    let index = registry::parse(bytes).map_err(|e| anyhow!("built-in Market index: {e}"))?;
    let key = registry::verify(bytes, file("index.json.sig"), trusted).map_err(|e| anyhow!("built-in Market index: {e}"))?;
    Ok(Catalog { origin: Origin::Bundled, index, trust: Trust::Verified { key: key.key, publisher: key.publisher }, offline_reason: None })
}

/// Recently opened catalogs, so the window does not refetch on every click.
static CACHE: Mutex<Option<(Instant, String, Arc<Catalog>)>> = Mutex::new(None);
const CACHE_TTL: Duration = Duration::from_secs(300);

/// [`open`], reusing a catalog fetched in the last few minutes.
pub fn open_cached(home: &Home, refresh: bool) -> Result<Arc<Catalog>> {
    let key = format!("{}|{:?}", index_address(home, &OpenOptions::default()), MarketSettings::load(home).trusted_keys);
    if !refresh
        && let Some((at, k, c)) = CACHE.lock().unwrap().as_ref()
        && *k == key
        && at.elapsed() < CACHE_TTL
    {
        return Ok(c.clone());
    }
    let c = Arc::new(open(home, &OpenOptions::default())?);
    *CACHE.lock().unwrap() = Some((Instant::now(), key, c.clone()));
    Ok(c)
}

// ---- installed state -------------------------------------------------------------

/// What an installed package looks like next to the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Compiled into Plonix: always on.
    BuiltIn,
    Available,
    Installed { version: String },
    Update { installed: String },
    /// An extension listed before its code is published, or one this
    /// version cannot run: listed, not installable.
    NeedsRuntime,
}

/// How far to trust a package, shown next to every package.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// Ships inside Plonix.
    BuiltIn,
    /// From a signed Market, unchanged since it was checked.
    Verified,
    /// Nobody vouches for it: from an unsigned Market, or added by hand.
    Unverified,
    /// Installed from the Market, but the file on disk is no longer the one
    /// that was checked.
    Changed,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verification {
    pub level: TrustLevel,
    /// Short, for a badge: "Verified by Plonix maintainers".
    pub label: String,
    /// One or two sentences for the details.
    pub detail: String,
}

impl Verification {
    fn built_in() -> Self {
        Self { level: TrustLevel::BuiltIn, label: "Built in".into(), detail: "Ships inside Plonix, reviewed with the app itself.".into() }
    }

    fn signed(publisher: &str, has_file: bool) -> Self {
        let checked = if has_file {
            "The file is pinned by SHA-256 in the signed Market list, and was checked in full before it installed."
        } else {
            "Listed in the signed Market list; the packages it installs are each checked the same way."
        };
        let who = if publisher == OFFICIAL_PUBLISHER {
            "Reviewed by the Plonix maintainers, who signed the Market list."
        } else {
            "Signed by a publisher whose key you chose to trust."
        };
        Self { level: TrustLevel::Verified, label: format!("Verified by {publisher}"), detail: format!("{who} {checked}") }
    }

    fn unsigned_catalog() -> Self {
        Self {
            level: TrustLevel::Unverified,
            label: "Not verified".into(),
            detail: "This Market list is not signed, so nobody vouches for it. The file still matches the list and is validated, but read what it does before you install it.".into(),
        }
    }

    fn by_hand(kind: Kind, source: &str) -> Self {
        let still = if kind == Kind::Extension {
            "It is still checked, and its code only runs in the sandbox with the capabilities you granted."
        } else {
            "It is still validated and cannot run code."
        };
        Self {
            level: TrustLevel::Unverified,
            label: "Not verified".into(),
            detail: format!(
                "You added this yourself ({}), not through a signed Market, so nobody has vouched for it. {still}",
                crate::detect::clean(source, 120)
            ),
        }
    }

    fn changed() -> Self {
        Self {
            level: TrustLevel::Changed,
            label: "Changed since install".into(),
            detail: "The file on disk is no longer the one that was checked. Plonix does not load it. Remove it and install it again.".into(),
        }
    }
}

/// One row of the Market.
#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    #[serde(flatten)]
    pub package: Package,
    pub status: Status,
    pub verification: Verification,
    /// Installed without the Market (`plonix rules add` and the like), so it
    /// is not in the catalog.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub local: bool,
    /// Packages this one installs (bundles and requirements), by name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub includes: Vec<String>,
}

/// What an install, update or removal did to one package.
#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub name: String,
    pub kind: Kind,
    pub version: String,
    pub action: Action,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Installed,
    Updated,
    Removed,
    /// Already there: built in, or the same version installed.
    Unchanged,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BundleLock {
    bundles: BTreeMap<String, BundleEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BundleEntry {
    version: String,
    /// Everything the bundle includes.
    members: Vec<String>,
    /// Members that were not installed before the bundle brought them in.
    added: Vec<String>,
}

/// Who vouched for an installed package, recorded when the Market installs it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Provenance {
    /// `None` when the Market list was not signed.
    publisher: Option<String>,
    sha256: String,
    catalog: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ProvenanceLock {
    /// `kind:name` to who vouched for it.
    packages: BTreeMap<String, Provenance>,
}

pub struct Market {
    home: Home,
    pub rules: Library,
    pub filters: FilterLibrary,
    pub lists: ListLibrary,
    pub skills: SkillLibrary,
    pub extensions: ExtensionLibrary,
}

impl Market {
    pub fn new(home: &Home) -> Self {
        Self {
            home: home.clone(),
            rules: Library::new(home),
            filters: FilterLibrary::new(home),
            lists: ListLibrary::new(home),
            skills: SkillLibrary::new(home),
            extensions: ExtensionLibrary::new(home),
        }
    }

    fn bundles_path(&self) -> std::path::PathBuf {
        self.home.root.join("market").join("bundles.json")
    }

    fn read_bundles(&self) -> BundleLock {
        std::fs::read(self.bundles_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn write_bundles(&self, lock: &BundleLock) -> Result<()> {
        std::fs::create_dir_all(self.home.root.join("market"))?;
        write_private(&self.bundles_path(), &serde_json::to_vec_pretty(lock)?)
    }

    fn provenance_path(&self) -> std::path::PathBuf {
        self.home.root.join("market").join("provenance.json")
    }

    fn read_provenance(&self) -> ProvenanceLock {
        std::fs::read(self.provenance_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn write_provenance(&self, lock: &ProvenanceLock) -> Result<()> {
        std::fs::create_dir_all(self.home.root.join("market"))?;
        write_private(&self.provenance_path(), &serde_json::to_vec_pretty(lock)?)
    }

    fn record(&self, cat: &Catalog, changes: &[Change], packages: &[&Package]) -> Result<()> {
        let mut lock = self.read_provenance();
        for c in changes.iter().filter(|c| matches!(c.action, Action::Installed | Action::Updated)) {
            let Some(p) = packages.iter().find(|p| p.name == c.name) else { continue };
            let publisher = match &cat.trust {
                Trust::Verified { publisher, .. } => Some(publisher.clone()),
                Trust::Unverified => None,
            };
            lock.packages.insert(format!("{}:{}", c.kind.as_str(), c.name), Provenance { publisher, sha256: p.sha256.clone(), catalog: cat.location() });
        }
        self.write_provenance(&lock)
    }

    fn forget(&self, changes: &[Change]) -> Result<()> {
        let mut lock = self.read_provenance();
        for c in changes.iter().filter(|c| c.action == Action::Removed) {
            lock.packages.remove(&format!("{}:{}", c.kind.as_str(), c.name));
        }
        self.write_provenance(&lock)
    }

    /// Everything installed through any route, with whether its file is intact.
    fn installed_all(&self) -> Vec<(Kind, crate::shelf::Installed)> {
        let mut v = vec![];
        v.extend(self.rules.installed().into_iter().map(|i| (Kind::Rules, i)));
        v.extend(self.filters.installed().into_iter().map(|i| (Kind::Filters, i)));
        v.extend(self.lists.installed().into_iter().map(|i| (Kind::List, i)));
        v.extend(self.skills.installed().into_iter().map(|i| (Kind::Skill, i)));
        v.extend(self.extensions.installed().into_iter().map(|i| (Kind::Extension, i)));
        v
    }

    /// How far an installed or built-in package can be trusted.
    pub fn verification(&self, kind: Kind, name: &str) -> Verification {
        if Self::builtin(kind, name) {
            return Verification::built_in();
        }
        if kind == Kind::Bundle {
            return match self.read_provenance().packages.get(&format!("bundle:{name}")) {
                Some(Provenance { publisher: Some(p), .. }) => Verification::signed(p, false),
                _ => Verification::unsigned_catalog(),
            };
        }
        let Some((_, item)) = self.installed_all().into_iter().find(|(k, i)| *k == kind && i.name == name) else {
            return Verification::unsigned_catalog();
        };
        if !item.intact {
            return Verification::changed();
        }
        match self.read_provenance().packages.get(&format!("{}:{name}", kind.as_str())) {
            Some(Provenance { publisher: Some(p), sha256, .. }) if *sha256 == item.entry.sha256 => Verification::signed(p, true),
            Some(Provenance { publisher: None, sha256, .. }) if *sha256 == item.entry.sha256 => Verification::unsigned_catalog(),
            _ => Verification::by_hand(kind, &item.entry.source),
        }
    }

    /// What the Market would say about a package before it is installed.
    fn offered(cat: &Catalog, p: &Package) -> Verification {
        match &cat.trust {
            Trust::Verified { publisher, .. } => Verification::signed(publisher, p.kind != Kind::Bundle),
            Trust::Unverified => Verification::unsigned_catalog(),
        }
    }

    }

/// A file someone wants to add from outside the Market, looked at but not installed.
#[derive(Debug, Clone, Serialize)]
pub struct External {
    pub kind: Kind,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub sha256: String,
    pub source: String,
    /// What installing it will do, in plain words.
    pub effects: Vec<String>,
    /// For an extension: what it asks to be allowed to do.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<CapabilityInfo>,
    /// Installed already (so adding replaces it), with its version.
    pub replaces: Option<String>,
    #[serde(skip)]
    bytes: Vec<u8>,
}

/// A capability as people see it before they grant it.
#[derive(Debug, Clone, Serialize)]
pub struct CapabilityInfo {
    pub id: Capability,
    pub what: &'static str,
    /// Needs a separate, explicit yes.
    pub sensitive: bool,
}

pub fn capability_infos(caps: &[Capability]) -> Vec<CapabilityInfo> {
    caps.iter().map(|c| CapabilityInfo { id: *c, what: c.describe(), sensitive: c.sensitive() }).collect()
}

/// What an extension's code can and cannot do, for the consent screen.
pub const SANDBOX_NOTE: &str = "Its code runs in the Plonix sandbox: no network, files, processes or clock, limited CPU time and memory. \
     It is stopped and switched off if it misbehaves.";

pub const PROGRAM_NOTE: &str = "It runs a program you install yourself, on this Mac only, over copies of captured requests and responses that are \
     deleted when it finishes. Plonix runs it so it checks nothing with outside services and does not update itself.";

/// What to tell someone about how an extension runs.
pub fn runtime_note(m: &extension::Manifest) -> &'static str {
    if m.runtime == extension::Runtime::Program { PROGRAM_NOTE } else { SANDBOX_NOTE }
}

/// Which kind of package a file is, from its contents.
pub fn detect_kind(bytes: &[u8]) -> Result<Kind, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "that file is not text".to_string())?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.starts_with("---") {
        return Ok(Kind::Skill);
    }
    let v: Value = serde_json::from_str(text).map_err(|_| "that is not a Plonix skill (Markdown) or pack (JSON)".to_string())?;
    if v.get("plonix_pack").is_some() {
        Ok(Kind::Rules)
    } else if v.get("plonix_filters").is_some() {
        Ok(Kind::Filters)
    } else if v.get("plonix_lists").is_some() {
        Ok(Kind::List)
    } else if v.get("plonix_extension_package").is_some() {
        Ok(Kind::Extension)
    } else if v.get("plonix_extension").is_some() {
        Err("that is an extension's manifest. Add its folder with `plonix extensions add <folder>`, or pack it with `plonix extensions pack`".into())
    } else if v.get("plonix_index").is_some() {
        Err("that is a Market list, not a package. Add it as the Market address in Settings › Market".into())
    } else {
        Err("that is not a Plonix skill or pack".into())
    }
}

impl Market {
    /// Reads and fully validates a file from outside the Market. Nothing is installed.
    pub fn inspect_external(&self, bytes: Vec<u8>, source: &str) -> Result<External, String> {
        if bytes.len() > max_bytes(Kind::Extension) {
            return Err("that file is too large".into());
        }
        let kind = detect_kind(&bytes)?;
        if bytes.len() > max_bytes(kind) {
            return Err("that file is too large".into());
        }
        let (name, version) = describe(kind, &bytes)?;
        let mut capabilities = vec![];
        let (description, author, effects) = match kind {
            Kind::Skill => {
                let sk = skill::parse(&bytes)?;
                let uses: Vec<String> = sk.uses.iter().map(|g| format!("{g:?}").to_lowercase()).collect();
                (
                    sk.description,
                    sk.author,
                    vec![
                        format!("Gives AI agents a playbook that reads: {}. Agents stay read-only.", uses.join(", ")),
                        "A skill is instructions an agent follows, so read them first: a misleading skill can steer the agent.".into(),
                    ],
                )
            }
            Kind::Rules => {
                let p = rulepack::parse(&bytes).map_err(|e| e.to_string())?;
                (p.doc.description.clone(), p.doc.author.clone(), vec![format!("Adds {} technology detection rules. Data only; it cannot run code or send anything.", p.rules.len())])
            }
            Kind::Filters => {
                let p = filterpack::parse(&bytes)?;
                (
                    p.doc.description.clone(),
                    p.doc.author.clone(),
                    vec![format!("Adds {} named Traffic filters. They can only narrow what you see.", p.doc.filters.len())],
                )
            }
            Kind::List => {
                let p = listpack::parse(&bytes)?;
                (
                    p.doc.description.clone(),
                    p.doc.author.clone(),
                    vec![format!("Adds {} payload lists for the Bench. Data only; you choose when to send them.", p.doc.lists.len())],
                )
            }
            Kind::Extension => {
                let p = extension::parse_package(&bytes)?;
                capabilities = capability_infos(&p.manifest.capabilities);
                let mut effects: Vec<String> = p.manifest.capabilities.iter().map(|c| format!("Can {}.", c.describe())).collect();
                effects.push(runtime_note(&p.manifest).into());
                (p.manifest.description, p.manifest.author, effects)
            }
            _ => unreachable!(),
        };
        if Self::builtin(kind, &name) {
            return Err(format!("`{name}` is the name of a built-in {}; give it a different name", kind.noun()));
        }
        let replaces = self.installed_version(kind, &name);
        Ok(External { kind, name, version, description, author, sha256: sha256_hex(&bytes), source: source.to_string(), effects, capabilities, replaces, bytes })
    }

    /// Installs a file from outside the Market. It is recorded as not
    /// verified. The caller has shown what it does, so adding it approves
    /// what an extension asks for; sensitive capabilities still need `consent`.
    pub fn add_external(&self, ext: &External, consent: &Consent) -> Result<Change> {
        let src = ext.source.as_str();
        let previous = match ext.kind {
            Kind::Rules => self.rules.install(&ext.bytes, src, Some(&ext.sha256))?.1,
            Kind::Filters => self.filters.install(&ext.bytes, src, Some(&ext.sha256))?.1,
            Kind::List => self.lists.install(&ext.bytes, src, Some(&ext.sha256))?.1,
            Kind::Skill => self.skills.install(&ext.bytes, src, Some(&ext.sha256))?.1,
            Kind::Extension => self.extensions.install(&ext.bytes, src, Some(&ext.sha256), &Consent { approve_new: true, ..consent.clone() })?.1,
            Kind::Bundle => bail!("a bundle cannot be added from a file"),
        };
        // Whatever the Market vouched for under this name no longer applies.
        let mut lock = self.read_provenance();
        lock.packages.remove(&format!("{}:{}", ext.kind.as_str(), ext.name));
        self.write_provenance(&lock)?;
        Ok(Change {
            name: ext.name.clone(),
            kind: ext.kind,
            version: ext.version.clone(),
            action: if previous.is_some() { Action::Updated } else { Action::Installed },
            from: previous,
        })
    }

    /// Installed packages that are not in the catalog: added by hand.
    fn local_listing(&self, cat: &Catalog) -> Vec<Listing> {
        let rules = self.rules.load();
        let filters = self.filters.load();
        let lists = self.lists.load();
        let skills = self.skills.load();
        let extensions = self.extensions.list();
        let mut out = vec![];
        for (kind, item) in self.installed_all() {
            if cat.index.get(&item.name).is_some() {
                continue;
            }
            let (description, author) = match kind {
                Kind::Rules => rules.packs.iter().find(|(_, i)| i.name == item.name).map(|(_, i)| (i.description.clone(), i.author.clone())),
                Kind::Filters => filters.packs.iter().find(|i| i.name == item.name).map(|i| (i.description.clone(), i.author.clone())),
                Kind::List => lists.packs.iter().find(|i| i.name == item.name).map(|i| (i.description.clone(), i.author.clone())),
                Kind::Skill => skills.get(&item.name).map(|(s, ..)| (s.description.clone(), s.author.clone())),
                Kind::Extension => extensions.iter().find(|e| e.name == item.name && e.intact).map(|e| (e.description.clone(), e.author.clone())),
                _ => None,
            }
            .unwrap_or_else(|| ("Not loaded: the file changed since it was installed.".into(), "unknown".into()));
            out.push(Listing {
                package: Package {
                    name: item.name.clone(),
                    kind,
                    version: item.entry.version.clone(),
                    description,
                    author,
                    url: String::new(),
                    sha256: item.entry.sha256.clone(),
                    homepage: String::new(),
                    about: vec![],
                    requires: vec![],
                },
                status: Status::Installed { version: item.entry.version.clone() },
                verification: self.verification(kind, &item.name),
                includes: vec![],
                local: true,
            });
        }
        out
    }

    fn builtin(kind: Kind, name: &str) -> bool {
        let list = match kind {
            Kind::Rules => rulepack::BUILTIN,
            Kind::Filters => filterpack::BUILTIN,
            Kind::List => listpack::BUILTIN,
            Kind::Skill => skill::BUILTIN,
            Kind::Bundle | Kind::Extension => return false,
        };
        list.iter().any(|(n, _)| *n == name)
    }

    pub fn installed_version(&self, kind: Kind, name: &str) -> Option<String> {
        match kind {
            Kind::Rules => self.rules.installed_version(name),
            Kind::Filters => self.filters.installed_version(name),
            Kind::List => self.lists.installed_version(name),
            Kind::Skill => self.skills.installed_version(name),
            Kind::Bundle => self.read_bundles().bundles.get(name).map(|b| b.version.clone()),
            Kind::Extension => self.extensions.installed_version(name),
        }
    }

    pub fn status(&self, p: &Package) -> Status {
        // Code is published as a package file; anything else is a listing
        // of what is coming.
        if p.kind == Kind::Extension && !p.url.ends_with(extension::PACKAGE_SUFFIX) && self.installed_version(p.kind, &p.name).is_none() {
            return Status::NeedsRuntime;
        }
        if Self::builtin(p.kind, &p.name) {
            return Status::BuiltIn;
        }
        match self.installed_version(p.kind, &p.name) {
            Some(v) if newer(&p.version, &v) => Status::Update { installed: v },
            Some(v) => Status::Installed { version: v },
            None => Status::Available,
        }
    }

    pub fn listing(&self, cat: &Catalog) -> Vec<Listing> {
        let mut rows: Vec<Listing> = cat
            .index
            .packages
            .iter()
            .map(|p| {
                let status = self.status(p);
                let verification = match status {
                    Status::Available | Status::NeedsRuntime => Self::offered(cat, p),
                    // Installed from here, by hand, or changed: what is on disk decides.
                    _ => self.verification(p.kind, &p.name),
                };
                Listing {
                    status,
                    verification,
                    local: false,
                    includes: registry::install_order(&cat.index, &p.name).unwrap_or_default().into_iter().filter(|n| *n != p.name).collect(),
                    package: p.clone(),
                }
            })
            .collect();
        rows.extend(self.local_listing(cat));
        rows
    }

    /// Installs a package and everything it requires, with no sensitive
    /// capabilities granted (see [`Market::install_with`]).
    pub fn install(&self, cat: &Catalog, name: &str) -> Result<Vec<Change>> {
        self.install_with(cat, name, &Consent::default())
    }

    /// Installs a package and everything it requires. Every file is
    /// downloaded and verified before anything is installed. `consent` is
    /// what the user agreed to for extensions.
    pub fn install_with(&self, cat: &Catalog, name: &str, consent: &Consent) -> Result<Vec<Change>> {
        let Some(top) = cat.index.get(name) else {
            bail!("`{}` is not in the Market (see `plonix market list`)", clean(name, 64));
        };
        let order = registry::install_order(&cat.index, name).map_err(|e| anyhow!(e))?;
        let packages: Vec<&Package> = order.iter().filter_map(|n| cat.index.get(n)).collect();
        if let Some(ext) = packages.iter().find(|p| self.status(p) == Status::NeedsRuntime) {
            let why = if ext.name == name { String::new() } else { format!(" ({} requires it)", top.name) };
            bail!(
                "{} is an extension{why} that is listed so you can see what is coming. Its code is not published for this version of Plonix yet.",
                ext.name
            );
        }
        // Download and verify first, so a bad file changes nothing.
        let mut planned = vec![];
        for p in &packages {
            let status = self.status(p);
            let needed = matches!(status, Status::Available | Status::Update { .. }) || (p.name == name && p.kind == Kind::Bundle);
            let bytes = if needed && p.kind != Kind::Bundle { Some(cat.fetch(p)?) } else { None };
            planned.push((*p, status, needed, bytes));
        }
        let source = |p: &Package| match &cat.origin {
            Origin::Bundled => format!("Plonix Market (built in) {}", p.url),
            Origin::Remote(base) => registry::resolve(base, &p.url).map(|l| l.to_string()).unwrap_or_default(),
        };
        let mut changes = vec![];
        for (p, status, needed, bytes) in planned {
            let from = match &status {
                Status::Update { installed } => Some(installed.clone()),
                Status::Installed { version } if p.kind == Kind::Bundle => Some(version.clone()),
                _ => None,
            };
            if !needed {
                changes.push(Change { name: p.name.clone(), kind: p.kind, version: p.version.clone(), action: Action::Unchanged, from: None });
                continue;
            }
            match (p.kind, bytes) {
                (Kind::Rules, Some(b)) => drop(self.rules.install(&b, &source(p), Some(&p.sha256))?),
                (Kind::Filters, Some(b)) => drop(self.filters.install(&b, &source(p), Some(&p.sha256))?),
                (Kind::List, Some(b)) => drop(self.lists.install(&b, &source(p), Some(&p.sha256))?),
                (Kind::Skill, Some(b)) => drop(self.skills.install(&b, &source(p), Some(&p.sha256))?),
                (Kind::Extension, Some(b)) => drop(self.extensions.install(&b, &source(p), Some(&p.sha256), consent)?),
                (Kind::Bundle, _) => {}
                _ => unreachable!("files are fetched for every kind but bundles"),
            }
            let action = if from.is_some() { Action::Updated } else { Action::Installed };
            changes.push(Change { name: p.name.clone(), kind: p.kind, version: p.version.clone(), action, from });
        }
        if top.kind == Kind::Bundle {
            let mut lock = self.read_bundles();
            let mut added: Vec<String> =
                changes.iter().filter(|c| c.action == Action::Installed && c.name != top.name).map(|c| c.name.clone()).collect();
            if let Some(old) = lock.bundles.get(&top.name) {
                for a in &old.added {
                    if !added.contains(a) {
                        added.push(a.clone());
                    }
                }
            }
            let members = order.iter().filter(|n| **n != top.name).cloned().collect();
            lock.bundles.insert(top.name.clone(), BundleEntry { version: top.version.clone(), members, added });
            self.write_bundles(&lock)?;
        }
        self.record(cat, &changes, &packages)?;
        Ok(changes)
    }

    /// Installs newer versions of everything installed from the catalog.
    pub fn update(&self, cat: &Catalog) -> Result<Vec<Change>> {
        let mut changes = vec![];
        for p in &cat.index.packages {
            if matches!(self.status(p), Status::Update { .. }) {
                changes.extend(self.install(cat, &p.name)?.into_iter().filter(|c| c.action != Action::Unchanged));
            }
        }
        Ok(changes)
    }

    /// Removes an installed package. Removing a bundle also removes the
    /// packages it brought in, unless another installed bundle includes them.
    pub fn remove(&self, name: &str) -> Result<Vec<Change>> {
        let mut lock = self.read_bundles();
        if let Some(bundle) = lock.bundles.remove(name) {
            let mut changes = vec![];
            for member in &bundle.added {
                if lock.bundles.values().any(|b| b.members.contains(member)) {
                    continue;
                }
                changes.extend(self.remove_one(member)?);
            }
            self.write_bundles(&lock)?;
            changes.push(Change { name: name.into(), kind: Kind::Bundle, version: bundle.version, action: Action::Removed, from: None });
            self.forget(&changes)?;
            return Ok(changes);
        }
        let changes = self.remove_one(name)?;
        if changes.is_empty() {
            for kind in [Kind::Rules, Kind::Filters, Kind::List, Kind::Skill, Kind::Extension] {
                if Self::builtin(kind, name) {
                    bail!("`{name}` is a built-in {} and cannot be removed", kind.noun());
                }
            }
            bail!("nothing named `{}` is installed", clean(name, 64));
        }
        // It no longer counts as added by a bundle.
        for b in lock.bundles.values_mut() {
            b.added.retain(|a| a != name);
        }
        self.write_bundles(&lock)?;
        self.forget(&changes)?;
        Ok(changes)
    }

    fn remove_one(&self, name: &str) -> Result<Vec<Change>> {
        let mut changes = vec![];
        for kind in [Kind::Rules, Kind::Filters, Kind::List, Kind::Skill, Kind::Extension] {
            let Some(version) = self.installed_version(kind, name) else { continue };
            match kind {
                Kind::Rules => self.rules.remove(name)?,
                Kind::Filters => self.filters.remove(name)?,
                Kind::List => self.lists.remove(name)?,
                Kind::Extension => self.extensions.remove(name)?,
                _ => self.skills.remove(name)?,
            };
            changes.push(Change { name: name.into(), kind, version, action: Action::Removed, from: None });
        }
        Ok(changes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> (tempfile::TempDir, Home) {
        let dir = tempfile::tempdir().unwrap();
        let home = Home { root: dir.path().to_path_buf() };
        (dir, home)
    }

    fn official() -> Catalog {
        bundled(&registry::trusted_keys(&[])).unwrap()
    }

    #[test]
    fn snapshot_holds_every_package_and_verifies() {
        let cat = official();
        assert!(cat.verified());
        for p in &cat.index.packages {
            if p.kind == Kind::Bundle {
                continue;
            }
            assert!(SNAPSHOT.iter().any(|(path, _)| *path == p.url), "{} ({}) is missing from SNAPSHOT", p.name, p.url);
            cat.fetch(p).unwrap_or_else(|e| panic!("{e:#}"));
        }
        for kind in Kind::ALL {
            assert!(cat.index.packages.iter().any(|p| p.kind == *kind), "the Market lists no {kind:?}");
        }
    }

    #[test]
    fn installs_bundles_with_requirements_and_removes_what_they_added() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let cat = official();
        let bundle = cat.index.packages.iter().find(|p| p.kind == Kind::Bundle).unwrap().clone();
        // One member installed by hand first: the bundle must not take it away.
        let first = bundle.requires.iter().filter_map(|n| cat.index.get(n)).find(|p| matches!(market.status(p), Status::Available)).unwrap();
        market.install(&cat, &first.name).unwrap();

        let changes = market.install(&cat, &bundle.name).unwrap();
        assert_eq!(changes.last().unwrap().name, bundle.name);
        assert!(matches!(market.status(&bundle), Status::Installed { .. }));
        for r in &bundle.requires {
            let p = cat.index.get(r).unwrap();
            assert!(matches!(market.status(p), Status::Installed { .. } | Status::BuiltIn), "{r}");
        }
        let again = market.install(&cat, &bundle.name).unwrap();
        assert!(again.iter().filter(|c| c.name != bundle.name).all(|c| c.action == Action::Unchanged));

        let removed = market.remove(&bundle.name).unwrap();
        assert!(!removed.iter().any(|c| c.name == first.name), "kept what was installed before the bundle");
        assert!(matches!(market.status(first), Status::Installed { .. }));
        assert!(matches!(market.status(&bundle), Status::Available));
        for c in removed.iter().filter(|c| c.kind != Kind::Bundle) {
            assert!(matches!(market.status(cat.index.get(&c.name).unwrap()), Status::Available));
        }
        assert!(market.remove("triage-host").unwrap_err().to_string().contains("built-in"));
        assert!(market.remove("nothing-here").is_err());
    }

    /// The official catalog with the example extension listed, as the Market will list it.
    fn with_example(cat: &Catalog) -> Catalog {
        let mut cat = cat.clone();
        let path = "extensions/security-headers.plonixext";
        let bytes = SNAPSHOT.iter().find(|(p, _)| *p == path).unwrap().1.as_bytes();
        let pkg = extension::parse_package(bytes).unwrap();
        if cat.index.get("security-headers").is_none() {
            cat.index.packages.push(Package {
                name: "security-headers".into(),
                kind: Kind::Extension,
                version: pkg.manifest.version.clone(),
                description: pkg.manifest.description.clone(),
                author: pkg.manifest.author.clone(),
                url: path.into(),
                sha256: pkg.sha256.clone(),
                homepage: String::new(),
                about: vec![],
                requires: vec![],
            });
        }
        cat
    }

    #[test]
    fn installs_extensions_from_the_signed_market() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let cat = with_example(&official());
        let p = cat.index.get("security-headers").unwrap().clone();
        assert_eq!(market.status(&p), Status::Available);
        let changes = market.install(&cat, &p.name).unwrap();
        assert_eq!(changes[0].action, Action::Installed);
        assert!(matches!(market.status(&p), Status::Installed { .. }));
        assert_eq!(market.verification(Kind::Extension, &p.name).level, TrustLevel::Verified);
        let info = market.extensions.info(&p.name).unwrap();
        assert!(info.state.enabled && info.state.granted.contains(&Capability::ProposeFindings), "{info:?}");
        assert_eq!(market.extensions.load().extensions.len(), 1);

        // A changed file on disk is not loaded.
        std::fs::write(home.root.join("extensions/packs/security-headers.json"), "{}").unwrap();
        assert_eq!(market.verification(Kind::Extension, &p.name).level, TrustLevel::Changed);
        assert!(market.extensions.load().extensions.is_empty());

        assert_eq!(market.remove(&p.name).unwrap()[0].action, Action::Removed);
        assert_eq!(market.status(&p), Status::Available);

        // A Market listing different bytes than it ships is refused.
        let mut bad = cat.clone();
        bad.index.packages.iter_mut().find(|x| x.name == p.name).unwrap().sha256 = "0".repeat(64);
        assert!(market.install(&bad, &p.name).unwrap_err().to_string().contains("checksum mismatch"));
    }

    #[test]
    fn refuses_extensions_and_tampered_packages() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let cat = official();
        let ext = cat.index.packages.iter().find(|p| p.name == "graphql-explorer").unwrap();
        assert_eq!(market.status(ext), Status::NeedsRuntime);
        assert!(market.install(&cat, &ext.name).unwrap_err().to_string().contains("not published"));

        let mut bad = cat.clone();
        let p = bad.index.packages.iter_mut().find(|p| p.kind == Kind::Skill && !Market::builtin(p.kind, &p.name)).unwrap();
        let name = p.name.clone();
        p.sha256 = "0".repeat(64);
        assert!(market.install(&bad, &name).unwrap_err().to_string().contains("checksum mismatch"));
        assert!(matches!(market.status(bad.index.get(&name).unwrap()), Status::Available));
    }

    #[test]
    fn local_index_needs_a_signature_from_a_trusted_key() {
        let (dir, home) = home();
        let store = dir.path().join("store");
        std::fs::create_dir_all(&store).unwrap();
        let index = store.join("index.json");
        std::fs::write(&index, SNAPSHOT[0].1).unwrap();
        let opts = OpenOptions { index: Some(index.display().to_string()), allow_unsigned: false };
        assert!(open(&home, &opts).unwrap_err().to_string().contains("not signed"));
        assert!(matches!(open(&home, &OpenOptions { allow_unsigned: true, ..opts.clone() }).unwrap().trust, Trust::Unverified));

        // Signed by a key nobody trusts.
        let (private, public) = registry::generate_key().unwrap();
        let sig = registry::sign(SNAPSHOT[0].1.as_bytes(), &private).unwrap();
        std::fs::write(store.join("index.json.sig"), serde_json::to_vec(&sig).unwrap()).unwrap();
        assert!(open(&home, &opts).unwrap_err().to_string().contains("not a key you trust"));

        // Trusted in Settings › Market.
        let mut values = settings::Values::new();
        values.insert("trusted_keys".into(), serde_json::json!([public]));
        settings::register(settings_section());
        settings::save_global(&home, SETTINGS, &values).unwrap();
        assert!(open(&home, &opts).unwrap().verified());

        // Changed after signing.
        std::fs::write(&index, SNAPSHOT[0].1.replace("Plonix contributors", "Someone else")).unwrap();
        assert!(open(&home, &opts).unwrap_err().to_string().contains("changed after it was signed"));
    }

    #[test]
    fn every_package_says_whether_it_is_verified() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let cat = official();
        let level = |name: &str| market.listing(&cat).into_iter().find(|l| l.package.name == name).unwrap().verification.level;
        assert_eq!(level("triage-host"), TrustLevel::BuiltIn);
        assert_eq!(level("api-inventory"), TrustLevel::Verified);
        assert_eq!(level("graphql-explorer"), TrustLevel::Verified);

        // Installed through the signed Market: still verified.
        market.install(&cat, "api-kit").unwrap();
        assert_eq!(level("leaks"), TrustLevel::Verified);
        assert_eq!(level("api-kit"), TrustLevel::Verified);

        // Something added by hand is listed, and not verified.
        let mine = "---\nplonix_skill: 1\nname: mine\nversion: 1.0.0\ntitle: Mine\ndescription: My skill.\nauthor: me\nuses: [traffic]\n---\nLook at traffic.\n";
        market.skills.install(mine.as_bytes(), "/tmp/mine.md", None).unwrap();
        let row = market.listing(&cat).into_iter().find(|l| l.package.name == "mine").expect("hand-installed skill is listed");
        assert!(row.local && row.verification.level == TrustLevel::Unverified, "{row:?}");
        assert!(row.verification.detail.contains("/tmp/mine.md"));

        // Hand-installing a Market package's name does not inherit its trust.
        let swapped = mine.replace("name: mine", "name: api-inventory");
        market.skills.install(swapped.as_bytes(), "/tmp/x.md", None).unwrap();
        assert_eq!(level("api-inventory"), TrustLevel::Unverified);

        // A file edited on disk is called out.
        std::fs::write(home.root.join("filters/packs/leaks.json"), "tampered").unwrap();
        assert_eq!(level("leaks"), TrustLevel::Changed);

        // An unsigned Market never produces a verified package.
        let mut unsigned = cat.clone();
        unsigned.trust = Trust::Unverified;
        let (_d2, home2) = self::home();
        let m2 = Market::new(&home2);
        assert_eq!(m2.listing(&unsigned).into_iter().find(|l| l.package.name == "api-inventory").unwrap().verification.level, TrustLevel::Unverified);
        m2.install(&unsigned, "api-inventory").unwrap();
        assert_eq!(m2.verification(Kind::Skill, "api-inventory").level, TrustLevel::Unverified);
    }
}
