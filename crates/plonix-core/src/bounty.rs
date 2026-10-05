//! Bug bounty and vulnerability disclosure programs.
//!
//! A program says which assets may be tested and under which rules. Plonix
//! turns it into settings it already enforces: in-scope assets become accepted
//! scope rules, listed exclusions become rejected ones, and the rules of
//! engagement (a request rate, headers every request must carry, no automated
//! testing, no disruptive tests) become a [`Guard`] the engine applies at its
//! single send choke point. Nothing Plonix sends can then go outside what the
//! program allows.
//!
//! Programs come from a bug bounty platform (see [`crate::platform`]) or from
//! policy text the user pastes, which [`parse_policy`] reads. Either way the
//! user reviews the result before it is applied.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::detect::clean;
use crate::scope::{self, Decision, Rule};

pub const MAX_ASSETS: usize = 2_000;
pub const MAX_HEADERS: usize = 8;
pub const MAX_NOT_ACCEPTED: usize = 60;
/// Fastest request rate a program can set, per second.
pub const MAX_RATE: f64 = 1_000.0;

/// What kind of thing an asset is. Only web assets become scope rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    /// A host or URL: `app.example.com`, `https://api.example.com/v2`.
    Web,
    /// A domain and all its subdomains: `*.example.com`.
    Wildcard,
    /// One IP address.
    Ip,
    /// An IP range: `203.0.113.0/24`.
    Cidr,
    /// A mobile app (store id or package).
    Mobile,
    /// A source code repository.
    Source,
    /// Anything else: hardware, executables, other services.
    Other,
}

impl AssetKind {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "web" | "url" | "domain" | "api" => AssetKind::Web,
            "wildcard" => AssetKind::Wildcard,
            "ip" => AssetKind::Ip,
            "cidr" => AssetKind::Cidr,
            "mobile" => AssetKind::Mobile,
            "source" => AssetKind::Source,
            _ => AssetKind::Other,
        }
    }

    /// Whether Plonix can test this kind of asset through its proxy and sends.
    pub fn testable(self) -> bool {
        matches!(self, AssetKind::Web | AssetKind::Wildcard | AssetKind::Ip | AssetKind::Cidr)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Asset {
    /// As the program writes it: `*.example.com`, `https://app.example.com`.
    pub identifier: String,
    pub kind: AssetKind,
    /// In scope (true) or listed as out of scope (false).
    pub in_scope: bool,
    #[serde(default)]
    pub bounty: bool,
    /// The program's note about this asset.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instruction: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub max_severity: String,
}

/// A header the program wants on every request to its assets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequiredHeader {
    pub name: String,
    pub value: String,
    /// The program's text had a placeholder (`<your username>`) that the
    /// user still has to fill in. Such a header is not sent until they do.
    #[serde(default)]
    pub needs_value: bool,
}

/// A program's rules of engagement, as Plonix enforces them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Rules {
    /// Most requests per second Plonix sends to the program's assets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_per_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<RequiredHeader>,
    /// No automated testing: Scans, crawls and Bench runs are off.
    #[serde(default)]
    pub no_automation: bool,
    /// No disruptive tests: intrusive scan checks are off and cannot be picked.
    #[serde(default)]
    pub no_intrusive: bool,
    /// Kinds of report the program does not accept, as it words them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_accepted: Vec<String>,
}

/// One program, ready to apply to a project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Program {
    /// `[a-z0-9-]`, unique per platform: the platform's handle, or a slug.
    pub id: String,
    pub name: String,
    /// The platform pack it came from, or `pasted`.
    pub platform: String,
    /// The program's page, for the user to open.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default)]
    pub bounty: bool,
    pub assets: Vec<Asset>,
    #[serde(default)]
    pub rules: Rules,
    /// When it was fetched or pasted, in ms.
    #[serde(default)]
    pub synced_at: i64,
}

impl Program {
    /// The note scope rules made from this program carry, so a re-sync can
    /// replace exactly those rules.
    pub fn rule_note(&self) -> String {
        format!("program:{}", self.key())
    }

    pub fn key(&self) -> String {
        format!("{}/{}", self.platform, self.id)
    }

    /// Checks a program that came from a client or a platform before it is
    /// stored or applied.
    pub fn validate(&self) -> Result<()> {
        if self.id.is_empty() || self.id.len() > 100 || !self.id.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)) {
            bail!("program id `{}` must be 1-100 letters, digits, `-`, `_` or `.`", clean(&self.id, 100));
        }
        if self.platform.is_empty() || self.platform.len() > 64 || !self.platform.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            bail!("platform `{}` must be 1-64 of [a-z0-9-]", clean(&self.platform, 64));
        }
        crate::detect::check_text(&self.name, 200, false).map_err(|e| anyhow::anyhow!("name: {e}"))?;
        if self.assets.len() > MAX_ASSETS {
            bail!("a program can list at most {MAX_ASSETS} assets");
        }
        for a in &self.assets {
            crate::detect::check_text(&a.identifier, 500, false).map_err(|e| anyhow::anyhow!("asset: {e}"))?;
        }
        let r = &self.rules;
        if let Some(rate) = r.rate_per_second
            && !(rate > 0.0 && rate <= MAX_RATE)
        {
            bail!("the request rate must be above 0 and at most {MAX_RATE} per second");
        }
        if r.headers.len() > MAX_HEADERS {
            bail!("at most {MAX_HEADERS} required headers");
        }
        for h in &r.headers {
            if !valid_header_name(&h.name) {
                bail!("`{}` is not a valid header name", clean(&h.name, 60));
            }
            if h.value.len() > 500 || h.value.chars().any(|c| c.is_control()) {
                bail!("the value of {} must be one line of at most 500 characters", h.name);
            }
        }
        if r.not_accepted.len() > MAX_NOT_ACCEPTED {
            bail!("at most {MAX_NOT_ACCEPTED} not-accepted items");
        }
        Ok(())
    }

    /// The scope rules this program asks for: accepted rules for in-scope web
    /// assets and rejected ones for listed exclusions. Assets Plonix cannot
    /// express as a host rule (mobile apps, `*.example.*`) are left out.
    pub fn scope_rules(&self, now: i64) -> Vec<Rule> {
        let note = self.rule_note();
        let mut out: Vec<Rule> = vec![];
        for a in self.assets.iter().filter(|a| a.kind.testable()) {
            for target in scope_targets(&a.identifier, a.kind) {
                let decision = if a.in_scope { Decision::Accepted } else { Decision::Rejected };
                // One rule per pattern; an exclusion wins over an inclusion of the same pattern.
                if let Some(prev) = out.iter_mut().find(|r| r.pattern == target.0) {
                    if decision == Decision::Rejected {
                        prev.decision = Decision::Rejected;
                    }
                    prev.include_subdomains |= target.1;
                    continue;
                }
                out.push(Rule { pattern: target.0, include_subdomains: target.1, decision, created_at: now, note: note.clone() });
            }
        }
        out
    }

    /// Headers ready to send: those with a value filled in.
    pub fn headers_to_send(&self) -> Vec<(String, String)> {
        self.rules.headers.iter().filter(|h| !h.needs_value && !h.value.is_empty()).map(|h| (h.name.clone(), h.value.clone())).collect()
    }
}

fn valid_header_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 100 && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// The host patterns an asset identifier stands for, each with whether it
/// covers subdomains. Identifiers sometimes list several targets at once
/// (`a.example.com, b.example.com`).
pub fn scope_targets(identifier: &str, kind: AssetKind) -> Vec<(String, bool)> {
    identifier
        .split([',', ' ', '\n', '\t', ';'])
        .filter(|s| !s.is_empty())
        .filter_map(|part| {
            let trimmed = part.split_once("://").map(|(_, r)| r).unwrap_or(part);
            if kind == AssetKind::Cidr || (trimmed.contains('/') && is_cidr(trimmed.split(['?', '#']).next().unwrap_or(""))) {
                let c = trimmed.split(['?', '#']).next().unwrap_or("");
                return is_cidr(c).then(|| (c.to_string(), false));
            }
            let host = scope::normalize_host(part);
            let (base, wild) = match host.strip_prefix("*.") {
                Some(b) => (b.to_string(), true),
                None => (host.clone(), kind == AssetKind::Wildcard),
            };
            let base = base.trim_start_matches('.').to_string();
            (is_host(&base) || is_ip(&base)).then_some((base, wild))
        })
        .collect()
}

/// A plain host name: letters, digits, `-` and dots, with at least one dot.
/// Rejects wildcards in the middle (`*.example.*`) and file names.
pub fn is_host(s: &str) -> bool {
    if s.len() > 253 || !s.contains('.') || s.starts_with('.') || s.ends_with('.') || is_ip(s) {
        return false;
    }
    let labels: Vec<&str> = s.split('.').collect();
    let tld = labels.last().copied().unwrap_or("");
    labels.iter().all(|l| !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !l.starts_with('-'))
        && tld.len() >= 2
        && tld.bytes().all(|b| b.is_ascii_lowercase())
        && !NOT_TLDS.contains(&tld)
}

/// File extensions that look like top-level domains in pasted text.
const NOT_TLDS: &[&str] = &[
    "js", "php", "html", "htm", "json", "png", "jpg", "jpeg", "gif", "svg", "md", "txt", "pdf", "zip", "exe", "apk", "ipa", "xml", "css", "asp", "aspx", "jsp", "yaml", "yml", "csv", "doc", "docx", "py", "rb", "sh", "map",
];

pub fn is_ip(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}

pub fn is_cidr(s: &str) -> bool {
    parse_cidr(s).is_some()
}

/// `203.0.113.0/24` → (network, prefix length).
pub fn parse_cidr(s: &str) -> Option<(std::net::IpAddr, u8)> {
    let (ip, len) = s.split_once('/')?;
    let ip: std::net::IpAddr = ip.parse().ok()?;
    let len: u8 = len.parse().ok()?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    (len <= max).then_some((ip, len))
}

/// Whether `host` (an IP address) is inside the range `cidr`.
pub fn cidr_contains(cidr: &str, host: &str) -> bool {
    let Some((net, len)) = parse_cidr(cidr) else { return false };
    let Ok(ip) = host.parse::<std::net::IpAddr>() else { return false };
    match (net, ip) {
        (std::net::IpAddr::V4(n), std::net::IpAddr::V4(i)) => {
            let mask = if len == 0 { 0 } else { u32::MAX << (32 - len) };
            u32::from(n) & mask == u32::from(i) & mask
        }
        (std::net::IpAddr::V6(n), std::net::IpAddr::V6(i)) => {
            let mask = if len == 0 { 0 } else { u128::MAX << (128 - len) };
            u128::from(n) & mask == u128::from(i) & mask
        }
        _ => false,
    }
}

// ---- reading policy text ---------------------------------------------------

/// What Plonix read from a program's policy text. The user reviews it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Reading {
    pub assets: Vec<Asset>,
    pub rules: Rules,
}

static RATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(\d+(?:\.\d+)?)\s*(?:http\s+)?(?:requests?|reqs?|rps|queries)\s*(?:per|/|a|each|every)\s*(second|sec|s|minute|min|m|hour|hr|h)\b").unwrap()
});
static RPS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(\d+(?:\.\d+)?)\s*(?:rps|req/s|requests/s)\b").unwrap());
static HEADER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)(?:^|[\s`'"(])((?:x-[a-z0-9][a-z0-9-]*)|user-agent)\s*:\s*([^\n`"]{1,200})"#).unwrap());
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<[^>]{1,60}>|\[[^\]]{1,60}\]|\{\{?[^}]{1,60}\}\}?|\byour[_ -]?(?:user(?:name)?|handle|alias|email|id)\b|\busername\b").unwrap());
static TRAILER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\s+(?:to|on|in|for|when|with|and|so|while|header|headers)\b").unwrap());
static TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:https?://)?(?:\*\.)?[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?)+(?::\d{1,5})?(?:/[^\s,;)\]'`]*)?|\b\d{1,3}(?:\.\d{1,3}){3}(?:/\d{1,2})?\b").unwrap()
});

/// Domains of the platforms themselves and common references, which policy
/// text mentions but which are never the program's assets.
const NOT_ASSETS: &[&str] = &[
    "hackerone.com", "bugcrowd.com", "intigriti.com", "yeswehack.com", "owasp.org", "cve.mitre.org", "first.org", "disclose.io", "securitytxt.org", "rfc-editor.org", "wearehackerone.com", "bugcrowdninja.com",
    "intigriti.me", "yeswehack.ninja", "cwe.mitre.org", "nvd.nist.gov", "w3.org", "example.com", "example.org", "example.net",
];

#[derive(Clone, Copy, PartialEq)]
enum Section {
    None,
    In,
    Out,
}

fn heading_section(line: &str) -> Option<Section> {
    let l = line.to_ascii_lowercase();
    let short = l.trim().trim_matches(|c: char| !c.is_alphanumeric()).len() <= 60;
    if !short {
        return None;
    }
    let out_words = ["out of scope", "out-of-scope", "not in scope", "exclusion", "excluded", "ineligible", "not eligible", "non-qualifying", "not qualify", "not accepted", "won't accept", "will not accept", "do not test", "don't test"];
    if out_words.iter().any(|w| l.contains(w)) {
        return Some(Section::Out);
    }
    let in_words = ["in scope", "in-scope", "targets", "scope", "assets", "eligible"];
    if in_words.iter().any(|w| l.contains(w)) {
        return Some(Section::In);
    }
    None
}

fn bullet_text(line: &str) -> Option<&str> {
    let t = line.trim();
    let rest = t.strip_prefix(['-', '*', '•', '·', '–']).or_else(|| t.split_once(". ").filter(|(n, _)| n.len() <= 3 && n.bytes().all(|b| b.is_ascii_digit())).map(|(_, r)| r))?;
    Some(rest.trim())
}

/// Reads a program's policy: assets under in-scope and out-of-scope headings,
/// the request rate, required headers, whether automated testing or
/// disruptive tests are banned, and what the program won't accept. It is a
/// careful first reading that the user confirms, not a promise.
pub fn parse_policy(text: &str) -> Reading {
    let mut r = Reading { rules: extract_rules(text), ..Default::default() };
    let mut section = Section::None;
    for line in text.lines().take(5_000) {
        let lower = line.to_ascii_lowercase();
        if let Some(s) = heading_section(line)
            && bullet_text(line).is_none()
        {
            section = s;
            continue;
        }
        let tokens: Vec<&str> = TOKEN.find_iter(line).map(|m| m.as_str()).collect();
        let mut found = false;
        for tok in tokens {
            // Skip e-mail addresses and anything after `@`.
            if let Some(i) = line.find(tok)
                && i > 0
                && line.as_bytes()[i - 1] == b'@'
            {
                continue;
            }
            let kind = if tok.contains("*.") {
                AssetKind::Wildcard
            } else if is_cidr(tok) {
                AssetKind::Cidr
            } else if is_ip(tok) {
                AssetKind::Ip
            } else {
                AssetKind::Web
            };
            let targets = scope_targets(tok, kind);
            let Some((host, _)) = targets.first() else { continue };
            if NOT_ASSETS.iter().any(|d| host == d || scope::is_subdomain_of(host, d)) {
                continue;
            }
            // A line that says "out of scope" next to a host decides it, whatever the section.
            let in_scope = if lower.contains("out of scope") || lower.contains("out-of-scope") || lower.contains("not in scope") || lower.contains("excluded") {
                false
            } else {
                section != Section::Out
            };
            let identifier = tok.trim_end_matches(['.', ':']).to_string();
            found = true;
            if let Some(prev) = r.assets.iter_mut().find(|a| a.identifier == identifier) {
                prev.in_scope &= in_scope;
                continue;
            }
            if r.assets.len() < MAX_ASSETS {
                r.assets.push(Asset { identifier, kind, in_scope, bounty: false, instruction: String::new(), max_severity: String::new() });
            }
        }
        // Under an exclusions heading, a bullet without a host is a kind of
        // report the program won't accept.
        if !found
            && section == Section::Out
            && let Some(item) = bullet_text(line)
            && (3..=200).contains(&item.chars().count())
            && r.rules.not_accepted.len() < MAX_NOT_ACCEPTED
            && !r.rules.not_accepted.iter().any(|x| x == item)
        {
            r.rules.not_accepted.push(clean(item, 200));
        }
    }
    r
}

/// The rules of engagement in a policy text.
pub fn extract_rules(text: &str) -> Rules {
    let mut rules = Rules::default();
    let mut rate: Option<f64> = None;
    let mut keep = |per_second: f64| {
        if per_second > 0.0 && per_second <= MAX_RATE {
            rate = Some(rate.map_or(per_second, |r: f64| r.min(per_second)));
        }
    };
    for c in RATE.captures_iter(text) {
        let n: f64 = c[1].parse().unwrap_or(0.0);
        let per = match c[2].to_ascii_lowercase().chars().next() {
            Some('m') => n / 60.0,
            Some('h') => n / 3600.0,
            _ => n,
        };
        keep(per);
    }
    for c in RPS.captures_iter(text) {
        keep(c[1].parse().unwrap_or(0.0));
    }
    rules.rate_per_second = rate.map(|r| (r * 1000.0).round() / 1000.0);

    for c in HEADER.captures_iter(text) {
        let name = canonical_header(&c[1]);
        // "X-Bug-Bounty: <your username> to all requests": the value ends where the sentence goes on.
        let raw = c[2].trim();
        let raw = TRAILER.find(raw).map_or(raw, |m| &raw[..m.start()]);
        let value = raw.trim().trim_end_matches(['.', ',', ';', ')']).trim().to_string();
        if value.is_empty() || rules.headers.len() >= MAX_HEADERS || rules.headers.iter().any(|h| h.name.eq_ignore_ascii_case(&name)) {
            continue;
        }
        let needs_value = PLACEHOLDER.is_match(&value);
        rules.headers.push(RequiredHeader { name, value: clean(&value, 200), needs_value });
    }

    for sentence in sentences(text) {
        let s = sentence.to_ascii_lowercase();
        let negative = ["not allowed", "not permitted", "prohibited", "forbidden", "do not", "don't", "must not", "never", "disallowed", "no automated", "not be used", "strictly", "banned", "will be disqualified"].iter().any(|n| s.contains(n));
        if !negative {
            continue;
        }
        if ["automated scan", "automated tool", "automated test", "automatic scan", "vulnerability scanner", "scanners", "automated vulnerability", "no automated", "automated traffic", "fuzzing", "brute force", "brute-force"].iter().any(|w| s.contains(w))
            && !s.contains("rate limit")
        {
            rules.no_automation = true;
        }
        if ["denial of service", "dos", "ddos", "load test", "stress test", "flood", "degrade", "disrupt"].iter().any(|w| s.split(|c: char| !c.is_alphanumeric() && c != ' ').any(|part| part.contains(w))) {
            rules.no_intrusive = true;
        }
    }
    rules
}

fn sentences(text: &str) -> impl Iterator<Item = &str> {
    text.split(['.', '\n', '!', ';']).filter(|s| s.trim().len() > 3)
}

fn canonical_header(name: &str) -> String {
    name.split('-')
        .map(|part| {
            let mut c = part.chars();
            match c.next() {
                Some(f) => f.to_ascii_uppercase().to_string() + &c.as_str().to_ascii_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// Fills placeholders in required headers with the user's platform name.
pub fn fill_placeholders(rules: &mut Rules, username: &str) {
    if username.is_empty() || username.chars().any(|c| c.is_control()) {
        return;
    }
    for h in rules.headers.iter_mut().filter(|h| h.needs_value) {
        h.value = PLACEHOLDER.replace_all(&h.value, username).into_owned();
        h.needs_value = false;
    }
}

/// The `Policy:` links and contacts in a security.txt file (RFC 9116).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SecurityTxt {
    pub contact: Vec<String>,
    pub policy: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub expires: String,
}

pub fn parse_security_txt(text: &str) -> SecurityTxt {
    let mut out = SecurityTxt::default();
    for line in text.lines().take(500) {
        let line = line.trim();
        if line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else { continue };
        let v = clean(v.trim(), 500);
        match k.trim().to_ascii_lowercase().as_str() {
            "contact" if out.contact.len() < 10 => out.contact.push(v),
            "policy" if out.policy.len() < 10 => out.policy.push(v),
            "expires" => out.expires = v,
            _ => {}
        }
    }
    out
}

/// Readable text from an HTML page: tags dropped, block elements on their own lines.
pub fn html_to_text(html: &str) -> String {
    static SCRIPT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<(script|style|noscript)\b.*?</(script|style|noscript)>").unwrap());
    static BLOCK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<(br|/?p|/?li|/?h[1-6]|/?div|/?tr|/?ul|/?ol|/?section|/?table)\b[^>]*>").unwrap());
    static LI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<li\b[^>]*>").unwrap());
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
    let s = SCRIPT.replace_all(html, "");
    let s = LI.replace_all(&s, "\n- ");
    let s = BLOCK.replace_all(&s, "\n");
    let s = TAG.replace_all(&s, "");
    let s = s.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ");
    s.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

/// A short id for a pasted program: `acme-cloud`.
pub fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            s.push(c);
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-');
    if s.is_empty() { "program".into() } else { s.chars().take(60).collect() }
}

// ---- reading a program from what the user gives -------------------------

/// What the user gave: policy text, a policy page, or a domain whose
/// security.txt points at its policy.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ReadRequest {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub name: String,
}

/// Largest policy page Plonix reads.
const MAX_POLICY_BYTES: u64 = 2 * 1024 * 1024;

/// Fetches a policy page or security.txt as text. Only http(s), a few redirects.
pub fn fetch_text(url: &str) -> Result<String> {
    use std::io::Read;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        bail!("give an address that starts with https://");
    }
    let mut b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout(Duration::from_secs(30)).redirects(3).user_agent(&format!("Plonix/{}", env!("CARGO_PKG_VERSION")));
    if let Some(p) = std::env::var("HTTPS_PROXY").ok().or_else(|| std::env::var("https_proxy").ok()).filter(|p| !p.is_empty()) {
        b = b.proxy(ureq::Proxy::new(&p).map_err(|e| anyhow::anyhow!("invalid HTTPS_PROXY: {e}"))?);
    }
    let resp = match b.build().get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => bail!("{} answered HTTP {code}", clean(url, 80)),
        Err(ureq::Error::Transport(t)) => bail!("could not reach {} ({t})", clean(url, 80)),
    };
    let html = resp.content_type().contains("html");
    let mut body = vec![];
    resp.into_reader().take(MAX_POLICY_BYTES).read_to_end(&mut body)?;
    let text = String::from_utf8_lossy(&body).into_owned();
    Ok(if html { html_to_text(&text) } else { text })
}

/// Reads a program from what the user gave. The result is a draft to review.
pub fn read(req: &ReadRequest, fetch: &dyn Fn(&str) -> Result<String>) -> Result<Program> {
    let (text, url, domain) = if !req.text.trim().is_empty() {
        (req.text.clone(), String::new(), String::new())
    } else if !req.url.trim().is_empty() {
        let url = req.url.trim().to_string();
        (fetch(&url)?, url, String::new())
    } else if !req.domain.trim().is_empty() {
        let domain = scope::normalize_host(req.domain.trim().trim_start_matches("*."));
        if !is_host(&domain) {
            bail!("`{}` is not a domain", clean(&domain, 80));
        }
        let txt = fetch(&format!("https://{domain}/.well-known/security.txt")).or_else(|_| fetch(&format!("https://{domain}/security.txt"))).map_err(|_| {
            anyhow::anyhow!("{domain} has no security.txt. Paste its disclosure policy instead, or give the policy page's address.")
        })?;
        let sec = parse_security_txt(&txt);
        let policy_url = sec.policy.iter().find(|p| p.starts_with("https://")).cloned();
        let policy = match &policy_url {
            Some(u) => fetch(u).unwrap_or_default(),
            None => String::new(),
        };
        (policy, policy_url.unwrap_or_default(), domain)
    } else {
        bail!("paste the program's policy, or give its address or domain");
    };
    let mut reading = parse_policy(&text);
    if reading.assets.is_empty() && !domain.is_empty() {
        reading.assets.push(Asset { identifier: format!("*.{domain}"), kind: AssetKind::Wildcard, in_scope: true, bounty: false, instruction: "From security.txt: the policy lists no assets, so the whole domain is assumed. Check this.".into(), max_severity: String::new() });
    }
    reading.rules.no_intrusive = true;
    let name = if !req.name.trim().is_empty() {
        clean(req.name.trim(), 200)
    } else if !domain.is_empty() {
        domain.clone()
    } else {
        first_heading(&text).or_else(|| reading.assets.iter().find(|a| a.in_scope).map(|a| scope::normalize_host(a.identifier.trim_start_matches("*.")))).unwrap_or_else(|| "Pasted program".into())
    };
    Ok(Program { id: slug(&name), name, platform: "pasted".into(), url, bounty: false, assets: reading.assets, rules: reading.rules, synced_at: crate::model::now_ms() })
}

/// The first short line of a policy, which is usually its title.
fn first_heading(text: &str) -> Option<String> {
    text.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty()).filter(|l| l.chars().count() <= 80 && TOKEN.find(l).is_none()).map(|l| clean(l, 80))
}

// ---- enforcement -----------------------------------------------------------

/// The program in effect for a project, as the engine enforces it.
pub struct Guard {
    pub program: Program,
    headers: Vec<(String, String)>,
    interval: Option<Duration>,
    next: tokio::sync::Mutex<Instant>,
}

impl Guard {
    pub fn new(program: Program) -> Self {
        let interval = program.rules.rate_per_second.filter(|r| *r > 0.0).map(|r| Duration::from_secs_f64(1.0 / r));
        let headers = program.headers_to_send();
        Self { program, headers, interval, next: tokio::sync::Mutex::new(Instant::now()) }
    }

    /// Waits until the program's request rate allows one more request.
    pub async fn pace(&self) {
        let Some(interval) = self.interval else { return };
        let mut next = self.next.lock().await;
        let now = Instant::now();
        if *next > now {
            tokio::time::sleep(*next - now).await;
        }
        *next = Instant::now().max(*next) + interval;
    }

    /// Adds the program's required headers that a request does not carry yet.
    /// Returns whether anything was added.
    pub fn add_headers(&self, headers: &mut Vec<(String, String)>) -> bool {
        let mut added = false;
        for (k, v) in &self.headers {
            if !headers.iter().any(|(hk, _)| hk.eq_ignore_ascii_case(k)) {
                headers.push((k.clone(), v.clone()));
                added = true;
            }
        }
        added
    }

    /// Refuses automated testing when the program bans it.
    pub fn check_automation(&self, what: &str) -> Result<(), String> {
        if self.program.rules.no_automation {
            return Err(format!(
                "{} does not allow automated testing, so {what} is off in this project. Single requests from the Bench still work.",
                self.program.name
            ));
        }
        Ok(())
    }

    pub fn no_intrusive(&self) -> bool {
        self.program.rules.no_intrusive
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program(assets: Vec<Asset>) -> Program {
        Program { id: "acme".into(), name: "Acme Cloud".into(), platform: "pasted".into(), url: String::new(), bounty: true, assets, rules: Rules::default(), synced_at: 0 }
    }

    fn asset(id: &str, kind: AssetKind, in_scope: bool) -> Asset {
        Asset { identifier: id.into(), kind, in_scope, bounty: true, instruction: String::new(), max_severity: String::new() }
    }

    #[test]
    fn assets_become_scope_rules() {
        let p = program(vec![
            asset("*.acme.example", AssetKind::Wildcard, true),
            asset("https://app.acme.example/login", AssetKind::Web, true),
            asset("status.acme.example", AssetKind::Web, false),
            asset("203.0.113.0/24", AssetKind::Cidr, true),
            asset("com.acme.mobile", AssetKind::Mobile, true),
            asset("*.acme.*", AssetKind::Wildcard, true),
            asset("a.example.org, b.example.org", AssetKind::Web, true),
        ]);
        let rules = p.scope_rules(1);
        let got: Vec<(String, bool, Decision)> = rules.iter().map(|r| (r.pattern.clone(), r.include_subdomains, r.decision)).collect();
        assert_eq!(
            got,
            vec![
                ("acme.example".into(), true, Decision::Accepted),
                ("app.acme.example".into(), false, Decision::Accepted),
                ("status.acme.example".into(), false, Decision::Rejected),
                ("203.0.113.0/24".into(), false, Decision::Accepted),
                ("a.example.org".into(), false, Decision::Accepted),
                ("b.example.org".into(), false, Decision::Accepted),
            ]
        );
        assert!(rules.iter().all(|r| r.note == "program:pasted/acme"));
        // The exclusion is more specific than the wildcard, so it wins.
        let set = scope::ScopeRules { rules };
        assert_eq!(set.decide("api.acme.example"), Decision::Accepted);
        assert_eq!(set.decide("status.acme.example"), Decision::Rejected);
        assert_eq!(set.decide("203.0.113.9"), Decision::Accepted);
        assert_eq!(set.decide("203.0.114.9"), Decision::Unknown);
    }

    #[test]
    fn cidr_ranges() {
        assert!(cidr_contains("10.0.0.0/8", "10.200.1.1"));
        assert!(!cidr_contains("10.0.0.0/8", "11.0.0.1"));
        assert!(cidr_contains("2001:db8::/32", "2001:db8::1"));
        assert!(!cidr_contains("10.0.0.0/8", "app.example.com"));
        assert!(!is_cidr("10.0.0.0/33"));
    }

    const POLICY: &str = "Acme Cloud Bug Bounty

In Scope
- *.api.acme.io
- https://app.acme.io
- 198.51.100.0/24

Out of Scope
- status.acme.io
- Missing security headers
- Self-XSS
- Reports from automated tools without a working proof

Rules
Please limit your testing to 5 requests per second. Do not use automated scanners.
Add the header X-Bug-Bounty: <your username> to all requests.
Denial of service testing is strictly prohibited.
Report to security@acme.io. Read https://hackerone.com/acme for details.";

    #[test]
    fn reads_a_policy() {
        let r = parse_policy(POLICY);
        let ids: Vec<(&str, bool)> = r.assets.iter().map(|a| (a.identifier.as_str(), a.in_scope)).collect();
        assert_eq!(ids, vec![("*.api.acme.io", true), ("https://app.acme.io", true), ("198.51.100.0/24", true), ("status.acme.io", false)]);
        assert_eq!(r.rules.rate_per_second, Some(5.0));
        assert!(r.rules.no_automation);
        assert!(r.rules.no_intrusive);
        assert_eq!(r.rules.headers, vec![RequiredHeader { name: "X-Bug-Bounty".into(), value: "<your username>".into(), needs_value: true }]);
        assert_eq!(r.rules.not_accepted, vec!["Missing security headers", "Self-XSS", "Reports from automated tools without a working proof"]);
    }

    #[test]
    fn rates_in_other_units_and_placeholders() {
        assert_eq!(extract_rules("no more than 60 requests per minute").rate_per_second, Some(1.0));
        assert_eq!(extract_rules("keep it under 10 rps, ideally 2 req/s").rate_per_second, Some(2.0));
        assert_eq!(extract_rules("Use automated scanners responsibly.").no_automation, false);
        let mut rules = extract_rules("Set `User-Agent: hackerone-[username]` on your traffic");
        assert_eq!(rules.headers[0].name, "User-Agent");
        assert!(rules.headers[0].needs_value);
        fill_placeholders(&mut rules, "neo");
        assert_eq!(rules.headers[0].value, "hackerone-neo");
        assert!(!rules.headers[0].needs_value);
    }

    #[test]
    fn validation_rejects_bad_headers_and_rates() {
        let mut p = program(vec![]);
        p.rules.headers.push(RequiredHeader { name: "X-Ok".into(), value: "a\r\nInjected: 1".into(), needs_value: false });
        assert!(p.validate().is_err());
        p.rules.headers.clear();
        p.rules.rate_per_second = Some(0.0);
        assert!(p.validate().is_err());
        p.rules.rate_per_second = Some(2.0);
        assert!(p.validate().is_ok());
    }

    #[tokio::test]
    async fn guard_paces_and_adds_headers() {
        let mut p = program(vec![]);
        p.rules.rate_per_second = Some(20.0);
        p.rules.headers = vec![
            RequiredHeader { name: "X-Bug-Bounty".into(), value: "neo".into(), needs_value: false },
            RequiredHeader { name: "X-Later".into(), value: "<you>".into(), needs_value: true },
        ];
        let g = Guard::new(p);
        let start = Instant::now();
        for _ in 0..5 {
            g.pace().await;
        }
        assert!(start.elapsed() >= Duration::from_millis(190), "{:?}", start.elapsed());
        let mut h = vec![("x-bug-bounty".to_string(), "mine".to_string())];
        assert!(!g.add_headers(&mut h));
        let mut h = vec![];
        assert!(g.add_headers(&mut h));
        assert_eq!(h, vec![("X-Bug-Bounty".to_string(), "neo".to_string())]);
    }

    #[test]
    fn reads_from_text_a_page_or_security_txt() {
        let pasted = read(&ReadRequest { text: POLICY.into(), ..Default::default() }, &|_| bail!("no network")).unwrap();
        assert_eq!(pasted.name, "Acme Cloud Bug Bounty");
        assert_eq!(pasted.id, "acme-cloud-bug-bounty");
        assert_eq!(pasted.platform, "pasted");
        assert!(pasted.validate().is_ok());

        let fetch = |url: &str| -> Result<String> {
            match url {
                "https://acme.io/.well-known/security.txt" => Ok("Contact: mailto:s@acme.io\nPolicy: https://acme.io/policy\n".into()),
                "https://acme.io/policy" => Ok("Test only app.acme.io. 2 requests per second.".into()),
                "https://quiet.io/.well-known/security.txt" => Ok("Contact: mailto:s@quiet.io\n".into()),
                _ => bail!("404"),
            }
        };
        let p = read(&ReadRequest { domain: "acme.io".into(), ..Default::default() }, &fetch).unwrap();
        assert_eq!(p.url, "https://acme.io/policy");
        assert_eq!(p.assets[0].identifier, "app.acme.io");
        assert_eq!(p.rules.rate_per_second, Some(2.0));
        let q = read(&ReadRequest { domain: "quiet.io".into(), ..Default::default() }, &fetch).unwrap();
        assert_eq!(q.assets[0].identifier, "*.quiet.io");
        assert!(read(&ReadRequest { domain: "none.io".into(), ..Default::default() }, &fetch).unwrap_err().to_string().contains("no security.txt"));
    }

    #[test]
    fn security_txt_and_html() {
        let s = parse_security_txt("# hi\nContact: mailto:security@acme.io\nPolicy: https://acme.io/security\nExpires: 2027-01-01T00:00:00Z\n");
        assert_eq!(s.policy, vec!["https://acme.io/security"]);
        assert_eq!(s.contact.len(), 1);
        let t = html_to_text("<h2>In scope</h2><ul><li>app.acme.io</li></ul><script>x()</script><p>5 requests&nbsp;per second</p>");
        assert_eq!(t, "In scope\n- app.acme.io\n5 requests per second");
        assert_eq!(slug("Acme Cloud (VDP)"), "acme-cloud-vdp");
    }
}
