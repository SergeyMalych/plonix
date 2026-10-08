//! Data types shared by the engine, the API and its clients.

use serde::{Deserialize, Serialize};

/// Ordered header list. Duplicates and order are preserved as seen on the wire.
pub type Headers = Vec<(String, String)>;

pub fn header<'a>(headers: &'a Headers, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

pub fn header_all<'a>(headers: &'a Headers, name: &'a str) -> impl Iterator<Item = &'a str> + 'a {
    headers
        .iter()
        .filter(move |(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Where an exchange came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// Captured passively by the intercepting proxy.
    Proxy,
    /// Sent actively by the engine on behalf of a user or agent.
    Replay,
    /// Loaded from a HAR file (see [`crate::har`]).
    Import,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Proxy => "proxy",
            Source::Replay => "replay",
            Source::Import => "import",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "replay" => Source::Replay,
            "import" => Source::Import,
            _ => Source::Proxy,
        }
    }
}

/// One full HTTP request/response pair.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Exchange {
    pub id: i64,
    /// Unix time in milliseconds when the request was received.
    pub ts: i64,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub method: String,
    /// Path without the query string.
    pub path: String,
    /// Raw query string without the leading `?` (empty if none).
    pub query: String,
    pub req_headers: Headers,
    #[serde(with = "body_b64")]
    pub req_body: Vec<u8>,
    /// `None` when the upstream could not be reached.
    pub status: Option<u16>,
    pub resp_headers: Headers,
    #[serde(with = "body_b64")]
    pub resp_body: Vec<u8>,
    pub duration_ms: i64,
    pub error: Option<String>,
    /// DNS names from the upstream TLS certificate, if HTTPS.
    pub tls_sans: Vec<String>,
    pub source: Option<Source>,
    /// Who initiated an active request (`cli`, `mcp`, `gui`...).
    pub initiator: Option<String>,
    /// The request body was longer than the recording limit: it went through
    /// in full, and `req_body` holds its start.
    #[serde(default)]
    pub req_truncated: bool,
    /// Full size of a truncated request body, when all of it went through.
    #[serde(default)]
    pub req_size: Option<i64>,
    /// Like `req_truncated`, for the response body.
    #[serde(default)]
    pub resp_truncated: bool,
    #[serde(default)]
    pub resp_size: Option<i64>,
    /// The protocol spoken with the server (`HTTP/1.1`, `HTTP/2`), or with
    /// the client when the server was not reached. Empty in older captures.
    #[serde(default)]
    pub http_version: String,
    /// Changed by hand in Intercept before it went on: the request and
    /// response fields above are what was actually sent.
    #[serde(default)]
    pub edited: bool,
    /// The request as it arrived, in HTTP/1.1 text, when it was edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_request: Option<String>,
    /// The response as the server sent it, when it was edited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_response: Option<String>,
    /// Match-and-replace rules that changed this exchange in the proxy
    /// (`#3 request header: ...`); the fields above are what was sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaced: Vec<String>,
    /// The client certificate Plonix presented to the server when it asked
    /// for one (`CN=alice for *.example.com`), see [`crate::clientcert`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_cert: Option<String>,
}

impl Exchange {
    pub fn url(&self) -> String {
        let default = (self.scheme == "https" && self.port == 443) || (self.scheme == "http" && self.port == 80);
        let mut url = if default {
            format!("{}://{}{}", self.scheme, self.host, self.path)
        } else {
            format!("{}://{}:{}{}", self.scheme, self.host, self.port, self.path)
        };
        if !self.query.is_empty() {
            url.push('?');
            url.push_str(&self.query);
        }
        url
    }

    /// Size of the response body as sent, which is more than was kept when
    /// the body was cut at the recording limit.
    pub fn resp_len(&self) -> i64 {
        self.resp_size.unwrap_or(self.resp_body.len() as i64)
    }

    pub fn mime(&self) -> String {
        header(&self.resp_headers, "content-type")
            .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
            .unwrap_or_default()
    }
}

/// One WebSocket message, captured on the connection a handshake exchange opened.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WsMessage {
    pub id: i64,
    /// The handshake exchange (status 101) this message belongs to.
    pub exchange_id: i64,
    /// Unix time in milliseconds when the message started.
    pub ts: i64,
    /// `to_server` (sent by the client) or `to_client`.
    pub direction: String,
    /// `text`, `binary`, `close`, `ping` or `pong`.
    pub opcode: String,
    /// The message as the application sees it: reassembled from its
    /// fragments, unmasked and decompressed. Cut at the recording limit.
    #[serde(with = "body_b64")]
    pub payload: Vec<u8>,
    /// Full payload size in bytes.
    pub size: i64,
    pub truncated: bool,
}

impl WsMessage {
    /// Readable text of a text or close message.
    pub fn text(&self) -> Option<String> {
        match self.opcode.as_str() {
            "text" => Some(String::from_utf8_lossy(&self.payload).into_owned()),
            "close" if self.payload.len() >= 2 => {
                let code = u16::from_be_bytes([self.payload[0], self.payload[1]]);
                let reason = String::from_utf8_lossy(&self.payload[2..]);
                Some(if reason.is_empty() { code.to_string() } else { format!("{code} {reason}") })
            }
            _ => None,
        }
    }
}

/// Compact row used for listings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExchangeSummary {
    pub id: i64,
    pub ts: i64,
    pub method: String,
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub query: String,
    pub status: Option<u16>,
    pub mime: String,
    pub resp_len: i64,
    pub duration_ms: i64,
    pub source: String,
    pub in_scope: bool,
    #[serde(default)]
    pub edited: bool,
    /// Rules changed it on the way (see [`crate::replace`]).
    #[serde(default)]
    pub replaced: bool,
}

/// What the captured traffic contains, for suggesting filters that matter.
/// Every list is sorted busiest first and only holds values that occur.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Facets {
    /// Exchanges looked at (the most recent ones, up to [`Facets::SAMPLE`]).
    pub sampled: i64,
    pub in_scope: i64,
    pub out_of_scope: i64,
    pub methods: Vec<Count>,
    /// `2xx`..`5xx`, and `none` for requests that got no response.
    pub statuses: Vec<Count>,
    /// Response kinds: json, html, javascript, xml, css, image, font, text.
    pub kinds: Vec<Count>,
    /// In-scope hosts.
    pub hosts: Vec<Count>,
    /// First path segments of in-scope traffic, e.g. `/api`.
    pub paths: Vec<Count>,
    /// Busiest out-of-scope hosts (at most 20), the usual candidates to hide.
    #[serde(default)]
    pub other_hosts: Vec<Count>,
    pub replays: i64,
}

impl Facets {
    pub const SAMPLE: usize = 50_000;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Count {
    pub value: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostSummary {
    pub host: String,
    pub requests: i64,
    pub first_seen: i64,
    pub last_seen: i64,
    pub scope: crate::scope::Decision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub method: String,
    pub path: String,
    pub requests: i64,
    pub statuses: Vec<u16>,
    pub params: Vec<String>,
    pub sample_id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub id: i64,
    pub created_at: i64,
    pub title: String,
    pub severity: String,
    pub status: String,
    pub description: String,
    pub exchange_ids: Vec<i64>,
    pub created_by: String,
    /// When the finding was last edited (its creation time if never).
    #[serde(default)]
    pub updated_at: i64,
}

/// Changes to a finding; fields left out stay as they are.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FindingEdit {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl FindingEdit {
    /// Trims and checks the edit, spelling the status the way it is stored.
    pub fn checked(mut self) -> Result<Self, String> {
        if let Some(t) = &self.title {
            if t.trim().is_empty() {
                return Err("title cannot be empty".into());
            }
            self.title = Some(t.trim().to_string());
        }
        if let Some(s) = &self.severity {
            self.severity = Some(check_severity(s)?);
        }
        if let Some(s) = &self.status {
            self.status = Some(parse_status(s).ok_or_else(|| format!("status must be one of {}", FINDING_STATUSES.join(", ")))?.to_string());
        }
        if self.title.is_none() && self.severity.is_none() && self.status.is_none() && self.description.is_none() {
            return Err("nothing to change: give a title, severity, status or description".into());
        }
        Ok(self)
    }
}

pub const SEVERITIES: &[&str] = &["info", "low", "medium", "high", "critical"];

/// Where a finding stands: `open` until the user has looked at it, then
/// `confirmed` (it is real), `false_positive` (it is not) or `fixed`.
pub const FINDING_STATUSES: &[&str] = &["open", "confirmed", "false_positive", "fixed"];

/// A status as people type it (`false positive`, `false-positive`, `fp`), as stored.
pub fn parse_status(s: &str) -> Option<&'static str> {
    let s = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    let s = if s == "fp" { "false_positive".to_string() } else { s };
    FINDING_STATUSES.iter().find(|x| **x == s).copied()
}

pub fn check_severity(s: &str) -> Result<String, String> {
    let s = s.trim().to_ascii_lowercase();
    if SEVERITIES.contains(&s.as_str()) { Ok(s) } else { Err(format!("severity must be one of {}", SEVERITIES.join(", "))) }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewFinding {
    pub title: String,
    #[serde(default = "default_severity")]
    pub severity: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub exchange_ids: Vec<i64>,
}

fn default_severity() -> String {
    "info".into()
}

/// Bodies travel as base64 in JSON so binary content survives.
mod body_b64 {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
