//! HAR files (HTTP Archive 1.2): export captured traffic, import traffic
//! other tools and browsers recorded.
//!
//! Export writes one entry per exchange: request and response headers,
//! cookies, query parameters, bodies (decoded from their content encoding;
//! binary ones in base64) and the time the exchange took. Plonix stores the
//! total time only, so the timings put it all under `wait`. WebSocket
//! messages go in `_webSocketMessages`, as browsers write them. Entries are
//! written one at a time, so an export of any size streams.
//!
//! Import reads entries one at a time too (see [`read_entries`]), so a large
//! file is never held in memory whole. Each entry becomes an exchange with
//! source `import` (see [`to_exchange`]); the engine stores it like captured
//! traffic, skipping entries it has already (see
//! [`crate::engine::Engine::import_har`]).

use std::io::{Read, Write};

use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Value, json};

use crate::codec;
use crate::model::{Exchange, Headers, Source, WsMessage, header, header_all, now_ms};
use crate::scope;

/// What an import did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportReport {
    /// Entries stored as new exchanges.
    pub imported: usize,
    /// Entries already in the project, left out.
    pub duplicates: usize,
    /// Entries that could not be read.
    pub skipped: usize,
    /// Why entries were skipped (the first few).
    pub problems: Vec<String>,
    /// The ids the imported exchanges got.
    pub first_id: Option<i64>,
    pub last_id: Option<i64>,
}

impl ImportReport {
    pub const MAX_PROBLEMS: usize = 20;

    pub fn skip(&mut self, n: usize, why: String) {
        self.skipped += 1;
        if self.problems.len() < Self::MAX_PROBLEMS {
            self.problems.push(format!("entry {n}: {why}"));
        }
    }
}

/// The largest body undone from its content encoding for an export.
const MAX_DECODED: usize = 512 * 1024 * 1024;

/// A file name for an export: `shop-2024-05-01.har`.
pub fn file_name(project: &str) -> String {
    let name: String = project.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let name = name.trim_matches('-');
    let day = iso_time(now_ms());
    format!("{}-{}.har", if name.is_empty() { "plonix" } else { name }, &day[..10])
}

// ---- export ------------------------------------------------------------

/// Writes a HAR file with `entries`, in the order given. Returns how many
/// entries were written.
pub fn write<W: Write>(mut out: W, entries: impl Iterator<Item = Result<(Exchange, Vec<WsMessage>)>>) -> Result<usize> {
    let creator = json!({ "name": "Plonix", "version": env!("CARGO_PKG_VERSION") });
    write!(out, "{{\"log\":{{\"version\":\"1.2\",\"creator\":{creator},\"pages\":[],\"entries\":[")?;
    let mut n = 0;
    for item in entries {
        let (ex, messages) = item?;
        if n > 0 {
            out.write_all(b",\n")?;
        } else {
            out.write_all(b"\n")?;
        }
        serde_json::to_writer(&mut out, &entry(&ex, &messages))?;
        n += 1;
    }
    out.write_all(b"\n]}}\n")?;
    out.flush()?;
    Ok(n)
}

/// One exchange as a HAR entry.
pub fn entry(ex: &Exchange, messages: &[WsMessage]) -> Value {
    let version = if ex.http_version.is_empty() { "HTTP/1.1".to_string() } else { ex.http_version.clone() };
    let mut request = json!({
        "method": ex.method,
        "url": ex.url(),
        "httpVersion": version,
        "cookies": request_cookies(&ex.req_headers),
        "headers": har_headers(&ex.req_headers),
        "queryString": query_params(&ex.query),
        "headersSize": -1,
        "bodySize": ex.req_size.unwrap_or(ex.req_body.len() as i64),
    });
    if !ex.req_body.is_empty() {
        let mime = header(&ex.req_headers, "content-type").unwrap_or("").to_string();
        let mut post = json!({ "mimeType": mime });
        put_text(&mut post, &ex.req_headers, &ex.req_body);
        if mime.to_ascii_lowercase().starts_with("application/x-www-form-urlencoded")
            && let Ok(text) = std::str::from_utf8(&ex.req_body)
        {
            post["params"] = query_params(text);
        }
        request["postData"] = post;
    }

    let decoded = codec::decode_whole(&ex.resp_headers, &ex.resp_body, MAX_DECODED).unwrap_or_else(|| ex.resp_body.clone());
    let mut content = json!({
        "size": decoded.len(),
        "mimeType": header(&ex.resp_headers, "content-type").unwrap_or(""),
    });
    if decoded.len() != ex.resp_body.len() {
        content["compression"] = json!(decoded.len() as i64 - ex.resp_body.len() as i64);
    }
    if !decoded.is_empty() {
        put_text(&mut content, &ex.resp_headers, &decoded);
    }
    let mut response = json!({
        "status": ex.status.unwrap_or(0),
        "statusText": ex.status.and_then(|s| http::StatusCode::from_u16(s).ok()).and_then(|s| s.canonical_reason()).unwrap_or(""),
        "httpVersion": version,
        "cookies": response_cookies(&ex.resp_headers),
        "headers": har_headers(&ex.resp_headers),
        "content": content,
        "redirectURL": header(&ex.resp_headers, "location").unwrap_or(""),
        "headersSize": -1,
        "bodySize": if ex.status.is_some() { ex.resp_len() } else { -1 },
    });
    if let Some(e) = &ex.error {
        response["_error"] = json!(e);
    }

    let mut notes = vec![];
    if ex.req_truncated {
        notes.push(format!("Plonix kept the first {} bytes of the request body", ex.req_body.len()));
    }
    if ex.resp_truncated {
        notes.push(format!("Plonix kept the first {} bytes of the response body", ex.resp_body.len()));
    }
    if ex.edited {
        notes.push("edited in Intercept before it went on".to_string());
    }
    let mut e = json!({
        "startedDateTime": iso_time(ex.ts),
        "time": ex.duration_ms,
        "request": request,
        "response": response,
        "cache": {},
        "timings": { "blocked": -1, "dns": -1, "connect": -1, "ssl": -1, "send": 0, "wait": ex.duration_ms, "receive": 0 },
    });
    if !notes.is_empty() {
        e["comment"] = json!(notes.join("; "));
    }
    if let Some(c) = &ex.client_cert {
        e["_clientCertificate"] = json!(c);
    }
    if !messages.is_empty() {
        e["_webSocketMessages"] = messages
            .iter()
            .map(|m| {
                let opcode = match m.opcode.as_str() {
                    "text" => 1,
                    "binary" => 2,
                    "close" => 8,
                    "ping" => 9,
                    "pong" => 10,
                    _ => 0,
                };
                let data = if m.opcode == "text" { String::from_utf8_lossy(&m.payload).into_owned() } else { STANDARD.encode(&m.payload) };
                json!({
                    "type": if m.direction == "to_server" { "send" } else { "receive" },
                    "time": m.ts as f64 / 1000.0,
                    "opcode": opcode,
                    "data": data,
                })
            })
            .collect();
    }
    e
}

/// Sets `text` (and `encoding: base64` when the body is not text).
fn put_text(obj: &mut Value, headers: &Headers, body: &[u8]) {
    match std::str::from_utf8(body) {
        Ok(text) if codec::is_textual(headers, body) => obj["text"] = json!(text),
        _ => {
            obj["text"] = json!(STANDARD.encode(body));
            obj["encoding"] = json!("base64");
        }
    }
}

fn har_headers(h: &Headers) -> Value {
    h.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect()
}

fn query_params(query: &str) -> Value {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            json!({ "name": percent_decode(k), "value": percent_decode(v) })
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn request_cookies(h: &Headers) -> Value {
    header_all(h, "cookie")
        .flat_map(|v| v.split(';'))
        .filter_map(|c| {
            let (k, v) = c.trim().split_once('=')?;
            Some(json!({ "name": k.trim(), "value": v.trim() }))
        })
        .collect()
}

fn response_cookies(h: &Headers) -> Value {
    header_all(h, "set-cookie")
        .filter_map(|line| {
            let mut parts = line.split(';');
            let (name, value) = parts.next()?.trim().split_once('=')?;
            let mut c = json!({ "name": name.trim(), "value": value.trim() });
            for attr in parts {
                let (k, v) = attr.trim().split_once('=').unwrap_or((attr.trim(), ""));
                match k.to_ascii_lowercase().as_str() {
                    "path" => c["path"] = json!(v),
                    "domain" => c["domain"] = json!(v),
                    "expires" => c["expires"] = json!(v),
                    "httponly" => c["httpOnly"] = json!(true),
                    "secure" => c["secure"] = json!(true),
                    "samesite" => c["sameSite"] = json!(v),
                    _ => {}
                }
            }
            Some(c)
        })
        .collect()
}

/// `2024-05-01T12:30:00.123Z` for a Unix time in milliseconds.
pub fn iso_time(ms: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp_nanos(ms as i128 * 1_000_000).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

/// Reads an ISO 8601 date and time with an offset (`Z`, `+02:00`, `+0200`)
/// as Unix milliseconds. A time without an offset is taken as UTC.
pub fn parse_iso_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let num = |a: usize, b: usize| s.get(a..b).filter(|x| x.bytes().all(|c| c.is_ascii_digit())).and_then(|x| x.parse::<i64>().ok());
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    if !matches!(s.as_bytes().get(10), Some(b'T' | b't' | b' ')) {
        return None;
    }
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    let mut rest = &s[19..];
    let mut nanos = 0i64;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
        rest = &frac[digits.len()..];
        let padded = format!("{:0<9}", &digits[..digits.len().min(9)]);
        nanos = padded.parse().ok()?;
    }
    let offset_s = match rest {
        "" | "Z" | "z" => 0,
        o => {
            let sign = match o.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let o = o[1..].replace(':', "");
            let h: i64 = o.get(0..2)?.parse().ok()?;
            let m: i64 = o.get(2..4).unwrap_or("00").parse().ok()?;
            sign * (h * 3600 + m * 60)
        }
    };
    let month = time::Month::try_from(month as u8).ok()?;
    let date = time::Date::from_calendar_date(year as i32, month, day as u8).ok()?;
    let t = date.with_hms_nano(hour as u8, minute as u8, second as u8, nanos as u32).ok()?.assume_utc();
    Some((t.unix_timestamp_nanos() / 1_000_000) as i64 - offset_s * 1000)
}

// ---- import --------------------------------------------------------------

/// Reads the entries of a HAR file one at a time and hands each to `each`,
/// without holding the whole file in memory. Everything outside
/// `log.entries` is skipped. An error from `each` stops the reading.
pub fn read_entries<R: Read>(reader: R, mut each: impl FnMut(Value) -> Result<()>) -> Result<()> {
    let mut failure: Option<anyhow::Error> = None;
    let mut de = serde_json::Deserializer::from_reader(std::io::BufReader::with_capacity(256 * 1024, reader));
    let mut found = false;
    let result = HarSeed { each: &mut each, failure: &mut failure, found: &mut found }.deserialize(&mut de).and_then(|()| de.end());
    if let Some(f) = failure {
        return Err(f);
    }
    result.map_err(|e| anyhow::anyhow!("this is not a HAR file Plonix can read ({e})"))?;
    anyhow::ensure!(found, "this is not a HAR file: it has no log.entries list");
    Ok(())
}

type Each<'a> = &'a mut dyn FnMut(Value) -> Result<()>;

struct HarSeed<'a, 'b> {
    each: Each<'a>,
    failure: &'b mut Option<anyhow::Error>,
    found: &'b mut bool,
}

impl<'de> DeserializeSeed<'de> for HarSeed<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for HarSeed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a HAR object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "log" {
                map.next_value_seed(LogSeed { each: &mut *self.each, failure: &mut *self.failure, found: &mut *self.found })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct LogSeed<'a, 'b> {
    each: Each<'a>,
    failure: &'b mut Option<anyhow::Error>,
    found: &'b mut bool,
}

impl<'de> DeserializeSeed<'de> for LogSeed<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_map(self)
    }
}

impl<'de> Visitor<'de> for LogSeed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a HAR log object")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
        while let Some(key) = map.next_key::<String>()? {
            if key == "entries" {
                *self.found = true;
                map.next_value_seed(EntriesSeed { each: &mut *self.each, failure: &mut *self.failure })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct EntriesSeed<'a, 'b> {
    each: Each<'a>,
    failure: &'b mut Option<anyhow::Error>,
}

impl<'de> DeserializeSeed<'de> for EntriesSeed<'_, '_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<(), D::Error> {
        d.deserialize_seq(self)
    }
}

impl<'de> Visitor<'de> for EntriesSeed<'_, '_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a list of HAR entries")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
        while let Some(v) = seq.next_element::<Value>()? {
            if let Err(e) = (self.each)(v) {
                *self.failure = Some(e);
                return Err(de::Error::custom("stopped"));
            }
        }
        Ok(())
    }
}

/// One HAR entry as an exchange, with its WebSocket messages. Bodies longer
/// than `body_limit` are kept in part, like captured ones.
pub fn to_exchange(entry: &Value, body_limit: usize) -> Result<(Exchange, Vec<WsMessage>), String> {
    let req = entry.get("request").filter(|r| r.is_object()).ok_or("it has no request")?;
    let url = req.get("url").and_then(Value::as_str).ok_or("its request has no URL")?;
    let (scheme, rest) = url.split_once("://").ok_or_else(|| format!("'{}' is not an absolute URL", clip(url)))?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" && scheme != "ws" && scheme != "wss" {
        return Err(format!("{scheme}:// requests are not HTTP"));
    }
    let scheme = match scheme.as_str() {
        "ws" => "http".to_string(),
        "wss" => "https".to_string(),
        _ => scheme,
    };
    let rest = rest.split('#').next().unwrap_or("");
    let (authority, target) = match rest.find(['/', '?']) {
        Some(i) if rest[i..].starts_with('/') => (&rest[..i], rest[i..].to_string()),
        Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
        None => (rest, "/".to_string()),
    };
    let host = scope::normalize_host(authority);
    if host.is_empty() {
        return Err(format!("'{}' has no host", clip(url)));
    }
    let authority = authority.rsplit_once('@').map(|(_, a)| a).unwrap_or(authority);
    let port = authority
        .rsplit_once(':')
        .filter(|(h, _)| !h.contains(':') || h.ends_with(']'))
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let method = req.get("method").and_then(Value::as_str).unwrap_or("GET").trim().to_ascii_uppercase();
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err("its request method is not valid".into());
    }
    let req_headers = headers_of(req.get("headers"));
    let req_body = match req.get("postData") {
        Some(p) => match p.get("text").and_then(Value::as_str) {
            Some(text) => body_of(text, p.get("encoding").and_then(Value::as_str))?,
            None => form_body(p.get("params")),
        },
        None => vec![],
    };

    let resp = entry.get("response").cloned().unwrap_or(Value::Null);
    let status = resp.get("status").and_then(Value::as_u64).filter(|s| (100..1000).contains(s)).map(|s| s as u16);
    let mut resp_headers = headers_of(resp.get("headers"));
    let content = resp.get("content");
    let resp_body = match content.and_then(|c| c.get("text")).and_then(Value::as_str) {
        Some(text) => body_of(text, content.and_then(|c| c.get("encoding")).and_then(Value::as_str))?,
        None => vec![],
    };
    // HAR content is usually decoded already while the headers still name
    // the encoding. Keep the header only when the body really is encoded.
    if header(&resp_headers, "content-encoding").is_some() && !resp_body.is_empty() && codec::decode_whole(&resp_headers, &resp_body, MAX_DECODED).is_none() {
        resp_headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-encoding"));
    }
    let error = match status {
        Some(_) => None,
        None => Some(resp.get("_error").and_then(Value::as_str).filter(|e| !e.is_empty()).unwrap_or("no response (imported)").to_string()),
    };
    let version = |v: Option<&Value>| match v.and_then(Value::as_str).unwrap_or("").trim().to_ascii_lowercase().as_str() {
        "h2" | "http/2" | "http/2.0" => "HTTP/2".to_string(),
        "h3" | "http/3" | "http/3.0" => "HTTP/3".to_string(),
        "http/1.0" => "HTTP/1.0".to_string(),
        "http/1.1" | "h1" => "HTTP/1.1".to_string(),
        _ => String::new(),
    };
    let http_version = Some(version(resp.get("httpVersion"))).filter(|v| !v.is_empty()).unwrap_or_else(|| version(req.get("httpVersion")));
    let ts = entry.get("startedDateTime").and_then(Value::as_str).and_then(parse_iso_time).unwrap_or_else(now_ms);
    let duration_ms = entry.get("time").and_then(Value::as_f64).filter(|t| t.is_finite() && *t >= 0.0).map(|t| t.round() as i64).unwrap_or(0);

    let (req_body, req_truncated, req_size) = cut(req_body, body_limit);
    let (resp_body, resp_truncated, resp_size) = cut(resp_body, body_limit);
    let ex = Exchange {
        ts,
        scheme,
        host,
        port,
        method,
        path,
        query,
        req_headers,
        req_body,
        status,
        resp_headers,
        resp_body,
        duration_ms,
        error,
        source: Some(Source::Import),
        initiator: Some("har".into()),
        req_truncated,
        req_size,
        resp_truncated,
        resp_size,
        http_version,
        ..Default::default()
    };
    let messages = ws_messages(entry.get("_webSocketMessages"), ts, body_limit);
    Ok((ex, messages))
}

fn clip(s: &str) -> String {
    if s.chars().count() > 80 { format!("{}…", s.chars().take(80).collect::<String>()) } else { s.to_string() }
}

fn cut(mut body: Vec<u8>, limit: usize) -> (Vec<u8>, bool, Option<i64>) {
    if body.len() <= limit {
        return (body, false, None);
    }
    let size = body.len() as i64;
    body.truncate(limit);
    (body, true, Some(size))
}

fn headers_of(v: Option<&Value>) -> Headers {
    v.and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|h| Some((h.get("name")?.as_str()?.to_string(), h.get("value").and_then(Value::as_str).unwrap_or("").to_string())))
                // HTTP/2 pseudo-headers are not headers.
                .filter(|(k, _)| !k.is_empty() && !k.starts_with(':'))
                .collect()
        })
        .unwrap_or_default()
}

fn body_of(text: &str, encoding: Option<&str>) -> Result<Vec<u8>, String> {
    match encoding.map(str::to_ascii_lowercase).as_deref() {
        Some("base64") => STANDARD.decode(text.trim()).map_err(|_| "a body marked base64 is not valid base64".to_string()),
        _ => Ok(text.as_bytes().to_vec()),
    }
}

fn form_body(params: Option<&Value>) -> Vec<u8> {
    let enc = crate::client::encode;
    params
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|p| Some(format!("{}={}", enc(p.get("name")?.as_str()?), enc(p.get("value").and_then(Value::as_str).unwrap_or("")))))
                .collect::<Vec<_>>()
                .join("&")
                .into_bytes()
        })
        .unwrap_or_default()
}

fn ws_messages(v: Option<&Value>, started: i64, limit: usize) -> Vec<WsMessage> {
    let Some(list) = v.and_then(Value::as_array) else { return vec![] };
    list.iter()
        .filter_map(|m| {
            let opcode = match m.get("opcode").and_then(Value::as_u64)? {
                1 => "text",
                2 => "binary",
                8 => "close",
                9 => "ping",
                10 => "pong",
                _ => return None,
            };
            let data = m.get("data").and_then(Value::as_str).unwrap_or("");
            let payload = if opcode == "text" { data.as_bytes().to_vec() } else { STANDARD.decode(data).unwrap_or_else(|_| data.as_bytes().to_vec()) };
            let size = payload.len() as i64;
            let (payload, truncated, _) = cut(payload, limit);
            Some(WsMessage {
                id: 0,
                exchange_id: 0,
                ts: m.get("time").and_then(Value::as_f64).map(|t| (t * 1000.0) as i64).unwrap_or(started),
                direction: if m.get("type").and_then(Value::as_str) == Some("send") { "to_server" } else { "to_client" }.into(),
                opcode: opcode.into(),
                payload,
                size,
                truncated,
            })
        })
        .collect()
}

/// Opens a HAR file for import, for a clear error when it cannot be read.
pub fn open(path: &std::path::Path) -> Result<std::fs::File> {
    std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_round_trip() {
        let ms = 1_714_566_600_123;
        assert_eq!(iso_time(ms), "2024-05-01T12:30:00.123Z");
        assert_eq!(parse_iso_time("2024-05-01T12:30:00.123Z"), Some(ms));
        assert_eq!(parse_iso_time("2024-05-01T14:30:00.123+02:00"), Some(ms));
        assert_eq!(parse_iso_time("2024-05-01T12:30:00.123456789Z"), Some(ms));
        assert_eq!(parse_iso_time("2024-05-01T12:30:00Z"), Some(ms - 123));
        assert_eq!(parse_iso_time("yesterday"), None);
    }

    #[test]
    fn percent_decoding_is_lenient() {
        assert_eq!(percent_decode("a%20b+c%2"), "a b c%2");
        assert_eq!(percent_decode("%zz%41"), "%zzA");
    }

    #[test]
    fn entries_are_read_one_by_one_and_bad_files_are_refused() {
        let file = r#"{"log": {"version": "1.2", "creator": {"name": "x"}, "pages": [{"id": "p"}],
            "entries": [{"request": {"url": "https://a.test/x?q=1", "method": "get", "headers": [{"name": ":authority", "value": "a.test"}, {"name": "Accept", "value": "*/*"}]},
                         "response": {"status": 200, "httpVersion": "h2", "headers": [{"name": "Content-Encoding", "value": "gzip"}], "content": {"text": "hello", "mimeType": "text/plain"}},
                         "startedDateTime": "2024-05-01T12:30:00.123Z", "time": 12.6},
                        {"request": {"url": "ftp://a.test/"}, "response": {}}]}}"#;
        let mut seen = vec![];
        read_entries(file.as_bytes(), |v| {
            seen.push(to_exchange(&v, 1024));
            Ok(())
        })
        .unwrap();
        assert_eq!(seen.len(), 2);
        let (ex, _) = seen[0].as_ref().unwrap();
        assert_eq!((ex.method.as_str(), ex.host.as_str(), ex.port, ex.path.as_str(), ex.query.as_str()), ("GET", "a.test", 443, "/x", "q=1"));
        assert_eq!((ex.status, ex.duration_ms, ex.ts, ex.http_version.as_str()), (Some(200), 13, 1_714_566_600_123, "HTTP/2"));
        assert_eq!(ex.req_headers, vec![("Accept".to_string(), "*/*".to_string())]);
        assert!(ex.resp_headers.is_empty(), "a decoded body loses its content-encoding header");
        assert_eq!(ex.source, Some(Source::Import));
        assert!(seen[1].as_ref().unwrap_err().contains("not HTTP"));
        assert!(read_entries(&b"{\"log\": {}}"[..], |_| Ok(())).unwrap_err().to_string().contains("no log.entries"));
        assert!(read_entries(&b"[1, 2]"[..], |_| Ok(())).is_err());
        assert!(read_entries(&b"{\"log\": {\"entries\": [{}"[..], |_| Ok(())).is_err());
        let stop = read_entries(file.as_bytes(), |_| anyhow::bail!("disk full")).unwrap_err();
        assert_eq!(stop.to_string(), "disk full");
    }

    #[test]
    fn binary_bodies_go_out_in_base64_and_come_back() {
        let ex = Exchange {
            ts: 1_714_566_600_000,
            scheme: "https".into(),
            host: "a.test".into(),
            port: 8443,
            method: "POST".into(),
            path: "/up".into(),
            query: "a=1&b=two%20words".into(),
            req_headers: vec![("Content-Type".into(), "application/octet-stream".into()), ("Cookie".into(), "sid=1; theme=dark".into())],
            req_body: vec![0, 159, 146, 150],
            status: Some(201),
            resp_headers: vec![("Content-Type".into(), "image/png".into()), ("Set-Cookie".into(), "sid=2; Path=/; HttpOnly".into())],
            resp_body: b"\x89PNG\r\n".to_vec(),
            duration_ms: 40,
            ..Default::default()
        };
        let e = entry(&ex, &[]);
        assert_eq!(e["request"]["url"], "https://a.test:8443/up?a=1&b=two%20words");
        assert_eq!(e["request"]["queryString"][1], json!({ "name": "b", "value": "two words" }));
        assert_eq!(e["request"]["cookies"][1], json!({ "name": "theme", "value": "dark" }));
        assert_eq!(e["request"]["postData"]["encoding"], "base64");
        assert_eq!(e["response"]["content"]["encoding"], "base64");
        assert_eq!(e["response"]["cookies"][0]["httpOnly"], true);
        assert_eq!(e["response"]["statusText"], "Created");
        let (back, _) = to_exchange(&e, 1 << 20).unwrap();
        assert_eq!((back.req_body, back.resp_body, back.port, back.ts), (ex.req_body, ex.resp_body, 8443, ex.ts));
    }
}
