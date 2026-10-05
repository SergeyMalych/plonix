//! The browser crawl against a real headless Chromium. Runs only when a
//! Chromium-based browser is found (or `PLONIX_BROWSER` points at one);
//! otherwise it says so and passes.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use plonix_core::Engine;
use plonix_core::browser::{self, Kind};
use plonix_core::ca::CertAuthority;
use plonix_core::crawl::CrawlRequest;
use plonix_core::engine::{self, EngineConfig, SendError};
use plonix_core::paths::Home;
use plonix_core::scope::Decision;
use plonix_core::store::Store;
use plonix_core::upstream::Upstream;
use tokio::net::TcpListener;

fn hits() -> &'static Mutex<Vec<String>> {
    static HITS: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    HITS.get_or_init(Mutex::default)
}

/// A small app whose links exist only once its script has run.
const HOME: &str = r#"<!doctype html><html><head><script src="http://offscope.test/tracker.js"></script></head><body>
<form method="post" action="/login"><input name="user"><input name="pass" type="password"><button>Sign in</button></form>
<div id="app"></div>
<script>
  document.getElementById('app').innerHTML =
    '<a href="/js-only">rendered link</a> <button id="more">Show more</button> <button id="wipe">Delete everything</button>';
  document.getElementById('more').onclick = () => { fetch('/api/data'); history.pushState({}, '', '/routed'); };
  document.getElementById('wipe').onclick = () => fetch('/api/delete', { method: 'POST' });
  document.forms[0].submit();
</script></body></html>"#;

async fn handler(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let path = req.uri().path().to_string();
    hits().lock().unwrap().push(format!("{} {path}", req.method()));
    let (ctype, body) = match path.as_str() {
        "/" => ("text/html", HOME),
        "/js-only" => ("text/html", "<p>only reachable once the page rendered</p>"),
        "/routed" => ("text/html", "<p>a client-side route</p>"),
        "/api/data" | "/api/delete" => ("application/json", "{\"ok\":true}"),
        _ => ("text/plain", "not here"),
    };
    Ok(Response::builder().header("content-type", ctype).body(Full::new(Bytes::from_static(body.as_bytes()))).unwrap())
}

async fn serve() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            tokio::spawn(hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(s), service_fn(handler)));
        }
    });
    addr
}

async fn start(home: &Home) -> engine::Running {
    home.ensure().unwrap();
    let ca = Arc::new(CertAuthority::load_or_create(home).unwrap());
    std::fs::create_dir_all(home.root.join("projects")).unwrap();
    let store = Store::open(&home.project_db("test")).unwrap();
    let engine = Engine::new("test", store, ca, Upstream::new(false, vec![]).unwrap()).unwrap();
    let config = EngineConfig {
        home: home.clone(),
        project: "test".into(),
        proxy_addr: "127.0.0.1:0".parse().unwrap(),
        proxy_port_fallback: false,
        api_addr: "127.0.0.1:0".parse().unwrap(),
        insecure_upstream: false,
    };
    engine::start_with(engine, &config).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_crawl_renders_follows_routes_and_stays_in_scope() {
    let home = plonix_core::paths::Home::resolve(None).unwrap();
    let Some(found) = browser::detect(&home).filter(|b| b.kind == Kind::Chromium) else {
        eprintln!("skipped: no Chromium-based browser found (set PLONIX_BROWSER to run this test)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve().await;
    let r = start(&home).await;
    let url = format!("http://127.0.0.1:{}/", up.port());

    // Refused until the host is accepted.
    let refused = r.engine.crawl(CrawlRequest { host: "127.0.0.1".into(), start: Some(url.clone()), browser: true, ..Default::default() }, "crawl").await;
    assert!(matches!(refused, Err(SendError::OutOfScope { .. })));
    // A start URL on another host is refused too.
    r.engine.decide("127.0.0.1", Decision::Accepted, false, "").unwrap();
    let elsewhere = r.engine.crawl(CrawlRequest { host: "127.0.0.1".into(), start: Some("http://offscope.test/".into()), browser: true, ..Default::default() }, "crawl").await;
    assert!(matches!(elsewhere, Err(SendError::BadRequest(_))));

    let req = CrawlRequest { host: "127.0.0.1".into(), start: Some(url), browser: true, click: true, max_seconds: Some(60), ..Default::default() };
    let report = r.engine.crawl(req, "crawl").await.unwrap();
    assert_eq!(report.browser.as_deref(), Some(found.name.as_str()));
    assert!(report.clicks >= 1, "{report:?}");
    assert!(report.forms.iter().any(|f| f.method == "POST" && f.action.ends_with("/login") && f.fields == ["user", "pass"]), "{report:?}");
    assert_eq!(report.blocked_hosts, ["offscope.test"], "{report:?}");

    // Every page the browser loaded went through the proxy into Traffic, so it is on the Map.
    let mut paths: Vec<String> = vec![];
    for _ in 0..100 {
        paths = r.engine.store.endpoints("127.0.0.1").unwrap().into_iter().map(|e| e.path).collect();
        if ["/js-only", "/routed", "/api/data"].iter().all(|p| paths.iter().any(|x| x == p)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for p in ["/", "/js-only", "/routed", "/api/data"] {
        assert!(paths.iter().any(|x| x == p), "{p} should be on the Map: {paths:?} {report:?}");
    }
    // Nothing destructive happened: no form was submitted, the delete button was never clicked.
    let hits = hits().lock().unwrap().clone();
    assert!(!hits.iter().any(|h| h.contains("/login") || h.contains("/api/delete")), "{hits:?}");
    assert!(r.engine.store.endpoints("offscope.test").unwrap().is_empty());
}
