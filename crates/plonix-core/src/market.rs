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

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blocklist::{self, Blocklist};
use crate::detect::clean;
use crate::extension::{self, Capability, Consent, ExtensionLibrary};
use crate::detectorpack::{self, DetectorLibrary};
use crate::filterpack::{self, FilterLibrary};
use crate::listpack::{self, ListLibrary};
use crate::paths::{Home, write_private};
use crate::platform::{self, PlatformLibrary};
use crate::registry::{self, COMMUNITY_PUBLISHER, Index, Kind, Location, OFFICIAL_PUBLISHER, Package, TrustedKey};
use crate::rulepack::{self, Library, MAX_PACK_BYTES, newer, sha256_hex};
use crate::settings::{self, Field, Level, Section};
use crate::skill::{self, SkillLibrary};
use crate::tool::{self, ToolLibrary};

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
    ("detectorpacks/mind-reader.json", include_str!("../../../store/detectorpacks/mind-reader.json")),
    ("lists/starter-lists.json", include_str!("../../../store/lists/starter-lists.json")),
    ("lists/extra-wordlists.json", include_str!("../../../store/lists/extra-wordlists.json")),
    ("platforms/hackerone.json", include_str!("../../../store/platforms/hackerone.json")),
    ("tools/saved-users.json", include_str!("../../../store/tools/saved-users.json")),
    ("tools/access-check.json", include_str!("../../../store/tools/access-check.json")),
    ("tools/callbacks.json", include_str!("../../../store/tools/callbacks.json")),
    ("tools/programs.json", include_str!("../../../store/tools/programs.json")),
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

/// The tools built into Plonix, each with the paragraphs the Market shows on
/// its page. They are served from [`SNAPSHOT`] and injected into the catalog
/// Plonix itself publishes (below), so switching a built-in capability on is
/// an ordinary Market install and does not need the catalog to carry them.
const BUILTIN_TOOLS: &[(&str, &[&str])] = &[
    (
        "tools/saved-users.json",
        &[
            "Adds a user switcher to the Bench. You keep a short list of the people an application knows — each with its own cookies or token — and choose which one a request is sent as.",
            "Nothing is sent on its own and scope still applies: switching user only changes the auth headers on a request you send yourself, through the same scope-gated path as everything else.",
            "Install it when you want to see how an application answers the same request for different users.",
        ],
    ),
    (
        "tools/access-check.json",
        &[
            "Adds an Access check screen. Pick some endpoints in Traffic or a branch of the Map, and it replays each request as every saved user, and once signed out, then lines the responses up side by side.",
            "It draws no conclusions on its own. It shows each response's status and size and points out where responses match or where a signed-out request still succeeded, so you can judge what belongs to whom.",
            "It replays only requests you already captured, through the scope-gated send path. Works best with the Saved users tool, so it has identities to replay as.",
        ],
    ),
    (
        "tools/callbacks.json",
        &[
            "Adds a Callbacks screen. Make a host for each test, put it in a request (a URL parameter, a header, a webhook field), and see every DNS lookup, HTTP request or mail that later reaches it, with the time, the address it came from and the raw request.",
            "Each host carries its own name, so a callback points straight at the test it came from. Insert one from the Bench in a click, find the request that carried it, and turn a callback into a finding.",
            "Nothing runs on its own: listening starts when you press Start and registers only with the callback server. It uses interactsh, the open-source callback tool by ProjectDiscovery (MIT license), which you install in Terminal with `brew install go && go install github.com/projectdiscovery/interactsh/cmd/interactsh-client@latest`. Use the public servers or your own, with a token.",
        ],
    ),
    (
        "tools/programs.json",
        &[
            "Adds a Programs screen. Bring in a bug bounty or disclosure program from a connected platform such as HackerOne, from its pasted policy, or from a domain's security.txt, and review it before you follow it.",
            "Following a program turns its in-scope assets into scope rules and keeps its rules while you test: the rate limit, the headers it asks for, and whether it allows automated testing. Connect a platform with your own token and every program you can see is pulled in, so you can search them by name or by asset.",
            "Nothing is sent on its own. Plonix only talks to the platform you connect, with the token you give it, and keeps the token in the Keychain.",
        ],
    ),
];

/// The built-in tool packages, parsed from the snapshot. Each is a real
/// package the Market can install and remove.
fn builtin_tool_packages() -> Vec<Package> {
    BUILTIN_TOOLS
        .iter()
        .filter_map(|(url, about)| {
            let bytes = SNAPSHOT.iter().find(|(p, _)| p == url)?.1.as_bytes();
            let t = tool::parse(bytes).ok()?;
            Some(Package {
                name: t.doc.name,
                kind: Kind::Tool,
                version: t.doc.version,
                description: t.doc.description,
                author: t.doc.author,
                url: url.to_string(),
                sha256: sha256_hex(bytes),
                homepage: t.doc.homepage,
                about: about.iter().map(|s| s.to_string()).collect(),
                requires: vec![],
            })
        })
        .collect()
}

/// Adds the built-in tools to a catalog Plonix itself vouches for. Only a
/// catalog signed by the official key gets them, so a third-party Market is
/// never made to look as if it offers Plonix's own tools.
fn inject_builtin_tools(index: &mut Index, trust: &Trust) {
    let official = matches!(trust, Trust::Verified { key, .. } if key == registry::OFFICIAL_KEY);
    if !official {
        return;
    }
    for p in builtin_tool_packages() {
        if !index.packages.iter().any(|x| x.name == p.name) {
            index.packages.push(p);
        }
    }
}

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
            Field::toggle("community", "Show the community Market", true)
                .help("Packages written and maintained by their authors, listed next to the Plonix Market with a Community badge. Plonix checks them automatically; the maintainers do not review their code."),
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
    pub community: bool,
}

impl MarketSettings {
    pub fn load(home: &Home) -> Self {
        let v = settings::global(home, SETTINGS);
        Self {
            index: v.get("index").and_then(Value::as_str).unwrap_or("").trim().to_string(),
            allow_unsigned: v.get("allow_unsigned").and_then(Value::as_bool).unwrap_or(false),
            community: v.get("community").and_then(Value::as_bool).unwrap_or(true),
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
    /// Packages that came from the community Market, by name.
    pub community: BTreeSet<String>,
    /// Why the community Market is not shown, when it is not.
    pub community_note: Option<String>,
    /// What the Plonix maintainers have pulled.
    pub blocklist: Blocklist,
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

    /// Signed by the Plonix maintainers.
    pub fn official(&self) -> bool {
        matches!(&self.trust, Trust::Verified { key, .. } if key == registry::OFFICIAL_KEY)
    }

    /// Listed in the community Market rather than this catalog itself.
    pub fn is_community(&self, p: &Package) -> bool {
        self.community.contains(&p.name)
    }

    /// Where a package's file is downloaded from.
    pub fn source(&self, p: &Package) -> String {
        match &self.origin {
            _ if self.is_community(p) => p.url.clone(),
            Origin::Bundled => format!("Plonix Market (built in) {}", p.url),
            Origin::Remote(base) => registry::resolve(base, &p.url).map(|l| l.to_string()).unwrap_or_default(),
        }
    }

    /// Downloads a package and checks it against the index: checksum,
    /// format, name and version. Nothing is installed.
    pub fn fetch(&self, p: &Package) -> Result<Vec<u8>> {
        let builtin = (p.kind == Kind::Platform).then(|| platform::BUILTIN.iter().find(|(n, _)| *n == p.name).map(|(_, t)| *t)).flatten();
        // A built-in tool is listed with the snapshot's checksum, so it comes from the snapshot, never from a remote Market.
        let builtin = builtin.or_else(|| {
            (p.kind == Kind::Tool && BUILTIN_TOOLS.iter().any(|(url, _)| *url == p.url))
                .then(|| SNAPSHOT.iter().find(|(path, text)| *path == p.url && sha256_hex(text.as_bytes()) == p.sha256).map(|(_, t)| *t))
                .flatten()
        });
        let bytes = match &self.origin {
            _ if builtin.is_some() => builtin.unwrap().as_bytes().to_vec(),
            // A community package's address was made absolute when the list was read.
            _ if self.is_community(p) => {
                let src = registry::location(&p.url).map_err(|e| anyhow!("{}: {e}", p.name))?;
                registry::fetch(&src, max_bytes(p.kind)).with_context(|| format!("downloading {}", p.name))?
            }
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
        if let Some(why) = self.blocklist.blocked(p.kind, &p.name, &actual) {
            bail!("{}", Blocklist::message(&p.name, why));
        }
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
        Kind::Detectors => detectorpack::parse(bytes).map(|x| (x.doc.name, x.doc.version)),
        Kind::List => listpack::parse(bytes).map(|x| (x.doc.name, x.doc.version)),
        Kind::Tool => tool::parse(bytes).map(|x| (x.doc.name, x.doc.version)),
        Kind::Skill => skill::parse(bytes).map(|x| (x.name, x.version)),
        Kind::Platform => platform::parse(bytes).map(|x| (x.doc.name, x.doc.version)),
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
    let settings = MarketSettings::load(home);
    let trusted = registry::trusted_keys(&settings.trusted_keys);
    let loc = registry::location(&address).map_err(|e| anyhow!(e))?;
    let mut cat = match open_at(&loc, &trusted, opts.allow_unsigned || settings.allow_unsigned) {
        Ok(c) => c,
        Err(e) if address == registry::DEFAULT_INDEX => {
            let mut c = bundled(&trusted)?;
            c.offline_reason = Some(format!("{e:#}"));
            c
        }
        Err(e) => return Err(e),
    };
    // The block list and the community Market sit next to the Plonix Market only.
    if cat.official()
        && let Origin::Remote(loc) = &cat.origin
    {
        let _ = blocklist::refresh(&home.root, loc);
    }
    cat.blocklist = blocklist::load(&home.root);
    let community = std::env::var("PLONIX_COMMUNITY_INDEX").ok().filter(|s| !s.trim().is_empty());
    if cat.official() && settings.community && (community.is_some() || address == registry::DEFAULT_INDEX) {
        let address = community.unwrap_or_else(|| registry::COMMUNITY_INDEX.to_string());
        match open_community(&address) {
            Ok((loc, index)) => merge_community(&mut cat, &loc, index),
            Err(e) => cat.community_note = Some(format!("{e:#}")),
        }
    }
    let blocked = cat.blocklist.clone();
    cat.index.packages.retain(|p| blocked.blocked(p.kind, &p.name, &p.sha256).is_none());
    Market::new(home).enforce(&blocked);
    Ok(cat)
}

/// Reads the community Market and checks it was signed with the community key.
pub fn open_community(address: &str) -> Result<(Location, Index)> {
    let loc = registry::location(address).map_err(|e| anyhow!(e))?;
    let bytes = registry::fetch(&loc, registry::MAX_INDEX_BYTES).with_context(|| format!("fetching the community Market {loc}"))?;
    let index = registry::parse(&bytes).map_err(|e| anyhow!("community Market {loc}: {e}"))?;
    let sig = registry::fetch(&registry::signature_location(&loc), registry::MAX_SIGNATURE_BYTES).with_context(|| format!("the community Market {loc} is not signed"))?;
    let keys = [TrustedKey { key: registry::COMMUNITY_KEY.into(), publisher: COMMUNITY_PUBLISHER.into() }];
    registry::verify(&bytes, &sig, &keys).map_err(|e| anyhow!("community Market {loc} failed verification: {e}"))?;
    Ok((loc, index))
}

/// Adds the community Market's packages to the catalog. The Plonix Market
/// wins a name both use; bundles, tools (which only switch on what Plonix
/// itself ships) and packages that require others are left out, so a
/// community package never pulls in anything by name.
fn merge_community(cat: &mut Catalog, loc: &Location, index: Index) {
    for mut p in index.packages {
        if matches!(p.kind, Kind::Bundle | Kind::Tool) || !p.requires.is_empty() || cat.index.get(&p.name).is_some() || Market::builtin(p.kind, &p.name) {
            continue;
        }
        let Ok(src) = registry::resolve(loc, &p.url) else { continue };
        p.url = src.to_string();
        cat.community.insert(p.name.clone());
        cat.index.packages.push(p);
    }
}

fn open_at(loc: &Location, trusted: &[TrustedKey], allow_unsigned: bool) -> Result<Catalog> {
    let bytes = registry::fetch(loc, registry::MAX_INDEX_BYTES).with_context(|| format!("fetching the Market index {loc}"))?;
    let mut index = registry::parse(&bytes).map_err(|e| anyhow!("Market index {loc}: {e}"))?;
    let sig = registry::fetch(&registry::signature_location(loc), registry::MAX_SIGNATURE_BYTES);
    let trust = match sig {
        Ok(sig) => {
            match registry::verify(&bytes, &sig, trusted) {
                Ok(key) => Trust::Verified { key: key.key, publisher: key.publisher },
                // Someone writing a Market checks it before signing it again.
                Err(_) if allow_unsigned => Trust::Unverified,
                Err(e) => bail!("Market index {loc} failed verification: {e}"),
            }
        }
        Err(_) if allow_unsigned => Trust::Unverified,
        Err(e) => bail!(
            "Market index {loc} is not signed ({e:#}). Plonix only installs from a signed Market; \
             Market authors can test an unsigned index with --allow-unsigned."
        ),
    };
    inject_builtin_tools(&mut index, &trust);
    inject_builtin_platforms(&mut index, &trust);
    inject_builtin_detectors(&mut index, &trust);
    Ok(Catalog { origin: Origin::Remote(loc.clone()), index, trust, offline_reason: None, community: BTreeSet::new(), community_note: None, blocklist: Blocklist::default() })
}

/// Lists the platform packs built into Plonix in the official Market. They
/// ship inside Plonix, so they are not in the signed index, and adding one
/// needs no new signature. Other catalogs never get them.
fn inject_builtin_platforms(index: &mut Index, trust: &Trust) {
    if !matches!(trust, Trust::Verified { key, .. } if key == registry::OFFICIAL_KEY) {
        return;
    }
    for (name, text) in platform::BUILTIN {
        if index.get(name).is_some() {
            continue;
        }
        let Ok(pack) = platform::parse(text.as_bytes()) else { continue };
        let d = pack.doc;
        index.packages.push(Package {
            name: d.name,
            kind: Kind::Platform,
            version: d.version,
            description: d.description,
            author: d.author,
            url: format!("platforms/{name}.json"),
            sha256: pack.sha256,
            homepage: d.homepage,
            about: d.about,
            requires: vec![],
        });
    }
}

/// Lists the detector packs built into Plonix in the official Market. Like the
/// built-in platforms, they ship inside Plonix, are not in the signed index,
/// and adding one needs no new signature. Other catalogs never get them.
fn inject_builtin_detectors(index: &mut Index, trust: &Trust) {
    if !matches!(trust, Trust::Verified { key, .. } if key == registry::OFFICIAL_KEY) {
        return;
    }
    for (name, text) in detectorpack::BUILTIN {
        if index.get(name).is_some() {
            continue;
        }
        let Ok(pack) = detectorpack::parse(text.as_bytes()) else { continue };
        let d = pack.doc;
        index.packages.push(Package {
            name: d.name,
            kind: Kind::Detectors,
            version: d.version,
            description: d.description,
            author: d.author,
            url: format!("detectorpacks/{name}.json"),
            sha256: pack.sha256,
            homepage: d.homepage,
            about: vec![],
            requires: vec![],
        });
    }
}

/// The copy of the Plonix Market built into this Plonix.
pub fn bundled(trusted: &[TrustedKey]) -> Result<Catalog> {
    let file = |name: &str| SNAPSHOT.iter().find(|(p, _)| *p == name).map(|(_, t)| t.as_bytes()).unwrap_or_default();
    let bytes = file("index.json");
    let mut index = registry::parse(bytes).map_err(|e| anyhow!("built-in Market index: {e}"))?;
    let key = registry::verify(bytes, file("index.json.sig"), trusted).map_err(|e| anyhow!("built-in Market index: {e}"))?;
    let trust = Trust::Verified { key: key.key, publisher: key.publisher };
    inject_builtin_tools(&mut index, &trust);
    inject_builtin_platforms(&mut index, &trust);
    inject_builtin_detectors(&mut index, &trust);
    Ok(Catalog { origin: Origin::Bundled, index, trust, offline_reason: None, community: BTreeSet::new(), community_note: None, blocklist: Blocklist::default() })
}

/// Recently opened catalogs, so the window does not refetch on every click.
static CACHE: Mutex<Option<(Instant, String, Arc<Catalog>)>> = Mutex::new(None);
const CACHE_TTL: Duration = Duration::from_secs(300);

/// [`open`], reusing a catalog fetched in the last few minutes.
pub fn open_cached(home: &Home, refresh: bool) -> Result<Arc<Catalog>> {
    let settings = MarketSettings::load(home);
    let key = format!("{}|{:?}|{}", index_address(home, &OpenOptions::default()), settings.trusted_keys, settings.allow_unsigned);
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

/// Which shelf of the Market a package is on: who stands behind it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Shelf {
    /// Built in, or from the Plonix Market: reviewed by the maintainers.
    Official,
    /// From the community Market: checked automatically, not reviewed.
    Community,
    /// From a Market whose publisher you chose to trust.
    Publisher,
    /// Added by you from a repository, a folder, a file or a link, or from an
    /// unsigned Market: nobody has looked at it.
    Own,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verification {
    pub level: TrustLevel,
    pub shelf: Shelf,
    /// Short, for a badge: "Verified by Plonix maintainers".
    pub label: String,
    /// One or two sentences for the details.
    pub detail: String,
}

impl Verification {
    fn built_in() -> Self {
        Self { level: TrustLevel::BuiltIn, shelf: Shelf::Official, label: "Built in".into(), detail: "Ships inside Plonix, reviewed with the app itself.".into() }
    }

    fn signed(publisher: &str, has_file: bool) -> Self {
        let checked = if has_file {
            "The file is pinned by SHA-256 in the signed Market list, and was checked in full before it installed."
        } else {
            "Listed in the signed Market list; the packages it installs are each checked the same way."
        };
        let (shelf, label, who) = if publisher == OFFICIAL_PUBLISHER {
            (Shelf::Official, "Official".to_string(), "Reviewed by the Plonix maintainers, who signed the Market list.")
        } else if publisher == COMMUNITY_PUBLISHER {
            (
                Shelf::Community,
                "Community".to_string(),
                "Written and maintained by its author, and listed in the community Market after automatic checks and a review of what it asks for. The Plonix maintainers have not reviewed its code.",
            )
        } else {
            (Shelf::Publisher, format!("Verified by {publisher}"), "Signed by a publisher whose key you chose to trust.")
        };
        Self { level: TrustLevel::Verified, shelf, label, detail: format!("{who} {checked}") }
    }

    fn unsigned_catalog() -> Self {
        Self {
            level: TrustLevel::Unverified,
            shelf: Shelf::Own,
            label: "Not verified".into(),
            detail: "This Market list is not signed, so nobody vouches for it. The file still matches the list and is validated, but read what it does before you install it.".into(),
        }
    }

    fn by_hand(kind: Kind, source: &str) -> Self {
        let still = if kind == Kind::Extension {
            "It is still checked, and it can only do what you said yes to when you added it."
        } else {
            "It is still validated and cannot run code."
        };
        Self {
            level: TrustLevel::Unverified,
            shelf: Shelf::Own,
            label: "Your own".into(),
            detail: format!(
                "You added this yourself from {}, not through a signed Market, so nobody has reviewed it. {still}",
                crate::detect::clean(&describe_source(source), 140)
            ),
        }
    }

    fn changed() -> Self {
        Self {
            level: TrustLevel::Changed,
            shelf: Shelf::Own,
            label: "Changed since install".into(),
            detail: "The file on disk is no longer the one that was checked. Plonix does not load it. Remove it and install it again.".into(),
        }
    }
}

/// Where something added by hand came from, in words.
pub fn describe_source(source: &str) -> String {
    if let Some(Ok(r)) = crate::github::parse(source) {
        return match &r.tag {
            Some(t) => format!("the GitHub repository {}/{} (release {t})", r.owner, r.repo),
            None => format!("the GitHub repository {}/{}", r.owner, r.repo),
        };
    }
    if std::path::Path::new(source).is_dir() {
        return format!("the folder {source}");
    }
    source.to_string()
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
    /// For something added by hand: where from (`github:owner/repo@tag`, a
    /// folder, a file or a link), so it can be checked for a newer release
    /// or read again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub added_from: Option<String>,
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

/// What updating everything did: the changes made, and the packages that could not be updated.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Updated {
    pub changes: Vec<Change>,
    pub failed: Vec<UpdateFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateFailure {
    pub name: String,
    pub kind: Kind,
    pub error: String,
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
    pub detectors: DetectorLibrary,
    pub lists: ListLibrary,
    pub tools: ToolLibrary,
    pub skills: SkillLibrary,
    pub extensions: ExtensionLibrary,
    pub platforms: PlatformLibrary,
}

impl Market {
    pub fn new(home: &Home) -> Self {
        Self {
            home: home.clone(),
            rules: Library::new(home),
            filters: FilterLibrary::new(home),
            detectors: DetectorLibrary::new(home),
            lists: ListLibrary::new(home),
            tools: ToolLibrary::new(home),
            skills: SkillLibrary::new(home),
            extensions: ExtensionLibrary::new(home),
            platforms: PlatformLibrary::new(home),
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
                _ if cat.is_community(p) => Some(COMMUNITY_PUBLISHER.to_string()),
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
        v.extend(self.detectors.installed().into_iter().map(|i| (Kind::Detectors, i)));
        v.extend(self.lists.installed().into_iter().map(|i| (Kind::List, i)));
        v.extend(self.tools.installed().into_iter().map(|i| (Kind::Tool, i)));
        v.extend(self.skills.installed().into_iter().map(|i| (Kind::Skill, i)));
        v.extend(self.extensions.installed().into_iter().map(|i| (Kind::Extension, i)));
        v.extend(self.platforms.installed().into_iter().map(|i| (Kind::Platform, i)));
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
            _ if cat.is_community(p) => Verification::signed(COMMUNITY_PUBLISHER, true),
            Trust::Verified { publisher, .. } => Verification::signed(publisher, p.kind != Kind::Bundle),
            Trust::Unverified => Verification::unsigned_catalog(),
        }
    }

    }

/// A newer release of something added from a GitHub repository.
#[derive(Debug, Clone, Serialize)]
pub struct AddedUpdate {
    pub name: String,
    pub kind: Kind,
    /// The release installed now.
    pub installed: String,
    pub latest: String,
    /// What to add to get it: `github:owner/repo@latest`.
    pub source: String,
    /// Why the repository could not be checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Reads a package from outside the Market: a GitHub repository
/// (`github:owner/repo`), an https address, a file, or an extension's
/// folder (packed on the fly). Returns the bytes and how to record where
/// they came from. Nothing is checked or installed yet.
pub fn read_external(source: &str) -> Result<(Vec<u8>, String)> {
    if let Some(repo) = crate::github::parse(source) {
        let repo = repo.map_err(|e| anyhow!(e))?;
        return crate::github::download(&repo, max_bytes(Kind::Extension));
    }
    let loc = registry::location(source).map_err(|e| anyhow!(e))?;
    match &loc {
        Location::File(p) => {
            let bytes = extension::read_source(p)?;
            Ok((bytes, std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()).display().to_string()))
        }
        Location::Url(u) => Ok((registry::fetch(&loc, max_bytes(Kind::Extension))?, u.clone())),
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

/// `program` is the program a program extension runs, which decides how
/// `run-program` is described.
pub fn capability_infos(caps: &[Capability], program: Option<&str>) -> Vec<CapabilityInfo> {
    caps.iter().map(|c| CapabilityInfo { id: *c, what: c.describe_for(program), sensitive: c.sensitive() }).collect()
}

/// What an extension's code can and cannot do, for the consent screen.
pub const SANDBOX_NOTE: &str = "Its code runs in the Plonix sandbox: no network, files, processes or clock, limited CPU time and memory. \
     It is stopped and switched off if it misbehaves.";

/// How a program extension runs, by the kind of program it drives: each kind
/// reaches different things, so each gets its own plain account.
pub fn program_note(program: Option<&str>) -> &'static str {
    match program.and_then(crate::program::get).map(|p| p.kind) {
        Some(crate::program::Kind::Enumerate) => {
            "The program runs on this Mac and gets only the domains you accepted in Scope, never your captured traffic. \
             It asks public sources on the internet which subdomains they know of. Nothing joins your scope until you accept it."
        }
        Some(crate::program::Kind::Probe) => {
            "Plonix runs this itself, with no outside program. It sends a small, fixed set of requests to the in-scope address \
             you pick, and each one shows in Traffic. Findings it proposes stay unconfirmed until you confirm them."
        }
        _ => {
            "The program runs on this Mac only, over copies of captured requests and responses that are deleted when it \
             finishes. It works offline: Plonix tells it not to check what it finds with outside services and not to update itself."
        }
    }
}

/// What to tell someone about how an extension runs.
pub fn runtime_note(m: &extension::Manifest) -> &'static str {
    if m.runtime == extension::Runtime::Program { program_note(m.program.as_deref()) } else { SANDBOX_NOTE }
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
    } else if v.get("plonix_detectors").is_some() {
        Ok(Kind::Detectors)
    } else if v.get("plonix_lists").is_some() {
        Ok(Kind::List)
    } else if v.get("plonix_platform").is_some() {
        Ok(Kind::Platform)
    } else if v.get("plonix_tool").is_some() {
        Ok(Kind::Tool)
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
            Kind::Detectors => {
                let p = detectorpack::parse(&bytes)?;
                (
                    p.doc.description.clone(),
                    p.doc.author.clone(),
                    vec![format!("Adds {} Mind Reader suggestions. Data only: each can only offer a chip that pre-fills another tab, never act on its own.", p.doc.detectors.len())],
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
            Kind::Platform => {
                let p = platform::parse(&bytes)?;
                (
                    p.doc.description.clone(),
                    p.doc.author.clone(),
                    vec![
                        format!("Lets Plonix read your programs from {} at {}, with a token you give it. It talks to no other address.", p.doc.title, p.doc.api),
                        "Data only; it cannot run code, and the program's scope still needs your review before it applies.".into(),
                    ],
                )
            }
            Kind::Tool => {
                let p = tool::parse(&bytes)?;
                let on = tool::feature(&p.doc.feature).map(|f| f.title).unwrap_or("a built-in tool");
                (p.doc.description.clone(), p.doc.author.clone(), vec![format!("Switches on {on} in Plonix. No code; it only turns a built-in capability on.")])
            }
            Kind::Extension => {
                let p = extension::parse_package(&bytes)?;
                capabilities = capability_infos(&p.manifest.capabilities, p.manifest.program.as_deref());
                let mut effects: Vec<String> = p.manifest.capabilities.iter().map(|c| format!("Can {}.", c.describe())).collect();
                effects.push(runtime_note(&p.manifest).into());
                (p.manifest.description, p.manifest.author, effects)
            }
            _ => unreachable!(),
        };
        if Self::builtin(kind, &name) {
            return Err(format!("`{name}` is the name of a built-in {}; give it a different name", kind.noun()));
        }
        let sha256 = sha256_hex(&bytes);
        if let Some(why) = blocklist::load(&self.home.root).blocked(kind, &name, &sha256) {
            return Err(Blocklist::message(&name, why));
        }
        let replaces = self.installed_version(kind, &name);
        Ok(External { kind, name, version, description, author, sha256, source: source.to_string(), effects, capabilities, replaces, bytes })
    }

    /// Installs a file from outside the Market. It is recorded as not
    /// verified. The caller has shown what it does, so adding it approves
    /// what an extension asks for; sensitive capabilities still need `consent`.
    pub fn add_external(&self, ext: &External, consent: &Consent) -> Result<Change> {
        let src = ext.source.as_str();
        let previous = match ext.kind {
            Kind::Rules => self.rules.install(&ext.bytes, src, Some(&ext.sha256))?.1,
            Kind::Filters => self.filters.install(&ext.bytes, src, Some(&ext.sha256))?.1,
            Kind::Detectors => self.detectors.install(&ext.bytes, src, Some(&ext.sha256))?.1,
            Kind::List => self.lists.install(&ext.bytes, src, Some(&ext.sha256))?.1,
            Kind::Tool => self.tools.install(&ext.bytes, src, Some(&ext.sha256))?.1,
            Kind::Skill => self.skills.install(&ext.bytes, src, Some(&ext.sha256))?.1,
            Kind::Platform => self.platforms.install(&ext.bytes, src, Some(&ext.sha256))?.1,
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

    /// Applies the block list to what is installed: a blocked extension is
    /// switched off with the reason, anything else blocked is removed.
    /// Returns what it did, in words.
    pub fn enforce(&self, list: &Blocklist) -> Vec<String> {
        if list.entries.is_empty() {
            return vec![];
        }
        let mut done = vec![];
        for (kind, item) in self.installed_all() {
            let Some(why) = list.blocked(kind, &item.name, &item.entry.sha256) else { continue };
            let msg = Blocklist::message(&item.name, why);
            if kind == Kind::Extension {
                let already = self.extensions.info(&item.name).is_some_and(|i| !i.state.enabled && i.state.disabled_reason.as_deref() == Some(msg.as_str()));
                if !already && self.extensions.disable(&item.name, &msg).is_ok() {
                    done.push(format!("{msg} It is switched off."));
                }
            } else if self.remove_one(&item.name).is_ok() {
                done.push(format!("{msg} It was removed."));
            }
        }
        done
    }

    /// Newer releases of packages added from a GitHub repository. Plonix
    /// never installs them on its own: adding one again shows what it asks
    /// for first.
    pub fn added_updates(&self) -> Vec<AddedUpdate> {
        let mut out = vec![];
        for (kind, item) in self.installed_all() {
            let Some(Ok(repo)) = crate::github::parse(&item.entry.source) else { continue };
            let Some(installed) = repo.tag.clone() else { continue };
            match crate::github::latest_tag(&repo) {
                Ok(latest) if latest != installed => out.push(AddedUpdate {
                    name: item.name.clone(),
                    kind,
                    installed,
                    latest: latest.clone(),
                    source: repo.latest().label(&latest),
                    error: None,
                }),
                Ok(_) => {}
                Err(e) => out.push(AddedUpdate {
                    name: item.name.clone(),
                    kind,
                    installed: installed.clone(),
                    latest: installed,
                    source: item.entry.source.clone(),
                    error: Some(format!("{e:#}")),
                }),
            }
        }
        out
    }

    /// Installed packages that are not in the catalog: added by hand.
    fn local_listing(&self, cat: &Catalog) -> Vec<Listing> {
        let rules = self.rules.load();
        let filters = self.filters.load();
        let detectors = self.detectors.load();
        let lists = self.lists.load();
        let skills = self.skills.load();
        let extensions = self.extensions.list();
        let platforms = self.platforms.load().0;
        let mut out = vec![];
        for (kind, item) in self.installed_all() {
            if cat.index.get(&item.name).is_some() {
                continue;
            }
            let (description, author) = match kind {
                Kind::Rules => rules.packs.iter().find(|(_, i)| i.name == item.name).map(|(_, i)| (i.description.clone(), i.author.clone())),
                Kind::Filters => filters.packs.iter().find(|i| i.name == item.name).map(|i| (i.description.clone(), i.author.clone())),
                Kind::Detectors => detectors.packs.iter().find(|i| i.name == item.name).map(|i| (i.description.clone(), i.author.clone())),
                Kind::List => lists.packs.iter().find(|i| i.name == item.name).map(|i| (i.description.clone(), i.author.clone())),
                Kind::Skill => skills.get(&item.name).map(|(s, ..)| (s.description.clone(), s.author.clone())),
                Kind::Extension => extensions.iter().find(|e| e.name == item.name && e.intact).map(|e| (e.description.clone(), e.author.clone())),
                Kind::Platform => platforms.iter().find(|(p, ..)| p.doc.name == item.name).map(|(p, ..)| (p.doc.description.clone(), p.doc.author.clone())),
                Kind::Tool => tool::feature(&item.name).map(|f| (f.summary.to_string(), "Plonix contributors".to_string())),
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
                added_from: (!item.entry.source.is_empty()).then(|| item.entry.source.clone()),
            });
        }
        out
    }

    pub fn builtin(kind: Kind, name: &str) -> bool {
        let list = match kind {
            Kind::Rules => rulepack::BUILTIN,
            Kind::Filters => filterpack::BUILTIN,
            Kind::Detectors => detectorpack::BUILTIN,
            Kind::List => listpack::BUILTIN,
            Kind::Skill => skill::BUILTIN,
            Kind::Platform => platform::BUILTIN,
            Kind::Bundle | Kind::Extension | Kind::Tool => return false,
        };
        list.iter().any(|(n, _)| *n == name)
    }

    pub fn installed_version(&self, kind: Kind, name: &str) -> Option<String> {
        match kind {
            Kind::Rules => self.rules.installed_version(name),
            Kind::Filters => self.filters.installed_version(name),
            Kind::Detectors => self.detectors.installed_version(name),
            Kind::List => self.lists.installed_version(name),
            Kind::Tool => self.tools.installed_version(name),
            Kind::Skill => self.skills.installed_version(name),
            Kind::Bundle => self.read_bundles().bundles.get(name).map(|b| b.version.clone()),
            Kind::Extension => self.extensions.installed_version(name),
            Kind::Platform => self.platforms.installed_version(name),
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
                    added_from: None,
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
        let source = |p: &Package| cat.source(p);
        let mut changes = vec![];
        let mut failed = None;
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
            let done = match (p.kind, bytes) {
                (Kind::Rules, Some(b)) => self.rules.install(&b, &source(p), Some(&p.sha256)).map(drop),
                (Kind::Filters, Some(b)) => self.filters.install(&b, &source(p), Some(&p.sha256)).map(drop),
                (Kind::Detectors, Some(b)) => self.detectors.install(&b, &source(p), Some(&p.sha256)).map(drop),
                (Kind::List, Some(b)) => self.lists.install(&b, &source(p), Some(&p.sha256)).map(drop),
                (Kind::Tool, Some(b)) => self.tools.install(&b, &source(p), Some(&p.sha256)).map(drop),
                (Kind::Skill, Some(b)) => self.skills.install(&b, &source(p), Some(&p.sha256)).map(drop),
                (Kind::Extension, Some(b)) => self.extensions.install(&b, &source(p), Some(&p.sha256), consent).map(drop),
                (Kind::Platform, Some(b)) => self.platforms.install(&b, &source(p), Some(&p.sha256)).map(drop),
                (Kind::Bundle, _) => Ok(()),
                _ => unreachable!("files are fetched for every kind but bundles"),
            };
            // Stop at the first failure, but still record below what was installed before it.
            if let Err(e) = done {
                failed = Some(e);
                break;
            }
            let action = if from.is_some() { Action::Updated } else { Action::Installed };
            changes.push(Change { name: p.name.clone(), kind: p.kind, version: p.version.clone(), action, from });
        }
        let mut lock = self.read_bundles();
        // A bundle that failed part way is not installed, but one installed before keeps track of what this attempt added.
        if top.kind == Kind::Bundle && (failed.is_none() || lock.bundles.contains_key(&top.name)) {
            let mut added: Vec<String> =
                changes.iter().filter(|c| c.action == Action::Installed && c.name != top.name).map(|c| c.name.clone()).collect();
            let mut version = top.version.clone();
            if let Some(old) = lock.bundles.get(&top.name) {
                for a in &old.added {
                    if !added.contains(a) {
                        added.push(a.clone());
                    }
                }
                if failed.is_some() {
                    version = old.version.clone();
                }
            }
            let members = order.iter().filter(|n| **n != top.name).cloned().collect();
            lock.bundles.insert(top.name.clone(), BundleEntry { version, members, added });
            self.write_bundles(&lock)?;
        }
        self.record(cat, &changes, &packages)?;
        match failed {
            Some(e) => Err(e),
            None => Ok(changes),
        }
    }

    /// Installs newer versions of everything installed from the catalog. One
    /// package that cannot be updated does not stop the others.
    pub fn update(&self, cat: &Catalog) -> Updated {
        let mut out = Updated::default();
        for p in &cat.index.packages {
            if matches!(self.status(p), Status::Update { .. }) {
                match self.install(cat, &p.name) {
                    Ok(changes) => out.changes.extend(changes.into_iter().filter(|c| c.action != Action::Unchanged)),
                    Err(e) => out.failed.push(UpdateFailure { name: p.name.clone(), kind: p.kind, error: format!("{e:#}") }),
                }
            }
        }
        out
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
            for kind in [Kind::Rules, Kind::Filters, Kind::Detectors, Kind::List, Kind::Tool, Kind::Skill, Kind::Extension, Kind::Platform] {
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
        for kind in [Kind::Rules, Kind::Filters, Kind::Detectors, Kind::List, Kind::Tool, Kind::Skill, Kind::Extension, Kind::Platform] {
            let Some(version) = self.installed_version(kind, name) else { continue };
            match kind {
                Kind::Rules => self.rules.remove(name)?,
                Kind::Filters => self.filters.remove(name)?,
                Kind::Detectors => self.detectors.remove(name)?,
                Kind::List => self.lists.remove(name)?,
                Kind::Extension => self.extensions.remove(name)?,
                Kind::Platform => self.platforms.remove(name)?,
                Kind::Tool => self.tools.remove(name)?,
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
    fn built_in_platforms_are_listed_only_in_the_official_market() {
        let c = official();
        let p = c.index.get("hackerone").expect("listed");
        assert_eq!(p.kind, Kind::Platform);
        assert!(!p.about.is_empty());
        assert_eq!(c.fetch(p).unwrap(), platform::BUILTIN[0].1.as_bytes());

        let mut other = official();
        other.index.packages.retain(|p| p.kind != Kind::Platform);
        other.trust = Trust::Verified { key: "ed25519:someone-else".into(), publisher: "Someone".into() };
        inject_builtin_platforms(&mut other.index, &other.trust);
        assert!(other.index.get("hackerone").is_none());
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
    fn built_in_tools_install_from_the_official_catalog_and_switch_features_on() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let cat = official();
        // The official catalog carries the built-in tools.
        for name in ["saved-users", "access-check", "callbacks"] {
            let p = cat.index.get(name).unwrap_or_else(|| panic!("{name} missing from the official catalog"));
            assert_eq!(p.kind, Kind::Tool);
            assert!(matches!(market.status(p), Status::Available));
        }
        // Installing one switches its feature on; removing it switches it off.
        market.install(&cat, "access-check").unwrap();
        assert!(crate::tool::ToolLibrary::new(&home).enabled_features().contains("access-check"));
        assert!(matches!(market.status(cat.index.get("access-check").unwrap()), Status::Installed { .. }));
        market.remove("access-check").unwrap();
        assert!(crate::tool::ToolLibrary::new(&home).enabled_features().is_empty());

        // A third-party (unofficial) catalog is not given Plonix's own tools.
        let mut other = cat.clone();
        other.trust = Trust::Verified { key: "ed25519:AAAA".into(), publisher: "someone".into() };
        let mut idx = registry::parse(SNAPSHOT.iter().find(|(p, _)| *p == "index.json").unwrap().1.as_bytes()).unwrap();
        inject_builtin_tools(&mut idx, &other.trust);
        assert!(!idx.packages.iter().any(|p| p.kind == Kind::Tool), "third-party catalogs must not get the built-in tools");
    }

    #[test]
    fn built_in_tools_come_from_the_snapshot_even_with_a_remote_market() {
        let mut cat = official();
        cat.origin = Origin::Remote(Location::Url("https://127.0.0.1:9/".into()));
        let p = cat.index.get("saved-users").unwrap();
        let want = SNAPSHOT.iter().find(|(path, _)| *path == p.url).unwrap().1.as_bytes();
        assert_eq!(cat.fetch(p).unwrap(), want);
    }

    #[test]
    fn a_bundle_that_fails_part_way_still_records_what_it_installed() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let mut cat = official();
        let bundle = Package {
            name: "test-kit".into(),
            kind: Kind::Bundle,
            version: "1.0.0".into(),
            description: String::new(),
            author: String::new(),
            url: String::new(),
            sha256: String::new(),
            homepage: String::new(),
            about: vec![],
            requires: vec!["leaks".into(), "api-inventory".into()],
        };
        cat.index.packages.push(bundle.clone());
        // Skills cannot be written, so the second member fails after the first is installed.
        std::fs::write(home.root.join("skills"), "not a folder").unwrap();
        assert!(market.install(&cat, "test-kit").is_err());
        assert!(matches!(market.status(cat.index.get("leaks").unwrap()), Status::Installed { .. }));
        assert_eq!(market.verification(Kind::Filters, "leaks").level, TrustLevel::Verified);
        assert_eq!(market.status(&bundle), Status::Available);
    }

    #[test]
    fn update_keeps_going_past_a_package_that_fails() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let mut cat = official();
        let file = |path: &str| SNAPSHOT.iter().find(|(p, _)| *p == path).unwrap().1;
        let skill = file(&cat.index.get("api-inventory").unwrap().url).replace("version: 1.0.0", "version: 0.9.0");
        market.skills.install(skill.as_bytes(), "old", None).unwrap();
        let pack = file(&cat.index.get("leaks").unwrap().url).replace("\"version\": \"1.0.0\"", "\"version\": \"0.9.0\"");
        market.filters.install(pack.as_bytes(), "old", None).unwrap();
        cat.index.packages.iter_mut().find(|p| p.name == "leaks").unwrap().sha256 = "0".repeat(64);

        let u = market.update(&cat);
        assert!(u.changes.iter().any(|c| c.name == "api-inventory" && c.action == Action::Updated), "{u:?}");
        assert!(u.failed.len() == 1 && u.failed[0].name == "leaks" && u.failed[0].error.contains("checksum mismatch"), "{u:?}");
        assert_eq!(market.installed_version(Kind::Skill, "api-inventory").as_deref(), Some("1.0.0"));
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
    fn a_sensitive_capability_can_be_allowed_after_install() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let cat = official();
        // Installed without its yes: it stays installed, but its tool may not run.
        market.install(&cat, "subdomain-discovery").unwrap();
        let lib = &market.extensions;
        assert!(!lib.info("subdomain-discovery").unwrap().state.granted.contains(&Capability::RunProgram));
        let st = lib.set_granted("subdomain-discovery", Capability::RunProgram, true).unwrap();
        assert!(st.granted.contains(&Capability::RunProgram) && st.granted.contains(&Capability::SuggestScope));
        assert!(lib.load().extensions[0].granted.contains(&Capability::RunProgram));
        let st = lib.set_granted("subdomain-discovery", Capability::RunProgram, false).unwrap();
        assert!(!st.granted.contains(&Capability::RunProgram));
        // Only what it asks for, and only for what is installed.
        assert!(lib.set_granted("subdomain-discovery", Capability::ReadOutOfScope, true).unwrap_err().to_string().contains("does not ask"));
        assert!(lib.set_granted("nothing-here", Capability::RunProgram, true).is_err());
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

    fn community_skill(dir: &std::path::Path, name: &str) -> Index {
        let text = format!("---\nplonix_skill: 1\nname: {name}\nversion: 1.0.0\ntitle: T\ndescription: From the community.\nauthor: jsmith\nuses: [traffic]\n---\nLook at traffic.\n");
        std::fs::write(dir.join(format!("{name}.md")), &text).unwrap();
        let index = format!(
            r#"{{"plonix_index":2,"name":"Plonix community Market","packages":[
                {{"name":"{name}","kind":"skill","version":"1.0.0","description":"d","author":"jsmith","url":"{name}.md","sha256":"{}"}},
                {{"name":"api-inventory","kind":"skill","version":"9.0.0","description":"d","author":"jsmith","url":"{name}.md","sha256":"{}"}}]}}"#,
            sha256_hex(text.as_bytes()),
            sha256_hex(text.as_bytes())
        );
        registry::parse(index.as_bytes()).unwrap()
    }

    #[test]
    fn community_packages_sit_on_their_own_shelf() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let dir = tempfile::tempdir().unwrap();
        let mut cat = official();
        let loc = Location::File(dir.path().join("index.json"));
        merge_community(&mut cat, &loc, community_skill(dir.path(), "graphql-notes"));
        // The Plonix Market keeps a name both use.
        assert_eq!(cat.index.get("api-inventory").unwrap().author, "Plonix contributors");
        assert!(cat.community.contains("graphql-notes") && !cat.community.contains("api-inventory"));

        let row = |name: &str| market.listing(&cat).into_iter().find(|l| l.package.name == name).unwrap();
        assert_eq!(row("graphql-notes").verification.shelf, Shelf::Community);
        assert_eq!(row("graphql-notes").verification.label, "Community");
        assert!(row("graphql-notes").verification.detail.contains("have not reviewed its code"));
        assert_eq!(row("api-inventory").verification.shelf, Shelf::Official);

        // Installing it records the community Market as the one that vouched for it.
        market.install(&cat, "graphql-notes").unwrap();
        let v = market.verification(Kind::Skill, "graphql-notes");
        assert_eq!((v.level, v.shelf), (TrustLevel::Verified, Shelf::Community));
    }

    #[test]
    fn added_by_hand_is_your_own() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let mine = "---\nplonix_skill: 1\nname: mine\nversion: 1.0.0\ntitle: Mine\ndescription: My skill.\nauthor: me\nuses: [traffic]\n---\nLook at traffic.\n";
        market.skills.install(mine.as_bytes(), "github:jsmith/notes@v1.0.0", None).unwrap();
        let row = market.listing(&official()).into_iter().find(|l| l.package.name == "mine").unwrap();
        assert_eq!((row.verification.shelf, row.verification.label.as_str()), (Shelf::Own, "Your own"));
        assert!(row.verification.detail.contains("GitHub repository jsmith/notes (release v1.0.0)"), "{}", row.verification.detail);
        assert_eq!(row.added_from.as_deref(), Some("github:jsmith/notes@v1.0.0"));
    }

    #[test]
    fn the_block_list_switches_off_extensions_and_removes_the_rest() {
        let (_d, home) = home();
        let market = Market::new(&home);
        let cat = official();
        market.install(&cat, "security-headers").unwrap();
        market.install(&cat, "admin-panels").unwrap();
        let list = blocklist::parse(
            br#"{"plonix_blocked":1,"entries":[
                {"name":"security-headers","kind":"extension","reason":"Misreads cookies."},
                {"name":"admin-panels","reason":"Pulled."}]}"#,
        )
        .unwrap();
        let done = market.enforce(&list);
        assert_eq!(done.len(), 2, "{done:?}");
        let info = market.extensions.info("security-headers").unwrap();
        assert!(!info.state.enabled && info.state.disabled_reason.unwrap().contains("Misreads cookies."));
        assert!(market.installed_version(Kind::Rules, "admin-panels").is_none());
        // Running it again changes nothing.
        assert!(market.enforce(&list).is_empty());

        // A blocked package is not downloaded from a Market either.
        let mut blocked = official();
        blocked.blocklist = list;
        let p = blocked.index.get("admin-panels").unwrap().clone();
        assert!(format!("{:#}", blocked.fetch(&p).unwrap_err()).contains("blocked admin-panels"));
    }
}
