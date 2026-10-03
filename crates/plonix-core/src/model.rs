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
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Proxy => "proxy",
            Source::Replay => "replay",
        }
    }
    pub fn parse(s: &str) -> Self {
        if s == "replay" { Source::Replay } else { Source::Proxy }
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

    pub fn mime(&self) -> String {
        header(&self.resp_headers, "content-type")
            .map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
            .unwrap_or_default()
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

pub const SEVERITIES: &[&str] = &["info", "low", "medium", "high", "critical"];

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
