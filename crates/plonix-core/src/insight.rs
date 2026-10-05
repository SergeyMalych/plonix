//! What stands out in one request and its response: tokens worth decoding
//! (JWTs, Base64, hex, URL-encoding), personal data and leaked secrets.
//!
//! Insights are computed on demand from an exchange that is already stored,
//! locally, and only ever contain text that the request or response already
//! shows (plus its decoded form). Nothing is sent anywhere.
//!
//! Each check is a [`Detector`]. The engine runs [`builtin`] detectors over
//! *candidates*: every query parameter, header, cookie and form field, each
//! string inside a JSON body, and the body text as a whole. Pattern detectors
//! ([`Pattern`]) are plain data (a linear-time regex, a label, a category), so
//! detectors can later be shipped in rule packs the same way detection rules
//! are.

use std::collections::HashMap;

use base64::Engine as _;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codec;
use crate::model::{Exchange, header};

/// Longest value an insight carries. Longer matches are cut, never extended.
const MAX_VALUE: usize = 4096;
/// Stop after this many distinct insights per exchange.
const MAX_INSIGHTS: usize = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Request,
    Response,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    /// An encoded or structured value worth decoding.
    Decode,
    /// Personal data.
    Pii,
    /// A credential or key that should not be exposed.
    Secret,
    /// Infrastructure detail, such as internal addresses.
    Info,
    /// A note from an installed extension, not from Plonix itself.
    Extension,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Insight {
    /// Detector id, e.g. `jwt`, `base64`, `email`.
    pub kind: String,
    pub category: Category,
    /// Short name for display, e.g. `JWT`.
    pub label: String,
    pub side: Side,
    /// Where it was first seen, e.g. `header Authorization`, `cookie sid`.
    pub location: String,
    /// The text as it appears in the request or response.
    pub value: String,
    /// The decoded form, when the detector can decode it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoded: Option<String>,
    /// Facts worth knowing, e.g. `expired 2026-10-01 12:00 UTC`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// How many times the same value was seen on this side.
    pub count: usize,
}

/// One piece of text to examine and where it came from.
#[derive(Debug, Clone)]
pub struct Candidate<'a> {
    pub side: Side,
    pub location: String,
    pub text: &'a str,
    /// True for a discrete value (a parameter, header, cookie or JSON
    /// string); false for a whole body, where only patterns apply.
    pub token: bool,
}

/// A check that turns candidates into insights.
pub trait Detector: Send + Sync {
    fn id(&self) -> &str;
    fn scan(&self, c: &Candidate, out: &mut Vec<Insight>);
}

fn insight(c: &Candidate, kind: &str, category: Category, label: &str, value: &str) -> Insight {
    Insight {
        kind: kind.into(),
        category,
        label: label.into(),
        side: c.side,
        location: c.location.clone(),
        value: cut(value, MAX_VALUE).to_string(),
        decoded: None,
        notes: vec![],
        count: 1,
    }
}

fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The built-in detectors, compiled once.
pub fn detectors() -> &'static [Box<dyn Detector>] {
    static DETECTORS: std::sync::OnceLock<Vec<Box<dyn Detector>>> = std::sync::OnceLock::new();
    DETECTORS.get_or_init(builtin)
}

/// The built-in detectors, in display order.
pub fn builtin() -> Vec<Box<dyn Detector>> {
    vec![
        Box::new(Jwt::new()),
        Box::new(Base64Text),
        Box::new(HexText),
        Box::new(UrlEncoded),
        Box::new(Pattern::new(
            "aws-access-key",
            "AWS access key",
            Category::Secret,
            r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
            None,
        )),
        Box::new(Pattern::new(
            "private-key",
            "Private key",
            Category::Secret,
            r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----",
            None,
        )),
        Box::new(Pattern::new("google-api-key", "Google API key", Category::Secret, r"\bAIza[0-9A-Za-z_\-]{35}\b", None)),
        Box::new(Pattern::new(
            "github-token",
            "GitHub token",
            Category::Secret,
            r"\b(?:ghp|gho|ghu|ghs|ghr)_[0-9A-Za-z]{36}\b|\bgithub_pat_[0-9A-Za-z_]{60,}\b",
            None,
        )),
        Box::new(Pattern::new("slack-token", "Slack token", Category::Secret, r"\bxox[abposr]-[0-9A-Za-z\-]{10,}\b", None)),
        Box::new(Pattern::new("stripe-key", "Stripe secret key", Category::Secret, r"\b[sr]k_live_[0-9A-Za-z]{20,}\b", None)),
        Box::new(Pattern::new(
            "email",
            "Email address",
            Category::Pii,
            r"\b[A-Za-z0-9._%+\-]{1,64}@[A-Za-z0-9\-]{1,63}(?:\.[A-Za-z0-9\-]{1,63})*\.[A-Za-z]{2,24}\b",
            Some(plausible_email),
        )),
        Box::new(Pattern::new("card-number", "Card number", Category::Pii, r"\b(?:\d[ \-]?){12,18}\d\b", Some(luhn_card))),
        Box::new(Pattern::new(
            "private-ip",
            "Internal IP address",
            Category::Info,
            r"\b(?:10\.(?:\d{1,3}\.){2}\d{1,3}|192\.168\.\d{1,3}\.\d{1,3}|172\.(?:1[6-9]|2\d|3[01])\.\d{1,3}\.\d{1,3})\b",
            Some(valid_ipv4),
        )),
        Box::new(Pattern::new(
            "stack-trace",
            "Stack trace",
            Category::Info,
            r"Traceback \(most recent call last\)|\bat [\w$.<>]+\([\w$-]+\.(?:java|kt|scala):\d+\)|\bat [\w$.<>\[\] ]+ \((?:/|[A-Za-z]:\\|file:)[^()\s]+\.[cm]?[jt]s:\d+:\d+\)|\bin /[\w/.\-]+\.php on line \d+|\bat [\w.`<>]+\(.*\) in [^\s]+\.cs:line \d+",
            None,
        )),
    ]
}

/// A detector defined by data: a regex over any candidate, with an optional
/// check on each match to cut false positives.
pub struct Pattern {
    id: String,
    label: String,
    category: Category,
    re: Regex,
    check: Option<fn(&str) -> bool>,
}

impl Pattern {
    /// Panics on an invalid regex; built-in patterns are covered by tests.
    pub fn new(id: &str, label: &str, category: Category, re: &str, check: Option<fn(&str) -> bool>) -> Self {
        Self { id: id.into(), label: label.into(), category, re: Regex::new(re).expect("built-in pattern"), check }
    }
}

impl Detector for Pattern {
    fn id(&self) -> &str {
        &self.id
    }
    fn scan(&self, c: &Candidate, out: &mut Vec<Insight>) {
        for m in self.re.find_iter(c.text).take(MAX_INSIGHTS) {
            if self.check.is_none_or(|f| f(m.as_str())) {
                out.push(insight(c, &self.id, self.category, &self.label, m.as_str()));
            }
        }
    }
}

fn plausible_email(s: &str) -> bool {
    // Skip asset names such as `logo@2x.png`.
    let tld = s.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    !matches!(tld.as_str(), "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "js" | "css" | "map")
}

fn luhn_card(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) || digits.iter().all(|d| *d == digits[0]) {
        return false;
    }
    // Card numbers start with a known issuer digit.
    if !matches!(digits[0], 2..=6) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| if i % 2 == 1 { if d * 2 > 9 { d * 2 - 9 } else { d * 2 } } else { *d })
        .sum();
    sum % 10 == 0
}

fn valid_ipv4(s: &str) -> bool {
    s.split('.').all(|p| p.parse::<u8>().is_ok())
}

/// JSON Web Tokens: decodes the header and payload. The signature is never
/// checked, and the notes say so.
pub struct Jwt {
    re: Regex,
}

impl Jwt {
    fn new() -> Self {
        Self { re: Regex::new(r"\beyJ[A-Za-z0-9_\-]{5,}\.eyJ[A-Za-z0-9_\-]{5,}\.[A-Za-z0-9_\-]*").unwrap() }
    }
}

impl Detector for Jwt {
    fn id(&self) -> &str {
        "jwt"
    }
    fn scan(&self, c: &Candidate, out: &mut Vec<Insight>) {
        for m in self.re.find_iter(c.text).take(20) {
            let token = m.as_str();
            let mut parts = token.split('.');
            let (Some(h), Some(p), sig) = (parts.next(), parts.next(), parts.next().unwrap_or("")) else { continue };
            let (Some(head), Some(payload)) = (b64_json(h), b64_json(p)) else { continue };
            let mut i = insight(c, "jwt", Category::Decode, "JWT", token);
            let alg = head.get("alg").and_then(Value::as_str).unwrap_or("?").to_string();
            i.notes.push(format!("alg {alg}"));
            if alg.eq_ignore_ascii_case("none") || sig.is_empty() {
                i.notes.push("unsigned: anyone can forge it".into());
            } else {
                i.notes.push("signature not verified".into());
            }
            for (claim, what) in [("exp", "expires"), ("iat", "issued"), ("nbf", "valid from")] {
                if let Some(t) = payload.get(claim).and_then(Value::as_i64) {
                    let when = fmt_unix(t);
                    if claim == "exp" && t < crate::model::now_ms() / 1000 {
                        i.notes.push(format!("expired {when}"));
                    } else {
                        i.notes.push(format!("{what} {when}"));
                    }
                }
            }
            if payload.get("exp").is_none() {
                i.notes.push("no expiry".into());
            }
            let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
            i.decoded = Some(format!("// header\n{}\n\n// payload\n{}", pretty(&head), pretty(&payload)));
            out.push(i);
        }
    }
}

fn b64_json(part: &str) -> Option<Value> {
    let bytes = b64_decode(part)?;
    serde_json::from_slice::<Value>(&bytes).ok().filter(Value::is_object)
}

fn fmt_unix(t: i64) -> String {
    match time::OffsetDateTime::from_unix_timestamp(t) {
        Ok(d) => format!("{:04}-{:02}-{:02} {:02}:{:02} UTC", d.year(), u8::from(d.month()), d.day(), d.hour(), d.minute()),
        Err(_) => t.to_string(),
    }
}

/// Standard or URL-safe Base64, with or without padding.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    use base64::engine::general_purpose::{STANDARD_NO_PAD, URL_SAFE_NO_PAD};
    let t = s.trim_end_matches('=');
    if t.contains(['-', '_']) { URL_SAFE_NO_PAD.decode(t).ok() } else { STANDARD_NO_PAD.decode(t).ok() }
}

/// Mostly printable UTF-8 text, the only decoded form worth showing.
fn readable(bytes: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(bytes).ok()?;
    let total = s.chars().count();
    if total < 4 {
        return None;
    }
    let printable = s.chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t')).count();
    let letters = s.chars().filter(|c| c.is_alphanumeric()).count();
    (printable * 100 >= total * 95 && letters * 100 >= total * 40).then(|| s.to_string())
}

/// Base64 values that decode to readable text.
pub struct Base64Text;

impl Detector for Base64Text {
    fn id(&self) -> &str {
        "base64"
    }
    fn scan(&self, c: &Candidate, out: &mut Vec<Insight>) {
        let v = c.text.trim();
        let v = v.strip_prefix("Basic ").unwrap_or(v);
        if !c.token || v.len() < 12 || v.len() > 64 * 1024 || v.starts_with("eyJ") && v.contains('.') {
            return;
        }
        let charset = v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'-' | b'_' | b'='));
        // Plain words and identifiers are valid Base64 too; require a mix.
        let mixed = v.bytes().any(|b| b.is_ascii_digit() || matches!(b, b'+' | b'/' | b'='))
            && v.bytes().any(|b| b.is_ascii_uppercase())
            && v.bytes().any(|b| b.is_ascii_lowercase());
        if !charset || !mixed || v.trim_end_matches('=').len() % 4 == 1 {
            return;
        }
        let Some(text) = b64_decode(v).as_deref().and_then(readable) else { return };
        let mut i = insight(c, "base64", Category::Decode, "Base64", v);
        if c.text.trim().starts_with("Basic ") && text.contains(':') {
            i.label = "Basic auth".into();
            i.notes.push("username:password, readable by anyone who sees the request".into());
        }
        i.decoded = Some(text);
        out.push(i);
    }
}

/// Hex strings that decode to readable text (random hex IDs do not).
pub struct HexText;

impl Detector for HexText {
    fn id(&self) -> &str {
        "hex"
    }
    fn scan(&self, c: &Candidate, out: &mut Vec<Insight>) {
        let v = c.text.trim();
        let v = v.strip_prefix("0x").unwrap_or(v);
        if !c.token || v.len() < 16 || v.len() % 2 == 1 || v.len() > 64 * 1024 || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
            return;
        }
        let bytes: Option<Vec<u8>> = (0..v.len()).step_by(2).map(|i| u8::from_str_radix(&v[i..i + 2], 16).ok()).collect();
        if let Some(text) = bytes.as_deref().and_then(readable) {
            let mut i = insight(c, "hex", Category::Decode, "Hex", v);
            i.decoded = Some(text);
            out.push(i);
        }
    }
}

/// Values with several percent-escapes, and doubly encoded ones.
pub struct UrlEncoded;

impl Detector for UrlEncoded {
    fn id(&self) -> &str {
        "url-encoded"
    }
    fn scan(&self, c: &Candidate, out: &mut Vec<Insight>) {
        if !c.token {
            return;
        }
        let escapes = c.text.as_bytes().windows(3).filter(|w| w[0] == b'%' && w[1].is_ascii_hexdigit() && w[2].is_ascii_hexdigit()).count();
        if escapes < 3 {
            return;
        }
        let once = percent_decode(c.text);
        if once == c.text {
            return;
        }
        let mut i = insight(c, "url-encoded", Category::Decode, "URL-encoded", c.text);
        let twice = percent_decode(&once);
        if twice != once && once.contains('%') {
            i.label = "Double URL-encoded".into();
            i.notes.push("encoded twice: a filter that decodes once may miss what is inside".into());
            i.decoded = Some(twice);
        } else {
            i.decoded = Some(once);
        }
        out.push(i);
    }
}

/// `%XX` and `+` decoding. Invalid escapes stay as they are.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                Some(v) => {
                    out.push(v);
                    i += 3;
                    continue;
                }
                None => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Runs `detectors` over every part of an exchange. Repeated values are
/// merged and counted; the first location wins.
pub fn analyze(ex: &Exchange, detectors: &[Box<dyn Detector>]) -> Vec<Insight> {
    let req_text = codec::body_text(&ex.req_headers, &ex.req_body);
    let resp_text = codec::body_text(&ex.resp_headers, &ex.resp_body);
    let mut owned: Vec<(Side, String, String, bool)> = Vec::new();

    for (k, v) in query_pairs(&ex.query) {
        owned.push((Side::Request, format!("query parameter {k}"), v, true));
    }
    for (name, value) in &ex.req_headers {
        if name.eq_ignore_ascii_case("cookie") {
            for (k, v) in value.split(';').filter_map(|p| p.trim().split_once('=')) {
                owned.push((Side::Request, format!("cookie {k}"), v.to_string(), true));
            }
        } else {
            let v = value.strip_prefix("Bearer ").unwrap_or(value);
            owned.push((Side::Request, format!("header {name}"), v.to_string(), true));
        }
    }
    let form = header(&ex.req_headers, "content-type").is_some_and(|ct| ct.to_ascii_lowercase().contains("x-www-form-urlencoded"));
    if let Some(body) = &req_text {
        if form {
            for (k, v) in query_pairs(body) {
                owned.push((Side::Request, format!("form field {k}"), v, true));
            }
        }
        add_body(&mut owned, Side::Request, "request body", body);
    }
    for (name, value) in &ex.resp_headers {
        if name.eq_ignore_ascii_case("set-cookie") {
            if let Some((k, v)) = value.split(';').next().and_then(|p| p.trim().split_once('=')) {
                owned.push((Side::Response, format!("cookie {k} (set)"), v.to_string(), true));
            }
        } else {
            owned.push((Side::Response, format!("header {name}"), value.clone(), true));
        }
    }
    if let Some(body) = &resp_text {
        add_body(&mut owned, Side::Response, "response body", body);
    }

    let mut found = Vec::new();
    for (side, location, text, token) in &owned {
        let c = Candidate { side: *side, location: location.clone(), text, token: *token };
        for d in detectors {
            d.scan(&c, &mut found);
        }
    }
    merge(found)
}

fn query_pairs(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (percent_decode(k), v.to_string()),
            None => (percent_decode(p), String::new()),
        })
        .filter(|(_, v)| !v.is_empty())
        .collect()
}

/// A body is scanned as a whole for patterns, and each JSON string in it is
/// examined as a value of its own.
fn add_body(owned: &mut Vec<(Side, String, String, bool)>, side: Side, location: &str, body: &str) {
    owned.push((side, location.to_string(), body.to_string(), false));
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let mut strings = Vec::new();
        json_strings(&v, "", &mut strings);
        for (path, s) in strings.into_iter().take(2000) {
            let at = if path.is_empty() { location.to_string() } else { format!("{location} {path}") };
            owned.push((side, at, s, true));
        }
    }
}

fn json_strings(v: &Value, path: &str, out: &mut Vec<(String, String)>) {
    match v {
        Value::String(s) if s.len() >= 8 => out.push((path.to_string(), s.clone())),
        Value::Array(a) => a.iter().enumerate().for_each(|(i, x)| json_strings(x, &format!("{path}[{i}]"), out)),
        Value::Object(o) => o.iter().for_each(|(k, x)| json_strings(x, &if path.is_empty() { k.clone() } else { format!("{path}.{k}") }, out)),
        _ => {}
    }
}

/// One insight per (side, kind, value). A value found as a token and again
/// in the whole body counts once, and the more specific location is kept.
fn merge(found: Vec<Insight>) -> Vec<Insight> {
    let mut order: Vec<(Side, String, String)> = Vec::new();
    let mut by_key: HashMap<(Side, String, String), Insight> = HashMap::new();
    let mut seen_at: HashMap<(Side, String, String), Vec<String>> = HashMap::new();
    for i in found {
        let key = (i.side, i.kind.clone(), i.value.clone());
        let locs = seen_at.entry(key.clone()).or_default();
        match by_key.get_mut(&key) {
            Some(existing) => {
                let body_wide = existing.location.ends_with(" body");
                if body_wide && !i.location.ends_with(" body") {
                    let count = existing.count;
                    *existing = Insight { count, ..i.clone() };
                } else if !locs.contains(&i.location) && !(i.location.ends_with(" body") && locs.iter().any(|l| l.contains(" body"))) {
                    existing.count += 1;
                }
            }
            None => {
                if by_key.len() >= MAX_INSIGHTS {
                    continue;
                }
                order.push(key.clone());
                by_key.insert(key, i.clone());
            }
        }
        locs.push(i.location);
    }
    let mut out: Vec<Insight> = order.into_iter().filter_map(|k| by_key.remove(&k)).collect();
    // Secrets first, then decodable tokens, then personal data.
    let rank = |c: Category| match c {
        Category::Secret => 0,
        Category::Decode => 1,
        Category::Pii => 2,
        Category::Info => 3,
        Category::Extension => 4,
    };
    out.sort_by_key(|i| (i.side == Side::Response, rank(i.category)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Exchange;

    fn ex(query: &str, req_headers: Vec<(&str, &str)>, resp_ct: &str, resp_body: &str) -> Exchange {
        Exchange {
            scheme: "https".into(),
            host: "api.example.com".into(),
            port: 443,
            method: "GET".into(),
            path: "/v1/me".into(),
            query: query.into(),
            req_headers: req_headers.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
            status: Some(200),
            resp_headers: vec![("Content-Type".into(), resp_ct.into())],
            resp_body: resp_body.as_bytes().to_vec(),
            ..Default::default()
        }
    }

    fn b64url(s: &str) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
    }

    fn kinds(v: &[Insight]) -> Vec<&str> {
        v.iter().map(|i| i.kind.as_str()).collect()
    }

    #[test]
    fn decodes_jwts_without_vouching_for_them() {
        let jwt = format!("{}.{}.c2ln", b64url(r#"{"alg":"HS256","typ":"JWT"}"#), b64url(r#"{"sub":"42","role":"user","exp":1000}"#));
        let auth = format!("Bearer {jwt}");
        let found = analyze(&ex("", vec![("Authorization", &auth)], "text/plain", ""), &builtin());
        assert_eq!(kinds(&found), vec!["jwt"]);
        let i = &found[0];
        assert_eq!((i.location.as_str(), i.value.as_str()), ("header Authorization", jwt.as_str()));
        assert!(i.decoded.as_ref().unwrap().contains("\"role\": \"user\""));
        assert!(i.notes.contains(&"alg HS256".to_string()));
        assert!(i.notes.contains(&"signature not verified".to_string()));
        assert!(i.notes.iter().any(|n| n.starts_with("expired 1970-01-01")));

        let none = format!("{}.{}.", b64url(r#"{"alg":"none"}"#), b64url(r#"{"sub":"1"}"#));
        let found = analyze(&ex(&format!("token={none}"), vec![], "text/plain", ""), &builtin());
        assert_eq!(found[0].location, "query parameter token");
        assert!(found[0].notes.iter().any(|n| n.starts_with("unsigned")));
        assert!(found[0].notes.contains(&"no expiry".to_string()));
    }

    #[test]
    fn decodes_base64_hex_and_url_encoding_only_when_readable() {
        let b64 = base64::engine::general_purpose::STANDARD.encode(r#"{"user":"alice","admin":false}"#);
        let found = analyze(
            &ex(
                &format!("state={b64}&id=6f1ed002ab5595859014ebf0951522d9&next=%252Fadmin%253Fx%253D1&plain=HelloWorldThisIsText"),
                vec![("Cookie", "prefs=757365723d616c6963653b726f6c653d61646d696e; sid=Zm9vYmFy")],
                "text/plain",
                "",
            ),
            &builtin(),
        );
        let get = |k: &str| found.iter().find(|i| i.kind == k).unwrap_or_else(|| panic!("{k} missing: {found:?}"));
        assert_eq!(get("base64").decoded.as_deref(), Some(r#"{"user":"alice","admin":false}"#));
        assert_eq!(get("base64").location, "query parameter state");
        assert_eq!(get("hex").decoded.as_deref(), Some("user=alice;role=admin"));
        assert_eq!(get("hex").location, "cookie prefs");
        let url = get("url-encoded");
        assert_eq!(url.label, "Double URL-encoded");
        assert_eq!(url.decoded.as_deref(), Some("/admin?x=1"));
        // A random hex id, a plain word and a short value are left alone.
        assert_eq!(found.iter().filter(|i| i.kind == "hex").count(), 1);
        assert_eq!(found.iter().filter(|i| i.kind == "base64").count(), 1);
    }

    #[test]
    fn spots_basic_auth() {
        let found = analyze(&ex("", vec![("Authorization", "Basic YWRtaW46czNjcmV0IQ==")], "text/plain", ""), &builtin());
        assert_eq!(found[0].label, "Basic auth");
        assert_eq!(found[0].decoded.as_deref(), Some("admin:s3cret!"));
    }

    #[test]
    fn flags_personal_data_and_secrets_in_bodies() {
        let body = r#"{"orders":[{"email":"bob@acme.test","card":"4111 1111 1111 1111"},{"email":"bob@acme.test"}],
            "debug":"upstream 10.0.3.17 failed","logo":"logo@2x.png","key":"AKIAIOSFODNN7EXAMPLE","n":"1234567890123"}"#;
        let found = analyze(&ex("", vec![], "application/json", body), &builtin());
        assert_eq!(found[0].kind, "aws-access-key", "secrets come first: {found:?}");
        let email = found.iter().find(|i| i.kind == "email").unwrap();
        assert_eq!((email.count, email.location.as_str()), (2, "response body orders[0].email"));
        assert!(found.iter().any(|i| i.kind == "card-number" && i.value == "4111 1111 1111 1111"));
        assert!(found.iter().any(|i| i.kind == "private-ip" && i.value == "10.0.3.17"));
        // Not a Luhn-valid card, and not an email.
        assert!(!found.iter().any(|i| i.value == "1234567890123"));
        assert!(!found.iter().any(|i| i.value.contains("logo@2x")));
    }

    #[test]
    fn spots_stack_traces() {
        for body in [
            "Error: boom\n    at search (/srv/app/routes/search.js:42:13)\n    at next (/srv/app/node_modules/x/index.js:1:2)",
            "Traceback (most recent call last):\n  File \"app.py\", line 3, in <module>",
            "java.lang.NullPointerException\n\tat com.acme.Orders.find(Orders.java:88)",
            "<b>Warning</b>: mysqli_query() in /var/www/html/search.php on line 17",
        ] {
            let found = analyze(&ex("", vec![], "text/plain", body), &builtin());
            assert!(found.iter().any(|i| i.kind == "stack-trace"), "{body}: {found:?}");
        }
        let calm = analyze(&ex("", vec![], "text/html", "<p>Meet us at the office (Main St.)</p>"), &builtin());
        assert!(!calm.iter().any(|i| i.kind == "stack-trace"), "{calm:?}");
    }

    #[test]
    fn quiet_on_ordinary_traffic() {
        let found = analyze(
            &ex(
                "page=2&sort=created_at&q=hello+world",
                vec![("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36"), ("Accept", "*/*")],
                "text/html",
                "<html><body><h1>Products</h1><p>Version 1.2.3, build 20261003</p></body></html>",
            ),
            &builtin(),
        );
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b+c%2"), "a b c%2");
        assert_eq!(percent_decode("%E2%9C%93"), "✓");
        assert_eq!(percent_decode("100%"), "100%");
    }
}
