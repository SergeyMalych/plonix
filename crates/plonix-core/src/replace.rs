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

use crate::model::{Exchange, Headers};
use crate::query::Query;
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

/// What a rule does. Header kinds work on one header by name, so they need
/// no pattern; `Replace` finds text and puts other text in its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Replace,
    /// Adds the header when the message does not carry it yet.
    AddHeader,
    /// Gives the header this value, adding it when it is missing.
    SetHeader,
    /// Gives the header this value, only when the message carries it.
    ChangeHeader,
    /// Takes the header out.
    RemoveHeader,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Replace => "replace",
            Kind::AddHeader => "add_header",
            Kind::SetHeader => "set_header",
            Kind::ChangeHeader => "change_header",
            Kind::RemoveHeader => "remove_header",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [Kind::Replace, Kind::AddHeader, Kind::SetHeader, Kind::ChangeHeader, Kind::RemoveHeader].into_iter().find(|k| k.as_str() == s)
    }

    fn on_header(self) -> bool {
        self != Kind::Replace
    }
}

/// Where traffic comes from: the browser through the proxy, the Bench (and
/// Run and Access check), or Scans (and crawls and extensions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Browser,
    Bench,
    Scans,
}

/// A rule as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: i64,
    #[serde(default)]
    pub kind: Kind,
    pub target: Target,
    /// Text to find: literal, or a regular expression when `regex` is set.
    /// For a header kind, the header's name.
    pub pattern: String,
    /// What replaces each match; with `regex`, `$1` or `${name}` insert captures.
    /// For a header kind, the header's value.
    pub replace: String,
    pub regex: bool,
    pub enabled: bool,
    /// Only change traffic to hosts accepted into scope.
    pub in_scope_only: bool,
    #[serde(default)]
    pub note: String,
    /// Traffic from the browser, through the proxy.
    #[serde(default = "yes")]
    pub browser: bool,
    /// Requests sent from the Bench, Run and Access check.
    #[serde(default)]
    pub bench: bool,
    /// Requests sent by Scans, crawls and extensions.
    #[serde(default)]
    pub scans: bool,
    /// Only when the exchange matches this traffic search (`host:api.example.com
    /// method:POST`); empty is always.
    #[serde(default)]
    pub when: String,
}

fn yes() -> bool {
    true
}

/// A new rule, or the fields of a rule to change.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RuleInput {
    #[serde(default)]
    pub kind: Option<Kind>,
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
    #[serde(default)]
    pub browser: Option<bool>,
    #[serde(default)]
    pub bench: Option<bool>,
    #[serde(default)]
    pub scans: Option<bool>,
    #[serde(default)]
    pub when: Option<String>,
}

impl RuleInput {
    /// Applies these fields to `rule` and checks the result.
    pub fn apply_to(&self, mut rule: Rule) -> Result<Rule, String> {
        if let Some(k) = self.kind {
            rule.kind = k;
        }
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
        rule.browser = self.browser.unwrap_or(rule.browser);
        rule.bench = self.bench.unwrap_or(rule.bench);
        rule.scans = self.scans.unwrap_or(rule.scans);
        if let Some(n) = &self.note {
            rule.note = n.trim().to_string();
        }
        if let Some(w) = &self.when {
            rule.when = w.trim().to_string();
        }
        if rule.kind.on_header() {
            rule.pattern = rule.pattern.trim().to_string();
            rule.replace = rule.replace.trim().to_string();
            rule.regex = false;
        }
        check(&rule)?;
        Ok(rule)
    }

    /// A new rule from these fields: a target and something to match are
    /// required. A header kind changes request headers unless told otherwise.
    pub fn new_rule(&self) -> Result<Rule, String> {
        let header_kind = self.kind.is_some_and(Kind::on_header);
        let target = match self.target {
            Some(t) => t,
            None if header_kind => Target::RequestHeader,
            None => return Err("a rule needs a target: request_line, request_header, request_body, response_header or response_body".into()),
        };
        let blank = Rule {
            id: 0,
            kind: Kind::Replace,
            target,
            pattern: String::new(),
            replace: String::new(),
            regex: false,
            enabled: true,
            in_scope_only: false,
            note: String::new(),
            browser: true,
            bench: false,
            scans: false,
            when: String::new(),
        };
        self.apply_to(blank)
    }
}

const MAX_PATTERN: usize = 4000;

/// Checks a rule: something to match, sane sizes, a regex that compiles, a
/// header name and value that can be sent, a condition that parses.
pub fn check(rule: &Rule) -> Result<(), String> {
    if rule.pattern.is_empty() {
        return Err(if rule.kind.on_header() { "a header rule needs the header's name".into() } else { "a rule needs something to match".into() });
    }
    if rule.pattern.len() > MAX_PATTERN || rule.replace.len() > 64 * 1024 || rule.note.len() > 200 || rule.when.len() > 1000 {
        return Err("the rule is too long".into());
    }
    if rule.kind.on_header() {
        if !matches!(rule.target, Target::RequestHeader | Target::ResponseHeader) {
            return Err("a header rule changes request headers or response headers".into());
        }
        if http::HeaderName::from_bytes(rule.pattern.as_bytes()).is_err() {
            return Err(format!("\"{}\" is not a header name", rule.pattern));
        }
        if rule.kind != Kind::RemoveHeader && http::HeaderValue::from_str(&rule.replace).is_err() {
            return Err("the header value cannot hold line breaks or control characters".into());
        }
    }
    if !(rule.browser || rule.bench || rule.scans) {
        return Err("a rule needs somewhere to apply: the browser, the Bench or Scans".into());
    }
    condition(rule)?;
    compile(rule).map(|_| ())
}

fn condition(rule: &Rule) -> Result<Option<Query>, String> {
    if rule.when.is_empty() {
        return Ok(None);
    }
    Query::parse(&rule.when).map(Some).map_err(|e| format!("the condition is not a traffic search: {e}"))
}

fn compile(rule: &Rule) -> Result<Regex, String> {
    let pattern = if rule.regex && !rule.kind.on_header() { rule.pattern.clone() } else { regex::escape(&rule.pattern) };
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
        let what = if !self.note.is_empty() {
            self.note.clone()
        } else {
            let name = self.pattern.chars().take(60).collect::<String>();
            match self.kind {
                Kind::Replace => format!("{}: {name}", self.target.label()),
                Kind::AddHeader => format!("add header {name}"),
                Kind::SetHeader | Kind::ChangeHeader => format!("change header {name}"),
                Kind::RemoveHeader => format!("remove header {name}"),
            }
        };
        format!("#{} {what}", self.id)
    }

    fn reaches(&self, reach: Reach) -> bool {
        match reach {
            Reach::Browser => self.browser,
            Reach::Bench => self.bench,
            Reach::Scans => self.scans,
        }
    }
}

struct Compiled {
    rule: Rule,
    re: Regex,
    when: Option<Query>,
}

/// What a rule looks at to decide whether it applies: where the traffic came
/// from, whether its host is in scope, and the exchange so far (for the
/// rule's condition; a request has no status yet).
#[derive(Clone, Copy)]
pub struct Passing<'a> {
    pub reach: Reach,
    pub in_scope: bool,
    pub ex: &'a Exchange,
}

/// The enabled rules, compiled, as the proxy and the engine use them.
#[derive(Default)]
pub struct RuleSet {
    rules: Vec<Compiled>,
}

impl RuleSet {
    pub fn new(rules: &[Rule]) -> Arc<Self> {
        let rules = rules
            .iter()
            .filter(|r| r.enabled)
            .filter_map(|r| match (compile(r), condition(r)) {
                (Ok(re), Ok(when)) => Some(Compiled { rule: r.clone(), re, when }),
                (Err(e), _) | (_, Err(e)) => {
                    tracing::warn!("match-and-replace rule {} skipped: {e}", r.id);
                    None
                }
            })
            .collect();
        Arc::new(Self { rules })
    }

    fn active<'s>(&'s self, target: Target, ctx: Passing<'s>) -> impl Iterator<Item = &'s Compiled> {
        self.rules.iter().filter(move |c| {
            c.rule.target == target
                && (ctx.in_scope || !c.rule.in_scope_only)
                && c.rule.reaches(ctx.reach)
                && c.when.as_ref().is_none_or(|q| q.matches(ctx.ex, ctx.in_scope))
        })
    }

    pub fn has(&self, target: Target, ctx: Passing) -> bool {
        self.active(target, ctx).next().is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Whether any rule applies to traffic from `reach` at all.
    pub fn reaches(&self, reach: Reach) -> bool {
        self.rules.iter().any(|c| c.rule.reaches(reach))
    }

    fn replace(c: &Compiled, text: &mut Vec<u8>) -> bool {
        let out = if c.rule.regex { c.re.replace_all(text, c.rule.replace.as_bytes()) } else { c.re.replace_all(text, NoExpand(c.rule.replace.as_bytes())) };
        if out[..] != text[..] {
            *text = out.into_owned();
            true
        } else {
            false
        }
    }

    /// Runs the rules for `target` over `text`. Returns the labels of the rules that changed it.
    fn run(&self, target: Target, ctx: Passing, text: &mut Vec<u8>) -> Vec<String> {
        self.active(target, ctx).filter(|c| Self::replace(c, text)).map(|c| c.rule.label()).collect()
    }

    /// Changes a request line (`METHOD /target`). A result that is not a
    /// valid request line is not used.
    pub fn request_line(&self, ctx: Passing, method: &mut String, target: &mut String) -> Vec<String> {
        if !self.has(Target::RequestLine, ctx) {
            return vec![];
        }
        let mut line = format!("{method} {target}").into_bytes();
        let applied = self.run(Target::RequestLine, ctx, &mut line);
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

    /// Changes headers, in rule order. Text rules see the headers as
    /// `Name: value` lines; lines that are not valid headers after the
    /// change are left out.
    pub fn headers(&self, target: Target, ctx: Passing, headers: &mut Headers) -> Vec<String> {
        let mut applied = vec![];
        for c in self.active(target, ctx) {
            let name = &c.rule.pattern;
            let same = |k: &String| k.eq_ignore_ascii_case(name);
            let changed = match c.rule.kind {
                Kind::Replace => {
                    let mut text = headers.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n").into_bytes();
                    let changed = Self::replace(c, &mut text);
                    if changed {
                        *headers = String::from_utf8_lossy(&text)
                            .lines()
                            .filter_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                let (k, v) = (k.trim(), v.trim());
                                (http::HeaderName::from_bytes(k.as_bytes()).is_ok() && http::HeaderValue::from_str(v).is_ok()).then(|| (k.to_string(), v.to_string()))
                            })
                            .collect();
                    }
                    changed
                }
                Kind::AddHeader => {
                    let missing = !headers.iter().any(|(k, _)| same(k));
                    if missing {
                        headers.push((name.clone(), c.rule.replace.clone()));
                    }
                    missing
                }
                Kind::SetHeader | Kind::ChangeHeader => match headers.iter().position(|(k, _)| same(k)) {
                    Some(first) => {
                        let before = headers.len();
                        let differs = headers[first].1 != c.rule.replace;
                        headers[first].1 = c.rule.replace.clone();
                        let mut i = 0;
                        headers.retain(|(k, _)| {
                            i += 1;
                            i - 1 == first || !same(k)
                        });
                        differs || headers.len() != before
                    }
                    None if c.rule.kind == Kind::SetHeader => {
                        headers.push((name.clone(), c.rule.replace.clone()));
                        true
                    }
                    None => false,
                },
                Kind::RemoveHeader => {
                    let before = headers.len();
                    headers.retain(|(k, _)| !same(k));
                    headers.len() != before
                }
            };
            if changed {
                applied.push(c.rule.label());
            }
        }
        applied
    }

    /// Changes a whole body.
    pub fn body(&self, target: Target, ctx: Passing, body: &mut Vec<u8>) -> Vec<String> {
        self.run(target, ctx, body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: i64, target: Target, pattern: &str, replace: &str, regex: bool) -> Rule {
        Rule {
            id,
            kind: Kind::Replace,
            target,
            pattern: pattern.into(),
            replace: replace.into(),
            regex,
            enabled: true,
            in_scope_only: false,
            note: String::new(),
            browser: true,
            bench: false,
            scans: false,
            when: String::new(),
        }
    }

    fn header(id: i64, kind: Kind, name: &str, value: &str) -> Rule {
        Rule { kind, ..rule(id, Target::RequestHeader, name, value, false) }
    }

    static EX: std::sync::LazyLock<Exchange> = std::sync::LazyLock::new(Exchange::default);

    fn ctx(in_scope: bool) -> Passing<'static> {
        Passing { reach: Reach::Browser, in_scope, ex: &EX }
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
        let applied = set.headers(Target::RequestHeader, ctx(false), &mut h);
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
        assert!(set.body(Target::RequestBody, ctx(false), &mut body).is_empty(), "in-scope only");
        assert_eq!(set.body(Target::RequestBody, ctx(true), &mut body).len(), 1);
        assert_eq!(body, b"abc $1");
    }

    #[test]
    fn request_lines_stay_valid() {
        let set = RuleSet::new(&[rule(1, Target::RequestLine, r"^GET /v1/(\S+)", "POST /v2/$1", true)]);
        let (mut m, mut t) = ("GET".to_string(), "/v1/items?id=1".to_string());
        assert_eq!(set.request_line(ctx(true), &mut m, &mut t).len(), 1);
        assert_eq!((m.as_str(), t.as_str()), ("POST", "/v2/items?id=1"));
        let bad = RuleSet::new(&[rule(1, Target::RequestLine, "GET /", "GET nowhere", false)]);
        let (mut m, mut t) = ("GET".to_string(), "/".to_string());
        assert!(bad.request_line(ctx(true), &mut m, &mut t).is_empty());
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

    #[test]
    fn header_kinds_add_change_and_remove() {
        let set = RuleSet::new(&[
            header(1, Kind::AddHeader, "X-Bug-Bounty", "sergey"),
            header(2, Kind::AddHeader, "Accept", "text/html"),
            header(3, Kind::ChangeHeader, "user-agent", "Plonix-Research"),
            header(4, Kind::ChangeHeader, "X-Missing", "1"),
            header(5, Kind::SetHeader, "X-Api-Version", "2"),
            header(6, Kind::RemoveHeader, "if-none-match", ""),
        ]);
        let mut h: Headers = vec![("Accept".into(), "*/*".into()), ("User-Agent".into(), "Browser/1".into()), ("If-None-Match".into(), "\"a\"".into())];
        let applied = set.headers(Target::RequestHeader, ctx(true), &mut h);
        assert_eq!(
            h,
            vec![("Accept".into(), "*/*".into()), ("User-Agent".into(), "Plonix-Research".into()), ("X-Bug-Bounty".into(), "sergey".into()), ("X-Api-Version".into(), "2".into())]
        );
        assert_eq!(applied, ["#1 add header X-Bug-Bounty", "#3 change header user-agent", "#5 change header X-Api-Version", "#6 remove header if-none-match"]);
        // A second pass changes nothing: the headers are already as the rules want.
        assert!(set.headers(Target::RequestHeader, ctx(true), &mut h).is_empty());
    }

    #[test]
    fn rules_apply_only_where_asked_and_when_their_condition_holds() {
        let mut r = header(1, Kind::AddHeader, "X-Test", "1");
        r.bench = true;
        r.browser = false;
        r.when = "host:api.example.com method:POST".into();
        let set = RuleSet::new(&[r]);
        let post = Exchange { host: "api.example.com".into(), method: "POST".into(), ..Default::default() };
        let get = Exchange { method: "GET".into(), ..post.clone() };
        let run = |reach, ex: &Exchange| {
            let mut h = Headers::new();
            set.headers(Target::RequestHeader, Passing { reach, in_scope: true, ex }, &mut h).len()
        };
        assert_eq!(run(Reach::Bench, &post), 1);
        assert_eq!(run(Reach::Browser, &post), 0, "not on browser traffic");
        assert_eq!(run(Reach::Scans, &post), 0, "not on Scans");
        assert_eq!(run(Reach::Bench, &get), 0, "the condition does not hold");
        assert!(set.reaches(Reach::Bench) && !set.reaches(Reach::Browser));
    }

    #[test]
    fn header_rules_are_checked() {
        let add = |name: &str, value: &str| RuleInput { kind: Some(Kind::AddHeader), pattern: Some(name.into()), replace: Some(value.into()), ..Default::default() }.new_rule();
        let ok = add(" X-Ok ", " yes ").unwrap();
        assert_eq!((ok.target, ok.pattern.as_str(), ok.replace.as_str()), (Target::RequestHeader, "X-Ok", "yes"));
        assert!(add("Bad Name", "x").unwrap_err().contains("not a header name"));
        assert!(add("X-Ok", "a\r\nInjected: 1").is_err());
        assert!(add("", "x").unwrap_err().contains("header's name"));
        let body = RuleInput { kind: Some(Kind::RemoveHeader), target: Some(Target::RequestBody), pattern: Some("X".into()), ..Default::default() };
        assert!(body.new_rule().is_err());
        let bad_when = RuleInput { when: Some("status:abc".into()), ..Default::default() }.apply_to(ok.clone());
        assert!(bad_when.unwrap_err().contains("condition"));
        let nowhere = RuleInput { browser: Some(false), ..Default::default() }.apply_to(ok);
        assert!(nowhere.unwrap_err().contains("somewhere to apply"));
    }
}
