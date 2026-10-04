//! Crawl: discover endpoints, parameters and forms by following links from
//! in-scope pages.
//!
//! A crawl seeds from the host's captured traffic (and an optional start
//! path), fetches in-scope pages through the engine's `send` — the single
//! scope choke point, so a crawl can only ever reach a host the user has
//! accepted — parses each response for links and forms, and follows the
//! same-host links it has not seen, up to a page and depth budget. It only
//! issues the GET requests a browser following links would; it never submits
//! a form or leaves the accepted host.
//!
//! This module holds the data model and the pure parsing/URL logic (fully
//! testable without the network). `Engine::crawl` drives it.

use std::collections::BTreeSet;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Links are extracted from these attributes, bounded to a sane body size.
pub const MAX_BODY_SCAN: usize = 2 * 1024 * 1024;
pub const DEFAULT_MAX_PAGES: usize = 100;
pub const DEFAULT_MAX_DEPTH: usize = 4;
pub const MAX_PAGES_CEIL: usize = 2000;

/// What a user asks for when starting a crawl. The host must be accepted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrawlRequest {
    pub host: String,
    /// Where to start, as a path (default `/`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// Use the browser driver (JS-rendered pages). Not available yet; a plain
    /// crawl runs instead and the report notes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub browser: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pages: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
}

/// A form discovered on a page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Form {
    pub method: String,
    pub action: String,
    pub fields: Vec<String>,
}

/// The outcome of a crawl.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrawlReport {
    pub host: String,
    /// Pages actually fetched (all through the scope choke point).
    pub pages_fetched: usize,
    /// Distinct in-scope URLs discovered (fetched or queued).
    pub urls_found: usize,
    /// Forms discovered, with their fields.
    pub forms: Vec<Form>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// Extracts candidate link targets from `href`/`src` attributes.
pub fn extract_links(html: &str) -> Vec<String> {
    let body = &html[..html.len().min(MAX_BODY_SCAN)];
    let re = link_re();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for c in re.captures_iter(body) {
        if let Some(m) = c.get(1) {
            let v = m.as_str().trim();
            if !v.is_empty() && seen.insert(v.to_string()) {
                out.push(v.to_string());
            }
        }
    }
    out
}

/// Extracts forms: method, action and input/select/textarea names.
pub fn extract_forms(html: &str) -> Vec<Form> {
    let body = &html[..html.len().min(MAX_BODY_SCAN)];
    let mut forms = Vec::new();
    for fc in form_re().captures_iter(body) {
        let tag = fc.get(1).map(|m| m.as_str()).unwrap_or("");
        let inner = fc.get(2).map(|m| m.as_str()).unwrap_or("");
        let method = attr(tag, "method").unwrap_or_else(|| "GET".into()).to_ascii_uppercase();
        let action = attr(tag, "action").unwrap_or_default();
        let mut fields = Vec::new();
        let mut seen = BTreeSet::new();
        for nc in field_re().captures_iter(inner) {
            if let Some(name) = nc.get(1).or_else(|| nc.get(2)) {
                let n = name.as_str().trim();
                if !n.is_empty() && seen.insert(n.to_string()) {
                    fields.push(n.to_string());
                }
            }
        }
        forms.push(Form { method, action, fields });
    }
    forms
}

/// Resolves a raw link against the page it was found on, returning a full URL
/// only when the target is on the same host and uses http(s). Fragments and
/// non-navigational schemes (`mailto:`, `javascript:`, `tel:`, `data:`) are
/// dropped, as are cross-host links (those are for scope to decide, not the
/// crawler to follow).
pub fn resolve_same_host(scheme: &str, authority: &str, base_path: &str, raw: &str) -> Option<String> {
    let raw = raw.split('#').next().unwrap_or("").trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    for bad in ["javascript:", "mailto:", "tel:", "data:", "blob:", "about:"] {
        if lower.starts_with(bad) {
            return None;
        }
    }
    // Absolute URL.
    if let Some((s, rest)) = raw.split_once("://") {
        let s = s.to_ascii_lowercase();
        if s != "http" && s != "https" {
            return None;
        }
        let (auth, path) = match rest.find(['/', '?']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        return same_host(authority, auth).then(|| format!("{s}://{auth}{}", norm_path(path)));
    }
    if raw.starts_with("//") {
        // Protocol-relative.
        let rest = &raw[2..];
        let (auth, path) = match rest.find(['/', '?']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        return same_host(authority, auth).then(|| format!("{scheme}://{auth}{}", norm_path(path)));
    }
    // Relative to the page.
    let path = if let Some(stripped) = raw.strip_prefix('/') {
        format!("/{stripped}")
    } else {
        let dir = base_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        format!("{dir}/{raw}")
    };
    Some(format!("{scheme}://{authority}{}", norm_path(&path)))
}

/// A normalized key for deduping URLs: scheme, authority and path with query
/// parameter *names* only (values stripped), so `/s?q=1` and `/s?q=2` are one
/// endpoint to crawl.
pub fn dedup_key(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let (auth, target) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut names: Vec<&str> = query.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').map(|(k, _)| k).unwrap_or(p)).collect();
    names.sort_unstable();
    names.dedup();
    format!("{}://{}{}?{}", scheme.to_ascii_lowercase(), auth.to_ascii_lowercase(), path, names.join("&"))
}

fn same_host(base_authority: &str, other_authority: &str) -> bool {
    host_of(base_authority).eq_ignore_ascii_case(host_of(other_authority))
}

fn host_of(authority: &str) -> &str {
    // Strip userinfo and port; leave IPv6 brackets alone enough for comparison.
    let a = authority.rsplit('@').next().unwrap_or(authority);
    match a.rsplit_once(':') {
        Some((h, _)) if !h.contains(':') || h.ends_with(']') => h,
        _ => a,
    }
}

fn norm_path(path: &str) -> String {
    let (p, q) = path.split_once('?').unwrap_or((path, ""));
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    let mut s = format!("/{}", out.join("/"));
    if !q.is_empty() {
        s.push('?');
        s.push_str(q);
    }
    s
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"(?i)\b{name}\s*=\s*["']([^"']*)["']"#)).ok()?;
    re.captures(tag).and_then(|c| c.get(1)).map(|m| m.as_str().trim().to_string()).filter(|s| !s.is_empty())
}

fn link_re() -> Regex {
    Regex::new(r#"(?i)(?:href|src)\s*=\s*["']([^"'>\s]+)["']"#).unwrap()
}

fn form_re() -> Regex {
    // <form ...> ... </form>, non-greedy. (?s) so bodies may span lines.
    Regex::new(r#"(?is)<form\b([^>]*)>(.*?)</form>"#).unwrap()
}

fn field_re() -> Regex {
    Regex::new(r#"(?is)<(?:input|select|textarea)\b[^>]*?\bname\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_links_from_href_and_src() {
        let html = r##"<a href="/a">a</a><a href='b/c?q=1'>b</a><script src="/js/app.js"></script><a href="#frag">f</a>"##;
        let links = extract_links(html);
        assert!(links.contains(&"/a".to_string()));
        assert!(links.contains(&"b/c?q=1".to_string()));
        assert!(links.contains(&"/js/app.js".to_string()));
    }

    #[test]
    fn extracts_forms_with_fields() {
        let html = r#"<form method="post" action="/login"><input name="user"><input name='pass' type="password"><textarea name="note"></textarea></form>"#;
        let forms = extract_forms(html);
        assert_eq!(forms.len(), 1);
        assert_eq!(forms[0].method, "POST");
        assert_eq!(forms[0].action, "/login");
        assert_eq!(forms[0].fields, vec!["user", "pass", "note"]);
    }

    #[test]
    fn resolves_relative_and_absolute_same_host_only() {
        let s = "https";
        let auth = "app.example.com";
        assert_eq!(resolve_same_host(s, auth, "/dir/page", "sub?q=1").as_deref(), Some("https://app.example.com/dir/sub?q=1"));
        assert_eq!(resolve_same_host(s, auth, "/dir/page", "/root").as_deref(), Some("https://app.example.com/root"));
        assert_eq!(resolve_same_host(s, auth, "/a/b", "../c").as_deref(), Some("https://app.example.com/c"));
        assert_eq!(resolve_same_host(s, auth, "/", "https://app.example.com/x").as_deref(), Some("https://app.example.com/x"));
        // Off-host and non-navigational links are dropped.
        assert_eq!(resolve_same_host(s, auth, "/", "https://evil.test/x"), None);
        assert_eq!(resolve_same_host(s, auth, "/", "mailto:a@b.com"), None);
        assert_eq!(resolve_same_host(s, auth, "/", "javascript:void(0)"), None);
        assert_eq!(resolve_same_host(s, auth, "/", "#top"), None);
    }

    #[test]
    fn same_host_ignores_port_and_userinfo() {
        // The page is served on a non-default port; a same-host absolute link keeps it.
        assert_eq!(
            resolve_same_host("http", "localhost:8080", "/", "http://localhost:8080/next").as_deref(),
            Some("http://localhost:8080/next")
        );
    }

    #[test]
    fn dedup_key_strips_query_values_and_fragments() {
        assert_eq!(dedup_key("https://h/s?q=1"), dedup_key("https://H/s?q=2"));
        assert_ne!(dedup_key("https://h/s?q=1"), dedup_key("https://h/s?q=1&p=2"));
        assert_eq!(dedup_key("https://h/a"), "https://h/a?");
    }
}
