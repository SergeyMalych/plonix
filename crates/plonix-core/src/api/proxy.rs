//! Intercept and match-and-replace rules: what the proxy does to traffic as it passes.

use super::*;
use crate::settings;
use crate::replace;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/intercept", get(intercept_state).put(put_intercept))
        .route("/api/intercept/forward-all", post(intercept_forward_all))
        .route("/api/intercept/{id}/forward", post(intercept_forward))
        .route("/api/intercept/{id}/drop", post(intercept_drop))
        .route("/api/replace", get(replace_rules).post(add_replace_rule))
        .route("/api/replace/{id}", axum::routing::patch(edit_replace_rule).delete(delete_replace_rule))
}

fn intercept_view(s: &AppState) -> Value {
    let i = &s.engine.intercept;
    let o = i.options();
    json!({
        "on": i.is_on(),
        "hold": o.hold,
        "filter": o.filter,
        "responses": o.responses,
        "timeout_s": o.timeout_s,
        "seq": i.seq(),
        "queue": i.queue(),
    })
}

/// Whether Intercept is on, its options and the held queue, oldest first.
async fn intercept_state(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    Json(intercept_view(&s)).into_response()
}

#[derive(Deserialize)]
struct InterceptBody {
    #[serde(default)]
    on: Option<bool>,
    #[serde(default)]
    hold: Option<intercept::HoldScope>,
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    responses: Option<bool>,
    #[serde(default)]
    timeout_s: Option<u64>,
}

/// Turns Intercept on or off and changes its options. The options are saved
/// with the project; on/off is not (a project always opens with it off).
async fn put_intercept(State(s): State<AppState>, caller: MaybeCaller, Json(b): Json<InterceptBody>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    if b.on == Some(true) {
        crate::usage::record("intercept_used");
    }
    let current = s.engine.intercept.options();
    let mut values = current.to_values();
    if let Some(h) = b.hold {
        values.insert("hold".into(), json!(if h == intercept::HoldScope::Everything { "everything" } else { "in_scope" }));
    }
    if let Some(f) = b.filter {
        values.insert("filter".into(), json!(f));
    }
    if let Some(r) = b.responses {
        values.insert("responses".into(), json!(r));
    }
    if let Some(t) = b.timeout_s {
        values.insert("timeout_s".into(), json!(t));
    }
    let section = intercept::settings_section();
    let values = match section.check(&Value::Object(values), &current.to_values()) {
        Ok(v) => v,
        Err(problems) => return bad_settings(problems),
    };
    let options = intercept::InterceptOptions::from_values(&values);
    if options != current {
        if let Err(e) = s.engine.set_intercept_options(options) {
            return bad_settings(vec![settings::Problem::new("filter", format!("{e:#}"))]);
        }
        if let Some(mut p) = this_project(&s)
            && let Err(e) = p.save_settings(intercept::SETTINGS_SECTION, values)
        {
            return internal(e);
        }
    }
    let mut released = 0;
    if let Some(on) = b.on {
        released = s.engine.intercept.set_on(on);
    }
    let mut v = intercept_view(&s);
    v["released"] = json!(released);
    Json(v).into_response()
}

#[derive(Deserialize, Default)]
struct ForwardBody {
    /// The item as edited text; leave out to send it on as it was.
    #[serde(default)]
    raw: Option<String>,
}

fn intercept_result(r: Result<(), intercept::InterceptError>) -> Response {
    match r {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e @ intercept::InterceptError::NotFound(_)) => err(StatusCode::NOT_FOUND, "not_found", &e.to_string()),
        Err(e @ intercept::InterceptError::BadEdit(_)) => err(StatusCode::BAD_REQUEST, "bad_edit", &e.to_string()),
    }
}

/// Sends a held item on, edited (`{"raw": "..."}`) or as it was.
async fn intercept_forward(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<u64>, body: axum::body::Bytes) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let b: ForwardBody = if body.is_empty() {
        ForwardBody::default()
    } else {
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(e) => return err(StatusCode::BAD_REQUEST, "bad_request", &format!("{e}")),
        }
    };
    intercept_result(s.engine.intercept.forward(id, b.raw.as_deref()))
}

/// Drops a held item; the client gets an error page.
async fn intercept_drop(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<u64>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    intercept_result(s.engine.intercept.drop_item(id))
}

/// Forwards everything held, unchanged.
async fn intercept_forward_all(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    Json(json!({ "forwarded": s.engine.intercept.forward_all() })).into_response()
}

/// Whether rules apply, and every rule in the order they apply. Rules change
/// live traffic, so they are the user's alone, like Intercept.
async fn replace_rules(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let enabled = this_project(&s).and_then(|p| p.settings(replace::SETTINGS_SECTION).get("enabled").and_then(Value::as_bool)).unwrap_or(true);
    match s.engine.store.replace_rules() {
        Ok(rules) => Json(json!({ "enabled": enabled, "rules": rules })).into_response(),
        Err(e) => internal(e),
    }
}

fn bad_rule(msg: &str) -> Response {
    err(StatusCode::BAD_REQUEST, "bad_rule", msg)
}

/// Adds a rule: `{"target": "request_header", "match": "...", "replace": "...", "regex": false, "in_scope_only": false, "note": ""}`.
async fn add_replace_rule(State(s): State<AppState>, caller: MaybeCaller, Json(b): Json<replace::RuleInput>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let rule = match b.new_rule() {
        Ok(r) => r,
        Err(e) => return bad_rule(&e),
    };
    match s.engine.add_replace_rule(&rule) {
        Ok(rule) => Json(rule).into_response(),
        Err(e) => internal(e),
    }
}

/// Changes some fields of a rule, or switches it on or off (`{"enabled": false}`).
async fn edit_replace_rule(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<i64>, Json(b): Json<replace::RuleInput>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let rule = match s.engine.store.replace_rule(id) {
        Ok(Some(r)) => r,
        Ok(None) => return err(StatusCode::NOT_FOUND, "not_found", &format!("no match-and-replace rule {id}")),
        Err(e) => return internal(e),
    };
    let rule = match b.apply_to(rule) {
        Ok(r) => r,
        Err(e) => return bad_rule(&e),
    };
    match s.engine.update_replace_rule(&rule) {
        Ok(_) => Json(rule).into_response(),
        Err(e) => internal(e),
    }
}

async fn delete_replace_rule(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<i64>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    match s.engine.delete_replace_rule(id) {
        Ok(true) => Json(json!({ "deleted": id })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "not_found", &format!("no match-and-replace rule {id}")),
        Err(e) => internal(e),
    }
}
