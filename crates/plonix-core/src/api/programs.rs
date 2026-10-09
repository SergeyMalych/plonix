//! Programs and the bounty platforms they come from.

use super::*;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/program", get(program_get))
        .route("/api/program/read", post(program_read))
        .route("/api/program/preview", post(program_preview))
        .route("/api/program/apply", post(program_apply))
        .route("/api/program/clear", post(program_clear))
        .route("/api/platforms", get(platforms_list))
        .route("/api/platforms/{name}/connect", post(platform_connect))
        .route("/api/platforms/{name}/disconnect", post(platform_disconnect))
        .route("/api/platforms/{name}/programs", get(platform_programs))
        .route("/api/platforms/{name}/sync", post(platform_sync))
        .route("/api/platforms/{name}/catalog", get(platform_catalog))
        .route("/api/platforms/{name}/programs/{handle}", get(platform_program))
}

/// The program this project follows, if any.
async fn program_get(State(s): State<AppState>) -> Response {
    Json(json!({ "program": s.engine.program().map(|g| g.program.clone()) })).into_response()
}

/// Reads a program from pasted policy text, a policy page or a domain's
/// security.txt. Nothing is applied: the result is a draft to review.
async fn program_read(Json(req): Json<crate::bounty::ReadRequest>) -> Response {
    match tokio::task::spawn_blocking(move || crate::bounty::read(&req, &crate::bounty::fetch_text)).await {
        Ok(Ok(p)) => Json(json!({ "program": p })).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "bad_request", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

#[derive(Deserialize)]
struct ProgramBody {
    program: crate::bounty::Program,
}

/// What applying a program would change.
async fn program_preview(State(s): State<AppState>, Json(b): Json<ProgramBody>) -> Response {
    match s.engine.program_preview(b.program) {
        Ok(p) => Json(p).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, "bad_request", &format!("{e:#}")),
    }
}

/// Makes the project follow a program: its scope and rules apply from now on.
async fn program_apply(State(s): State<AppState>, Json(b): Json<ProgramBody>) -> Response {
    crate::usage::record("program_applied");
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.apply_program(b.program)).await {
        Ok(Ok(p)) => Json(p).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "bad_request", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

#[derive(Deserialize)]
struct ClearBody {
    #[serde(default)]
    remove_scope: bool,
}

async fn program_clear(State(s): State<AppState>, Json(b): Json<ClearBody>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.clear_program(b.remove_scope)).await {
        Ok(Ok(cleared)) => Json(json!({ "cleared": cleared })).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn platforms_list(State(s): State<AppState>) -> Response {
    let home = s.home.clone();
    match tokio::task::spawn_blocking(move || crate::platform::PlatformLibrary::new(&home).infos()).await {
        Ok((platforms, problems)) => Json(json!({ "platforms": platforms, "problems": problems })).into_response(),
        Err(e) => internal(e.into()),
    }
}

fn platform_error(e: crate::platform::FetchError) -> Response {
    use crate::platform::FetchError;
    match e {
        e @ FetchError::NotConnected(_) => err(StatusCode::CONFLICT, "not_connected", &e.to_string()),
        e @ FetchError::Unauthorized(..) => err(StatusCode::FORBIDDEN, "platform_refused", &e.to_string()),
        e @ FetchError::Other(_) => err(StatusCode::BAD_GATEWAY, "platform_unavailable", &e.to_string()),
    }
}

/// Runs `f` with a platform's pack and the saved token.
async fn with_platform<T: Send + 'static>(
    home: Home,
    name: String,
    f: impl FnOnce(&crate::platform::Client) -> Result<T, crate::platform::FetchError> + Send + 'static,
) -> Result<T, Response> {
    let out = tokio::task::spawn_blocking(move || {
        let lib = crate::platform::PlatformLibrary::new(&home);
        let Some(pack) = lib.get(&name) else { return Err(err(StatusCode::NOT_FOUND, "not_found", "no such platform; add it from the Market")) };
        let Some(cred) = lib.credentials().get(&name) else { return Err(platform_error(crate::platform::FetchError::NotConnected(pack.doc.title.clone()))) };
        let client = crate::platform::Client::new(&pack, cred).map_err(internal)?;
        f(&client).map_err(platform_error)
    })
    .await;
    out.unwrap_or_else(|e| Err(internal(e.into())))
}

#[derive(Deserialize)]
struct ConnectBody {
    #[serde(default)]
    user: String,
    secret: String,
}

/// Saves a platform token after checking that the platform accepts it.
async fn platform_connect(State(s): State<AppState>, Path(name): Path<String>, Json(b): Json<ConnectBody>) -> Response {
    let home = s.home.clone();
    let out = tokio::task::spawn_blocking(move || {
        let lib = crate::platform::PlatformLibrary::new(&home);
        let Some(pack) = lib.get(&name) else { return err(StatusCode::NOT_FOUND, "not_found", "no such platform; add it from the Market") };
        let cred = crate::platform::Credential { user: b.user.trim().to_string(), secret: b.secret.trim().to_string() };
        if cred.secret.is_empty() || (pack.doc.auth.kind == crate::platform::AuthKind::Basic && cred.user.is_empty()) {
            return err(StatusCode::BAD_REQUEST, "bad_request", "fill in both fields");
        }
        let programs = match crate::platform::Client::new(&pack, cred.clone()).map_err(|e| crate::platform::FetchError::Other(e.to_string())).and_then(|c| c.programs()) {
            Ok(p) => p,
            Err(e) => return platform_error(e),
        };
        if let Err(e) = lib.credentials().set(&name, &cred) {
            return internal(e);
        }
        // A new token can see different programs: start over and pull them all.
        lib.forget_catalog(&name);
        let sync = crate::platform::start_sync(&home, &name).ok();
        Json(json!({ "connected": true, "programs": programs.len(), "sync": sync })).into_response()
    })
    .await;
    out.unwrap_or_else(|e| internal(e.into()))
}

async fn platform_disconnect(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    let home = s.home.clone();
    match tokio::task::spawn_blocking(move || {
        let lib = crate::platform::PlatformLibrary::new(&home);
        lib.forget_catalog(&name);
        lib.credentials().remove(&name)
    })
    .await
    {
        Ok(Ok(removed)) => Json(json!({ "removed": removed })).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

/// The programs the user can work on at a platform.
async fn platform_programs(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    match with_platform(s.home.clone(), name, |c| c.programs()).await {
        Ok(list) => Json(json!({ "programs": list })).into_response(),
        Err(r) => r,
    }
}

/// Starts pulling every program, with its assets and rules, from a platform.
async fn platform_sync(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    let home = s.home.clone();
    match tokio::task::spawn_blocking(move || crate::platform::start_sync(&home, &name)).await {
        Ok(Ok(st)) => Json(json!({ "sync": st })).into_response(),
        Ok(Err(e)) => platform_error(e),
        Err(e) => internal(e.into()),
    }
}

/// The programs last pulled from a platform, and how the current pull is going.
async fn platform_catalog(State(s): State<AppState>, Path(name): Path<String>) -> Response {
    let home = s.home.clone();
    match tokio::task::spawn_blocking(move || (crate::platform::PlatformLibrary::new(&home).catalog(&name), crate::platform::sync_status(&home, &name))).await {
        Ok((catalog, sync)) => Json(json!({ "catalog": catalog, "sync": sync })).into_response(),
        Err(e) => internal(e.into()),
    }
}

/// One program with its assets and rules, read from the platform. Nothing is applied.
#[derive(Deserialize)]
struct ProgramQuery {
    #[serde(default)]
    name: String,
    #[serde(default)]
    bounty: bool,
}

async fn platform_program(State(s): State<AppState>, Path((name, handle)): Path<(String, String)>, Query(q): Query<ProgramQuery>) -> Response {
    let out = with_platform(s.home.clone(), name, move |c| c.program(&c.summary(&handle, &q.name, q.bounty))).await;
    match out {
        Ok(p) => Json(json!({ "program": p })).into_response(),
        Err(r) => r,
    }
}
