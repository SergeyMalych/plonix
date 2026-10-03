//! Scanning: detectors decide relevance, tactics do the checks.
//!
//! The design is in `docs/scanning.md`. Two things are kept deliberately
//! apart (see also `detect.rs` for technology fingerprinting, which this
//! builds on):
//!
//! * A **detector** is passive. It reads only data Plonix already has —
//!   detected technologies and captured exchanges — and, when a class of
//!   checks is relevant, emits a **signal** (`jwt-present`, `php`, …) backed
//!   by evidence. A detector never sends a request, so detectors are
//!   declarative and ship in packs, matched with the exact same grammar and
//!   limits as detection rules (`detect::ConditionSet`).
//! * A **tactic** is an active check. It is **gated** by one or more signals
//!   (`requires`) and is selected only when every gating signal is active for
//!   a target. That is what makes scanning smart: a JWT tactic requires
//!   `jwt-present`, so it is never selected for an app with no JWT; a
//!   PHP-oriented tactic requires `php`, so it is skipped on a Node.js site.
//!
//! This module is the data model and the *selection* logic — pure functions
//! over already-captured data. Actually sending a tactic's requests is the
//! scan runner's job, and every such request goes through the engine's single
//! scope choke point (`engine::send`), so a scan can only ever reach a host
//! the user has accepted into scope.

use std::collections::BTreeSet;

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::detect::{self, ConditionDef, ConditionSet, Detection, MatchMode, check_id, check_text, clean};
use crate::model::Exchange;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_PACK_BYTES: usize = 1024 * 1024;
pub const MAX_DETECTORS: usize = 500;
pub const MAX_TACTICS: usize = 500;
pub const MAX_REQUIRES: usize = 8;
pub const MAX_TECH: usize = 16;
pub const MAX_PAYLOADS: usize = 64;
pub const MAX_PAYLOAD_LEN: usize = 512;
const MAX_PATTERN: usize = 1000;
const REGEX_SIZE_LIMIT: usize = 256 * 1024;

const METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

/// Scan packs shipped with Plonix, so detectors and a few benign checks work
/// before anything is installed. Kept to relevance logic and clearly
/// non-destructive checks.
pub const BUILTIN: &[(&str, &str)] = &[
    ("baseline", include_str!("../../../store/scanpacks/baseline.json")),
    ("probes", include_str!("../../../store/scanpacks/probes.json")),
];

/// How serious a finding from a tactic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

/// How much a tactic touches the target. The runner treats `Intrusive` as
/// off by default; the user turns it on per scan, deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Intrusiveness {
    /// No new requests: judges only traffic already captured.
    Passive,
    /// Well-formed, read-only requests a browser could make.
    Safe,
    /// Crafted but non-destructive requests.
    Active,
    /// Potentially heavier or state-touching. Off by default.
    Intrusive,
}

impl Intrusiveness {
    /// Whether this level is recommended on by default in a suggested profile.
    pub fn default_on(self) -> bool {
        !matches!(self, Intrusiveness::Intrusive)
    }
}

/// A detector as written in a pack. It activates when any listed technology
/// is detected on the target, or when its traffic conditions match. At least
/// one of `tech`/`conditions` must be set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectorDef {
    /// Stable id, e.g. `jwt`.
    pub id: String,
    /// The signal this detector emits when active, e.g. `jwt-present`.
    pub signal: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Detected technology ids that activate this detector (`detect.rs` ids).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tech: Vec<String>,
    /// Traffic conditions, using the detection-rule grammar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditions: Vec<ConditionDef>,
    /// `any` (default) or `all`, for the conditions.
    #[serde(default, rename = "match")]
    pub mode: MatchMode,
}

/// Where a tactic places its payload, relative to a discovered target.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectLocation {
    /// Send the target as-is (a path probe).
    #[default]
    None,
    /// Into a query-string parameter.
    Query,
    /// As a request header value.
    Header,
    /// Appended to the request path.
    PathSuffix,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InjectDef {
    #[serde(default)]
    pub location: InjectLocation,
    /// Parameter or header name, when the location needs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// What in a response indicates the finding. At least one of the three must
/// be set.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectDef {
    /// Any of these response status codes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub status: Vec<u16>,
    /// A regex that must match somewhere in the decoded response body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The response body contains the payload that was sent.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reflects_payload: bool,
}

/// A tactic's bounded request plan plus how to judge the response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckDef {
    #[serde(default = "get_method")]
    pub method: String,
    /// A fixed path to probe on the target host, e.g. `/.well-known/security.txt`.
    /// Mutually exclusive with `inject`: a tactic either probes a fixed path
    /// once per host, or mutates discovered endpoints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub inject: InjectDef,
    /// A small, fixed set of payload tokens from the pack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub payloads: Vec<String>,
    pub expect: ExpectDef,
}

fn get_method() -> String {
    "GET".into()
}

/// A tactic as written in a pack.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TacticDef {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Signal ids that gate this tactic. Every one must be active for a
    /// target before the tactic is selected.
    pub requires: Vec<String>,
    pub severity: Severity,
    /// Required: the author must declare how intrusive the check is.
    pub intrusiveness: Intrusiveness,
    /// Variant label, so one logical check can carry several request shapes
    /// that update independently.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub variant: String,
    pub check: CheckDef,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub remediation: String,
}

/// The pack document: `plonix_scanpack: 1`, alongside detection rule packs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanPackDoc {
    pub plonix_scanpack: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub detectors: Vec<DetectorDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tactics: Vec<TacticDef>,
}

/// A compiled, validated detector.
#[derive(Debug, Clone)]
pub struct Detector {
    pub def: DetectorDef,
    pub pack: String,
    conditions: Option<ConditionSet>,
}

/// A compiled, validated tactic.
#[derive(Debug, Clone)]
pub struct Tactic {
    pub def: TacticDef,
    pub pack: String,
    body_re: Option<Regex>,
}

impl Tactic {
    /// Compiled body regex from `expect.body`, for the runner.
    pub fn body_regex(&self) -> Option<&Regex> {
        self.body_re.as_ref()
    }
}

/// A signal that is active for a target, with the evidence behind it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveSignal {
    pub signal: String,
    pub detector: String,
    pub evidence: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchange_id: Option<i64>,
}

/// A tactic selected to run against a target.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelectedTactic {
    pub id: String,
    pub title: String,
    pub severity: Severity,
    pub intrusiveness: Intrusiveness,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub variant: String,
    pub pack: String,
    /// The gating signals (all active) that selected this tactic.
    pub requires: Vec<String>,
}

/// A fingerprint-driven suggestion for scanning one target.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanSuggestion {
    /// Signals the detectors found active, with evidence.
    pub signals: Vec<ActiveSignal>,
    /// Applicable, non-intrusive tactics: recommended on by default.
    pub recommended: Vec<SelectedTactic>,
    /// Applicable but intrusive tactics: off by default, shown for opt-in.
    pub optional: Vec<SelectedTactic>,
    /// How many tactics were skipped because their signals were not active.
    pub skipped: usize,
}

/// What a user asks for when starting an active scan. The host must already
/// be accepted into scope; the runner refuses otherwise.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScanRequest {
    pub host: String,
    /// Explicit tactic ids to run. Empty means every applicable tactic the
    /// profile would recommend (non-intrusive, plus intrusive only when
    /// `include_intrusive` is set).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tactics: Vec<String>,
    /// Allow intrusive tactics. Off by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub include_intrusive: bool,
    /// A ceiling on how many requests the scan may send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_requests: Option<usize>,
}

/// A finding a scan recorded.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanFindingRef {
    pub id: i64,
    pub title: String,
    pub severity: Severity,
}

/// The outcome of an active scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScanReport {
    pub host: String,
    pub signals: Vec<ActiveSignal>,
    /// Ids of the tactics that actually ran.
    pub tactics_run: Vec<String>,
    /// How many requests the scan sent (all through the scope choke point).
    pub requests_sent: usize,
    pub findings: Vec<ScanFindingRef>,
    /// Human-readable notes (e.g. a budget was reached).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The default ceiling on requests a single scan may send, so a scan is always
/// bounded even if a pack is broad.
pub const DEFAULT_REQUEST_BUDGET: usize = 500;

/// The compiled set of detectors and tactics from all loaded scan packs.
#[derive(Debug, Default, Clone)]
pub struct Catalog {
    pub detectors: Vec<Detector>,
    pub tactics: Vec<Tactic>,
}

/// A serializable summary of a detector, for the catalog view.
#[derive(Debug, Clone, Serialize)]
pub struct DetectorInfo {
    pub id: String,
    pub signal: String,
    pub description: String,
    pub pack: String,
}

/// A serializable summary of a tactic, for the catalog view.
#[derive(Debug, Clone, Serialize)]
pub struct TacticInfo {
    pub id: String,
    pub title: String,
    pub description: String,
    pub requires: Vec<String>,
    pub severity: Severity,
    pub intrusiveness: Intrusiveness,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub variant: String,
    pub pack: String,
}

/// The whole catalog, described for a client (the Scans UI, an advising agent).
#[derive(Debug, Clone, Serialize)]
pub struct CatalogView {
    pub detectors: Vec<DetectorInfo>,
    pub tactics: Vec<TacticInfo>,
}

impl Catalog {
    /// A serializable description of every detector and tactic.
    pub fn describe(&self) -> CatalogView {
        CatalogView {
            detectors: self
                .detectors
                .iter()
                .map(|d| DetectorInfo { id: d.def.id.clone(), signal: d.def.signal.clone(), description: d.def.description.clone(), pack: d.pack.clone() })
                .collect(),
            tactics: self
                .tactics
                .iter()
                .map(|t| TacticInfo {
                    id: t.def.id.clone(),
                    title: t.def.title.clone(),
                    description: t.def.description.clone(),
                    requires: t.def.requires.clone(),
                    severity: t.def.severity,
                    intrusiveness: t.def.intrusiveness,
                    variant: t.def.variant.clone(),
                    pack: t.pack.clone(),
                })
                .collect(),
        }
    }

    /// Signals active for a target, from its detected technologies and
    /// captured exchanges. Pure: reads only what it is given.
    pub fn signals(&self, tech: &[Detection], exchanges: &[Exchange]) -> Vec<ActiveSignal> {
        let tech_ids: BTreeSet<&str> = tech.iter().map(|d| d.id.as_str()).collect();
        let mut out: Vec<ActiveSignal> = Vec::new();
        for d in &self.detectors {
            if out.iter().any(|s| s.signal == d.def.signal) {
                continue; // a signal, once active, needs no second detector
            }
            if let Some(sig) = d.evaluate(&tech_ids, exchanges) {
                out.push(sig);
            }
        }
        out
    }

    /// Tactics whose required signals are all in `active`.
    pub fn select(&self, active: &BTreeSet<String>) -> Vec<SelectedTactic> {
        self.tactics
            .iter()
            .filter(|t| t.def.requires.iter().all(|r| active.contains(r)))
            .map(|t| SelectedTactic {
                id: t.def.id.clone(),
                title: t.def.title.clone(),
                severity: t.def.severity,
                intrusiveness: t.def.intrusiveness,
                variant: t.def.variant.clone(),
                pack: t.pack.clone(),
                requires: t.def.requires.clone(),
            })
            .collect()
    }

    /// The full suggestion for a target: active signals, the tactics they
    /// gate split into recommended (non-intrusive) and optional (intrusive),
    /// and how many tactics did not apply.
    pub fn suggest(&self, tech: &[Detection], exchanges: &[Exchange]) -> ScanSuggestion {
        let signals = self.signals(tech, exchanges);
        let active: BTreeSet<String> = signals.iter().map(|s| s.signal.clone()).collect();
        let selected = self.select(&active);
        let selected_ids: BTreeSet<&str> = selected.iter().map(|s| s.id.as_str()).collect();
        let skipped = self.tactics.iter().filter(|t| !selected_ids.contains(t.def.id.as_str())).count();
        let (recommended, optional): (Vec<_>, Vec<_>) = selected.into_iter().partition(|t| t.intrusiveness.default_on());
        ScanSuggestion { signals, recommended, optional, skipped }
    }

    /// Signals that some tactic requires but no detector emits. A diagnostic
    /// for pack authors; not fatal, since a signal may come from another pack.
    pub fn unmet_requirements(&self) -> Vec<String> {
        let emitted: BTreeSet<&str> = self.detectors.iter().map(|d| d.def.signal.as_str()).collect();
        let mut unmet: BTreeSet<String> = BTreeSet::new();
        for t in &self.tactics {
            for r in &t.def.requires {
                if !emitted.contains(r.as_str()) {
                    unmet.insert(r.clone());
                }
            }
        }
        unmet.into_iter().collect()
    }
}

impl Detector {
    /// Whether this detector is active, and the evidence if so.
    fn evaluate(&self, tech_ids: &BTreeSet<&str>, exchanges: &[Exchange]) -> Option<ActiveSignal> {
        for t in &self.def.tech {
            if tech_ids.contains(t.as_str()) {
                return Some(ActiveSignal {
                    signal: self.def.signal.clone(),
                    detector: self.def.id.clone(),
                    evidence: format!("technology {} detected", clean(t, 64)),
                    exchange_id: None,
                });
            }
        }
        if let Some(cs) = &self.conditions {
            if let Some(hits) = cs.evaluate(exchanges) {
                let (id, ev) = hits.into_iter().next().unwrap_or((0, String::new()));
                return Some(ActiveSignal {
                    signal: self.def.signal.clone(),
                    detector: self.def.id.clone(),
                    evidence: ev,
                    exchange_id: (id != 0).then_some(id),
                });
            }
        }
        None
    }
}

/// Validates and compiles a detector. Errors name the offending field.
pub fn compile_detector(def: DetectorDef, pack: &str) -> Result<Detector, String> {
    check_id(&def.id).map_err(|e| format!("id: {e}"))?;
    check_id(&def.signal).map_err(|e| format!("signal: {e}"))?;
    check_text(&def.description, 500, true).map_err(|e| format!("description: {e}"))?;
    if def.tech.is_empty() && def.conditions.is_empty() {
        return Err("a detector needs at least one of `tech` or `conditions`".into());
    }
    if def.tech.len() > MAX_TECH {
        return Err(format!("tech: at most {MAX_TECH} entries"));
    }
    for (i, t) in def.tech.iter().enumerate() {
        check_id(t).map_err(|e| format!("tech[{i}]: {e}"))?;
    }
    let conditions =
        if def.conditions.is_empty() { None } else { Some(ConditionSet::compile(&def.conditions, def.mode)?) };
    Ok(Detector { def, pack: pack.to_string(), conditions })
}

/// Validates and compiles a tactic. Errors name the offending field.
pub fn compile_tactic(def: TacticDef, pack: &str) -> Result<Tactic, String> {
    check_id(&def.id).map_err(|e| format!("id: {e}"))?;
    check_text(&def.title, 120, false).map_err(|e| format!("title: {e}"))?;
    check_text(&def.description, 2000, true).map_err(|e| format!("description: {e}"))?;
    check_text(&def.remediation, 2000, true).map_err(|e| format!("remediation: {e}"))?;
    check_text(&def.variant, 64, true).map_err(|e| format!("variant: {e}"))?;
    if def.requires.is_empty() {
        return Err("requires: a tactic must be gated by at least one signal".into());
    }
    if def.requires.len() > MAX_REQUIRES {
        return Err(format!("requires: at most {MAX_REQUIRES} signals"));
    }
    for (i, r) in def.requires.iter().enumerate() {
        check_id(r).map_err(|e| format!("requires[{i}]: {e}"))?;
    }
    let method = def.check.method.to_ascii_uppercase();
    if !METHODS.contains(&method.as_str()) {
        return Err(format!("check.method: `{}` is not one of {}", clean(&def.check.method, 16), METHODS.join(", ")));
    }
    if def.check.payloads.len() > MAX_PAYLOADS {
        return Err(format!("check.payloads: at most {MAX_PAYLOADS} payloads"));
    }
    for (i, p) in def.check.payloads.iter().enumerate() {
        if p.len() > MAX_PAYLOAD_LEN {
            return Err(format!("check.payloads[{i}]: longer than {MAX_PAYLOAD_LEN} bytes"));
        }
    }
    let injecting = !matches!(def.check.inject.location, InjectLocation::None);
    if let Some(p) = &def.check.path {
        if injecting {
            return Err("check: set either `path` (probe a fixed path) or `inject` (mutate endpoints), not both".into());
        }
        if !p.starts_with('/') || p.len() > 1024 || p.bytes().any(|b| b.is_ascii_whitespace() || b.is_ascii_control()) {
            return Err("check.path: an absolute path (starting with `/`), up to 1024 bytes, no whitespace".into());
        }
    }
    match def.check.inject.location {
        InjectLocation::Query | InjectLocation::Header => {
            let name = def
                .check
                .inject
                .name
                .as_deref()
                .filter(|n| !n.is_empty())
                .ok_or("check.inject.name: required for query and header injection")?;
            if name.len() > 128 || !name.bytes().all(|b| b.is_ascii_graphic() && b != b':' && b != b'=' && b != b';') {
                return Err("check.inject.name: 1-128 printable characters without `:`, `=` or `;`".into());
            }
        }
        InjectLocation::None | InjectLocation::PathSuffix => {}
    }
    if injecting && def.check.payloads.is_empty() {
        return Err("check.payloads: an injection location needs at least one payload".into());
    }
    let e = &def.check.expect;
    if e.status.is_empty() && e.body.is_none() && !e.reflects_payload {
        return Err("check.expect: set at least one of status, body or reflects_payload".into());
    }
    for s in &e.status {
        if !(100..=599).contains(s) {
            return Err(format!("check.expect.status: {s} is not a valid status code"));
        }
    }
    let body_re = e.body.as_deref().map(build_regex).transpose().map_err(|e| format!("check.expect.body: {e}"))?;
    Ok(Tactic { def, pack: pack.to_string(), body_re })
}

/// Parses and validates one scan-pack document into compiled detectors and
/// tactics. The bytes are untrusted: size-limited, strictly schema-checked
/// (unknown fields are errors), and every regex and payload is bounded.
pub fn parse_pack(bytes: &[u8], source: &str) -> Result<(ScanPackDoc, Vec<Detector>, Vec<Tactic>), String> {
    if bytes.len() > MAX_PACK_BYTES {
        return Err(format!("{source}: scan pack is larger than {} bytes", MAX_PACK_BYTES));
    }
    let doc: ScanPackDoc =
        serde_json::from_slice(bytes).map_err(|e| format!("{source}: invalid scan pack: {}", clean(&e.to_string(), 300)))?;
    if doc.plonix_scanpack != FORMAT_VERSION {
        return Err(format!("{source}: plonix_scanpack: format {} is not supported", doc.plonix_scanpack));
    }
    if doc.detectors.len() > MAX_DETECTORS {
        return Err(format!("{source}: at most {MAX_DETECTORS} detectors per pack"));
    }
    if doc.tactics.len() > MAX_TACTICS {
        return Err(format!("{source}: at most {MAX_TACTICS} tactics per pack"));
    }
    if doc.detectors.is_empty() && doc.tactics.is_empty() {
        return Err(format!("{source}: a scan pack must define at least one detector or tactic"));
    }
    check_text(&doc.description, 300, false).map_err(|e| format!("{source}: description: {e}"))?;
    check_text(&doc.author, 100, false).map_err(|e| format!("{source}: author: {e}"))?;
    let mut detectors = Vec::with_capacity(doc.detectors.len());
    let mut seen = BTreeSet::new();
    for d in &doc.detectors {
        if !seen.insert(d.id.clone()) {
            return Err(format!("{source}: detector id `{}` is listed twice", clean(&d.id, 64)));
        }
        detectors.push(compile_detector(d.clone(), &doc.name).map_err(|e| format!("{source}: detector `{}`: {e}", clean(&d.id, 64)))?);
    }
    let mut tactics = Vec::with_capacity(doc.tactics.len());
    seen.clear();
    for t in &doc.tactics {
        if !seen.insert(t.id.clone()) {
            return Err(format!("{source}: tactic id `{}` is listed twice", clean(&t.id, 64)));
        }
        tactics.push(compile_tactic(t.clone(), &doc.name).map_err(|e| format!("{source}: tactic `{}`: {e}", clean(&t.id, 64)))?);
    }
    Ok((doc, detectors, tactics))
}

/// The catalog built from the packs shipped with Plonix, so detectors work
/// before anything is installed. Panics only on a build bug (a malformed
/// built-in pack), which the tests catch.
pub fn builtin_catalog() -> Catalog {
    let mut cat = Catalog::default();
    for (name, body) in BUILTIN {
        let (_, mut d, mut t) = parse_pack(body.as_bytes(), name).unwrap_or_else(|e| panic!("built-in scan pack `{name}` is invalid: {e}"));
        cat.detectors.append(&mut d);
        cat.tactics.append(&mut t);
    }
    cat
}

/// A target the runner can aim a tactic at: a discovered endpoint on a host.
#[derive(Debug, Clone)]
pub struct ScanTarget {
    pub method: String,
    pub path: String,
}

/// One concrete request a tactic wants sent, already resolved against a host
/// and target. The runner hands `url` straight to the engine's `send`, so the
/// engine's scope check still applies — this struct grants no reach of its own.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    /// The payload this request carries, for the reflection check and evidence.
    pub payload: Option<String>,
}

/// A finding a tactic produced, ready to record once a person's scan recorded it.
#[derive(Debug, Clone, PartialEq)]
pub struct FindingDraft {
    pub title: String,
    pub severity: Severity,
    pub description: String,
    pub exchange_id: i64,
}

fn encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Tactic {
    /// The concrete requests this tactic makes against one host. For a
    /// fixed-path tactic, `target` is ignored and one request is planned. For
    /// an injecting tactic, one request per payload is planned against the
    /// discovered `target` (none planned if there is no target to mutate).
    /// Every URL still goes through the engine's scope check when sent.
    pub fn plan(&self, scheme: &str, host: &str, target: Option<&ScanTarget>) -> Vec<PlannedRequest> {
        let base = format!("{scheme}://{host}");
        let method = self.def.check.method.to_ascii_uppercase();
        if let Some(path) = &self.def.check.path {
            return vec![PlannedRequest { method, url: format!("{base}{path}"), headers: vec![], payload: None }];
        }
        let Some(t) = target else { return vec![] };
        let path = if t.path.starts_with('/') { t.path.clone() } else { format!("/{}", t.path) };
        match self.def.check.inject.location {
            InjectLocation::None => {
                vec![PlannedRequest { method, url: format!("{base}{path}"), headers: vec![], payload: None }]
            }
            InjectLocation::PathSuffix => self
                .def
                .check
                .payloads
                .iter()
                .map(|p| PlannedRequest { method: method.clone(), url: format!("{base}{path}{}", encode_component(p)), headers: vec![], payload: Some(p.clone()) })
                .collect(),
            InjectLocation::Query => {
                let name = self.def.check.inject.name.as_deref().unwrap_or("q");
                self.def
                    .check
                    .payloads
                    .iter()
                    .map(|p| PlannedRequest {
                        method: method.clone(),
                        url: format!("{base}{path}?{}={}", encode_component(name), encode_component(p)),
                        headers: vec![],
                        payload: Some(p.clone()),
                    })
                    .collect()
            }
            InjectLocation::Header => {
                let name = self.def.check.inject.name.clone().unwrap_or_default();
                self.def
                    .check
                    .payloads
                    .iter()
                    .map(|p| PlannedRequest { method: method.clone(), url: format!("{base}{path}"), headers: vec![(name.clone(), p.clone())], payload: Some(p.clone()) })
                    .collect()
            }
        }
    }

    /// Judges a response. Every expectation the tactic set must hold (AND),
    /// to keep false positives down. Returns a finding draft when it fires.
    pub fn evaluate(&self, req: &PlannedRequest, status: Option<u16>, resp_headers: &[(String, String)], resp_body: &[u8]) -> Option<FindingDraft> {
        let e = &self.def.check.expect;
        if !e.status.is_empty() && !status.is_some_and(|s| e.status.contains(&s)) {
            return None;
        }
        let text = crate::codec::body_text(&resp_headers.to_vec(), resp_body).unwrap_or_default();
        if let Some(re) = &self.body_re {
            if !re.is_match(&text) {
                return None;
            }
        }
        if e.reflects_payload {
            match &req.payload {
                Some(p) if !p.is_empty() && text.contains(p.as_str()) => {}
                _ => return None,
            }
        }
        let mut description = self.def.description.clone();
        if let Some(p) = &req.payload {
            if !description.is_empty() {
                description.push_str("\n\n");
            }
            description.push_str(&format!("Payload: {}", clean(p, 200)));
        }
        if !self.def.remediation.is_empty() {
            if !description.is_empty() {
                description.push_str("\n\n");
            }
            description.push_str(&format!("Remediation: {}", self.def.remediation));
        }
        Some(FindingDraft { title: self.def.title.clone(), severity: self.def.severity, description, exchange_id: 0 })
    }
}

fn build_regex(p: &str) -> Result<Regex, String> {
    if p.is_empty() || p.len() > MAX_PATTERN {
        return Err(format!("pattern must be 1-{MAX_PATTERN} characters"));
    }
    RegexBuilder::new(p)
        .case_insensitive(true)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT * 4)
        .build()
        .map_err(|e| format!("invalid pattern: {}", detect::clean(&e.to_string(), 300)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Exchange;

    fn ex(host: &str, headers: &[(&str, &str)]) -> Exchange {
        Exchange {
            id: 1,
            host: host.into(),
            method: "GET".into(),
            path: "/".into(),
            req_headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            ..Default::default()
        }
    }

    fn tech(ids: &[&str]) -> Vec<Detection> {
        ids.iter()
            .map(|id| Detection {
                id: id.to_string(),
                name: id.to_string(),
                category: "framework".into(),
                version: None,
                confidence: 100,
                pack: "t".into(),
                evidence: "e".into(),
                exchange_id: None,
                implied_by: None,
            })
            .collect()
    }

    fn jwt_detector() -> Detector {
        compile_detector(
            DetectorDef {
                id: "jwt".into(),
                signal: "jwt-present".into(),
                description: String::new(),
                tech: vec![],
                conditions: vec![ConditionDef { request_header: Some("authorization".into()), regex: Some("bearer ".into()), ..Default::default() }],
                mode: MatchMode::Any,
            },
            "baseline",
        )
        .unwrap()
    }

    fn tech_detector(id: &str, signal: &str, techs: &[&str]) -> Detector {
        compile_detector(
            DetectorDef {
                id: id.into(),
                signal: signal.into(),
                description: String::new(),
                tech: techs.iter().map(|s| s.to_string()).collect(),
                conditions: vec![],
                mode: MatchMode::Any,
            },
            "baseline",
        )
        .unwrap()
    }

    fn tactic(id: &str, requires: &[&str], intr: Intrusiveness) -> Tactic {
        compile_tactic(
            TacticDef {
                id: id.into(),
                title: id.into(),
                description: String::new(),
                requires: requires.iter().map(|s| s.to_string()).collect(),
                severity: Severity::Medium,
                intrusiveness: intr,
                variant: String::new(),
                check: CheckDef { method: "GET".into(), path: None, inject: InjectDef::default(), payloads: vec![], expect: ExpectDef { status: vec![200], ..Default::default() } },
                remediation: String::new(),
            },
            "baseline",
        )
        .unwrap()
    }

    #[test]
    fn detector_emits_signal_from_condition() {
        let cat = Catalog { detectors: vec![jwt_detector()], tactics: vec![] };
        let sigs = cat.signals(&[], &[ex("api.example.com", &[("authorization", "Bearer eyJhbGc")])]);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].signal, "jwt-present");
        assert_eq!(sigs[0].exchange_id, Some(1));
    }

    #[test]
    fn detector_does_not_fire_without_evidence() {
        let cat = Catalog { detectors: vec![jwt_detector()], tactics: vec![] };
        assert!(cat.signals(&[], &[ex("api.example.com", &[("accept", "*/*")])]).is_empty());
    }

    #[test]
    fn jwt_tactic_only_runs_when_jwt_present() {
        let cat = Catalog { detectors: vec![jwt_detector()], tactics: vec![tactic("jwt-alg-none", &["jwt-present"], Intrusiveness::Active)] };
        let with = cat.suggest(&[], &[ex("api.example.com", &[("authorization", "Bearer eyJ")])]);
        assert_eq!(with.recommended.len() + with.optional.len(), 1);
        assert_eq!(with.skipped, 0);
        let without = cat.suggest(&[], &[ex("api.example.com", &[])]);
        assert_eq!(without.recommended.len() + without.optional.len(), 0);
        assert_eq!(without.skipped, 1);
    }

    #[test]
    fn php_checks_skipped_on_nodejs_site() {
        let cat = Catalog {
            detectors: vec![tech_detector("php", "php", &["php"]), tech_detector("nodejs", "nodejs", &["node", "express"])],
            tactics: vec![tactic("php-check", &["php"], Intrusiveness::Active), tactic("node-check", &["nodejs"], Intrusiveness::Safe)],
        };
        let node = cat.suggest(&tech(&["express"]), &[]);
        let picked: Vec<&str> = node.recommended.iter().chain(&node.optional).map(|t| t.id.as_str()).collect();
        assert_eq!(picked, vec!["node-check"], "PHP check must be skipped on a Node.js site");
        assert_eq!(node.skipped, 1);

        let php = cat.suggest(&tech(&["php"]), &[]);
        let picked: Vec<&str> = php.recommended.iter().chain(&php.optional).map(|t| t.id.as_str()).collect();
        assert_eq!(picked, vec!["php-check"]);
    }

    #[test]
    fn intrusive_tactics_are_optional_not_recommended() {
        let cat = Catalog {
            detectors: vec![jwt_detector()],
            tactics: vec![tactic("safe-one", &["jwt-present"], Intrusiveness::Safe), tactic("intrusive-one", &["jwt-present"], Intrusiveness::Intrusive)],
        };
        let s = cat.suggest(&[], &[ex("h", &[("authorization", "Bearer x")])]);
        assert_eq!(s.recommended.iter().map(|t| t.id.clone()).collect::<Vec<_>>(), vec!["safe-one"]);
        assert_eq!(s.optional.iter().map(|t| t.id.clone()).collect::<Vec<_>>(), vec!["intrusive-one"]);
    }

    #[test]
    fn tactic_requiring_two_signals_needs_both() {
        let cat = Catalog {
            detectors: vec![jwt_detector(), tech_detector("php", "php", &["php"])],
            tactics: vec![tactic("combo", &["jwt-present", "php"], Intrusiveness::Active)],
        };
        let jwt_only = cat.suggest(&[], &[ex("h", &[("authorization", "Bearer x")])]);
        assert_eq!(jwt_only.recommended.len() + jwt_only.optional.len(), 0);
        let both = cat.suggest(&tech(&["php"]), &[ex("h", &[("authorization", "Bearer x")])]);
        assert_eq!(both.recommended.len() + both.optional.len(), 1);
    }

    #[test]
    fn intrusiveness_is_required_in_the_schema() {
        let json = r#"{"id":"x","title":"x","requires":["s"],"severity":"low","check":{"expect":{"status":[200]}}}"#;
        assert!(serde_json::from_str::<TacticDef>(json).is_err(), "intrusiveness must be required");
    }

    #[test]
    fn injection_requires_a_payload_and_a_name() {
        let mut def = TacticDef {
            id: "x".into(),
            title: "x".into(),
            description: String::new(),
            requires: vec!["s".into()],
            severity: Severity::Low,
            intrusiveness: Intrusiveness::Active,
            variant: String::new(),
            check: CheckDef {
                method: "GET".into(),
                path: None,
                inject: InjectDef { location: InjectLocation::Query, name: None },
                payloads: vec![],
                expect: ExpectDef { reflects_payload: true, ..Default::default() },
            },
            remediation: String::new(),
        };
        assert!(compile_tactic(def.clone(), "p").is_err(), "query injection without a name must fail");
        def.check.inject.name = Some("q".into());
        assert!(compile_tactic(def.clone(), "p").is_err(), "injection without a payload must fail");
        def.check.payloads = vec!["probe".into()];
        assert!(compile_tactic(def, "p").is_ok());
    }

    #[test]
    fn expect_must_say_something() {
        let def = TacticDef {
            id: "x".into(),
            title: "x".into(),
            description: String::new(),
            requires: vec!["s".into()],
            severity: Severity::Low,
            intrusiveness: Intrusiveness::Safe,
            variant: String::new(),
            check: CheckDef { method: "GET".into(), path: None, inject: InjectDef::default(), payloads: vec![], expect: ExpectDef::default() },
            remediation: String::new(),
        };
        assert!(compile_tactic(def, "p").is_err());
    }

    #[test]
    fn builtin_packs_parse_and_fingerprint() {
        let cat = builtin_catalog();
        assert!(!cat.detectors.is_empty());
        // A Node.js app with a JWT lights up the right signals and no others.
        let sigs = cat.signals(
            &tech(&["express"]),
            &[ex("api.example.com", &[("authorization", "Bearer eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig")])],
        );
        let names: BTreeSet<&str> = sigs.iter().map(|s| s.signal.as_str()).collect();
        assert!(names.contains("web"));
        assert!(names.contains("jwt-present"));
        assert!(names.contains("nodejs"));
        assert!(!names.contains("php"), "php must not fire on a Node.js site");
    }

    #[test]
    fn builtin_tactics_have_their_gating_signals() {
        let cat = builtin_catalog();
        assert!(cat.tactics.iter().any(|t| t.def.id == "exposed-git-config"));
        assert!(cat.tactics.iter().any(|t| t.def.id == "reflected-parameter"));
        // Every signal a built-in tactic gates on is emitted by a built-in detector.
        assert!(cat.unmet_requirements().is_empty(), "built-in tactics gate on signals no built-in detector emits: {:?}", cat.unmet_requirements());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let json = br#"{"plonix_scanpack":1,"name":"x","version":"0.1.0","description":"d","author":"a","detectors":[{"id":"d","signal":"s","surprise":true,"tech":["php"]}]}"#;
        assert!(parse_pack(json, "test").is_err());
    }

    fn tactic_with_check(check: CheckDef, intr: Intrusiveness) -> Tactic {
        compile_tactic(
            TacticDef {
                id: "t".into(),
                title: "Test finding".into(),
                description: "desc".into(),
                requires: vec!["web".into()],
                severity: Severity::Medium,
                intrusiveness: intr,
                variant: String::new(),
                check,
                remediation: String::new(),
            },
            "p",
        )
        .unwrap()
    }

    #[test]
    fn fixed_path_plans_one_request_per_host() {
        let t = tactic_with_check(
            CheckDef { method: "GET".into(), path: Some("/.well-known/security.txt".into()), inject: InjectDef::default(), payloads: vec![], expect: ExpectDef { status: vec![200], ..Default::default() } },
            Intrusiveness::Safe,
        );
        let reqs = t.plan("https", "example.com", None);
        assert_eq!(reqs.len(), 1);
        assert_eq!(reqs[0].url, "https://example.com/.well-known/security.txt");
        assert_eq!(reqs[0].payload, None);
    }

    #[test]
    fn query_injection_plans_one_request_per_payload_and_encodes() {
        let t = tactic_with_check(
            CheckDef {
                method: "GET".into(),
                path: None,
                inject: InjectDef { location: InjectLocation::Query, name: Some("q".into()) },
                payloads: vec!["a b".into(), "x&y".into()],
                expect: ExpectDef { reflects_payload: true, ..Default::default() },
            },
            Intrusiveness::Active,
        );
        let reqs = t.plan("https", "h", Some(&ScanTarget { method: "GET".into(), path: "/search".into() }));
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].url, "https://h/search?q=a%20b");
        assert_eq!(reqs[1].url, "https://h/search?q=x%26y");
        assert_eq!(reqs[0].payload.as_deref(), Some("a b"));
    }

    #[test]
    fn injecting_tactic_plans_nothing_without_a_target() {
        let t = tactic_with_check(
            CheckDef { method: "GET".into(), path: None, inject: InjectDef { location: InjectLocation::PathSuffix, name: None }, payloads: vec!["~".into()], expect: ExpectDef { status: vec![200], ..Default::default() } },
            Intrusiveness::Active,
        );
        assert!(t.plan("https", "h", None).is_empty());
    }

    #[test]
    fn path_and_inject_are_mutually_exclusive() {
        let def = TacticDef {
            id: "t".into(),
            title: "x".into(),
            description: String::new(),
            requires: vec!["web".into()],
            severity: Severity::Low,
            intrusiveness: Intrusiveness::Safe,
            variant: String::new(),
            check: CheckDef {
                method: "GET".into(),
                path: Some("/x".into()),
                inject: InjectDef { location: InjectLocation::Query, name: Some("q".into()) },
                payloads: vec!["p".into()],
                expect: ExpectDef { status: vec![200], ..Default::default() },
            },
            remediation: String::new(),
        };
        assert!(compile_tactic(def, "p").is_err());
    }

    #[test]
    fn evaluate_requires_every_expectation() {
        let t = tactic_with_check(
            CheckDef {
                method: "GET".into(),
                path: None,
                inject: InjectDef { location: InjectLocation::Query, name: Some("q".into()) },
                payloads: vec!["<xyz>".into()],
                expect: ExpectDef { status: vec![200], body: Some("error".into()), reflects_payload: true },
            },
            Intrusiveness::Active,
        );
        let req = PlannedRequest { method: "GET".into(), url: "https://h/s?q=%3Cxyz%3E".into(), headers: vec![], payload: Some("<xyz>".into()) };
        let hdr = vec![("content-type".to_string(), "text/html".to_string())];
        // All three hold: status 200, body has "error", payload reflected.
        assert!(t.evaluate(&req, Some(200), &hdr, b"<h1>error</h1> echo <xyz>").is_some());
        // Wrong status.
        assert!(t.evaluate(&req, Some(404), &hdr, b"<h1>error</h1> <xyz>").is_none());
        // Body regex misses.
        assert!(t.evaluate(&req, Some(200), &hdr, b"ok <xyz>").is_none());
        // Payload not reflected.
        assert!(t.evaluate(&req, Some(200), &hdr, b"<h1>error</h1>").is_none());
    }

    #[test]
    fn evaluate_status_only_fires_on_match() {
        let t = tactic_with_check(
            CheckDef { method: "GET".into(), path: Some("/.well-known/security.txt".into()), inject: InjectDef::default(), payloads: vec![], expect: ExpectDef { status: vec![200], ..Default::default() } },
            Intrusiveness::Safe,
        );
        let req = PlannedRequest { method: "GET".into(), url: "https://h/.well-known/security.txt".into(), headers: vec![], payload: None };
        assert!(t.evaluate(&req, Some(200), &[], b"Contact: mailto:x").is_some());
        assert!(t.evaluate(&req, Some(404), &[], b"not found").is_none());
    }

    #[test]
    fn unmet_requirements_are_reported() {
        let cat = Catalog { detectors: vec![jwt_detector()], tactics: vec![tactic("x", &["jwt-present", "graphql"], Intrusiveness::Safe)] };
        assert_eq!(cat.unmet_requirements(), vec!["graphql".to_string()]);
    }
}
