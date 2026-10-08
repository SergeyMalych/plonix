//! The demo project: a ready-made project to explore Plonix with.
//!
//! It holds traffic captured ahead of time from Brightcart, a made-up online
//! shop on reserved `.example` hosts, together with the scope decisions,
//! findings and Bench experiment a researcher would have after a short
//! session. Nothing in it was ever sent anywhere, and opening it sends
//! nothing: the traffic is written straight into the project's database.
//!
//! The Start screen keeps one demo project. Starting it over writes a fresh
//! copy, so it can be explored and changed freely.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use base64::Engine as _;
use serde_json::json;

use bytes::Bytes;

use crate::engine::{self, Responder};
use crate::model::{Exchange, FindingEdit, Headers, NewFinding, Source, now_ms};
use crate::paths::Home;
use crate::project::{self, PROJECT_FILE, Project};
use crate::scope::{Decision, Rule};
use crate::store::Store;
use crate::users::SavedUser;
use crate::upstream::{InboundResponse, OutboundRequest};

pub const NAME: &str = "Demo: Brightcart shop";
const FOLDER: &str = "plonix-demo";

/// The demo project the Start screen knows about, if any.
pub fn find(home: &Home) -> Option<Project> {
    project::list(home).into_iter().filter_map(|e| Project::load(&e.path).ok()).find(|p| p.file.demo)
}

/// Returns the demo project, creating it on first use. With `fresh`, an
/// existing demo is replaced by a new copy (it must not be open).
pub fn ensure(home: &Home, fresh: bool) -> Result<Project> {
    let existing = find(home);
    if let Some(p) = &existing {
        if !fresh {
            return Ok(p.clone());
        }
        if p.is_open() {
            bail!("close the demo project before starting it over");
        }
    }
    let dir = match &existing {
        Some(p) => {
            clear(&p.dir)?;
            project::forget(home, p.id())?;
            p.dir.clone()
        }
        None => free_folder(&home.default_projects_dir()),
    };
    let p = create(&dir)?;
    project::remember(home, &p, false)?;
    Ok(p)
}

/// A folder for a new demo that does not hold anything yet.
fn free_folder(parent: &Path) -> PathBuf {
    let taken = |d: &Path| d.exists() && std::fs::read_dir(d).map(|mut r| r.next().is_some()).unwrap_or(true);
    let first = parent.join(FOLDER);
    if !taken(&first) {
        return first;
    }
    (2..).map(|n| parent.join(format!("{FOLDER}-{n}"))).find(|d| !taken(d)).expect("a free folder name")
}

/// Removes what a demo project folder holds. Only files Plonix itself puts
/// in a project folder are touched, and only in a folder marked as a demo.
fn clear(dir: &Path) -> Result<()> {
    match Project::load(dir) {
        Ok(p) if p.file.demo => {}
        _ => bail!("{} is not a demo project", dir.display()),
    }
    for name in ["traffic.db", "traffic.db-wal", "traffic.db-shm", ".plonix-open", ".plonix.lock", PROJECT_FILE] {
        let path = dir.join(name);
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
    }
    let browser = dir.join("browser");
    if browser.is_dir() {
        std::fs::remove_dir_all(&browser)?;
    }
    Ok(())
}

/// Creates a demo project in `dir` (new or empty) and fills it.
pub fn create(dir: &Path) -> Result<Project> {
    let mut p = Project::create(dir, NAME)?;
    p.update(|f| f.demo = true)?;
    let store = Store::open(&p.db_path())?;
    seed(&store, now_ms())?;
    Ok(p)
}

// ---- the captured session ---------------------------------------------------

const WWW: &str = "www.brightcart.example";
const API: &str = "api.brightcart.example";
/// The handle the demo's researcher header carries.
const RESEARCHER: &str = "brightcart-researcher";
/// The two demo shoppers' session cookies, so the Access check can replay a
/// request as each of them. Session A is the one in the captured traffic.
const SESSION_A: &str = "s%3Ah7Qd2kXvR9wLm4ZpT8yB1cN6.Jf0aWq3Ue5rYt7Io9Pl2Kj4Hg6Fd8Sa";
const SESSION_B: &str = "s%3A2bV9nK4xQ7wE1rT6yU3iO8pA.Lm5Zj0Hg7Fd2Sa9Wq4Ue1rYt6Io3Pl0K";
const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_6) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.6 Safari/605.1.15";

fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s)
}

fn b64url(s: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s)
}

/// A signed-looking token for the demo shopper. The signature is filler.
fn jwt(now: i64) -> String {
    let iat = now / 1000 - 3000;
    format!(
        "{}.{}.{}",
        b64url(r#"{"alg":"HS256","typ":"JWT","kid":"web-2026-09"}"#),
        b64url(&format!(r#"{{"sub":"usr_8f2c41","email":"maya.lopez@mail.example","role":"customer","iat":{iat},"exp":{}}}"#, iat + 3600)),
        "3q2-7wAAvu8kM1n0dXN0LWEtZGVtby1zaWduYXR1cmU",
    )
}

/// The same claims with `"alg":"none"` and the role raised: what an
/// attacker would try on the Bench.
fn unsigned_admin_jwt(now: i64) -> String {
    let iat = now / 1000 - 3000;
    format!(
        "{}.{}.",
        b64url(r#"{"alg":"none","typ":"JWT"}"#),
        b64url(&format!(r#"{{"sub":"usr_8f2c41","email":"maya.lopez@mail.example","role":"admin","iat":{iat},"exp":{}}}"#, iat + 3600)),
    )
}

// ---- the demo responder ----------------------------------------------------
//
// The demo's hosts are made-up `.example` names that resolve nowhere, so a real
// send fails. This stand-in answers them locally, as the made-up API would, so
// a Bench run against the demo returns varied results to explore. It is wired
// in only for the demo project (see `engine::start` / `session::open`); it never
// answers a real host, so it can never stand in for live traffic.

/// The engine responder the demo installs.
pub fn responder() -> Responder {
    std::sync::Arc::new(respond)
}

/// Answers a demo host, or `None` to let the request go out as usual.
fn respond(req: &OutboundRequest) -> Option<InboundResponse> {
    // Only ever answer the reserved `.example` hosts the demo is built on.
    if !req.host.ends_with(".example") {
        return None;
    }
    let path = req.target.split('?').next().unwrap_or("/");
    let (status, mime, body) = route(&req.method, &req.host, path, &req.headers);
    Some(inbound(req, status, mime, body))
}

/// Routes one demo request to a status and body.
fn route(method: &str, host: &str, path: &str, headers: &Headers) -> (u16, &'static str, String) {
    let json = "application/json";
    let auth = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("authorization")).map(|(_, v)| v.as_str()).unwrap_or("");

    if host == API {
        // The admin endpoint enforces the signature: an unsigned (`alg: none`)
        // token is refused outright; a customer token lacks the role.
        if path.starts_with("/v1/admin/") {
            return if is_none_alg(auth) {
                (401, json, json!({ "error": "invalid_token", "message": "unsupported algorithm: none" }).to_string())
            } else {
                (403, json, json!({ "error": "forbidden", "message": "admin role required" }).to_string())
            };
        }
        // The order lookup: any id returns an order, not only the caller's own
        // (the finding the demo is built around). Ids outside the demo range
        // are not found, so a run shows a mix of hits and misses.
        if let Some(rest) = path.strip_prefix("/v1/orders/") {
            if let Ok(id) = rest.parse::<i64>() {
                return match order_value(id) {
                    Some(v) => (200, json, serde_json::to_string_pretty(&v).unwrap_or_default()),
                    None => (404, json, json!({ "error": "not_found", "message": "no such order" }).to_string()),
                };
            }
        }
        if path == "/v1/me" {
            // Who you are is read from the session cookie, so the Access check
            // shows this endpoint answering each saved user as themselves and
            // refusing a signed-out request — access working as it should.
            let cookie = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("cookie")).map(|(_, v)| v.as_str()).unwrap_or("");
            let sess = cookie.split(';').find_map(|c| c.trim().strip_prefix("bc_session=")).unwrap_or("");
            return if sess == SESSION_A {
                (200, json, json!({ "id": "usr_8f2c41", "email": "maya.lopez@mail.example", "name": "Maya Lopez", "role": "customer" }).to_string())
            } else if sess == SESSION_B {
                (200, json, json!({ "id": "usr_3b91de", "email": "dana.quinn@mail.example", "name": "Dana Quinn", "role": "customer" }).to_string())
            } else {
                (401, json, json!({ "error": "unauthorized", "message": "sign in to continue" }).to_string())
            };
        }
        if method == "GET" {
            // The catalog is public; everything else wants a session. So the
            // Access check shows signed-out turned away from the account
            // endpoints but still let in where the demo's flaw lets it be.
            let public = path.starts_with("/v1/products") || path.starts_with("/v1/config") || path.starts_with("/v1/search");
            return if public || has_session(headers) {
                (200, json, json!({ "ok": true, "path": path }).to_string())
            } else {
                (401, json, json!({ "error": "unauthorized", "message": "sign in to continue" }).to_string())
            };
        }
    }

    (404, json, json!({ "error": "not_found", "path": path }).to_string())
}

/// Whether a request carries one of the demo's sessions: a known session
/// cookie or a signed bearer token (an `alg: none` token does not count).
fn has_session(headers: &Headers) -> bool {
    let cookie = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("cookie")).map(|(_, v)| v.as_str()).unwrap_or("");
    let sess = cookie.split(';').find_map(|c| c.trim().strip_prefix("bc_session=")).unwrap_or("");
    if sess == SESSION_A || sess == SESSION_B {
        return true;
    }
    let auth = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("authorization")).map(|(_, v)| v.as_str()).unwrap_or("");
    !auth.is_empty() && !is_none_alg(auth)
}

/// The order the demo API returns for `id`, or `None` outside the demo range.
/// Each id maps to a different customer and a slightly different order, so a
/// run through the ids returns responses that visibly differ.
fn order_value(id: i64) -> Option<serde_json::Value> {
    if !(1000..=1099).contains(&id) {
        return None;
    }
    let people = [
        ("Maya Lopez", "maya.lopez@mail.example", "4111 1111 1111 1111"),
        ("Daniel Okafor", "d.okafor@mail.example", "5555 5555 5555 4444"),
        ("Priya Nair", "priya.nair@mail.example", "4000 0566 5566 5556"),
        ("Tom Becker", "t.becker@mail.example", "6011 0009 9013 9424"),
        ("Sofia Rossi", "sofia.rossi@mail.example", "3782 822463 10005"),
    ];
    let (name, email, card) = people[(id as usize) % people.len()];
    // Vary the line items by id so response lengths differ down the run.
    let catalog = [(1001, "Trail running shoes", 8900), (1002, "Merino hoodie", 7400), (1003, "Insulated bottle", 2400)];
    let n = 1 + (id as usize % catalog.len());
    let items: Vec<_> = catalog.iter().take(n).map(|(p, nm, price)| json!({ "product": p, "name": nm, "qty": 1, "price": price })).collect();
    let total: i64 = catalog.iter().take(n).map(|(_, _, p)| *p as i64).sum();
    Some(json!({
        "id": id, "status": "shipped", "total": total, "currency": "EUR",
        "customer": { "name": name, "email": email },
        "payment": { "method": "card", "card_number": card, "expiry": "08/29", "holder": name },
        "items": items,
        "shipping": { "carrier": "Parcelline", "tracking": format!("PL00{id}178826GB") }
    }))
}

/// Whether a bearer token is unsigned (`alg: none`), by reading its header.
fn is_none_alg(auth: &str) -> bool {
    let tok = auth.strip_prefix("Bearer ").unwrap_or(auth);
    let head = tok.split('.').next().unwrap_or("");
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(head)
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        .map(|s| s.replace([' ', '\t'], "").contains("\"alg\":\"none\""))
        .unwrap_or(false)
}

/// Wraps a demo body as the response the engine records.
fn inbound(req: &OutboundRequest, status: u16, mime: &str, body: String) -> InboundResponse {
    let bytes = Bytes::from(body.into_bytes());
    let headers: Headers = vec![
        ("Content-Type".into(), mime.into()),
        ("Content-Length".into(), bytes.len().to_string()),
        ("Server".into(), "nginx/1.25.4".into()),
        ("X-Powered-By".into(), "Express".into()),
    ];
    let tls_sans = if req.scheme == "https" { vec![req.host.clone()] } else { vec![] };
    InboundResponse { status, headers, body: bytes, tls_sans, version: "HTTP/2".into(), client_cert: None, truncated_from: None }
}

struct Seeder<'a> {
    store: &'a Store,
    ts: i64,
    /// Response body sizes by exchange id, for the Bench history.
    sizes: std::collections::HashMap<i64, usize>,
}

struct Req<'a> {
    method: &'a str,
    url: &'a str,
    headers: Vec<(&'a str, String)>,
    body: String,
}

struct Resp<'a> {
    status: u16,
    headers: Vec<(&'a str, String)>,
    body: String,
    ms: i64,
}

fn req<'a>(method: &'a str, url: &'a str) -> Req<'a> {
    Req { method, url, headers: vec![("User-Agent", UA.into()), ("Accept", "*/*".into())], body: String::new() }
}

impl<'a> Req<'a> {
    fn h(mut self, k: &'a str, v: impl Into<String>) -> Self {
        self.headers.push((k, v.into()));
        self
    }
    fn json(mut self, body: serde_json::Value) -> Self {
        self.headers.push(("Content-Type", "application/json".into()));
        self.body = body.to_string();
        self
    }
    fn form(mut self, body: &str) -> Self {
        self.headers.push(("Content-Type", "application/x-www-form-urlencoded".into()));
        self.body = body.into();
        self
    }
    fn body(mut self, b: impl Into<String>) -> Self {
        self.body = b.into();
        self
    }
}

fn resp<'a>(status: u16, mime: &str) -> Resp<'a> {
    let mut headers = vec![("Date", "Sun, 04 Oct 2026 09:12:44 GMT".to_string())];
    if !mime.is_empty() {
        headers.push(("Content-Type", mime.into()));
    }
    Resp { status, headers, body: String::new(), ms: 40 }
}

impl<'a> Resp<'a> {
    fn h(mut self, k: &'a str, v: impl Into<String>) -> Self {
        self.headers.push((k, v.into()));
        self
    }
    fn body(mut self, b: impl Into<String>) -> Self {
        self.body = b.into();
        self
    }
    fn json(self, v: serde_json::Value) -> Self {
        self.body(serde_json::to_string_pretty(&v).unwrap_or_default())
    }
    fn ms(mut self, ms: i64) -> Self {
        self.ms = ms;
        self
    }
    /// What the shop's API servers add to every response.
    fn api(self) -> Self {
        self.h("Server", "nginx/1.25.4")
            .h("X-Powered-By", "Express")
            .h("X-Request-Id", "req_7c1e0b9a")
            .h("Access-Control-Allow-Origin", "https://www.brightcart.example")
            .h("Access-Control-Allow-Credentials", "true")
    }
}

impl Seeder<'_> {
    fn add(&mut self, r: Req, s: Resp) -> Result<i64> {
        self.add_as(r, s, Source::Proxy)
    }

    fn add_as(&mut self, r: Req, s: Resp, source: Source) -> Result<i64> {
        let (scheme, rest) = r.url.split_once("://").expect("absolute demo URL");
        let (authority, target) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let mut req_headers: Headers = vec![("Host".into(), authority.into())];
        req_headers.extend(r.headers.into_iter().map(|(k, v)| (k.to_string(), v)));
        let mut resp_headers: Headers = s.headers.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        resp_headers.push(("Content-Length".into(), s.body.len().to_string()));
        let tls_sans = if scheme == "https" && (authority == WWW || authority == "brightcart.example") {
            vec![WWW.into(), "brightcart.example".into(), "admin.brightcart.example".into()]
        } else if scheme == "https" {
            vec![authority.into()]
        } else {
            vec![]
        };
        self.ts += 700 + (self.ts % 1900);
        // The demo's first rule sends a researcher header to the shop's API,
        // so captured API calls carry it and show what the rule changed.
        let (mut replaced, mut original_request) = (vec![], None);
        if source == Source::Proxy && authority == API {
            let mut before = Exchange { method: r.method.into(), path: path.into(), query: query.into(), req_headers: req_headers.clone(), ..Default::default() };
            before.req_body = r.body.clone().into_bytes();
            original_request = Some(crate::ask::request_text(&before, 64 * 1024).0);
            req_headers.push(("X-Bug-Bounty".into(), RESEARCHER.into()));
            replaced.push("#1 add header X-Bug-Bounty".to_string());
        }
        let ex = Exchange {
            ts: self.ts,
            scheme: scheme.into(),
            host: authority.into(),
            port: if scheme == "https" { 443 } else { 80 },
            method: r.method.into(),
            path: path.into(),
            query: query.into(),
            req_headers,
            req_body: r.body.into_bytes(),
            status: Some(s.status),
            resp_headers,
            resp_body: s.body.into_bytes(),
            duration_ms: s.ms,
            tls_sans,
            source: Some(source),
            initiator: (source == Source::Replay).then(|| "gui".to_string()),
            http_version: "HTTP/2".into(),
            replaced,
            original_request,
            ..Default::default()
        };
        let id = self.store.insert_exchange(&ex)?;
        self.sizes.insert(id, ex.resp_body.len());
        Ok(id)
    }
}

/// Fills `store` with the demo session, as if captured in the hour before `now`.
pub fn seed(store: &Store, now: i64) -> Result<()> {
    let mut s = Seeder { store, ts: now - 55 * 60 * 1000, sizes: Default::default() };
    let token = jwt(now);
    let bearer = format!("Bearer {token}");
    let session = SESSION_A;
    let cart_state = b64(r#"{"cart":"c_51d0","items":2,"currency":"EUR","admin":false}"#);
    let cookies = format!("bc_session={session}; cart_state={cart_state}; consent=analytics%3D1");
    let page = |path: &'static str| format!("https://{WWW}{path}");

    // Landing page, with the assets and third parties it pulls in.
    let home_html = format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Brightcart: everyday things, delivered</title>
  <link rel="stylesheet" href="https://cdn.bcstatic.example/assets/app.3f9c1e.css">
  <link rel="preconnect" href="https://fonts.gstatic.com">
  <script src="https://cdn.bcstatic.example/assets/app.3f9c1e.js" defer></script>
  <script async src="https://www.googletagmanager.com/gtag/js?id=G-BC0DEMO"></script>
</head>
<body>
  <header>
    <a href="/">Brightcart</a>
    <form action="/search"><input name="q" placeholder="Search products"></form>
    <a href="https://auth.brightcart-id.example/authorize?client_id=bc-web&amp;redirect_uri=https%3A%2F%2F{WWW}%2Fcallback">Sign in</a>
  </header>
  <main>
    <h1>New this week</h1>
    <article><img src="https://media.brightcart.example/p/1001/main.jpg" alt=""><a href="/product/1001">Trail running shoes</a> <b>€89.00</b></article>
    <article><img src="https://media.brightcart.example/p/1002/main.jpg" alt=""><a href="/product/1002">Merino hoodie</a> <b>€74.00</b></article>
    <article><img src="https://media.brightcart.example/p/1003/main.jpg" alt=""><a href="/product/1003">Insulated bottle</a> <b>€24.00</b></article>
  </main>
  <footer>Need help? support@brightcart.example</footer>
</body>
</html>"#
    );
    let csp = "default-src 'self'; script-src 'self' https://cdn.bcstatic.example https://www.googletagmanager.com; img-src 'self' https://media.brightcart.example data:; connect-src 'self' https://api.brightcart.example https://uploads.brightcart-files.example";
    let html_resp = |body: String| {
        resp(200, "text/html; charset=utf-8")
            .h("Server", "nginx/1.25.4")
            .h("Content-Security-Policy", csp)
            .h("Strict-Transport-Security", "max-age=31536000")
            .body(body)
            .ms(120)
    };
    s.add(
        req("GET", "https://brightcart.example/").h("Accept", "text/html"),
        resp(301, "text/html").h("Location", format!("https://{WWW}/")).h("Server", "nginx/1.25.4").ms(18),
    )?;
    s.add(
        req("GET", "https://www.brightcart.example/").h("Accept", "text/html"),
        html_resp(home_html.clone())
            .h("Set-Cookie", format!("bc_session={session}; Path=/; Secure; HttpOnly; SameSite=Lax"))
            .h("Set-Cookie", format!("cart_state={cart_state}; Path=/; Secure")),
    )?;

    let js = format!(
        r#"/* Brightcart web app v4.12.0 */
const CONFIG = {{
  apiBase: "https://api.brightcart.example/v1",
  stagingApi: "https://staging-api.brightcart.example/v1",
  uploads: "https://uploads.brightcart-files.example",
  graphql: "https://api.brightcart.example/graphql",
  // TODO remove before release: used by the image importer
  awsAccessKeyId: "{}",
  imageBucket: "bc-product-images-prod",
  internalSearch: "http://10.20.4.17:9200/products",
}};
export async function api(path, opts = {{}}) {{
  const r = await fetch(CONFIG.apiBase + path, {{ credentials: "include", ...opts }});
  if (!r.ok) throw new Error("API " + r.status);
  return r.json();
}}
"#,
        ["AKIA", "IOSFODNN7", "EXAMPLE"].concat()
    );
    let referer = page("/");
    s.add(
        req("GET", "https://cdn.bcstatic.example/assets/app.3f9c1e.js").h("Referer", referer.clone()),
        resp(200, "application/javascript").h("Server", "cdn").h("Cache-Control", "public, max-age=31536000, immutable").h("Age", "8140").body(js).ms(22),
    )?;
    s.add(
        req("GET", "https://cdn.bcstatic.example/assets/app.3f9c1e.css").h("Referer", referer.clone()),
        resp(200, "text/css").h("Server", "cdn").h("Cache-Control", "public, max-age=31536000, immutable").body(":root{--brand:#2f5bea}body{font-family:Inter,system-ui,sans-serif;margin:0}").ms(19),
    )?;
    s.add(
        req("GET", "https://fonts.gstatic.com/s/inter/v13/UcC73FwrK3iLTeHuS_fvQtMwCp50KnMa1ZL7.woff2").h("Referer", "https://cdn.bcstatic.example/"),
        resp(200, "font/woff2").body("wOF2").ms(31),
    )?;
    s.add(
        req("GET", "https://www.googletagmanager.com/gtag/js?id=G-BC0DEMO").h("Referer", referer.clone()),
        resp(200, "application/javascript").body("/* tag manager */window.dataLayer=window.dataLayer||[];").ms(48),
    )?;
    for url in ["https://media.brightcart.example/p/1001/main.jpg", "https://media.brightcart.example/p/1002/main.jpg", "https://media.brightcart.example/p/1003/main.jpg"] {
        s.add(req("GET", url).h("Referer", referer.clone()), resp(200, "image/jpeg").h("Server", "cdn").body("\u{ff}\u{d8}\u{ff}").ms(26))?;
    }
    s.add(
        req("POST", "https://www.google-analytics.com/g/collect?v=2&tid=G-BC0DEMO&en=page_view&dl=https%3A%2F%2Fwww.brightcart.example%2F").h("Referer", referer.clone()),
        resp(204, "").ms(35),
    )?;

    // Browsing the catalog through the API.
    let api_get = |url: &'static str| req("GET", url).h("Accept", "application/json").h("Origin", page("")).h("Cookie", cookies.clone());
    s.add(
        api_get("https://api.brightcart.example/v1/config"),
        resp(200, "application/json").api().json(json!({
            "env": "production",
            "features": { "graphql": true, "wishlist": true },
            "endpoints": { "uploads": "https://uploads.brightcart-files.example", "fallback": "https://staging-api.brightcart.example/v1" }
        })),
    )?;
    let products = json!({
        "items": [
            { "id": 1001, "name": "Trail running shoes", "price": 8900, "currency": "EUR", "stock": 14 },
            { "id": 1002, "name": "Merino hoodie", "price": 7400, "currency": "EUR", "stock": 3 },
            { "id": 1003, "name": "Insulated bottle", "price": 2400, "currency": "EUR", "stock": 120 }
        ],
        "page": 1, "pages": 6
    });
    s.add(api_get("https://api.brightcart.example/v1/products?category=new&page=1&sort=popular"), resp(200, "application/json").api().json(products.clone()))?;
    s.add(api_get("https://api.brightcart.example/v1/products?category=new&page=2&sort=popular"), resp(200, "application/json").api().json(json!({ "items": [ { "id": 1004, "name": "Rain shell", "price": 12900, "currency": "EUR", "stock": 7 } ], "page": 2, "pages": 6 })))?;
    let product = |id: i64, name: &str, price: i64| json!({ "id": id, "name": name, "price": price, "currency": "EUR", "stock": 14, "images": [format!("https://media.brightcart.example/p/{id}/main.jpg")], "seller": { "id": "sel_204", "name": "Northfield Outdoor" } });
    s.add(
        req("GET", "https://www.brightcart.example/product/1001").h("Accept", "text/html").h("Referer", referer.clone()).h("Cookie", cookies.clone()),
        html_resp("<!doctype html><title>Trail running shoes · Brightcart</title><div id=\"app\" data-product=\"1001\"></div>".into()),
    )?;
    s.add(api_get("https://api.brightcart.example/v1/products/1001"), resp(200, "application/json").api().json(product(1001, "Trail running shoes", 8900)))?;
    s.add(api_get("https://api.brightcart.example/v1/products/1001/reviews?limit=5"), resp(200, "application/json").api().json(json!({ "items": [ { "rating": 5, "text": "Great grip on wet rock.", "author": "J." } ], "total": 42 })))?;
    s.add(api_get("https://api.brightcart.example/v1/products/1002"), resp(200, "application/json").api().json(product(1002, "Merino hoodie", 7400)))?;
    s.add(api_get("https://api.brightcart.example/v1/products/9999"), resp(404, "application/json").api().json(json!({ "error": "not_found", "message": "No product 9999" })))?;
    // A link-preview endpoint: the server fetches whatever URL the client hands
    // it. A parameter carrying a URL or hostname is the classic server-side
    // request smell the Scans hand-off points at.
    s.add(
        api_get("https://api.brightcart.example/v1/preview?url=https%3A%2F%2Fmedia.brightcart.example%2Fp%2F1001%2Fmain.jpg"),
        resp(200, "application/json").api().json(json!({ "title": "Trail running shoes", "image": "https://media.brightcart.example/p/1001/main.jpg", "width": 1200, "height": 800 })),
    )?;
    // A report export that takes a file path. A path-valued parameter is the
    // "could this be pointed at another file?" smell a detector hands to Scans.
    s.add(
        api_get("https://api.brightcart.example/v1/reports/export?path=%2Freports%2F2025%2Fq1-summary.pdf&format=pdf"),
        resp(200, "application/pdf").api().body("%PDF-1.7\n% demo report\n").ms(120),
    )?;
    // A back-office import that accepts XML. An XML body is the "how does the
    // parser treat entities and referenced documents?" smell behind the XML scan.
    s.add(
        req("POST", "https://api.brightcart.example/v1/catalog/import")
            .h("Accept", "application/json")
            .h("Origin", page(""))
            .h("Content-Type", "application/xml")
            .body("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<catalog><item sku=\"TRX-01\"><name>Trail shoes</name><price>120.00</price></item></catalog>"),
        resp(202, "application/json").api().json(json!({ "accepted": 1, "job": "imp_5531" })),
    )?;
    // Real apps have long paths: deep resources and signed download links.
    s.add(
        api_get("https://api.brightcart.example/v1/accounts/me/orders/48213/shipments/2/tracking-events/carrier-updates/latest-delivery-attempt-notifications?lang=en"),
        resp(200, "application/json").api().json(json!({ "items": [ { "at": "2025-09-14T09:12:00Z", "status": "out_for_delivery" } ] })),
    )?;
    s.add(
        api_get("https://api.brightcart.example/v1/exports/download/aW52b2ljZXMtMjAyNS0wOS1icmlnaHRjYXJ0LWV4cG9ydC1hbGwtb3JkZXJzLXdpdGgtbGluZS1pdGVtcw/orders-2025-09.csv"),
        resp(200, "text/csv").api().body("order,total\n48213,113.00\n").ms(140),
    )?;
    s.add(
        api_get("https://api.brightcart.example/v1/exports/download/b3JkZXJzLTIwMjUtMDgtYnJpZ2h0Y2FydC1leHBvcnQtYWxsLW9yZGVycy13aXRoLWxpbmUtaXRlbXM/orders-2025-09.csv"),
        resp(200, "text/csv").api().body("order,total\n47990,64.00\n").ms(130),
    )?;

    // The API describes itself: the Map lists what it offers that nobody has
    // visited yet.
    let op = |summary: &str| json!({ "summary": summary });
    s.add(
        api_get("https://api.brightcart.example/v1/openapi.json"),
        resp(200, "application/json").api().json(json!({
            "openapi": "3.0.3",
            "info": { "title": "Brightcart API", "version": "1.8.2" },
            "servers": [ { "url": "https://api.brightcart.example/v1" } ],
            "paths": {
                "/me": { "get": op("The signed-in customer") },
                "/products": { "get": op("List products") },
                "/products/{productId}": { "get": op("One product") },
                "/products/{productId}/reviews": { "get": op("Reviews of a product"), "post": op("Write a review") },
                "/search": { "get": { "summary": "Search products", "parameters": [ { "in": "query", "name": "q" }, { "in": "query", "name": "limit" } ] } },
                "/orders": { "get": op("Your orders") },
                "/orders/{orderId}": { "get": op("One order"), "delete": op("Cancel an order") },
                "/orders/{orderId}/invoice.pdf": { "get": op("An order's invoice") },
                "/addresses": { "get": op("Saved addresses"), "post": op("Add an address") },
                "/addresses/{addressId}": { "put": op("Change an address"), "delete": op("Remove an address") },
                "/admin/orders": { "get": op("Every customer's orders (staff only)") },
                "/admin/users/{userId}/role": { "put": { "summary": "Change a user's role (staff only)", "parameters": [ { "in": "path", "name": "userId" } ] } },
                "/coupons/{code}/redeem": { "post": op("Redeem a coupon") }
            }
        })),
    )?;

    // Search: the payload in `q` is URL-encoded twice, and a malformed
    // query makes the server print a stack trace.
    s.add(
        api_get("https://api.brightcart.example/v1/search?q=hoodie&limit=20"),
        resp(200, "application/json").api().json(json!({ "query": "hoodie", "items": [ { "id": 1002, "name": "Merino hoodie" } ] })),
    )?;
    s.add(
        api_get("https://api.brightcart.example/v1/search?q=%253Cscript%253Ealert(1)%253C%252Fscript%253E&limit=20"),
        resp(200, "application/json").api().json(json!({ "query": "%3Cscript%3Ealert(1)%3C%2Fscript%3E", "items": [] })),
    )?;
    let search_500 = s.add(
        api_get("https://api.brightcart.example/v1/search?q=shoes%27%29&limit=20"),
        resp(500, "application/json").api().ms(310).json(json!({
            "error": "internal",
            "message": "SearchQueryError: unbalanced parenthesis in query",
            "stack": "SearchQueryError: unbalanced parenthesis in query\n    at parseQuery (/srv/app/node_modules/@bc/search/lib/parse.js:88:11)\n    at SearchController.list (/srv/app/src/controllers/search.js:41:20)\n    at Layer.handle (/srv/app/node_modules/express/lib/router/layer.js:95:5)",
            "upstream": "10.20.4.17:9200"
        })),
    )?;

    // Signing in on the identity host, then back to the shop.
    s.add(
        req("GET", "https://www.brightcart.example/login?next=%252Faccount%252Forders").h("Accept", "text/html").h("Cookie", cookies.clone()),
        resp(302, "text/html")
            .h("Server", "nginx/1.25.4")
            .h("Location", "https://auth.brightcart-id.example/authorize?client_id=bc-web&response_type=code&redirect_uri=https%3A%2F%2Fwww.brightcart.example%2Fcallback&state=Zm9vYmFyYmF6cXV4")
            .ms(15),
    )?;
    s.add(
        req("GET", "https://auth.brightcart-id.example/authorize?client_id=bc-web&response_type=code&redirect_uri=https%3A%2F%2Fwww.brightcart.example%2Fcallback&state=Zm9vYmFyYmF6cXV4")
            .h("Accept", "text/html")
            .h("Referer", page("/login")),
        resp(200, "text/html; charset=utf-8")
            .h("Server", "envoy")
            .body("<!doctype html><title>Sign in to Brightcart</title><form method=post action=/login><input name=email><input type=password name=password><button>Sign in</button></form>")
            .ms(64),
    )?;
    s.add(
        req("POST", "https://auth.brightcart-id.example/login").h("Origin", "https://auth.brightcart-id.example").h("Referer", "https://auth.brightcart-id.example/authorize").form("email=maya.lopez%40mail.example&password=correct-horse-battery&client_id=bc-web"),
        resp(302, "text/html")
            .h("Server", "envoy")
            .h("Location", "https://www.brightcart.example/callback?code=7f3b9c2e1d&state=Zm9vYmFyYmF6cXV4")
            .h("Set-Cookie", "bcid_sso=Q2hlY2tPdXRUaGVEZW1vU1NPQ29va2ll; Path=/; Secure; HttpOnly")
            .ms(212),
    )?;
    s.add(
        req("GET", "https://www.brightcart.example/callback?code=7f3b9c2e1d&state=Zm9vYmFyYmF6cXV4").h("Accept", "text/html").h("Referer", "https://auth.brightcart-id.example/").h("Cookie", cookies.clone()),
        resp(302, "text/html").h("Server", "nginx/1.25.4").h("Location", "/account/orders").ms(88),
    )?;
    s.add(
        req("POST", "https://auth.brightcart-id.example/oauth/token").h("Origin", page("")).h("Referer", page("/callback")).form("grant_type=authorization_code&code=7f3b9c2e1d&client_id=bc-web"),
        resp(200, "application/json")
            .h("Server", "envoy")
            .h("Cache-Control", "no-store")
            .json(json!({ "access_token": token, "token_type": "Bearer", "expires_in": 3600, "refresh_token": "rt_4mQ9xV2bN8kL1pZ7wE3r" }))
            .ms(95),
    )?;

    // The signed-in account, carrying a bearer token.
    let authed = |method: &'static str, url: &'static str| {
        req(method, url).h("Accept", "application/json").h("Origin", page("")).h("Authorization", bearer.clone()).h("Cookie", cookies.clone())
    };
    s.add(
        authed("GET", "https://api.brightcart.example/v1/me"),
        resp(200, "application/json").api().json(json!({
            "id": "usr_8f2c41", "email": "maya.lopez@mail.example", "name": "Maya Lopez", "role": "customer",
            "phone": "+44 7700 900123", "marketing_opt_in": true
        })),
    )?;
    s.add(
        authed("GET", "https://api.brightcart.example/v1/users/usr_8f2c41/addresses"),
        resp(200, "application/json").api().json(json!({ "items": [ { "id": "adr_19", "line1": "12 Harbour Street", "city": "Bristol", "postcode": "BS1 4XE", "country": "GB" } ] })),
    )?;
    s.add(
        authed("GET", "https://api.brightcart.example/v1/orders?limit=10"),
        resp(200, "application/json").api().json(json!({ "items": [ { "id": 1042, "status": "shipped", "total": 16300, "currency": "EUR", "placed_at": "2026-09-28" } ] })),
    )?;
    let order = |id: i64, name: &str, email: &str, card: &str, total: i64| {
        json!({
            "id": id, "status": "shipped", "total": total, "currency": "EUR",
            "customer": { "name": name, "email": email },
            "payment": { "method": "card", "card_number": card, "expiry": "08/29", "holder": name },
            "items": [ { "product": 1001, "qty": 1, "price": 8900 }, { "product": 1002, "qty": 1, "price": 7400 } ],
            "shipping": { "carrier": "Parcelline", "tracking": "PL0043178826GB" }
        })
    };
    let own_order = s.add(
        authed("GET", "https://api.brightcart.example/v1/orders/1042"),
        resp(200, "application/json").api().json(order(1042, "Maya Lopez", "maya.lopez@mail.example", "4111 1111 1111 1111", 16300)),
    )?;
    s.add(
        authed("GET", "https://api.brightcart.example/v1/orders/1042/invoice.pdf"),
        resp(200, "application/pdf").api().body("%PDF-1.7\n% demo invoice\n").ms(140),
    )?;
    s.add(
        authed("GET", "https://api.brightcart.example/v1/admin/orders?limit=10"),
        resp(403, "application/json").api().json(json!({ "error": "forbidden", "message": "admin role required" })),
    )?;
    s.add(
        authed("POST", "https://api.brightcart.example/graphql").json(json!({ "query": "query Wishlist { me { wishlist { id name price } } }", "operationName": "Wishlist" })),
        resp(200, "application/json").api().json(json!({ "data": { "me": { "wishlist": [ { "id": 1003, "name": "Insulated bottle", "price": 2400 } ] } } })),
    )?;
    s.add(
        authed("POST", "https://api.brightcart.example/graphql").json(json!({ "query": "query { me { wishlist { id name prce } } }" })),
        resp(200, "application/json").api().json(json!({ "errors": [ { "message": "Cannot query field \"prce\" on type \"Product\". Did you mean \"price\"?", "locations": [ { "line": 1, "column": 31 } ] } ] })),
    )?;

    // Cart and checkout.
    s.add(
        authed("POST", "https://api.brightcart.example/v1/cart/items").json(json!({ "product": 1003, "qty": 2 })),
        resp(201, "application/json").api().json(json!({ "cart": "c_51d0", "items": 3, "total": 21100 })),
    )?;
    s.add(
        authed("PATCH", "https://api.brightcart.example/v1/cart/items/1003").json(json!({ "qty": 1, "price": 100 })),
        resp(200, "application/json").api().json(json!({ "cart": "c_51d0", "items": 3, "total": 18800, "warning": "client price ignored" })),
    )?;
    s.add(
        authed("POST", "https://api.brightcart.example/v1/coupons/apply").json(json!({ "code": "WELCOME10" })),
        resp(200, "application/json").api().json(json!({ "applied": "WELCOME10", "discount": 1880 })),
    )?;
    s.add(
        authed("POST", "https://api.brightcart.example/v1/coupons/apply").json(json!({ "code": "STAFF50" })),
        resp(422, "application/json").api().json(json!({ "error": "invalid_coupon", "message": "Coupon STAFF50 is restricted to staff accounts" })),
    )?;
    s.add(
        req("GET", "https://js.stripe.com/v3/").h("Referer", page("/checkout")),
        resp(200, "application/javascript").body("/* payment widget */").ms(54),
    )?;
    s.add(
        req("POST", "https://o450912.ingest.sentry.io/api/4504551/envelope/?sentry_key=0f1e2d3c4b5a69788796a5b4c3d2e1f0")
            .h("Origin", page(""))
            .h("Content-Type", "text/plain;charset=UTF-8")
            .h("Referer", page("/checkout")),
        resp(200, "application/json").json(json!({ "id": "c0ffee00c0ffee00c0ffee00c0ffee00" })).ms(73),
    )?;

    // A profile picture goes to a separate upload host, with the same token.
    s.add(
        req("POST", "https://uploads.brightcart-files.example/v2/avatars")
            .h("Authorization", bearer.clone())
            .h("Origin", page(""))
            .h("Referer", page("/account"))
            .h("Content-Type", "multipart/form-data; boundary=----bcform")
            .h("X-Upload-Token", "upt_2Lr8nQ4vZ1xC7bM3")
            .h("Accept", "application/json"),
        resp(201, "application/json").h("Server", "uploader/2.3").json(json!({ "url": "https://media.brightcart.example/u/usr_8f2c41/avatar.png", "bytes": 48213 })).ms(260),
    )?;
    s.add(
        req("GET", "https://uploads.brightcart-files.example/v2/avatars/usr_8f2c41").h("Authorization", bearer.clone()).h("Origin", page("")).h("Accept", "application/json"),
        resp(200, "application/json").h("Server", "uploader/2.3").json(json!({ "owner": "usr_8f2c41", "url": "https://media.brightcart.example/u/usr_8f2c41/avatar.png" })),
    )?;

    // A few more page views, so the timeline looks like a real session.
    for path in ["/account", "/account/orders", "/checkout", "/robots.txt"] {
        let url: &'static str = match path {
            "/account" => "https://www.brightcart.example/account",
            "/account/orders" => "https://www.brightcart.example/account/orders",
            "/checkout" => "https://www.brightcart.example/checkout",
            _ => "https://www.brightcart.example/robots.txt",
        };
        let r = req("GET", url).h("Accept", "text/html").h("Cookie", cookies.clone()).h("Referer", referer.clone());
        if path == "/robots.txt" {
            s.add(r, resp(200, "text/plain").h("Server", "nginx/1.25.4").body("User-agent: *\nDisallow: /admin/\nDisallow: /internal/debug\nSitemap: https://www.brightcart.example/sitemap.xml\n"))?;
        } else {
            s.add(r, html_resp(format!("<!doctype html><title>Brightcart</title><div id=\"app\" data-page=\"{path}\"></div>")))?;
        }
    }

    // Experiments sent from the Bench: the same order lookup with another
    // order number, and the token with its signature stripped.
    let other_order = s.add_as(
        authed("GET", "https://api.brightcart.example/v1/orders/1041"),
        resp(200, "application/json").api().json(order(1041, "Daniel Okafor", "d.okafor@mail.example", "5555 5555 5555 4444", 4800)),
        Source::Replay,
    )?;
    let unsigned = format!("Bearer {}", unsigned_admin_jwt(now));
    let alg_none = s.add_as(
        req("GET", "https://api.brightcart.example/v1/admin/orders?limit=10").h("Accept", "application/json").h("Authorization", unsigned.clone()),
        resp(401, "application/json").api().json(json!({ "error": "invalid_token", "message": "unsupported algorithm: none" })),
        Source::Replay,
    )?;

    // Scope: the two shop hosts are accepted. Everything else waits as a
    // suggestion, with the evidence the analyzer finds in the traffic.
    let rule = |pattern: &str, note: &str| Rule { pattern: pattern.into(), include_subdomains: false, decision: Decision::Accepted, created_at: now - 54 * 60 * 1000, note: note.into() };
    store.put_rules(&[rule(WWW, "the shop"), rule(API, "its API"), rule("brightcart.example", "")])?;
    engine::reanalyze(store, &store.rules()?)?;

    // Findings written up during the session.
    let finding = |title: &str, severity: &str, status: &str, description: &str, ids: Vec<i64>| -> Result<()> {
        let f = store.add_finding(&NewFinding { title: title.into(), severity: severity.into(), description: description.into(), exchange_ids: ids }, "demo")?;
        if status != "open" {
            store.update_finding(f.id, &FindingEdit { status: Some(status.into()), ..Default::default() })?;
        }
        Ok(())
    };
    finding(
        "Any signed-in customer can read other customers' orders",
        "high",
        "confirmed",
        "GET /v1/orders/{id} returns the order for any id, not only the caller's own. Signed in as Maya Lopez (order 1042), asking for order 1041 returned Daniel Okafor's order, with his email, shipping details and card number.\n\nReproduce: open the Bench tab \"Order lookup\", change the order number and send.\n\nFix: check that the order belongs to the caller before returning it.",
        vec![own_order, other_order],
    )?;
    finding(
        "Full card numbers in order responses",
        "medium",
        "open",
        "Order details include payment.card_number unmasked. Lens marks it under Spotted as a card number. Only the last four digits should leave the payment service.",
        vec![own_order],
    )?;
    let js_id = store.search(&crate::query::Query::parse("host:cdn.bcstatic.example")?, &store.rules()?, 1, 0).map(|(v, _)| v.first().map(|e| e.id))?.into_iter().collect();
    finding(
        "Cloud access key and internal address in the public JavaScript bundle",
        "medium",
        "open",
        "app.3f9c1e.js ships an access key id (awsAccessKeyId) and the internal search address 10.20.4.17:9200. It also names a staging API host, staging-api.brightcart.example. Rotate the key and keep build-time secrets out of client code.",
        js_id,
    )?;
    finding(
        "Stack trace and internal address in search errors",
        "low",
        "open",
        "A search with an unbalanced quote and parenthesis returns HTTP 500 with a Node.js stack trace, file paths and the upstream search address.",
        vec![search_500],
    )?;
    finding(
        "Unsigned tokens are rejected",
        "info",
        "false_positive",
        "Tried the bearer token with alg set to none and the role raised to admin. The API refuses it (401 unsupported algorithm), so token signatures are enforced.",
        vec![alg_none],
    )?;

    let sizes = &s.sizes;
    // The Bench, as the session left it: the order lookup experiment with
    // both sends ready to compare, and the token experiment.
    let raw = |auth: &str| format!("Accept: application/json\nOrigin: https://{WWW}\nAuthorization: {auth}\nCookie: {cookies}\n\n");
    let sent = |id: i64, status: u16, url: &str, auth: &str| {
        json!({ "id": id, "ts": now, "status": status, "ms": 40, "len": sizes.get(&id).copied().unwrap_or(0), "req": { "method": "GET", "url": url, "raw": raw(auth) } })
    };
    let lookup = json!({
        "name": "Order lookup",
        "from": own_order,
        "method": "GET",
        "url": "https://api.brightcart.example/v1/orders/1041",
        "raw": raw(&bearer),
        "bodyB64": null,
        "history": [
            sent(other_order, 200, "https://api.brightcart.example/v1/orders/1041", &bearer),
            sent(own_order, 200, "https://api.brightcart.example/v1/orders/1042", &bearer)
        ],
        "cur": other_order,
        "picks": []
    });
    let none_tab = json!({
        "name": "Unsigned admin token",
        "from": alg_none,
        "method": "GET",
        "url": "https://api.brightcart.example/v1/admin/orders?limit=10",
        "raw": format!("Accept: application/json\nAuthorization: {unsigned}\n\n"),
        "bodyB64": null,
        "history": [ sent(alg_none, 401, "https://api.brightcart.example/v1/admin/orders?limit=10", &unsigned) ],
        "cur": alg_none,
        "picks": []
    });
    // A run ready to try: the order id is marked as a position, with a range of
    // ids queued. Pressing Start walks the ids and shows how each one returns a
    // different customer's order.
    let order_run = json!({
        "name": "Order IDs — run",
        "from": own_order,
        "method": "GET",
        "url": format!("https://{API}/v1/orders/\u{2022}1042\u{2022}"),
        "raw": raw(&bearer),
        "bodyB64": null,
        "history": [],
        "cur": null,
        "picks": [],
        "panel": "run",
        "run": { "mode": "sweep", "lists": [{ "kind": "range", "from": 1032, "to": 1052, "step": 1 }], "base": true, "max": "", "delay": "40" }
    });
    store.set_view_state("bench", &json!({ "tabs": [order_run, lookup, none_tab], "active": 0 }))?;

    // Two saved users for the Access check: the shopper from the capture and a
    // second one, each with their own session cookie. Install the Saved users
    // and Access check tools from the Market to replay requests as them.
    let jar = |sess: &str| format!("bc_session={sess}; cart_state={cart_state}; consent=analytics%3D1");
    let user = |id: &str, name: &str, note: &str, sess: &str| {
        let mut u = SavedUser { id: id.into(), name: name.into(), note: note.into(), headers: vec![("Cookie".into(), jar(sess))], cookies: vec![], keep_fresh: true };
        u.fold_cookie_header();
        u
    };
    let mut maya = user("maya", "Maya (customer)", "The signed-in shopper from the captured traffic.", SESSION_A);
    let mut dana = user("dana", "Dana (another customer)", "A second shopper, to compare what each may see.", SESSION_B);
    // The session cookie belongs to the shop; Dana's is marked to run out
    // tomorrow, and a promo cookie from an old visit has already expired.
    for u in [&mut maya, &mut dana] {
        u.cookies.iter_mut().filter(|c| c.name == "bc_session").for_each(|c| c.domain = "brightcart.example".into());
    }
    dana.cookies.iter_mut().filter(|c| c.name == "bc_session").for_each(|c| c.expires = Some(crate::users::now_secs() + 86_400));
    dana.cookies.push(crate::users::Cookie { name: "promo".into(), value: "SPRING10".into(), domain: "brightcart.example".into(), expires: Some(crate::users::now_secs() - 3_600) });
    store.set_saved_users(&[maya, dana])?;

    seed_filters(store)?;
    seed_traffic_rules(store)?;
    Ok(())
}

/// Rules that change traffic as it passes: a researcher header on every
/// request to the shop, fresh responses instead of cached ones, and a text
/// rule kept switched off.
fn seed_traffic_rules(store: &Store) -> Result<()> {
    use crate::replace::{Kind, Rule as TrafficRule, Target};
    let base = TrafficRule {
        id: 0,
        kind: Kind::AddHeader,
        target: Target::RequestHeader,
        pattern: String::new(),
        replace: String::new(),
        regex: false,
        enabled: true,
        in_scope_only: true,
        note: String::new(),
        browser: true,
        bench: true,
        scans: true,
        when: String::new(),
    };
    for r in [
        TrafficRule { pattern: "X-Bug-Bounty".into(), replace: RESEARCHER.into(), ..base.clone() },
        TrafficRule { kind: Kind::RemoveHeader, pattern: "If-None-Match".into(), bench: false, scans: false, ..base.clone() },
        TrafficRule {
            kind: Kind::Replace,
            target: Target::ResponseBody,
            pattern: r#""beta_checkout":false"#.into(),
            replace: r#""beta_checkout":true"#.into(),
            enabled: false,
            bench: false,
            scans: false,
            when: format!("host:{API} path:/v1/config"),
            note: "Try the new checkout".into(),
            ..base
        },
    ] {
        crate::replace::check(&r).map_err(anyhow::Error::msg)?;
        store.add_replace_rule(&r)?;
    }
    Ok(())
}

/// Traffic starts with static files hidden, and the filters overview offers
/// a few views of this traffic, each a set of include and exclude chips.
fn seed_filters(store: &Store) -> Result<()> {
    let inc = |term: &str| json!({ "term": term, "mode": "include" });
    let exc = |term: &str| json!({ "term": term, "mode": "exclude" });
    store.set_view_state("traffic", &json!({ "filters": [exc("kind:static")], "text": "" }))?;
    let view = |title: &str, why: &str, filters: Vec<serde_json::Value>| json!({ "title": title, "why": why, "filters": filters });
    store.set_view_state(
        "filter_tour",
        &json!({ "views": [
            view("Hide the noise", "Two exclude chips: images, fonts, styles and scripts, and the analytics a page pulls in.", vec![exc("kind:static"), exc("is:trackers")]),
            view("Just the shop's API", "Two include chips must both match: one host, and JSON responses.", vec![inc("host:api.brightcart.example"), inc("mime:json")]),
            view("What went wrong", "Two values of one field match either one: client errors or server errors.", vec![inc("status:4xx"), inc("status:5xx")]),
            view("Requests that change things", "A named filter from the built-in pack: POST, PUT, PATCH and DELETE.", vec![inc("is:writes"), exc("kind:static")]),
            view("The sign-in flow", "The built-in Auth flows filter: logins, OAuth, tokens and sessions, on any host.", vec![inc("is:auth")]),
            view("Find a value anywhere", "Plain text matches URLs, headers and decoded bodies. Here: every response with a card number field.", vec![inc("card_number")]),
            view("Third parties", "Out-of-scope hosts, without the trackers: what to decide on next.", vec![inc("scope:out"), exc("is:trackers")]),
            view("Your Bench experiments", "Requests sent from the Bench rather than captured.", vec![inc("source:replay")]),
            view("Mix them", "API failures outside the product catalog: one include and two exclude chips.", vec![inc("host:api.brightcart.example"), exc("status:2xx"), exc("path:/v1/products")]),
        ] }),
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insight;

    fn home() -> Home {
        Home { root: tempfile::tempdir().unwrap().keep() }
    }

    #[test]
    fn the_demo_shows_every_screen_something() {
        let home = home();
        let p = ensure(&home, false).unwrap();
        assert!(p.file.demo);
        assert_eq!(p.dir.file_name().unwrap(), FOLDER);
        let store = Store::open(&p.db_path()).unwrap();
        let rules = store.rules().unwrap();
        assert!(store.count().unwrap() > 40);

        // Scope suggestions, strongest first: the upload host shares the session token.
        let sugg = store.suggestions(&rules).unwrap();
        let domains: Vec<&str> = sugg.iter().map(|s| s.domain.as_str()).collect();
        for d in ["uploads.brightcart-files.example", "auth.brightcart-id.example", "cdn.bcstatic.example", "staging-api.brightcart.example", "admin.brightcart.example", "media.brightcart.example"] {
            assert!(domains.contains(&d), "{d} is suggested, got {domains:?}");
        }
        assert_eq!(domains[0], "uploads.brightcart-files.example");
        assert!(!domains.iter().any(|d| d.contains("google")), "analytics is never suggested");

        // Findings, the Bench and Lens insights.
        assert_eq!(store.findings().unwrap().len(), 5);

        // Two saved users, their cookies one by one; one of Dana's has run out.
        let users = store.saved_users().unwrap();
        assert_eq!(users.len(), 2);
        assert!(users.iter().all(|u| u.headers.is_empty() && u.cookies.iter().any(|c| c.name == "bc_session")));
        let now = crate::users::now_secs();
        assert!(users[1].cookies.iter().any(|c| !c.live(now)));
        assert!(users[1].request_headers("api.brightcart.example", now)[0].1.contains(SESSION_B));
        let named = crate::filterpack::FilterLibrary::at(&home.root.join("filters")).load();
        let tour = store.view_state("filter_tour").unwrap().unwrap();
        for v in tour["views"].as_array().unwrap() {
            let mut q = String::new();
            for f in v["filters"].as_array().unwrap() {
                let minus = if f["mode"] == "exclude" { "-" } else { "" };
                q.push_str(&format!("{minus}{} ", f["term"].as_str().unwrap()));
            }
            // Two values of one field are one comma list, as the window sends them.
            let q = q.replace("status:4xx status:5xx", "status:4xx,5xx");
            let (_, n) = store.search(&named.parse(&q).unwrap(), &rules, 1, 0).unwrap();
            assert!(n > 0, "{} shows something ({q})", v["title"]);
        }
        let bench = store.view_state("bench").unwrap().unwrap();
        assert_eq!(bench["tabs"].as_array().unwrap().len(), 3);
        let all = store.exchanges_after(0, 500).unwrap();
        let kinds: std::collections::BTreeSet<String> =
            all.iter().flat_map(|ex| insight::analyze(ex, insight::detectors()).into_iter().map(|i| i.kind)).collect();
        for k in ["jwt", "base64", "url-encoded", "card-number", "email", "aws-access-key", "private-ip"] {
            assert!(kinds.contains(k), "Lens spots {k}, got {kinds:?}");
        }
        assert!(all.iter().all(|ex| ex.host.ends_with(".example") || !rules.in_scope(&ex.host)), "only made-up hosts are in scope");
    }

    #[test]
    fn the_responder_answers_demo_hosts_with_varied_orders() {
        let out = |method: &str, url: &str, auth: &str| {
            let rest = url.split_once("://").unwrap().1;
            let (host, target) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
            let mut headers: Headers = vec![];
            if !auth.is_empty() {
                headers.push(("Authorization".into(), auth.into()));
            }
            OutboundRequest {
                scheme: "https".into(),
                host: host.into(),
                port: 443,
                method: method.into(),
                target: target.into(),
                headers,
                body: Bytes::new(),
                extra_headers: vec![],
            }
        };
        let signed = format!("Bearer {}", jwt(now_ms()));

        // Any id in range returns an order, and different ids differ in length.
        let a = respond(&out("GET", "https://api.brightcart.example/v1/orders/1041", &signed)).unwrap();
        let b = respond(&out("GET", "https://api.brightcart.example/v1/orders/1042", &signed)).unwrap();
        assert_eq!(a.status, 200);
        assert_eq!(b.status, 200);
        assert_ne!(a.body.len(), b.body.len(), "different ids return different orders");
        assert!(String::from_utf8_lossy(&b.body).contains("card_number"));

        // Out of range is not found.
        assert_eq!(respond(&out("GET", "https://api.brightcart.example/v1/orders/5", &signed)).unwrap().status, 404);

        // The admin endpoint: a signed customer token is forbidden, an unsigned
        // one is refused outright.
        let unsigned = format!("Bearer {}", unsigned_admin_jwt(now_ms()));
        assert_eq!(respond(&out("GET", "https://api.brightcart.example/v1/admin/orders", &signed)).unwrap().status, 403);
        assert_eq!(respond(&out("GET", "https://api.brightcart.example/v1/admin/orders", &unsigned)).unwrap().status, 401);

        // A real host is never answered by the demo responder.
        assert!(respond(&out("GET", "https://api.github.com/", "")).is_none());
    }

    #[test]
    fn starting_over_replaces_the_demo() {
        let home = home();
        let first = ensure(&home, false).unwrap();
        assert_eq!(ensure(&home, false).unwrap().id(), first.id(), "the demo is reused");
        let again = ensure(&home, true).unwrap();
        assert_ne!(again.id(), first.id(), "a fresh copy gets a new id, so its Bench starts clean");
        assert_eq!(again.dir, first.dir);
        assert_eq!(project::list(&home).len(), 1);

        let lock = again.lock().unwrap();
        assert!(ensure(&home, true).is_err(), "an open demo is not replaced");
        drop(lock);
    }

    #[test]
    fn only_demo_folders_are_cleared() {
        let home = home();
        let p = Project::create(&home.root.join("mine"), "mine").unwrap();
        assert!(clear(&p.dir).is_err());
        assert!(p.dir.join(PROJECT_FILE).exists());
    }

    #[test]
    fn a_taken_folder_is_skipped() {
        let home = home();
        let taken = home.default_projects_dir().join(FOLDER);
        std::fs::create_dir_all(&taken).unwrap();
        std::fs::write(taken.join("notes.txt"), "mine").unwrap();
        let p = ensure(&home, false).unwrap();
        assert_eq!(p.dir.file_name().unwrap(), "plonix-demo-2");
    }
}
