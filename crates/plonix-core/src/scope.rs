//! Adaptive scope.
//!
//! Everything is recorded, in scope or not. Scope starts from a seed domain
//! and grows from *suggestions*: hosts that the analyzer links to in-scope
//! traffic, each backed by evidence. The user accepts or rejects them.
//!
//! The engine never sends an active request (replay, agent request) to a host
//! whose decision is not [`Decision::Accepted`].

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::codec;
use crate::model::{Exchange, header, header_all};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Accepted,
    Rejected,
    Unknown,
}

impl Decision {
    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Accepted => "accepted",
            Decision::Rejected => "rejected",
            Decision::Unknown => "unknown",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "accepted" => Decision::Accepted,
            "rejected" => Decision::Rejected,
            _ => Decision::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    /// Host name, lowercase, no port.
    pub pattern: String,
    pub include_subdomains: bool,
    pub decision: Decision,
    pub created_at: i64,
    #[serde(default)]
    pub note: String,
}

impl Rule {
    fn matches(&self, host: &str) -> bool {
        host == self.pattern || (self.include_subdomains && is_subdomain_of(host, &self.pattern)) || (self.pattern.contains('/') && crate::bounty::cidr_contains(&self.pattern, host))
    }
    /// Longer patterns are more specific; an exact rule beats a subdomain rule of equal length.
    fn specificity(&self, host: &str) -> usize {
        // An IP range is less specific than any single address or host inside it,
        // and a narrower range beats a wider one (prefix lengths go up to 128).
        if let Some((_, len)) = self.pattern.contains('/').then(|| crate::bounty::parse_cidr(&self.pattern)).flatten() {
            return usize::from(len);
        }
        1000 + self.pattern.len() * 2 + usize::from(host == self.pattern && !self.include_subdomains)
    }
}

/// The set of scope rules. The most specific matching rule decides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ScopeRules {
    pub rules: Vec<Rule>,
}

impl ScopeRules {
    pub fn decide(&self, host: &str) -> Decision {
        let host = normalize_host(host);
        self.rules
            .iter()
            .filter(|r| r.matches(&host))
            .max_by_key(|r| r.specificity(&host))
            .map(|r| r.decision)
            .unwrap_or(Decision::Unknown)
    }

    pub fn in_scope(&self, host: &str) -> bool {
        self.decide(host) == Decision::Accepted
    }

    /// Decision for a suggestion domain, which may be a `*.base` wildcard.
    pub fn decide_domain(&self, domain: &str) -> Decision {
        match domain.strip_prefix("*.") {
            Some(base) => {
                let base = normalize_host(base);
                // A wildcard is decided by a subdomain-covering rule, or by a
                // rule for its base domain alone ("only example.com").
                self.rules
                    .iter()
                    .filter(|r| base == r.pattern || (r.include_subdomains && is_subdomain_of(&base, &r.pattern)))
                    .max_by_key(|r| r.pattern.len())
                    .map(|r| r.decision)
                    .unwrap_or(Decision::Unknown)
            }
            None => self.decide(domain),
        }
    }

    fn accepted_patterns(&self) -> impl Iterator<Item = &Rule> {
        self.rules.iter().filter(|r| r.decision == Decision::Accepted)
    }
}

/// Turns a user-typed target ("https://App.Example.com:8443/x") into a host.
pub fn normalize_host(input: &str) -> String {
    let s = input.trim();
    let s = s.split_once("://").map(|(_, r)| r).unwrap_or(s);
    let s = s.split(['/', '?', '#']).next().unwrap_or("");
    let s = s.rsplit_once('@').map(|(_, h)| h).unwrap_or(s);
    let host = if s.starts_with('[') {
        s.split(']').next().unwrap_or("").trim_start_matches('[')
    } else if s.matches(':').count() == 1 {
        s.split(':').next().unwrap_or("")
    } else {
        s
    };
    host.trim_end_matches('.').to_ascii_lowercase()
}

pub fn is_subdomain_of(host: &str, parent: &str) -> bool {
    host.len() > parent.len() + 1 && host.ends_with(parent) && host.as_bytes()[host.len() - parent.len() - 1] == b'.'
}

/// Whether TLS SAN `san` (possibly `*.x`) covers `host`.
pub fn san_covers(san: &str, host: &str) -> bool {
    match san.strip_prefix("*.") {
        // A wildcard covers exactly one extra label.
        Some(base) => is_subdomain_of(host, base) && !host[..host.len() - base.len() - 1].contains('.'),
        None => san == host,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// The host's request carried a Referer/Origin from an in-scope page.
    RequestedFrom,
    /// An in-scope response redirected to the host.
    RedirectedFrom,
    /// An in-scope response body or CSP referenced the host.
    LinkedFrom,
    /// The host received a cookie or token first seen on in-scope traffic.
    SharesSession,
    /// The host's TLS certificate also covers an in-scope host, or vice versa.
    SharesCertificate,
    /// An extension enumerated it as a subdomain of an in-scope domain. Unlike
    /// the other kinds this comes from a tool the user ran, not from captured
    /// traffic, so it carries no originating exchange.
    Discovered,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::RequestedFrom => "requested_from",
            EvidenceKind::RedirectedFrom => "redirected_from",
            EvidenceKind::LinkedFrom => "linked_from",
            EvidenceKind::SharesSession => "shares_session",
            EvidenceKind::SharesCertificate => "shares_certificate",
            EvidenceKind::Discovered => "discovered",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "requested_from" => EvidenceKind::RequestedFrom,
            "redirected_from" => EvidenceKind::RedirectedFrom,
            "linked_from" => EvidenceKind::LinkedFrom,
            "shares_session" => EvidenceKind::SharesSession,
            "shares_certificate" => EvidenceKind::SharesCertificate,
            "discovered" => EvidenceKind::Discovered,
            _ => return None,
        })
    }
    pub fn weight(self) -> i64 {
        match self {
            EvidenceKind::SharesSession => 5,
            EvidenceKind::SharesCertificate | EvidenceKind::RedirectedFrom => 3,
            EvidenceKind::RequestedFrom | EvidenceKind::Discovered => 2,
            EvidenceKind::LinkedFrom => 1,
        }
    }
    pub fn describe(self, via: &str) -> String {
        match self {
            EvidenceKind::RequestedFrom => format!("called from in-scope page on {via}"),
            EvidenceKind::RedirectedFrom => format!("{via} redirects here"),
            EvidenceKind::LinkedFrom => format!("referenced by {via}"),
            EvidenceKind::SharesSession => format!("receives a session token issued to {via}"),
            EvidenceKind::SharesCertificate => format!("shares a TLS certificate with {via}"),
            EvidenceKind::Discovered => format!("found while enumerating subdomains of {via}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewEvidence {
    pub domain: String,
    pub kind: EvidenceKind,
    /// The in-scope host this evidence ties the domain to.
    pub via: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub kind: EvidenceKind,
    pub via: String,
    pub detail: String,
    pub summary: String,
    pub exchange_id: i64,
    pub count: i64,
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Suggestion {
    pub domain: String,
    pub score: i64,
    pub requests: i64,
    pub evidence: Vec<Evidence>,
}

/// What the analyzer learned from one exchange.
#[derive(Debug, Default)]
pub struct Analysis {
    /// Hashes of session tokens seen on in-scope traffic, to be remembered.
    pub tokens: Vec<String>,
    pub evidence: Vec<NewEvidence>,
}

/// Third-party analytics/telemetry that is never worth suggesting.
pub(crate) const NOISE: &[&str] = &[
    "google-analytics.com",
    "googletagmanager.com",
    "doubleclick.net",
    "googlesyndication.com",
    "googleadservices.com",
    "gstatic.com",
    "fonts.googleapis.com",
    "safebrowsing.googleapis.com",
    "update.googleapis.com",
    "clients2.google.com",
    "optimizationguide-pa.googleapis.com",
    "facebook.net",
    "hotjar.com",
    "segment.io",
    "w3.org",
    "schema.org",
    "mozilla.org",
    "mozilla.net",
];

pub fn is_noise(domain: &str) -> bool {
    let d = domain.trim_start_matches("*.");
    NOISE.iter().any(|n| d == *n || is_subdomain_of(d, n))
}

/// Analyzes one exchange against the current rules.
///
/// `token_owner` maps a token hash to the in-scope host it was first seen on.
pub fn analyze(ex: &Exchange, rules: &ScopeRules, token_owner: &dyn Fn(&str) -> Option<String>) -> Analysis {
    let host = normalize_host(&ex.host);
    let mut out = Analysis::default();
    let candidate = |d: &str| -> bool {
        !d.is_empty() && d != host && valid_hostname(d.trim_start_matches("*.")) && rules.decide_domain(d) == Decision::Unknown && !is_noise(d)
    };

    match rules.decide(&host) {
        Decision::Accepted => {
            out.tokens = session_tokens(ex, true);
            if let Some(loc) = header(&ex.resp_headers, "location") {
                let d = url_host(loc).unwrap_or_default();
                if candidate(&d) {
                    out.evidence.push(ev(&d, EvidenceKind::RedirectedFrom, &host, format!("Location: {}", clip(loc))));
                }
            }
            for csp in header_all(&ex.resp_headers, "content-security-policy") {
                for d in csp_hosts(csp) {
                    if candidate(&d) {
                        out.evidence.push(ev(&d, EvidenceKind::LinkedFrom, &host, "Content-Security-Policy".into()));
                    }
                }
            }
            if let Some(text) = codec::body_text(&ex.resp_headers, &ex.resp_body) {
                for (d, snippet) in linked_hosts(&text) {
                    if candidate(&d) {
                        out.evidence.push(ev(&d, EvidenceKind::LinkedFrom, &host, format!("{} {}", ex.path, snippet)));
                    }
                }
            }
            for san in &ex.tls_sans {
                let san = san.to_ascii_lowercase();
                if candidate(&san) {
                    out.evidence.push(ev(&san, EvidenceKind::SharesCertificate, &host, format!("SAN {san} on the certificate of {host}")));
                }
            }
        }
        Decision::Unknown if !is_noise(&host) && valid_hostname(&host) => {
            for name in ["referer", "origin"] {
                if let Some(v) = header(&ex.req_headers, name) {
                    if let Some(src) = url_host(v) {
                        if src != host && rules.in_scope(&src) {
                            out.evidence.push(ev(&host, EvidenceKind::RequestedFrom, &src, format!("{name}: {}", clip(v))));
                        }
                    }
                }
            }
            for hash in session_tokens(ex, false) {
                if let Some(owner) = token_owner(&hash) {
                    if owner != host && rules.in_scope(&owner) {
                        out.evidence.push(ev(&host, EvidenceKind::SharesSession, &owner, format!("token {}…", &hash[..8])));
                    }
                }
            }
            for san in &ex.tls_sans {
                let san = san.to_ascii_lowercase();
                for rule in rules.accepted_patterns() {
                    if san_covers(&san, &rule.pattern) || (san == rule.pattern) {
                        out.evidence.push(ev(&host, EvidenceKind::SharesCertificate, &rule.pattern, format!("its certificate lists {san}")));
                        break;
                    }
                }
            }
        }
        _ => {}
    }
    out.evidence.dedup_by(|a, b| a.domain == b.domain && a.kind == b.kind && a.via == b.via);
    out
}

fn ev(domain: &str, kind: EvidenceKind, via: &str, detail: String) -> NewEvidence {
    NewEvidence { domain: domain.to_string(), kind, via: via.to_string(), detail }
}

fn clip(s: &str) -> String {
    if s.len() <= 120 {
        s.to_string()
    } else {
        let mut end = 120;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Token-like values: cookie values, Authorization, and *token*/*auth*/api-key headers.
/// With `include_response`, Set-Cookie values issued by the server are included too.
pub fn session_tokens(ex: &Exchange, include_response: bool) -> Vec<String> {
    let mut raw: Vec<&str> = Vec::new();
    for (k, v) in &ex.req_headers {
        let k = k.to_ascii_lowercase();
        if k == "cookie" {
            raw.extend(v.split(';').filter_map(|p| p.split_once('=').map(|(_, val)| val.trim())));
        } else if k == "authorization" {
            raw.push(v.trim());
        } else if k.contains("token") || k.contains("auth") || k.contains("api-key") || k.contains("apikey") || k.contains("session") {
            raw.push(v.trim());
        }
    }
    if include_response {
        for v in header_all(&ex.resp_headers, "set-cookie") {
            if let Some((_, val)) = v.split(';').next().and_then(|p| p.split_once('=')) {
                raw.push(val.trim());
            }
        }
    }
    let mut out: Vec<String> = raw
        .into_iter()
        .map(|v| v.trim_matches('"'))
        .filter(|v| v.len() >= 12 && !matches!(*v, "deleted"))
        .map(token_hash)
        .collect();
    out.sort();
    out.dedup();
    out
}

pub fn token_hash(v: &str) -> String {
    Sha256::digest(v.as_bytes()).iter().take(16).map(|b| format!("{b:02x}")).collect()
}

/// Host part of an absolute URL (`https://h/...`, `//h/...`).
pub fn url_host(url: &str) -> Option<String> {
    let rest = url.trim().split_once("//")?.1;
    let h = normalize_host(rest);
    if h.is_empty() { None } else { Some(h) }
}

fn valid_hostname(h: &str) -> bool {
    if h.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    let labels: Vec<&str> = h.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-'))
        && labels.last().is_some_and(|tld| tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()))
}

/// Hosts referenced by absolute or protocol-relative URLs in a text body.
fn linked_hosts(text: &str) -> Vec<(String, String)> {
    static ABS: OnceLock<Regex> = OnceLock::new();
    static REL: OnceLock<Regex> = OnceLock::new();
    let abs = ABS.get_or_init(|| Regex::new(r"(?i)\b(?:https?|wss?):(?:\\?/){2}([a-z0-9][a-z0-9.-]*[a-z0-9])").unwrap());
    let rel = REL.get_or_init(|| Regex::new(r#"(?i)(?:src|href|action)\s*=\s*["']//([a-z0-9][a-z0-9.-]*[a-z0-9])"#).unwrap());
    let mut out: Vec<(String, String)> = Vec::new();
    for re in [abs, rel] {
        for cap in re.captures_iter(text) {
            let host = cap[1].to_ascii_lowercase();
            if valid_hostname(&host) && !out.iter().any(|(h, _)| *h == host) {
                out.push((host, clip(cap.get(0).map(|m| m.as_str()).unwrap_or(""))));
            }
            if out.len() >= 200 {
                return out;
            }
        }
    }
    out
}

fn csp_hosts(csp: &str) -> Vec<String> {
    csp.split([';', ' '])
        .map(str::trim)
        .filter(|t| t.contains('.') && !t.starts_with('\'') && !t.contains("data:"))
        .filter_map(|t| {
            let t = t.split_once("://").map(|(_, r)| r).unwrap_or(t);
            let h = normalize_host(t);
            if valid_hostname(h.trim_start_matches("*.")) { Some(h) } else { None }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(p: &str, sub: bool, d: Decision) -> Rule {
        Rule { pattern: p.into(), include_subdomains: sub, decision: d, created_at: 0, note: String::new() }
    }

    fn ex(host: &str) -> Exchange {
        Exchange { scheme: "https".into(), host: host.into(), port: 443, method: "GET".into(), path: "/".into(), ..Default::default() }
    }

    fn seeded() -> ScopeRules {
        ScopeRules { rules: vec![rule("example.com", true, Decision::Accepted)] }
    }

    #[test]
    fn narrower_ip_range_wins_whatever_the_order() {
        // Rejected first, as one program import can list it; wider accept after.
        let r = ScopeRules { rules: vec![rule("10.0.5.0/24", false, Decision::Rejected), rule("10.0.0.0/16", false, Decision::Accepted)] };
        assert_eq!(r.decide("10.0.5.9"), Decision::Rejected);
        assert_eq!(r.decide("10.0.6.9"), Decision::Accepted);
        let mut r = r;
        r.rules.push(rule("10.0.5.9", false, Decision::Accepted));
        assert_eq!(r.decide("10.0.5.9"), Decision::Accepted);
    }

    #[test]
    fn most_specific_rule_wins() {
        let mut r = seeded();
        r.rules.push(rule("ads.example.com", false, Decision::Rejected));
        assert_eq!(r.decide("example.com"), Decision::Accepted);
        assert_eq!(r.decide("API.Example.com:443"), Decision::Accepted);
        assert_eq!(r.decide("ads.example.com"), Decision::Rejected);
        assert_eq!(r.decide("x.ads.example.com"), Decision::Accepted);
        assert_eq!(r.decide("notexample.com"), Decision::Unknown);
        assert_eq!(r.decide("example.com.evil.net"), Decision::Unknown);
    }

    #[test]
    fn normalizes_targets() {
        assert_eq!(normalize_host("https://App.Example.com:8443/login?x=1"), "app.example.com");
        assert_eq!(normalize_host("example.com."), "example.com");
        assert_eq!(normalize_host("user@host.test"), "host.test");
        assert_eq!(normalize_host("[::1]:8080"), "::1");
    }

    #[test]
    fn wildcard_sans() {
        assert!(san_covers("*.example.com", "a.example.com"));
        assert!(!san_covers("*.example.com", "a.b.example.com"));
        assert!(!san_covers("*.example.com", "example.com"));
    }

    #[test]
    fn wildcard_suggestions_are_decided_by_their_base_domain() {
        let mut r = ScopeRules::default();
        assert_eq!(r.decide_domain("*.cdn.net"), Decision::Unknown);
        // Accepting only cdn.net settles the *.cdn.net suggestion without
        // bringing its subdomains into scope.
        r.rules.push(rule("cdn.net", false, Decision::Accepted));
        assert_eq!(r.decide_domain("*.cdn.net"), Decision::Accepted);
        assert_eq!(r.decide("a.cdn.net"), Decision::Unknown);
        // A subdomain-covering rule further up still decides it.
        let r = ScopeRules { rules: vec![rule("net", true, Decision::Rejected)] };
        assert_eq!(r.decide_domain("*.cdn.net"), Decision::Rejected);
    }

    #[test]
    fn referer_from_in_scope_page() {
        let mut e = ex("api.partner.io");
        e.req_headers = vec![("Referer".into(), "https://www.example.com/app".into())];
        let a = analyze(&e, &seeded(), &|_| None);
        assert_eq!(a.evidence.len(), 1);
        assert_eq!(a.evidence[0].domain, "api.partner.io");
        assert_eq!(a.evidence[0].kind, EvidenceKind::RequestedFrom);
        assert_eq!(a.evidence[0].via, "www.example.com");
    }

    #[test]
    fn referer_from_out_of_scope_page_is_ignored() {
        let mut e = ex("api.partner.io");
        e.req_headers = vec![("Referer".into(), "https://random.org/".into())];
        assert!(analyze(&e, &seeded(), &|_| None).evidence.is_empty());
    }

    #[test]
    fn links_redirects_and_csp_from_in_scope_response() {
        let mut e = ex("www.example.com");
        e.resp_headers = vec![
            ("Content-Type".into(), "text/html".into()),
            ("Location".into(), "https://sso.idp.net/login".into()),
            ("Content-Security-Policy".into(), "default-src 'self'; connect-src https://api.backend.dev wss://ws.example.com".into()),
        ];
        e.resp_body = br#"<script src="//cdn.assets.net/app.js"></script>
            <a href="https://docs.example.com/x">docs</a>
            {"api":"https:\/\/graph.backend.dev\/v1"} https://www.google-analytics.com/ga.js version 1.2.3"#
            .to_vec();
        let a = analyze(&e, &seeded(), &|_| None);
        let got: Vec<(String, EvidenceKind)> = a.evidence.iter().map(|e| (e.domain.clone(), e.kind)).collect();
        assert!(got.contains(&("sso.idp.net".into(), EvidenceKind::RedirectedFrom)));
        assert!(got.contains(&("api.backend.dev".into(), EvidenceKind::LinkedFrom)));
        assert!(got.contains(&("cdn.assets.net".into(), EvidenceKind::LinkedFrom)));
        assert!(got.contains(&("graph.backend.dev".into(), EvidenceKind::LinkedFrom)));
        // Already in scope (subdomain of the seed) and noise are not suggested.
        assert!(!got.iter().any(|(d, _)| d.ends_with("example.com")));
        assert!(!got.iter().any(|(d, _)| d.contains("google-analytics")));
    }

    #[test]
    fn session_token_reuse() {
        let mut login = ex("www.example.com");
        login.resp_headers = vec![("Set-Cookie".into(), "sid=s3cr3t-session-value-123; Path=/; HttpOnly".into())];
        let a = analyze(&login, &seeded(), &|_| None);
        assert_eq!(a.tokens.len(), 1);
        let issued = a.tokens[0].clone();

        let mut other = ex("files.storage.net");
        other.req_headers = vec![("Cookie".into(), "a=1; sid=s3cr3t-session-value-123".into())];
        let owner = |h: &str| if h == issued { Some("www.example.com".to_string()) } else { None };
        let b = analyze(&other, &seeded(), &owner);
        assert_eq!(b.evidence.len(), 1);
        assert_eq!(b.evidence[0].kind, EvidenceKind::SharesSession);
        assert_eq!(b.evidence[0].via, "www.example.com");
    }

    #[test]
    fn shared_certificate_both_directions() {
        let rules = ScopeRules { rules: vec![rule("app.example.com", false, Decision::Accepted)] };
        let mut inside = ex("app.example.com");
        inside.tls_sans = vec!["app.example.com".into(), "admin.example-internal.com".into()];
        let a = analyze(&inside, &rules, &|_| None);
        assert_eq!(a.evidence[0].domain, "admin.example-internal.com");
        assert_eq!(a.evidence[0].kind, EvidenceKind::SharesCertificate);

        let mut outside = ex("static.example.com");
        outside.tls_sans = vec!["*.example.com".into()];
        let b = analyze(&outside, &rules, &|_| None);
        assert_eq!(b.evidence[0].domain, "static.example.com");
        assert_eq!(b.evidence[0].via, "app.example.com");
    }

    #[test]
    fn decided_hosts_get_no_suggestions() {
        let mut r = seeded();
        r.rules.push(rule("partner.io", true, Decision::Rejected));
        let mut e = ex("api.partner.io");
        e.req_headers = vec![("Origin".into(), "https://example.com".into())];
        assert!(analyze(&e, &r, &|_| None).evidence.is_empty());
    }
}
