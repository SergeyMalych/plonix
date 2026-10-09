//! The web UI, served by the engine on its loopback API address.
//!
//! The page itself is static and embedded in the binary; everything it shows
//! comes from the same token-authenticated `/api/*` endpoints the CLI uses.
//!
//! The page gets the API token through a one-time launch code: a client that
//! already holds the token (`plonix ui`, `plonix open`) asks for a code with
//! `POST /api/ui/launch` and opens `http://127.0.0.1:<port>/#code=<code>`.
//! The page trades the code for the token once (`POST /ui/session`). Codes are
//! single-use and expire quickly, so the URL is safe to appear in a process
//! list or browser history, and the token never travels in a URL.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use axum::extract::Path;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const LAUNCHER_HTML: &str = include_str!("../ui/launcher.html");

/// Everything the two pages load from `/ui/`: name, content type, contents.
const FILES: &[(&str, &'static str, &str)] = &[
    ("common.js", "text/javascript; charset=utf-8", include_str!("../ui/common.js")),
    ("settings.js", "text/javascript; charset=utf-8", include_str!("../ui/settings.js")),
    ("app/core.js", "text/javascript; charset=utf-8", include_str!("../ui/app/core.js")),
    ("app/traffic.js", "text/javascript; charset=utf-8", include_str!("../ui/app/traffic.js")),
    ("app/lens.js", "text/javascript; charset=utf-8", include_str!("../ui/app/lens.js")),
    ("app/bench.js", "text/javascript; charset=utf-8", include_str!("../ui/app/bench.js")),
    ("app/scope.js", "text/javascript; charset=utf-8", include_str!("../ui/app/scope.js")),
    ("app/map.js", "text/javascript; charset=utf-8", include_str!("../ui/app/map.js")),
    ("app/users.js", "text/javascript; charset=utf-8", include_str!("../ui/app/users.js")),
    ("app/findings.js", "text/javascript; charset=utf-8", include_str!("../ui/app/findings.js")),
    ("app/scans.js", "text/javascript; charset=utf-8", include_str!("../ui/app/scans.js")),
    ("app/programs.js", "text/javascript; charset=utf-8", include_str!("../ui/app/programs.js")),
    ("app/agents.js", "text/javascript; charset=utf-8", include_str!("../ui/app/agents.js")),
    ("app/market.js", "text/javascript; charset=utf-8", include_str!("../ui/app/market.js")),
    ("app/ask.js", "text/javascript; charset=utf-8", include_str!("../ui/app/ask.js")),
    ("app/settings.js", "text/javascript; charset=utf-8", include_str!("../ui/app/settings.js")),
    ("app/rules.js", "text/javascript; charset=utf-8", include_str!("../ui/app/rules.js")),
    ("app/shell.js", "text/javascript; charset=utf-8", include_str!("../ui/app/shell.js")),
    ("launcher.js", "text/javascript; charset=utf-8", include_str!("../ui/launcher.js")),
    ("app.css", "text/css; charset=utf-8", include_str!("../ui/app.css")),
    ("studio.css", "text/css; charset=utf-8", include_str!("../ui/studio.css")),
    ("icon.svg", "image/svg+xml", include_str!("../ui/icon.svg")),
];

/// Font files the stylesheets load: name, content type, bytes.
const FONTS: &[(&str, &str, &[u8])] = &[("fonts/jost.woff2", "font/woff2", include_bytes!("../ui/fonts/jost.woff2"))];

/// How long a launch code stays valid.
pub const CODE_TTL: Duration = Duration::from_secs(120);

/// The page may only load its own scripts and styles and talk to its own
/// origin. Captured traffic is attacker-controlled; this keeps any markup in
/// it inert even if a rendering bug let it through.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   font-src 'self'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Outstanding one-time launch codes.
#[derive(Default)]
pub struct LaunchCodes {
    codes: Mutex<HashMap<String, Instant>>,
}

impl LaunchCodes {
    pub fn issue(&self) -> anyhow::Result<String> {
        let mut buf = [0u8; 18];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut buf)
            .map_err(|_| anyhow::anyhow!("no system randomness"))?;
        let code: String = buf.iter().map(|b| format!("{b:02x}")).collect();
        let mut codes = self.codes.lock().unwrap();
        codes.retain(|_, t| t.elapsed() < CODE_TTL);
        codes.insert(code.clone(), Instant::now());
        Ok(code)
    }

    /// Consumes a code. True when it was issued, unused and not expired.
    pub fn redeem(&self, code: &str) -> bool {
        let mut codes = self.codes.lock().unwrap();
        codes.retain(|_, t| t.elapsed() < CODE_TTL);
        codes.remove(code).is_some()
    }
}

fn asset(content_type: &'static str, body: impl Into<String>) -> Response {
    asset_bytes(content_type, body.into().into_bytes())
}

fn asset_bytes(content_type: &'static str, body: Vec<u8>) -> Response {
    let mut r = (StatusCode::OK, body).into_response();
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

/// The project window. Its project id is in a meta tag, so state the page
/// keeps in the browser stays with the project even if another project
/// later gets the same address.
pub async fn index(project_id: String) -> Response {
    let id: String = project_id.chars().filter(char::is_ascii_alphanumeric).collect();
    asset("text/html; charset=utf-8", INDEX_HTML.replace("{{PROJECT_ID}}", &id))
}

/// The Start screen.
pub async fn launcher() -> Response {
    asset("text/html; charset=utf-8", LAUNCHER_HTML)
}

/// A script, stylesheet or icon the pages load, by name (`/ui/{*file}`).
pub async fn file(Path(name): Path<String>) -> Response {
    if let Some((_, content_type, bytes)) = FONTS.iter().find(|(n, ..)| *n == name) {
        return asset_bytes(content_type, bytes.to_vec());
    }
    match FILES.iter().find(|(n, ..)| *n == name) {
        Some((_, content_type, body)) => asset(content_type, *body),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A screenshot from a Market item's guide (see [`crate::guide`]).
pub async fn guide_shot(Path(file): Path<String>) -> Response {
    match file.strip_suffix(".jpg").and_then(crate::guide::shot) {
        Some(bytes) => asset_bytes("image/jpeg", bytes.to_vec()),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_codes_are_single_use() {
        let codes = LaunchCodes::default();
        let c = codes.issue().unwrap();
        assert_eq!(c.len(), 36);
        assert!(!codes.redeem("nope"));
        assert!(codes.redeem(&c));
        assert!(!codes.redeem(&c));
    }

    /// Every `/ui/…` file a page refers to is served, so a new script
    /// cannot be added to a page and forgotten here.
    #[test]
    fn every_file_a_page_loads_is_served() {
        for page in [INDEX_HTML, LAUNCHER_HTML] {
            for part in page.split("\"/ui/").skip(1) {
                let name = &part[..part.find('"').unwrap()];
                assert!(FILES.iter().any(|(n, ..)| *n == name), "/ui/{name} is not served");
            }
        }
    }

    /// Every font a stylesheet points at is served.
    #[test]
    fn every_font_a_stylesheet_loads_is_served() {
        for (_, _, css) in FILES.iter().filter(|(n, ..)| n.ends_with(".css")) {
            for part in css.split("url(\"/ui/").skip(1) {
                let name = &part[..part.find('"').unwrap()];
                assert!(FONTS.iter().any(|(n, ..)| *n == name), "/ui/{name} is not served");
            }
        }
    }

    #[test]
    fn launch_codes_expire() {
        let codes = LaunchCodes::default();
        let c = codes.issue().unwrap();
        let Some(old) = Instant::now().checked_sub(CODE_TTL + Duration::from_secs(1)) else { return };
        codes.codes.lock().unwrap().insert(c.clone(), old);
        assert!(!codes.redeem(&c));
    }
}
