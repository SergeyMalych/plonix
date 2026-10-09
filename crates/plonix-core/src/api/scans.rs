//! Scans and the crawler.

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/scan/catalog", get(scan_catalog))
        .route("/api/scan/suggest/{host}", get(scan_suggest))
        .route("/api/scan/plan/{host}", get(scan_plan))
        .route("/api/scan/estimate", post(scan_estimate))
        .route("/api/scan", post(scan_run))
        .route("/api/crawl", post(crawl_run))
}

async fn scan_catalog(State(s): State<AppState>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.scan_catalog().describe()).await {
        Ok(view) => Json(view).into_response(),
        Err(e) => internal(e.into()),
    }
}

async fn scan_suggest(State(s): State<AppState>, caller: MaybeCaller, Path(host): Path<String>) -> Response {
    if agent_in_scope_only(&s, &caller) && !s.engine.rules().in_scope(&host) {
        return outside_agent_data();
    }
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.scan_suggest(&host)).await {
        Ok(Ok(suggestion)) => Json(suggestion).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn scan_plan(State(s): State<AppState>, caller: MaybeCaller, Path(host): Path<String>) -> Response {
    if agent_in_scope_only(&s, &caller) && !s.engine.rules().in_scope(&host) {
        return outside_agent_data();
    }
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.scan_plan(&host)).await {
        Ok(Ok(plan)) => Json(plan).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn scan_run(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<crate::scan::ScanRequest>) -> Response {
    crate::usage::record("scan_run");
    match s.engine.scan(req, &initiator(&headers)).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => send_error(e),
    }
}

async fn scan_estimate(State(s): State<AppState>, Json(req): Json<crate::scan::ScanRequest>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.scan_estimate(&req)).await {
        Ok(Ok(requests)) => Json(serde_json::json!({ "requests": requests })).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn crawl_run(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<crate::crawl::CrawlRequest>) -> Response {
    crate::usage::record("crawl_run");
    match s.engine.crawl(req, &initiator(&headers)).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => send_error(e),
    }
}
