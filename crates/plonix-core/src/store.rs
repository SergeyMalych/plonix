//! SQLite-backed project store: traffic, scope rules, evidence and findings.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::codec;
use crate::model::*;
use crate::query::Query;
use crate::scope::{Decision, Evidence, EvidenceKind, NewEvidence, Rule, ScopeRules, Suggestion};

const SCHEMA: &str = r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS exchanges (
    id INTEGER PRIMARY KEY,
    ts INTEGER NOT NULL,
    scheme TEXT NOT NULL,
    host TEXT NOT NULL,
    port INTEGER NOT NULL,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    query TEXT NOT NULL,
    req_headers TEXT NOT NULL,
    req_body BLOB NOT NULL,
    status INTEGER,
    resp_headers TEXT NOT NULL,
    resp_body BLOB NOT NULL,
    resp_len INTEGER NOT NULL,
    mime TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    error TEXT,
    tls_sans TEXT NOT NULL,
    source TEXT NOT NULL,
    initiator TEXT
);
CREATE INDEX IF NOT EXISTS exchanges_host ON exchanges(host);
CREATE INDEX IF NOT EXISTS exchanges_ts ON exchanges(ts);
CREATE VIRTUAL TABLE IF NOT EXISTS exchanges_fts USING fts5(url, req, resp, tokenize = 'trigram case_sensitive 0');
CREATE TABLE IF NOT EXISTS scope_rules (
    pattern TEXT PRIMARY KEY,
    include_subdomains INTEGER NOT NULL,
    decision TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    note TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS session_tokens (
    hash TEXT PRIMARY KEY,
    host TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS evidence (
    domain TEXT NOT NULL,
    kind TEXT NOT NULL,
    via TEXT NOT NULL,
    detail TEXT NOT NULL,
    exchange_id INTEGER NOT NULL,
    count INTEGER NOT NULL,
    first_seen INTEGER NOT NULL,
    last_seen INTEGER NOT NULL,
    PRIMARY KEY (domain, kind, via)
);
CREATE TABLE IF NOT EXISTS findings (
    id INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    title TEXT NOT NULL,
    severity TEXT NOT NULL,
    status TEXT NOT NULL,
    description TEXT NOT NULL,
    exchange_ids TEXT NOT NULL,
    created_by TEXT NOT NULL
);
"#;

pub struct Store {
    conn: Mutex<Connection>,
}

const SUMMARY_COLS: &str = "e.id, e.ts, e.method, e.scheme, e.host, e.port, e.path, e.query, e.status, e.mime, e.resp_len, e.duration_ms, e.source";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    // ---- traffic ---------------------------------------------------------

    pub fn insert_exchange(&self, ex: &Exchange) -> Result<i64> {
        let req_text = format!(
            "{} {}{}\n{}\n{}",
            ex.method,
            ex.path,
            if ex.query.is_empty() { String::new() } else { format!("?{}", ex.query) },
            codec::headers_text(&ex.req_headers),
            codec::body_text(&ex.req_headers, &ex.req_body).unwrap_or_default()
        );
        let resp_text = format!(
            "{}\n{}\n{}",
            ex.status.map(|s| s.to_string()).unwrap_or_default(),
            codec::headers_text(&ex.resp_headers),
            codec::body_text(&ex.resp_headers, &ex.resp_body).unwrap_or_default()
        );
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO exchanges (ts, scheme, host, port, method, path, query, req_headers, req_body, status,
                resp_headers, resp_body, resp_len, mime, duration_ms, error, tls_sans, source, initiator)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)",
            params![
                ex.ts,
                ex.scheme,
                ex.host.to_ascii_lowercase(),
                ex.port,
                ex.method,
                ex.path,
                ex.query,
                serde_json::to_string(&ex.req_headers)?,
                ex.req_body,
                ex.status,
                serde_json::to_string(&ex.resp_headers)?,
                ex.resp_body,
                ex.resp_body.len() as i64,
                ex.mime(),
                ex.duration_ms,
                ex.error,
                serde_json::to_string(&ex.tls_sans)?,
                ex.source.unwrap_or(Source::Proxy).as_str(),
                ex.initiator,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO exchanges_fts (rowid, url, req, resp) VALUES (?1, ?2, ?3, ?4)",
            params![id, ex.url(), req_text, resp_text],
        )?;
        tx.commit()?;
        Ok(id)
    }

    pub fn get_exchange(&self, id: i64) -> Result<Option<Exchange>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(
            "SELECT id, ts, scheme, host, port, method, path, query, req_headers, req_body, status, resp_headers,
                    resp_body, duration_ms, error, tls_sans, source, initiator FROM exchanges WHERE id = ?1",
            [id],
            row_to_exchange,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Exchanges with id greater than `after`, in id order, for rescans.
    pub fn exchanges_after(&self, after: i64, limit: usize) -> Result<Vec<Exchange>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, ts, scheme, host, port, method, path, query, req_headers, req_body, status, resp_headers,
                    resp_body, duration_ms, error, tls_sans, source, initiator FROM exchanges WHERE id > ?1 ORDER BY id LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![after, limit as i64], row_to_exchange)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// A host's most recent exchanges, newest first.
    pub fn exchanges_for_host(&self, host: &str, limit: usize) -> Result<Vec<Exchange>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, ts, scheme, host, port, method, path, query, req_headers, req_body, status, resp_headers,
                    resp_body, duration_ms, error, tls_sans, source, initiator FROM exchanges WHERE host = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![host.to_ascii_lowercase(), limit as i64], row_to_exchange)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn count(&self) -> Result<i64> {
        Ok(self.conn.lock().unwrap().query_row("SELECT count(*) FROM exchanges", [], |r| r.get(0))?)
    }

    pub fn distinct_hosts(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT DISTINCT host FROM exchanges")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Searches traffic, newest first. Returns the page and the total match count.
    pub fn search(&self, query: &Query, rules: &ScopeRules, limit: usize, offset: usize) -> Result<(Vec<ExchangeSummary>, i64)> {
        let scope_hosts: Vec<String> = self.distinct_hosts()?.into_iter().filter(|h| rules.in_scope(h)).collect();
        let (clause, mut params) = query.to_sql(&scope_hosts);
        let conn = self.conn.lock().unwrap();
        let total: i64 = conn.query_row(
            &format!("SELECT count(*) FROM exchanges e WHERE {clause}"),
            params_from_iter(params.iter()),
            |r| r.get(0),
        )?;
        params.push((limit as i64).into());
        params.push((offset as i64).into());
        let mut stmt = conn.prepare(&format!(
            "SELECT {SUMMARY_COLS} FROM exchanges e WHERE {clause} ORDER BY e.id DESC LIMIT ? OFFSET ?"
        ))?;
        let rows = stmt.query_map(params_from_iter(params.iter()), |r| {
            let host: String = r.get(4)?;
            Ok(ExchangeSummary {
                id: r.get(0)?,
                ts: r.get(1)?,
                method: r.get(2)?,
                scheme: r.get(3)?,
                in_scope: rules.in_scope(&host),
                host,
                port: r.get(5)?,
                path: r.get(6)?,
                query: r.get(7)?,
                status: r.get(8)?,
                mime: r.get(9)?,
                resp_len: r.get(10)?,
                duration_ms: r.get(11)?,
                source: r.get(12)?,
            })
        })?;
        Ok((rows.collect::<Result<_, _>>()?, total))
    }

    pub fn hosts(&self, rules: &ScopeRules) -> Result<Vec<HostSummary>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT host, count(*), min(ts), max(ts) FROM exchanges GROUP BY host ORDER BY count(*) DESC")?;
        let rows = stmt.query_map([], |r| {
            let host: String = r.get(0)?;
            Ok(HostSummary { scope: rules.decide(&host), host, requests: r.get(1)?, first_seen: r.get(2)?, last_seen: r.get(3)? })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Counts over recent traffic that filter suggestions are built from.
    pub fn facets(&self, rules: &ScopeRules) -> Result<Facets> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT host, method, status, mime, source, path FROM exchanges ORDER BY id DESC LIMIT ?1")?;
        let mut rows = stmt.query([Facets::SAMPLE as i64])?;
        let mut f = Facets::default();
        let (mut methods, mut statuses, mut kinds, mut hosts, mut paths) =
            (BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
        let mut scope_cache: BTreeMap<String, bool> = BTreeMap::new();
        while let Some(r) = rows.next()? {
            let host: String = r.get(0)?;
            let method: String = r.get(1)?;
            let status: Option<u16> = r.get(2)?;
            let mime: String = r.get(3)?;
            let source: String = r.get(4)?;
            let path: String = r.get(5)?;
            f.sampled += 1;
            let in_scope = *scope_cache.entry(host.clone()).or_insert_with(|| rules.in_scope(&host));
            *methods.entry(method).or_insert(0) += 1;
            let class = match status {
                Some(s) if (100..600).contains(&s) => format!("{}xx", s / 100),
                Some(_) => "other".into(),
                None => "none".into(),
            };
            *statuses.entry(class).or_insert(0) += 1;
            if let Some(kind) = mime_kind(&mime) {
                *kinds.entry(kind).or_insert(0) += 1;
            }
            if source == "replay" {
                f.replays += 1;
            }
            if in_scope {
                f.in_scope += 1;
                if let Some(seg) = first_segment(&path) {
                    *paths.entry(seg).or_insert(0) += 1;
                }
                *hosts.entry(host).or_insert(0) += 1;
            } else {
                f.out_of_scope += 1;
            }
        }
        f.methods = ranked(methods);
        f.statuses = ranked(statuses);
        f.kinds = ranked(kinds);
        f.hosts = ranked(hosts);
        f.paths = ranked(paths);
        Ok(f)
    }

    /// Endpoints seen on a host, with numeric/UUID path segments folded into `{id}`.
    pub fn endpoints(&self, host: &str) -> Result<Vec<Endpoint>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, method, path, query, status, req_headers, req_body FROM exchanges WHERE host = ?1 ORDER BY id DESC LIMIT 20000",
        )?;
        let mut map: BTreeMap<(String, String), (Endpoint, BTreeSet<u16>, BTreeSet<String>)> = BTreeMap::new();
        let mut rows = stmt.query([host.to_ascii_lowercase()])?;
        while let Some(r) = rows.next()? {
            let id: i64 = r.get(0)?;
            let method: String = r.get(1)?;
            let path = fold_path(&r.get::<_, String>(2)?);
            let query: String = r.get(3)?;
            let status: Option<u16> = r.get(4)?;
            let headers: Headers = serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default();
            let body: Vec<u8> = r.get(6)?;
            let entry = map.entry((path.clone(), method.clone())).or_insert_with(|| {
                (
                    Endpoint { method, path, requests: 0, statuses: vec![], params: vec![], sample_id: id },
                    BTreeSet::new(),
                    BTreeSet::new(),
                )
            });
            entry.0.requests += 1;
            entry.1.extend(status);
            entry.2.extend(param_names(&query, &headers, &body));
        }
        Ok(map
            .into_values()
            .map(|(mut e, statuses, params)| {
                e.statuses = statuses.into_iter().collect();
                e.params = params.into_iter().collect();
                e
            })
            .collect())
    }

    // ---- scope -----------------------------------------------------------

    pub fn rules(&self) -> Result<ScopeRules> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT pattern, include_subdomains, decision, created_at, note FROM scope_rules ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| {
            Ok(Rule {
                pattern: r.get(0)?,
                include_subdomains: r.get(1)?,
                decision: Decision::parse(&r.get::<_, String>(2)?),
                created_at: r.get(3)?,
                note: r.get(4)?,
            })
        })?;
        Ok(ScopeRules { rules: rows.collect::<Result<_, _>>()? })
    }

    pub fn put_rule(&self, rule: &Rule) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO scope_rules (pattern, include_subdomains, decision, created_at, note) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(pattern) DO UPDATE SET include_subdomains = ?2, decision = ?3, note = ?5",
            params![rule.pattern, rule.include_subdomains, rule.decision.as_str(), rule.created_at, rule.note],
        )?;
        Ok(())
    }

    pub fn delete_rule(&self, pattern: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute("DELETE FROM scope_rules WHERE pattern = ?1", [pattern])? > 0)
    }

    pub fn add_tokens(&self, hashes: &[String], host: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        for h in hashes {
            conn.execute("INSERT OR IGNORE INTO session_tokens (hash, host) VALUES (?1, ?2)", params![h, host])?;
        }
        Ok(())
    }

    pub fn token_owner(&self, hash: &str) -> Option<String> {
        let conn = self.conn.lock().unwrap();
        conn.query_row("SELECT host FROM session_tokens WHERE hash = ?1", [hash], |r| r.get(0)).ok()
    }

    pub fn add_evidence(&self, ev: &NewEvidence, exchange_id: i64, ts: i64) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO evidence (domain, kind, via, detail, exchange_id, count, first_seen, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)
             ON CONFLICT(domain, kind, via) DO UPDATE SET count = count + 1, last_seen = max(last_seen, ?6)",
            params![ev.domain, ev.kind.as_str(), ev.via, ev.detail, exchange_id, ts],
        )?;
        Ok(())
    }

    /// Forgets derived scope state so it can be rebuilt by a rescan.
    pub fn clear_analysis(&self) -> Result<()> {
        self.conn.lock().unwrap().execute_batch("DELETE FROM evidence; DELETE FROM session_tokens;")?;
        Ok(())
    }

    /// Pending suggestions (domains with evidence and no decision), best first.
    pub fn suggestions(&self, rules: &ScopeRules) -> Result<Vec<Suggestion>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT domain, kind, via, detail, exchange_id, count, first_seen, last_seen FROM evidence ORDER BY domain, first_seen",
        )?;
        let mut by_domain: BTreeMap<String, Vec<Evidence>> = BTreeMap::new();
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let domain: String = r.get(0)?;
            if rules.decide_domain(&domain) != Decision::Unknown {
                continue;
            }
            let Some(kind) = EvidenceKind::parse(&r.get::<_, String>(1)?) else { continue };
            let via: String = r.get(2)?;
            by_domain.entry(domain).or_default().push(Evidence {
                summary: kind.describe(&via),
                kind,
                via,
                detail: r.get(3)?,
                exchange_id: r.get(4)?,
                count: r.get(5)?,
                first_seen: r.get(6)?,
                last_seen: r.get(7)?,
            });
        }
        let mut out: Vec<Suggestion> = by_domain
            .into_iter()
            .map(|(domain, evidence)| {
                let requests = if domain.starts_with("*.") {
                    0
                } else {
                    conn.query_row("SELECT count(*) FROM exchanges WHERE host = ?1", [&domain], |r| r.get(0)).unwrap_or(0)
                };
                Suggestion { score: evidence.iter().map(|e| e.kind.weight()).sum(), domain, requests, evidence }
            })
            .collect();
        out.sort_by(|a, b| b.score.cmp(&a.score).then(b.requests.cmp(&a.requests)).then(a.domain.cmp(&b.domain)));
        Ok(out)
    }

    // ---- findings --------------------------------------------------------

    pub fn add_finding(&self, f: &NewFinding, created_by: &str) -> Result<Finding> {
        let conn = self.conn.lock().unwrap();
        let now = now_ms();
        conn.execute(
            "INSERT INTO findings (created_at, title, severity, status, description, exchange_ids, created_by)
             VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6)",
            params![now, f.title, f.severity, f.description, serde_json::to_string(&f.exchange_ids)?, created_by],
        )?;
        Ok(Finding {
            id: conn.last_insert_rowid(),
            created_at: now,
            title: f.title.clone(),
            severity: f.severity.clone(),
            status: "open".into(),
            description: f.description.clone(),
            exchange_ids: f.exchange_ids.clone(),
            created_by: created_by.into(),
        })
    }

    pub fn findings(&self) -> Result<Vec<Finding>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, created_at, title, severity, status, description, exchange_ids, created_by FROM findings ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Finding {
                id: r.get(0)?,
                created_at: r.get(1)?,
                title: r.get(2)?,
                severity: r.get(3)?,
                status: r.get(4)?,
                description: r.get(5)?,
                exchange_ids: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
                created_by: r.get(7)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }
}

fn row_to_exchange(r: &Row) -> rusqlite::Result<Exchange> {
    let json = |i: usize| -> rusqlite::Result<String> { r.get(i) };
    Ok(Exchange {
        id: r.get(0)?,
        ts: r.get(1)?,
        scheme: r.get(2)?,
        host: r.get(3)?,
        port: r.get(4)?,
        method: r.get(5)?,
        path: r.get(6)?,
        query: r.get(7)?,
        req_headers: serde_json::from_str(&json(8)?).unwrap_or_default(),
        req_body: r.get(9)?,
        status: r.get(10)?,
        resp_headers: serde_json::from_str(&json(11)?).unwrap_or_default(),
        resp_body: r.get(12)?,
        duration_ms: r.get(13)?,
        error: r.get(14)?,
        tls_sans: serde_json::from_str(&json(15)?).unwrap_or_default(),
        source: Some(Source::parse(&r.get::<_, String>(16)?)),
        initiator: r.get(17)?,
    })
}

/// `/users/42/orders/9f1c...` → `/users/{id}/orders/{id}`
fn ranked(map: BTreeMap<String, i64>) -> Vec<Count> {
    let mut v: Vec<Count> = map.into_iter().map(|(value, count)| Count { value, count }).collect();
    v.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
    v
}

/// A response content type, as one of the kinds `mime:` filters are useful for.
pub fn mime_kind(mime: &str) -> Option<String> {
    let m = mime.to_ascii_lowercase();
    let kind = if m.contains("json") {
        "json"
    } else if m.contains("html") {
        "html"
    } else if m.contains("javascript") || m.contains("ecmascript") {
        "javascript"
    } else if m.contains("xml") && !m.starts_with("image/") {
        "xml"
    } else if m.contains("css") {
        "css"
    } else if m.starts_with("image/") {
        "image"
    } else if m.starts_with("font/") || m.contains("woff") || m.contains("font") {
        "font"
    } else if m.starts_with("text/") {
        "text"
    } else {
        return None;
    };
    Some(kind.into())
}

/// `/api/users/7` → `/api`. Skips files and ids, which make poor filters.
fn first_segment(path: &str) -> Option<String> {
    let seg = path.trim_start_matches('/').split(['/', '?']).next()?;
    let folded = fold_path(seg);
    if seg.is_empty() || seg.contains('.') || folded == "{id}" || seg.len() > 40 {
        return None;
    }
    Some(format!("/{seg}"))
}

pub fn fold_path(path: &str) -> String {
    path.split('/')
        .map(|seg| {
            let numeric = !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit());
            let hexish = seg.len() >= 16 && seg.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
            if numeric || hexish { "{id}" } else { seg }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn param_names(query: &str, headers: &Headers, body: &[u8]) -> Vec<String> {
    let mut out: Vec<String> = query
        .split('&')
        .filter_map(|p| p.split('=').next())
        .filter(|k| !k.is_empty())
        .map(|k| format!("query:{k}"))
        .collect();
    let ct = header(headers, "content-type").unwrap_or("").to_ascii_lowercase();
    if ct.contains("x-www-form-urlencoded") {
        out.extend(
            String::from_utf8_lossy(body)
                .split('&')
                .filter_map(|p| p.split('=').next().map(str::to_string))
                .filter(|k| !k.is_empty())
                .map(|k| format!("body:{k}")),
        );
    } else if ct.contains("json") {
        if let Ok(serde_json::Value::Object(m)) = serde_json::from_slice::<serde_json::Value>(body) {
            out.extend(m.keys().map(|k| format!("json:{k}")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(host: &str, method: &str, path: &str, status: u16, body: &str) -> Exchange {
        Exchange {
            ts: now_ms(),
            scheme: "https".into(),
            host: host.into(),
            port: 443,
            method: method.into(),
            path: path.into(),
            query: "q=1&page=2".into(),
            req_headers: vec![("Host".into(), host.into()), ("Content-Type".into(), "application/json".into())],
            req_body: br#"{"user":"alice","password":"hunter2"}"#.to_vec(),
            status: Some(status),
            resp_headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
            resp_body: body.as_bytes().to_vec(),
            source: Some(Source::Proxy),
            ..Default::default()
        }
    }

    fn seeded(store: &Store) -> ScopeRules {
        store
            .put_rule(&Rule { pattern: "example.com".into(), include_subdomains: true, decision: Decision::Accepted, created_at: 0, note: String::new() })
            .unwrap();
        store.rules().unwrap()
    }

    #[test]
    fn insert_get_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let mut ex = sample("www.example.com", "POST", "/login", 302, "<b>welcome</b>");
        ex.tls_sans = vec!["www.example.com".into()];
        ex.resp_body.extend_from_slice(&[0, 159, 146, 150]);
        let id = s.insert_exchange(&ex).unwrap();
        let got = s.get_exchange(id).unwrap().unwrap();
        assert_eq!(got.id, id);
        assert_eq!(got.req_body, ex.req_body);
        assert_eq!(got.resp_body, ex.resp_body);
        assert_eq!(got.req_headers, ex.req_headers);
        assert_eq!(got.tls_sans, ex.tls_sans);
        assert_eq!(got.url(), "https://www.example.com/login?q=1&page=2");
        assert!(s.get_exchange(id + 1).unwrap().is_none());
    }

    #[test]
    fn facets_only_list_what_occurs() {
        let s = Store::open_in_memory().unwrap();
        let rules = seeded(&s);
        let mut json = sample("api.example.com", "POST", "/api/v1/users", 201, "{}");
        json.resp_headers = vec![("Content-Type".into(), "application/json".into())];
        s.insert_exchange(&json).unwrap();
        s.insert_exchange(&sample("api.example.com", "GET", "/api/v1/users/7", 500, "err")).unwrap();
        s.insert_exchange(&sample("www.example.com", "GET", "/favicon.ico", 200, "")).unwrap();
        s.insert_exchange(&sample("cdn.other.net", "GET", "/static/app.js", 404, "")).unwrap();

        let f = s.facets(&rules).unwrap();
        assert_eq!((f.sampled, f.in_scope, f.out_of_scope, f.replays), (4, 3, 1, 0));
        assert_eq!(f.methods, vec![Count { value: "GET".into(), count: 3 }, Count { value: "POST".into(), count: 1 }]);
        let statuses: Vec<&str> = f.statuses.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(statuses, vec!["2xx", "4xx", "5xx"]);
        assert_eq!(f.kinds[0], Count { value: "html".into(), count: 3 });
        assert!(f.kinds.iter().any(|c| c.value == "json"));
        assert!(!f.kinds.iter().any(|c| c.value == "image"));
        // Only in-scope traffic, and files are not path filters.
        assert_eq!(f.paths, vec![Count { value: "/api".into(), count: 2 }]);
        assert_eq!(f.hosts[0], Count { value: "api.example.com".into(), count: 2 });
        assert!(!f.hosts.iter().any(|c| c.value == "cdn.other.net"));
    }

    #[test]
    fn search_filters_and_full_text() {
        let s = Store::open_in_memory().unwrap();
        let rules = seeded(&s);
        s.insert_exchange(&sample("www.example.com", "GET", "/", 200, "Hello Dashboard")).unwrap();
        s.insert_exchange(&sample("api.example.com", "POST", "/api/v1/users", 201, "{\"id\":7}")).unwrap();
        s.insert_exchange(&sample("cdn.other.net", "GET", "/static/app.js", 404, "not found")).unwrap();
        s.insert_exchange(&sample("api.example.com", "DELETE", "/api/v1/users/7", 500, "Internal Error")).unwrap();

        let run = |q: &str| -> Vec<String> {
            let (rows, total) = s.search(&Query::parse(q).unwrap(), &rules, 50, 0).unwrap();
            assert_eq!(rows.len() as i64, total);
            rows.into_iter().map(|r| format!("{} {}", r.method, r.path)).collect()
        };
        assert_eq!(run("").len(), 4);
        assert_eq!(run("host:example.com").len(), 3);
        assert_eq!(run("host:api.example.com method:post"), vec!["POST /api/v1/users"]);
        assert_eq!(run("status:5xx"), vec!["DELETE /api/v1/users/7"]);
        assert_eq!(run("scope:out"), vec!["GET /static/app.js"]);
        assert_eq!(run("scope:in -host:api.example.com"), vec!["GET /"]);
        // Substring, case-insensitive, over decoded bodies, headers and URL.
        assert_eq!(run("dashb"), vec!["GET /"]);
        assert_eq!(run("hunter2").len(), 4);
        assert_eq!(run("\"internal error\""), vec!["DELETE /api/v1/users/7"]);
        assert_eq!(run("path:/api -status:500"), vec!["POST /api/v1/users"]);
        assert_eq!(run("7}"), vec!["POST /api/v1/users"], "short terms fall back to a scan");
        assert_eq!(run("other.net/static"), vec!["GET /static/app.js"]);
        let (page, total) = s.search(&Query::parse("").unwrap(), &rules, 2, 1).unwrap();
        assert_eq!((page.len(), total), (2, 4));
        assert!(page[0].id > page[1].id, "newest first");
    }

    #[test]
    fn site_map_folds_ids_and_collects_params() {
        let s = Store::open_in_memory().unwrap();
        s.insert_exchange(&sample("api.example.com", "GET", "/users/1", 200, "")).unwrap();
        s.insert_exchange(&sample("api.example.com", "GET", "/users/2", 404, "")).unwrap();
        s.insert_exchange(&sample("api.example.com", "POST", "/users", 201, "")).unwrap();
        let eps = s.endpoints("api.example.com").unwrap();
        assert_eq!(eps.len(), 2);
        let get = eps.iter().find(|e| e.path == "/users/{id}").unwrap();
        assert_eq!(get.requests, 2);
        assert_eq!(get.statuses, vec![200, 404]);
        assert!(get.params.contains(&"query:page".to_string()));
        assert!(get.params.contains(&"json:password".to_string()));
    }

    #[test]
    fn suggestions_aggregate_and_hide_decided() {
        let s = Store::open_in_memory().unwrap();
        let rules = seeded(&s);
        let ev = |d: &str, k: EvidenceKind, via: &str| NewEvidence { domain: d.into(), kind: k, via: via.into(), detail: "x".into() };
        s.add_evidence(&ev("cdn.net", EvidenceKind::LinkedFrom, "www.example.com"), 1, 10).unwrap();
        s.add_evidence(&ev("cdn.net", EvidenceKind::LinkedFrom, "www.example.com"), 2, 20).unwrap();
        s.add_evidence(&ev("auth.idp.io", EvidenceKind::SharesSession, "www.example.com"), 3, 30).unwrap();
        s.add_evidence(&ev("auth.idp.io", EvidenceKind::RequestedFrom, "app.example.com"), 4, 40).unwrap();
        let sug = s.suggestions(&rules).unwrap();
        assert_eq!(sug[0].domain, "auth.idp.io");
        assert_eq!(sug[0].score, 7);
        assert_eq!(sug[1].evidence[0].count, 2);
        s.put_rule(&Rule { pattern: "cdn.net".into(), include_subdomains: false, decision: Decision::Rejected, created_at: 1, note: String::new() })
            .unwrap();
        let sug = s.suggestions(&s.rules().unwrap()).unwrap();
        assert_eq!(sug.len(), 1);
    }

    #[test]
    fn findings_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let f = s
            .add_finding(&NewFinding { title: "IDOR".into(), severity: "high".into(), description: "d".into(), exchange_ids: vec![1, 2] }, "mcp")
            .unwrap();
        let all = s.findings().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, f.id);
        assert_eq!(all[0].exchange_ids, vec![1, 2]);
        assert_eq!(all[0].created_by, "mcp");
    }
}
