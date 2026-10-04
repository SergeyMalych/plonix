//! Filter packs: named traffic filters anyone can write and share.
//!
//! A named filter is a label plus a query in the normal search language,
//! used as `is:<id>` in a search or picked from the Traffic filter builder.
//! `-is:<id>` hides what it matches. Like rule packs, filter packs are
//! untrusted data: the query must parse, may not refer to other named
//! filters, and can only narrow what the user already sees. A filter can't
//! widen scope, send anything, or run code.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::detect::{check_text, clean};
use crate::paths::Home;
use crate::query::Query;
use crate::rulepack::{MAX_PACK_BYTES, check_pack_name, check_version, sha256_hex};
use crate::shelf::Shelf;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_FILTERS_PER_PACK: usize = 200;
pub const MAX_INSTALLED_PACKS: usize = 100;
const MAX_QUERY: usize = 500;

pub const BUILTIN: &[(&str, &str)] = &[("common", include_str!("../../../store/filterpacks/common.json"))];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterPackDoc {
    /// Format version, currently 1.
    pub plonix_filters: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    pub filters: Vec<FilterDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterDef {
    /// Used as `is:<id>`. `[a-z0-9-]`, at most 40 characters.
    pub id: String,
    /// Shown on the filter chip, e.g. "GraphQL".
    pub label: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// A query in the search language, e.g. `path:*graphql*`.
    pub query: String,
}

/// A filter as served to clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NamedFilter {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub description: String,
    pub query: String,
    pub pack: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterPackInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub filters: usize,
    pub sha256: String,
    pub source: String,
    pub builtin: bool,
}

#[derive(Debug, Clone)]
pub struct FilterPack {
    pub doc: FilterPackDoc,
    pub sha256: String,
}

impl FilterPack {
    pub fn info(&self, source: &str, builtin: bool) -> FilterPackInfo {
        FilterPackInfo {
            name: self.doc.name.clone(),
            version: self.doc.version.clone(),
            description: self.doc.description.clone(),
            author: self.doc.author.clone(),
            filters: self.doc.filters.len(),
            sha256: self.sha256.clone(),
            source: source.to_string(),
            builtin,
        }
    }
}

fn check_filter_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 40
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else { Err(format!("`{}` must be 1-40 of [a-z0-9-], starting with a letter or digit", clean(id, 40))) }
}

/// Parses and validates a filter pack from untrusted bytes. Reports every problem.
pub fn parse(bytes: &[u8]) -> Result<FilterPack, String> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(format!("pack is {} bytes; the limit is {MAX_PACK_BYTES}", bytes.len()));
    }
    let doc: FilterPackDoc =
        serde_json::from_slice(bytes).map_err(|e| format!("not a valid filter pack: {}", clean(&e.to_string(), 300)))?;
    let mut errors = vec![];
    if doc.plonix_filters != FORMAT_VERSION {
        errors.push(format!("plonix_filters: format {} is not supported (this Plonix reads format {FORMAT_VERSION})", doc.plonix_filters));
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
    if doc.filters.is_empty() {
        errors.push("filters: a pack needs at least one filter".into());
    }
    if doc.filters.len() > MAX_FILTERS_PER_PACK {
        errors.push(format!("filters: at most {MAX_FILTERS_PER_PACK} filters per pack"));
    }
    let mut seen = BTreeMap::new();
    for (i, f) in doc.filters.iter().take(MAX_FILTERS_PER_PACK).enumerate() {
        let at = format!("filters[{i}] ({})", clean(&f.id, 40));
        if let Err(e) = check_filter_id(&f.id) {
            errors.push(format!("{at}: id: {e}"));
        }
        if let Some(prev) = seen.insert(f.id.clone(), i) {
            errors.push(format!("{at}: duplicate id, also used by filters[{prev}]"));
        }
        if let Err(e) = check_text(&f.label, 40, false) {
            errors.push(format!("{at}: label: {e}"));
        }
        if let Err(e) = check_text(&f.description, 300, true) {
            errors.push(format!("{at}: description: {e}"));
        }
        if let Err(e) = check_text(&f.query, MAX_QUERY, false) {
            errors.push(format!("{at}: query: {e}"));
        } else {
            match Query::parse(&f.query) {
                // Parsing without named filters also rejects `is:`, so packs can't nest or loop.
                Err(e) => errors.push(format!("{at}: query: {}", clean(&e.to_string(), 200))),
                Ok(q) if q.terms.is_empty() => errors.push(format!("{at}: query: matches everything; give it at least one term")),
                Ok(_) => {}
            }
        }
    }
    if !errors.is_empty() {
        return Err(format!("invalid filter pack:\n  - {}", errors.join("\n  - ")));
    }
    Ok(FilterPack { doc, sha256: sha256_hex(bytes) })
}

/// Every named filter in effect.
#[derive(Debug, Default, Clone)]
pub struct FilterSet {
    pub filters: BTreeMap<String, NamedFilter>,
    pub packs: Vec<FilterPackInfo>,
    pub problems: Vec<String>,
}

impl FilterSet {
    pub fn query_for(&self, id: &str) -> Option<String> {
        self.filters.get(id).map(|f| f.query.clone())
    }

    /// Parses a search, expanding `is:` with these filters.
    pub fn parse(&self, input: &str) -> Result<Query> {
        Query::parse_with(input, &|id| self.query_for(id))
    }

    fn add(&mut self, pack: &FilterPack, info: FilterPackInfo) {
        for f in &pack.doc.filters {
            if let Some(existing) = self.filters.get(&f.id) {
                // First one wins: built-ins, then installed packs by name.
                self.problems.push(format!("is:{} from {} is ignored: {} already defines it", f.id, pack.doc.name, existing.pack));
                continue;
            }
            self.filters.insert(
                f.id.clone(),
                NamedFilter {
                    id: f.id.clone(),
                    label: f.label.clone(),
                    description: f.description.clone(),
                    query: f.query.clone(),
                    pack: pack.doc.name.clone(),
                },
            );
        }
        self.packs.push(info);
    }
}

/// Filter packs installed in a Plonix home, plus the built-in ones.
pub struct FilterLibrary {
    shelf: Shelf,
}

impl FilterLibrary {
    pub fn new(home: &Home) -> Self {
        Self::at(&home.root.join("filters"))
    }

    pub fn at(dir: &Path) -> Self {
        Self { shelf: Shelf::new(dir, "filter pack", "filters", MAX_INSTALLED_PACKS) }
    }

    pub fn stamp(&self) -> Option<std::time::SystemTime> {
        self.shelf.stamp()
    }

    pub fn install(&self, bytes: &[u8], source: &str, expected_sha256: Option<&str>) -> Result<(FilterPack, Option<String>)> {
        Shelf::check_sha(bytes, expected_sha256, "pack")?;
        let pack = parse(bytes).map_err(|e| anyhow!(e))?;
        if BUILTIN.iter().any(|(n, _)| *n == pack.doc.name) {
            bail!("`{}` is the name of a built-in pack; give the pack a different name", pack.doc.name);
        }
        let previous = self.shelf.put(&pack.doc.name, &pack.doc.version, bytes, source)?;
        Ok((pack, previous))
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        if BUILTIN.iter().any(|(n, _)| *n == name) {
            bail!("`{name}` is built in and cannot be removed");
        }
        self.shelf.remove(name)
    }

    pub fn installed_version(&self, name: &str) -> Option<String> {
        self.shelf.installed_version(name)
    }

    pub fn load(&self) -> FilterSet {
        let mut set = FilterSet::default();
        for (name, text) in BUILTIN {
            match parse(text.as_bytes()) {
                Ok(p) => {
                    let info = p.info("built-in", true);
                    set.add(&p, info);
                }
                Err(e) => set.problems.push(format!("built-in filter pack {name}: {e}")),
            }
        }
        let (verified, problems) = self.shelf.verified();
        set.problems.extend(problems);
        for v in verified {
            match parse(&v.bytes) {
                Ok(p) if p.doc.name == v.name => {
                    let info = p.info(&v.entry.source, false);
                    set.add(&p, info);
                }
                Ok(_) => set.problems.push(format!("filter pack {}: name inside the file does not match", v.name)),
                Err(e) => set.problems.push(format!("filter pack {}: {e}", v.name)),
            }
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACK: &str = r#"{"plonix_filters":1,"name":"acme","version":"1.0.0","description":"Acme filters","author":"red team",
        "filters":[{"id":"acme-admin","label":"Acme admin","query":"host:admin.acme.test path:/console"}]}"#;

    #[test]
    fn builtin_filters_are_valid() {
        let set = FilterLibrary::at(Path::new("/nonexistent")).load();
        assert!(set.problems.is_empty(), "{:?}", set.problems);
        for id in ["api", "auth", "graphql", "trackers"] {
            assert!(set.filters.contains_key(id), "missing is:{id}");
        }
        assert!(set.parse("is:graphql -is:trackers").is_ok());
    }

    #[test]
    fn install_load_remove_and_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let lib = FilterLibrary::at(dir.path());
        assert!(lib.install(PACK.as_bytes(), "x", Some(&"0".repeat(64))).unwrap_err().to_string().contains("checksum mismatch"));
        lib.install(PACK.as_bytes(), "./acme.json", None).unwrap();
        let set = lib.load();
        assert_eq!(set.filters["acme-admin"].pack, "acme");
        assert!(set.parse("-is:acme-admin").is_ok());

        std::fs::write(dir.path().join("packs/acme.json"), PACK.replace("/console", "/x")).unwrap();
        let set = lib.load();
        assert!(!set.filters.contains_key("acme-admin"));
        assert!(set.problems[0].contains("checksum mismatch"), "{:?}", set.problems);

        assert!(lib.remove("acme").unwrap());
        assert!(lib.remove("common").is_err());
    }

    #[test]
    fn invalid_packs_report_every_problem() {
        let bad = r#"{"plonix_filters":1,"name":"bad","version":"1.0.0","description":"d","author":"a","filters":[
            {"id":"Loop","label":"x","query":"is:api"},
            {"id":"empty","label":"x","query":"   "},
            {"id":"bad-status","label":"x","query":"status:abc"},
            {"id":"ansi","label":"x\u001b[2J","query":"path:/a"}]}"#;
        let err = parse(bad.as_bytes()).unwrap_err();
        for want in ["filters[0] (Loop): id", "unknown filter is:api", "filters[1] (empty): query", "bad status", "control characters"] {
            assert!(err.contains(want), "missing `{want}` in:\n{err}");
        }
        assert!(parse(PACK.replace("\"query\"", "\"exec\":\"rm -rf /\",\"query\"").as_bytes()).unwrap_err().contains("unknown field"));
    }

    #[test]
    fn later_packs_cannot_redefine_filters() {
        let dir = tempfile::tempdir().unwrap();
        let lib = FilterLibrary::at(dir.path());
        let shadow = PACK.replace("acme-admin", "trackers").replace("host:admin.acme.test path:/console", "status:200");
        lib.install(shadow.as_bytes(), "x", None).unwrap();
        let set = lib.load();
        assert_eq!(set.filters["trackers"].pack, "common");
        assert!(set.problems.iter().any(|p| p.contains("is:trackers from acme is ignored")));
    }
}
