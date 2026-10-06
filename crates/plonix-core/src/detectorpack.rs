//! Detector packs: rules that recognize something in a request/response and
//! suggest the next step in another tab — a file upload worth checking in
//! Scans, a token worth tweaking on the Bench, a cookie worth writing up as a
//! finding. Anyone can publish one.
//!
//! A detector never acts on its own. Matching is purely declarative — a closed
//! set of conditions over the exchange Plonix already captured — and the only
//! thing a match produces is a suggestion chip that pre-fills another tab; the
//! human reviews and runs it. A pack can't run code, send a request, or widen
//! scope, and chips only ever appear for hosts already in scope.
//!
//! Matching itself happens in the app (the chips live there, next to the
//! traffic). This module validates untrusted packs — every pattern compiles,
//! every handler and value class is one Plonix knows, text is bounded — and
//! serves the clean definitions. Like the other pack types, packs are
//! size-limited, strictly schema-checked, and pinned by SHA-256 on disk.
//!
//! ```text
//! $PLONIX_HOME/detectors/
//! ├── lock.json          name → version, sha256, source, installed_at
//! └── packs/<name>.json  the exact bytes that were verified
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

use crate::detect::{check_text, clean};
use crate::paths::Home;
use crate::rulepack::{MAX_PACK_BYTES, check_pack_name, check_version, sha256_hex};
use crate::shelf::Shelf;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_DETECTORS_PER_PACK: usize = 200;
pub const MAX_INSTALLED_PACKS: usize = 100;
const MAX_PATTERN: usize = 1000;
const REGEX_SIZE_LIMIT: usize = 256 * 1024;

/// The pack shipped with Plonix, so the suggestions work out of the box.
pub const BUILTIN: &[(&str, &str)] =
    &[("mind-reader", include_str!("../../../store/detectorpacks/mind-reader.json"))];

/// Handlers a detector may hand off to. Each names a built-in action in the
/// app; a pack can only pick from this list, never run its own code.
pub const HANDLERS: &[&str] = &["scan", "bench", "finding", "access"];

/// Value classes a `param` condition may look for. A closed set, each a safe,
/// read-only shape test the app knows how to run.
pub const VALUE_CLASSES: &[&str] = &["host_or_url", "path_or_file", "jwt"];

/// Where a `param` condition looks.
pub const PARAM_PLACES: &[&str] = &["query", "body"];

/// Severities a `finding` handler may pre-set.
pub const SEVERITIES: &[&str] = &["info", "low", "medium", "high", "critical"];

/// Modes an `access` handler may pick.
pub const ACCESS_MODES: &[&str] = &["users", "anon"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectorPackDoc {
    /// Format version, currently 1.
    pub plonix_detectors: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    pub detectors: Vec<DetectorDef>,
}

/// One detector: what to recognize (`when`) and what to suggest (`suggest`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectorDef {
    /// Stable id, `[a-z0-9-]`, at most 40 characters.
    pub id: String,
    pub when: When,
    pub suggest: Suggest,
    /// Higher shows first; chips tie-break on this. Default 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub priority: i32,
}

fn is_zero(n: &i32) -> bool {
    *n == 0
}

/// Conditions over the exchange. Every field that is set must hold (AND). A
/// detector with no condition set is rejected — it would match everything.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct When {
    /// Request methods that qualify, e.g. `["POST","PUT"]`. Empty: any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub method: Vec<String>,
    /// Substring of the request's Content-Type (case-insensitive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub req_content_type: Option<String>,
    /// Substring of the response's Content-Type (case-insensitive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resp_content_type: Option<String>,
    /// Regex over the request path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_regex: Option<String>,
    /// Regex over the request body text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub req_body_regex: Option<String>,
    /// Regex over the response body text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resp_body_regex: Option<String>,
    /// A condition on a request header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub req_header: Option<HeaderCond>,
    /// A condition on a response header.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resp_header: Option<HeaderCond>,
    /// A status-code range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusCond>,
    /// A request parameter whose value looks like something.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<ParamCond>,
}

impl When {
    fn is_empty(&self) -> bool {
        self.method.is_empty()
            && self.req_content_type.is_none()
            && self.resp_content_type.is_none()
            && self.path_regex.is_none()
            && self.req_body_regex.is_none()
            && self.resp_body_regex.is_none()
            && self.req_header.is_none()
            && self.resp_header.is_none()
            && self.status.is_none()
            && self.param.is_none()
    }

    /// Every regex pattern in this condition, for validation.
    fn patterns(&self) -> Vec<(&str, &str)> {
        let mut out = vec![];
        if let Some(p) = &self.path_regex {
            out.push(("path_regex", p.as_str()));
        }
        if let Some(p) = &self.req_body_regex {
            out.push(("req_body_regex", p.as_str()));
        }
        if let Some(p) = &self.resp_body_regex {
            out.push(("resp_body_regex", p.as_str()));
        }
        for (field, cond) in [("req_header", &self.req_header), ("resp_header", &self.resp_header)] {
            if let Some(c) = cond {
                if let Some(p) = &c.regex {
                    out.push((field, p.as_str()));
                }
                if let Some(p) = &c.absent_regex {
                    out.push((field, p.as_str()));
                }
            }
        }
        out
    }
}

/// A header condition: the header must exist, and, if given, a value must
/// (`contains`/`regex`) or must not (`absent_regex`) match.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeaderCond {
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    /// The header is present, but at least one value does not match this
    /// (e.g. a `Set-Cookie` that lacks `HttpOnly`, even when another has it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absent_regex: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StatusCond {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamCond {
    /// One of [`VALUE_CLASSES`].
    pub value_class: String,
    /// Where to look: `query`, `body`, or both. Empty: both.
    #[serde(rename = "in", default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<String>,
}

/// What a match offers. `handler` names the tab it hands off to; the rest are
/// the fields that handler reads. Chip and text may use `{method}`, `{path}`,
/// `{param}`, `{value}` and `{ct}` placeholders, filled in by the app.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suggest {
    /// Chip label, e.g. "Scan this XML endpoint".
    pub chip: String,
    /// One of [`HANDLERS`].
    pub handler: String,
    /// `scan`: the focus label shown in the Scans banner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
    /// `scan`: OWASP categories to pre-pick, e.g. `["A03"]`. An empty array or
    /// omitting it pre-picks nothing — the researcher chooses in Scans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
    /// `scan`/`finding`: a short title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `scan`: why this is worth a look.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// `scan`/`bench`/`finding`: a note shown to the researcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// `finding`: a pre-set severity, one of [`SEVERITIES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    /// `access`: one of [`ACCESS_MODES`]. Default `users`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectorPackInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    pub detectors: usize,
    pub sha256: String,
    pub source: String,
    pub builtin: bool,
}

#[derive(Debug, Clone)]
pub struct DetectorPack {
    pub doc: DetectorPackDoc,
    pub sha256: String,
}

impl DetectorPack {
    pub fn info(&self, source: &str, builtin: bool) -> DetectorPackInfo {
        DetectorPackInfo {
            name: self.doc.name.clone(),
            version: self.doc.version.clone(),
            description: self.doc.description.clone(),
            author: self.doc.author.clone(),
            detectors: self.doc.detectors.len(),
            sha256: self.sha256.clone(),
            source: source.to_string(),
            builtin,
        }
    }
}

fn check_detector_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 40
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { Ok(()) } else { Err(format!("`{}` must be 1-40 of [a-z0-9-], starting with a letter or digit", clean(id, 40))) }
}

fn check_pattern(field: &str, p: &str) -> Result<(), String> {
    if p.is_empty() || p.len() > MAX_PATTERN {
        return Err(format!("{field}: pattern must be 1-{MAX_PATTERN} characters"));
    }
    RegexBuilder::new(p)
        .case_insensitive(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT * 4)
        .build()
        .map(|_| ())
        .map_err(|e| format!("{field}: invalid pattern: {}", clean(&e.to_string(), 200)))
}

fn one_of(field: &str, value: &str, allowed: &[&str]) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!("{field}: `{}` is not one of {}", clean(value, 40), allowed.join(", ")))
    }
}

fn check_header_cond(field: &str, c: &HeaderCond) -> Vec<String> {
    let mut errs = vec![];
    if let Err(e) = check_text(&c.name, 100, false) {
        errs.push(format!("{field}: name: {e}"));
    }
    if let Some(s) = &c.contains {
        if let Err(e) = check_text(s, 200, false) {
            errs.push(format!("{field}: contains: {e}"));
        }
    }
    if !c.present && c.contains.is_none() && c.regex.is_none() && c.absent_regex.is_none() {
        errs.push(format!("{field}: needs `present`, `contains`, `regex` or `absent_regex`"));
    }
    errs
}

fn check_suggest(s: &Suggest) -> Vec<String> {
    let mut errs = vec![];
    if let Err(e) = check_text(&s.chip, 60, false) {
        errs.push(format!("suggest.chip: {e}"));
    }
    if let Err(e) = one_of("suggest.handler", &s.handler, HANDLERS) {
        errs.push(e);
    }
    for (field, value, max) in [
        ("suggest.focus", &s.focus, 60),
        ("suggest.title", &s.title, 140),
        ("suggest.why", &s.why, 600),
        ("suggest.note", &s.note, 600),
    ] {
        if let Some(v) = value {
            if let Err(e) = check_text(v, max, false) {
                errs.push(format!("{field}: {e}"));
            }
        }
    }
    if let Some(cats) = &s.categories {
        if cats.len() > 20 {
            errs.push("suggest.categories: at most 20".into());
        }
        for c in cats {
            let ok = !c.is_empty() && c.len() <= 12 && c.bytes().all(|b| b.is_ascii_alphanumeric());
            if !ok {
                errs.push(format!("suggest.categories: `{}` must be 1-12 alphanumeric characters", clean(c, 12)));
            }
        }
    }
    if let Some(sev) = &s.severity {
        if let Err(e) = one_of("suggest.severity", sev, SEVERITIES) {
            errs.push(e);
        }
    }
    if let Some(m) = &s.mode {
        if let Err(e) = one_of("suggest.mode", m, ACCESS_MODES) {
            errs.push(e);
        }
    }
    // Each handler needs the fields it reads, so a chip is never blank.
    match s.handler.as_str() {
        "scan" => {
            if s.focus.is_none() {
                errs.push("suggest.focus: a `scan` handler needs a focus label".into());
            }
            if s.title.is_none() {
                errs.push("suggest.title: a `scan` handler needs a title".into());
            }
            if s.why.is_none() {
                errs.push("suggest.why: a `scan` handler needs a why".into());
            }
        }
        "finding" => {
            if s.title.is_none() {
                errs.push("suggest.title: a `finding` handler needs a title".into());
            }
        }
        "bench" => {
            if s.note.is_none() {
                errs.push("suggest.note: a `bench` handler needs a note".into());
            }
        }
        _ => {}
    }
    errs
}

/// Parses and validates a detector pack from untrusted bytes. Reports every problem.
pub fn parse(bytes: &[u8]) -> Result<DetectorPack, String> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(format!("pack is {} bytes; the limit is {MAX_PACK_BYTES}", bytes.len()));
    }
    let doc: DetectorPackDoc =
        serde_json::from_slice(bytes).map_err(|e| format!("not a valid detector pack: {}", clean(&e.to_string(), 300)))?;
    let mut errors = vec![];
    if doc.plonix_detectors != FORMAT_VERSION {
        errors.push(format!("plonix_detectors: format {} is not supported (this Plonix reads format {FORMAT_VERSION})", doc.plonix_detectors));
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
    if doc.detectors.is_empty() {
        errors.push("detectors: a pack needs at least one detector".into());
    }
    if doc.detectors.len() > MAX_DETECTORS_PER_PACK {
        errors.push(format!("detectors: at most {MAX_DETECTORS_PER_PACK} detectors per pack"));
    }
    let mut seen = BTreeMap::new();
    for (i, d) in doc.detectors.iter().take(MAX_DETECTORS_PER_PACK).enumerate() {
        let at = format!("detectors[{i}] ({})", clean(&d.id, 40));
        if let Err(e) = check_detector_id(&d.id) {
            errors.push(format!("{at}: id: {e}"));
        }
        if let Some(prev) = seen.insert(d.id.clone(), i) {
            errors.push(format!("{at}: duplicate id, also used by detectors[{prev}]"));
        }
        if d.when.is_empty() {
            errors.push(format!("{at}: when: needs at least one condition, or it matches everything"));
        }
        for m in &d.when.method {
            let ok = !m.is_empty() && m.len() <= 10 && m.bytes().all(|b| b.is_ascii_alphabetic());
            if !ok {
                errors.push(format!("{at}: when.method: `{}` is not a method name", clean(m, 10)));
            }
        }
        for (field, pat) in d.when.patterns() {
            if let Err(e) = check_pattern(&format!("{at}: when.{field}"), pat) {
                errors.push(e);
            }
        }
        for (field, ct) in [("req_content_type", &d.when.req_content_type), ("resp_content_type", &d.when.resp_content_type)] {
            if let Some(v) = ct {
                if let Err(e) = check_text(v, 100, false) {
                    errors.push(format!("{at}: when.{field}: {e}"));
                }
            }
        }
        for (field, cond) in [("req_header", &d.when.req_header), ("resp_header", &d.when.resp_header)] {
            if let Some(c) = cond {
                for e in check_header_cond(&format!("{at}: when.{field}"), c) {
                    errors.push(e);
                }
            }
        }
        if let Some(p) = &d.when.param {
            if let Err(e) = one_of(&format!("{at}: when.param.value_class"), &p.value_class, VALUE_CLASSES) {
                errors.push(e);
            }
            for place in &p.places {
                if let Err(e) = one_of(&format!("{at}: when.param.in"), place, PARAM_PLACES) {
                    errors.push(e);
                }
            }
        }
        for e in check_suggest(&d.suggest) {
            errors.push(format!("{at}: {e}"));
        }
    }
    if !errors.is_empty() {
        return Err(format!("invalid detector pack:\n  - {}", errors.join("\n  - ")));
    }
    Ok(DetectorPack { doc, sha256: sha256_hex(bytes) })
}

/// A detector as served to the app, tagged with the pack it came from.
#[derive(Debug, Clone, Serialize)]
pub struct ServedDetector {
    #[serde(flatten)]
    pub def: DetectorDef,
    pub pack: String,
}

/// Every detector in effect, highest priority first.
#[derive(Debug, Default, Clone)]
pub struct DetectorSet {
    pub detectors: Vec<ServedDetector>,
    pub packs: Vec<DetectorPackInfo>,
    pub problems: Vec<String>,
}

impl DetectorSet {
    fn add(&mut self, pack: &DetectorPack, info: DetectorPackInfo) {
        for d in &pack.doc.detectors {
            // First pack wins on a duplicate id: built-ins, then installed by name.
            if let Some(existing) = self.detectors.iter().find(|s| s.def.id == d.id) {
                self.problems.push(format!("detector {} from {} is ignored: {} already defines it", d.id, pack.doc.name, existing.pack));
                continue;
            }
            self.detectors.push(ServedDetector { def: d.clone(), pack: pack.doc.name.clone() });
        }
        self.packs.push(info);
    }

    fn sort(&mut self) {
        self.detectors.sort_by(|a, b| b.def.priority.cmp(&a.def.priority).then_with(|| a.def.id.cmp(&b.def.id)));
    }
}

/// Detector packs installed in a Plonix home, plus the built-in one.
pub struct DetectorLibrary {
    shelf: Shelf,
}

impl DetectorLibrary {
    pub fn new(home: &Home) -> Self {
        Self::at(&home.root.join("detectors"))
    }

    pub fn at(dir: &Path) -> Self {
        Self { shelf: Shelf::new(dir, "detector pack", "detectors", MAX_INSTALLED_PACKS) }
    }

    pub fn stamp(&self) -> Option<std::time::SystemTime> {
        self.shelf.stamp()
    }

    pub fn install(&self, bytes: &[u8], source: &str, expected_sha256: Option<&str>) -> Result<(DetectorPack, Option<String>)> {
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

    pub fn load(&self) -> DetectorSet {
        let mut set = DetectorSet::default();
        for (name, text) in BUILTIN {
            match parse(text.as_bytes()) {
                Ok(p) => {
                    let info = p.info("built-in", true);
                    set.add(&p, info);
                }
                Err(e) => set.problems.push(format!("built-in detector pack {name}: {e}")),
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
                Ok(_) => set.problems.push(format!("detector pack {}: name inside the file does not match", v.name)),
                Err(e) => set.problems.push(format!("detector pack {}: {e}", v.name)),
            }
        }
        set.sort();
        set
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACK: &str = r#"{"plonix_detectors":1,"name":"acme","version":"1.0.0","description":"Acme detectors","author":"red team",
        "detectors":[{"id":"acme-xml","when":{"req_content_type":"xml"},
            "suggest":{"chip":"Scan this XML endpoint","handler":"scan","focus":"XML parsing","title":"XML on this endpoint","why":"It parses XML."}}]}"#;

    #[test]
    fn builtin_pack_is_valid() {
        let set = DetectorLibrary::at(Path::new("/nonexistent")).load();
        assert!(set.problems.is_empty(), "{:?}", set.problems);
        assert!(set.detectors.len() >= 8, "{:?}", set.detectors.iter().map(|d| &d.def.id).collect::<Vec<_>>());
        for id in ["path-traversal", "xml-parsing", "jwt-token", "cookie-no-httponly", "admin-area", "db-error", "graphql-endpoint", "mass-assignment"] {
            assert!(set.detectors.iter().any(|d| d.def.id == id), "missing {id}");
        }
        // Every built-in handler is one the app knows.
        for d in &set.detectors {
            assert!(HANDLERS.contains(&d.def.suggest.handler.as_str()), "bad handler {}", d.def.suggest.handler);
        }
        // Sorted by priority, highest first.
        let prios: Vec<i32> = set.detectors.iter().map(|d| d.def.priority).collect();
        assert!(prios.windows(2).all(|w| w[0] >= w[1]), "not sorted: {prios:?}");
    }

    #[test]
    fn install_load_remove_and_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let lib = DetectorLibrary::at(dir.path());
        assert!(lib.install(PACK.as_bytes(), "x", Some(&"0".repeat(64))).unwrap_err().to_string().contains("checksum mismatch"));
        lib.install(PACK.as_bytes(), "./acme.json", None).unwrap();
        let set = lib.load();
        assert!(set.detectors.iter().any(|d| d.def.id == "acme-xml" && d.pack == "acme"));

        std::fs::write(dir.path().join("packs/acme.json"), PACK.replace("XML parsing", "evil")).unwrap();
        let set = lib.load();
        assert!(!set.detectors.iter().any(|d| d.def.id == "acme-xml"));
        assert!(set.problems.iter().any(|p| p.contains("checksum mismatch")), "{:?}", set.problems);

        assert!(lib.remove("acme").unwrap());
        assert!(lib.remove("mind-reader").is_err());
    }

    #[test]
    fn later_packs_cannot_redefine_a_detector() {
        let dir = tempfile::tempdir().unwrap();
        let lib = DetectorLibrary::at(dir.path());
        let shadow = PACK.replace("acme-xml", "jwt-token");
        lib.install(shadow.as_bytes(), "x", None).unwrap();
        let set = lib.load();
        let jwt = set.detectors.iter().find(|d| d.def.id == "jwt-token").unwrap();
        assert_eq!(jwt.pack, "mind-reader");
        assert!(set.problems.iter().any(|p| p.contains("detector jwt-token from acme is ignored")));
    }

    #[test]
    fn invalid_packs_report_every_problem() {
        let bad = r#"{"plonix_detectors":1,"name":"bad","version":"1.0.0","description":"d","author":"a","detectors":[
            {"id":"Loud","when":{},"suggest":{"chip":"x","handler":"scan"}},
            {"id":"bad-re","when":{"path_regex":"("},"suggest":{"chip":"x","handler":"bench","note":"n"}},
            {"id":"bad-handler","when":{"status":{"min":200}},"suggest":{"chip":"x","handler":"explode"}},
            {"id":"bad-class","when":{"param":{"value_class":"ssn"}},"suggest":{"chip":"x","handler":"finding","title":"t"}},
            {"id":"bad-class","when":{"status":{"min":200}},"suggest":{"chip":"x","handler":"access"}}]}"#;
        let err = parse(bad.as_bytes()).unwrap_err();
        for want in [
            "detectors[0] (Loud): id",
            "when: needs at least one condition",
            "a `scan` handler needs a focus",
            "when.path_regex: invalid pattern",
            "suggest.handler: `explode`",
            "when.param.value_class: `ssn`",
            "duplicate id",
        ] {
            assert!(err.contains(want), "missing `{want}` in:\n{err}");
        }
        // No arbitrary code fields.
        assert!(parse(PACK.replace("\"suggest\"", "\"run\":\"sh\",\"suggest\"").as_bytes()).unwrap_err().contains("unknown field"));
        assert!(parse(&vec![b' '; MAX_PACK_BYTES + 1]).is_err());
    }
}
