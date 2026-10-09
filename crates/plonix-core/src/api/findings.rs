//! Findings and the report export.

use super::*;
use crate::model::{FindingEdit, NewFinding};
use crate::report;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/findings", get(findings).post(add_finding))
        .route("/api/findings/export", get(export_findings))
        .route("/api/findings/{id}", get(finding).patch(edit_finding).delete(delete_finding))
}

async fn findings(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    let in_scope_only = agent_in_scope_only(&s, &caller);
    let list = s.engine.store.findings().and_then(|all| {
        if !in_scope_only {
            return Ok(all);
        }
        let rules = s.engine.rules();
        let visible = |ex: &Exchange| rules.in_scope(&ex.host);
        let mut out = Vec::with_capacity(all.len());
        for f in all {
            if report::shown(&s.engine.store, &f, &visible)? {
                out.push(f);
            }
        }
        Ok(out)
    });
    match list {
        Ok(f) => Json(f).into_response(),
        Err(e) => internal(e),
    }
}

async fn add_finding(State(s): State<AppState>, headers: HeaderMap, Json(f): Json<NewFinding>) -> Response {
    let f = match f.checked() {
        Ok(f) => f,
        Err(e) => return err(StatusCode::BAD_REQUEST, "bad_request", &e),
    };
    match s.engine.store.add_finding(&f, &initiator(&headers)) {
        Ok(f) => {
            crate::usage::record("finding_added");
            (StatusCode::CREATED, Json(f)).into_response()
        }
        Err(e) => internal(e),
    }
}

fn finding_not_found(id: i64) -> Response {
    err(StatusCode::NOT_FOUND, "not_found", &format!("finding {id} not found"))
}

async fn finding(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<i64>) -> Response {
    match s.engine.store.finding(id) {
        Ok(Some(f)) if agent_in_scope_only(&s, &caller) => {
            let rules = s.engine.rules();
            match report::shown(&s.engine.store, &f, &|ex: &Exchange| rules.in_scope(&ex.host)) {
                Ok(true) => Json(f).into_response(),
                Ok(false) => outside_agent_data(),
                Err(e) => internal(e),
            }
        }
        Ok(Some(f)) => Json(f).into_response(),
        Ok(None) => finding_not_found(id),
        Err(e) => internal(e),
    }
}

/// Changes a finding's title, severity, status or description. User only:
/// agents never reach it (it is in no mode's capabilities).
async fn edit_finding(State(s): State<AppState>, Path(id): Path<i64>, Json(edit): Json<FindingEdit>) -> Response {
    let edit = match edit.checked() {
        Ok(e) => e,
        Err(e) => return err(StatusCode::BAD_REQUEST, "bad_request", &e),
    };
    match s.engine.store.update_finding(id, &edit) {
        Ok(Some(f)) => Json(f).into_response(),
        Ok(None) => finding_not_found(id),
        Err(e) => internal(e),
    }
}

/// Deletes a finding; the requests it pointed to stay. User only.
async fn delete_finding(State(s): State<AppState>, Path(id): Path<i64>) -> Response {
    match s.engine.store.delete_finding(id) {
        Ok(true) => Json(json!({ "deleted": id })).into_response(),
        Ok(false) => finding_not_found(id),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct ExportParams {
    #[serde(default = "default_format")]
    format: String,
    /// Comma-separated finding ids; none means every finding.
    #[serde(default)]
    ids: String,
    /// Comma-separated statuses; by default everything but false positives.
    #[serde(default)]
    status: String,
}

fn default_format() -> String {
    "md".into()
}

/// The findings as a report (Markdown, HTML or JSON) with their evidence
/// requests. Agents limited to in-scope traffic get out-of-scope evidence
/// as a note instead of the request.
async fn export_findings(State(s): State<AppState>, caller: MaybeCaller, Query(p): Query<ExportParams>) -> Response {
    let Some(format) = report::Format::parse(&p.format) else {
        return err(StatusCode::BAD_REQUEST, "bad_request", "format must be md, html or json");
    };
    crate::usage::record("report_exported");
    let sel = match report::Selection::parse(&p.ids, &p.status) {
        Ok(sel) => sel,
        Err(e) => return err(StatusCode::BAD_REQUEST, "bad_request", &e),
    };
    let in_scope_only = agent_in_scope_only(&s, &caller);
    let engine = s.engine.clone();
    let built = tokio::task::spawn_blocking(move || {
        let rules = engine.rules();
        let visible = |ex: &Exchange| !in_scope_only || rules.in_scope(&ex.host);
        report::build(&engine.store, &engine.project, &sel, &visible)
    })
    .await;
    let r = match built {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => return internal(e),
        Err(e) => return internal(e.into()),
    };
    (
        [
            ("content-type", format.content_type().to_string()),
            ("content-disposition", format!("attachment; filename=\"{}\"", report::file_name(&r.project, format))),
            ("content-security-policy", "default-src 'none'; style-src 'unsafe-inline'".to_string()),
            ("x-content-type-options", "nosniff".to_string()),
        ],
        report::render(&r, format),
    )
        .into_response()
}
