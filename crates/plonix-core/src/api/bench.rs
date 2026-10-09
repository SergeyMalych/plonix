//! Bench: send, replay and payload runs, saved users and the access check.

use super::*;
use crate::engine::{ReplayRequest, SendRequest};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/send", post(send))
        .route("/api/replay", post(replay))
        .route("/api/run", post(run))
        .route("/api/run/lists", get(run_lists))
        .route("/api/users", get(saved_users).put(set_saved_users))
        .route("/api/users/acting", put(set_acting_user))
        .route("/api/access-check", post(access_check))
}

fn send_result(s: &AppState, r: Result<Exchange, SendError>) -> Response {
    match r {
        Ok(ex) => {
            let in_scope = s.engine.rules().in_scope(&ex.host);
            Json(view(ex, in_scope)).into_response()
        }
        Err(e) => send_error(e),
    }
}

async fn send(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<SendRequest>) -> Response {
    crate::usage::record("bench_send");
    let r = s.engine.send(req, &initiator(&headers)).await;
    send_result(&s, r)
}

async fn replay(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<ReplayRequest>) -> Response {
    crate::usage::record("bench_send");
    let r = s.engine.replay(req, &initiator(&headers)).await;
    send_result(&s, r)
}

/// Runs payloads through the marked positions of a request. User-only: this
/// route is in no agent mode's capabilities, so agents cannot start a run.
async fn run(State(s): State<AppState>, headers: HeaderMap, Json(req): Json<crate::runs::RunRequest>) -> Response {
    crate::usage::record("bench_run");
    match s.engine.run(req, &initiator(&headers)).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => send_error(e),
    }
}

/// The payload lists available for a run: the built-in ones plus any installed
/// from the Market.
async fn run_lists(State(s): State<AppState>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.lists()).await {
        Ok(set) => {
            // Each list carries a small sample of its values so the Bench can
            // preview what a list holds without fetching all of it.
            let lists: Vec<_> = set
                .catalog()
                .into_iter()
                .map(|l| {
                    let sample: Vec<&String> = set.values.get(&l.id).map(|v| v.iter().take(6).collect()).unwrap_or_default();
                    json!({ "id": l.id, "title": l.title, "description": l.description, "count": l.count, "pack": l.pack, "builtin": l.builtin, "sample": sample })
                })
                .collect();
            Json(json!({ "lists": lists, "packs": set.packs, "problems": set.problems })).into_response()
        }
        Err(e) => internal(e.into()),
    }
}

#[derive(serde::Deserialize)]
struct SavedUsersBody {
    users: Vec<crate::users::SavedUser>,
}

/// The saved users for this project. User-only: these carry credentials, and
/// agents are refused here as they are for everything that is not read-only.
async fn saved_users(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let acting = s.engine.store.acting_user().ok().flatten().map(|u| u.id);
    match s.engine.store.saved_users() {
        Ok(users) => Json(json!({ "users": users, "acting": acting })).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(serde::Deserialize)]
struct ActingBody {
    id: Option<String>,
}

/// Picks the saved user the person acts as (or none: the browser's own
/// session). User-only. Changing it sends nothing by itself.
async fn set_acting_user(State(s): State<AppState>, caller: MaybeCaller, Json(body): Json<ActingBody>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let id = body.id.filter(|i| !i.is_empty());
    if let Some(id) = &id {
        match s.engine.store.saved_users() {
            Ok(users) if users.iter().any(|u| &u.id == id) => {}
            Ok(_) => return err(StatusCode::BAD_REQUEST, "bad_request", &format!("there is no saved user '{}'", crate::detect::clean(id, 40))),
            Err(e) => return internal(e),
        }
    }
    match s.engine.store.set_acting_user(id.as_deref()) {
        Ok(()) => Json(json!({ "acting": id })).into_response(),
        Err(e) => internal(e),
    }
}

async fn set_saved_users(State(s): State<AppState>, caller: MaybeCaller, Json(body): Json<SavedUsersBody>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let clean = match crate::users::check(&body.users) {
        Ok(u) => u,
        Err(e) => return err(StatusCode::BAD_REQUEST, "bad_request", &e),
    };
    match s.engine.store.set_saved_users(&clean) {
        Ok(()) => Json(json!({ "users": clean })).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(serde::Deserialize)]
struct AccessCheckBody {
    #[serde(default)]
    targets: Vec<i64>,
    /// A whole branch of an application: every endpoint on this host whose
    /// path starts with `prefix` is checked, one captured request each.
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    prefix: Option<String>,
    /// Which saved users to replay as. Empty means all of them.
    #[serde(default)]
    user_ids: Vec<String>,
    #[serde(default = "default_true")]
    include_anon: bool,
    #[serde(default)]
    delay_ms: Option<u64>,
}

fn default_true() -> bool {
    true
}

/// Replays the chosen requests as each saved user and once signed out.
/// User-only, and every replay is scope-gated in the engine.
async fn access_check(State(s): State<AppState>, caller: MaybeCaller, headers: HeaderMap, Json(body): Json<AccessCheckBody>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    crate::usage::record("access_check");
    // Resolve a host+prefix branch to one representative request per endpoint.
    let mut targets = body.targets;
    if targets.is_empty()
        && let Some(host) = body.host.as_deref()
    {
        let prefix = body.prefix.as_deref().unwrap_or("/");
        match s.engine.store.endpoints(host) {
            Ok(eps) => targets = eps.into_iter().filter(|e| e.path.starts_with(prefix)).map(|e| e.sample_id).collect(),
            Err(e) => return internal(e),
        }
    }
    let all = match s.engine.store.saved_users() {
        Ok(u) => u,
        Err(e) => return internal(e),
    };
    let users = if body.user_ids.is_empty() { all } else { all.into_iter().filter(|u| body.user_ids.contains(&u.id)).collect() };
    let req = crate::authcheck::AuthCheckRequest { targets, users, include_anon: body.include_anon, delay_ms: body.delay_ms };
    match s.engine.access_check(req, &initiator(&headers)).await {
        Ok(report) => Json(report).into_response(),
        Err(e) => send_error(e),
    }
}
