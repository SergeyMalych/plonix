//! Edits an AI agent suggests for a request on the Bench.
//!
//! When the user asks Claude about a draft on the Bench, Claude can answer
//! with a concrete edited request through the `propose_bench_edit` MCP tool.
//! The engine only keeps that suggestion here, in memory: nothing is sent and
//! the draft is not touched. The Bench shows it as a [`diff`] against the
//! draft as it is now, and only the user's Apply changes the draft; sending
//! it stays the user's own click on Send.
//!
//! JWTs follow the Lens rule: an edited token keeps its original signature
//! and is marked as not re-signed. Claude has no signing key, so a signature
//! it makes up is put back to the original; the user can re-sign the token in
//! the Lens after applying.

use std::collections::VecDeque;
use std::sync::{LazyLock, Mutex};

use base64::Engine as _;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::now_ms;

/// Suggestions kept in all, oldest dropped first.
pub const MAX_KEPT: usize = 40;
/// Suggestions kept for one draft; a new one drops that draft's oldest.
pub const MAX_PER_DRAFT: usize = 5;
const MAX_BODY: usize = 512 * 1024;
const MAX_URL: usize = 16 * 1024;
const MAX_HEADERS: usize = 100;
const MAX_HEADER_VALUE: usize = 16 * 1024;
const MAX_SUMMARY: usize = 4000;

/// A request as the Bench holds it: method, URL, headers and a text body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRequest {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default)]
    pub body: String,
}

/// What an agent sends to suggest an edit.
#[derive(Debug, Clone, Deserialize)]
pub struct NewProposal {
    /// The Bench draft it is for, from the "Ask Claude" prompt.
    pub draft: String,
    /// What was changed and why, in a sentence or two.
    #[serde(default)]
    pub summary: String,
    #[serde(flatten)]
    pub request: DraftRequest,
}

/// A suggested edit waiting for the user on the Bench.
#[derive(Debug, Clone, Serialize)]
pub struct Proposal {
    pub id: u64,
    pub draft: String,
    pub summary: String,
    /// Which agent suggested it.
    pub from: String,
    pub created: i64,
    pub request: DraftRequest,
}

/// Pending suggestions, in memory only: they never outlive the engine and
/// nothing in here can send or change anything.
#[derive(Default)]
pub struct Proposals {
    inner: Mutex<(u64, VecDeque<Proposal>)>,
}

impl Proposals {
    /// Checks and keeps a suggestion. It is only stored; the user decides.
    pub fn add(&self, new: NewProposal, from: &str) -> Result<Proposal, String> {
        let draft = new.draft.trim().to_string();
        if draft.is_empty() || draft.len() > 64 || !draft.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("`draft_id` must be the draft id given in the question (letters, digits, - and _)".into());
        }
        let request = check_request(new.request)?;
        let summary: String = new.summary.trim().chars().take(MAX_SUMMARY).collect();
        let from: String = from.trim().chars().take(32).collect();
        let mut g = self.inner.lock().unwrap();
        g.0 += 1;
        let p = Proposal { id: g.0, draft, summary, from: if from.is_empty() { "agent".into() } else { from }, created: now_ms(), request };
        if g.1.iter().filter(|q| q.draft == p.draft).count() >= MAX_PER_DRAFT
            && let Some(i) = g.1.iter().position(|q| q.draft == p.draft)
        {
            g.1.remove(i);
        }
        g.1.push_back(p.clone());
        while g.1.len() > MAX_KEPT {
            g.1.pop_front();
        }
        Ok(p)
    }

    /// Pending suggestions, newest first; only those for `draft` when given.
    pub fn list(&self, draft: Option<&str>) -> Vec<Proposal> {
        let g = self.inner.lock().unwrap();
        g.1.iter().rev().filter(|p| draft.is_none_or(|d| p.draft == d)).cloned().collect()
    }

    pub fn get(&self, id: u64) -> Option<Proposal> {
        self.inner.lock().unwrap().1.iter().find(|p| p.id == id).cloned()
    }

    /// Drops a suggestion once the user applied or discarded it.
    pub fn remove(&self, id: u64) -> Option<Proposal> {
        let mut g = self.inner.lock().unwrap();
        let i = g.1.iter().position(|p| p.id == id)?;
        g.1.remove(i)
    }
}

fn is_token(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// A suggested request must be one the Bench could hold: an http(s) URL,
/// a method and header names that are HTTP tokens, no line breaks in
/// values, and sizes kept sane.
fn check_request(mut r: DraftRequest) -> Result<DraftRequest, String> {
    r.method = r.method.trim().to_ascii_uppercase();
    if r.method.len() > 20 || !is_token(&r.method) {
        return Err("`method` must be an HTTP method such as GET or POST".into());
    }
    r.url = r.url.trim().to_string();
    let lower = r.url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) || r.url.len() > MAX_URL || r.url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("`url` must be a full http:// or https:// URL without spaces".into());
    }
    if r.headers.len() > MAX_HEADERS {
        return Err(format!("at most {MAX_HEADERS} headers"));
    }
    for (k, v) in &mut r.headers {
        *k = k.trim().to_string();
        *v = v.trim().to_string();
        if k.len() > 256 || !is_token(k) {
            return Err(format!("`{k}` is not a valid header name"));
        }
        if v.len() > MAX_HEADER_VALUE || v.contains(['\r', '\n', '\0']) {
            return Err(format!("the value of `{k}` must be one line"));
        }
    }
    if r.body.len() > MAX_BODY {
        return Err(format!("the body is larger than {} KB", MAX_BODY / 1024));
    }
    Ok(r)
}

// ---- the diff the Bench shows ----------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Same,
    Add,
    Del,
    Changed,
}

/// One line of a line-level diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Line {
    pub op: Op,
    pub text: String,
}

/// A named value (header, query or form field) that was added, removed or changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Field {
    pub name: String,
    pub op: Op,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub old: String,
    pub new: String,
}

/// How the body is compared: as text lines, pretty JSON, or form fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyView {
    None,
    Text,
    Json,
    Form,
}

#[derive(Debug, Clone, Serialize)]
pub struct BodyDiff {
    pub view: BodyView,
    pub changed: bool,
    /// Line diff (text and JSON views).
    pub lines: Vec<Line>,
    /// Field diff, URL-decoded (form view).
    pub fields: Vec<Field>,
}

/// A JWT the suggestion changes, compared in decoded form.
#[derive(Debug, Clone, Serialize)]
pub struct TokenChange {
    pub location: String,
    /// The decoded header and claims, line by line.
    pub lines: Vec<Line>,
    pub notes: Vec<String>,
    /// False when the token's content changed but it carries the original
    /// signature (or none), so it is not validly signed.
    pub signed: bool,
}

/// What the suggestion changes, against the draft as it is now.
#[derive(Debug, Clone, Serialize)]
pub struct Diff {
    pub same: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<Change>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<Change>,
    /// Query parameters, decoded, when the URL changed.
    pub query: Vec<Field>,
    pub headers: Vec<Field>,
    pub body: BodyDiff,
    pub tokens: Vec<TokenChange>,
    /// What Apply writes into the draft: the suggestion, with JWT
    /// signatures kept as the Lens rule says.
    pub proposed: DraftRequest,
}

/// Compares a suggestion with the current draft.
pub fn diff(current: &DraftRequest, proposed: &DraftRequest) -> Diff {
    let (proposed, tokens) = keep_signatures(current, proposed);
    let method = (!current.method.eq_ignore_ascii_case(&proposed.method)).then(|| Change { old: current.method.clone(), new: proposed.method.clone() });
    let url = (current.url != proposed.url).then(|| Change { old: current.url.clone(), new: proposed.url.clone() });
    let query = if url.is_some() { fields(&query_pairs(&current.url), &query_pairs(&proposed.url)) } else { vec![] };
    let headers = fields(&current.headers, &proposed.headers);
    let body = body_diff(current, &proposed);
    let same = method.is_none() && url.is_none() && headers.is_empty() && !body.changed;
    Diff { same, method, url, query, headers, body, tokens, proposed }
}

/// Pairs named values by name (ignoring case) and occurrence, and keeps only
/// the ones that differ: additions and changes in the proposed order, then
/// removals.
fn fields(old: &[(String, String)], new: &[(String, String)]) -> Vec<Field> {
    let key = |k: &str| k.to_ascii_lowercase();
    let nth = |list: &[(String, String)], i: usize| {
        let k = key(&list[i].0);
        list[..i].iter().filter(|(n, _)| key(n) == k).count()
    };
    let find = |list: &[(String, String)], name: &str, n: usize| {
        let k = key(name);
        list.iter().filter(|(x, _)| key(x) == k).nth(n).map(|(_, v)| v.clone())
    };
    let mut out = vec![];
    for (i, (name, v)) in new.iter().enumerate() {
        match find(old, name, nth(new, i)) {
            None => out.push(Field { name: name.clone(), op: Op::Add, old: None, new: Some(v.clone()) }),
            Some(o) if o != *v => out.push(Field { name: name.clone(), op: Op::Changed, old: Some(o), new: Some(v.clone()) }),
            Some(_) => {}
        }
    }
    for (i, (name, v)) in old.iter().enumerate() {
        if find(new, name, nth(old, i)).is_none() {
            out.push(Field { name: name.clone(), op: Op::Del, old: Some(v.clone()), new: None });
        }
    }
    out
}

fn query_pairs(url: &str) -> Vec<(String, String)> {
    let q = url.split_once('?').map(|(_, q)| q).unwrap_or("");
    form_pairs(q.split('#').next().unwrap_or(""))
}

/// `a=1&b=2`, decoded (`+` is a space, `%xx` an escape).
fn form_pairs(s: &str) -> Vec<(String, String)> {
    let dec = |t: &str| crate::insight::percent_decode(&t.replace('+', " "));
    s.split('&').filter(|p| !p.is_empty()).map(|p| p.split_once('=').map(|(k, v)| (dec(k), dec(v))).unwrap_or_else(|| (dec(p), String::new()))).collect()
}

fn looks_like_form(s: &str) -> bool {
    let s = s.trim();
    !s.is_empty() && !s.contains(['\n', ' ', '{', '<']) && s.split('&').all(|p| p.split_once('=').is_some_and(|(k, _)| !k.is_empty()))
}

fn header<'a>(r: &'a DraftRequest, name: &str) -> Option<&'a str> {
    r.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

fn body_diff(cur: &DraftRequest, new: &DraftRequest) -> BodyDiff {
    let (a, b) = (cur.body.replace("\r\n", "\n"), new.body.replace("\r\n", "\n"));
    let changed = a != b;
    if a.is_empty() && b.is_empty() {
        return BodyDiff { view: BodyView::None, changed, lines: vec![], fields: vec![] };
    }
    let json = |s: &str| if s.trim().is_empty() { Some(String::new()) } else { serde_json::from_str::<Value>(s).ok().filter(|v| v.is_object() || v.is_array()).map(|v| serde_json::to_string_pretty(&v).unwrap_or_default()) };
    if let (Some(ja), Some(jb)) = (json(&a), json(&b))
        && !(ja.is_empty() && jb.is_empty())
    {
        return BodyDiff { view: BodyView::Json, changed, lines: if changed { lines(&ja, &jb) } else { vec![] }, fields: vec![] };
    }
    let form_type = [cur, new].iter().any(|r| header(r, "content-type").is_some_and(|t| t.to_ascii_lowercase().contains("x-www-form-urlencoded")));
    if (form_type || (looks_like_form(&a) && looks_like_form(&b))) && (a.is_empty() || looks_like_form(&a)) && (b.is_empty() || looks_like_form(&b)) {
        return BodyDiff { view: BodyView::Form, changed, lines: vec![], fields: fields(&form_pairs(a.trim()), &form_pairs(b.trim())) };
    }
    BodyDiff { view: BodyView::Text, changed, lines: if changed { lines(&a, &b) } else { vec![] }, fields: vec![] }
}

/// A line-level diff (longest common subsequence), with every line kept so
/// the reader sees context; long unchanged runs are folded by the Bench.
pub fn lines(old: &str, new: &str) -> Vec<Line> {
    let a: Vec<&str> = old.split('\n').collect();
    let b: Vec<&str> = new.split('\n').collect();
    let mut pre = 0;
    while pre < a.len() && pre < b.len() && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0;
    while suf < a.len() - pre && suf < b.len() - pre && a[a.len() - 1 - suf] == b[b.len() - 1 - suf] {
        suf += 1;
    }
    let line = |op, t: &str| Line { op, text: t.to_string() };
    let mut out: Vec<Line> = a[..pre].iter().map(|t| line(Op::Same, t)).collect();
    let (x, y) = (&a[pre..a.len() - suf], &b[pre..b.len() - suf]);
    let (n, m) = (x.len(), y.len());
    if n * m > 4_000_000 {
        out.extend(x.iter().map(|t| line(Op::Del, t)));
        out.extend(y.iter().map(|t| line(Op::Add, t)));
    } else {
        let w = m + 1;
        let mut l = vec![0u32; (n + 1) * w];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                l[i * w + j] = if x[i] == y[j] { l[(i + 1) * w + j + 1] + 1 } else { l[(i + 1) * w + j].max(l[i * w + j + 1]) };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if x[i] == y[j] {
                out.push(line(Op::Same, x[i]));
                i += 1;
                j += 1;
            } else if l[(i + 1) * w + j] >= l[i * w + j + 1] {
                out.push(line(Op::Del, x[i]));
                i += 1;
            } else {
                out.push(line(Op::Add, y[j]));
                j += 1;
            }
        }
        out.extend(x[i..].iter().map(|t| line(Op::Del, t)));
        out.extend(y[j..].iter().map(|t| line(Op::Add, t)));
    }
    out.extend(a[a.len() - suf..].iter().map(|t| line(Op::Same, t)));
    out
}

// ---- JWTs --------------------------------------------------------------------

static JWT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"eyJ[A-Za-z0-9_-]{5,}\.eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]*").unwrap());

/// A JWT found in a request, with where it sits.
struct Jwt {
    /// Where it is: "URL", "header Authorization" or "body", plus which
    /// occurrence there, so a token is matched with the one it replaces.
    key: (String, usize),
    location: String,
    signing_input: String,
    sig: String,
}

fn find_jwts(r: &DraftRequest) -> Vec<Jwt> {
    let mut out = vec![];
    let mut scan = |text: &str, loc: String, slot: String| {
        for (i, m) in JWT.find_iter(text).enumerate() {
            let (input, sig) = m.as_str().rsplit_once('.').unwrap_or((m.as_str(), ""));
            out.push(Jwt { key: (slot.clone(), i), location: loc.clone(), signing_input: input.into(), sig: sig.into() });
        }
    };
    scan(&r.url, "URL".into(), "url".into());
    for (i, (k, v)) in r.headers.iter().enumerate() {
        let n = r.headers[..i].iter().filter(|(x, _)| x.eq_ignore_ascii_case(k)).count();
        scan(v, format!("header {k}"), format!("h:{}#{n}", k.to_ascii_lowercase()));
    }
    scan(&r.body, "body".into(), "body".into());
    out
}

fn b64url_json(s: &str) -> Option<Value> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// The decoded header and claims, one line each per field, for the diff.
fn decoded(signing_input: &str) -> (String, Option<String>) {
    let (h, p) = signing_input.split_once('.').unwrap_or((signing_input, ""));
    let (hv, pv) = (b64url_json(h), b64url_json(p));
    let alg = hv.as_ref().and_then(|v| v["alg"].as_str()).map(str::to_string);
    let pretty = |v: Option<Value>| v.map(|v| serde_json::to_string_pretty(&v).unwrap_or_default()).unwrap_or_else(|| "(not JSON)".into());
    (format!("header {}\nclaims {}", pretty(hv), pretty(pv)), alg)
}

/// Applies the Lens rule to a suggestion: a token whose header or claims
/// changed keeps the original token's signature (Claude cannot sign), and a
/// signature Claude made up for an unchanged token is put back. A token
/// with no signature, or `alg: none`, is left as suggested: that is a
/// deliberate test. Returns the request Apply should write and what changed.
fn keep_signatures(current: &DraftRequest, proposed: &DraftRequest) -> (DraftRequest, Vec<TokenChange>) {
    let old = find_jwts(current);
    let new = find_jwts(proposed);
    let mut fixed = std::collections::HashMap::new();
    let mut changes = vec![];
    for t in &new {
        let orig = old.iter().find(|o| o.key == t.key).or_else(|| (old.len() == 1 && new.len() == 1).then(|| &old[0]));
        let (new_text, alg) = decoded(&t.signing_input);
        let unsigned_by_design = t.sig.is_empty() || alg.as_deref().is_some_and(|a| a.eq_ignore_ascii_case("none"));
        let Some(o) = orig else {
            let mut notes = vec!["new token".to_string()];
            notes.push(if unsigned_by_design { "unsigned".into() } else { "signature not valid: Claude has no signing key".into() });
            changes.push(TokenChange { location: t.location.clone(), lines: lines("", &new_text), notes, signed: false });
            continue;
        };
        if o.signing_input == t.signing_input && o.sig == t.sig {
            continue;
        }
        let mut notes = vec![];
        let mut sig = t.sig.clone();
        if unsigned_by_design {
            notes.push(if t.sig.is_empty() { "signature removed".into() } else { "alg none".to_string() });
            notes.push("unsigned".into());
        } else {
            if t.sig != o.sig {
                sig = o.sig.clone();
                notes.push("Claude's signature replaced with the original".into());
            }
            if o.signing_input != t.signing_input {
                notes.push("not re-signed: original signature kept".into());
                notes.push("unsigned".into());
            }
        }
        fixed.insert(t.key.clone(), format!("{}.{}", t.signing_input, sig));
        if o.signing_input == t.signing_input && sig == o.sig {
            // Only the signature differed, and it is back to the original.
            continue;
        }
        let (old_text, _) = decoded(&o.signing_input);
        changes.push(TokenChange { location: t.location.clone(), lines: lines(&old_text, &new_text), notes, signed: false });
    }
    let rewrite = |text: &str, slot: &str| -> String {
        let mut i = 0;
        JWT.replace_all(text, |m: &regex::Captures| {
            let k = (slot.to_string(), i);
            i += 1;
            fixed.get(&k).cloned().unwrap_or_else(|| m[0].to_string())
        })
        .into_owned()
    };
    let mut out = proposed.clone();
    out.url = rewrite(&proposed.url, "url");
    for (i, (k, v)) in out.headers.iter_mut().enumerate() {
        let n = proposed.headers[..i].iter().filter(|(x, _)| x.eq_ignore_ascii_case(k)).count();
        *v = rewrite(v, &format!("h:{}#{n}", k.to_ascii_lowercase()));
    }
    out.body = rewrite(&proposed.body, "body");
    (out, changes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, url: &str, headers: &[(&str, &str)], body: &str) -> DraftRequest {
        DraftRequest { method: method.into(), url: url.into(), headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(), body: body.into() }
    }

    fn new(draft: &str, r: DraftRequest) -> NewProposal {
        NewProposal { draft: draft.into(), summary: "  try another id ".into(), request: r }
    }

    fn jwt(header: &str, claims: &str, sig: &str) -> String {
        let e = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!("{}.{}.{sig}", e(header), e(claims))
    }

    #[test]
    fn proposals_are_checked_kept_and_bounded() {
        let p = Proposals::default();
        let a = p.add(new("d1", req("post", " https://a.test/x ", &[("Accept", " */* ")], "")), "claude-code").unwrap();
        assert_eq!((a.id, a.request.method.as_str(), a.request.url.as_str(), a.summary.as_str()), (1, "POST", "https://a.test/x", "try another id"));
        assert_eq!(a.request.headers[0].1, "*/*");
        assert_eq!(a.from, "claude-code");

        for bad in [
            new("", req("GET", "https://a.test/", &[], "")),
            new("d1/../x", req("GET", "https://a.test/", &[], "")),
            new("d1", req("GET", "file:///etc/passwd", &[], "")),
            new("d1", req("GET", "https://a.test/a b", &[], "")),
            new("d1", req("GE T", "https://a.test/", &[], "")),
            new("d1", req("GET", "https://a.test/", &[("X-A", "1\r\nX-Injected: 2")], "")),
            new("d1", req("GET", "https://a.test/", &[("Bad Name", "1")], "")),
            new("d1", req("GET", "https://a.test/", &[], &"x".repeat(MAX_BODY + 1))),
        ] {
            assert!(p.add(bad, "a").is_err());
        }
        assert_eq!(p.list(None).len(), 1);

        for _ in 0..MAX_PER_DRAFT + 2 {
            p.add(new("d2", req("GET", "https://a.test/", &[], "")), "a").unwrap();
        }
        let d2 = p.list(Some("d2"));
        assert_eq!(d2.len(), MAX_PER_DRAFT);
        assert!(d2[0].id > d2[1].id, "newest first");
        assert_eq!(p.list(Some("d1")).len(), 1, "other drafts keep theirs");
        for _ in 0..MAX_KEPT {
            p.add(new(&format!("e{}", p.list(None).len()), req("GET", "https://a.test/", &[], "")), "a").unwrap();
        }
        assert_eq!(p.list(None).len(), MAX_KEPT);

        let last = p.list(None)[0].id;
        assert!(p.get(last).is_some());
        assert_eq!(p.remove(last).map(|x| x.id), Some(last));
        assert!(p.get(last).is_none() && p.remove(last).is_none());
    }

    #[test]
    fn diff_reports_request_line_headers_and_query() {
        let cur = req("GET", "https://a.test/item?id=1&x=%2Fa", &[("Accept", "*/*"), ("X-Old", "1"), ("Cookie", "s=1")], "");
        let new = req("POST", "https://a.test/item?id=2&x=%2Fa&debug=true", &[("accept", "*/*"), ("Cookie", "s=2"), ("X-New", "y")], "");
        let d = diff(&cur, &new);
        assert!(!d.same);
        assert_eq!(d.method.as_ref().map(|c| (c.old.as_str(), c.new.as_str())), Some(("GET", "POST")));
        assert!(d.url.is_some());
        assert_eq!(
            d.query,
            vec![
                Field { name: "id".into(), op: Op::Changed, old: Some("1".into()), new: Some("2".into()) },
                Field { name: "debug".into(), op: Op::Add, old: None, new: Some("true".into()) },
            ]
        );
        let ops: Vec<(&str, Op)> = d.headers.iter().map(|f| (f.name.as_str(), f.op)).collect();
        assert_eq!(ops, vec![("Cookie", Op::Changed), ("X-New", Op::Add), ("X-Old", Op::Del)], "header names compare case-insensitively");
        assert_eq!(d.body.view, BodyView::None);
        assert_eq!(d.proposed, new);

        assert!(diff(&cur, &cur).same);
    }

    #[test]
    fn json_bodies_are_compared_pretty_and_forms_by_field() {
        let cur = req("POST", "https://a.test/", &[], r#"{"user":"alice","role":"user"}"#);
        let new = req("POST", "https://a.test/", &[], r#"{"user":"alice","role":"admin"}"#);
        let d = diff(&cur, &new);
        assert_eq!(d.body.view, BodyView::Json);
        let changed: Vec<(Op, &str)> = d.body.lines.iter().filter(|l| l.op != Op::Same).map(|l| (l.op, l.text.trim().trim_end_matches(','))).collect();
        assert_eq!(changed, vec![(Op::Del, r#""role": "user""#), (Op::Add, r#""role": "admin""#)]);

        let cur = req("POST", "https://a.test/", &[("Content-Type", "application/x-www-form-urlencoded")], "user=alice&next=%2Fhome");
        let new = req("POST", "https://a.test/", &[("Content-Type", "application/x-www-form-urlencoded")], "user=alice&next=%2F%2Fevil.test&debug=1");
        let d = diff(&cur, &new);
        assert_eq!(d.body.view, BodyView::Form);
        assert_eq!(d.body.fields[0], Field { name: "next".into(), op: Op::Changed, old: Some("/home".into()), new: Some("//evil.test".into()) });
        assert_eq!(d.body.fields[1].op, Op::Add);

        let d = diff(&req("POST", "https://a.test/", &[], "line one\nline two\nline three"), &req("POST", "https://a.test/", &[], "line one\nline 2\nline three"));
        assert_eq!(d.body.view, BodyView::Text);
        let ops: Vec<Op> = d.body.lines.iter().map(|l| l.op).collect();
        assert_eq!(ops, vec![Op::Same, Op::Del, Op::Add, Op::Same]);
    }

    #[test]
    fn line_diff_keeps_order_and_context() {
        let l = lines("a\nb\nc\nd", "a\nc\nd\ne");
        let got: Vec<(Op, &str)> = l.iter().map(|l| (l.op, l.text.as_str())).collect();
        assert_eq!(got, vec![(Op::Same, "a"), (Op::Del, "b"), (Op::Same, "c"), (Op::Same, "d"), (Op::Add, "e")]);
        assert!(lines("same", "same").iter().all(|l| l.op == Op::Same));
    }

    #[test]
    fn edited_jwt_keeps_the_original_signature_and_is_marked_unsigned() {
        let orig = jwt(r#"{"alg":"HS256","typ":"JWT"}"#, r#"{"sub":"alice","role":"user"}"#, "ORIGSIG");
        // Claude raises the role and invents a signature for it.
        let edited = jwt(r#"{"alg":"HS256","typ":"JWT"}"#, r#"{"sub":"alice","role":"admin"}"#, "MADEUP");
        let cur = req("GET", "https://a.test/me", &[("Authorization", &format!("Bearer {orig}"))], "");
        let new = req("GET", "https://a.test/me", &[("Authorization", &format!("Bearer {edited}"))], "");
        let d = diff(&cur, &new);
        let auth = &d.proposed.headers[0].1;
        assert!(auth.ends_with(".ORIGSIG") && !auth.contains("MADEUP"), "{auth}");
        assert_eq!(d.tokens.len(), 1);
        let t = &d.tokens[0];
        assert_eq!(t.location, "header Authorization");
        assert!(!t.signed);
        assert!(t.notes.iter().any(|n| n.contains("original signature kept")) && t.notes.iter().any(|n| n == "unsigned"), "{:?}", t.notes);
        assert!(t.lines.iter().any(|l| l.op == Op::Add && l.text.contains("admin")));
        assert!(t.lines.iter().any(|l| l.op == Op::Del && l.text.contains("\"user\"")));

        // A made-up signature on an unchanged token is simply put back.
        let resig = jwt(r#"{"alg":"HS256","typ":"JWT"}"#, r#"{"sub":"alice","role":"user"}"#, "OTHER");
        let d = diff(&cur, &req("GET", "https://a.test/me", &[("Authorization", &format!("Bearer {resig}"))], ""));
        assert!(d.same && d.tokens.is_empty(), "{:?}", d.headers);

        // alg none with no signature is a deliberate test and kept as suggested.
        let none = jwt(r#"{"alg":"none"}"#, r#"{"sub":"alice","role":"admin"}"#, "");
        let d = diff(&cur, &req("GET", "https://a.test/me", &[("Authorization", &format!("Bearer {none}"))], ""));
        assert!(d.proposed.headers[0].1.ends_with('.'));
        assert!(d.tokens[0].notes.iter().any(|n| n == "unsigned"));

        // A token moved from a header to the query is still matched with the one it replaces.
        let d = diff(&cur, &req("GET", &format!("https://a.test/me?token={edited}"), &[], ""));
        assert!(d.proposed.url.ends_with(".ORIGSIG"), "{}", d.proposed.url);
    }
}
