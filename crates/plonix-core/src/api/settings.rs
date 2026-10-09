//! Settings, storage, saved sessions and client certificates.

use super::*;
use crate::settings::{self, Level};
use crate::clientcert::{self, CertInput};
use crate::model::now_ms;
use crate::replace;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/client-certs", get(client_certs).post(add_client_cert))
        .route("/api/client-certs/{id}", axum::routing::delete(delete_client_cert))
        .route("/api/settings", get(get_settings))
        .route("/api/settings/{section}", put(put_settings))
        .route("/api/storage", get(storage))
        .route("/api/storage/prune", post(prune))
        .route("/api/sessions", get(sessions))
}

/// Whether certificates are presented, and each one described (never its
/// key). Like match and replace, this is the user's alone.
async fn client_certs(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let enabled = this_project(&s).and_then(|p| p.settings(clientcert::SETTINGS_SECTION).get("enabled").and_then(Value::as_bool)).unwrap_or(true);
    match s.engine.client_cert_list() {
        Ok(certs) => Json(json!({ "enabled": enabled, "certs": certs })).into_response(),
        Err(e) => internal(e),
    }
}

/// Adds a certificate: `{"host": "*.example.com", "cert_pem": "...", "key_pem": "..."}`,
/// or `{"host": ..., "pkcs12_base64": "...", "password": "..."}`.
async fn add_client_cert(State(s): State<AppState>, caller: MaybeCaller, Json(b): Json<CertInput>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let engine = s.engine.clone();
    let done = tokio::task::spawn_blocking(move || match b.into_stored(now_ms()) {
        Ok(cert) => engine.add_client_cert(&cert).map(Ok),
        Err(e) => Ok(Err(e)),
    })
    .await;
    match done {
        Ok(Ok(Ok(info))) => Json(info).into_response(),
        Ok(Ok(Err(e))) => err(StatusCode::BAD_REQUEST, "bad_cert", &e),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn delete_client_cert(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<i64>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    match s.engine.remove_client_cert(id) {
        Ok(true) => Json(json!({ "deleted": id })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "not_found", &format!("no client certificate {id}")),
        Err(e) => internal(e),
    }
}

/// Every settings section with its values: global ones, and this project's.
async fn get_settings(State(s): State<AppState>) -> Response {
    let project = this_project(&s);
    let mut v = settings::describe(&s.home, project.as_ref().map(|p| &p.file.settings));
    v["project"] = json!(project.map(|p| json!({ "id": p.id(), "name": p.name(), "dir": p.dir })));
    Json(v).into_response()
}

#[derive(Deserialize)]
struct SettingsBody {
    values: Value,
}

/// Saves one section. Proxy changes apply to the running session at once.
async fn put_settings(State(s): State<AppState>, Path(id): Path<String>, Json(b): Json<SettingsBody>) -> Response {
    let Some(section) = settings::section(&id) else {
        return err(StatusCode::NOT_FOUND, "not_found", &format!("no settings section '{id}'"));
    };
    let mut project = this_project(&s);
    let current = match (section.level, &project) {
        (Level::Global, _) => settings::global(&s.home, &id),
        (Level::Project, Some(p)) => p.settings(&id),
        (Level::Project, None) => return err(StatusCode::CONFLICT, "no_project", "this engine has no project folder to save settings in"),
    };
    let values = match section.check(&b.values, &current) {
        Ok(v) => v,
        Err(problems) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "some settings need fixing", "code": "bad_settings", "problems": problems })),
            )
                .into_response();
        }
    };
    if id == settings::PROXY {
        // Apply first: a listen address that cannot be bound is not saved.
        let p = settings::ProxySettings::from_values(&values);
        if let Err(e) = s.engine.apply_proxy_settings(&p).await {
            let problems = [settings::Problem::new("listen_port", format!("{e:#}"))];
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": format!("{e:#}"), "code": "bad_settings", "problems": problems })))
                .into_response();
        }
        crate::session::refresh(&s.home, &s.engine, s.api_addr);
    }
    if id == replace::SETTINGS_SECTION
        && let Err(e) = s.engine.set_replace_on(values.get("enabled").and_then(Value::as_bool).unwrap_or(true))
    {
        return internal(e);
    }
    if id == clientcert::SETTINGS_SECTION
        && let Err(e) = s.engine.set_client_certs_on(values.get("enabled").and_then(Value::as_bool).unwrap_or(true))
    {
        return internal(e);
    }
    if id == intercept::SETTINGS_SECTION
        && let Err(e) = s.engine.set_intercept_options(intercept::InterceptOptions::from_values(&values))
    {
        return bad_settings(vec![settings::Problem::new("filter", format!("{e:#}"))]);
    }
    let saved = match (section.level, project.as_mut()) {
        (Level::Global, _) => settings::save_global(&s.home, &id, &values),
        (Level::Project, Some(p)) => p.save_settings(&id, values.clone()),
        (Level::Project, None) => unreachable!(),
    };
    if let Err(e) = saved {
        return internal(e);
    }
    Json(json!({ "section": id, "values": values, "applies": section.applies, "proxy": s.proxy_addr() })).into_response()
}

/// How much traffic is out of scope, and the storage policy.
async fn storage(State(s): State<AppState>) -> Response {
    let engine = s.engine.clone();
    let stats = match tokio::task::spawn_blocking(move || engine.storage_stats()).await {
        Ok(Ok(st)) => st,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e.into()),
    };
    let project = this_project(&s);
    let policy = project.as_ref().map(|p| settings::StorageSettings::from_values(&p.settings(settings::STORAGE)));
    Json(json!({
        "stats": stats,
        "keep_only_in_scope": policy.is_some_and(|p| p.keep_only_in_scope),
        "last_prune": project.and_then(|p| p.file.last_prune),
    }))
    .into_response()
}

#[derive(Deserialize)]
struct PruneBody {
    /// Must be true: deleting traffic is never a side effect of a stray request.
    #[serde(default)]
    confirm: bool,
}

/// Deletes out-of-scope traffic now.
async fn prune(State(s): State<AppState>, Json(b): Json<PruneBody>) -> Response {
    if !b.confirm {
        return err(StatusCode::BAD_REQUEST, "bad_request", "send {\"confirm\": true} to delete out-of-scope traffic");
    }
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.prune_out_of_scope()).await {
        Ok(Ok(report)) => {
            if let Some(mut p) = this_project(&s) {
                let _ = p.update(|f| f.last_prune = Some(report.clone()));
            }
            Json(report).into_response()
        }
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

/// Every running session (this one included), so a window can switch.
async fn sessions(State(s): State<AppState>) -> Response {
    let home = s.home.clone();
    match tokio::task::spawn_blocking(move || crate::session::running(&home)).await {
        Ok(list) => Json(list).into_response(),
        Err(e) => internal(e.into()),
    }
}
