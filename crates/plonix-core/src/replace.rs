//! Match and replace: per-project rules the proxy applies to traffic as it
//! passes, before Intercept sees a request and before the client gets a
//! response.
//!
//! A rule names what it changes (request first line, request headers,
//! request body, response headers, response body), what to look for
//! (literal text, or a regular expression whose `$1` captures the
//! replacement can use) and what to put instead. Header rules see the
//! headers as `Name: value` lines, one per header, so a rule can change a
//! value, rename a header, add one (replace with two lines) or remove one
//! (replace a whole line with nothing).
//!
//! Body rules only apply to bodies that are read in full within the body
//! limit; a longer or still-arriving body passes unchanged. A compressed
//! response body is matched decoded, and sent uncompressed when a rule
//! changed it.

use std::sync::Arc;

use regex::bytes::{NoExpand, Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::model::Headers;
use crate::settings::{Field, Level, Section};

pub const SETTINGS_SECTION: &str = "replace";

/// The Settings section: one switch for all rules; the rules themselves are
/// listed under it and kept in the project's database.
pub fn settings_section() -> Section {
    Section::new(SETTINGS_SECTION, "Match and replace", Level::Project)
        .describe(
            "Rules that change traffic as it passes through the proxy: before Intercept sees a request, and before the browser gets a response. \
             Body rules only apply to bodies within the body limit (Settings › Proxy); longer or streaming bodies pass unchanged.",
        )
        .order(16)
        .field(Field::toggle("enabled", "Apply match-and-replace rules", true).help("Off leaves every rule in place but changes nothing."))
}

/// What a rule changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// `METHOD /path?query`.
    RequestLine,
    RequestHeader,
    RequestBody,
    ResponseHeader,
    ResponseBody,
}

impl Target {
    pub const ALL: &[Target] = &[Target::RequestLine, Target::RequestHeader, Target::RequestBody, Target::ResponseHeader, Target::ResponseBody];

    pub fn as_str(self) -> &'static str {
        match self {
            Target::RequestLine => "request_line",
            Target::RequestHeader => "request_header",
            Target::RequestBody => "request_body",
            Target::ResponseHeader => "response_header",
            Target::ResponseBody => "response_body",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.as_str() == s)
    }

    fn label(self) -> &'static str {
        match self {
            Target::RequestLine => "request line",
            Target::RequestHeader => "request header",
            Target::RequestBody => "request body",
            Target::ResponseHeader => "response header",
            Target::ResponseBody => "response body",
        }
    }
}

/// A rule as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: i64,
    pub target: Target,
    /// Text to find: literal, or a regular expression when `regex` is set.
    pub pattern: String,
    /// What replaces each match; with `regex`, `$1` or `${name}` insert captures.
    pub replace: String,
    pub regex: bool,
    pub enabled: bool,
    /// Only change traffic to hosts accepted into scope.
    pub in_scope_only: bool,
    #[serde(default)]
    pub note: String,
}

/// A new rule, or the fields of a rule to change.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RuleInput {
    #[serde(default)]
    pub target: Option<Target>,
    #[serde(default, rename = "match", alias = "pattern")]
    pub pattern: Option<String>,
    #[serde(default)]
    pub replace: Option<String>,
    #[serde(default)]
    pub regex: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub in_scope_only: Option<bool>,
    #[serde(default)]
    pub note: Option<String>,
}

impl RuleInput {
    /// Applies these fields to `rule` and checks the result.
    pub fn apply_to(&self, mut rule: Rule) -> Result<Rule, String> {
        if let Some(t) = self.target {
            rule.target = t;
        }
        if let Some(p) = &self.pattern {
            rule.pattern = p.clone();
        }
        if let Some(r) = &self.replace {
            rule.replace = r.clone();
        }
        rule.regex = self.regex.unwrap_or(rule.regex);
        rule.enabled = self.enabled.unwrap_or(rule.enabled);
        rule.in_scope_only = self.in_scope_only.unwrap_or(rule.in_scope_only);
        if let Some(n) = &self.note {
            rule.note = n.trim().to_string();
        }
        check(&rule)?;
        Ok(rule)
    }

    /// A new rule from these fields: a target and something to match are required.
    pub fn new_rule(&self) -> Result<Rule, String> {
        let target = self.target.ok_or("a rule needs a target: request_line, request_header, request_body, response_header or response_body")?;
        let blank = Rule { id: 0, target, pattern: String::new(), replace: String::new(), regex: false, enabled: true, in_scope_only: false, note: String::new() };
        self.apply_to(blank)
    }
}

const MAX_PATTERN: usize = 4000;

/// Checks a rule: something to match, sane sizes, a regex that compiles.
pub fn check(rule: &Rule) -> Result<(), String> {
    if rule.pattern.is_empty() {
        return Err("a rule needs something to match".into());
    }
    if rule.pattern.len() > MAX_PATTERN || rule.replace.len() > 64 * 1024 || rule.note.len() > 200 {
        return Err("the rule is too long".into());
    }
    compile(rule).map(|_| ())
}

fn compile(rule: &Rule) -> Result<Regex, String> {
    let pattern = if rule.regex { rule.pattern.clone() } else { regex::escape(&rule.pattern) };
    let lines = matches!(rule.target, Target::RequestHeader | Target::ResponseHeader);
    RegexBuilder::new(&pattern)
        .multi_line(lines)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| format!("the pattern is not a valid regular expression: {}", e.to_string().lines().last().unwrap_or("")))
}

impl Rule {
    /// How the rule is named in an exchange's record.
    pub fn label(&self) -> String {
        let what = if self.note.is_empty() { format!("{}: {}", self.target.label(), self.pattern.chars().take(60).collect::<String>()) } else { self.note.clone() };
        format!("#{} {what}", self.id)
    }
}

struct Compiled {
    rule: Rule,
    re: Regex,
}

/// The enabled rules, compiled, as the proxy uses them.
#[derive(Default)]
pub struct RuleSet {
    rules: Vec<Compiled>,
}

impl RuleSet {
    pub fn new(rules: &[Rule]) -> Arc<Self> {
        let rules = rules
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| match compile(r) {
                Ok(re) => Some(Compiled { rule: r.clone(), re }),
                Err(e) => {
                    tracing::warn!("match-and-replace rule {} skipped: {e}", r.id);
                    None
                }
            })
            .collect();
        Arc::new(Self { rules })
    }

    fn active(&self, target: Target, in_scope: bool) -> impl Iterator<Item = &Compiled> {
        self.rules.iter().filter(move |c| c.rule.target == target && (in_scope || !c.rule.in_scope_only))
    }

    pub fn has(&self, target: Target, in_scope: bool) -> bool {
        self.active(target, in_scope).next().is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Runs the rules for `target` over `text`. Returns the labels of the rules that changed it.
    fn run(&self, target: Target, in_scope: bool, text: &mut Vec<u8>) -> Vec<String> {
        let mut applied = vec![];
        for c in self.active(target, in_scope) {
            let out = if c.rule.regex { c.re.replace_all(text, c.rule.replace.as_bytes()) } else { c.re.replace_all(text, NoExpand(c.rule.replace.as_bytes())) };
            if out[..] != text[..] {
                *text = out.into_owned();
                applied.push(c.rule.label());
            }
        }
        applied
    }

    /// Changes a request line (`METHOD /target`). A result that is not a
    /// valid request line is not used.
    pub fn request_line(&self, in_scope: bool, method: &mut String, target: &mut String) -> Vec<String> {
        if !self.has(Target::RequestLine, in_scope) {
            return vec![];
        }
        let mut line = format!("{method} {target}").into_bytes();
        let applied = self.run(Target::RequestLine, in_scope, &mut line);
        if applied.is_empty() {
            return applied;
        }
        let line = String::from_utf8_lossy(&line).into_owned();
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next(), parts.next()) {
            (Some(m), Some(t), None) if http::Method::from_bytes(m.as_bytes()).is_ok() && t.starts_with('/') && http::uri::PathAndQuery::try_from(t).is_ok() => {
                *method = m.to_string();
                *target = t.to_string();
                applied
            }
            _ => {
                tracing::warn!("match-and-replace: \"{line}\" is not a request line; the request was left as it was");
                vec![]
            }
        }
    }

    /// Changes headers, seen as `Name: value` lines. Lines that are not
    /// valid headers after the change are left out.
    pub fn headers(&self, target: Target, in_scope: bool, headers: &mut Headers) -> Vec<String> {
        if !self.has(target, in_scope) {
            return vec![];
        }
        let mut text = headers.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n").into_bytes();
        let applied = self.run(target, in_scope, &mut text);
        if !applied.is_empty() {
            *headers = String::from_utf8_lossy(&text)
                .lines()
                .filter_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    let (k, v) = (k.trim(), v.trim());
                    (http::HeaderName::from_bytes(k.as_bytes()).is_ok() && http::HeaderValue::from_str(v).is_ok()).then(|| (k.to_string(), v.to_string()))
                })
                .collect();
        }
        applied
    }

    /// Changes a whole body.
    pub fn body(&self, target: Target, in_scope: bool, body: &mut Vec<u8>) -> Vec<String> {
        self.run(target, in_scope, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i64, target: Target, pattern: &str, replace: &str, regex: bool) -> Rule {
        Rule { id, target, pattern: pattern.into(), replace: replace.into(), regex, enabled: true, in_scope_only: false, note: String::new() }
    }

    #[test]
    fn headers_change_add_and_disappear() {
        let set = RuleSet::new(&[
            rule(1, Target::RequestHeader, r"(?i)^user-agent: (.*)$", "User-Agent: $1 (tested)", true),
            rule(2, Target::RequestHeader, "^X-Debug: .*$", "", true),
            rule(3, Target::RequestHeader, "Accept: */*", "Accept: */*\nX-Added: yes", false),
            rule(4, Target::ResponseHeader, "never", "x", false),
        ]);
        let mut h: Headers = vec![("user-agent".into(), "Browser/1".into()), ("X-Debug".into(), "1".into()), ("Accept".into(), "*/*".into())];
        let applied = set.headers(Target::RequestHeader, false, &mut h);
        assert_eq!(h, vec![("User-Agent".into(), "Browser/1 (tested)".into()), ("Accept".into(), "*/*".into()), ("X-Added".into(), "yes".into())]);
        assert_eq!(applied.len(), 3);
        assert!(applied[0].starts_with("#1 request header: "));
    }

    #[test]
    fn literal_text_is_not_a_pattern_and_scope_is_respected() {
        let mut r = rule(1, Target::RequestBody, "a.c", "$1", false);
        r.in_scope_only = true;
        let set = RuleSet::new(&[r]);
        let mut body = b"abc a.c".to_vec();
        assert!(set.body(Target::RequestBody, false, &mut body).is_empty(), "in-scope only");
        assert_eq!(set.body(Target::RequestBody, true, &mut body).len(), 1);
        assert_eq!(body, b"abc $1");
    }

    #[test]
    fn request_lines_stay_valid() {
        let set = RuleSet::new(&[rule(1, Target::RequestLine, r"^GET /v1/(\S+)", "POST /v2/$1", true)]);
        let (mut m, mut t) = ("GET".to_string(), "/v1/items?id=1".to_string());
        assert_eq!(set.request_line(true, &mut m, &mut t).len(), 1);
        assert_eq!((m.as_str(), t.as_str()), ("POST", "/v2/items?id=1"));
        let bad = RuleSet::new(&[rule(1, Target::RequestLine, "GET /", "GET nowhere", false)]);
        let (mut m, mut t) = ("GET".to_string(), "/".to_string());
        assert!(bad.request_line(true, &mut m, &mut t).is_empty());
        assert_eq!(t, "/");
    }

    #[test]
    fn rules_are_checked() {
        let input = RuleInput { target: Some(Target::ResponseBody), pattern: Some("(".into()), regex: Some(true), ..Default::default() };
        assert!(input.new_rule().unwrap_err().contains("regular expression"));
        assert!(RuleInput { target: Some(Target::ResponseBody), ..Default::default() }.new_rule().is_err());
        assert!(RuleInput { pattern: Some("x".into()), ..Default::default() }.new_rule().is_err());
        let ok = RuleInput { target: Some(Target::ResponseBody), pattern: Some("(".into()), ..Default::default() }.new_rule().unwrap();
        assert!(ok.enabled && !ok.regex);
        let off = RuleInput { enabled: Some(false), ..Default::default() }.apply_to(ok).unwrap();
        assert!(RuleSet::new(&[off]).is_empty(), "disabled rules do nothing");
    }
}
