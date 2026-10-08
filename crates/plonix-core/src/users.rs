//! Saved users: named sets of cookies and request headers (auth tokens) that
//! stand for the different people an application knows. The person acts as
//! one of them at a time (browser capture, the Bench and Scans send as that
//! user), the Bench can pick one per tab, and the access check replays a
//! request as each of them.
//!
//! A saved user is only data: a label, its cookies and a list of headers to
//! apply to a request before it is sent. Applying a user never widens scope:
//! every request still goes through the scope-gated send paths. Cookies the
//! server sets in answer to a request sent as a user are kept with that user,
//! so its session stays current. Users are stored per project (see
//! [`crate::store::Store::saved_users`]).

use serde::{Deserialize, Serialize};

use crate::detect::clean;
use crate::model::Headers;

/// Most users can hold at once. Enough for every role an application has,
/// without the switcher becoming a list to scroll.
pub const MAX_USERS: usize = 50;
/// Most headers one saved user may carry.
pub const MAX_HEADERS: usize = 40;
const MAX_NAME: usize = 60;
const MAX_NOTE: usize = 200;
const MAX_HEADER_NAME: usize = 120;
const MAX_HEADER_VALUE: usize = 8_192;
/// Most cookies one saved user may carry.
pub const MAX_COOKIES: usize = 100;
const MAX_COOKIE_NAME: usize = 200;
const MAX_COOKIE_VALUE: usize = 4_096;

/// The request headers stripped when a request is sent with no user (the
/// "signed out" identity), and the ones a saved user replaces rather than
/// adds to. Matched without regard to case.
pub const AUTH_HEADERS: &[&str] =
    &["cookie", "authorization", "x-api-key", "x-auth-token", "x-csrf-token", "x-xsrf-token", "x-session-token"];

/// Whether a header name is one the identities own, so that switching user
/// replaces it instead of leaving two copies. Beyond the fixed list, any name
/// that reads like a credential (the same test scope uses for session tokens).
pub fn is_auth_header(name: &str) -> bool {
    let k = name.to_ascii_lowercase();
    AUTH_HEADERS.contains(&k.as_str()) || ["token", "auth", "api-key", "apikey", "session"].iter().any(|w| k.contains(w))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedUser {
    /// Stable id the Bench and the access check refer to. `[a-z0-9-]`.
    /// Derived from the name when left out.
    #[serde(default)]
    pub id: String,
    /// What the person calls this user, e.g. "Alice (admin)".
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// Headers applied to a request sent as this user, other than cookies:
    /// usually an `Authorization` or an API-key header. A `Cookie` header
    /// given here is split into [`Self::cookies`] when the user is saved.
    #[serde(default)]
    pub headers: Headers,
    /// The user's cookies, sent as one `Cookie` header.
    #[serde(default)]
    pub cookies: Vec<Cookie>,
    /// Keep the cookies current: a cookie the server sets or clears in answer
    /// to a request sent as this user is updated here.
    #[serde(default = "yes")]
    pub keep_fresh: bool,
}

fn yes() -> bool {
    true
}

/// One cookie of a saved user.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// The host the cookie is sent to, and its subdomains. Empty means every
    /// in-scope host.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub domain: String,
    /// When the cookie stops being sent, in seconds since 1970. None keeps it
    /// until it is removed or expired by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
}

impl Cookie {
    pub fn live(&self, now: i64) -> bool {
        self.expires.is_none_or(|t| t > now)
    }

    fn sent_to(&self, host: &str) -> bool {
        let d = self.domain.as_str();
        d.is_empty() || host.eq_ignore_ascii_case(d) || host.to_ascii_lowercase().ends_with(&format!(".{d}"))
    }
}

/// How an exchange sent as a saved user says so in its `replaced` notes,
/// followed by the user's name.
pub const SENT_AS: &str = "sent as saved user: ";

/// Seconds since 1970, the clock cookie expiry is measured on.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl SavedUser {
    /// The headers a request to `host` carries when sent as this user: its
    /// own headers, then one `Cookie` header with its live cookies for that
    /// host. The caller removes the request's own auth headers first.
    pub fn request_headers(&self, host: &str, now: i64) -> Headers {
        let mut out = self.headers.clone();
        let jar: Vec<String> =
            self.cookies.iter().filter(|c| c.live(now) && c.sent_to(host)).map(|c| format!("{}={}", c.name, c.value)).collect();
        if !jar.is_empty() {
            out.push(("Cookie".into(), jar.join("; ")));
        }
        out
    }

    /// The headers a request from this user's own browser window carries to
    /// `host`. The window keeps its own session: its headers go out as they
    /// are, and the user's headers and live cookies fill in only what it did
    /// not send. So a window opened for a user with saved cookies starts
    /// signed in, and signing in there replaces them.
    pub fn window_headers(&self, host: &str, sent: &Headers, now: i64) -> Headers {
        let mut out = sent.clone();
        for (k, v) in &self.headers {
            if !out.iter().any(|(o, _)| o.eq_ignore_ascii_case(k)) {
                out.push((k.clone(), v.clone()));
            }
        }
        let has: Vec<String> = out
            .iter()
            .filter(|(k, _)| k.eq_ignore_ascii_case("cookie"))
            .flat_map(|(_, v)| v.split(';').filter_map(|p| p.split_once('=').map(|(n, _)| n.trim().to_string())))
            .collect();
        let missing: Vec<String> =
            self.cookies.iter().filter(|c| c.live(now) && c.sent_to(host) && !has.contains(&c.name)).map(|c| format!("{}={}", c.name, c.value)).collect();
        if !missing.is_empty() {
            match out.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case("cookie")) {
                Some((_, v)) if !v.trim().is_empty() => *v = format!("{v}; {}", missing.join("; ")),
                Some((_, v)) => *v = missing.join("; "),
                None => out.push(("Cookie".into(), missing.join("; "))),
            }
        }
        out
    }

    /// Moves any `Cookie` header into [`Self::cookies`], one entry per
    /// cookie, so cookies can be seen and changed one by one. Users saved
    /// before cookies were kept separately carry them as a header.
    pub fn fold_cookie_header(&mut self) {
        let mut found = vec![];
        self.headers.retain(|(k, v)| {
            let cookie = k.eq_ignore_ascii_case("cookie");
            if cookie {
                found.push(v.clone());
            }
            !cookie
        });
        for line in found {
            for pair in line.split(';') {
                let Some((n, v)) = pair.split_once('=') else { continue };
                let n = n.trim();
                if n.is_empty() {
                    continue;
                }
                match self.cookies.iter_mut().find(|c| c.name == n && c.domain.is_empty()) {
                    Some(c) => c.value = v.trim().to_string(),
                    None => self.cookies.push(Cookie { name: n.into(), value: v.trim().into(), domain: String::new(), expires: None }),
                }
            }
        }
    }

    /// Takes in the cookies a server set in a response to a request sent as
    /// this user to `host`. A cookie the server clears is kept, marked
    /// expired, so the person can see the session ended. Returns whether
    /// anything changed.
    pub fn absorb(&mut self, host: &str, resp_headers: &[(String, String)], now: i64) -> bool {
        let mut changed = false;
        for (k, v) in resp_headers {
            if !k.eq_ignore_ascii_case("set-cookie") {
                continue;
            }
            let Some(set) = parse_set_cookie(v, now) else { continue };
            let room = self.cookies.len() < MAX_COOKIES;
            let slot = self.cookies.iter_mut().find(|c| c.name == set.name && (c.domain == set.domain || c.domain.is_empty() || (set.domain.is_empty() && c.sent_to(host))));
            match slot {
                Some(c) => {
                    let domain = if set.domain.is_empty() { c.domain.clone() } else { set.domain.clone() };
                    let next = Cookie { domain, ..set };
                    if *c != next {
                        *c = next;
                        changed = true;
                    }
                }
                None if room && set.live(now) => {
                    let domain = if set.domain.is_empty() { host.to_ascii_lowercase() } else { set.domain.clone() };
                    self.cookies.push(Cookie { domain, ..set });
                    changed = true;
                }
                None => {}
            }
        }
        changed
    }
}

/// Reads a `Set-Cookie` value: the cookie, the domain it names and when it
/// expires (`Max-Age` wins over `Expires`, as browsers do).
fn parse_set_cookie(v: &str, now: i64) -> Option<Cookie> {
    let mut parts = v.split(';');
    let (name, value) = parts.next()?.split_once('=')?;
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_COOKIE_NAME || value.len() > MAX_COOKIE_VALUE {
        return None;
    }
    let (mut domain, mut max_age, mut expires) = (String::new(), None, None);
    for attr in parts {
        let (k, val) = attr.split_once('=').unwrap_or((attr, ""));
        match k.trim().to_ascii_lowercase().as_str() {
            "domain" => domain = val.trim().trim_start_matches('.').to_ascii_lowercase(),
            "max-age" => max_age = val.trim().parse::<i64>().ok(),
            "expires" => expires = http_date(val.trim()),
            _ => {}
        }
    }
    let expires = match max_age {
        Some(n) if n <= 0 => Some(now),
        Some(n) => Some(now.saturating_add(n)),
        None => expires,
    };
    Some(Cookie { name: name.into(), value: value.trim().into(), domain, expires })
}

/// Seconds since 1970 for a cookie date such as `Wed, 21 Oct 2015 07:28:00
/// GMT` (or the `21-Oct-2015` form older servers send).
fn http_date(s: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let (mut day, mut month, mut year, mut hms) = (None, None, None, None);
    for tok in s.split([' ', '-', ',']).filter(|t| !t.is_empty()) {
        let low = tok.to_ascii_lowercase();
        if tok.contains(':') {
            let n: Vec<i64> = tok.split(':').filter_map(|x| x.parse().ok()).collect();
            if n.len() == 3 {
                hms = Some(n[0] * 3600 + n[1] * 60 + n[2]);
            }
        } else if let Some(m) = MONTHS.iter().position(|m| low.starts_with(m)) {
            month = Some(m as i64 + 1);
        } else if let Ok(n) = tok.parse::<i64>() {
            if tok.len() <= 2 && day.is_none() {
                day = Some(n);
            } else {
                year = Some(if n < 70 { 2000 + n } else if n < 100 { 1900 + n } else { n });
            }
        }
    }
    let (d, m, y) = (day?, month?, year?);
    // Days from 1970-01-01 to y-m-d (proleptic Gregorian).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * 86_400 + hms.unwrap_or(0))
}

/// Checks and tidies a list of saved users from the client. Returns the
/// cleaned list or the first problem, so one bad entry cannot corrupt the
/// store. Ids are filled in and de-duplicated here.
pub fn check(users: &[SavedUser]) -> Result<Vec<SavedUser>, String> {
    if users.len() > MAX_USERS {
        return Err(format!("at most {MAX_USERS} saved users"));
    }
    let mut out: Vec<SavedUser> = Vec::with_capacity(users.len());
    for (i, u) in users.iter().enumerate() {
        let name = u.name.trim();
        if name.is_empty() {
            return Err(format!("user {}: a name is required", i + 1));
        }
        if name.chars().count() > MAX_NAME {
            return Err(format!("user {}: the name is longer than {MAX_NAME} characters", i + 1));
        }
        if u.note.chars().count() > MAX_NOTE {
            return Err(format!("{name}: the note is longer than {MAX_NOTE} characters"));
        }
        if u.headers.len() > MAX_HEADERS {
            return Err(format!("{name}: at most {MAX_HEADERS} headers"));
        }
        let mut headers: Headers = Vec::with_capacity(u.headers.len());
        for (k, v) in &u.headers {
            let k = k.trim();
            if k.is_empty() {
                continue; // an empty row from the editor: drop it
            }
            if k.len() > MAX_HEADER_NAME || !k.bytes().all(is_header_name_byte) {
                return Err(format!("{name}: `{}` is not a valid header name", clean(k, 40)));
            }
            if v.len() > MAX_HEADER_VALUE {
                return Err(format!("{name}: the value of `{k}` is longer than {MAX_HEADER_VALUE} bytes"));
            }
            if v.bytes().any(|b| b == b'\r' || b == b'\n') {
                return Err(format!("{name}: the value of `{k}` contains a line break"));
            }
            headers.push((k.to_string(), v.clone()));
        }
        let mut user = SavedUser { headers, cookies: u.cookies.clone(), ..u.clone() };
        user.fold_cookie_header();
        if user.cookies.len() > MAX_COOKIES {
            return Err(format!("{name}: at most {MAX_COOKIES} cookies"));
        }
        let mut cookies: Vec<Cookie> = Vec::with_capacity(user.cookies.len());
        for c in user.cookies {
            let n = c.name.trim();
            if n.is_empty() {
                continue; // an empty row from the editor: drop it
            }
            if n.len() > MAX_COOKIE_NAME || n.bytes().any(|b| b.is_ascii_control() || b"=; ,".contains(&b)) {
                return Err(format!("{name}: `{}` is not a valid cookie name", clean(n, 40)));
            }
            if c.value.len() > MAX_COOKIE_VALUE || c.value.bytes().any(|b| b.is_ascii_control() || b == b';') {
                return Err(format!("{name}: the value of cookie `{n}` is too long or contains a `;` or a line break"));
            }
            let domain = c.domain.trim().trim_start_matches('.').to_ascii_lowercase();
            if !domain.bytes().all(|b| b.is_ascii_alphanumeric() || b"-.:[]".contains(&b)) {
                return Err(format!("{name}: `{}` is not a valid cookie domain", clean(&domain, 40)));
            }
            match cookies.iter_mut().find(|x| x.name == n && x.domain == domain) {
                Some(x) => (x.value, x.expires) = (c.value.trim().to_string(), c.expires),
                None => cookies.push(Cookie { name: n.to_string(), value: c.value.trim().to_string(), domain, expires: c.expires }),
            }
        }
        let id = make_id(&u.id, name, &out);
        out.push(SavedUser { id, name: name.to_string(), note: u.note.trim().to_string(), headers: user.headers, cookies, keep_fresh: u.keep_fresh });
    }
    Ok(out)
}

fn is_header_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_".contains(&b)
}

/// Keeps a client-supplied id when it is well formed and unused, otherwise
/// derives a fresh one from the name.
fn make_id(given: &str, name: &str, taken: &[SavedUser]) -> String {
    let used = |id: &str| taken.iter().any(|u| u.id == id);
    let g = given.trim();
    if !g.is_empty() && g.len() <= 40 && g.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !used(g) {
        return g.to_string();
    }
    let mut base = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if !base.ends_with('-') {
            base.push('-');
        }
    }
    let base: String = base.trim_matches('-').chars().take(32).collect();
    let base = if base.is_empty() { "user".to_string() } else { base };
    if !used(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|id| !used(id)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(name: &str, headers: &[(&str, &str)]) -> SavedUser {
        SavedUser {
            id: String::new(),
            name: name.into(),
            note: String::new(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            cookies: vec![],
            keep_fresh: true,
        }
    }

    fn c(name: &str, value: &str) -> Cookie {
        Cookie { name: name.into(), value: value.into(), domain: String::new(), expires: None }
    }

    #[test]
    fn a_cookie_header_is_split_into_cookies() {
        let out = check(&[u("A", &[("Cookie", "s=1; theme=dark"), ("Authorization", "Bearer x")])]).unwrap();
        assert_eq!(out[0].headers, vec![("Authorization".to_string(), "Bearer x".to_string())]);
        assert_eq!(out[0].cookies, vec![c("s", "1"), c("theme", "dark")]);
        assert!(check(&[SavedUser { cookies: vec![c("a b", "1")], ..u("A", &[]) }]).unwrap_err().contains("cookie name"));
    }

    #[test]
    fn request_headers_send_only_live_cookies_for_the_host() {
        let user = SavedUser {
            cookies: vec![
                c("s", "1"),
                Cookie { expires: Some(50), ..c("old", "x") },
                Cookie { domain: "shop.test".into(), ..c("cart", "9") },
                Cookie { domain: "other.test".into(), ..c("no", "0") },
            ],
            ..u("A", &[("Authorization", "Bearer t")])
        };
        let h = user.request_headers("api.shop.test", 100);
        assert_eq!(h, vec![("Authorization".to_string(), "Bearer t".to_string()), ("Cookie".to_string(), "s=1; cart=9".to_string())]);
        assert_eq!(user.request_headers("x.test", 10)[1].1, "s=1; old=x");
    }

    #[test]
    fn a_users_window_keeps_its_own_session_and_fills_the_gaps() {
        let user = SavedUser {
            cookies: vec![c("s", "saved"), c("theme", "dark"), Cookie { expires: Some(50), ..c("old", "x") }],
            ..u("A", &[("Authorization", "Bearer saved"), ("X-Team", "red")])
        };
        let hv = |pairs: &[(&str, &str)]| -> Headers { pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect() };
        // Nothing of its own yet: the window goes out with the user's session.
        let fresh = user.window_headers("shop.test", &hv(&[("Accept", "*/*")]), 100);
        assert_eq!(fresh, hv(&[("Accept", "*/*"), ("Authorization", "Bearer saved"), ("X-Team", "red"), ("Cookie", "s=saved; theme=dark")]));
        // Signed in there: its own session wins, the user only fills the gaps.
        let own = user.window_headers("shop.test", &hv(&[("cookie", "s=window"), ("authorization", "Bearer window")]), 100);
        assert_eq!(own, hv(&[("cookie", "s=window; theme=dark"), ("authorization", "Bearer window"), ("X-Team", "red")]));
    }

    #[test]
    fn set_cookie_updates_adds_and_expires() {
        let mut user = SavedUser { cookies: vec![c("s", "1"), c("gone", "x")], ..u("A", &[]) };
        let resp = vec![
            ("Set-Cookie".to_string(), "s=2; Path=/; HttpOnly".to_string()),
            ("set-cookie".to_string(), "fresh=y; Max-Age=60; Domain=.shop.test".to_string()),
            ("Set-Cookie".to_string(), "gone=; Expires=Thu, 01 Jan 1970 00:00:00 GMT".to_string()),
        ];
        assert!(user.absorb("www.shop.test", &resp, 1000));
        assert_eq!(user.cookies[0], c("s", "2"));
        assert_eq!(user.cookies[1].expires, Some(0));
        assert!(!user.cookies[1].live(1000));
        assert_eq!(user.cookies[2], Cookie { domain: "shop.test".into(), expires: Some(1060), ..c("fresh", "y") });
        assert!(!user.absorb("www.shop.test", &resp[..1], 1000), "the same value again changes nothing");
    }

    #[test]
    fn cookie_dates_parse() {
        assert_eq!(http_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1_445_412_480));
        assert_eq!(http_date("Wed, 21-Oct-2015 07:28:00 GMT"), Some(1_445_412_480));
        assert_eq!(http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(http_date("soon"), None);
    }

    #[test]
    fn ids_are_derived_and_unique() {
        let out = check(&[u("Alice (admin)", &[("Cookie", "s=1")]), u("Alice (admin)", &[]), u("Bob", &[])]).unwrap();
        assert_eq!(out[0].id, "alice-admin");
        assert_eq!(out[1].id, "alice-admin-2");
        assert_eq!(out[2].id, "bob");
        assert_eq!(out[0].cookies, vec![c("s", "1")]);
    }

    #[test]
    fn a_given_id_is_kept_when_free() {
        let out = check(&[SavedUser { id: "carol".into(), ..u("Carol", &[]) }]).unwrap();
        assert_eq!(out[0].id, "carol");
    }

    #[test]
    fn empty_header_rows_are_dropped_and_bad_ones_rejected() {
        let out = check(&[u("A", &[("", ""), ("X-Api-Key", "x")])]).unwrap();
        assert_eq!(out[0].headers, vec![("X-Api-Key".to_string(), "x".to_string())]);
        assert!(check(&[u("A", &[("bad header", "x")])]).unwrap_err().contains("not a valid header name"));
        assert!(check(&[u("A", &[("Cookie", "a\r\nb")])]).unwrap_err().contains("line break"));
        assert!(check(&[u("", &[])]).unwrap_err().contains("name is required"));
    }

    #[test]
    fn auth_headers_recognised() {
        assert!(is_auth_header("cookie") && is_auth_header("Authorization") && is_auth_header("X-API-Key"));
        assert!(!is_auth_header("Accept"));
        assert!(is_auth_header("X-Access-Token") && is_auth_header("Api-Key") && is_auth_header("X-Session-Id"));
    }
}
