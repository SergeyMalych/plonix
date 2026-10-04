//! The Market: browse, install, update and remove everything modular in
//! Plonix (skills, rule packs, filter packs, bundles and extensions) from
//! one signed catalog.
//!
//! The catalog is a [`registry::Index`]. It is only used when its signature
//! verifies against a trusted key ([`registry::verify`]), and every package
//! is checked against the SHA-256 the index lists before it is installed.
//! Nothing installed from the Market is ever executed: skills are text,
//! packs are data, and extensions with code are listed but cannot be
//! installed until the sandboxed runtime exists.
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
use crate::filterpack::{self, FilterLibrary};
use crate::paths::{Home, write_private};
use crate::registry::{self, Index, Kind, Location, Package, TrustedKey};
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
    ("skills/triage-host.md", include_str!("../../../store/skills/triage-host.md")),
    ("skills/explain-request.md", include_str!("../../../store/skills/explain-request.md")),
    ("skills/review-sign-in.md", include_str!("../../../store/skills/review-sign-in.md")),
    ("skills/check-scope.md", include_str!("../../../store/skills/check-scope.md")),
    ("skills/draft-finding.md", include_str!("../../../store/skills/draft-finding.md")),
    ("skills/api-inventory.md", include_str!("../../../store/skills/api-inventory.md")),
    ("extensions/graphql-explorer.json", include_str!("../../../store/extensions/graphql-explorer.json")),
];

// ---- settings -------------------------------------------------------------------

pub const SETTINGS: &str = "market";

pub fn settings_section() -> Section {
    Section::new(SETTINGS, "Market", Level::Global)
        .describe("Where Plonix finds skills, rules, filters, bundles and extensions. Applies to all projects.")
        .order(40)
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
}

impl MarketSettings {
    pub fn load(home: &Home) -> Self {
        let v = settings::global(home, SETTINGS);
        Self {
            index: v.get("index").and_then(Value::as_str).unwrap_or("").trim().to_string(),
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
                registry::fetch(&src, MAX_PACK_BYTES.max(skill::MAX_SKILL_BYTES)).with_context(|| format!("downloading {}", p.name))?
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

/// The name and version inside a package file, after full validation.
pub fn describe(kind: Kind, bytes: &[u8]) -> Result<(String, String), String> {
    match kind {
        Kind::Rules => rulepack::parse(bytes).map(|x| (x.doc.name, x.doc.version)).map_err(|e| e.to_string()),
        Kind::Filters => filterpack::parse(bytes).map(|x| (x.doc.name, x.doc.version)),
        Kind::Skill => skill::parse(bytes).map(|x| (x.name, x.version)),
        Kind::Extension => crate::extension::parse_manifest(bytes).map(|m| (m.name, m.version)),
        Kind::Bundle => Err("a bundle has no file".into()),
    }
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
    match open_at(&loc, &trusted, opts.allow_unsigned) {
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
    /// An extension with code: listed, not installable yet.
    NeedsRuntime,
}

/// One row of the Market.
#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    #[serde(flatten)]
    pub package: Package,
    pub status: Status,
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

pub struct Market {
    home: Home,
    pub rules: Library,
    pub filters: FilterLibrary,
    pub skills: SkillLibrary,
}

impl Market {
    pub fn new(home: &Home) -> Self {
        Self { home: home.clone(), rules: Library::new(home), filters: FilterLibrary::new(home), skills: SkillLibrary::new(home) }
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

    fn builtin(kind: Kind, name: &str) -> bool {
        let list = match kind {
            Kind::Rules => rulepack::BUILTIN,
            Kind::Filters => filterpack::BUILTIN,
            Kind::Skill => skill::BUILTIN,
            Kind::Bundle | Kind::Extension => return false,
        };
        list.iter().any(|(n, _)| *n == name)
    }

    pub fn installed_version(&self, kind: Kind, name: &str) -> Option<String> {
        match kind {
            Kind::Rules => self.rules.installed_version(name),
            Kind::Filters => self.filters.installed_version(name),
            Kind::Skill => self.skills.installed_version(name),
            Kind::Bundle => self.read_bundles().bundles.get(name).map(|b| b.version.clone()),
            Kind::Extension => None,
        }
    }

    pub fn status(&self, p: &Package) -> Status {
        if p.kind == Kind::Extension {
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
        cat.index
            .packages
            .iter()
            .map(|p| Listing {
                status: self.status(p),
                includes: registry::install_order(&cat.index, &p.name).unwrap_or_default().into_iter().filter(|n| *n != p.name).collect(),
                package: p.clone(),
            })
            .collect()
    }

    /// Installs a package and everything it requires. Every file is
    /// downloaded and verified before anything is installed.
    pub fn install(&self, cat: &Catalog, name: &str) -> Result<Vec<Change>> {
        let Some(top) = cat.index.get(name) else {
            bail!("`{}` is not in the Market (see `plonix market list`)", clean(name, 64));
        };
        let order = registry::install_order(&cat.index, name).map_err(|e| anyhow!(e))?;
        let packages: Vec<&Package> = order.iter().filter_map(|n| cat.index.get(n)).collect();
        if let Some(ext) = packages.iter().find(|p| p.kind == Kind::Extension) {
            let why = if ext.name == name { String::new() } else { format!(" ({} requires it)", top.name) };
            bail!(
                "{} is an extension{why}. Extensions need the sandboxed extension runtime, which this version of Plonix \
                 does not have yet (see docs/extensions.md).",
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
                (Kind::Skill, Some(b)) => drop(self.skills.install(&b, &source(p), Some(&p.sha256))?),
                (Kind::Bundle, _) => {}
                _ => unreachable!("extensions are refused above and files are fetched for every other kind"),
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
            return Ok(changes);
        }
        let changes = self.remove_one(name)?;
        if changes.is_empty() {
            for kind in [Kind::Rules, Kind::Filters, Kind::Skill] {
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
        Ok(changes)
    }

    fn remove_one(&self, name: &str) -> Result<Vec<Change>> {
        let mut changes = vec![];
        for kind in [Kind::Rules, Kind::Filters, Kind::Skill] {
            let Some(version) = self.installed_version(kind, name) else { continue };
            match kind {
                Kind::Rules => self.rules.remove(name)?,
                Kind::Filters => self.filters.remove(name)?,
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

    #[test]
    fn refuses_extensions_and_tampered_packages() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let cat = official();
        let ext = cat.index.packages.iter().find(|p| p.kind == Kind::Extension).unwrap();
        assert_eq!(market.status(ext), Status::NeedsRuntime);
        assert!(market.install(&cat, &ext.name).unwrap_err().to_string().contains("runtime"));

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
}
