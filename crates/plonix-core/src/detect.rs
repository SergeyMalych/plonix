//! Technology detection from captured traffic.
//!
//! Detection is driven entirely by declarative rules (see
//! `docs/detection-rules.md`). Rules come from rule packs, which may be
//! written by anyone and loaded from a file or URL, so everything in this
//! module treats them as untrusted input:
//!
//! * the schema is strict (unknown fields are errors) and every string has a
//!   length and character limit;
//! * patterns are compiled with the `regex` crate, which runs in linear time
//!   (no backtracking, no catastrophic patterns) and with a compiled-size cap;
//! * matching only reads traffic that is already captured. A rule cannot
//!   send a request, touch scope, or reach the file system or network.

use std::collections::{BTreeMap, HashMap};

use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::codec;
use crate::model::{Exchange, header_all};

/// Categories a rule may use. Fixed so that packs from different authors
/// group the same way in the Map.
pub const CATEGORIES: &[&str] = &[
    "web-server",
    "language",
    "framework",
    "cms",
    "ecommerce",
    "javascript",
    "cdn",
    "waf",
    "hosting",
    "load-balancer",
    "cache",
    "auth",
    "api",
    "analytics",
    "database",
    "devops",
    "other",
];

pub const MAX_CONDITIONS: usize = 32;
pub const MAX_IMPLIES: usize = 16;
const MAX_PATTERN: usize = 1000;
const REGEX_SIZE_LIMIT: usize = 256 * 1024;
/// How much of each response body `body` conditions look at.
pub const MAX_BODY_SCAN: usize = 512 * 1024;
/// How many of a host's most recent exchanges are examined.
pub const HOST_SAMPLE: usize = 300;

/// A detection rule as written in a pack.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleDef {
    /// Stable identifier, e.g. `nginx`. Shared across packs: two packs that
    /// detect the same technology should use the same id.
    pub id: String,
    /// Display name, e.g. `nginx`.
    pub name: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub website: String,
    /// 1–100. How sure a match makes us.
    #[serde(default = "full_confidence")]
    pub confidence: u8,
    /// `any` (default): one matching condition is enough. `all`: every
    /// condition must match somewhere in the host's traffic.
    #[serde(default, rename = "match")]
    pub mode: MatchMode,
    pub conditions: Vec<ConditionDef>,
    /// Ids of technologies this one implies (WordPress implies PHP).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implies: Vec<String>,
}

fn full_confidence() -> u8 {
    100
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchMode {
    #[default]
    Any,
    All,
}

/// One condition. Exactly one target field is set:
///
/// | field            | matches                                   | `regex` applies to |
/// |------------------|-------------------------------------------|--------------------|
/// | `header`         | a response header by name                 | its value          |
/// | `request_header` | a request header by name                  | its value          |
/// | `cookie`         | a cookie by name (Set-Cookie or Cookie);  | its value          |
/// |                  | `name*` matches names starting with name  |                    |
/// | `query_param`    | a query string parameter by name          | its value          |
/// | `path`           | regex on the request path                 | —                  |
/// | `host`           | regex on the host name                    | —                  |
/// | `body`           | regex on the decoded response body        | —                  |
///
/// `version` is an optional template such as `$1` that extracts a version
/// from the capture groups of the pattern that matched.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_header: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cookie: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_param: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone)]
enum Target {
    Header(String),
    RequestHeader(String),
    Cookie(String),
    QueryParam(String),
    Path,
    Host,
    Body,
}

#[derive(Debug, Clone)]
struct Condition {
    target: Target,
    /// For named targets: optional value pattern. For the others: required.
    regex: Option<Regex>,
    version: Option<String>,
}

/// A validated, compiled rule.
#[derive(Debug, Clone)]
pub struct Rule {
    pub def: RuleDef,
    /// Name of the pack the rule came from.
    pub pack: String,
    conditions: Vec<Condition>,
}

/// Checks a rule and compiles its patterns. Errors name the offending field.
pub fn compile(def: RuleDef, pack: &str) -> Result<Rule, String> {
    check_id(&def.id).map_err(|e| format!("id: {e}"))?;
    check_text(&def.name, 64, false).map_err(|e| format!("name: {e}"))?;
    if !CATEGORIES.contains(&def.category.as_str()) {
        return Err(format!("category: `{}` is not one of {}", clean(&def.category, 40), CATEGORIES.join(", ")));
    }
    check_text(&def.description, 500, true).map_err(|e| format!("description: {e}"))?;
    check_text(&def.website, 200, true).map_err(|e| format!("website: {e}"))?;
    if !def.website.is_empty() && !def.website.starts_with("https://") && !def.website.starts_with("http://") {
        return Err("website: must be an http(s) URL".into());
    }
    if !(1..=100).contains(&def.confidence) {
        return Err("confidence: must be between 1 and 100".into());
    }
    if def.conditions.is_empty() {
        return Err("conditions: at least one condition is required".into());
    }
    if def.conditions.len() > MAX_CONDITIONS {
        return Err(format!("conditions: at most {MAX_CONDITIONS} conditions per rule"));
    }
    if def.implies.len() > MAX_IMPLIES {
        return Err(format!("implies: at most {MAX_IMPLIES} entries"));
    }
    for (i, id) in def.implies.iter().enumerate() {
        check_id(id).map_err(|e| format!("implies[{i}]: {e}"))?;
    }
    let conditions = def
        .conditions
        .iter()
        .enumerate()
        .map(|(i, c)| compile_condition(c).map_err(|e| format!("conditions[{i}]: {e}")))
        .collect::<Result<_, _>>()?;
    Ok(Rule { def, pack: pack.to_string(), conditions })
}

fn compile_condition(c: &ConditionDef) -> Result<Condition, String> {
    let named = [
        c.header.as_ref().map(|n| Target::Header(n.to_ascii_lowercase())),
        c.request_header.as_ref().map(|n| Target::RequestHeader(n.to_ascii_lowercase())),
        c.cookie.as_ref().map(|n| Target::Cookie(n.clone())),
        c.query_param.as_ref().map(|n| Target::QueryParam(n.clone())),
    ];
    let pattern_targets = [(c.path.as_ref(), Target::Path), (c.host.as_ref(), Target::Host), (c.body.as_ref(), Target::Body)];
    let set = named.iter().filter(|t| t.is_some()).count() + pattern_targets.iter().filter(|(p, _)| p.is_some()).count();
    if set != 1 {
        return Err("set exactly one of header, request_header, cookie, query_param, path, host, body".into());
    }
    let (target, pattern) = if let Some(t) = named.into_iter().flatten().next() {
        let name = match &t {
            Target::Header(n) | Target::RequestHeader(n) | Target::Cookie(n) | Target::QueryParam(n) => n,
            _ => unreachable!(),
        };
        let wildcard_ok = matches!(t, Target::Cookie(_)) && name.len() > 1 && name.find('*') == Some(name.len() - 1);
        if name.is_empty()
            || name.len() > 128
            || !name.bytes().all(|b| b.is_ascii_graphic() && b != b':' && b != b'=' && b != b';')
            || (name.contains('*') && !wildcard_ok)
        {
            return Err("name must be 1-128 printable characters without `:`, `=` or `;` (a cookie name may end in `*` to match a prefix)".into());
        }
        (t, c.regex.as_deref())
    } else {
        if c.regex.is_some() {
            return Err("`regex` only applies to header, request_header, cookie and query_param; put the pattern in the field itself".into());
        }
        let (p, t) = pattern_targets.into_iter().find(|(p, _)| p.is_some()).unwrap();
        (t, p.map(String::as_str))
    };
    let regex = pattern.map(build_regex).transpose()?;
    if let Some(v) = &c.version {
        let Some(re) = &regex else { return Err("version needs a pattern with a capture group".into()) };
        check_version_template(v, re.captures_len() - 1)?;
    }
    Ok(Condition { target, regex, version: c.version.clone() })
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
        .map_err(|e| format!("invalid pattern: {}", clean(&e.to_string(), 300)))
}

/// `$1`, `$2`... and literal version characters only.
fn check_version_template(t: &str, groups: usize) -> Result<(), String> {
    if t.is_empty() || t.len() > 32 {
        return Err("version template must be 1-32 characters".into());
    }
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' {
            match chars.next().and_then(|d| d.to_digit(10)) {
                Some(n) if n >= 1 && (n as usize) <= groups => {}
                _ => return Err(format!("version `{t}` refers to a capture group the pattern does not have")),
            }
        } else if !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')) {
            return Err(format!("version template `{}` may only contain $N and [A-Za-z0-9._-]", clean(t, 32)));
        }
    }
    Ok(())
}

/// Ids: lowercase letters, digits, `.`, `_`, `-`; 1–64 characters.
pub fn check_id(id: &str) -> Result<(), String> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'));
    if ok { Ok(()) } else { Err(format!("`{}` must be 1-64 of [a-z0-9._-], starting with a letter or digit", clean(id, 64))) }
}

/// Free text from a pack: bounded and free of control characters, so it
/// cannot rewrite the user's terminal (ANSI escapes) or fake output lines.
pub fn check_text(s: &str, max: usize, allow_empty: bool) -> Result<(), String> {
    if s.is_empty() && !allow_empty {
        return Err("must not be empty".into());
    }
    if s.chars().count() > max {
        return Err(format!("at most {max} characters"));
    }
    if s.chars().any(|c| c.is_control() || is_invisible(c)) {
        return Err("must not contain control characters or invisible characters".into());
    }
    Ok(())
}

/// Characters that do not show, or that change the direction of the text
/// around them, so untrusted text could hide what it says.
pub fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}' | '\u{e0000}'..='\u{e007f}')
}

/// Makes untrusted text safe to show in an error message.
pub fn clean(s: &str, max: usize) -> String {
    let mut out: String = s.chars().filter(|c| !c.is_control()).take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// One technology seen on a host.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Detection {
    pub id: String,
    pub name: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub confidence: u8,
    pub pack: String,
    /// Why: the condition that matched and a sample of what it matched.
    pub evidence: String,
    /// Exchange the evidence came from (none for implied technologies).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchange_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub implied_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostTech {
    pub host: String,
    pub tech: Vec<Detection>,
}

/// A set of compiled rules, ready to run against traffic.
#[derive(Debug, Default, Clone)]
pub struct Detector {
    pub rules: Vec<Rule>,
}

struct Hit {
    version: Option<String>,
    evidence: String,
    exchange_id: i64,
}

impl Detector {
    pub fn new(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    fn needs_body(&self) -> bool {
        self.rules.iter().any(|r| r.conditions.iter().any(|c| matches!(c.target, Target::Body)))
    }

    /// Technologies seen in one host's traffic. `exchanges` should all
    /// belong to that host, most recent first.
    pub fn detect(&self, exchanges: &[Exchange]) -> Vec<Detection> {
        let needs_body = self.needs_body();
        // hits[rule][condition] = first hit
        let mut hits: Vec<Vec<Option<Hit>>> = self.rules.iter().map(|r| r.conditions.iter().map(|_| None).collect()).collect();
        for ex in exchanges {
            let view = View::new(ex, needs_body);
            for (ri, rule) in self.rules.iter().enumerate() {
                for (ci, cond) in rule.conditions.iter().enumerate() {
                    if hits[ri][ci].is_some() {
                        // Keep looking only to pick up a version we don't have yet.
                        if cond.version.is_none() || hits[ri][ci].as_ref().is_some_and(|h| h.version.is_some()) {
                            continue;
                        }
                    }
                    if let Some(hit) = cond.eval(&view) {
                        hits[ri][ci] = Some(hit);
                    }
                }
            }
        }

        let mut found: BTreeMap<String, Detection> = BTreeMap::new();
        for (rule, hits) in self.rules.iter().zip(hits) {
            let matched = match rule.def.mode {
                MatchMode::Any => hits.iter().any(Option::is_some),
                MatchMode::All => hits.iter().all(Option::is_some),
            };
            if !matched {
                continue;
            }
            let version = hits.iter().flatten().find_map(|h| h.version.clone());
            let first = hits.iter().flatten().next().unwrap();
            let d = Detection {
                id: rule.def.id.clone(),
                name: rule.def.name.clone(),
                category: rule.def.category.clone(),
                version,
                confidence: rule.def.confidence,
                pack: rule.pack.clone(),
                evidence: first.evidence.clone(),
                exchange_id: Some(first.exchange_id),
                implied_by: None,
            };
            merge(&mut found, d);
        }

        // Implied technologies, transitively, from the rules we know about.
        let by_id: HashMap<&str, &Rule> = self.rules.iter().map(|r| (r.def.id.as_str(), r)).collect();
        let mut queue: Vec<(String, String, u8)> =
            found.values().flat_map(|d| implies_of(&by_id, &d.id).map(|i| (i, d.id.clone(), d.confidence)).collect::<Vec<_>>()).collect();
        let mut guard = 0;
        while let Some((id, by, conf)) = queue.pop() {
            guard += 1;
            if guard > 1000 || found.contains_key(&id) {
                continue;
            }
            let Some(rule) = by_id.get(id.as_str()) else { continue };
            let d = Detection {
                id: id.clone(),
                name: rule.def.name.clone(),
                category: rule.def.category.clone(),
                version: None,
                confidence: conf,
                pack: rule.pack.clone(),
                evidence: format!("implied by {by}"),
                exchange_id: None,
                implied_by: Some(by),
            };
            queue.extend(implies_of(&by_id, &id).map(|i| (i, id.clone(), conf)));
            found.insert(id, d);
        }

        let mut out: Vec<Detection> = found.into_values().collect();
        out.sort_by(|a, b| {
            CATEGORIES
                .iter()
                .position(|c| *c == a.category)
                .cmp(&CATEGORIES.iter().position(|c| *c == b.category))
                .then(b.confidence.cmp(&a.confidence))
                .then(a.name.cmp(&b.name))
        });
        out
    }
}

fn implies_of<'a>(by_id: &'a HashMap<&str, &Rule>, id: &str) -> impl Iterator<Item = String> + 'a {
    by_id.get(id).map(|r| r.def.implies.clone()).unwrap_or_default().into_iter()
}

/// Same technology from several rules or packs: keep the most confident,
/// and any version one of them found.
fn merge(found: &mut BTreeMap<String, Detection>, d: Detection) {
    match found.get_mut(&d.id) {
        None => {
            found.insert(d.id.clone(), d);
        }
        Some(existing) => {
            let version = existing.version.clone().or_else(|| d.version.clone());
            if d.confidence > existing.confidence {
                *existing = d;
            }
            existing.version = version;
        }
    }
}

/// The parts of an exchange conditions look at, extracted once.
struct View<'a> {
    ex: &'a Exchange,
    cookies: Vec<(String, String)>,
    params: Vec<(String, String)>,
    body: Option<String>,
}

impl<'a> View<'a> {
    fn new(ex: &'a Exchange, needs_body: bool) -> Self {
        let mut cookies = vec![];
        for v in header_all(&ex.resp_headers, "set-cookie") {
            let pair = v.split(';').next().unwrap_or("");
            if let Some((k, v)) = pair.split_once('=') {
                cookies.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        for v in header_all(&ex.req_headers, "cookie") {
            for pair in v.split(';') {
                if let Some((k, v)) = pair.split_once('=') {
                    cookies.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
        }
        let params = ex
            .query
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| match p.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (p.to_string(), String::new()),
            })
            .collect();
        let body = if needs_body {
            codec::body_text(&ex.resp_headers, &ex.resp_body).map(|mut t| {
                if t.len() > MAX_BODY_SCAN {
                    let mut cut = MAX_BODY_SCAN;
                    while !t.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    t.truncate(cut);
                }
                t
            })
        } else {
            None
        };
        Self { ex, cookies, params, body }
    }
}

impl Condition {
    fn eval(&self, v: &View) -> Option<Hit> {
        let id = v.ex.id;
        // Cookie, request header and query values are often credentials, so
        // evidence names them without repeating their values.
        match &self.target {
            Target::Header(name) => {
                header_all(&v.ex.resp_headers, name).find_map(|val| self.hit(val, &format!("response header {name}"), true, id))
            }
            Target::RequestHeader(name) => {
                header_all(&v.ex.req_headers, name).find_map(|val| self.hit(val, &format!("request header {name}"), false, id))
            }
            Target::Cookie(name) => {
                let matches = |k: &str| match name.strip_suffix('*') {
                    Some(prefix) => k.starts_with(prefix),
                    None => k == name,
                };
                v.cookies.iter().filter(|(k, _)| matches(k)).find_map(|(k, val)| self.hit(val, &format!("cookie {}", clean(k, 64)), false, id))
            }
            Target::QueryParam(name) => v
                .params
                .iter()
                .filter(|(k, _)| k == name)
                .find_map(|(k, val)| self.hit(val, &format!("query parameter {}", clean(k, 64)), false, id)),
            Target::Path => self.hit(&v.ex.path, "path", true, id),
            Target::Host => self.hit(&v.ex.host, "host", true, id),
            Target::Body => v.body.as_deref().and_then(|b| self.hit(b, "response body", true, id)),
        }
    }

    /// Matches `value` against the condition's pattern (a present value is
    /// enough when there is none) and builds the evidence line.
    fn hit(&self, value: &str, what: &str, show_value: bool, exchange_id: i64) -> Option<Hit> {
        let Some(re) = &self.regex else {
            let evidence = if show_value { format!("{what}: {}", clean(value, 80)) } else { what.to_string() };
            return Some(Hit { version: None, evidence, exchange_id });
        };
        let caps = re.captures(value)?;
        let version = self.version.as_ref().and_then(|t| {
            let mut out = String::new();
            caps.expand(t, &mut out);
            let out: String = out.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')).take(32).collect();
            (!out.is_empty()).then_some(out)
        });
        let evidence = if show_value {
            format!("{what} ~ {}", clean(caps.get(0).map(|m| m.as_str()).unwrap_or(""), 80))
        } else {
            format!("{what} (value matches)")
        };
        Some(Hit { version, evidence, exchange_id })
    }
}

/// A compiled set of conditions matched against a host's exchanges. Shared
/// with scan detectors (see `scan.rs`) so a detector reads traffic with
/// exactly the same grammar, limits and evidence as a detection rule. Like
/// everything here it only reads already-captured traffic — it cannot send a
/// request or touch scope.
#[derive(Debug, Clone)]
pub struct ConditionSet {
    conditions: Vec<Condition>,
    mode: MatchMode,
}

impl ConditionSet {
    /// Compiles and validates conditions. `mode` is `Any` (one hit is enough)
    /// or `All` (every condition must hit somewhere in the traffic).
    pub fn compile(defs: &[ConditionDef], mode: MatchMode) -> Result<Self, String> {
        if defs.is_empty() {
            return Err("at least one condition is required".into());
        }
        if defs.len() > MAX_CONDITIONS {
            return Err(format!("at most {MAX_CONDITIONS} conditions"));
        }
        let conditions = defs
            .iter()
            .enumerate()
            .map(|(i, c)| compile_condition(c).map_err(|e| format!("conditions[{i}]: {e}")))
            .collect::<Result<_, _>>()?;
        Ok(Self { conditions, mode })
    }

    fn needs_body(&self) -> bool {
        self.conditions.iter().any(|c| matches!(c.target, Target::Body))
    }

    /// If the set matches across `exchanges`, returns one evidence line (with
    /// the exchange it came from) per condition that hit; otherwise `None`.
    pub fn evaluate(&self, exchanges: &[Exchange]) -> Option<Vec<(i64, String)>> {
        let needs_body = self.needs_body();
        let mut hits: Vec<Option<Hit>> = self.conditions.iter().map(|_| None).collect();
        for ex in exchanges {
            let view = View::new(ex, needs_body);
            for (ci, cond) in self.conditions.iter().enumerate() {
                if hits[ci].is_none() {
                    if let Some(hit) = cond.eval(&view) {
                        hits[ci] = Some(hit);
                    }
                }
            }
        }
        let matched = match self.mode {
            MatchMode::Any => hits.iter().any(Option::is_some),
            MatchMode::All => hits.iter().all(Option::is_some),
        };
        matched.then(|| hits.into_iter().flatten().map(|h| (h.exchange_id, h.evidence)).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(json: &str) -> Result<Rule, String> {
        let def: RuleDef = serde_json::from_str(json).map_err(|e| e.to_string())?;
        compile(def, "test")
    }

    fn ex(id: i64, path: &str, resp_headers: &[(&str, &str)], body: &str) -> Exchange {
        Exchange {
            id,
            scheme: "https".into(),
            host: "app.example.com".into(),
            port: 443,
            method: "GET".into(),
            path: path.into(),
            status: Some(200),
            resp_headers: resp_headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            resp_body: body.as_bytes().to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn header_with_version() {
        let r = rule(r#"{"id":"nginx","name":"nginx","category":"web-server",
            "conditions":[{"header":"Server","regex":"^nginx(?:/([\\d.]+))?","version":"$1"}]}"#)
        .unwrap();
        let d = Detector::new(vec![r]);
        let found = d.detect(&[ex(7, "/", &[("Server", "nginx/1.25.3")], "")]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].version.as_deref(), Some("1.25.3"));
        assert_eq!(found[0].exchange_id, Some(7));
        assert!(found[0].evidence.contains("server ~ nginx/1.25.3"), "{}", found[0].evidence);
        assert!(d.detect(&[ex(1, "/", &[("Server", "Apache")], "")]).is_empty());
    }

    #[test]
    fn version_found_on_a_later_exchange() {
        let r = rule(r#"{"id":"nginx","name":"nginx","category":"web-server",
            "conditions":[{"header":"server","regex":"^nginx(?:/([\\d.]+))?","version":"$1"}]}"#)
        .unwrap();
        let found = Detector::new(vec![r]).detect(&[ex(1, "/", &[("Server", "nginx")], ""), ex(2, "/", &[("Server", "nginx/1.2")], "")]);
        assert_eq!(found[0].version.as_deref(), Some("1.2"));
    }

    #[test]
    fn cookies_paths_bodies_and_all_mode() {
        let wp = rule(r#"{"id":"wordpress","name":"WordPress","category":"cms","match":"any","implies":["php"],
            "conditions":[{"path":"^/wp-(content|includes)/"},
                          {"body":"<meta name=\"generator\" content=\"WordPress ([\\d.]+)\"","version":"$1"}]}"#)
        .unwrap();
        let php = rule(r#"{"id":"php","name":"PHP","category":"language","conditions":[{"cookie":"PHPSESSID"}]}"#).unwrap();
        let both = rule(r#"{"id":"combo","name":"Combo","category":"other","match":"all",
            "conditions":[{"cookie":"PHPSESSID"},{"path":"^/admin"}]}"#)
        .unwrap();
        let d = Detector::new(vec![wp, php, both]);

        let page = ex(1, "/", &[("Content-Type", "text/html")], r#"<html><meta name="generator" content="WordPress 6.4.2"></html>"#);
        let found = d.detect(&[page]);
        let ids: Vec<_> = found.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["php", "wordpress"]);
        assert_eq!(found[1].version.as_deref(), Some("6.4.2"));
        assert_eq!(found[0].implied_by.as_deref(), Some("wordpress"));

        let mut login = ex(2, "/admin/login", &[], "");
        login.req_headers = vec![("Cookie".into(), "a=1; PHPSESSID=abc".into())];
        let found = d.detect(&[login.clone()]);
        let ids: Vec<_> = found.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["php", "combo"]);
        assert!(found[0].implied_by.is_none(), "direct evidence wins over implication");

        login.path = "/".into();
        assert!(!d.detect(&[login]).iter().any(|d| d.id == "combo"), "all mode needs every condition");
    }

    #[test]
    fn evidence_never_repeats_credentials() {
        let jwt = rule(r#"{"id":"jwt","name":"JWT","category":"auth",
            "conditions":[{"request_header":"Authorization","regex":"^Bearer eyJ"}]}"#)
        .unwrap();
        let sess = rule(r#"{"id":"php","name":"PHP","category":"language","conditions":[{"cookie":"PHPSESSID"}]}"#).unwrap();
        let mut e = ex(1, "/", &[], "");
        e.req_headers = vec![("Authorization".into(), "Bearer eyJhbGciOi.secret".into()), ("Cookie".into(), "PHPSESSID=s3cr3t".into())];
        for d in Detector::new(vec![jwt, sess]).detect(&[e]) {
            assert!(!d.evidence.contains("eyJ") && !d.evidence.contains("s3cr3t"), "{}", d.evidence);
        }
    }

    #[test]
    fn rejects_bad_rules() {
        let cases = [
            (r#"{"id":"Bad Id","name":"x","category":"cms","conditions":[{"path":"x"}]}"#, "id"),
            (r#"{"id":"x","name":"x","category":"nope","conditions":[{"path":"x"}]}"#, "category"),
            (r#"{"id":"x","name":"x","category":"cms","conditions":[]}"#, "at least one"),
            (r#"{"id":"x","name":"x","category":"cms","conditions":[{"path":"x","host":"y"}]}"#, "exactly one"),
            (r#"{"id":"x","name":"x","category":"cms","conditions":[{"path":"(unclosed"}]}"#, "invalid pattern"),
            (r#"{"id":"x","name":"x","category":"cms","conditions":[{"path":"a","regex":"b"}]}"#, "only applies"),
            (r#"{"id":"x","name":"x","category":"cms","conditions":[{"path":"a","version":"$1"}]}"#, "capture group"),
            (r#"{"id":"x","name":"x","category":"cms","conditions":[{"path":"(a)","version":"$(rm)"}]}"#, "capture group"),
            (r#"{"id":"x","name":"x\u001b[31m","category":"cms","conditions":[{"path":"a"}]}"#, "control"),
            (r#"{"id":"x","name":"x","category":"cms","confidence":0,"conditions":[{"path":"a"}]}"#, "confidence"),
            (r#"{"id":"x","name":"x","category":"cms","conditions":[{"path":"a"}],"exec":"sh"}"#, "unknown field"),
            (r#"{"id":"x","name":"x","category":"cms","conditions":[{"path":"(?=a)"}]}"#, "invalid pattern"),
        ];
        for (json, want) in cases {
            let err = rule(json).unwrap_err();
            assert!(err.contains(want), "{json}: expected `{want}` in `{err}`");
        }
    }

    #[test]
    fn huge_patterns_are_refused() {
        // A pattern that compiles to an enormous automaton is refused instead
        // of eating memory.
        let def = RuleDef {
            id: "x".into(),
            name: "x".into(),
            category: "other".into(),
            description: String::new(),
            website: String::new(),
            confidence: 100,
            mode: MatchMode::Any,
            conditions: vec![ConditionDef { body: Some("\\w{1000}\\w{1000}".into()), ..Default::default() }],
            implies: vec![],
        };
        // Either too long or too large compiled; both are refusals.
        assert!(compile(def, "t").is_err());
    }

    #[test]
    fn implication_cycles_terminate() {
        let a = rule(r#"{"id":"a","name":"A","category":"other","implies":["b"],"conditions":[{"path":"^/a"}]}"#).unwrap();
        let b = rule(r#"{"id":"b","name":"B","category":"other","implies":["a"],"conditions":[{"path":"^/b"}]}"#).unwrap();
        let found = Detector::new(vec![a, b]).detect(&[ex(1, "/a", &[], "")]);
        assert_eq!(found.len(), 2);
    }
}
