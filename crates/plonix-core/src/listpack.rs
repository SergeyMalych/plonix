//! Payload lists: named lists of values anyone can write and share, used on
//! the Bench to feed marked positions (see [`crate::runs`]).
//!
//! Like rule packs and filter packs, list packs are untrusted data: just a set
//! of named lists of strings. A list can't run code, send anything, or widen
//! scope — it only supplies values the person then chooses to send, through the
//! scope-gated send path. Built-in lists ship with Plonix; more can be
//! installed from the Market.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::detect::{check_text, clean};
use crate::paths::Home;
use crate::rulepack::{MAX_PACK_BYTES, check_pack_name, check_version, sha256_hex};
use crate::shelf::Shelf;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_LISTS_PER_PACK: usize = 200;
pub const MAX_VALUES_PER_LIST: usize = 50_000;
pub const MAX_VALUE_LEN: usize = 8_192;
pub const MAX_INSTALLED_PACKS: usize = 100;

/// The list packs built into Plonix.
pub const BUILTIN: &[(&str, &str)] = &[("starter-lists", include_str!("../../../store/lists/starter-lists.json"))];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListPackDoc {
    /// Format version, currently 1.
    pub plonix_lists: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    pub lists: Vec<ListDef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListDef {
    /// Chosen in the Bench as `builtin:<id>`. `[a-z0-9-]`, at most 40 chars.
    pub id: String,
    /// Shown in the list picker, e.g. "Common parameters".
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The values fed through a position.
    pub values: Vec<String>,
}

/// A named list as served to clients (without its values).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NamedList {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub description: String,
    pub count: usize,
    pub pack: String,
    pub builtin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListPackInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub lists: usize,
    pub sha256: String,
    pub source: String,
    pub builtin: bool,
}

#[derive(Debug, Clone)]
pub struct ListPack {
    pub doc: ListPackDoc,
    pub sha256: String,
}

impl ListPack {
    pub fn info(&self, source: &str, builtin: bool) -> ListPackInfo {
        ListPackInfo {
            name: self.doc.name.clone(),
            version: self.doc.version.clone(),
            description: self.doc.description.clone(),
            author: self.doc.author.clone(),
            lists: self.doc.lists.len(),
            sha256: self.sha256.clone(),
            source: source.to_string(),
            builtin,
        }
    }
}

fn check_list_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 40
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else { Err(format!("`{}` must be 1-40 of [a-z0-9-], starting with a letter or digit", clean(id, 40))) }
}

/// A value may be empty (e.g. an empty-input probe) and may hold punctuation,
/// but not ANSI/control characters, which could garble a terminal.
fn check_value(v: &str) -> Result<(), String> {
    if v.len() > MAX_VALUE_LEN {
        return Err(format!("a value is longer than {MAX_VALUE_LEN} bytes"));
    }
    if v.chars().any(|c| c.is_control() && c != '\t') {
        return Err("a value contains control characters".into());
    }
    Ok(())
}

/// Parses and validates a list pack from untrusted bytes. Reports every problem.
pub fn parse(bytes: &[u8]) -> Result<ListPack, String> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(format!("pack is {} bytes; the limit is {MAX_PACK_BYTES}", bytes.len()));
    }
    let doc: ListPackDoc =
        serde_json::from_slice(bytes).map_err(|e| format!("not a valid list pack: {}", clean(&e.to_string(), 300)))?;
    let mut errors = vec![];
    if doc.plonix_lists != FORMAT_VERSION {
        errors.push(format!("plonix_lists: format {} is not supported (this Plonix reads format {FORMAT_VERSION})", doc.plonix_lists));
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
    if doc.lists.is_empty() {
        errors.push("lists: a pack needs at least one list".into());
    }
    if doc.lists.len() > MAX_LISTS_PER_PACK {
        errors.push(format!("lists: at most {MAX_LISTS_PER_PACK} lists per pack"));
    }
    let mut seen = BTreeMap::new();
    for (i, l) in doc.lists.iter().take(MAX_LISTS_PER_PACK).enumerate() {
        let at = format!("lists[{i}] ({})", clean(&l.id, 40));
        if let Err(e) = check_list_id(&l.id) {
            errors.push(format!("{at}: id: {e}"));
        }
        if let Some(prev) = seen.insert(l.id.clone(), i) {
            errors.push(format!("{at}: duplicate id, also used by lists[{prev}]"));
        }
        if let Err(e) = check_text(&l.title, 80, false) {
            errors.push(format!("{at}: title: {e}"));
        }
        if let Err(e) = check_text(&l.description, 300, true) {
            errors.push(format!("{at}: description: {e}"));
        }
        if l.values.is_empty() {
            errors.push(format!("{at}: values: a list needs at least one value"));
        }
        if l.values.len() > MAX_VALUES_PER_LIST {
            errors.push(format!("{at}: values: at most {MAX_VALUES_PER_LIST} values per list"));
        }
        for v in l.values.iter().take(MAX_VALUES_PER_LIST) {
            if let Err(e) = check_value(v) {
                errors.push(format!("{at}: values: {e}"));
                break;
            }
        }
    }
    if !errors.is_empty() {
        return Err(format!("invalid list pack:\n  - {}", errors.join("\n  - ")));
    }
    Ok(ListPack { doc, sha256: sha256_hex(bytes) })
}

/// Every named list in effect.
#[derive(Debug, Default, Clone)]
pub struct ListSet {
    /// id to its values.
    pub values: BTreeMap<String, Vec<String>>,
    /// id to its metadata (what clients see).
    pub lists: BTreeMap<String, NamedList>,
    pub packs: Vec<ListPackInfo>,
    pub problems: Vec<String>,
}

impl ListSet {
    /// The values of a named list, if it exists.
    pub fn values_of(&self, id: &str) -> Option<Vec<String>> {
        self.values.get(id).cloned()
    }

    /// The lists clients can choose, built-in first then installed, each in the
    /// order its pack lists them.
    pub fn catalog(&self) -> Vec<&NamedList> {
        let mut out: Vec<&NamedList> = self.lists.values().collect();
        out.sort_by(|a, b| b.builtin.cmp(&a.builtin).then(a.id.cmp(&b.id)));
        out
    }

    fn add(&mut self, pack: &ListPack, info: ListPackInfo) {
        for l in &pack.doc.lists {
            if let Some(existing) = self.lists.get(&l.id) {
                // First one wins: built-ins, then installed packs by name.
                self.problems.push(format!("list {} from {} is ignored: {} already defines it", l.id, pack.doc.name, existing.pack));
                continue;
            }
            self.lists.insert(
                l.id.clone(),
                NamedList { id: l.id.clone(), title: l.title.clone(), description: l.description.clone(), count: l.values.len(), pack: pack.doc.name.clone(), builtin: info.builtin },
            );
            self.values.insert(l.id.clone(), l.values.clone());
        }
        self.packs.push(info);
    }
}

/// List packs installed in a Plonix home, plus the built-in ones.
pub struct ListLibrary {
    shelf: Shelf,
}

impl ListLibrary {
    pub fn new(home: &Home) -> Self {
        Self::at(&home.root.join("lists"))
    }

    pub fn at(dir: &Path) -> Self {
        Self { shelf: Shelf::new(dir, "list pack", "lists", MAX_INSTALLED_PACKS) }
    }

    pub fn stamp(&self) -> Option<std::time::SystemTime> {
        self.shelf.stamp()
    }

    pub fn install(&self, bytes: &[u8], source: &str, expected_sha256: Option<&str>) -> Result<(ListPack, Option<String>)> {
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

    pub fn installed(&self) -> Vec<crate::shelf::Installed> {
        self.shelf.installed()
    }

    pub fn load(&self) -> ListSet {
        let mut set = ListSet::default();
        for (name, text) in BUILTIN {
            match parse(text.as_bytes()) {
                Ok(p) => {
                    let info = p.info("built-in", true);
                    set.add(&p, info);
                }
                Err(e) => set.problems.push(format!("built-in list pack {name}: {e}")),
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
                Ok(_) => set.problems.push(format!("list pack {}: name inside the file does not match", v.name)),
                Err(e) => set.problems.push(format!("list pack {}: {e}", v.name)),
            }
        }
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACK: &str = r#"{"plonix_lists":1,"name":"acme","version":"1.0.0","description":"Acme lists","author":"red team",
        "lists":[{"id":"acme-ids","title":"Acme ids","values":["1","2","3"]}]}"#;

    #[test]
    fn builtin_lists_are_valid_and_cover_the_starters() {
        let set = ListLibrary::at(Path::new("/nonexistent")).load();
        assert!(set.problems.is_empty(), "{:?}", set.problems);
        for id in ["digits", "numbers-1-100", "common-params", "common-paths", "input-probes"] {
            assert!(set.lists.contains_key(id), "missing built-in list {id}");
        }
        assert_eq!(set.values_of("numbers-1-100").unwrap().len(), 100);
        assert!(set.lists["digits"].builtin);
    }

    #[test]
    fn install_load_remove_and_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let lib = ListLibrary::at(dir.path());
        assert!(lib.install(PACK.as_bytes(), "x", Some(&"0".repeat(64))).unwrap_err().to_string().contains("checksum mismatch"));
        lib.install(PACK.as_bytes(), "./acme.json", None).unwrap();
        let set = lib.load();
        assert_eq!(set.lists["acme-ids"].pack, "acme");
        assert_eq!(set.values_of("acme-ids").unwrap(), vec!["1", "2", "3"]);
        assert!(!set.lists["acme-ids"].builtin);

        std::fs::write(dir.path().join("packs/acme.json"), PACK.replace("\"3\"", "\"4\"")).unwrap();
        let set = lib.load();
        assert!(!set.lists.contains_key("acme-ids"));
        assert!(set.problems[0].contains("checksum mismatch"), "{:?}", set.problems);

        assert!(lib.remove("acme").unwrap());
        assert!(lib.remove("starter-lists").is_err());
    }

    #[test]
    fn invalid_packs_report_every_problem() {
        let bad = r#"{"plonix_lists":1,"name":"bad","version":"1.0.0","description":"d","author":"a","lists":[
            {"id":"Up","title":"x","values":["a"]},
            {"id":"empty","title":"x","values":[]},
            {"id":"ansi","title":"x\u001b[2J","values":["a"]}]}"#;
        let err = parse(bad.as_bytes()).unwrap_err();
        for want in ["lists[0] (Up): id", "lists[1] (empty): values", "control characters"] {
            assert!(err.contains(want), "missing `{want}` in:\n{err}");
        }
        assert!(parse(PACK.replace("\"values\"", "\"exec\":\"rm -rf /\",\"values\"").as_bytes()).unwrap_err().contains("unknown field"));
    }

    #[test]
    fn later_packs_cannot_redefine_a_builtin_list() {
        let dir = tempfile::tempdir().unwrap();
        let lib = ListLibrary::at(dir.path());
        let shadow = PACK.replace("acme-ids", "digits").replace("\"1\",\"2\",\"3\"", "\"x\"");
        lib.install(shadow.as_bytes(), "x", None).unwrap();
        let set = lib.load();
        assert_eq!(set.lists["digits"].pack, "starter-lists");
        assert!(set.problems.iter().any(|p| p.contains("list digits from acme is ignored")));
    }
}
