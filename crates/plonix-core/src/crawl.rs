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
//! A browser crawl (`browser: true`) does the same walk in a headless
//! Chromium routed through the proxy, so pages are rendered and their
//! JavaScript runs: links come from the live DOM, router links and
//! `history.pushState`, and every request the pages make lands in Traffic.
//! [`crate::browser_crawl`] drives it.
//!
//! This module holds the data model and the pure parsing/URL logic (fully
//! testable without the network). `Engine::crawl` drives it.

use std::collections::BTreeSet;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::scope::{Decision, ScopeRules, normalize_host};

/// Links are extracted from these attributes, bounded to a sane body size.
pub const MAX_BODY_SCAN: usize = 2 * 1024 * 1024;
pub const DEFAULT_MAX_PAGES: usize = 100;
pub const DEFAULT_MAX_DEPTH: usize = 4;
pub const MAX_PAGES_CEIL: usize = 2000;
/// A browser crawl stops after this long, whatever is left in its queue.
pub const DEFAULT_MAX_SECONDS: u64 = 180;
pub const MAX_SECONDS_CEIL: u64 = 1800;
/// Buttons and script links a browser crawl clicks on one page, at most.
pub const MAX_CLICKS_PER_PAGE: usize = 12;

/// What a user asks for when starting a crawl. The host must be accepted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CrawlRequest {
    pub host: String,
    /// Where to start: a path (default `/`), or a full URL on `host`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// Render pages in a headless Chromium-based browser, for JavaScript apps.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub browser: bool,
    /// Browser crawl only: also click buttons and script links that do not
    /// look destructive (never inside a form, never logout/delete/pay...).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub click: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pages: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
    /// Browser crawl only: stop after this many seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_seconds: Option<u64>,
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
    /// The browser that rendered the pages, for a browser crawl.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    /// Buttons and script links clicked (browser crawl with clicking on).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub clicks: usize,
    /// Hosts the pages tried to reach that are not accepted into scope; the
    /// browser crawl blocked those requests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl CrawlReport {
    pub fn new(host: &str) -> Self {
        CrawlReport { host: host.to_string(), pages_fetched: 0, urls_found: 0, forms: vec![], browser: None, clicks: 0, blocked_hosts: vec![], notes: vec![] }
    }

    /// Adds a form unless it is already listed.
    pub fn add_form(&mut self, form: Form) {
        if !self.forms.contains(&form) {
            self.forms.push(form);
        }
    }
}

/// Extracts candidate link targets from `href`/`src` attributes.
pub fn extract_links(html: &str) -> Vec<String> {
    let body = &html[..html.floor_char_boundary(MAX_BODY_SCAN)];
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
    let body = &html[..html.floor_char_boundary(MAX_BODY_SCAN)];
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

/// Splits an absolute http(s) URL into scheme, authority and path (with query).
pub fn split_url(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let rest = rest.split('#').next().unwrap_or("");
    let (auth, path) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    (!auth.is_empty()).then_some((scheme, auth, path))
}

/// The next page a crawl may visit from `page`: `raw` resolved against it,
/// on the same host, and that host accepted into scope. Anything else is
/// `None`, so a crawl never navigates to or follows a link out of scope.
pub fn follow(rules: &ScopeRules, page: &str, raw: &str) -> Option<String> {
    let (scheme, authority, path) = split_url(page)?;
    let base_path = path.split('?').next().unwrap_or("/");
    let next = resolve_same_host(scheme, authority, base_path, raw)?;
    (rules.decide(&normalize_host(&next)) == Decision::Accepted).then_some(next)
}

/// For a request a rendered page makes: the host to block when it is not
/// accepted into scope. Non-network schemes (`data:`, `blob:`) stay local
/// and pass.
pub fn blocked_host(rules: &ScopeRules, url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !["http", "https", "ws", "wss"].iter().any(|s| scheme.eq_ignore_ascii_case(s)) {
        return None;
    }
    let host = normalize_host(rest);
    (rules.decide(&host) != Decision::Accepted).then_some(host)
}

/// Whether a button or link label suggests an action with side effects the
/// crawl must not take: signing out, deleting, paying, sending and the like.
/// Errs towards skipping: a harmless "Orders" link is skipped too.
pub fn looks_destructive(label: &str) -> bool {
    let lower = label.to_ascii_lowercase();
    let compact: String = lower.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if ["logout", "logoff", "signout", "signoff", "closeaccount"].iter().any(|w| compact.contains(w)) {
        return true;
    }
    const RISKY: [&str; 36] = [
        "delet", "remov", "destroy", "erase", "drop", "purg", "wipe", "pay", "purchas", "buy", "checkout", "order", "subscri", "unsubscri", "cancel",
        "deactivat", "disabl", "revok", "reset", "transfer", "send", "submit", "confirm", "approv", "archiv", "ban", "block", "kick", "leav", "terminat",
        "uninstall", "discard", "clear", "donat", "refund", "withdraw",
    ];
    lower.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).any(|t| RISKY.iter().any(|r| t.starts_with(r)))
}

/// What the in-page collector ([`COLLECT_JS`]) reports for a rendered page.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PageScan {
    /// Where the page ended up, after redirects and client-side routing.
    #[serde(default)]
    pub location: String,
    /// Absolute link targets: anchors, frames, router links, pushState routes.
    #[serde(default)]
    pub links: Vec<String>,
    #[serde(default)]
    pub forms: Vec<Form>,
    /// Click candidates, in document order.
    #[serde(default)]
    pub clickables: Vec<Clickable>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Clickable {
    /// A stable key to find the element again after the page re-renders.
    pub key: String,
    /// Its text, label, title and target, for the destructive check.
    pub label: String,
}

impl PageScan {
    /// The click candidates that do not look destructive, deduped, capped.
    pub fn safe_clicks(&self) -> Vec<&Clickable> {
        let mut seen = BTreeSet::new();
        self.clickables.iter().filter(|c| !looks_destructive(&c.label) && seen.insert(c.key.as_str())).take(MAX_CLICKS_PER_PAGE).collect()
    }
}

/// Runs in every document before the page's own scripts. Records
/// client-side routes, and keeps the crawl read-only: forms never submit,
/// popups never open (their URL is recorded instead), service workers never
/// register, and sockets only open to the page's own host.
pub const INIT_JS: &str = r#"(() => {
  if (window.__plonixCrawl) return;
  window.__plonixCrawl = true;
  const routes = (window.__plonixRoutes = []);
  const note = (u) => { try { if (u != null && u !== '') routes.push(new URL(String(u), location.href).href); } catch (_) {} };
  for (const k of ['pushState', 'replaceState']) {
    const orig = history[k];
    history[k] = function (s, t, u) { note(u); return orig.apply(this, arguments); };
  }
  window.addEventListener('submit', (e) => { e.preventDefault(); e.stopImmediatePropagation(); }, true);
  HTMLFormElement.prototype.submit = function () {};
  HTMLFormElement.prototype.requestSubmit = function () {};
  window.open = (u) => { note(u); return null; };
  try { if (navigator.serviceWorker) navigator.serviceWorker.register = () => Promise.reject(new Error('off during a crawl')); } catch (_) {}
  const WS = window.WebSocket;
  if (WS) {
    window.WebSocket = function (u, p) {
      if (new URL(String(u), location.href).hostname !== location.hostname) throw new Error('off-host socket blocked during a crawl');
      return p === undefined ? new WS(u) : new WS(u, p);
    };
    window.WebSocket.prototype = WS.prototype;
  }
})();"#;

/// Defines `window.__plonixClickables()`, shared by the collector and the
/// clicker so both see the same candidates under the same keys. Candidates
/// are buttons and script links outside any form: links with a real target
/// are followed by navigation instead, and nothing that submits a form is
/// ever a candidate.
pub const CLICKABLES_JS: &str = r##"window.__plonixClickables = () => {
  const sel = 'button, [role=button], [role=link], [role=tab], [role=menuitem], a:not([href]), a[href^="#"], a[href^="javascript:"], [onclick]';
  const list = [];
  for (const el of document.querySelectorAll(sel)) {
    if (el.closest('form') || el.disabled || el.getAttribute('aria-disabled') === 'true' || el.hasAttribute('target')) continue;
    if (el.matches('input[type=submit], input[type=image], [form]') || !el.getClientRects().length) continue;
    const text = (el.innerText || el.value || '').trim().replace(/\s+/g, ' ').slice(0, 80);
    const label = [text, el.getAttribute('aria-label'), el.getAttribute('title'), el.id, el.getAttribute('name'), el.getAttribute('href'), typeof el.className === 'string' ? el.className : '']
      .filter(Boolean).join(' ');
    list.push({ el, key: el.tagName.toLowerCase() + '|' + label, label });
  }
  return list;
};"##;

/// Evaluated (after [`CLICKABLES_JS`]) on a rendered page; returns a
/// [`PageScan`] as a JSON string.
pub const COLLECT_JS: &str = r#"(() => {
  const out = { location: location.href, links: [], forms: [], clickables: [] };
  const seen = new Set();
  const add = (u) => { try { const h = new URL(String(u), location.href).href; if (!seen.has(h)) { seen.add(h); out.links.push(h); } } catch (_) {} };
  document.querySelectorAll('a[href], area[href]').forEach((a) => add(a.getAttribute('href')));
  document.querySelectorAll('iframe[src], frame[src]').forEach((f) => add(f.getAttribute('src')));
  const routeAttrs = ['routerlink', 'ng-reflect-router-link', 'data-href', 'data-url', 'data-route', 'data-link', 'to'];
  document.querySelectorAll(routeAttrs.map((a) => '[' + a + ']').join(',')).forEach((el) => {
    for (const a of routeAttrs) { const v = el.getAttribute(a); if (v && /^[\/.?#\w]/.test(v)) add(v); }
  });
  (window.__plonixRoutes || []).forEach(add);
  document.querySelectorAll('form').forEach((f) => {
    const fields = [];
    for (const el of f.querySelectorAll('input[name], select[name], textarea[name]')) if (!fields.includes(el.name)) fields.push(el.name);
    let action = location.href;
    try { action = new URL(f.getAttribute('action') || '', location.href).href; } catch (_) {}
    out.forms.push({ method: (f.getAttribute('method') || 'GET').toUpperCase(), action, fields });
  });
  out.clickables = window.__plonixClickables().map((c) => ({ key: c.key, label: c.label }));
  return JSON.stringify(out);
})()"#;

/// Clicks the first candidate with the given key (evaluated after
/// [`CLICKABLES_JS`]); true if one was found.
pub fn click_js(key: &str) -> String {
    let key = serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into());
    format!("(() => {{ const c = window.__plonixClickables().find((c) => c.key === {key}); if (!c) return false; c.el.click(); return true; }})()")
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
    fn long_pages_cut_on_a_character_boundary() {
        let html = format!("{}<a href=\"/x\">", "€".repeat(MAX_BODY_SCAN));
        assert!(extract_links(&html).is_empty());
        assert!(extract_forms(&html).is_empty());
    }

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

    fn rules(accepted: &[&str], rejected: &[&str]) -> ScopeRules {
        use crate::scope::Rule;
        let rule = |p: &&str, decision| Rule { pattern: p.to_string(), include_subdomains: false, decision, created_at: 0, note: String::new() };
        ScopeRules { rules: accepted.iter().map(|p| rule(p, Decision::Accepted)).chain(rejected.iter().map(|p| rule(p, Decision::Rejected))).collect() }
    }

    #[test]
    fn follow_stays_on_the_host_and_in_scope() {
        let r = rules(&["app.test"], &[]);
        let page = "https://app.test/shop/list?page=2";
        assert_eq!(follow(&r, page, "item/7").as_deref(), Some("https://app.test/shop/item/7"));
        assert_eq!(follow(&r, page, "https://app.test/cart#top").as_deref(), Some("https://app.test/cart"));
        assert_eq!(follow(&r, page, "https://other.test/x"), None);
        assert_eq!(follow(&r, page, "javascript:void(0)"), None);
        // Even the page's own host is not followed once it is out of scope.
        assert_eq!(follow(&rules(&[], &["app.test"]), page, "/a"), None);
        assert_eq!(follow(&rules(&[], &[]), page, "/a"), None);
        // A page that is not on the web has nothing to follow.
        assert_eq!(follow(&r, "about:blank", "/a"), None);
    }

    #[test]
    fn requests_to_hosts_outside_scope_are_blocked() {
        let r = rules(&["app.test"], &["tracker.test"]);
        assert_eq!(blocked_host(&r, "https://app.test:8443/api?x=1"), None);
        assert_eq!(blocked_host(&r, "wss://app.test/live"), None);
        assert_eq!(blocked_host(&r, "https://cdn.other.test/lib.js").as_deref(), Some("cdn.other.test"));
        assert_eq!(blocked_host(&r, "http://tracker.test/p").as_deref(), Some("tracker.test"));
        assert_eq!(blocked_host(&r, "data:text/plain,hi"), None);
        assert_eq!(blocked_host(&r, "blob:https://app.test/1234"), None);
    }

    #[test]
    fn destructive_labels_are_skipped() {
        for l in ["Log out", "Logout", "sign-out", "Delete project", "Remove", "Pay now", "Checkout", "Unsubscribe", "Cancel order", "Reset password", "/account/logout", "Send"] {
            assert!(looks_destructive(l), "{l} should be skipped");
        }
        for l in ["Next page", "Show more", "Settings", "Profile", "Open menu", "Tab: Details", "Load comments", "Expand"] {
            assert!(!looks_destructive(l), "{l} should be clickable");
        }
    }

    #[test]
    fn page_scans_parse_and_pick_safe_clicks() {
        let json = r#"{"location":"https://app.test/","links":["https://app.test/a"],"forms":[{"method":"POST","action":"https://app.test/login","fields":["user"]}],
            "clickables":[{"key":"button|More","label":"More"},{"key":"button|More","label":"More"},{"key":"button|Delete","label":"Delete"},{"key":"a|Tab","label":"Tab"}]}"#;
        let scan: PageScan = serde_json::from_str(json).unwrap();
        assert_eq!(scan.links, vec!["https://app.test/a"]);
        assert_eq!(scan.forms[0].fields, vec!["user"]);
        let keys: Vec<&str> = scan.safe_clicks().iter().map(|c| c.key.as_str()).collect();
        assert_eq!(keys, vec!["button|More", "a|Tab"]);
        // Keys are embedded as JSON strings, so quotes cannot break out.
        assert!(click_js(r#"a"b"#).contains(r#"=== "a\"b")"#));
    }

    #[test]
    fn dedup_key_strips_query_values_and_fragments() {
        assert_eq!(dedup_key("https://h/s?q=1"), dedup_key("https://H/s?q=2"));
        assert_ne!(dedup_key("https://h/s?q=1"), dedup_key("https://h/s?q=1&p=2"));
        assert_eq!(dedup_key("https://h/a"), "https://h/a?");
    }
}
