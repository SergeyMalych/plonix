//! Local HTTP API. Every client (GUI, CLI, MCP) goes through it.
//!
//! Bound to loopback, requires `Authorization: Bearer <token>` (token in
//! `$PLONIX_HOME/api-token`) and a loopback `Host` header, so that web pages
//! cannot drive it through the browser (CSRF / DNS rebinding).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::codec;
use crate::engine::{Engine, ReplayRequest, SendError, SendRequest};
use crate::model::{Exchange, NewFinding, SEVERITIES};
use crate::query;
use crate::scope::Decision;

#[derive(Clone)]
struct AppState {
    engine: Arc<Engine>,
    token: String,
    api_addr: SocketAddr,
    proxy_addr: SocketAddr,
}

pub fn router(engine: Arc<Engine>, token: String, api_addr: SocketAddr, proxy_addr: SocketAddr) -> Router {
    let state = AppState { engine, token, api_addr, proxy_addr };
    Router::new()
        .route("/api/status", get(status))
        .route("/api/traffic", get(traffic))
        .route("/api/traffic/{id}", get(exchange))
        .route("/api/hosts", get(hosts))
        .route("/api/hosts/{host}/endpoints", get(endpoints))
        .route("/api/tech", get(tech_all))
        .route("/api/tech/{host}", get(tech_host))
        .route("/api/rules", get(rule_packs))
        .route("/api/scope", get(scope))
        .route("/api/scope/accept", post(accept))
        .route("/api/scope/reject", post(reject))
        .route("/api/scope/remove", post(remove))
        .route("/api/send", post(send))
        .route("/api/replay", post(replay))
        .route("/api/findings", get(findings).post(add_finding))
        .route("/api/shutdown", post(shutdown))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

async fn guard(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let host = req.headers().get("host").and_then(|h| h.to_str().ok()).unwrap_or("");
    let host_ok = host.rsplit_once(':').is_some_and(|(h, p)| {
        p.parse::<u16>().ok() == Some(s.api_addr.port()) && matches!(h, "127.0.0.1" | "localhost" | "[::1]")
    });
    if !host_ok {
        return err(StatusCode::FORBIDDEN, "bad_host", "requests must target the loopback API address");
    }
    let auth = req.headers().get("authorization").and_then(|h| h.to_str().ok()).unwrap_or("");
    let ok = auth.strip_prefix("Bearer ").is_some_and(|t| constant_eq(t.trim().as_bytes(), s.token.as_bytes()));
    if !ok {
        return err(StatusCode::UNAUTHORIZED, "unauthorized", "missing or wrong API token (see $PLONIX_HOME/api-token)");
    }
    next.run(req).await
}

fn constant_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn err(status: StatusCode, code: &str, msg: &str) -> Response {
    (status, Json(json!({ "error": msg, "code": code }))).into_response()
}

fn internal(e: anyhow::Error) -> Response {
    err(StatusCode::INTERNAL_SERVER_ERROR, "internal", &format!("{e:#}"))
}

fn initiator(h: &HeaderMap) -> String {
    h.get("x-plonix-client").and_then(|v| v.to_str().ok()).unwrap_or("api").chars().take(32).collect()
}

async fn status(State(s): State<AppState>) -> Response {
    let rules = s.engine.rules();
    let pending = s.engine.store.suggestions(&rules).map(|v| v.len()).unwrap_or(0);
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "project": s.engine.project,
        "proxy": s.proxy_addr.to_string(),
        "api": s.api_addr.to_string(),
        "pid": std::process::id(),
        "started_at": s.engine.started_at,
        "exchanges": s.engine.store.count().unwrap_or(0),
        "scope_rules": rules.rules.len(),
        "pending_suggestions": pending,
        "ca_fingerprint": s.engine.ca.fingerprint(),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct TrafficParams {
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_limit() -> usize {
    100
}

async fn traffic(State(s): State<AppState>, Query(p): Query<TrafficParams>) -> Response {
    let q = match query::Query::parse(&p.q) {
        Ok(q) => q,
        Err(e) => return err(StatusCode::BAD_REQUEST, "bad_query", &e.to_string()),
    };
    match s.engine.store.search(&q, &s.engine.rules(), p.limit.min(5000), p.offset) {
        Ok((items, total)) => Json(json!({ "total": total, "items": items })).into_response(),
        Err(e) => internal(e),
    }
}

/// Full exchange plus decoded text bodies for display.
#[derive(Serialize)]
pub struct ExchangeView {
    #[serde(flatten)]
    pub exchange: Exchange,
    pub url: String,
    pub in_scope: bool,
    pub req_text: Option<String>,
    pub resp_text: Option<String>,
}

pub fn view(ex: Exchange, in_scope: bool) -> ExchangeView {
    ExchangeView {
        url: ex.url(),
        in_scope,
        req_text: codec::body_text(&ex.req_headers, &ex.req_body),
        resp_text: codec::body_text(&ex.resp_headers, &ex.resp_body),
        exchange: ex,
    }
}

async fn exchange(State(s): State<AppState>, Path(id): Path<i64>) -> Response {
    match s.engine.store.get_exchange(id) {
        Ok(Some(ex)) => {
            let in_scope = s.engine.rules().in_scope(&ex.host);
            Json(view(ex, in_scope)).into_response()
        }
        Ok(None) => err(StatusCode::NOT_FOUND, "not_found", &format!("exchange {id} not found")),
        Err(e) => internal(e),
    }
}

async fn hosts(State(s): State<AppState>) -> Response {
    match s.engine.store.hosts(&s.engine.rules()) {
        Ok(h) => Json(h).into_response(),
        Err(e) => internal(e),
    }
}

async fn endpoints(State(s): State<AppState>, Path(host): Path<String>) -> Response {
    match s.engine.store.endpoints(&host) {
        Ok(e) => Json(e).into_response(),
        Err(e) => internal(e),
    }
}

async fn tech_all(State(s): State<AppState>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.detect_all()).await {
        Ok(Ok(hosts)) => Json(hosts).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn tech_host(State(s): State<AppState>, Path(host): Path<String>) -> Response {
    let engine = s.engine.clone();
    let h = host.clone();
    match tokio::task::spawn_blocking(move || engine.detect_host(&h)).await {
        Ok(Ok(tech)) => Json(json!({ "host": host.to_ascii_lowercase(), "tech": tech })).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn rule_packs(State(s): State<AppState>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.detection_rules()).await {
        Ok(r) => Json(json!({ "packs": r.packs, "rules": r.detector.rules.len(), "problems": r.problems })).into_response(),
        Err(e) => internal(e.into()),
    }
}

async fn scope(State(s): State<AppState>) -> Response {
    let rules = s.engine.rules();
    match s.engine.store.suggestions(&rules) {
        Ok(sug) => Json(json!({ "rules": rules.rules, "suggestions": sug })).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct DomainBody {
    domain: String,
    #[serde(default)]
    include_subdomains: bool,
    #[serde(default)]
    note: String,
}

async fn decide(s: AppState, b: DomainBody, d: Decision) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.decide(&b.domain, d, b.include_subdomains, &b.note)).await {
        Ok(Ok(rule)) => Json(rule).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "bad_domain", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

async fn accept(State(s): State<AppState>, Json(b): Json<DomainBody>) -> Response {
    decide(s, b, Decision::Accepted).await
}

async fn reject(State(s): State<AppState>, Json(b): Json<DomainBody>) -> Response {
    decide(s, b, Decision::Rejected).await
}

async fn remove(State(s): State<AppState>, Json(b): Json<DomainBody>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.remove_rule(&b.domain)).await {
        Ok(Ok(removed)) => Json(json!({ "removed": removed })).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

fn send_result(s: &AppState, r: Result<Exchange, SendError>) -> Response {
    match r {
        Ok(ex) => {
            let in_scope = s.engine.rules().in_scope(&ex.host);
            Json(view(ex, in_scope)).into_response()
        }
        Err(e @ SendError::OutOfScope { .. }) => err(StatusCode::FORBIDDEN, "out_of_scope", &e.to_string()),
        Err(e @ SendError::BadRequest(_)) => err(StatusCode::BAD_REQUEST, "bad_request", &e.to_string()),
        Err(e @ SendError::NotFound(_)) => err(StatusCode::NOT_FOUND, "not_found", &e.to_string()),
        Err(SendError::Other(e)) => internal(e),
    }
}

async fn send(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<SendRequest>) -> Response {
    let r = s.engine.send(req, &initiator(&headers)).await;
    send_result(&s, r)
}

async fn replay(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<ReplayRequest>) -> Response {
    let r = s.engine.replay(req, &initiator(&headers)).await;
    send_result(&s, r)
}

async fn findings(State(s): State<AppState>) -> Response {
    match s.engine.store.findings() {
        Ok(f) => Json(f).into_response(),
        Err(e) => internal(e),
    }
}

async fn add_finding(State(s): State<AppState>, headers: HeaderMap, Json(f): Json<NewFinding>) -> Response {
    if f.title.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "bad_request", "title is required");
    }
    if !SEVERITIES.contains(&f.severity.as_str()) {
        return err(StatusCode::BAD_REQUEST, "bad_request", &format!("severity must be one of {}", SEVERITIES.join(", ")));
    }
    match s.engine.store.add_finding(&f, &initiator(&headers)) {
        Ok(f) => (StatusCode::CREATED, Json(f)).into_response(),
        Err(e) => internal(e),
    }
}

async fn shutdown(State(s): State<AppState>) -> Response {
    s.engine.shutdown.notify_waiters();
    Json(Value::from(json!({ "ok": true }))).into_response()
}
