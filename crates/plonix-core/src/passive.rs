//! Passive checks: analysis over already-captured traffic that sends nothing.
//!
//! A scan runs these alongside its active tactics, so a person sees the issues
//! already visible in the traffic they captured — cookie flags, CORS, open
//! redirects, verbose errors, version and source-map disclosure — without a
//! single new request. Every check reads one captured [`Exchange`] and, being
//! read-only, is always safe to run; the scan runner still only ever looks at
//! hosts the user accepted into scope.
//!
//! The checks are deliberately conservative: each flags a concrete, named
//! condition with the exchange as evidence, and leans toward silence over a
//! guess, because a noisy assistant is one a researcher stops trusting.

use crate::model::{header, header_all, Exchange};
use crate::scan::Severity;

/// One issue a passive check found in a captured exchange.
#[derive(Debug, Clone, PartialEq)]
pub struct PassiveFinding {
    /// Stable check id, e.g. `insecure-session-cookie`.
    pub check: &'static str,
    pub title: String,
    pub severity: Severity,
    pub description: String,
    /// The captured exchange that is the evidence.
    pub exchange_id: i64,
    /// OWASP category this maps to, e.g. `A05`.
    pub owasp: &'static str,
}

/// Collects findings while collapsing repeats: a host-wide issue (one Server
/// version, one CORS policy) is reported once, not once per request, while
/// genuinely distinct issues (a different cookie, script or endpoint) each
/// stand on their own. The first matching exchange is kept as the evidence.
#[derive(Default)]
struct Collector {
    seen: std::collections::BTreeSet<String>,
    out: Vec<PassiveFinding>,
}

impl Collector {
    fn add(&mut self, key: String, f: PassiveFinding) {
        if self.seen.insert(key) {
            self.out.push(f);
        }
    }
}

/// Every passive check, run over a sample of a host's captured exchanges.
/// Pure: reads only what it is given and sends nothing. Repeats of the same
/// issue across many exchanges are collapsed to one finding.
pub fn run(exchanges: &[Exchange]) -> Vec<PassiveFinding> {
    let mut c = Collector::default();
    for ex in exchanges {
        insecure_session_cookie(ex, &mut c);
        permissive_cors(ex, &mut c);
        open_redirect_reflection(ex, &mut c);
        exposed_source_map(ex, &mut c);
        verbose_error(ex, &mut c);
        version_disclosure(ex, &mut c);
    }
    c.out
}

/// A session-like cookie set without `Secure` (on https) or without `HttpOnly`.
/// Only session-ish cookies are judged, so an analytics cookie without flags
/// does not raise noise.
fn insecure_session_cookie(ex: &Exchange, c: &mut Collector) {
    for sc in header_all(&ex.resp_headers, "set-cookie") {
        let Some((name, attrs)) = split_cookie(sc) else { continue };
        if !looks_like_session(name) {
            continue;
        }
        let has = |a: &str| attrs.iter().any(|x| x.eq_ignore_ascii_case(a));
        let mut missing = Vec::new();
        if ex.scheme == "https" && !has("secure") {
            missing.push("Secure");
        }
        if !has("httponly") {
            missing.push("HttpOnly");
        }
        if missing.is_empty() {
            continue;
        }
        let key = format!("cookie:{}:{name}", ex.host);
        c.add(key, PassiveFinding {
            check: "insecure-session-cookie",
            title: format!("Session cookie `{name}` without {}", missing.join(" and ")),
            severity: Severity::Medium,
            description: format!(
                "The response sets the session cookie `{name}` without the {} flag. Without Secure it can be sent over plain HTTP; without HttpOnly it is readable from JavaScript, so a cross-site scripting bug can steal it.",
                missing.join(" and ")
            ),
            exchange_id: ex.id,
            owasp: "A05",
        });
    }
}

/// CORS that reflects the caller's Origin together with credentials — the
/// combination that actually exposes authenticated data cross-origin. A bare
/// `*` (no credentials) is normal for public assets and is not flagged.
fn permissive_cors(ex: &Exchange, c: &mut Collector) {
    let Some(acao) = header(&ex.resp_headers, "access-control-allow-origin") else { return };
    let creds = header(&ex.resp_headers, "access-control-allow-credentials").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
    if !creds {
        return;
    }
    let origin = header(&ex.req_headers, "origin").map(str::trim).filter(|o| !o.is_empty());
    let reflects = acao.trim() == "*" || origin.is_some_and(|o| o.eq_ignore_ascii_case(acao.trim()));
    if !reflects {
        return;
    }
    c.add(format!("cors:{}", ex.host), PassiveFinding {
        check: "permissive-cors",
        title: "CORS reflects the caller's origin with credentials".into(),
        severity: Severity::High,
        description: format!(
            "The response returns `Access-Control-Allow-Origin: {}` together with `Access-Control-Allow-Credentials: true`. Any origin this is reflected to can read authenticated responses from this endpoint in a victim's browser.",
            acao.trim()
        ),
        exchange_id: ex.id,
        owasp: "A05",
    });
}

/// A redirect whose `Location` is taken from a request parameter — the shape of
/// an open redirect. Conservative: a parameter value must make up the whole
/// destination, so a tracking parameter merely appended is not flagged.
fn open_redirect_reflection(ex: &Exchange, c: &mut Collector) {
    if !matches!(ex.status, Some(301 | 302 | 303 | 307 | 308)) {
        return;
    }
    let Some(location) = header(&ex.resp_headers, "location").map(str::trim).filter(|l| !l.is_empty()) else { return };
    for (key, value) in query_pairs(&ex.query) {
        if value.len() < 4 {
            continue;
        }
        // The parameter drives the destination: it is the Location, or the
        // Location is that value (optionally with a scheme) pointing off-path.
        let drives = location == value || location.ends_with(&value) && (looks_like_url(&value) || value.starts_with('/'));
        if !drives {
            continue;
        }
        c.add(format!("redirect:{}:{}:{key}", ex.host, ex.path), PassiveFinding {
            check: "open-redirect-reflection",
            title: format!("Redirect destination comes from the `{key}` parameter"),
            severity: Severity::Medium,
            description: format!(
                "This {} redirect sends the browser to `{location}`, taken from the request parameter `{key}`. If the destination is not restricted to this site, it is an open redirect that can lend this host's trust to a phishing page.",
                ex.status.unwrap_or(0)
            ),
            exchange_id: ex.id,
            owasp: "A01",
        });
        return;
    }
}

/// A script that ships a reference to its source map, which hands a reader the
/// original, unminified source.
fn exposed_source_map(ex: &Exchange, c: &mut Collector) {
    let is_js = ex.mime().contains("javascript") || ex.path.ends_with(".js");
    if !is_js {
        return;
    }
    let body = String::from_utf8_lossy(&ex.resp_body);
    let Some(idx) = body.find("sourceMappingURL=").filter(|&i| {
        // Only the real `//# sourceMappingURL=` / `//@ ` comment forms.
        body[..i].trim_end().ends_with("//#") || body[..i].trim_end().ends_with("//@")
    }) else {
        return;
    };
    let target: String = body[idx + "sourceMappingURL=".len()..].split_whitespace().next().unwrap_or("").chars().take(120).collect();
    if target.starts_with("data:") || target.is_empty() {
        return; // inline map: nothing extra exposed
    }
    c.add(format!("sourcemap:{}:{}", ex.host, ex.path), PassiveFinding {
        check: "exposed-source-map",
        title: "Script links its source map".into(),
        severity: Severity::Low,
        description: format!(
            "`{}` references the source map `{target}`. If that map is reachable, it reveals the original, unminified source — helpful to an attacker reading the client.",
            ex.path
        ),
        exchange_id: ex.id,
        owasp: "A05",
    });
}

/// A response body carrying a recognizable stack trace or framework error page,
/// which leaks internal paths, versions and sometimes query fragments.
fn verbose_error(ex: &Exchange, c: &mut Collector) {
    // Only worth looking at error-ish responses; a 200 page rarely carries a
    // genuine stack trace, and judging one risks flagging ordinary prose.
    if !matches!(ex.status, Some(s) if s >= 500) {
        return;
    }
    let body = String::from_utf8_lossy(&ex.resp_body);
    let hay = &body[..body.len().min(20_000)];
    // Distinctive, framework-specific signatures — not the bare word "error".
    const SIGNS: &[&str] = &[
        "Traceback (most recent call last)",
        "at java.",
        "org.springframework",
        "System.Web",
        "Microsoft .NET",
        "ActiveRecord::",
        "Rails.root",
        "Fatal error:",
        "Warning: ",
        "Notice: ",
        "Stack trace:",
        "goroutine ",
        "You have an error in your SQL syntax",
        "SQLSTATE[",
        "ORA-0",
    ];
    let Some(sign) = SIGNS.iter().find(|s| hay.contains(*s)) else { return };
    c.add(format!("verbose:{}:{}", ex.host, ex.path), PassiveFinding {
        check: "verbose-error",
        title: "Server error page leaks internal detail".into(),
        severity: Severity::Medium,
        description: format!(
            "This {} response includes a stack trace or framework error (matched `{sign}`), which exposes internal paths, component versions and sometimes request data. Error pages shown to users should be generic.",
            ex.status.unwrap_or(0)
        ),
        exchange_id: ex.id,
        owasp: "A05",
    });
}

/// A `Server` or `X-Powered-By` header that names a component and its version.
fn version_disclosure(ex: &Exchange, c: &mut Collector) {
    for name in ["server", "x-powered-by"] {
        let Some(value) = header(&ex.resp_headers, name).map(str::trim).filter(|v| !v.is_empty()) else { continue };
        if !has_version(value) {
            continue; // a bare "nginx" or "Express" names no version to exploit
        }
        c.add(format!("version:{}:{name}:{value}", ex.host), PassiveFinding {
            check: "version-disclosure",
            title: format!("`{}` header reveals a component version", header_label(name)),
            severity: Severity::Info,
            description: format!(
                "The response advertises `{}: {value}`. A precise version lets an attacker look up known vulnerabilities for it; the version is usually safe to drop from the header.",
                header_label(name)
            ),
            exchange_id: ex.id,
            owasp: "A05",
        });
        return; // one disclosure per exchange is enough
    }
}

// --- small helpers --------------------------------------------------------

/// Splits a `Set-Cookie` value into its cookie name and the lowercased list of
/// attribute keywords (`secure`, `httponly`, `samesite`, …).
fn split_cookie(sc: &str) -> Option<(&str, Vec<String>)> {
    let mut parts = sc.split(';');
    let first = parts.next()?.trim();
    let name = first.split('=').next()?.trim();
    if name.is_empty() {
        return None;
    }
    let attrs = parts.map(|p| p.trim().split('=').next().unwrap_or("").trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect();
    Some((name, attrs))
}

/// Whether a cookie name looks like it carries a session or credential.
fn looks_like_session(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    const HINTS: &[&str] = &["session", "sess", "sid", "auth", "token", "jwt", "csrf", "xsrf", "login", "remember"];
    // Well-known framework session cookie names.
    const EXACT: &[&str] = &["phpsessid", "jsessionid", "asp.net_sessionid", "connect.sid", "_session_id", "laravel_session"];
    EXACT.contains(&n.as_str()) || HINTS.iter().any(|h| n.contains(h))
}

fn query_pairs(query: &str) -> impl Iterator<Item = (String, String)> + '_ {
    query.split('&').filter(|p| !p.is_empty()).map(|p| {
        let (k, v) = p.split_once('=').unwrap_or((p, ""));
        (k.to_string(), percent_decode(v))
    })
}

/// Minimal percent-decoding for comparing a query value to a Location, enough
/// for `%2F` and `%3A` in a redirect target; unknown escapes are left as-is.
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let decoded = (b[i] == b'%' && i + 2 < b.len()).then(|| u8::from_str_radix(&s[i + 1..i + 3], 16).ok()).flatten();
        if let Some(byte) = decoded {
            out.push(byte as char);
            i += 3;
        } else {
            out.push(if b[i] == b'+' { ' ' } else { b[i] as char });
            i += 1;
        }
    }
    out
}

fn looks_like_url(v: &str) -> bool {
    v.starts_with("http://") || v.starts_with("https://") || v.starts_with("//")
}

/// A header value that contains a version number like `1.25` or `8.1.2`.
fn has_version(v: &str) -> bool {
    let b = v.as_bytes();
    (0..b.len()).any(|i| b[i].is_ascii_digit() && b[i + 1..].iter().take_while(|c| c.is_ascii_digit() || **c == b'.').any(|c| *c == b'.'))
}

fn header_label(lower: &str) -> &'static str {
    match lower {
        "server" => "Server",
        "x-powered-by" => "X-Powered-By",
        _ => "Header",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Exchange;

    fn ex(scheme: &str, status: Option<u16>, req: &[(&str, &str)], resp: &[(&str, &str)], body: &str) -> Exchange {
        Exchange {
            id: 7,
            scheme: scheme.into(),
            host: "api.example".into(),
            path: "/x".into(),
            status,
            req_headers: req.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            resp_headers: resp.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            resp_body: body.as_bytes().to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn session_cookie_without_flags_is_flagged_but_tracking_is_not() {
        let f = run(&[ex("https", Some(200), &[], &[("Set-Cookie", "sessionid=abc; Path=/")], "")]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].check, "insecure-session-cookie");
        assert!(f[0].title.contains("Secure and HttpOnly"));

        // A non-session cookie, and a fully-flagged session cookie, stay quiet.
        let quiet = run(&[
            ex("https", Some(200), &[], &[("Set-Cookie", "_ga=GA1.2; Path=/")], ""),
            ex("https", Some(200), &[], &[("Set-Cookie", "sessionid=abc; Secure; HttpOnly")], ""),
        ]);
        assert!(quiet.is_empty(), "got {quiet:?}");
    }

    #[test]
    fn cors_needs_credentials_to_be_flagged() {
        let flagged = run(&[ex(
            "https",
            Some(200),
            &[("Origin", "https://evil.test")],
            &[("Access-Control-Allow-Origin", "https://evil.test"), ("Access-Control-Allow-Credentials", "true")],
            "",
        )]);
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].severity, Severity::High);

        // Bare `*` without credentials is normal and not flagged.
        let star = run(&[ex("https", Some(200), &[], &[("Access-Control-Allow-Origin", "*")], "")]);
        assert!(star.is_empty());
    }

    #[test]
    fn open_redirect_reflection_from_a_param() {
        let mut e = ex("https", Some(302), &[], &[("Location", "https://evil.test/landing")], "");
        e.query = "next=https%3A%2F%2Fevil.test%2Flanding".into();
        let f = run(&[e]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].check, "open-redirect-reflection");
        assert!(f[0].title.contains("next"));
    }

    #[test]
    fn verbose_error_only_on_5xx_with_a_signature() {
        let f = run(&[ex("https", Some(500), &[], &[], "Traceback (most recent call last):\n  File \"app.py\"")]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].check, "verbose-error");

        // A 200 JSON error body is not a stack trace.
        let ok = run(&[ex("https", Some(200), &[], &[], "{\"error\":\"bad request\"}")]);
        assert!(ok.is_empty());
    }

    #[test]
    fn version_disclosure_needs_a_number() {
        let f = run(&[ex("https", Some(200), &[], &[("Server", "nginx/1.25.4")], "")]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].severity, Severity::Info);

        let bare = run(&[ex("https", Some(200), &[], &[("Server", "nginx")], "")]);
        assert!(bare.is_empty());
    }

    #[test]
    fn a_host_wide_issue_collapses_to_one_finding() {
        // The same Server version on many exchanges is one issue, not many.
        let many: Vec<Exchange> = (0..10).map(|_| ex("https", Some(200), &[], &[("Server", "nginx/1.25.4")], "")).collect();
        let f = run(&many);
        assert_eq!(f.len(), 1, "a host-wide disclosure should be reported once");
    }

    #[test]
    fn source_map_reference_is_flagged_once() {
        let mut e = ex("https", Some(200), &[], &[("Content-Type", "application/javascript")], "console.log(1)\n//# sourceMappingURL=app.min.js.map");
        e.path = "/app.min.js".into();
        let f = run(&[e]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].check, "exposed-source-map");
    }
}
