//! SQLite-backed project store: traffic, scope rules, evidence and findings.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::codec;
use crate::model::*;
use crate::query::Query;
use crate::exclude::Group;
use crate::scope::{Decision, Evidence, EvidenceKind, NewEvidence, Rule, ScopeRules, Suggestion};

/// Connection settings, applied every time a database is opened. They are
/// not part of the schema and cannot run inside a transaction.
const PRAGMAS: &str = "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;";

/// One step of the database schema, moving it from `version - 1` to
/// `version`. A step runs in a transaction together with the version bump,
/// so it either completes or leaves the database as it was. Steps must be
/// idempotent: a database a crashed or older build left half-way still
/// migrates cleanly.
#[derive(Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub what: &'static str,
    pub run: fn(&rusqlite::Transaction) -> Result<()>,
}

/// Every schema change, oldest first. Append new steps at the end with the
/// next version number; never edit or reorder a step that has shipped.
pub const MIGRATIONS: &[Migration] = &[
    Migration { version: 1, what: "traffic, scope, evidence, findings and view state", run: v1_initial },
    Migration { version: 2, what: "when findings were last edited", run: v2_finding_updated_at },
    Migration { version: 3, what: "how much of long bodies was kept, WebSocket messages and the HTTP version", run: v3_proxy_transport },
    Migration { version: 4, what: "requests and responses edited in Intercept, with their originals", run: v4_intercept_edits },
];

/// The schema version this build reads and writes.
pub const SCHEMA_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// Version 1 is the schema from before databases were versioned. Every
/// statement is `IF NOT EXISTS`, so a database from that time, which has the
/// tables but `user_version` 0, is taken up as version 1 with its data as is.
fn v1_initial(tx: &rusqlite::Transaction) -> Result<()> {
    tx.execute_batch(V1_SCHEMA)?;
    Ok(())
}

/// Findings can be edited: record when, starting from their creation time.
fn v2_finding_updated_at(tx: &rusqlite::Transaction) -> Result<()> {
    if !has_column(tx, "findings", "updated_at")? {
        tx.execute_batch("ALTER TABLE findings ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0")?;
    }
    tx.execute_batch("UPDATE findings SET updated_at = created_at WHERE updated_at = 0")?;
    Ok(())
}

/// Bodies longer than the recording limit are kept in part (see proxy.rs):
/// whether each body was cut, and its full size. WebSocket messages are
/// stored against the handshake exchange that opened their connection. Each
/// exchange notes the HTTP version spoken with the server.
fn v3_proxy_transport(tx: &rusqlite::Transaction) -> Result<()> {
    let columns = [
        ("exchanges", "req_truncated", "INTEGER NOT NULL DEFAULT 0"),
        ("exchanges", "req_size", "INTEGER"),
        ("exchanges", "resp_truncated", "INTEGER NOT NULL DEFAULT 0"),
        ("exchanges", "resp_size", "INTEGER"),
        // HTTP/1.1 or HTTP/2.
        ("exchanges", "http_version", "TEXT NOT NULL DEFAULT ''"),
    ];
    for (table, column, decl) in columns {
        if !has_column(tx, table, column)? {
            tx.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
        }
    }
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS ws_messages (
            id INTEGER PRIMARY KEY,
            exchange_id INTEGER NOT NULL,
            ts INTEGER NOT NULL,
            direction TEXT NOT NULL,
            opcode TEXT NOT NULL,
            payload BLOB NOT NULL,
            size INTEGER NOT NULL,
            truncated INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS ws_messages_exchange ON ws_messages(exchange_id, id);",
    )?;
    Ok(())
}

/// Requests and responses can be edited in flight (see intercept.rs): mark
/// such exchanges and keep what arrived before the edit.
fn v4_intercept_edits(tx: &rusqlite::Transaction) -> Result<()> {
    let columns = [("edited", "INTEGER NOT NULL DEFAULT 0"), ("original_request", "TEXT"), ("original_response", "TEXT")];
    for (column, decl) in columns {
        if !has_column(tx, "exchanges", column)? {
            tx.execute_batch(&format!("ALTER TABLE exchanges ADD COLUMN {column} {decl}"))?;
        }
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>()?;
    Ok(names.iter().any(|n| n == column))
}

const V1_SCHEMA: &str = r#"
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
CREATE TABLE IF NOT EXISTS view_state (
    view TEXT PRIMARY KEY,
    state TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS exclude_groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    domains TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
"#;

pub struct Store {
    conn: Mutex<Connection>,
}

const SUMMARY_COLS: &str = "e.id, e.ts, e.method, e.scheme, e.host, e.port, e.path, e.query, e.status, e.mime, e.resp_len, e.duration_ms, e.source, e.edited";

/// The ORDER BY for a Traffic column sort (see [`Store::search_sorted`]).
/// Only known columns map to SQL, so the value is never spliced in raw.
fn sort_clause(sort: Option<&str>) -> String {
    let Some(sort) = sort.map(str::trim).filter(|s| !s.is_empty()) else { return "e.id DESC".into() };
    let (key, dir) = match sort.strip_prefix('-') {
        Some(k) => (k, "DESC"),
        None => (sort, "ASC"),
    };
    let col = match key {
        "n" | "id" => return format!("e.id {dir}"),
        "method" => "e.method",
        "host" => "e.host",
        "path" => "e.path",
        "status" => "e.status",
        "type" | "mime" => "e.mime",
        "size" => "e.resp_len",
        "ms" | "duration" => "e.duration_ms",
        "time" | "ts" => "e.ts",
        _ => return "e.id DESC".into(),
    };
    // Rows with no value (no response yet) go last either way.
    format!("{col} IS NULL, {col} {dir}, e.id DESC")
}

/// The columns [`row_to_exchange`] reads, in order.
const EXCHANGE_COLS: &str = "id, ts, scheme, host, port, method, path, query, req_headers, req_body, status, resp_headers,
    resp_body, duration_ms, error, tls_sans, source, initiator, req_truncated, req_size, resp_truncated, resp_size, http_version,
    edited, original_request, original_response";

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(PRAGMAS)?;
        migrate(&mut conn, MIGRATIONS)?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// The schema version of the open database.
    pub fn schema_version(&self) -> Result<i64> {
        Ok(schema_version(&self.conn.lock().unwrap())?)
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
                resp_headers, resp_body, resp_len, mime, duration_ms, error, tls_sans, source, initiator,
                req_truncated, req_size, resp_truncated, resp_size, http_version, edited, original_request, original_response)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)",
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
                ex.resp_len(),
                ex.mime(),
                ex.duration_ms,
                ex.error,
                serde_json::to_string(&ex.tls_sans)?,
                ex.source.unwrap_or(Source::Proxy).as_str(),
                ex.initiator,
                ex.req_truncated,
                ex.req_size,
                ex.resp_truncated,
                ex.resp_size,
                ex.http_version,
                ex.edited,
                ex.original_request,
                ex.original_response,
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
            &format!("SELECT {EXCHANGE_COLS} FROM exchanges WHERE id = ?1"),
            [id],
            row_to_exchange,
        )
        .optional()
        .map_err(Into::into)
    }

    /// Exchanges with id greater than `after`, in id order, for rescans.
    pub fn exchanges_after(&self, after: i64, limit: usize) -> Result<Vec<Exchange>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("SELECT {EXCHANGE_COLS} FROM exchanges WHERE id > ?1 ORDER BY id LIMIT ?2"))?;
        let rows = stmt.query_map(params![after, limit as i64], row_to_exchange)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// A host's most recent exchanges, newest first.
    pub fn exchanges_for_host(&self, host: &str, limit: usize) -> Result<Vec<Exchange>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("SELECT {EXCHANGE_COLS} FROM exchanges WHERE host = ?1 ORDER BY id DESC LIMIT ?2"))?;
        let rows = stmt.query_map(params![host.to_ascii_lowercase(), limit as i64], row_to_exchange)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Stores WebSocket messages, in the order given.
    pub fn insert_ws_messages(&self, messages: &[WsMessage]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for m in messages {
            tx.execute(
                "INSERT INTO ws_messages (exchange_id, ts, direction, opcode, payload, size, truncated) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![m.exchange_id, m.ts, m.direction, m.opcode, m.payload, m.size, m.truncated],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// One connection's WebSocket messages in the order they were sent, and how many there are in all.
    pub fn ws_messages(&self, exchange_id: i64, limit: usize, offset: usize) -> Result<(Vec<WsMessage>, i64)> {
        let conn = self.conn.lock().unwrap();
        let total = conn.query_row("SELECT count(*) FROM ws_messages WHERE exchange_id = ?1", [exchange_id], |r| r.get(0))?;
        let mut stmt = conn.prepare(
            "SELECT id, exchange_id, ts, direction, opcode, payload, size, truncated FROM ws_messages
             WHERE exchange_id = ?1 ORDER BY id LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(params![exchange_id, limit as i64, offset as i64], |r| {
            Ok(WsMessage {
                id: r.get(0)?,
                exchange_id: r.get(1)?,
                ts: r.get(2)?,
                direction: r.get(3)?,
                opcode: r.get(4)?,
                payload: r.get(5)?,
                size: r.get(6)?,
                truncated: r.get(7)?,
            })
        })?;
        Ok((rows.collect::<Result<_, _>>()?, total))
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
        self.search_sorted(query, rules, None, limit, offset)
    }

    /// Like [`Store::search`], ordered by a Traffic column instead: `status`
    /// sorts ascending, `-status` descending, ties newest first. Unknown
    /// columns fall back to newest first.
    pub fn search_sorted(&self, query: &Query, rules: &ScopeRules, sort: Option<&str>, limit: usize, offset: usize) -> Result<(Vec<ExchangeSummary>, i64)> {
        let order = sort_clause(sort);
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
            "SELECT {SUMMARY_COLS} FROM exchanges e WHERE {clause} ORDER BY {order} LIMIT ? OFFSET ?"
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
                edited: r.get(13)?,
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
        let (mut methods, mut statuses, mut kinds, mut hosts, mut paths, mut other_hosts) =
            (BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
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
                *other_hosts.entry(host).or_insert(0) += 1;
            }
        }
        f.methods = ranked(methods);
        f.statuses = ranked(statuses);
        f.kinds = ranked(kinds);
        f.hosts = ranked(hosts);
        f.paths = ranked(paths);
        f.other_hosts = ranked(other_hosts);
        f.other_hosts.truncate(20);
        Ok(f)
    }

    // ---- saved view state -------------------------------------------------

    /// Saved UI state of a view (e.g. its active filters), as JSON.
    pub fn view_state(&self, view: &str) -> Result<Option<serde_json::Value>> {
        let conn = self.conn.lock().unwrap();
        let raw: Option<String> =
            conn.query_row("SELECT state FROM view_state WHERE view = ?1", [view], |r| r.get(0)).optional()?;
        Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
    }

    pub fn set_view_state(&self, view: &str, state: &serde_json::Value) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "INSERT INTO view_state (view, state) VALUES (?1, ?2) ON CONFLICT(view) DO UPDATE SET state = excluded.state",
            params![view, serde_json::to_string(state)?],
        )?;
        Ok(())
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

    /// Inserts or updates several scope rules in one transaction.
    pub fn put_rules(&self, rules: &[Rule]) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        for rule in rules {
            tx.execute(
                "INSERT INTO scope_rules (pattern, include_subdomains, decision, created_at, note) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(pattern) DO UPDATE SET include_subdomains = ?2, decision = ?3, note = ?5",
                params![rule.pattern, rule.include_subdomains, rule.decision.as_str(), rule.created_at, rule.note],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes the exclusion rule for one host (a rule an exclusion group owns),
    /// leaving any manually added rule for the same host untouched.
    pub fn delete_group_rule(&self, pattern: &str) -> Result<bool> {
        Ok(self
            .conn
            .lock()
            .unwrap()
            .execute("DELETE FROM scope_rules WHERE pattern = ?1 AND note LIKE 'group:%'", [pattern])?
            > 0)
    }

    /// Removes every exclusion rule a group owns.
    pub fn delete_group_rules(&self, group_id: &str) -> Result<usize> {
        Ok(self.conn.lock().unwrap().execute("DELETE FROM scope_rules WHERE note = ?1", [format!("group:{group_id}")])?)
    }

    // ---- custom exclusion groups ----------------------------------------

    pub fn custom_groups(&self) -> Result<Vec<Group>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id, name, description, domains FROM exclude_groups ORDER BY created_at")?;
        let rows = stmt.query_map([], |r| {
            let domains: String = r.get(3)?;
            Ok(Group {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                domains: serde_json::from_str(&domains).unwrap_or_default(),
                builtin: false,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn put_custom_group(&self, g: &Group, created_at: i64) -> Result<()> {
        let domains = serde_json::to_string(&g.domains)?;
        self.conn.lock().unwrap().execute(
            "INSERT INTO exclude_groups (id, name, description, domains, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET name = ?2, description = ?3, domains = ?4",
            params![g.id, g.name, g.description, domains, created_at],
        )?;
        Ok(())
    }

    pub fn delete_custom_group(&self, id: &str) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute("DELETE FROM exclude_groups WHERE id = ?1", [id])? > 0)
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

    /// Drops evidence for any domain the current rules now decide, so the
    /// wait list only ever holds domains still waiting on a decision. Called
    /// whenever a rule is added: a new rule can cover domains that were
    /// pending before it existed. Returns how many were pruned.
    pub fn prune_decided(&self, rules: &ScopeRules) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let domains: Vec<String> = {
            let mut stmt = conn.prepare("SELECT DISTINCT domain FROM evidence")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            rows.filter_map(Result::ok).filter(|d| rules.decide_domain(d) != Decision::Unknown).collect()
        };
        let mut pruned = 0;
        for d in &domains {
            pruned += conn.execute("DELETE FROM evidence WHERE domain = ?1", [d])?;
        }
        Ok(pruned)
    }

    /// Exchanges to these hosts, not counting the `keep` ids.
    pub fn count_for_hosts(&self, hosts: &[String], keep: &BTreeSet<i64>) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        let mut n = 0;
        for h in hosts {
            n += conn.query_row("SELECT count(*) FROM exchanges WHERE host = ?1", [h], |r| r.get::<_, i64>(0))?;
            for id in keep {
                n -= conn.query_row("SELECT count(*) FROM exchanges WHERE host = ?1 AND id = ?2", params![h, id], |r| r.get::<_, i64>(0))?;
            }
        }
        Ok(n)
    }

    /// Deletes exchanges to these hosts (and their search index and scope
    /// evidence), except the `keep` ids. Returns how many were deleted.
    pub fn delete_for_hosts(&self, hosts: &[String], keep: &BTreeSet<i64>) -> Result<i64> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute_batch("CREATE TEMP TABLE IF NOT EXISTS doomed (id INTEGER PRIMARY KEY); DELETE FROM doomed;")?;
        for h in hosts {
            tx.execute("INSERT OR IGNORE INTO doomed SELECT id FROM exchanges WHERE host = ?1", [h])?;
        }
        for id in keep {
            tx.execute("DELETE FROM doomed WHERE id = ?1", [id])?;
        }
        let n = tx.execute("DELETE FROM exchanges WHERE id IN (SELECT id FROM doomed)", [])?;
        tx.execute("DELETE FROM exchanges_fts WHERE rowid IN (SELECT id FROM doomed)", [])?;
        tx.execute("DELETE FROM evidence WHERE exchange_id IN (SELECT id FROM doomed)", [])?;
        tx.execute("DELETE FROM ws_messages WHERE exchange_id IN (SELECT id FROM doomed)", [])?;
        tx.execute("DELETE FROM doomed", [])?;
        tx.commit()?;
        Ok(n as i64)
    }

    /// Rewrites the database without free space, so deleted traffic is
    /// gone from the file (and from the search index and write-ahead log).
    pub fn compact(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("INSERT INTO exchanges_fts(exchanges_fts) VALUES('optimize'); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")?;
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
            "INSERT INTO findings (created_at, title, severity, status, description, exchange_ids, created_by, updated_at)
             VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6, ?1)",
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
            updated_at: now,
        })
    }

    pub fn findings(&self) -> Result<Vec<Finding>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&format!("SELECT {FINDING_COLS} FROM findings ORDER BY id"))?;
        let rows = stmt.query_map([], row_to_finding)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn finding(&self, id: i64) -> Result<Option<Finding>> {
        let conn = self.conn.lock().unwrap();
        conn.query_row(&format!("SELECT {FINDING_COLS} FROM findings WHERE id = ?1"), [id], row_to_finding).optional().map_err(Into::into)
    }

    /// Applies a checked edit (see [`FindingEdit::checked`]). `None` when
    /// there is no such finding.
    pub fn update_finding(&self, id: i64, edit: &FindingEdit) -> Result<Option<Finding>> {
        {
            let conn = self.conn.lock().unwrap();
            let changed = conn.execute(
                "UPDATE findings SET title = coalesce(?2, title), severity = coalesce(?3, severity), status = coalesce(?4, status),
                    description = coalesce(?5, description), updated_at = ?6 WHERE id = ?1",
                params![id, edit.title, edit.severity, edit.status, edit.description, now_ms()],
            )?;
            if changed == 0 {
                return Ok(None);
            }
        }
        self.finding(id)
    }

    /// Deletes a finding. The requests it pointed to stay in the traffic.
    pub fn delete_finding(&self, id: i64) -> Result<bool> {
        Ok(self.conn.lock().unwrap().execute("DELETE FROM findings WHERE id = ?1", [id])? > 0)
    }
}

const FINDING_COLS: &str = "id, created_at, title, severity, status, description, exchange_ids, created_by, updated_at";

fn row_to_finding(r: &Row) -> rusqlite::Result<Finding> {
    Ok(Finding {
        id: r.get(0)?,
        created_at: r.get(1)?,
        title: r.get(2)?,
        severity: r.get(3)?,
        status: r.get(4)?,
        description: r.get(5)?,
        exchange_ids: serde_json::from_str(&r.get::<_, String>(6)?).unwrap_or_default(),
        created_by: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn schema_version(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

/// Brings a database up to the last of `steps`, one step at a time. Each
/// step runs in its own write transaction that first re-reads the version,
/// so two processes opening the same file never apply a step twice. A
/// database from a newer build is refused rather than half understood.
fn migrate(conn: &mut Connection, steps: &[Migration]) -> Result<()> {
    let latest = steps.last().map_or(0, |m| m.version);
    let found = schema_version(conn)?;
    if found > latest {
        bail!(
            "this project's database was written by a newer version of Plonix (schema version {found}; this version knows up to {latest}). \
             Update Plonix to open it; the file was not changed"
        );
    }
    for step in steps.iter().filter(|m| m.version > found) {
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if schema_version(&tx)? >= step.version {
            continue;
        }
        (step.run)(&tx).with_context(|| format!("upgrading the database to schema version {} ({})", step.version, step.what))?;
        tx.pragma_update(None, "user_version", step.version)?;
        tx.commit()?;
    }
    Ok(())
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
        req_truncated: r.get(18)?,
        req_size: r.get(19)?,
        resp_truncated: r.get(20)?,
        resp_size: r.get(21)?,
        http_version: r.get(22)?,
        edited: r.get(23)?,
        original_request: r.get(24)?,
        original_response: r.get(25)?,
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
    fn truncated_bodies_keep_their_full_size() {
        let s = Store::open_in_memory().unwrap();
        let mut ex = sample("www.example.com", "GET", "/big", 200, "<b>start of a long page");
        ex.resp_truncated = true;
        ex.resp_size = Some(50_000_000);
        ex.http_version = "HTTP/2".into();
        let id = s.insert_exchange(&ex).unwrap();
        let got = s.get_exchange(id).unwrap().unwrap();
        assert_eq!(got.http_version, "HTTP/2");
        assert!(got.resp_truncated && !got.req_truncated);
        assert_eq!((got.resp_size, got.req_size), (Some(50_000_000), None));
        let (hits, _) = s.search(&Query::parse("start of a long").unwrap(), &ScopeRules::default(), 10, 0).unwrap();
        assert_eq!(hits[0].resp_len, 50_000_000, "listings show the size as sent");
    }

    #[test]
    fn websocket_messages_follow_their_exchange() {
        let s = Store::open_in_memory().unwrap();
        let keep = s.insert_exchange(&sample("www.example.com", "GET", "/socket", 101, "")).unwrap();
        let gone = s.insert_exchange(&sample("cdn.other.test", "GET", "/socket", 101, "")).unwrap();
        let msg = |exchange_id, direction: &str, payload: &[u8]| WsMessage {
            exchange_id,
            ts: now_ms(),
            direction: direction.into(),
            opcode: "text".into(),
            payload: payload.to_vec(),
            size: payload.len() as i64,
            ..Default::default()
        };
        s.insert_ws_messages(&[msg(keep, "to_server", b"hello"), msg(keep, "to_client", b"hi"), msg(gone, "to_server", b"x")]).unwrap();
        let (list, total) = s.ws_messages(keep, 10, 0).unwrap();
        assert_eq!(total, 2);
        assert_eq!((list[0].direction.as_str(), list[0].payload.as_slice()), ("to_server", &b"hello"[..]));
        assert_eq!(list[1].text().as_deref(), Some("hi"));
        assert_eq!(s.ws_messages(keep, 10, 1).unwrap().0.len(), 1);
        s.delete_for_hosts(&["cdn.other.test".into()], &BTreeSet::new()).unwrap();
        assert_eq!(s.ws_messages(gone, 10, 0).unwrap().1, 0, "deleting traffic deletes its messages");
        assert_eq!(s.ws_messages(keep, 10, 0).unwrap().1, 2);
    }

    #[test]
    fn older_databases_gain_the_new_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("old.db");
        {
            // A database from before these columns, with traffic in it.
            let mut conn = Connection::open(&path).unwrap();
            migrate(&mut conn, &MIGRATIONS[..2]).unwrap();
            let s = Store { conn: Mutex::new(conn) };
            assert!(s.insert_exchange(&sample("a.test", "GET", "/", 200, "x")).is_err(), "the old table lacks the columns");
            s.conn.lock().unwrap().execute_batch(
                "INSERT INTO exchanges (ts, scheme, host, port, method, path, query, req_headers, req_body, status, resp_headers, resp_body,
                    resp_len, mime, duration_ms, tls_sans, source) VALUES (1, 'https', 'old.test', 443, 'GET', '/', '', '[]', x'', 200, '[]', x'', 0, '', 0, '[]', 'proxy')",
            )
            .unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        let old = s.get_exchange(1).unwrap().unwrap();
        assert!(!old.resp_truncated && old.resp_size.is_none() && old.http_version.is_empty(), "older captures read as whole");
        assert_eq!(s.ws_messages(1, 10, 0).unwrap().1, 0, "the messages table is there");
        s.insert_exchange(&sample("a.test", "GET", "/", 200, "x")).unwrap();
        drop(s);
        let s = Store::open(&path).unwrap();
        assert_eq!(s.count().unwrap(), 2, "opening again changes nothing");
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

        let sorted = |sort: &str| -> Vec<Option<u16>> {
            let (rows, _) = s.search_sorted(&Query::parse("").unwrap(), &rules, Some(sort), 50, 0).unwrap();
            rows.into_iter().map(|r| r.status).collect()
        };
        assert_eq!(sorted("status"), vec![Some(200), Some(201), Some(404), Some(500)]);
        assert_eq!(sorted("-status"), vec![Some(500), Some(404), Some(201), Some(200)]);
        assert_eq!(sorted("id; DROP TABLE exchanges"), sorted("-n"), "unknown sorts fall back to newest first");
        let (hosts, _) = s.search_sorted(&Query::parse("").unwrap(), &rules, Some("host"), 50, 0).unwrap();
        assert_eq!(hosts.first().map(|r| r.host.as_str()), Some("api.example.com"));
        assert_eq!(hosts.last().map(|r| r.host.as_str()), Some("www.example.com"));
    }

    #[test]
    fn include_and_exclude_filters() {
        let s = Store::open_in_memory().unwrap();
        let rules = seeded(&s);
        let with_type = |host: &str, path: &str, status: u16, ct: &str| {
            let mut ex = sample(host, "GET", path, status, "");
            ex.resp_headers = vec![("Content-Type".into(), ct.into())];
            ex
        };
        s.insert_exchange(&with_type("www.example.com", "/", 200, "text/html")).unwrap();
        s.insert_exchange(&with_type("www.example.com", "/app.JS", 200, "application/javascript")).unwrap();
        s.insert_exchange(&with_type("www.example.com", "/logo", 200, "image/png")).unwrap();
        s.insert_exchange(&with_type("www.example.com", "/fonts/a.woff2", 304, "")).unwrap();
        s.insert_exchange(&with_type("api.example.com", "/api/me", 302, "application/json")).unwrap();
        s.insert_exchange(&with_type("tracker.ads.net", "/collect", 204, "")).unwrap();
        let mut failed = sample("api.example.com", "POST", "/api/upload", 0, "");
        failed.status = None;
        failed.resp_headers.clear();
        s.insert_exchange(&failed).unwrap();

        let run = |q: &str| -> Vec<String> {
            let (rows, _) = s.search(&Query::parse(q).unwrap(), &rules, 50, 0).unwrap();
            let mut v: Vec<String> = rows.into_iter().map(|r| r.path).collect();
            v.sort();
            v
        };
        assert_eq!(run("-kind:static"), vec!["/", "/api/me", "/api/upload", "/collect"]);
        assert_eq!(run("kind:static"), vec!["/app.JS", "/fonts/a.woff2", "/logo"]);
        assert_eq!(run("ext:js,woff2"), vec!["/app.JS", "/fonts/a.woff2"]);
        assert_eq!(run("status:2xx,3xx -host:www.example.com"), vec!["/api/me", "/collect"]);
        assert_eq!(run("-host:www.example.com,tracker.ads.net"), vec!["/api/me", "/api/upload"]);
        // Excluding a status class keeps requests that got no response.
        assert_eq!(run("host:api.example.com -status:3xx"), vec!["/api/upload"]);
        assert_eq!(run("host:api.example.com -mime:json"), vec!["/api/upload"]);
        assert_eq!(s.facets(&rules).unwrap().other_hosts, vec![Count { value: "tracker.ads.net".into(), count: 1 }]);
    }

    #[test]
    fn view_state_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.view_state("traffic").unwrap(), None);
        let v = serde_json::json!({ "filters": [{ "term": "kind:static", "mode": "exclude" }] });
        s.set_view_state("traffic", &v).unwrap();
        s.set_view_state("traffic", &v).unwrap();
        assert_eq!(s.view_state("traffic").unwrap(), Some(v));
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
    fn adding_a_covering_rule_clears_matching_pending_items() {
        let s = Store::open_in_memory().unwrap();
        let ev = |d: &str, k: EvidenceKind, via: &str| NewEvidence { domain: d.into(), kind: k, via: via.into(), detail: "x".into() };
        // Three domains wait on a decision: a subdomain, a wildcard under the
        // same base, and an unrelated sibling.
        s.add_evidence(&ev("api.shop.test", EvidenceKind::RequestedFrom, "shop.test"), 1, 10).unwrap();
        s.add_evidence(&ev("*.cdn.shop.test", EvidenceKind::LinkedFrom, "shop.test"), 2, 20).unwrap();
        s.add_evidence(&ev("auth.other.test", EvidenceKind::SharesSession, "shop.test"), 3, 30).unwrap();
        assert_eq!(s.suggestions(&ScopeRules::default()).unwrap().len(), 3);

        // Accepting shop.test with its subdomains covers the first two. Pruning
        // against the new rule set drops exactly those from the wait list.
        s.put_rule(&Rule { pattern: "shop.test".into(), include_subdomains: true, decision: Decision::Accepted, created_at: 1, note: String::new() }).unwrap();
        let pruned = s.prune_decided(&s.rules().unwrap()).unwrap();
        assert_eq!(pruned, 2);
        let left: Vec<_> = s.suggestions(&s.rules().unwrap()).unwrap().into_iter().map(|x| x.domain).collect();
        assert_eq!(left, vec!["auth.other.test"]);

        // A later rejection of the sibling clears the wait list entirely, and
        // pruning is idempotent once nothing matches.
        s.put_rule(&Rule { pattern: "auth.other.test".into(), include_subdomains: false, decision: Decision::Rejected, created_at: 2, note: String::new() }).unwrap();
        assert_eq!(s.prune_decided(&s.rules().unwrap()).unwrap(), 1);
        assert_eq!(s.prune_decided(&s.rules().unwrap()).unwrap(), 0);
        assert!(s.suggestions(&s.rules().unwrap()).unwrap().is_empty());
    }

    #[test]
    fn fresh_database_gets_the_latest_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traffic.db");
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        s.insert_exchange(&sample("www.example.com", "GET", "/", 200, "hi")).unwrap();
        drop(s);
        // Opening again changes nothing and keeps the data.
        let s = Store::open(&path).unwrap();
        assert_eq!((s.schema_version().unwrap(), s.count().unwrap()), (SCHEMA_VERSION, 1));
    }

    #[test]
    fn unversioned_database_is_taken_up_without_losing_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traffic.db");
        {
            // A project from before schema versions: the tables, user_version 0.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(V1_SCHEMA).unwrap();
            conn.execute(
                "INSERT INTO findings (created_at, title, severity, status, description, exchange_ids, created_by)
                 VALUES (5, 'Old finding', 'low', 'open', 'kept', '[1]', 'cli')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO scope_rules (pattern, include_subdomains, decision, created_at) VALUES ('example.com', 1, 'accepted', 1)", [])
                .unwrap();
            assert_eq!(schema_version(&conn).unwrap(), 0);
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
        let f = &s.findings().unwrap()[0];
        assert_eq!((f.title.as_str(), f.description.as_str(), f.exchange_ids.clone()), ("Old finding", "kept", vec![1]));
        assert_eq!(f.updated_at, 5, "edit time starts at the creation time");
        assert!(s.rules().unwrap().in_scope("www.example.com"));
        s.insert_exchange(&sample("www.example.com", "GET", "/", 200, "hi")).unwrap();
    }

    #[test]
    fn newer_database_is_refused_and_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("traffic.db");
        drop(Store::open(&path).unwrap());
        Connection::open(&path).unwrap().pragma_update(None, "user_version", SCHEMA_VERSION + 1).unwrap();
        let e = format!("{:#}", Store::open(&path).err().expect("a newer schema must be refused"));
        assert!(e.contains("newer version of Plonix") && e.contains(&format!("schema version {}", SCHEMA_VERSION + 1)), "{e}");
        assert_eq!(schema_version(&Connection::open(&path).unwrap()).unwrap(), SCHEMA_VERSION + 1);
    }

    #[test]
    fn each_migration_step_runs_once_and_in_order() {
        fn one(tx: &rusqlite::Transaction) -> Result<()> {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS ran (step INTEGER); INSERT INTO ran VALUES (1);")?;
            Ok(())
        }
        fn two(tx: &rusqlite::Transaction) -> Result<()> {
            tx.execute_batch("INSERT INTO ran VALUES (2);")?;
            Ok(())
        }
        fn broken(tx: &rusqlite::Transaction) -> Result<()> {
            tx.execute_batch("INSERT INTO ran VALUES (3);")?;
            bail!("step failed")
        }
        let steps = [Migration { version: 1, what: "one", run: one }, Migration { version: 2, what: "two", run: two }];
        let mut conn = Connection::open_in_memory().unwrap();
        let ran = |c: &Connection| -> Vec<i64> {
            let mut st = c.prepare("SELECT step FROM ran ORDER BY rowid").unwrap();
            st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
        };
        migrate(&mut conn, &steps[..1]).unwrap();
        assert_eq!((schema_version(&conn).unwrap(), ran(&conn)), (1, vec![1]));
        migrate(&mut conn, &steps).unwrap();
        migrate(&mut conn, &steps).unwrap();
        assert_eq!((schema_version(&conn).unwrap(), ran(&conn)), (2, vec![1, 2]));

        // A failing step is rolled back whole and leaves the version alone.
        let with_broken = [steps[0], steps[1], Migration { version: 3, what: "broken", run: broken }];
        let e = format!("{:#}", migrate(&mut conn, &with_broken).unwrap_err());
        assert!(e.contains("schema version 3 (broken)") && e.contains("step failed"), "{e}");
        assert_eq!((schema_version(&conn).unwrap(), ran(&conn)), (2, vec![1, 2]));
    }

    #[test]
    fn finding_edit_time_migration_is_idempotent() {
        // A version 1 database where the column already exists (a step that
        // was interrupted after the change, or added by hand) still upgrades.
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn, &MIGRATIONS[..1]).unwrap();
        conn.execute_batch(
            "ALTER TABLE findings ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
             INSERT INTO findings (created_at, title, severity, status, description, exchange_ids, created_by) VALUES (7, 't', 'low', 'open', '', '[]', 'cli');",
        )
        .unwrap();
        migrate(&mut conn, MIGRATIONS).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(conn.query_row("SELECT updated_at FROM findings", [], |r| r.get::<_, i64>(0)).unwrap(), 7);
    }

    #[test]
    fn findings_can_be_edited_and_deleted() {
        let s = Store::open_in_memory().unwrap();
        let f = s.add_finding(&NewFinding { title: "XSS".into(), severity: "low".into(), description: "first".into(), exchange_ids: vec![3] }, "gui").unwrap();
        let edit = FindingEdit { status: Some("false positive".into()), ..Default::default() }.checked().unwrap();
        let g = s.update_finding(f.id, &edit).unwrap().unwrap();
        assert_eq!((g.status.as_str(), g.title.as_str(), g.severity.as_str(), g.description.as_str()), ("false_positive", "XSS", "low", "first"));
        assert!(g.updated_at >= f.updated_at);
        let edit = FindingEdit { title: Some("  Stored XSS ".into()), severity: Some("High".into()), description: Some(String::new()), status: None };
        let g = s.update_finding(f.id, &edit.checked().unwrap()).unwrap().unwrap();
        assert_eq!((g.status.as_str(), g.title.as_str(), g.severity.as_str(), g.description.as_str()), ("false_positive", "Stored XSS", "high", ""));
        assert_eq!(g.exchange_ids, vec![3]);
        assert!(s.update_finding(f.id + 1, &edit_status("fixed")).unwrap().is_none());
        assert!(s.delete_finding(f.id).unwrap());
        assert!(!s.delete_finding(f.id).unwrap());
        assert!(s.finding(f.id).unwrap().is_none() && s.findings().unwrap().is_empty());
    }

    fn edit_status(st: &str) -> FindingEdit {
        FindingEdit { status: Some(st.into()), ..Default::default() }.checked().unwrap()
    }

    #[test]
    fn finding_edits_are_checked() {
        let bad = |e: FindingEdit| e.checked().unwrap_err();
        assert!(bad(FindingEdit::default()).contains("nothing to change"));
        assert!(bad(FindingEdit { title: Some("  ".into()), ..Default::default() }).contains("title"));
        assert!(bad(FindingEdit { severity: Some("urgent".into()), ..Default::default() }).contains("severity must be one of"));
        assert!(bad(FindingEdit { status: Some("done".into()), ..Default::default() }).contains("open, confirmed, false_positive, fixed"));
        for (typed, stored) in [("Confirmed", "confirmed"), ("false-positive", "false_positive"), ("fp", "false_positive"), (" fixed ", "fixed")] {
            assert_eq!(edit_status(typed).status.as_deref(), Some(stored));
        }
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
