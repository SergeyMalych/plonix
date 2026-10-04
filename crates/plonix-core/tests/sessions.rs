//! Projects, sessions running side by side, settings that apply live,
//! "keep only in-scope traffic", upstream proxies and the Start screen.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use plonix_core::hub;
use plonix_core::model::NewFinding;
use plonix_core::paths::Home;
use plonix_core::project::{self, Project};
use plonix_core::scope::Decision;
use plonix_core::session::{self, OpenOptions, Session};
use plonix_core::settings;
use plonix_core::store::Store;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn upstream_handler(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let text = format!("hello from {}", req.uri().path());
    Ok(Response::builder().header("content-type", "text/plain").body(Full::new(Bytes::from(text))).unwrap())
}

async fn serve_http() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            tokio::spawn(hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(s), service_fn(upstream_handler)));
        }
    });
    addr
}

async fn via_proxy(proxy: SocketAddr, url: &str) -> (u16, String) {
    let tcp = TcpStream::connect(proxy).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tcp)).await.unwrap();
    tokio::spawn(conn);
    let uri: hyper::Uri = url.parse().unwrap();
    let req = Request::builder().uri(url).header("host", uri.authority().unwrap().as_str()).body(Full::new(Bytes::new())).unwrap();
    let resp = sender.send_request(req).await.unwrap();
    let status = resp.status().as_u16();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn wait_for_count(s: &Session, n: i64) {
    for _ in 0..300 {
        if s.engine.store.count().unwrap() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {n} exchanges, have {}", s.engine.store.count().unwrap());
}

fn home() -> (tempfile::TempDir, Home) {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().join("home") };
    (dir, home)
}

fn new_project(home: &Home, name: &str) -> Project {
    let p = Project::create(&home.root.join("work").join(project::slug(name)), name).unwrap();
    project::remember(home, &p, false).unwrap();
    p
}

fn set(p: &mut Project, section: &str, values: Value) {
    let s = settings::section(section).unwrap();
    let v = s.check(&values, &p.settings(section)).unwrap();
    p.save_settings(section, v).unwrap();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn api(s: &Session, method: &str, path: &str, body: Option<Value>) -> Result<Value, (u16, Value)> {
    let req = ureq::request(method, &format!("http://{}{path}", s.api_addr)).set("Authorization", &format!("Bearer {}", s.token));
    let r = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match r {
        Ok(r) => Ok(r.into_json().unwrap()),
        Err(ureq::Error::Status(code, r)) => Err((code, r.into_json().unwrap_or(Value::Null))),
        Err(e) => panic!("{e}"),
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn two_projects_run_side_by_side_without_sharing_anything() {
    let (_d, home) = home();
    let up = serve_http().await;
    // Both projects ask for the same proxy port: the second moves to the next free one.
    let port = free_port();
    let mut a = new_project(&home, "Alpha");
    let mut b = new_project(&home, "Beta");
    set(&mut a, settings::PROXY, json!({ "listen_port": port }));
    set(&mut b, settings::PROXY, json!({ "listen_port": port }));
    let opts = OpenOptions { api_port: Some(0), ..Default::default() };
    let sa = session::open(&home, a.clone(), opts.clone()).await.unwrap();
    let sb = session::open(&home, b.clone(), opts.clone()).await.unwrap();
    assert_eq!(sa.proxy_addr().port(), port);
    assert_ne!(sb.proxy_addr().port(), port);
    assert_ne!(sa.api_addr, sb.api_addr);

    // Traffic through each proxy lands only in that project's database.
    via_proxy(sa.proxy_addr(), &format!("http://localhost:{}/alpha", up.port())).await;
    via_proxy(sb.proxy_addr(), &format!("http://localhost:{}/beta-1", up.port())).await;
    via_proxy(sb.proxy_addr(), &format!("http://localhost:{}/beta-2", up.port())).await;
    wait_for_count(&sa, 1).await;
    wait_for_count(&sb, 2).await;
    assert_eq!(sa.engine.store.count().unwrap(), 1);
    let only = sa.engine.store.exchanges_after(0, 10).unwrap();
    assert_eq!(only[0].path, "/alpha");

    // Scope is per project too.
    sa.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    assert!(sa.engine.rules().in_scope("localhost"));
    assert!(!sb.engine.rules().in_scope("localhost"));

    // Each API answers for its own project.
    assert_eq!(api(&sa, "GET", "/api/status", None).unwrap()["project"], "Alpha");
    assert_eq!(api(&sb, "GET", "/api/status", None).unwrap()["exchanges"], 2);

    // Both are announced; the one opened last is current.
    let h = home.clone();
    let running = blocking(move || session::running(&h)).await;
    assert_eq!(running.len(), 2);
    assert_eq!(home.read_engine_info().unwrap().project, "Beta");
    assert_eq!(session::find(&home, "alpha").unwrap().api, sa.info().api);
    assert_eq!(session::find(&home, b.dir.to_str().unwrap()).unwrap().project_id, b.id());

    // A project is never opened twice.
    let again = session::open(&home, a.clone(), opts.clone()).await;
    assert!(again.err().unwrap().to_string().contains("already open"));

    // Closing the current session hands "current" to the other one.
    blocking(move || sb.close().unwrap()).await;
    assert_eq!(home.read_engine_info().unwrap().project, "Alpha");
    blocking(move || sa.close().unwrap()).await;
    assert!(home.read_engine_info().is_none());
    assert!(session::running(&home).is_empty());

    // The data stays in each folder and comes back when reopened.
    let sa = session::open(&home, Project::load(&a.dir).unwrap(), opts).await.unwrap();
    assert_eq!(sa.engine.store.count().unwrap(), 1);
    assert!(sa.engine.rules().in_scope("localhost"));
    blocking(move || sa.close().unwrap()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn keep_only_in_scope_traffic_deletes_the_rest_on_close() {
    let (_d, home) = home();
    let up = serve_http().await;
    let mut p = new_project(&home, "Scoped");
    set(&mut p, settings::STORAGE, json!({ "keep_only_in_scope": true }));
    let opts = OpenOptions { proxy_port: Some(0), api_port: Some(0), ..Default::default() };

    // With nothing in scope, nothing is deleted.
    let s = session::open(&home, p.clone(), opts.clone()).await.unwrap();
    via_proxy(s.proxy_addr(), &format!("http://127.0.0.1:{}/early", up.port())).await;
    wait_for_count(&s, 1).await;
    let report = blocking(move || s.close().unwrap()).await.unwrap();
    assert_eq!((report.removed, report.kept), (0, 1));
    assert!(report.skipped.contains("nothing is in scope"));

    let s = session::open(&home, Project::load(&p.dir).unwrap(), opts.clone()).await.unwrap();
    s.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    via_proxy(s.proxy_addr(), &format!("http://localhost:{}/in", up.port())).await;
    via_proxy(s.proxy_addr(), &format!("http://127.0.0.1:{}/out-secret-token", up.port())).await;
    via_proxy(s.proxy_addr(), &format!("http://127.0.0.1:{}/out-evidence", up.port())).await;
    wait_for_count(&s, 4).await;
    let all = s.engine.store.exchanges_after(0, 10).unwrap();
    let evidence = all.iter().find(|e| e.path == "/out-evidence").unwrap().id;
    s.engine
        .store
        .add_finding(&NewFinding { title: "kept".into(), severity: "low".into(), description: String::new(), exchange_ids: vec![evidence] }, "test")
        .unwrap();
    let stats = api(&s, "GET", "/api/storage", None).unwrap();
    assert_eq!(stats["stats"]["out_of_scope"], 2, "{stats}");
    assert_eq!(stats["keep_only_in_scope"], true);
    // Deleting on demand needs an explicit confirmation.
    assert_eq!(api(&s, "POST", "/api/storage/prune", Some(json!({}))).unwrap_err().0, 400);

    let report = blocking(move || s.close().unwrap()).await.unwrap();
    assert_eq!((report.removed, report.kept), (2, 2));
    let saved = Project::load(&p.dir).unwrap().file.last_prune.unwrap();
    assert_eq!(saved.removed, 2);

    // Gone from the file itself, not just hidden.
    let store = Store::open(&p.db_path()).unwrap();
    let paths: Vec<String> = store.exchanges_after(0, 10).unwrap().into_iter().map(|e| e.path).collect();
    assert_eq!(paths, vec!["/in", "/out-evidence"]);
    drop(store);
    let raw = std::fs::read(p.db_path()).unwrap();
    assert!(!raw.windows(16).any(|w| w == b"out-secret-token"), "deleted traffic is still in the database file");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unfinished_session_is_cleaned_up_when_the_project_opens() {
    let (_d, home) = home();
    let mut p = new_project(&home, "Crashed");
    set(&mut p, settings::STORAGE, json!({ "keep_only_in_scope": true }));
    {
        let store = Store::open(&p.db_path()).unwrap();
        let mut ex = plonix_core::model::Exchange { host: "in.test".into(), scheme: "http".into(), port: 80, method: "GET".into(), path: "/".into(), ..Default::default() };
        store.insert_exchange(&ex).unwrap();
        ex.host = "out.test".into();
        store.insert_exchange(&ex).unwrap();
        store.put_rule(&plonix_core::scope::Rule { pattern: "in.test".into(), include_subdomains: false, decision: Decision::Accepted, created_at: 0, note: String::new() }).unwrap();
    }
    // What a session that never closed leaves behind.
    std::fs::write(p.open_marker(), "12345").unwrap();
    let s = session::open(&home, p.clone(), OpenOptions { proxy_port: Some(0), api_port: Some(0), ..Default::default() }).await.unwrap();
    assert_eq!(s.engine.store.count().unwrap(), 1);
    assert_eq!(Project::load(&p.dir).unwrap().file.last_prune.unwrap().removed, 1);
    blocking(move || s.close().unwrap()).await;
    assert!(!p.open_marker().exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn proxy_settings_apply_to_the_running_session() {
    let (_d, home) = home();
    let up = serve_http().await;
    let p = new_project(&home, "Live");
    let s = session::open(&home, p.clone(), OpenOptions { proxy_port: Some(0), api_port: Some(0), ..Default::default() }).await.unwrap();
    let old = s.proxy_addr();

    // Bad values are refused field by field and nothing changes.
    let (code, body) = api(&s, "PUT", "/api/settings/proxy", Some(json!({ "values": { "listen_host": "nowhere", "listen_port": 99999 } }))).unwrap_err();
    assert_eq!(code, 400);
    let fields: Vec<&str> = body["problems"].as_array().unwrap().iter().map(|p| p["field"].as_str().unwrap()).collect();
    assert_eq!(fields, vec!["listen_port"]);
    assert_eq!(s.proxy_addr(), old);

    // A new port: the proxy moves there at once, and the announcement follows.
    let port = free_port();
    let r = api(&s, "PUT", "/api/settings/proxy", Some(json!({ "values": { "listen_port": port } }))).unwrap();
    assert_eq!(r["proxy"], format!("127.0.0.1:{port}"));
    assert_eq!(s.proxy_addr().port(), port);
    let (status, _) = via_proxy(s.proxy_addr(), &format!("http://localhost:{}/moved", up.port())).await;
    assert_eq!(status, 200);
    assert!(TcpStream::connect(old).await.is_err(), "the old port is released");
    assert_eq!(session::find(&home, p.id()).unwrap().proxy, format!("127.0.0.1:{port}"));
    assert_eq!(Project::load(&p.dir).unwrap().settings(settings::PROXY)["listen_port"], port);

    // Global sections are listed too, and saved for all projects.
    let all = api(&s, "GET", "/api/settings", None).unwrap();
    let ids: Vec<&str> = all["sections"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert!(ids.starts_with(&["proxy", "intercept", "storage", "interface"]), "{ids:?}");
    api(&s, "PUT", "/api/settings/interface", Some(json!({ "values": { "open_projects_in": "browser" } }))).unwrap();
    assert!(settings::InterfaceSettings::load(&home).open_in_browser);
    blocking(move || s.close().unwrap()).await;
}

/// A minimal HTTP proxy: CONNECT tunnels, and absolute-form requests
/// relayed as they are. Records each request line.
async fn http_proxy(seen: Arc<Mutex<Vec<String>>>) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut c, _) = l.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if c.read(&mut b).await.unwrap_or(0) == 0 {
                        return;
                    }
                    head.push(b[0]);
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let line = text.lines().next().unwrap().to_string();
                seen.lock().unwrap().push(text.clone());
                let target = line.split_whitespace().nth(1).unwrap().to_string();
                if line.starts_with("CONNECT ") {
                    let mut s = TcpStream::connect(&target).await.unwrap();
                    c.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut c, &mut s).await;
                } else {
                    let authority = target.trim_start_matches("http://").split('/').next().unwrap().to_string();
                    let mut s = TcpStream::connect(&authority).await.unwrap();
                    s.write_all(&head).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut c, &mut s).await;
                }
            });
        }
    });
    addr
}

/// A minimal SOCKS5 proxy (no login, domain names only).
async fn socks5_proxy(seen: Arc<Mutex<Vec<String>>>) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut c, _) = l.accept().await.unwrap();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut hello = [0u8; 3];
                c.read_exact(&mut hello).await.unwrap();
                c.write_all(&[5, 0]).await.unwrap();
                let mut req = [0u8; 5];
                c.read_exact(&mut req).await.unwrap();
                assert_eq!(req[3], 3, "expected a domain name");
                let mut name = vec![0u8; req[4] as usize + 2];
                c.read_exact(&mut name).await.unwrap();
                let port = u16::from_be_bytes([name[name.len() - 2], name[name.len() - 1]]);
                let host = String::from_utf8_lossy(&name[..name.len() - 2]).to_string();
                seen.lock().unwrap().push(format!("{host}:{port}"));
                let mut s = TcpStream::connect((host.as_str(), port)).await.unwrap();
                c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut c, &mut s).await;
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread")]
async fn traffic_goes_through_an_upstream_proxy_except_bypassed_hosts() {
    let (_d, home) = home();
    let up = serve_http().await;
    let seen = Arc::new(Mutex::new(vec![]));
    let proxy = http_proxy(seen.clone()).await;
    let mut p = new_project(&home, "Chained");
    set(
        &mut p,
        settings::PROXY,
        json!({ "upstream_proxy": format!("http://{proxy}"), "upstream_username": "me", "upstream_password": "pw", "upstream_bypass": ["127.0.0.1"] }),
    );
    let s = session::open(&home, p.clone(), OpenOptions { proxy_port: Some(0), api_port: Some(0), ..Default::default() }).await.unwrap();
    let (status, body) = via_proxy(s.proxy_addr(), &format!("http://localhost:{}/chained", up.port())).await;
    assert_eq!((status, body.as_str()), (200, "hello from /chained"));
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].starts_with(&format!("GET http://localhost:{}/chained HTTP/1.1", up.port())), "{}", seen[0]);
        assert!(seen[0].lines().any(|l| l.eq_ignore_ascii_case("Proxy-Authorization: Basic bWU6cHc=")), "{}", seen[0]);
    }
    // Bypassed: reached directly.
    let (status, _) = via_proxy(s.proxy_addr(), &format!("http://127.0.0.1:{}/direct", up.port())).await;
    assert_eq!(status, 200);
    assert_eq!(seen.lock().unwrap().len(), 1);

    // Switch to SOCKS5 while the session runs.
    let socks_seen = Arc::new(Mutex::new(vec![]));
    let socks = socks5_proxy(socks_seen.clone()).await;
    api(&s, "PUT", "/api/settings/proxy", Some(json!({ "values": { "upstream_proxy": format!("socks5://{socks}"), "upstream_username": "" } }))).unwrap();
    let (status, body) = via_proxy(s.proxy_addr(), &format!("http://localhost:{}/socks", up.port())).await;
    assert_eq!((status, body.as_str()), (200, "hello from /socks"));
    assert_eq!(socks_seen.lock().unwrap().as_slice(), [format!("localhost:{}", up.port())]);
    blocking(move || s.close().unwrap()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hosts_that_are_never_decrypted_pass_through_untouched() {
    let (_d, home) = home();
    let up = serve_http().await;
    let mut p = new_project(&home, "Pinned");
    set(&mut p, settings::PROXY, json!({ "passthrough_hosts": ["localhost"] }));
    let s = session::open(&home, p.clone(), OpenOptions { proxy_port: Some(0), api_port: Some(0), ..Default::default() }).await.unwrap();
    // A tunnel to a plain HTTP server: if Plonix tried TLS here, the request would fail.
    let mut tcp = TcpStream::connect(s.proxy_addr()).await.unwrap();
    let target = format!("localhost:{}", up.port());
    tcp.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 1024];
    let n = tcp.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
    tcp.write_all(format!("GET /tunneled HTTP/1.1\r\nHost: {target}\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut out = String::new();
    tcp.read_to_string(&mut out).await.unwrap();
    assert!(out.contains("hello from /tunneled"), "{out}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(s.engine.store.count().unwrap(), 0, "tunneled traffic is not recorded");
    blocking(move || s.close().unwrap()).await;
}

fn hub_call(h: &hub::Hub, token: &str, method: &str, path: &str, body: Option<Value>) -> Result<Value, (u16, Value)> {
    let req = ureq::request(method, &format!("{}{path}", h.url())).set("Authorization", &format!("Bearer {token}"));
    let r = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match r {
        Ok(r) => Ok(r.into_json().unwrap()),
        Err(ureq::Error::Status(code, r)) => Err((code, r.into_json().unwrap_or(Value::Null))),
        Err(e) => panic!("{e}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_start_screen_creates_opens_and_closes_projects() {
    let (dir, home) = home();
    let h = hub::start(&home, Some(0)).await.unwrap();
    *h.session_options.lock().unwrap() = OpenOptions { proxy_port: Some(0), api_port: Some(0), ..Default::default() };
    let token = home.load_or_create_token().unwrap();
    let hh = h.clone();
    let t = token.clone();
    let loc = dir.path().join("elsewhere").display().to_string();
    let (created, listed, opened) = blocking(move || {
        let created = hub_call(&hh, &t, "POST", "/api/projects", Some(json!({ "name": "Acme (staging)", "location": loc }))).unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        let opened = hub_call(&hh, &t, "POST", &format!("/api/projects/{id}/open"), Some(json!({}))).unwrap();
        let listed = hub_call(&hh, &t, "GET", "/api/projects", None).unwrap();
        (created, listed, opened)
    })
    .await;
    assert!(created["path"].as_str().unwrap().ends_with("elsewhere/acme-staging"), "{created}");
    assert!(opened["url"].as_str().unwrap().contains("/#code="));
    assert_eq!(listed[0]["name"], "Acme (staging)");
    assert!(listed[0]["session"]["proxy"].is_string(), "{listed}");
    assert_eq!(listed[0]["session"]["here"], true);
    let id = created["id"].as_str().unwrap().to_string();

    // The window's one-time link signs in to the project's own engine.
    let api = opened["api"].as_str().unwrap().to_string();
    let code = opened["url"].as_str().unwrap().rsplit_once("#code=").unwrap().1.to_string();
    let tok = blocking(move || ureq::post(&format!("{api}/ui/session")).send_json(json!({ "code": code })).unwrap().into_json::<Value>().unwrap()).await;
    assert_eq!(tok["token"], token.as_str());

    // Opening it again reuses the session.
    let (hh, t, i) = (h.clone(), token.clone(), id.clone());
    let again = blocking(move || hub_call(&hh, &t, "POST", &format!("/api/projects/{i}/open"), Some(json!({}))).unwrap()).await;
    assert_eq!(again["started"], false);
    assert_eq!(again["api"], opened["api"]);

    // Settings of an open project go through its engine and apply at once.
    let (hh, t, i) = (h.clone(), token.clone(), id.clone());
    let port = free_port();
    let saved = blocking(move || hub_call(&hh, &t, "PUT", &format!("/api/projects/{i}/settings/proxy"), Some(json!({ "values": { "listen_port": port } }))).unwrap()).await;
    assert_eq!(saved["proxy"], format!("127.0.0.1:{port}"));
    let (hh, t, i) = (h.clone(), token.clone(), id.clone());
    assert_eq!(blocking(move || hub_call(&hh, &t, "POST", &format!("/api/projects/{i}/forget"), Some(json!({})))).await.unwrap_err().0, 409);

    // Close: the session ends and the list says so.
    assert!(h.close(&id).await.unwrap());
    assert!(h.hosted().await.is_empty());
    let (hh, t) = (h.clone(), token.clone());
    let listed = blocking(move || hub_call(&hh, &t, "GET", "/api/projects", None).unwrap()).await;
    assert!(listed[0]["session"].is_null(), "{listed}");

    // Renaming, and bad requests.
    let (hh, t, i) = (h.clone(), token.clone(), id.clone());
    blocking(move || {
        hub_call(&hh, &t, "PUT", &format!("/api/projects/{i}/settings/general"), Some(json!({ "values": { "name": "Acme" } }))).unwrap();
        assert_eq!(hub_call(&hh, &t, "GET", "/api/projects", None).unwrap()[0]["name"], "Acme");
        assert_eq!(hub_call(&hh, &t, "POST", "/api/projects", Some(json!({ "name": "  " }))).unwrap_err().0, 400);
        assert_eq!(hub_call(&hh, &t, "POST", "/api/projects", Some(json!({ "name": "x", "location": "relative/dir" }))).unwrap_err().0, 400);
        assert_eq!(hub_call(&hh, "wrong", "GET", "/api/projects", None).unwrap_err().0, 401);
    })
    .await;
    h.shutdown().await;
}

#[derive(Debug)]
struct OneCert(Arc<rustls::sign::CertifiedKey>);
impl rustls::server::ResolvesServerCert for OneCert {
    fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// HTTPS on `localhost` that speaks HTTP/2 only and answers with the
/// protocol it got. Returns its address and the root its certificate chains to.
async fn h2_server() -> (SocketAddr, rustls_pki_types::CertificateDer<'static>) {
    use rustls_pki_types::pem::PemObject;
    let (ca_pem, ca_key) = plonix_core::ca::CertAuthority::generate_pem().unwrap();
    let ca = plonix_core::ca::CertAuthority::from_pem(&ca_pem, &ca_key).unwrap();
    let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(OneCert(ca.leaf_for("localhost").unwrap())));
    config.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(s).await else { return };
                let svc = service_fn(|req: Request<Incoming>| async move {
                    Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("{:?}", req.version())))))
                });
                let _ = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new()).serve_connection(TokioIo::new(tls), svc).await;
            });
        }
    });
    (addr, rustls_pki_types::CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn http2_servers_are_reached_through_upstream_proxies() {
    use plonix_core::upstream::{OutboundRequest, ProxyServer, Upstream, UpstreamOptions};
    let (up, root) = h2_server().await;
    let get = |port| OutboundRequest {
        scheme: "https".into(),
        host: "localhost".into(),
        port,
        method: "GET".into(),
        target: "/".into(),
        headers: vec![],
        body: Bytes::new(),
        extra_headers: vec![],
    };
    for kind in ["http", "socks5"] {
        let seen = Arc::new(Mutex::new(vec![]));
        let proxy = if kind == "http" { http_proxy(seen.clone()).await } else { socks5_proxy(seen.clone()).await };
        let options = UpstreamOptions { extra_roots: vec![root.clone()], proxy: Some(ProxyServer::parse(&format!("{kind}://{proxy}")).unwrap()), ..Default::default() };
        let resp = Upstream::with_options(options).unwrap().send(get(up.port())).await.unwrap();
        assert_eq!((resp.status, resp.version.as_str(), &resp.body[..]), (200, "HTTP/2", &b"HTTP/2.0"[..]), "through {kind}");
        assert_eq!(seen.lock().unwrap().len(), 1, "went through the {kind} proxy");
    }

    // The request timeout still holds for a server that never answers.
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = silent.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = vec![];
        loop {
            held.push(silent.accept().await.unwrap());
        }
    });
    let options = UpstreamOptions { extra_roots: vec![root], total_timeout: Duration::from_millis(300), ..Default::default() };
    let err = Upstream::with_options(options).unwrap().send(get(port)).await.unwrap_err();
    assert!(format!("{err:#}").contains("timed out"), "{err:#}");
}
