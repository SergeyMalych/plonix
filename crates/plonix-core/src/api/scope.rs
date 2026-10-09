//! Scope: accept, reject and remove domains, and the exclusion groups.

use super::*;
use crate::scope::Decision;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/scope", get(scope))
        .route("/api/scope/accept", post(accept))
        .route("/api/scope/reject", post(reject))
        .route("/api/scope/remove", post(remove))
        .route("/api/scope/exclusions", get(exclusions))
        .route("/api/scope/exclusions/group", post(exclude_group))
        .route("/api/scope/exclusions/domain", post(exclude_domain))
        .route("/api/scope/exclusions/custom", post(save_custom_group).delete(delete_custom_group))
        .route("/api/scope/exclusions/asked", post(exclusions_asked))
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

async fn exclusions(State(s): State<AppState>) -> Response {
    match s.engine.exclusions() {
        Ok(ex) => Json(ex).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(serde::Deserialize)]
struct GroupToggle {
    id: String,
    on: bool,
}

async fn exclude_group(State(s): State<AppState>, Json(b): Json<GroupToggle>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.set_group_excluded(&b.id, b.on).and_then(|()| engine.exclusions())).await {
        Ok(Ok(ex)) => Json(ex).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "bad_request", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

#[derive(serde::Deserialize)]
struct DomainToggle {
    id: String,
    host: String,
    on: bool,
}

async fn exclude_domain(State(s): State<AppState>, Json(b): Json<DomainToggle>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.set_domain_excluded(&b.id, &b.host, b.on).and_then(|()| engine.exclusions())).await {
        Ok(Ok(ex)) => Json(ex).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "bad_request", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

async fn save_custom_group(State(s): State<AppState>, Json(g): Json<crate::exclude::Group>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.save_custom_group(g).and_then(|saved| Ok((saved, engine.exclusions()?)))).await {
        Ok(Ok((saved, ex))) => Json(json!({ "group": saved, "exclusions": ex })).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "bad_request", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

#[derive(serde::Deserialize)]
struct IdBody {
    id: String,
}

async fn delete_custom_group(State(s): State<AppState>, Json(b): Json<IdBody>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.remove_custom_group(&b.id).and_then(|()| engine.exclusions())).await {
        Ok(Ok(ex)) => Json(ex).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn exclusions_asked(State(s): State<AppState>) -> Response {
    match s.engine.mark_exclusions_asked() {
        Ok(()) => Json(json!({ "asked": true })).into_response(),
        Err(e) => internal(e),
    }
}
