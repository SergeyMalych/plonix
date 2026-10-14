//! Whether each saved user's session still works, and what it may do.
//!
//! Read from what Plonix already holds, without sending anything: the
//! expiry a token or cookie states (a JSON Web Token's `exp`, a cookie's
//! expiry, the `expires_in` of the answer that handed a token out), the
//! answers the server gave to requests that carried the user's credentials,
//! and the roles, scopes and groups the tokens and "who am I" answers name.
//!
//! A user is called expired only when its credentials say so, and refused
//! only when the server turned them away in a way that points at the
//! session itself (a 401, or a redirect to a sign-in page, where the same
//! credentials were accepted before or the answer says the token is no
//! good). A 403 is a missing permission, not an expired session. Where
//! nothing says, the answer is that it can't be told.

use std::collections::{BTreeMap, HashMap, HashSet};

use base64::Engine as _;
use serde::Serialize;

use crate::gather::{bare, sent_tokens, session_cookie, session_header};
use crate::model::{Exchange, Source};
use crate::scope::ScopeRules;
use crate::users::SavedUser;

/// How close an expiry has to be to call the session about to run out.
pub const SOON_SECS: i64 = 3_600;
/// Most endpoints listed as allowed, and as refused.
const MAX_SEEN: usize = 8;
/// Response bodies longer than this are not read for roles or a token's lifetime.
const MAX_BODY: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// The server accepted these credentials last time they were used.
    Working,
    /// Not used yet, but nothing has run out: the expiry is still ahead.
    Valid,
    /// A credential runs out within [`SOON_SECS`].
    Expiring,
    /// Some credentials have run out, or one part of the app refused them.
    Partly,
    /// The server turned the session away the last time it was used.
    Refused,
    /// Every credential it sends has passed its stated expiry.
    Expired,
    /// Nothing to go on: no stated expiry and no traffic as this user.
    Unknown,
}

/// What one saved user's session looks like.
#[derive(Debug, Clone, Serialize)]
pub struct Standing {
    pub user: String,
    pub state: State,
    /// The soonest a live credential runs out, in seconds since 1970.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<i64>,
    /// When the last of the run-out credentials ran out, in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expired_at: Option<i64>,
    /// The last answer that accepted these credentials.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accepted: Option<Answer>,
    /// The last answer that turned the session away.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<Answer>,
    /// The server accepted a credential after its stated expiry had passed.
    pub late_accept: bool,
    pub credentials: Vec<Lifetime>,
    pub grants: Vec<Grant>,
    /// Endpoints that answered these credentials with 2xx, newest first.
    pub allowed: Vec<Answer>,
    /// Endpoints that answered them with 401 or 403, newest first.
    pub blocked: Vec<Answer>,
    pub allowed_total: usize,
    pub blocked_total: usize,
    /// A captured request to replay as this user to find out for sure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probe: Option<Answer>,
}

/// One answer seen in traffic.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Answer {
    pub exchange: i64,
    /// When, in milliseconds since 1970.
    pub at: i64,
    pub method: String,
    pub host: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    /// What a refusal said, such as "token expired".
    #[serde(skip_serializing_if = "String::is_empty")]
    pub why: String,
}

/// When one of the user's credentials runs out, and how that is known.
#[derive(Debug, Clone, Serialize)]
pub struct Lifetime {
    /// `cookie` or `header`.
    pub kind: &'static str,
    pub name: String,
    /// Where it is sent: a cookie's domain, or a header's host and path.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub place: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issued: Option<i64>,
    /// `exp claim`, `cookie expiry`, `expires_in`, or empty when unknown.
    #[serde(skip_serializing_if = "str::is_empty")]
    pub source: &'static str,
    pub expired: bool,
}

/// One role, scope, group or permission the user's session names.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Grant {
    /// `role`, `scope`, `group`, `permission` or `flag`.
    pub kind: &'static str,
    pub value: String,
    /// Where it was read, such as "Authorization token" or "GET /v1/me answer".
    pub from: String,
}

/// The standing of every saved user, read from recent exchanges.
pub fn standings(exchanges: &[Exchange], rules: &ScopeRules, users: &[SavedUser], now: i64) -> Vec<Standing> {
    let mut ordered: Vec<&Exchange> = exchanges.iter().filter(|ex| ex.source != Some(Source::Scan) && rules.in_scope(&ex.host)).collect();
    ordered.sort_by_key(|ex| (ex.ts, ex.id));

    // Each user's credential values. A value two users share (a cart or a
    // preference cookie copied into both) says nothing about who sent it.
    let values: Vec<HashSet<String>> = users.iter().map(|u| creds(u).into_iter().map(|c| c.value).collect()).collect();
    let mut owners: HashMap<&str, usize> = HashMap::new();
    for set in &values {
        for v in set {
            *owners.entry(v.as_str()).or_default() += 1;
        }
    }
    users
        .iter()
        .zip(&values)
        .map(|(u, mine)| {
            let mine: HashSet<&str> = mine.iter().map(String::as_str).filter(|v| owners.get(v) == Some(&1)).collect();
            let sent: Vec<&Exchange> = ordered.iter().copied().filter(|ex| sent_tokens(ex).iter().any(|(_, _, v)| mine.contains(bare(v)))).collect();
            standing(u, &sent, &ordered, now)
        })
        .collect()
}

/// A credential of a saved user, with the bare token value.
struct Cred {
    kind: &'static str,
    name: String,
    place: String,
    value: String,
    cookie_expires: Option<i64>,
}

/// The credentials a user sends that carry a session: its auth headers and
/// session-looking cookies (a theme or promo cookie is not one).
fn creds(u: &SavedUser) -> Vec<Cred> {
    let mut out = vec![];
    for (k, v) in &u.headers {
        if session_header(k, v) {
            out.push(Cred { kind: "header", name: k.clone(), place: String::new(), value: bare(v).to_string(), cookie_expires: None });
        }
    }
    for sh in &u.scoped {
        if session_header(&sh.name, &sh.value) {
            out.push(Cred { kind: "header", name: sh.name.clone(), place: format!("{}{}", sh.host, sh.path), value: bare(&sh.value).to_string(), cookie_expires: None });
        }
    }
    for c in &u.cookies {
        if session_cookie(&c.name, &c.value) {
            out.push(Cred { kind: "cookie", name: c.name.clone(), place: c.domain.clone(), value: c.value.clone(), cookie_expires: c.expires });
        }
    }
    out
}

fn standing(u: &SavedUser, sent: &[&Exchange], all: &[&Exchange], now: i64) -> Standing {
    let creds = creds(u);
    let mut grants: Vec<Grant> = vec![];

    // How long each credential lives.
    let mut lives = vec![];
    for c in &creds {
        let claims = jwt_claims(&c.value);
        let (mut expires, mut issued, mut source) = (None, None, "");
        if let Some(cl) = &claims {
            expires = cl.get("exp").and_then(num);
            issued = cl.get("iat").and_then(num);
            if expires.is_some() {
                source = "exp claim";
            }
            let from = format!("{} {}", c.name, if c.kind == "cookie" { "cookie" } else { "token" });
            add_grants(&mut grants, cl, &from);
        }
        if expires.is_none() && c.cookie_expires.is_some() {
            (expires, source) = (c.cookie_expires, "cookie expiry");
        }
        if expires.is_none()
            && let Some((at, secs, body)) = handed_out(all, &c.value)
        {
            (expires, issued, source) = (Some(at / 1000 + secs), Some(at / 1000), "expires_in");
            add_grants(&mut grants, &body, "the answer that handed it out");
        }
        lives.push(Lifetime { kind: c.kind, name: c.name.clone(), place: c.place.clone(), expires, issued, source, expired: expires.is_some_and(|t| t <= now) });
    }

    // What the server made of them.
    let mut accepted: Option<Answer> = None;
    let mut refused: Option<Answer> = None;
    let mut ok_paths: HashSet<(String, String)> = HashSet::new();
    let mut seen: BTreeMap<(String, String, String), Answer> = BTreeMap::new();
    let mut who: Option<&Exchange> = None;
    for ex in sent {
        let Some(st) = ex.status else { continue };
        let a = answer(ex);
        if is_ok(ex) {
            ok_paths.insert((ex.host.clone(), ex.path.clone()));
            accepted = Some(a.clone());
            if says_who(ex) {
                who = Some(ex);
            }
        } else if signed_out(ex) && (whoami_path(&ex.path) || ok_paths.contains(&(ex.host.clone(), ex.path.clone())) || says_bad_token(ex)) {
            refused = Some(Answer { why: refusal_reason(ex), ..a.clone() });
        }
        if (200..300).contains(&st) || st == 401 || st == 403 {
            seen.insert((ex.method.clone(), ex.host.clone(), shape(&ex.path)), Answer { path: shape(&ex.path), ..a });
        }
    }
    if let Some(ex) = who
        && let Some(v) = json_body(ex)
    {
        // The answer's own word on who the caller is.
        let v = v.get("user").filter(|x| x.is_object()).cloned().unwrap_or(v);
        add_grants(&mut grants, &v, &format!("{} {} answer", ex.method, ex.path));
    }
    let mut seen: Vec<Answer> = seen.into_values().collect();
    seen.sort_by_key(|a| std::cmp::Reverse((a.at, a.exchange)));
    let (allowed, blocked): (Vec<Answer>, Vec<Answer>) = seen.into_iter().partition(|a| a.status.is_some_and(|s| (200..300).contains(&s)));

    // The verdict.
    let live: Vec<&Lifetime> = lives.iter().filter(|l| !l.expired).collect();
    let until = live.iter().filter_map(|l| l.expires).min();
    let expired_at = lives.iter().filter(|l| l.expired).filter_map(|l| l.expires).max();
    let accepted_at = accepted.as_ref().map(|a| a.at);
    let refused_at = refused.as_ref().map(|a| a.at);
    let late_accept = match (expired_at, accepted_at) {
        (Some(e), Some(a)) => a / 1000 > e,
        _ => false,
    };
    let state = if creds.is_empty() {
        State::Unknown
    } else if refused_at.is_some() && refused_at >= accepted_at {
        State::Refused
    } else if live.is_empty() && !late_accept {
        State::Expired
    } else if live.len() < lives.len() || refused.is_some() {
        State::Partly
    } else if until.is_some_and(|t| t - now < SOON_SECS) {
        State::Expiring
    } else if accepted.is_some() {
        State::Working
    } else if until.is_some() {
        State::Valid
    } else {
        State::Unknown
    };

    // A request to replay as this user to settle it: its own "who am I",
    // else its last accepted GET, else anyone's "who am I".
    let get_ok = |ex: &&&Exchange| ex.method == "GET" && is_ok(ex);
    let probe = sent
        .iter()
        .rev()
        .filter(get_ok)
        .find(|ex| says_who(ex))
        .or_else(|| sent.iter().rev().find(get_ok))
        .or_else(|| all.iter().rev().find(|ex| says_who(ex) && !sent_tokens(ex).is_empty()))
        .map(|ex| answer(ex));

    Standing {
        user: u.id.clone(),
        state,
        until,
        expired_at,
        accepted,
        refused,
        late_accept,
        credentials: lives,
        grants,
        allowed_total: allowed.len(),
        blocked_total: blocked.len(),
        allowed: allowed.into_iter().take(MAX_SEEN).collect(),
        blocked: blocked.into_iter().take(MAX_SEEN).collect(),
        probe,
    }
}

fn answer(ex: &Exchange) -> Answer {
    Answer { exchange: ex.id, at: ex.ts, method: ex.method.clone(), host: ex.host.clone(), path: ex.path.clone(), status: ex.status, why: String::new() }
}

fn is_ok(ex: &Exchange) -> bool {
    ex.status.is_some_and(|s| (200..300).contains(&s))
}

fn location(ex: &Exchange) -> Option<&str> {
    ex.resp_headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("location")).map(|(_, v)| v.as_str())
}

/// An answer that says "sign in first": 401, the 419 and 440 some
/// frameworks send for a lapsed session, or a redirect to a sign-in page.
fn signed_out(ex: &Exchange) -> bool {
    match ex.status {
        Some(401 | 419 | 440) => true,
        Some(s) if (300..400).contains(&s) => location(ex).is_some_and(|l| {
            let l = l.to_ascii_lowercase();
            ["login", "signin", "sign-in", "sign_in", "logon", "/auth", "sso", "session/new", "expired"].iter().any(|w| l.contains(w))
        }),
        _ => false,
    }
}

/// The answer itself blames the token or the session.
fn says_bad_token(ex: &Exchange) -> bool {
    let body = String::from_utf8_lossy(&ex.resp_body[..ex.resp_body.len().min(4_096)]).to_ascii_lowercase();
    let header = ex.resp_headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("www-authenticate")).map(|(_, v)| v.to_ascii_lowercase()).unwrap_or_default();
    ["expired", "invalid_token", "invalid token", "session", "unauthenticated", "not authenticated", "sign in", "log in", "login"].iter().any(|w| body.contains(w) || header.contains(w))
}

/// A short reason from the refusal, such as "token expired".
fn refusal_reason(ex: &Exchange) -> String {
    if let Some(v) = json_body(ex) {
        for k in ["message", "error_description", "detail", "error"] {
            if let Some(s) = v.get(k).and_then(|x| x.as_str()).filter(|s| !s.trim().is_empty()) {
                return crate::detect::clean(s.trim(), 80);
            }
        }
    }
    match location(ex) {
        Some(l) if ex.status.is_some_and(|s| (300..400).contains(&s)) => format!("sent to {}", crate::detect::clean(l.split('?').next().unwrap_or(l), 60)),
        _ => String::new(),
    }
}

/// Whether a path is a "who am I" endpoint, whose answer names the caller.
fn whoami_path(path: &str) -> bool {
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    ["me", "user", "profile", "account", "whoami", "userinfo", "session", "self", "current"].contains(&last.as_str())
}

/// A "who am I" answer: an accepted GET to such an endpoint, in JSON.
fn says_who(ex: &Exchange) -> bool {
    ex.method == "GET" && is_ok(ex) && whoami_path(&ex.path) && json_body(ex).is_some_and(|v| v.is_object())
}

/// A path with its ids folded, so `/v1/orders/1041` and `/v1/orders/1042`
/// are one endpoint.
fn shape(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            let digits = seg.bytes().filter(|b| b.is_ascii_digit()).count();
            let idlike = !seg.is_empty() && (seg.bytes().all(|b| b.is_ascii_digit()) || (seg.len() >= 16 && digits >= 4) || (seg.len() == 36 && seg.matches('-').count() == 4));
            if idlike { "{id}" } else { seg }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn json_body(ex: &Exchange) -> Option<serde_json::Value> {
    if ex.resp_body.len() > MAX_BODY {
        return None;
    }
    serde_json::from_slice(&ex.resp_body).ok()
}

/// The latest answer that handed `token` out with an `expires_in`: (when, seconds, body).
fn handed_out(all: &[&Exchange], token: &str) -> Option<(i64, i64, serde_json::Value)> {
    if token.len() < 12 {
        return None;
    }
    all.iter().rev().filter(|ex| is_ok(ex) && ex.resp_body.len() <= MAX_BODY).find_map(|ex| {
        if !String::from_utf8_lossy(&ex.resp_body).contains(token) {
            return None;
        }
        let v = json_body(ex)?;
        let secs = v.get("expires_in").and_then(num).filter(|s| *s > 0)?;
        Some((ex.ts, secs, v))
    })
}

fn num(v: &serde_json::Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// The claims of a JSON Web Token, if `token` is one.
fn jwt_claims(token: &str) -> Option<serde_json::Value> {
    let mut parts = token.split('.');
    let (_, body, _) = (parts.next()?, parts.next()?, parts.next()?);
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(body.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes).ok().filter(|v| v.is_object())
}

/// Reads the roles, scopes, groups and permissions an object names: the
/// usual claim names, and namespaced ones that end the same way.
fn add_grants(out: &mut Vec<Grant>, v: &serde_json::Value, from: &str) {
    fn push(out: &mut Vec<Grant>, from: &str, kind: &'static str, value: &str) {
        let value = crate::detect::clean(value.trim(), 80);
        if value.is_empty() || out.len() >= 60 || out.iter().any(|g| g.kind == kind && g.value == value) {
            return;
        }
        out.push(Grant { kind, value, from: from.to_string() });
    }
    fn take(out: &mut Vec<Grant>, from: &str, kind: &'static str, x: &serde_json::Value, split: bool) {
        match x {
            serde_json::Value::String(s) if split => s.split([' ', ',']).filter(|p| !p.is_empty()).for_each(|p| push(out, from, kind, p)),
            serde_json::Value::String(s) => push(out, from, kind, s),
            serde_json::Value::Array(a) => a.iter().filter_map(|e| e.as_str().or_else(|| e.get("name").and_then(|n| n.as_str()))).for_each(|s| push(out, from, kind, s)),
            _ => {}
        }
    }
    let Some(obj) = v.as_object() else { return };
    for (k, x) in obj {
        let key = k.to_ascii_lowercase();
        let last = key.rsplit(['/', ':', '.']).next().unwrap_or(&key);
        match last {
            "role" | "roles" | "user_role" | "user_roles" | "authorities" => take(out, from, "role", x, false),
            "scope" | "scopes" | "scp" => take(out, from, "scope", x, true),
            "groups" | "group" => take(out, from, "group", x, false),
            "permissions" | "perms" | "permission" | "entitlements" => take(out, from, "permission", x, false),
            "is_admin" | "admin" | "is_staff" | "staff" | "is_superuser" | "superuser" if x.as_bool() == Some(true) => push(out, from, "flag", last.trim_start_matches("is_")),
            _ => {}
        }
    }
    // Realm and client roles nest one level down.
    if let Some(r) = obj.get("realm_access").and_then(|r| r.get("roles")) {
        take(out, from, "role", r, false);
    }
    if let Some(clients) = obj.get("resource_access").and_then(|r| r.as_object()) {
        for (client, r) in clients {
            for role in r.get("roles").and_then(|x| x.as_array()).into_iter().flatten().filter_map(|e| e.as_str()) {
                push(out, from, "role", &format!("{client}: {role}"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::{Cookie, ScopedHeader};

    fn rules() -> ScopeRules {
        let rule = crate::scope::Rule { pattern: "shop.test".into(), include_subdomains: true, decision: crate::scope::Decision::Accepted, created_at: 0, note: String::new() };
        ScopeRules { rules: vec![rule] }
    }

    fn jwt(claims: &str) -> String {
        let e = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!("{}.{}.c2lnbmF0dXJl", e(r#"{"alg":"HS256"}"#), e(claims))
    }

    fn user(id: &str, headers: &[(&str, &str)], cookies: &[(&str, &str, Option<i64>)]) -> SavedUser {
        SavedUser {
            id: id.into(),
            name: id.into(),
            note: String::new(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            scoped: vec![],
            cookies: cookies.iter().map(|(n, v, e)| Cookie { name: n.to_string(), value: v.to_string(), domain: "shop.test".into(), expires: *e }).collect(),
            keep_fresh: true,
        }
    }

    fn ex(id: i64, path: &str, headers: &[(&str, &str)], status: u16, body: &str) -> Exchange {
        Exchange {
            id,
            ts: id * 60_000,
            host: "api.shop.test".into(),
            path: path.into(),
            method: "GET".into(),
            req_headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            status: Some(status),
            resp_body: body.as_bytes().to_vec(),
            source: Some(Source::Proxy),
            ..Default::default()
        }
    }

    const SID: &str = "s1-9f8e7d6c5b4a39281706";
    const NOW: i64 = 10_000;

    #[test]
    fn nothing_to_go_on_is_unknown_not_expired() {
        let u = user("maya", &[], &[("sid", SID, None)]);
        let s = &standings(&[], &rules(), &[u], NOW)[0];
        assert_eq!(s.state, State::Unknown);
        assert!(s.grants.is_empty() && s.probe.is_none());
    }

    #[test]
    fn a_token_past_its_exp_claim_is_expired_and_names_its_roles() {
        let t = jwt(r#"{"sub":"u9","exp":9000,"iat":5400,"roles":["admin","support"],"scope":"orders:read orders:refund","realm_access":{"roles":["ops"]}}"#);
        let u = user("ari", &[("Authorization", &format!("Bearer {t}"))], &[]);
        let s = &standings(&[], &rules(), &[u], NOW)[0];
        assert_eq!(s.state, State::Expired);
        assert_eq!(s.expired_at, Some(9000));
        assert_eq!((s.credentials[0].source, s.credentials[0].issued), ("exp claim", Some(5400)));
        let got: Vec<(&str, &str)> = s.grants.iter().map(|g| (g.kind, g.value.as_str())).collect();
        for want in [("role", "admin"), ("role", "support"), ("scope", "orders:read"), ("scope", "orders:refund"), ("role", "ops")] {
            assert!(got.contains(&want), "{want:?} in {got:?}");
        }
    }

    #[test]
    fn the_servers_answers_decide_working_and_refused() {
        let cookie = format!("sid={SID}");
        let u = user("maya", &[], &[("sid", SID, None)]);
        let me = r#"{"email":"maya@shop.test","role":"customer","permissions":["orders.read"]}"#;
        let traffic = vec![
            ex(1, "/v1/me", &[("Cookie", &cookie)], 200, me),
            ex(2, "/v1/orders/1041", &[("Cookie", &cookie)], 200, "{}"),
            ex(3, "/v1/admin/orders", &[("Cookie", &cookie)], 403, r#"{"error":"forbidden"}"#),
        ];
        let s = &standings(&traffic, &rules(), std::slice::from_ref(&u), NOW)[0];
        assert_eq!(s.state, State::Working);
        assert_eq!(s.accepted.as_ref().unwrap().exchange, 2, "a 403 is a permission, not a lapsed session");
        assert_eq!(s.allowed.iter().map(|a| a.path.as_str()).collect::<Vec<_>>(), ["/v1/orders/{id}", "/v1/me"]);
        assert_eq!(s.blocked[0].path, "/v1/admin/orders");
        assert!(s.grants.iter().any(|g| g.kind == "role" && g.value == "customer" && g.from == "GET /v1/me answer"));
        assert!(s.grants.iter().any(|g| g.kind == "permission" && g.value == "orders.read"));
        assert_eq!(s.probe.as_ref().unwrap().path, "/v1/me");

        // Later, the same endpoint turns the same cookie away.
        let mut later = traffic.clone();
        later.push(ex(4, "/v1/me", &[("Cookie", &cookie)], 401, r#"{"message":"session expired"}"#));
        let s = &standings(&later, &rules(), std::slice::from_ref(&u), NOW)[0];
        assert_eq!(s.state, State::Refused);
        assert_eq!(s.refused.as_ref().unwrap().why, "session expired");

        // A 401 from an endpoint it never reached, with no word on why, is not proof.
        let mut odd = traffic.clone();
        odd.push(ex(5, "/v3/reports", &[("Cookie", &cookie)], 401, ""));
        assert_eq!(standings(&odd, &rules(), std::slice::from_ref(&u), NOW)[0].state, State::Working);
    }

    #[test]
    fn a_value_two_users_share_does_not_mix_their_traffic() {
        let shared = "cart-7d6c5b4a3928170612345";
        let maya = user("maya", &[], &[("sid", SID, None), ("cart_state", shared, None)]);
        let dana = user("dana", &[], &[("sid", "s2-1a2b3c4d5e6f70819203", None), ("cart_state", shared, None)]);
        let traffic = vec![ex(1, "/v1/me", &[("Cookie", &format!("sid={SID}; cart_state={shared}"))], 200, "{}")];
        let s = standings(&traffic, &rules(), &[maya, dana], NOW);
        assert_eq!((s[0].state, s[1].state), (State::Working, State::Unknown));
    }

    #[test]
    fn expiry_from_cookies_and_the_answer_that_handed_a_token_out() {
        let seller = "slr_live_7Hq2Lm9Xc4Vb8Nz1";
        let mut u = user("maya", &[], &[("sid", SID, Some(NOW + 600))]);
        u.scoped.push(ScopedHeader { name: "Authorization".into(), value: format!("Bearer {seller}"), host: "api.shop.test".into(), path: "/v2/seller".into() });
        let handed = ex(1, "/v1/seller/session", &[("Cookie", &format!("sid={SID}"))], 201, &format!(r#"{{"seller_token":"{seller}","expires_in":1800,"scope":"seller"}}"#));
        let s = &standings(&[handed], &rules(), &[u], NOW)[0];
        assert_eq!(s.state, State::Partly, "the seller token ran out, the cookie has ten minutes left: {s:#?}");
        assert_eq!(s.until, Some(NOW + 600));
        let tok = s.credentials.iter().find(|c| c.name == "Authorization").unwrap();
        assert_eq!((tok.source, tok.expires, tok.place.as_str()), ("expires_in", Some(60 + 1800), "api.shop.test/v2/seller"));
        assert!(tok.expired);
        assert!(s.grants.iter().any(|g| g.kind == "scope" && g.value == "seller"));
    }

    #[test]
    fn accepted_after_its_expiry_is_flagged() {
        let t = jwt(r#"{"sub":"u1","exp":30}"#);
        let auth = format!("Bearer {t}");
        let u = user("maya", &[("Authorization", &auth)], &[]);
        let s = &standings(&[ex(2, "/v1/orders", &[("Authorization", &auth)], 200, "{}")], &rules(), &[u], NOW)[0];
        assert!(s.late_accept);
        assert_eq!(s.state, State::Partly);
    }

    #[test]
    fn a_redirect_to_sign_in_is_a_refusal() {
        let cookie = format!("sid={SID}");
        let u = user("maya", &[], &[("sid", SID, None)]);
        let mut back = ex(2, "/account", &[("Cookie", &cookie)], 302, "");
        back.resp_headers.push(("Location".into(), "https://shop.test/login?next=/account".into()));
        let s = &standings(&[ex(1, "/account", &[("Cookie", &cookie)], 200, ""), back], &rules(), &[u], NOW)[0];
        assert_eq!(s.state, State::Refused);
        assert_eq!(s.refused.as_ref().unwrap().why, "sent to https://shop.test/login");
    }
}
