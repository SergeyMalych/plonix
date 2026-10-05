//! Traffic search language.
//!
//! A query is a list of space separated terms. Field terms filter on
//! metadata; anything else is full-text matched against URL, headers and
//! decoded bodies. All terms must match.
//!
//! Include and exclude: a plain term shows only what matches it, a term with
//! a leading `-` hides what matches it. A field term can list several values
//! separated by commas and matches any of them: `status:2xx,3xx` shows both
//! classes, `-host:a.com,b.com` hides both hosts.
//!
//! ```text
//! host:example.com      host or any subdomain (globs: host:*.cdn.*)
//! method:POST           status:404  status:5xx  status:none (no response)
//! path:/api             path prefix (globs: path:*admin*)
//! ext:js                file extension of the path
//! mime:json             substring of the response content type
//! kind:static           images, fonts, stylesheets, scripts and media
//! scope:in | scope:out  source:proxy | source:replay | source:import
//! is:graphql            a named filter from a filter pack (see filterpack.rs)
//! "set-cookie: sid"     quoted phrase, full text
//! passw -logout         full text is substring and case-insensitive
//! ```

use anyhow::{Result, bail};
use rusqlite::types::Value;

use crate::codec;
use crate::model::{Exchange, Source};

#[derive(Debug, Clone, PartialEq)]
pub enum Field {
    Host(String),
    Method(String),
    Status(StatusMatch),
    Path(String),
    Mime(String),
    Scope(bool),
    Source(String),
    Ext(String),
    Kind(Kind),
    Text(String),
    /// Comma separated values of one field: matches if any of them does.
    AnyOf(Vec<Field>),
    /// A named filter (`is:name`): all of its terms must match.
    Group(Vec<Term>),
}

/// Looks up the query behind a named filter (`is:name`).
pub type Resolver<'a> = &'a dyn Fn(&str) -> Option<String>;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Static,
}

/// Path extensions that `kind:static` counts as static files.
pub const STATIC_EXTS: &[&str] = &[
    "css", "js", "mjs", "map", "png", "jpg", "jpeg", "gif", "webp", "avif", "svg", "ico", "bmp", "woff", "woff2", "ttf", "otf", "eot",
    "mp4", "webm", "mp3", "wav", "ogg",
];

/// Content types that `kind:static` counts as static files.
const STATIC_MIME_PREFIXES: &[&str] = &["image/", "font/", "video/", "audio/"];
const STATIC_MIME_PARTS: &[&str] = &["css", "javascript", "ecmascript", "woff"];

#[derive(Debug, Clone, PartialEq)]
pub enum StatusMatch {
    Exact(u16),
    Class(u16),
    None,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Term {
    pub negate: bool,
    pub field: Field,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub terms: Vec<Term>,
}

impl Query {
    pub fn parse(input: &str) -> Result<Self> {
        Self::parse_with(input, &|_| None)
    }

    /// Parses a query, expanding `is:name` through `resolve`. A named
    /// filter's own query may not use `is:` (no nesting, so no loops).
    pub fn parse_with(input: &str, resolve: Resolver) -> Result<Self> {
        let mut terms = Vec::new();
        for raw in tokenize(input) {
            let (negate, tok) = match raw.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => (true, rest.to_string()),
                _ => (false, raw.clone()),
            };
            let field = match tok.split_once(':') {
                Some((k, v)) if !v.is_empty() && is_field(k) => parse_field(&k.to_ascii_lowercase(), v, resolve)?,
                _ => Field::Text(tok.trim_matches('"').to_string()),
            };
            if let Field::Text(t) = &field {
                if t.is_empty() {
                    continue;
                }
            }
            terms.push(Term { negate, field });
        }
        Ok(Query { terms })
    }

    /// Builds a SQL `WHERE` clause over the `exchanges` table (alias `e`).
    /// `scope_hosts` lists the stored hosts currently in scope.
    pub fn to_sql(&self, scope_hosts: &[String]) -> (String, Vec<Value>) {
        let mut clauses = Vec::new();
        let mut params: Vec<Value> = Vec::new();
        for term in &self.terms {
            let clause = field_sql(&term.field, scope_hosts, &mut params);
            // Columns can be NULL (no response): exclude only rows that match.
            clauses.push(if term.negate { format!("NOT coalesce({clause}, 0)") } else { clause });
        }
        if clauses.is_empty() {
            ("1".into(), params)
        } else {
            (clauses.join(" AND "), params)
        }
    }
}

impl Query {
    /// Whether one exchange matches, without the database: used for traffic
    /// passing through the proxy right now (Intercept). Full-text terms look
    /// at the URL, headers and decoded bodies, like the search index does.
    pub fn matches(&self, ex: &Exchange, in_scope: bool) -> bool {
        let mut text = None;
        terms_match(&self.terms, ex, in_scope, &mut text)
    }
}

fn terms_match(terms: &[Term], ex: &Exchange, in_scope: bool, text: &mut Option<String>) -> bool {
    terms.iter().all(|t| field_matches(&t.field, ex, in_scope, text) != t.negate)
}

fn field_matches(field: &Field, ex: &Exchange, in_scope: bool, text: &mut Option<String>) -> bool {
    let path = ex.path.to_ascii_lowercase();
    match field {
        Field::Host(h) => {
            let host = ex.host.to_ascii_lowercase();
            if h.contains('*') { glob_match(h, &host) } else { host == *h || host.ends_with(&format!(".{h}")) }
        }
        Field::Method(m) => ex.method.eq_ignore_ascii_case(m),
        Field::Status(StatusMatch::Exact(s)) => ex.status == Some(*s),
        Field::Status(StatusMatch::Class(c)) => ex.status.is_some_and(|s| s / 100 == *c),
        Field::Status(StatusMatch::None) => ex.status.is_none(),
        Field::Path(p) => {
            let p = p.to_ascii_lowercase();
            if p.contains('*') { glob_match(&p, &path) } else { path.starts_with(&p) }
        }
        Field::Mime(m) => ex.mime().contains(&m.to_ascii_lowercase()),
        Field::Scope(inside) => in_scope == *inside,
        Field::Source(s) => ex.source.unwrap_or(Source::Proxy).as_str() == s,
        Field::Ext(x) => path.ends_with(&format!(".{x}")),
        Field::Kind(Kind::Static) => {
            let mime = ex.mime();
            STATIC_MIME_PREFIXES.iter().any(|m| mime.starts_with(m))
                || STATIC_MIME_PARTS.iter().any(|m| mime.contains(m))
                || STATIC_EXTS.iter().any(|x| path.ends_with(&format!(".{x}")))
        }
        Field::Text(t) => {
            let hay = text.get_or_insert_with(|| {
                format!(
                    "{}\n{} {}\n{}\n{}\n{}\n{}",
                    ex.url(),
                    ex.method,
                    ex.path,
                    codec::headers_text(&ex.req_headers),
                    codec::body_text(&ex.req_headers, &ex.req_body).unwrap_or_default(),
                    codec::headers_text(&ex.resp_headers),
                    codec::body_text(&ex.resp_headers, &ex.resp_body).unwrap_or_default()
                )
                .to_lowercase()
            });
            hay.contains(&t.to_lowercase())
        }
        Field::AnyOf(list) => list.iter().any(|f| field_matches(f, ex, in_scope, text)),
        Field::Group(terms) => terms_match(terms, ex, in_scope, text),
    }
}

/// `*` matches any run of characters; everything else is literal.
fn glob_match(pattern: &str, s: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if parts.len() == 1 {
        return s == pattern;
    }
    if !s.starts_with(first) || s.len() < first.len() + last.len() || !s.ends_with(last) {
        return false;
    }
    let mut rest = &s[first.len()..s.len() - last.len()];
    for mid in &parts[1..parts.len() - 1] {
        match rest.find(mid) {
            Some(i) => rest = &rest[i + mid.len()..],
            None => return false,
        }
    }
    true
}

fn field_sql(field: &Field, scope_hosts: &[String], params: &mut Vec<Value>) -> String {
    match field {
        Field::Host(h) => {
            if h.contains('*') {
                params.push(Value::Text(glob_to_like(h)));
                "e.host LIKE ? ESCAPE '\\'".to_string()
            } else {
                params.push(Value::Text(h.clone()));
                params.push(Value::Text(format!("%.{}", escape_like(h))));
                "(e.host = ? OR e.host LIKE ? ESCAPE '\\')".to_string()
            }
        }
        Field::Method(m) => {
            params.push(Value::Text(m.clone()));
            "e.method = ?".to_string()
        }
        Field::Status(StatusMatch::Exact(s)) => {
            params.push(Value::Integer(*s as i64));
            "e.status = ?".to_string()
        }
        Field::Status(StatusMatch::Class(c)) => {
            params.push(Value::Integer(*c as i64 * 100));
            params.push(Value::Integer(*c as i64 * 100 + 99));
            "(e.status BETWEEN ? AND ?)".to_string()
        }
        Field::Status(StatusMatch::None) => "e.status IS NULL".to_string(),
        Field::Path(p) => {
            let pat = if p.contains('*') { glob_to_like(p) } else { format!("{}%", escape_like(p)) };
            params.push(Value::Text(pat));
            "e.path LIKE ? ESCAPE '\\'".to_string()
        }
        Field::Mime(m) => {
            params.push(Value::Text(format!("%{}%", escape_like(&m.to_ascii_lowercase()))));
            "e.mime LIKE ? ESCAPE '\\'".to_string()
        }
        Field::Scope(inside) => {
            let marks = vec!["?"; scope_hosts.len()].join(",");
            params.extend(scope_hosts.iter().map(|h| Value::Text(h.clone())));
            let in_list = if scope_hosts.is_empty() { "0".to_string() } else { format!("e.host IN ({marks})") };
            if *inside { in_list } else { format!("NOT ({in_list})") }
        }
        Field::Source(s) => {
            params.push(Value::Text(s.clone()));
            "e.source = ?".to_string()
        }
        // The index uses the trigram tokenizer, so matches are substrings.
        // Trigrams need at least three characters; shorter terms scan.
        Field::Text(t) if t.chars().count() >= 3 => {
            params.push(Value::Text(fts_phrase(t)));
            "e.id IN (SELECT rowid FROM exchanges_fts WHERE exchanges_fts MATCH ?)".to_string()
        }
        Field::Text(t) => {
            let pat = format!("%{}%", escape_like(t));
            params.extend([Value::Text(pat.clone()), Value::Text(pat.clone()), Value::Text(pat)]);
            "e.id IN (SELECT rowid FROM exchanges_fts WHERE url LIKE ? ESCAPE '\\' OR req LIKE ? ESCAPE '\\' OR resp LIKE ? ESCAPE '\\')".to_string()
        }
        Field::Ext(x) => {
            params.push(Value::Text(format!("%.{}", escape_like(x))));
            "lower(e.path) LIKE ? ESCAPE '\\'".to_string()
        }
        Field::Kind(Kind::Static) => {
            let mut ors = Vec::new();
            for m in STATIC_MIME_PREFIXES {
                params.push(Value::Text(format!("{m}%")));
                ors.push("e.mime LIKE ?");
            }
            for m in STATIC_MIME_PARTS {
                params.push(Value::Text(format!("%{m}%")));
                ors.push("e.mime LIKE ?");
            }
            for x in STATIC_EXTS {
                params.push(Value::Text(format!("%.{x}")));
                ors.push("lower(e.path) LIKE ?");
            }
            format!("({})", ors.join(" OR "))
        }
        Field::AnyOf(list) => {
            let ors: Vec<String> = list.iter().map(|f| field_sql(f, scope_hosts, params)).collect();
            format!("({})", ors.join(" OR "))
        }
        Field::Group(terms) => {
            let (clause, mut inner) = Query { terms: terms.clone() }.to_sql(scope_hosts);
            params.append(&mut inner);
            format!("({clause})")
        }
    }
}

fn is_field(k: &str) -> bool {
    matches!(k.to_ascii_lowercase().as_str(), "host" | "method" | "status" | "path" | "mime" | "scope" | "source" | "ext" | "kind" | "is")
}

fn parse_field(k: &str, v: &str, resolve: Resolver) -> Result<Field> {
    let mut list = Vec::new();
    for one in v.trim_matches('"').split(',').map(str::trim).filter(|x| !x.is_empty()) {
        list.push(if k == "is" { parse_named(one, resolve)? } else { parse_value(k, one)? });
    }
    Ok(match list.len() {
        0 => bail!("{k}: needs a value"),
        1 => list.pop().unwrap(),
        _ => Field::AnyOf(list),
    })
}

fn parse_named(name: &str, resolve: Resolver) -> Result<Field> {
    let name = name.to_ascii_lowercase();
    let Some(q) = resolve(&name) else { bail!("unknown filter is:{name} (see `plonix filters`)") };
    // Expanded without a resolver: a named filter cannot refer to another.
    let inner = Query::parse(&q).map_err(|e| anyhow::anyhow!("filter is:{name}: {e}"))?;
    Ok(Field::Group(inner.terms))
}

fn parse_value(k: &str, v: &str) -> Result<Field> {
    Ok(match k {
        "host" => Field::Host(v.to_ascii_lowercase()),
        "method" => Field::Method(v.to_ascii_uppercase()),
        "status" => {
            let lv = v.to_ascii_lowercase();
            if lv == "none" || lv == "error" {
                Field::Status(StatusMatch::None)
            } else if lv.len() == 3 && lv.ends_with("xx") {
                match lv[..1].parse::<u16>() {
                    Ok(c) if (1..=5).contains(&c) => Field::Status(StatusMatch::Class(c)),
                    _ => bail!("bad status class '{v}' (use 2xx..5xx)"),
                }
            } else {
                match lv.parse::<u16>() {
                    Ok(s) => Field::Status(StatusMatch::Exact(s)),
                    Err(_) => bail!("bad status '{v}'"),
                }
            }
        }
        "path" => Field::Path(v.to_string()),
        "mime" => Field::Mime(v.to_string()),
        "scope" => match v.to_ascii_lowercase().as_str() {
            "in" | "yes" | "true" => Field::Scope(true),
            "out" | "no" | "false" => Field::Scope(false),
            _ => bail!("scope must be 'in' or 'out'"),
        },
        "source" => Field::Source(v.to_ascii_lowercase()),
        "ext" => Field::Ext(v.trim_start_matches('.').to_ascii_lowercase()),
        "kind" => match v.to_ascii_lowercase().as_str() {
            "static" => Field::Kind(Kind::Static),
            _ => bail!("kind must be 'static'"),
        },
        _ => unreachable!(),
    })
}

/// Splits on whitespace, keeping double-quoted sections together.
fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in input.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c);
            }
            c if c.is_whitespace() && !quoted => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// FTS5 phrase literal: always quoted so punctuation is never parsed as syntax.
fn fts_phrase(t: &str) -> String {
    format!("\"{}\"", t.replace('"', "\"\""))
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

fn glob_to_like(s: &str) -> String {
    escape_like(s).replace('*', "%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fields_text_and_negation() {
        let q = Query::parse(r#"host:Example.com method:post status:5xx -path:/static "set-cookie: sid" token"#).unwrap();
        assert_eq!(
            q.terms,
            vec![
                Term { negate: false, field: Field::Host("example.com".into()) },
                Term { negate: false, field: Field::Method("POST".into()) },
                Term { negate: false, field: Field::Status(StatusMatch::Class(5)) },
                Term { negate: true, field: Field::Path("/static".into()) },
                Term { negate: false, field: Field::Text("set-cookie: sid".into()) },
                Term { negate: false, field: Field::Text("token".into()) },
            ]
        );
    }

    #[test]
    fn unknown_prefix_is_text() {
        let q = Query::parse("https://x.test/a").unwrap();
        assert_eq!(q.terms[0].field, Field::Text("https://x.test/a".into()));
    }

    #[test]
    fn named_filters_expand_and_negate_as_a_group() {
        let defs = |n: &str| match n {
            "graphql" => Some("path:/graphql method:POST".to_string()),
            "loop" => Some("is:graphql".to_string()),
            _ => None,
        };
        let q = Query::parse_with("-is:graphql host:a.com", &defs).unwrap();
        assert_eq!(q.terms.len(), 2);
        assert!(q.terms[0].negate);
        assert!(matches!(&q.terms[0].field, Field::Group(t) if t.len() == 2));
        let (sql, params) = q.to_sql(&[]);
        assert!(sql.starts_with("NOT coalesce(("), "{sql}");
        assert_eq!(sql.matches('?').count(), params.len());
        assert!(Query::parse_with("is:graphql,nope", &defs).unwrap_err().to_string().contains("unknown filter is:nope"));
        assert!(Query::parse_with("is:loop", &defs).is_err(), "named filters cannot nest");
        assert!(Query::parse("is:graphql").is_err());
    }

    #[test]
    fn rejects_bad_values() {
        assert!(Query::parse("status:abc").is_err());
        assert!(Query::parse("scope:maybe").is_err());
    }

    #[test]
    fn comma_lists_match_any_value() {
        let q = Query::parse("status:2xx,3xx -host:a.com,b.com ext:.JS kind:static").unwrap();
        assert_eq!(
            q.terms,
            vec![
                Term {
                    negate: false,
                    field: Field::AnyOf(vec![Field::Status(StatusMatch::Class(2)), Field::Status(StatusMatch::Class(3))])
                },
                Term { negate: true, field: Field::AnyOf(vec![Field::Host("a.com".into()), Field::Host("b.com".into())]) },
                Term { negate: false, field: Field::Ext("js".into()) },
                Term { negate: false, field: Field::Kind(Kind::Static) },
            ]
        );
        assert!(Query::parse("status:2xx,abc").is_err());
        assert!(Query::parse("kind:weird").is_err());
        assert!(Query::parse("host:,").is_err());
    }

    #[test]
    fn matches_one_exchange_like_the_search_does() {
        let ex = Exchange {
            scheme: "https".into(),
            host: "api.app.test".into(),
            port: 443,
            method: "POST".into(),
            path: "/api/Login".into(),
            query: "next=1".into(),
            req_headers: vec![("content-type".into(), "application/json".into())],
            req_body: br#"{"user":"alice"}"#.to_vec(),
            ..Default::default()
        };
        let m = |q: &str, scope: bool| Query::parse(q).unwrap().matches(&ex, scope);
        assert!(m("method:POST path:/api", true));
        assert!(m("host:app.test path:*login*", false));
        assert!(m("alice -method:GET", false));
        assert!(m("status:none", true), "a request on its way has no status yet");
        assert!(!m("scope:in", false));
        assert!(!m("method:GET,PUT", true));
        assert!(!m("host:*.other.test", true));
        assert!(m("", true));
        assert!(glob_match("*.cdn.*", "a.cdn.test") && !glob_match("a*b", "ab-c"));
    }

    #[test]
    fn sql_has_matching_params() {
        let q = Query::parse("host:a.com scope:in x -kind:static method:GET,POST -ext:png").unwrap();
        let (sql, params) = q.to_sql(&["a.com".into(), "b.com".into()]);
        assert_eq!(sql.matches('?').count(), params.len());
    }
}
