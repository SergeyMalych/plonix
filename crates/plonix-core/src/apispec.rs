//! API descriptions (OpenAPI 3 and Swagger 2) found in captured traffic.
//!
//! When the app being tested serves a description of its own API, its
//! endpoints are listed next to the ones actually captured, so the parts of
//! the API nobody has visited yet stand out. This only reads responses that
//! are already stored; nothing is sent.

use serde::Serialize;
use serde_json::Value;

use crate::codec;
use crate::model::{Endpoint, Exchange};

/// Most endpoints listed from one description.
const MAX_ENDPOINTS: usize = 2000;
const METHODS: &[&str] = &["get", "put", "post", "delete", "options", "head", "patch", "trace"];

#[derive(Debug, Clone, Serialize)]
pub struct ApiSpec {
    /// The exchange whose response is the description.
    pub source_id: i64,
    pub title: String,
    pub version: String,
    pub scheme: String,
    /// The host the described API is served from.
    pub host: String,
    /// The path every endpoint starts with, without a trailing slash.
    pub base_path: String,
    pub endpoints: Vec<SpecEndpoint>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpecEndpoint {
    pub method: String,
    /// The full path, base path included, with `{name}` placeholders.
    pub path: String,
    pub summary: String,
    /// Parameters as `in:name`, e.g. `query:limit`.
    pub params: Vec<String>,
    /// Whether captured traffic already has a request for it.
    pub visited: bool,
}

/// Reads an exchange's response as an API description, if it is one.
pub fn parse(ex: &Exchange) -> Option<ApiSpec> {
    let text = codec::body_text(&ex.resp_headers, &ex.resp_body)?;
    let head: String = text.chars().take(4000).collect();
    if !(head.contains("\"openapi\"") || head.contains("\"swagger\"")) || !text.contains("\"paths\"") {
        return None;
    }
    let doc: Value = serde_json::from_str(&text).ok()?;
    let paths = doc.get("paths")?.as_object()?;
    let (mut scheme, mut host, mut base) = (ex.scheme.clone(), ex.host.clone(), String::new());
    if doc.get("openapi").is_some() {
        if let Some(url) = doc.pointer("/servers/0/url").and_then(Value::as_str) {
            if let Some((s, rest)) = url.split_once("://") {
                let (authority, path) = rest.split_once('/').map(|(a, p)| (a, format!("/{p}"))).unwrap_or((rest, String::new()));
                let h = authority.rsplit('@').next().unwrap_or(authority).split(':').next().unwrap_or("");
                if !h.is_empty() && !h.contains('{') {
                    host = h.to_ascii_lowercase();
                    scheme = s.to_ascii_lowercase();
                }
                base = path;
            } else if url.starts_with('/') {
                base = url.to_string();
            }
        }
    } else if doc.get("swagger").is_some() {
        if let Some(h) = doc.get("host").and_then(Value::as_str) {
            let h = h.split(':').next().unwrap_or("");
            if !h.is_empty() {
                host = h.to_ascii_lowercase();
            }
        }
        if let Some(s) = doc.pointer("/schemes/0").and_then(Value::as_str) {
            scheme = s.to_ascii_lowercase();
        }
        base = doc.get("basePath").and_then(Value::as_str).unwrap_or("").to_string();
    } else {
        return None;
    }
    if base.contains('{') {
        base.clear();
    }
    let base = base.trim_end_matches('/').to_string();
    let mut endpoints = Vec::new();
    for (path, item) in paths {
        let Some(item) = item.as_object() else { continue };
        let shared = params_of(item.get("parameters"));
        for m in METHODS {
            let Some(op) = item.get(*m) else { continue };
            let mut params = shared.clone();
            for p in params_of(op.get("parameters")) {
                if !params.contains(&p) {
                    params.push(p);
                }
            }
            let summary = ["summary", "operationId", "description"].iter().find_map(|k| op.get(*k).and_then(Value::as_str)).unwrap_or("");
            endpoints.push(SpecEndpoint {
                method: m.to_ascii_uppercase(),
                path: format!("{base}{path}"),
                summary: summary.lines().next().unwrap_or("").chars().take(160).collect(),
                params,
                visited: false,
            });
            if endpoints.len() >= MAX_ENDPOINTS {
                break;
            }
        }
    }
    if endpoints.is_empty() {
        return None;
    }
    let str_at = |p: &str| doc.pointer(p).and_then(Value::as_str).unwrap_or("").chars().take(120).collect::<String>();
    Some(ApiSpec { source_id: ex.id, title: str_at("/info/title"), version: str_at("/info/version"), scheme, host, base_path: base, endpoints })
}

fn params_of(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| Some(format!("{}:{}", p.get("in")?.as_str()?, p.get("name")?.as_str()?)))
                .take(40)
                .collect()
        })
        .unwrap_or_default()
}

/// Marks the endpoints captured traffic already has a request for.
pub fn mark_visited(spec: &mut ApiSpec, seen: &[Endpoint]) {
    for e in &mut spec.endpoints {
        e.visited = seen.iter().any(|s| s.method == e.method && path_matches(&e.path, &s.path));
    }
}

/// Whether a described path such as `/users/{id}/orders` covers a captured
/// (folded) path such as `/users/{id}/orders` or `/users/me/orders`.
pub fn path_matches(template: &str, seen: &str) -> bool {
    let t: Vec<&str> = template.trim_end_matches('/').split('/').collect();
    let s: Vec<&str> = seen.trim_end_matches('/').split('/').collect();
    t.len() == s.len() && t.iter().zip(&s).all(|(a, b)| (a.starts_with('{') && a.ends_with('}') && !b.is_empty()) || a == b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ex(body: &str) -> Exchange {
        Exchange {
            scheme: "https".into(),
            host: "docs.acme.test".into(),
            port: 443,
            method: "GET".into(),
            path: "/openapi.json".into(),
            status: Some(200),
            resp_headers: vec![("Content-Type".into(), "application/json".into())],
            resp_body: body.as_bytes().to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn reads_openapi_3() {
        let body = r#"{"openapi":"3.0.1","info":{"title":"Shop","version":"2"},"servers":[{"url":"https://api.acme.test/v2/"}],
            "paths":{"/orders/{orderId}":{"parameters":[{"in":"path","name":"orderId"}],"get":{"summary":"Get an order"},"delete":{"operationId":"deleteOrder"}},
            "/me":{"get":{"parameters":[{"in":"query","name":"expand"}]}}}}"#;
        let mut spec = parse(&ex(body)).unwrap();
        assert_eq!((spec.host.as_str(), spec.base_path.as_str(), spec.title.as_str()), ("api.acme.test", "/v2", "Shop"));
        assert_eq!(spec.endpoints.len(), 3);
        let get = spec.endpoints.iter().find(|e| e.method == "GET" && e.path == "/v2/orders/{orderId}").unwrap();
        assert_eq!((get.summary.as_str(), get.params.clone()), ("Get an order", vec!["path:orderId".to_string()]));
        let seen = vec![Endpoint { method: "GET".into(), path: "/v2/orders/{id}".into(), requests: 3, statuses: vec![200], params: vec![], sample_id: 1 }];
        mark_visited(&mut spec, &seen);
        let visited: Vec<_> = spec.endpoints.iter().filter(|e| e.visited).map(|e| format!("{} {}", e.method, e.path)).collect();
        assert_eq!(visited, vec!["GET /v2/orders/{orderId}"]);
    }

    #[test]
    fn reads_swagger_2_and_ignores_other_json() {
        let body = r#"{"swagger":"2.0","host":"api.acme.test:8443","basePath":"/api","schemes":["https"],"paths":{"/users":{"post":{}}}}"#;
        let spec = parse(&ex(body)).unwrap();
        assert_eq!((spec.host.as_str(), spec.endpoints[0].path.as_str(), spec.endpoints[0].method.as_str()), ("api.acme.test", "/api/users", "POST"));
        assert!(parse(&ex(r#"{"paths":{"/a":{}},"name":"swagger"}"#)).is_none());
        assert!(parse(&ex("<html>openapi</html>")).is_none());
    }

    #[test]
    fn template_matching() {
        assert!(path_matches("/users/{id}/orders", "/users/me/orders"));
        assert!(path_matches("/users/{id}", "/users/{id}/"));
        assert!(!path_matches("/users/{id}", "/users"));
        assert!(!path_matches("/users/me", "/users/{id}"));
    }
}
