//! The browser Plonix opens through its proxy, and trusting the CA.

use super::*;
use crate::browser;
use crate::chromium;
use crate::trust;
use crate::scope::Decision;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/browser", get(browser_status))
        .route("/api/browser/open", post(open_browser))
        .route("/api/browser/install", post(install_browser))
        .route("/api/ca/trust", post(trust_ca))
}

#[derive(Deserialize)]
struct OpenBody {
    target: String,
    /// Accept the target's domain (and subdomains) into scope first.
    #[serde(default = "yes")]
    scope: bool,
    /// Open the saved user's own browser window instead: a profile of its
    /// own whose traffic is sent and recorded as that user.
    #[serde(default)]
    as_user: Option<String>,
}

fn yes() -> bool {
    true
}

/// Opens the capture browser at a target: an isolated browser profile that
/// routes through this engine's proxy and trusts its CA.
async fn open_browser(State(s): State<AppState>, Json(b): Json<OpenBody>) -> Response {
    let target = match browser::parse_target(&b.target) {
        Ok(t) => t,
        Err(e) => return err(StatusCode::BAD_REQUEST, "bad_target", &format!("{e:#}")),
    };
    let mut rule = Value::Null;
    if b.scope {
        let engine = s.engine.clone();
        let host = target.host.clone();
        match tokio::task::spawn_blocking(move || engine.decide(&host, Decision::Accepted, true, "")).await {
            Ok(Ok(r)) => rule = json!(r),
            Ok(Err(e)) => return err(StatusCode::BAD_REQUEST, "bad_target", &format!("{e:#}")),
            Err(e) => return internal(e.into()),
        }
    }
    let Some(found) = browser::detect(&s.home) else {
        let can_install = chromium::platform().is_some();
        let how = if can_install { "Get the Plonix browser (a Chromium download, once), install" } else { "Install" };
        let msg = format!(
            "no browser found to launch. {how} Google Chrome, Brave, Edge or Firefox, or set any browser's HTTP and HTTPS proxy to {}",
            s.proxy_addr()
        );
        return (StatusCode::NOT_FOUND, Json(json!({ "error": msg, "code": "no_browser", "can_install": can_install }))).into_response();
    };
    let mut profile = browser::profile_dir(&s.home, s.engine.project_ref.get().map(|p| p.dir.as_path()));
    let mut proxy = s.proxy_addr();
    let mut as_user = Value::Null;
    if let Some(id) = b.as_user.as_deref().filter(|id| !id.is_empty()) {
        let user = match s.engine.store.saved_users() {
            Ok(users) => users.into_iter().find(|u| u.id == id),
            Err(e) => return internal(e),
        };
        let Some(user) = user else { return err(StatusCode::NOT_FOUND, "no_user", &format!("no saved user '{id}'")) };
        proxy = match s.engine.user_proxy(&user.id).await {
            Ok(a) => a.to_string(),
            Err(e) => return internal(e),
        };
        // Ids are [a-z0-9-], so they make a safe folder name.
        profile = std::path::PathBuf::from(format!("{}-{}", profile.display(), user.id));
        as_user = json!({ "id": user.id, "name": user.name });
    }
    let launched = browser::launch(&profile, &found, &proxy, &s.engine.ca.spki_sha256(), &target.url);
    if let Err(e) = launched {
        return internal(e);
    }
    crate::usage::record("capture_started");
    // Firefox checks the keychain; the others trust the CA by its key pin.
    let needs_trust = found.kind == browser::Kind::Firefox && {
        let home = s.home.clone();
        tokio::task::spawn_blocking(move || trust::is_trusted(&home)).await.ok().flatten() != Some(true)
    };
    Json(json!({
        "url": target.url,
        "host": target.host,
        "browser": found.name,
        "needs_trust": needs_trust,
        "can_trust": trust::supported(),
        "scope": rule,
        "as_user": as_user,
    }))
    .into_response()
}

/// Which browser "Open target" uses, the Plonix browser and its download,
/// and whether the system trusts the CA.
async fn browser_status(State(s): State<AppState>) -> Response {
    let home = s.home.clone();
    let r = tokio::task::spawn_blocking(move || {
        let found = browser::detect(&home);
        let firefox = found.as_ref().is_some_and(|b| b.kind == browser::Kind::Firefox);
        json!({
            "browser": found.map(|b| json!({ "name": b.name, "kind": if b.kind == browser::Kind::Firefox { "firefox" } else { "chromium" } })),
            "plonix_browser": chromium::installed(&home),
            "can_install": chromium::platform().is_some(),
            "install": chromium::progress(),
            "ca_trusted": if firefox { trust::is_trusted(&home) } else { None },
            "can_trust": trust::supported(),
        })
    })
    .await;
    match r {
        Ok(v) => Json(v).into_response(),
        Err(e) => internal(e.into()),
    }
}

/// Starts downloading the Plonix browser; `GET /api/browser` follows it.
async fn install_browser(State(s): State<AppState>) -> Response {
    if chromium::platform().is_none() {
        return err(StatusCode::BAD_REQUEST, "unsupported", "the Plonix browser is available for macOS and 64-bit Linux only");
    }
    Json(json!({ "install": chromium::start_install(&s.home) })).into_response()
}

/// Trusts the CA in the login keychain. macOS asks the user to confirm.
async fn trust_ca(State(s): State<AppState>) -> Response {
    let home = s.home.clone();
    match tokio::task::spawn_blocking(move || trust::trust(&home).map(|()| trust::is_trusted(&home))).await {
        Ok(Ok(trusted)) => Json(json!({ "trusted": trusted.unwrap_or(true) })).into_response(),
        Ok(Err(e)) => err(StatusCode::CONFLICT, "not_trusted", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}
