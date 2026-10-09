//! Callbacks: the out-of-band listener and its payloads.

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/callbacks", get(callbacks_get))
        .route("/api/callbacks/start", post(callbacks_start))
        .route("/api/callbacks/stop", post(callbacks_stop))
        .route("/api/callbacks/clear", post(callbacks_clear))
        .route("/api/callbacks/config", put(callbacks_config))
        .route("/api/callbacks/payloads", post(callbacks_new_payload))
        .route("/api/callbacks/payloads/{id}", axum::routing::patch(callbacks_rename_payload).delete(callbacks_remove_payload))
}

/// The Callbacks screen is the user's alone, and only once its tool is
/// installed from the Market.
fn callbacks_gate(s: &AppState, caller: &MaybeCaller) -> Option<Response> {
    if let Some(r) = user_only(caller) {
        return Some(r);
    }
    (!crate::tool::ToolLibrary::new(&s.home).enabled_features().contains("callbacks"))
        .then(|| err(StatusCode::NOT_FOUND, "not_installed", "install Callbacks from the Market first"))
}

fn callbacks_view(s: &AppState, since: u64) -> Value {
    let cb = &s.engine.callbacks;
    if let Err(e) = cb.persist(&s.engine.store) {
        tracing::warn!("callbacks: could not save: {e:#}");
    }
    let mut v = cb.snapshot(since);
    v["installed"] = json!(crate::callbacks::locate().is_some());
    v["install"] = json!(crate::callbacks::INSTALL);
    v["homepage"] = json!(crate::callbacks::HOMEPAGE);
    v["config"] = crate::callbacks::Config::load(&s.home).public();
    v
}

#[derive(Deserialize)]
struct SinceParams {
    #[serde(default)]
    since: u64,
}

async fn callbacks_get(State(s): State<AppState>, caller: MaybeCaller, Query(p): Query<SinceParams>) -> Response {
    if let Some(r) = callbacks_gate(&s, &caller) {
        return r;
    }
    Json(callbacks_view(&s, p.since)).into_response()
}

/// Starts listening. Registers with the callback server; sends nothing to
/// any target.
async fn callbacks_start(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    if let Some(r) = callbacks_gate(&s, &caller) {
        return r;
    }
    let Some(exe) = crate::callbacks::locate() else {
        return err(
            StatusCode::BAD_REQUEST,
            "not_installed",
            &format!("{} is not installed. Install it with `{}`, then start again.", crate::callbacks::PROGRAM, crate::callbacks::INSTALL),
        );
    };
    crate::usage::record("callbacks_start");
    let project = s.engine.project_ref.get().map(|p| p.id.clone()).unwrap_or_else(|| s.engine.project.clone());
    let session = crate::callbacks::session_path(&s.home, &project);
    let config = crate::callbacks::Config::load(&s.home);
    match s.engine.callbacks.start(&exe, &config, &session) {
        Ok(()) => Json(callbacks_view(&s, u64::MAX)).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, "start_failed", &e),
    }
}

async fn callbacks_stop(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    if let Some(r) = callbacks_gate(&s, &caller) {
        return r;
    }
    let cb = s.engine.callbacks.clone();
    let _ = tokio::task::spawn_blocking(move || cb.stop()).await;
    Json(callbacks_view(&s, u64::MAX)).into_response()
}

async fn callbacks_clear(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    if let Some(r) = callbacks_gate(&s, &caller) {
        return r;
    }
    s.engine.callbacks.clear();
    Json(callbacks_view(&s, 0)).into_response()
}

#[derive(Deserialize)]
struct CallbacksConfigBody {
    #[serde(default)]
    server: String,
    /// A new token; left out keeps the saved one.
    #[serde(default)]
    token: Option<String>,
}

/// Which callback server to use. Takes effect the next time listening starts.
async fn callbacks_config(State(s): State<AppState>, caller: MaybeCaller, Json(b): Json<CallbacksConfigBody>) -> Response {
    if let Some(r) = callbacks_gate(&s, &caller) {
        return r;
    }
    let server = match crate::callbacks::check_server(&b.server) {
        Ok(v) => v,
        Err(e) => return err(StatusCode::BAD_REQUEST, "bad_server", &e),
    };
    let mut config = crate::callbacks::Config::load(&s.home);
    config.server = server;
    if let Some(t) = b.token {
        let t = t.trim();
        if t.len() > 512 || t.chars().any(|c| c.is_control() || c == ' ') {
            return err(StatusCode::BAD_REQUEST, "bad_token", "the token has characters a server token cannot have");
        }
        config.token = t.to_string();
    }
    if let Err(e) = config.save(&s.home) {
        return internal(e);
    }
    Json(callbacks_view(&s, u64::MAX)).into_response()
}

#[derive(Deserialize)]
struct PayloadBody {
    #[serde(default)]
    label: String,
}

async fn callbacks_new_payload(State(s): State<AppState>, caller: MaybeCaller, Json(b): Json<PayloadBody>) -> Response {
    if let Some(r) = callbacks_gate(&s, &caller) {
        return r;
    }
    match s.engine.callbacks.new_payload(&b.label) {
        Ok(p) => {
            let _ = s.engine.callbacks.persist(&s.engine.store);
            Json(p).into_response()
        }
        Err(e) => err(StatusCode::CONFLICT, "not_listening", &e),
    }
}

async fn callbacks_rename_payload(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<String>, Json(b): Json<PayloadBody>) -> Response {
    if let Some(r) = callbacks_gate(&s, &caller) {
        return r;
    }
    if !s.engine.callbacks.rename_payload(&id, &b.label) {
        return err(StatusCode::NOT_FOUND, "not_found", "no such host");
    }
    Json(callbacks_view(&s, u64::MAX)).into_response()
}

async fn callbacks_remove_payload(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<String>) -> Response {
    if let Some(r) = callbacks_gate(&s, &caller) {
        return r;
    }
    if !s.engine.callbacks.remove_payload(&id) {
        return err(StatusCode::NOT_FOUND, "not_found", "no such host");
    }
    Json(callbacks_view(&s, u64::MAX)).into_response()
}
