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
        .route("/api/users/found", get(found_users))
        .route("/api/users/found/apply", post(apply_found_user))
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

/// The sessions captured traffic carries, with the saved user each belongs
/// to. User-only: they are credentials.
fn found_sessions(s: &AppState) -> anyhow::Result<(Vec<crate::gather::FoundSession>, Vec<crate::users::SavedUser>)> {
    // Reading the capture is the slow part, and the Lens asks on every request
    // it opens: the answer is kept until traffic, users or scope change, or
    // for half a minute (tokens expire with time).
    static CACHE: std::sync::Mutex<Option<(String, std::time::Instant, Vec<crate::gather::FoundSession>)>> = std::sync::Mutex::new(None);
    let users = s.engine.store.saved_users()?;
    let rules = s.engine.rules();
    let key = format!(
        "{:p}|{}|{}|{}|{}",
        Arc::as_ptr(&s.engine),
        s.engine.store.max_id()?,
        s.engine.store.count()?,
        serde_json::to_string(&users)?,
        serde_json::to_string(&rules)?
    );
    if let Some((k, at, found)) = CACHE.lock().unwrap().as_ref()
        && *k == key
        && at.elapsed() < std::time::Duration::from_secs(30)
    {
        return Ok((found.clone(), users));
    }
    let exchanges = s.engine.store.recent_exchanges(crate::gather::MAX_EXCHANGES)?;
    let found = crate::gather::sessions(&exchanges, &rules, &users, crate::users::now_secs());
    *CACHE.lock().unwrap() = Some((key, std::time::Instant::now(), found.clone()));
    Ok((found, users))
}

#[derive(serde::Deserialize)]
struct FoundQuery {
    /// Only the session this request carried.
    exchange: Option<i64>,
}

async fn found_users(State(s): State<AppState>, caller: MaybeCaller, Query(q): Query<FoundQuery>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let st = s.clone();
    match tokio::task::spawn_blocking(move || found_sessions(&st)).await {
        Ok(Ok((found, _))) => {
            let found: Vec<_> = found.into_iter().filter(|f| q.exchange.is_none_or(|id| f.exchanges.contains(&id))).collect();
            Json(json!({ "sessions": found })).into_response()
        }
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

#[derive(serde::Deserialize)]
struct ApplyFoundBody {
    key: String,
    /// The saved user to add the session to. None makes a new user.
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Keeps a session found in traffic with a saved user, or as a new one.
/// User-only, and it sends nothing.
async fn apply_found_user(State(s): State<AppState>, caller: MaybeCaller, Json(body): Json<ApplyFoundBody>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let st = s.clone();
    let work = move || -> anyhow::Result<Result<serde_json::Value, String>> {
        let (found, _) = found_sessions(&st)?;
        let Some(f) = found.into_iter().find(|f| f.key == body.key) else {
            return Ok(Err("that session is no longer in the traffic".into()));
        };
        st.engine.store.update_saved_users(|users| {
            let now = crate::users::now_secs();
            let (at, added) = match body.user.as_deref().filter(|u| !u.is_empty()) {
                Some(id) => {
                    let Some(at) = users.iter().position(|u| u.id == id) else {
                        return Ok(Err(format!("there is no saved user '{}'", crate::detect::clean(id, 40))));
                    };
                    (at, crate::gather::apply(&mut users[at], &f, now))
                }
                None => {
                    if users.len() >= crate::users::MAX_USERS {
                        return Ok(Err(format!("at most {} saved users", crate::users::MAX_USERS)));
                    }
                    let name = body.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| crate::gather::user_name(&f, users));
                    let note = if f.label.is_empty() { "Found in captured traffic.".to_string() } else { format!("Found in captured traffic: {}.", f.label.chars().take(150).collect::<String>()) };
                    let mut u = crate::users::SavedUser { id: String::new(), name, note, headers: vec![], scoped: vec![], cookies: vec![], keep_fresh: true };
                    let added = crate::gather::apply(&mut u, &f, now);
                    users.push(u);
                    (users.len() - 1, added)
                }
            };
            match crate::users::check(users) {
                Ok(clean) => *users = clean,
                Err(e) => return Ok(Err(e)),
            }
            Ok(Ok(json!({ "user": users[at].id, "added": added, "users": users.clone() })))
        })
    };
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(Ok(v))) => Json(v).into_response(),
        Ok(Ok(Err(e))) => err(StatusCode::BAD_REQUEST, "bad_request", &e),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
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
