//! Users found in traffic: the sessions captured browser traffic carries, so
//! the person can keep them as saved users without copying a cookie or a
//! token by hand.
//!
//! A large application rarely signs everything with one token. The shop's
//! pages use a session cookie, its API a bearer token, its seller back office
//! another token again. Each of these is one person's session, and is found
//! here as one: tokens sent together in one request, a token handed back in
//! the answer to a request signed with another, and tokens naming the same
//! account all belong to the same session. Each token comes with the part of
//! the application it is sent to (a cookie's domain, a header's host and,
//! where the host takes different tokens under different paths, the path),
//! so a saved user sends the right one to the right place.
//!
//! Nothing here changes anything: the person picks which session goes to
//! which user. Only captured traffic to in-scope hosts is read; requests the
//! person sent themselves (the Bench, Scans) are experiments, not sessions.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use base64::Engine as _;
use serde::Serialize;

use crate::model::{Exchange, Source};
use crate::scope::ScopeRules;
use crate::users::{Cookie, SavedUser, ScopedHeader};

/// Most recent captured exchanges read.
pub const MAX_EXCHANGES: usize = 4_000;
/// Most distinct tokens followed, so a busy capture stays quick to read.
const MAX_TOKENS: usize = 600;
/// Response bodies longer than this are not searched for handed-back tokens.
const MAX_BODY: usize = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Cookie,
    Header,
}

/// One session found in the traffic.
#[derive(Debug, Clone, Serialize)]
pub struct FoundSession {
    /// Stays the same as more traffic arrives, to apply this session later.
    pub key: String,
    /// Who the session is, when the traffic says: an email, a user name or an
    /// account id. Empty when nothing names it.
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub role: String,
    /// The saved user already holding some of these tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    pub credentials: Vec<FoundCredential>,
    /// How many credentials the matched user does not send yet (all of them
    /// with no match).
    pub fresh: usize,
    /// Hosts outside scope the same tokens were sent to.
    pub elsewhere: Vec<String>,
    /// How many captured requests carried the session, and when.
    pub requests: usize,
    pub first_ts: i64,
    pub last_ts: i64,
    /// One request that carried it, to open.
    pub sample: i64,
    #[serde(skip)]
    pub exchanges: BTreeSet<i64>,
}

/// One cookie or header of a found session, and where it is sent.
#[derive(Debug, Clone, Serialize)]
pub struct FoundCredential {
    pub kind: Kind,
    pub name: String,
    /// The value, shortened, to recognise it by.
    pub preview: String,
    #[serde(skip)]
    pub value: String,
    /// A cookie's domain, or the host a header is sent to.
    pub host: String,
    /// For a header: only under this path. Empty means the whole host.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// Not sent by the matched user yet.
    pub new: bool,
    pub requests: usize,
    pub last_ts: i64,
}

impl FoundSession {
    /// Where the session is used, from widest to narrowest, for a summary.
    pub fn hosts(&self) -> Vec<String> {
        let mut hs: Vec<String> = self.credentials.iter().map(|c| c.host.clone()).collect();
        hs.sort();
        hs.dedup();
        hs
    }
}

/// A short form of a secret, enough to tell two apart.
pub fn preview(v: &str) -> String {
    let n = v.chars().count();
    if n <= 14 {
        return v.to_string();
    }
    let head: String = v.chars().take(8).collect();
    let tail: String = v.chars().skip(n - 4).collect();
    format!("{head}…{tail}")
}

/// Tracking and preference cookies that are not anyone's session.
const NOT_SESSIONS: &[&str] = &["_ga", "_gid", "_gat", "_gcl", "_fbp", "_fbc", "__utm", "_hj", "ajs_", "mp_", "amplitude", "optimizely", "_uet", "__cf_bm", "_clck", "_clsk", "intercom-", "consent", "cookieconsent", "theme", "lang", "locale"];

/// Whether a cookie reads like part of a session: a session-ish name, or a
/// long, random-looking value.
fn session_cookie(name: &str, value: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if NOT_SESSIONS.iter().any(|p| n.starts_with(p)) || value.len() < 8 {
        return false;
    }
    let named = ["sess", "sid", "auth", "token", "jwt", "sso", "login", "remember", "user", "account", "csrf", "xsrf", "identity"].iter().any(|w| n.contains(w));
    let random = value.len() >= 20 && value.bytes().any(|b| b.is_ascii_digit()) && value.bytes().any(|b| b.is_ascii_alphabetic()) && !value.contains(' ');
    named || random
}

/// A header carrying a credential (see [`crate::users::is_auth_header`]),
/// cookies aside.
fn session_header(name: &str, value: &str) -> bool {
    !name.eq_ignore_ascii_case("cookie") && crate::users::is_auth_header(name) && value.trim().len() >= 8
}

/// The token itself, without an auth scheme such as `Bearer `.
fn bare(value: &str) -> &str {
    match value.split_once(' ') {
        Some((scheme, rest)) if !scheme.is_empty() && scheme.bytes().all(|b| b.is_ascii_alphabetic()) => rest.trim(),
        _ => value.trim(),
    }
}

/// Who a JSON Web Token names: (account, label, role). An unsigned token
/// (`alg: none`) names nobody: it is something someone made up.
fn jwt_claims(token: &str) -> Option<(String, String, String)> {
    let mut parts = token.split('.');
    let (head, body, _) = (parts.next()?, parts.next()?, parts.next()?);
    let dec = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    let head = dec(head)?;
    if head.get("alg").and_then(|a| a.as_str()).is_some_and(|a| a.eq_ignore_ascii_case("none")) {
        return None;
    }
    let claims = dec(body)?;
    let s = |k: &str| claims.get(k).and_then(|v| v.as_str().map(String::from).or_else(|| v.as_i64().map(|n| n.to_string())));
    let account = s("sub").or_else(|| s("user_id")).or_else(|| s("uid")).or_else(|| s("email"))?;
    let label = s("email").or_else(|| s("preferred_username")).or_else(|| s("name")).unwrap_or_else(|| account.clone());
    let role = s("role").or_else(|| claims.get("roles").and_then(|r| r.get(0)).and_then(|r| r.as_str()).map(String::from)).unwrap_or_default();
    Some((account, label, role))
}

/// Who a "who am I" answer says the caller is: (label, role).
fn whoami(ex: &Exchange) -> Option<(String, String)> {
    let last = ex.path.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_ascii_lowercase();
    if !["me", "user", "profile", "account", "whoami", "userinfo", "session", "self"].contains(&last.as_str()) || !ex.status.is_some_and(|s| (200..300).contains(&s)) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&ex.resp_body).ok()?;
    let v = v.get("user").filter(|u| u.is_object()).unwrap_or(&v);
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(String::from);
    let label = s("email").or_else(|| s("username")).or_else(|| s("login")).or_else(|| s("name"))?;
    Some((label, s("role").unwrap_or_default()))
}

#[derive(Default)]
struct Tok {
    kind: Option<Kind>,
    name: String,
    value: String,
    /// In-scope host → paths it was sent under.
    sent: BTreeMap<String, BTreeSet<String>>,
    /// The domain a server set it for, when one did.
    set_domain: Option<String>,
    requests: usize,
    last_ts: i64,
    first_ts: i64,
    exchanges: BTreeSet<i64>,
    /// Sent with these other tokens in one request.
    with: BTreeSet<usize>,
    elsewhere: BTreeSet<String>,
}

struct Uf(Vec<usize>);

impl Uf {
    fn find(&mut self, x: usize) -> usize {
        let mut r = x;
        while self.0[r] != r {
            r = self.0[r];
        }
        let mut x = x;
        while self.0[x] != r {
            let next = self.0[x];
            self.0[x] = r;
            x = next;
        }
        r
    }
    fn join(&mut self, a: usize, b: usize) {
        let (a, b) = (self.find(a), self.find(b));
        if a != b {
            self.0[a.max(b)] = a.min(b);
        }
    }
}

/// The tokens a request carries: (kind, name, value).
fn sent_tokens(ex: &Exchange) -> Vec<(Kind, String, String)> {
    let mut out = vec![];
    for (k, v) in &ex.req_headers {
        if k.eq_ignore_ascii_case("cookie") {
            for pair in v.split(';') {
                if let Some((n, val)) = pair.split_once('=') {
                    let (n, val) = (n.trim(), val.trim());
                    if !n.is_empty() && session_cookie(n, val) {
                        out.push((Kind::Cookie, n.to_string(), val.to_string()));
                    }
                }
            }
        } else if session_header(k, v) {
            out.push((Kind::Header, k.clone(), v.trim().to_string()));
        }
    }
    out
}

/// Cookies a response sets: (name, value, domain).
fn set_cookies(ex: &Exchange) -> Vec<(String, String, Option<String>)> {
    let mut out = vec![];
    for (k, v) in &ex.resp_headers {
        if !k.eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        let mut parts = v.split(';');
        let Some((n, val)) = parts.next().and_then(|p| p.split_once('=')) else { continue };
        let (n, val) = (n.trim(), val.trim());
        if n.is_empty() || !session_cookie(n, val) {
            continue;
        }
        let domain = parts.filter_map(|a| a.split_once('=')).find(|(a, _)| a.trim().eq_ignore_ascii_case("domain")).map(|(_, d)| d.trim().trim_start_matches('.').to_ascii_lowercase());
        out.push((n.to_string(), val.to_string(), domain));
    }
    out
}

fn captured(ex: &Exchange) -> bool {
    matches!(ex.source, None | Some(Source::Proxy) | Some(Source::Import))
}

/// The longest shared ending of host names, by whole labels, kept to at least
/// two labels: `www.shop.test` and `api.shop.test` share `shop.test`.
fn common_domain<'a>(hosts: impl Iterator<Item = &'a String>) -> Option<String> {
    let mut common: Option<Vec<&str>> = None;
    for h in hosts {
        let labels: Vec<&str> = h.rsplit('.').collect();
        common = Some(match common {
            None => labels,
            Some(c) => c.iter().zip(labels.iter()).take_while(|(a, b)| a == b).map(|(a, _)| *a).collect(),
        });
    }
    let c = common?;
    (c.len() >= 2).then(|| c.iter().rev().copied().collect::<Vec<_>>().join("."))
}

/// The longest path prefix, by whole segments, all of `paths` share.
fn common_path<'a>(paths: impl Iterator<Item = &'a String>) -> String {
    let mut common: Option<Vec<&str>> = None;
    for p in paths {
        let segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        common = Some(match common {
            None => segs,
            Some(c) => c.iter().zip(segs.iter()).take_while(|(a, b)| a == b).map(|(a, _)| *a).collect(),
        });
    }
    common.map(|c| c.iter().map(|s| format!("/{s}")).collect()).unwrap_or_default()
}

/// Finds the sessions in `exchanges` (any order), matched against `users`.
/// Newest session first.
pub fn sessions(exchanges: &[Exchange], rules: &ScopeRules, users: &[SavedUser], now: i64) -> Vec<FoundSession> {
    let mut toks: Vec<Tok> = vec![];
    let mut index: HashMap<(Kind, String, String), usize> = HashMap::new();
    let mut edges: Vec<(usize, usize)> = vec![];
    let mut whoamis: Vec<(Vec<usize>, String, String)> = vec![];
    let mut ordered: Vec<&Exchange> = exchanges.iter().filter(|ex| captured(ex)).collect();
    ordered.sort_by_key(|ex| ex.id);
    let key = |kind: Kind, name: &str, value: &str| (kind, if kind == Kind::Header { name.to_ascii_lowercase() } else { name.to_string() }, value.to_string());

    // Pass one: the tokens in-scope requests carry and the cookies set there.
    let mut carried: Vec<(&Exchange, Vec<usize>)> = vec![];
    for ex in &ordered {
        if !rules.in_scope(&ex.host) {
            continue;
        }
        let mut here = vec![];
        let sent = sent_tokens(ex);
        let set = set_cookies(ex);
        for (kind, name, value, domain) in sent.into_iter().map(|(k, n, v)| (k, n, v, None)).chain(set.into_iter().map(|(n, v, d)| (Kind::Cookie, n, v, Some(d)))) {
            let k = key(kind, &name, &value);
            let id = match index.get(&k) {
                Some(&id) => id,
                None if toks.len() < MAX_TOKENS => {
                    toks.push(Tok { kind: Some(kind), name: name.clone(), value: value.clone(), first_ts: ex.ts, ..Default::default() });
                    index.insert(k, toks.len() - 1);
                    toks.len() - 1
                }
                None => continue,
            };
            let t = &mut toks[id];
            match domain {
                // Set by the server for a whole domain: where it will be sent.
                Some(d) => {
                    if d.is_some() {
                        t.set_domain = d;
                    }
                }
                None => {
                    let paths = t.sent.entry(ex.host.to_ascii_lowercase()).or_default();
                    if paths.len() < 200 {
                        paths.insert(ex.path.clone());
                    }
                    t.requests += 1;
                }
            }
            t.last_ts = t.last_ts.max(ex.ts);
            t.exchanges.insert(ex.id);
            here.push(id);
        }
        here.sort();
        here.dedup();
        for (n, a) in here.iter().enumerate() {
            for b in &here[n + 1..] {
                toks[*a].with.insert(*b);
                toks[*b].with.insert(*a);
            }
        }
        if let Some((label, role)) = whoami(ex)
            && !here.is_empty()
        {
            whoamis.push((here.clone(), label, role));
        }
        carried.push((ex, here));
    }

    // A token handed back in the answer to a request signed with others
    // (a login, a token exchange) is the same person's.
    let searchable: Vec<(usize, &str)> = toks.iter().enumerate().map(|(i, t)| (i, bare(&t.value))).filter(|(_, v)| v.len() >= 16).collect();
    if !searchable.is_empty() {
        let pattern = searchable.iter().map(|(_, v)| regex::escape(v)).collect::<Vec<_>>().join("|");
        if let Ok(re) = regex::bytes::RegexBuilder::new(&pattern).size_limit(32 << 20).build() {
            let by_value: HashMap<&str, Vec<usize>> = searchable.iter().fold(HashMap::new(), |mut m, (i, v)| {
                m.entry(*v).or_default().push(*i);
                m
            });
            for (ex, here) in &carried {
                if here.is_empty() || ex.resp_body.len() > MAX_BODY {
                    continue;
                }
                for m in re.find_iter(&ex.resp_body) {
                    let Ok(v) = std::str::from_utf8(m.as_bytes()) else { continue };
                    for &t in by_value.get(v).into_iter().flatten() {
                        if !here.contains(&t) {
                            edges.extend(here.iter().map(|&h| (h, t)));
                        }
                    }
                }
            }
        }
    }

    // Tokens naming the same account belong together.
    let mut accounts: HashMap<String, usize> = HashMap::new();
    let mut named: HashMap<usize, (String, String)> = HashMap::new();
    for (i, t) in toks.iter().enumerate() {
        if let Some((account, label, role)) = jwt_claims(bare(&t.value)) {
            named.insert(i, (label, role));
            match accounts.get(&account) {
                Some(&j) => edges.push((i, j)),
                None => {
                    accounts.insert(account, i);
                }
            }
        }
    }

    // A token sent alongside two values of another token for the same place
    // (an app-wide key sent with two people's session cookies) could join two
    // people into one. Two values for different places (a shopper token for
    // the API, a seller token for its back office) are one person's. Leave
    // the doubtful tokens out of the joining; take them back in only when
    // what they were sent with turned out to be one session anyway.
    let mut uf = Uf((0..toks.len()).collect());
    let same_place = |a: &Tok, b: &Tok| a.sent.iter().any(|(host, paths)| b.sent.get(host).is_some_and(|other| !paths.is_disjoint(other)));
    let clashes = |t: &Tok| -> bool {
        let with: Vec<&Tok> = t.with.iter().map(|&o| &toks[o]).collect();
        with.iter().enumerate().any(|(n, a)| with[n + 1..].iter().any(|b| a.kind == b.kind && a.name.eq_ignore_ascii_case(&b.name) && a.value != b.value && same_place(a, b)))
    };
    let doubtful: Vec<bool> = toks.iter().map(clashes).collect();
    for (a, b) in &edges {
        if !doubtful[*a] && !doubtful[*b] {
            uf.join(*a, *b);
        }
    }
    for (i, t) in toks.iter().enumerate() {
        if doubtful[i] {
            continue;
        }
        for &o in &t.with {
            if !doubtful[o] {
                uf.join(i, o);
            }
        }
    }
    let mut shared = vec![false; toks.len()];
    for (i, t) in toks.iter().enumerate() {
        if !doubtful[i] {
            continue;
        }
        let roots: BTreeSet<usize> = t.with.iter().filter(|&&o| !doubtful[o]).map(|&o| uf.find(o)).collect();
        match roots.len() {
            0 => {}
            1 => uf.join(i, *roots.iter().next().unwrap()),
            _ => shared[i] = true,
        }
    }

    // Hosts outside scope the same tokens went to.
    for ex in &ordered {
        if rules.in_scope(&ex.host) {
            continue;
        }
        for (kind, name, value) in sent_tokens(ex) {
            if let Some(&id) = index.get(&key(kind, &name, &value)) {
                toks[id].elsewhere.insert(ex.host.to_ascii_lowercase());
            }
        }
    }

    // Group into sessions.
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (i, gone) in shared.iter().enumerate() {
        if !gone {
            groups.entry(uf.find(i)).or_default().push(i);
        }
    }
    let mut out = vec![];
    for (_, members) in groups {
        let sent_any = members.iter().any(|&i| toks[i].requests > 0);
        if !sent_any {
            continue; // set by a server, never sent back: not a session in use
        }
        let mut creds = credentials(&toks, &members);
        if creds.is_empty() {
            continue;
        }
        // Who it is.
        let (mut label, mut role) = (String::new(), String::new());
        for &i in &members {
            if let Some((l, r)) = named.get(&i) {
                (label, role) = (l.clone(), r.clone());
                break;
            }
        }
        if let Some((_, l, r)) = whoamis.iter().rev().find(|(ids, _, _)| ids.iter().any(|i| members.contains(i))) {
            label = l.clone();
            if !r.is_empty() {
                role = r.clone();
            }
        }
        let user = match_user(users, &members.iter().map(|&i| toks[i].value.as_str()).collect::<Vec<_>>());
        let holder = user.and_then(|id| users.iter().find(|u| u.id == id));
        for c in &mut creds {
            c.new = !holder.is_some_and(|u| sends(u, c, now));
        }
        let exchanges: BTreeSet<i64> = members.iter().flat_map(|&i| toks[i].exchanges.iter().copied()).collect();
        let first = members.iter().copied().min_by_key(|&i| (toks[i].first_ts, i)).unwrap();
        let ft = &toks[first];
        out.push(FoundSession {
            key: crate::scope::token_hash(&format!("{:?}\u{1f}{}\u{1f}{}", ft.kind, ft.name, ft.value))[..16].to_string(),
            label,
            role,
            user: user.map(String::from),
            fresh: creds.iter().filter(|c| c.new).count(),
            credentials: creds,
            elsewhere: members.iter().flat_map(|&i| toks[i].elsewhere.iter().cloned()).collect::<BTreeSet<_>>().into_iter().collect(),
            requests: exchanges.len(),
            first_ts: members.iter().map(|&i| toks[i].first_ts).min().unwrap_or(0),
            last_ts: members.iter().map(|&i| toks[i].last_ts).max().unwrap_or(0),
            sample: exchanges.iter().next_back().copied().unwrap_or(0),
            exchanges,
        });
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.last_ts));
    out
}

/// The credentials of one session: each cookie once, with its domain; each
/// header per host it is sent to, and per path where a host takes different
/// values of it under different paths.
fn credentials(toks: &[Tok], members: &[usize]) -> Vec<FoundCredential> {
    let mut out = vec![];
    // Cookies: the latest value of each name.
    let mut cookies: BTreeMap<String, &Tok> = BTreeMap::new();
    for &i in members {
        let t = &toks[i];
        if t.kind == Some(Kind::Cookie) && (t.requests > 0 || t.set_domain.is_some()) {
            let slot = cookies.entry(t.name.clone()).or_insert(t);
            if t.last_ts > slot.last_ts {
                *slot = t;
            }
        }
    }
    for t in cookies.values() {
        let domain = t.set_domain.clone().or_else(|| if t.sent.len() > 1 { common_domain(t.sent.keys()) } else { None }).or_else(|| t.sent.keys().next().cloned()).unwrap_or_default();
        out.push(FoundCredential { kind: Kind::Cookie, name: t.name.clone(), preview: preview(&t.value), value: t.value.clone(), host: domain, path: String::new(), new: true, requests: t.requests, last_ts: t.last_ts });
    }
    // Headers: per name and host, the values and the paths each went to.
    let mut by_place: BTreeMap<(String, String), Vec<&Tok>> = BTreeMap::new();
    for &i in members {
        let t = &toks[i];
        if t.kind != Some(Kind::Header) {
            continue;
        }
        for host in t.sent.keys() {
            by_place.entry((t.name.to_ascii_lowercase(), host.clone())).or_default().push(t);
        }
    }
    for ((_, host), vals) in by_place {
        // Each value's reach on this host; the same reach twice is the same
        // token renewed, and the newest one is kept.
        let mut reach: BTreeMap<String, &Tok> = BTreeMap::new();
        for t in vals {
            let prefix = common_path(t.sent[&host].iter());
            let slot = reach.entry(prefix).or_insert(t);
            if t.last_ts > slot.last_ts {
                *slot = t;
            }
        }
        let widest = reach.keys().min_by_key(|p| p.len()).cloned().unwrap_or_default();
        for (prefix, t) in reach {
            let path = if prefix == widest { String::new() } else { prefix };
            let requests = t.sent[&host].len().min(t.requests);
            out.push(FoundCredential { kind: Kind::Header, name: t.name.clone(), preview: preview(&t.value), value: t.value.clone(), host: host.clone(), path, new: true, requests, last_ts: t.last_ts });
        }
    }
    out
}

/// The saved user holding most of these token values.
fn match_user<'a>(users: &'a [SavedUser], values: &[&str]) -> Option<&'a str> {
    users
        .iter()
        .map(|u| {
            let held = values.iter().filter(|v| u.cookies.iter().any(|c| c.value == **v) || u.headers.iter().any(|(_, hv)| hv == *v) || u.scoped.iter().any(|s| s.value == **v)).count();
            (held, u)
        })
        .filter(|(n, _)| *n > 0)
        .max_by_key(|(n, _)| *n)
        .map(|(_, u)| u.id.as_str())
}

/// Whether `user` already sends this credential where it belongs.
fn sends(user: &SavedUser, c: &FoundCredential, now: i64) -> bool {
    match c.kind {
        Kind::Cookie => user.cookies.iter().any(|x| x.name == c.name && x.value == c.value && x.live(now)),
        Kind::Header => {
            let path = if c.path.is_empty() { "/" } else { &c.path };
            user.headers_for(&c.host, path).iter().any(|(k, v)| k.eq_ignore_ascii_case(&c.name) && *v == c.value)
        }
    }
}

/// Puts a found session's credentials into `user`: each cookie replaces the
/// one of the same name, and each header is kept for its host and path.
/// Returns how many changed.
pub fn apply(user: &mut SavedUser, found: &FoundSession, now: i64) -> usize {
    let mut changed = 0;
    for c in &found.credentials {
        if sends(user, c, now) {
            continue;
        }
        match c.kind {
            // What a saved user cannot hold is left out rather than failing the save.
            Kind::Cookie if user.cookies.len() >= crate::users::MAX_COOKIES && !user.cookies.iter().any(|x| x.name == c.name) => continue,
            Kind::Header if user.scoped.len() >= crate::users::MAX_HEADERS && !user.scoped.iter().any(|s| s.name.eq_ignore_ascii_case(&c.name) && s.host == c.host && s.path == c.path) => continue,
            Kind::Cookie => {
                match user.cookies.iter_mut().find(|x| x.name == c.name && (x.domain == c.host || x.domain.is_empty() || !x.live(now))) {
                    Some(x) => *x = Cookie { name: c.name.clone(), value: c.value.clone(), domain: c.host.clone(), expires: None },
                    None => user.cookies.push(Cookie { name: c.name.clone(), value: c.value.clone(), domain: c.host.clone(), expires: None }),
                }
            }
            Kind::Header => {
                user.scoped.retain(|s| !(s.name.eq_ignore_ascii_case(&c.name) && s.host == c.host && s.path == c.path));
                user.scoped.push(ScopedHeader { name: c.name.clone(), value: c.value.clone(), host: c.host.clone(), path: c.path.clone() });
            }
        }
        changed += 1;
    }
    changed
}

/// A name for a new user made from a found session.
pub fn user_name(found: &FoundSession, taken: &[SavedUser]) -> String {
    let base = if found.label.is_empty() {
        format!("User {}", taken.len() + 1)
    } else {
        // "sam.ortiz@shop.test" reads as "Sam Ortiz".
        let local = found.label.split('@').next().unwrap_or(&found.label);
        let words: Vec<String> = local
            .split(['.', '_', '-', ' '])
            .filter(|w| !w.is_empty())
            .map(|w| {
                let mut c = w.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            })
            .collect();
        if words.is_empty() { found.label.clone() } else { words.join(" ") }
    };
    let name = if found.role.is_empty() { base } else { format!("{base} ({})", found.role) };
    name.chars().take(60).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{Decision, Rule};

    fn rules(hosts: &[&str]) -> ScopeRules {
        let rules: Vec<Rule> = hosts.iter().map(|h| Rule { pattern: h.to_string(), include_subdomains: false, decision: Decision::Accepted, created_at: 0, note: String::new() }).collect();
        ScopeRules { rules }
    }

    fn ex(id: i64, host: &str, path: &str, headers: &[(&str, &str)]) -> Exchange {
        Exchange {
            id,
            ts: id * 1000,
            host: host.into(),
            path: path.into(),
            method: "GET".into(),
            req_headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            status: Some(200),
            source: Some(Source::Proxy),
            ..Default::default()
        }
    }

    fn jwt(claims: &str) -> String {
        let e = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!("{}.{}.c2lnbmF0dXJl", e(r#"{"alg":"HS256"}"#), e(claims))
    }

    const S1: &str = "s1-9f8e7d6c5b4a39281706";
    const S2: &str = "s2-1a2b3c4d5e6f70819203";

    #[test]
    fn one_persons_tokens_across_hosts_and_paths_are_one_session() {
        let cust = format!("Bearer {}", jwt(r#"{"sub":"u1","email":"maya@shop.test","role":"customer"}"#));
        let seller = "Bearer slr_7Hq2Lm9Xc4Vb8Nz1".to_string();
        let mut handed = ex(4, "api.shop.test", "/v1/seller/session", &[("Authorization", &cust), ("Cookie", &format!("sid={S1}"))]);
        handed.resp_body = br#"{"seller_token":"slr_7Hq2Lm9Xc4Vb8Nz1"}"#.to_vec();
        let mut me = ex(2, "api.shop.test", "/v1/me", &[("Authorization", &cust)]);
        me.resp_body = br#"{"email":"maya@shop.test","role":"customer"}"#.to_vec();
        let traffic = vec![
            ex(1, "www.shop.test", "/account", &[("Cookie", &format!("sid={S1}; theme=dark; consent=yes"))]),
            me,
            ex(3, "api.shop.test", "/v1/orders", &[("Authorization", &cust), ("Cookie", &format!("sid={S1}"))]),
            handed,
            ex(5, "api.shop.test", "/v2/seller/payouts", &[("Authorization", &seller)]),
            ex(6, "api.shop.test", "/v2/seller/listings", &[("Authorization", &seller)]),
            ex(7, "uploads.files.test", "/v2/avatars", &[("Authorization", &cust)]),
            // Someone else entirely.
            ex(8, "www.shop.test", "/", &[("Cookie", &format!("sid={S2}"))]),
            // An experiment from the Bench is not a session.
            Exchange { source: Some(Source::Replay), ..ex(9, "api.shop.test", "/v1/admin", &[("Authorization", "Bearer forged-token-value-123")]) },
        ];
        let found = sessions(&traffic, &rules(&["www.shop.test", "api.shop.test"]), &[], 0);
        assert_eq!(found.len(), 2, "{found:#?}");
        let maya = found.iter().find(|s| s.label == "maya@shop.test").unwrap();
        assert_eq!(maya.role, "customer");
        let cred = |name: &str, path: &str| maya.credentials.iter().find(|c| c.name.eq_ignore_ascii_case(name) && c.path == path).map(|c| (c.host.clone(), c.value.clone()));
        assert_eq!(cred("sid", ""), Some(("shop.test".into(), S1.into())));
        assert_eq!(cred("Authorization", ""), Some(("api.shop.test".into(), cust.clone())));
        assert_eq!(cred("Authorization", "/v2/seller"), Some(("api.shop.test".into(), seller.clone())));
        assert!(cred("theme", "").is_none() && cred("consent", "").is_none(), "preferences are not credentials");
        assert_eq!(maya.credentials.len(), 3);
        assert_eq!(maya.elsewhere, vec!["uploads.files.test".to_string()]);
        assert!(maya.fresh == 3 && maya.user.is_none());
        assert!(!found.iter().any(|s| s.credentials.iter().any(|c| c.value.contains("forged"))));

        // Kept as a user, it sends each token to its own part of the app.
        let mut user = SavedUser { id: "maya".into(), name: "Maya".into(), note: String::new(), headers: vec![], scoped: vec![], cookies: vec![Cookie { name: "sid".into(), value: S1.into(), domain: String::new(), expires: None }], keep_fresh: true };
        let again = sessions(&traffic, &rules(&["www.shop.test", "api.shop.test"]), std::slice::from_ref(&user), 0);
        let mine = again.iter().find(|s| s.user.as_deref() == Some("maya")).expect("matched by the session cookie");
        assert_eq!(mine.fresh, 2, "the cookie is already there");
        assert_eq!(apply(&mut user, mine, 0), 2);
        let auth = |path: &str| user.request_headers("api.shop.test", path, 0).into_iter().find(|(k, _)| k == "Authorization").map(|(_, v)| v);
        assert_eq!(auth("/v1/orders"), Some(cust));
        assert_eq!(auth("/v2/seller/payouts"), Some(seller));
        assert!(user.request_headers("www.shop.test", "/", 0).iter().all(|(k, _)| k != "Authorization"));
        let after = sessions(&traffic, &rules(&["www.shop.test", "api.shop.test"]), std::slice::from_ref(&user), 0);
        assert_eq!(after.iter().find(|s| s.user.is_some()).unwrap().fresh, 0);
    }

    #[test]
    fn an_app_wide_key_does_not_join_two_people() {
        let traffic = vec![
            ex(1, "api.shop.test", "/a", &[("X-Api-Key", "app-key-shared-by-all-1"), ("Cookie", &format!("sid={S1}"))]),
            ex(2, "api.shop.test", "/a", &[("X-Api-Key", "app-key-shared-by-all-1"), ("Cookie", &format!("sid={S2}"))]),
        ];
        let found = sessions(&traffic, &rules(&["api.shop.test"]), &[], 0);
        assert_eq!(found.len(), 2);
        assert!(found.iter().all(|s| s.credentials.len() == 1 && s.credentials[0].name == "sid"));
    }

    #[test]
    fn names_read_from_who_it_is() {
        let f = |label: &str, role: &str| FoundSession { key: String::new(), label: label.into(), role: role.into(), user: None, credentials: vec![], fresh: 0, elsewhere: vec![], requests: 0, first_ts: 0, last_ts: 0, sample: 0, exchanges: Default::default() };
        assert_eq!(user_name(&f("sam.ortiz@shop.test", "support"), &[]), "Sam Ortiz (support)");
        assert_eq!(user_name(&f("", ""), &[]), "User 1");
        assert_eq!(preview("Bearer eyJhbGciOiJIUzI1NiJ9.x.y"), "Bearer e….x.y");
    }
}
