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

const INDEX_HTML: &str = include_str!("../ui/index.html");
const APP_JS: &str = include_str!("../ui/app.js");
const APP_CSS: &str = include_str!("../ui/app.css");
const ICON_SVG: &str = include_str!("../ui/icon.svg");

/// How long a launch code stays valid.
pub const CODE_TTL: Duration = Duration::from_secs(120);

/// The page may only load its own scripts and styles and talk to its own
/// origin. Captured traffic is attacker-controlled; this keeps any markup in
/// it inert even if a rendering bug let it through.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; \
                   connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

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

fn asset(content_type: &'static str, body: &'static str) -> Response {
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

pub async fn index() -> Response {
    asset("text/html; charset=utf-8", INDEX_HTML)
}

pub async fn app_js() -> Response {
    asset("text/javascript; charset=utf-8", APP_JS)
}

pub async fn app_css() -> Response {
    asset("text/css; charset=utf-8", APP_CSS)
}

pub async fn icon() -> Response {
    asset("image/svg+xml", ICON_SVG)
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

    #[test]
    fn launch_codes_expire() {
        let codes = LaunchCodes::default();
        let c = codes.issue().unwrap();
        let Some(old) = Instant::now().checked_sub(CODE_TTL + Duration::from_secs(1)) else { return };
        codes.codes.lock().unwrap().insert(c.clone(), old);
        assert!(!codes.redeem(&c));
    }
}
