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
//! scope:in | scope:out  source:proxy | source:replay
//! "set-cookie: sid"     quoted phrase, full text
//! passw -logout         full text is substring and case-insensitive
//! ```

use anyhow::{Result, bail};
use rusqlite::types::Value;

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
}

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
        let mut terms = Vec::new();
        for raw in tokenize(input) {
            let (negate, tok) = match raw.strip_prefix('-') {
                Some(rest) if !rest.is_empty() => (true, rest.to_string()),
                _ => (false, raw.clone()),
            };
            let field = match tok.split_once(':') {
                Some((k, v)) if !v.is_empty() && is_field(k) => parse_field(&k.to_ascii_lowercase(), v)?,
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
    }
}

fn is_field(k: &str) -> bool {
    matches!(k.to_ascii_lowercase().as_str(), "host" | "method" | "status" | "path" | "mime" | "scope" | "source" | "ext" | "kind")
}

fn parse_field(k: &str, v: &str) -> Result<Field> {
    let mut list = Vec::new();
    for one in v.trim_matches('"').split(',').map(str::trim).filter(|x| !x.is_empty()) {
        list.push(parse_value(k, one)?);
    }
    Ok(match list.len() {
        0 => bail!("{k}: needs a value"),
        1 => list.pop().unwrap(),
        _ => Field::AnyOf(list),
    })
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
    fn sql_has_matching_params() {
        let q = Query::parse("host:a.com scope:in x -kind:static method:GET,POST -ext:png").unwrap();
        let (sql, params) = q.to_sql(&["a.com".into(), "b.com".into()]);
        assert_eq!(sql.matches('?').count(), params.len());
    }
}
