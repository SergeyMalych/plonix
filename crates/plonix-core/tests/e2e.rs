//! End-to-end tests: a real proxy, real upstream servers (HTTP and HTTPS),
//! the store, adaptive scope and the local API.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use plonix_core::ca::CertAuthority;
use plonix_core::engine::{self, EngineConfig, ReplayRequest, Running, SendError, SendRequest};
use plonix_core::paths::Home;
use plonix_core::query::Query;
use plonix_core::scope::{Decision, EvidenceKind};
use plonix_core::store::Store;
use plonix_core::upstream::Upstream;
use plonix_core::Engine;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn upstream_handler(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let (parts, body) = req.into_parts();
    let body = body.collect().await.unwrap().to_bytes();
    let resp = match parts.uri.path() {
        "/" => Response::builder()
            .header("content-type", "text/html")
            .header("set-cookie", "sid=session-abcdef123456; Path=/; HttpOnly")
            .body(Full::new(Bytes::from(
                "<h1>welcome home</h1><script src=\"https://cdn.linked-assets.test/app.js\"></script>",
            ))),
        "/echo" => {
            let mut text = format!("{} {}\n", parts.method, parts.uri);
            for (k, v) in &parts.headers {
                text.push_str(&format!("{}: {}\n", k, v.to_str().unwrap_or("")));
            }
            text.push('\n');
            text.push_str(&String::from_utf8_lossy(&body));
            Response::builder().header("content-type", "text/plain").body(Full::new(Bytes::from(text)))
        }
        _ => Response::builder().status(404).body(Full::new(Bytes::from_static(b"nope"))),
    };
    Ok(resp.unwrap())
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

/// HTTPS server for `localhost` with a certificate from a separate test CA.
async fn serve_https() -> (SocketAddr, CertificateDer<'static>) {
    let (ca_pem, ca_key) = CertAuthority::generate_pem().unwrap();
    let test_ca = CertAuthority::from_pem(&ca_pem, &ca_key).unwrap();
    let leaf = test_ca.leaf_for("localhost").unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Fixed(leaf)));
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(s).await {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(tls), service_fn(upstream_handler))
                        .await;
                }
            });
        }
    });
    (addr, CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap())
}

#[derive(Debug)]
struct Fixed(Arc<rustls::sign::CertifiedKey>);
impl rustls::server::ResolvesServerCert for Fixed {
    fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

async fn start(home: &Home, extra_root: Option<CertificateDer<'static>>) -> Running {
    home.ensure().unwrap();
    let ca = Arc::new(CertAuthority::load_or_create(home).unwrap());
    let store = Store::open(&home.project_db("test")).unwrap();
    let upstream = Upstream::new(false, extra_root.into_iter().collect()).unwrap();
    let engine = Engine::new("test", store, ca, upstream).unwrap();
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

/// Sends one request through the proxy in absolute form.
async fn via_proxy(proxy: SocketAddr, url: &str, headers: &[(&str, &str)]) -> (u16, String) {
    let tcp = TcpStream::connect(proxy).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tcp)).await.unwrap();
    tokio::spawn(conn);
    let uri: hyper::Uri = url.parse().unwrap();
    let mut b = Request::builder().uri(url).header("host", uri.authority().unwrap().as_str());
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let resp = sender.send_request(b.body(Full::new(Bytes::new())).unwrap()).await.unwrap();
    let status = resp.status().as_u16();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

async fn wait_for_count(engine: &Engine, n: i64) {
    for _ in 0..200 {
        if engine.store.count().unwrap() >= n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {n} exchanges, have {}", engine.store.count().unwrap());
}

#[tokio::test]
async fn captures_plain_http_and_searches_it() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;

    let (status, body) = via_proxy(r.proxy_addr, &format!("http://localhost:{}/", up.port()), &[]).await;
    assert_eq!((status, body.contains("welcome home")), (200, true));
    let (status, _) = via_proxy(r.proxy_addr, &format!("http://localhost:{}/missing?x=1", up.port()), &[]).await;
    assert_eq!(status, 404);
    wait_for_count(&r.engine, 2).await;

    let rules = r.engine.rules();
    let (hits, _) = r.engine.store.search(&Query::parse("welcome").unwrap(), &rules, 10, 0).unwrap();
    assert_eq!(hits.len(), 1);
    let ex = r.engine.store.get_exchange(hits[0].id).unwrap().unwrap();
    assert_eq!(ex.host, "localhost");
    assert_eq!(ex.status, Some(200));
    assert!(ex.resp_headers.iter().any(|(k, _)| k == "set-cookie"));
    let (hits, _) = r.engine.store.search(&Query::parse("status:404 path:/missing").unwrap(), &rules, 10, 0).unwrap();
    assert_eq!(hits[0].query, "x=1");
}

#[tokio::test]
async fn intercepts_https_with_minted_certificate() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let (up, upstream_root) = serve_https().await;
    let r = start(&home, Some(upstream_root)).await;

    // CONNECT, then TLS that must verify against the Plonix CA.
    let mut tcp = TcpStream::connect(r.proxy_addr).await.unwrap();
    let target = format!("localhost:{}", up.port());
    tcp.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes()).await.unwrap();
    let mut buf = [0u8; 1024];
    let n = tcp.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));

    let mut roots = rustls::RootCertStore::empty();
    roots.add(r.engine.ca.ca_der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .expect("client trusts the minted certificate");
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tls)).await.unwrap();
    tokio::spawn(conn);
    let req = Request::builder()
        .method("POST")
        .uri("/echo?debug=1")
        .header("host", &target)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Full::new(Bytes::from_static(b"user=alice&pass=s3cret")))
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("user=alice&pass=s3cret"));

    wait_for_count(&r.engine, 1).await;
    let ex = r.engine.store.get_exchange(1).unwrap().unwrap();
    assert_eq!((ex.scheme.as_str(), ex.method.as_str(), ex.path.as_str(), ex.query.as_str()), ("https", "POST", "/echo", "debug=1"));
    assert_eq!(ex.req_body, b"user=alice&pass=s3cret");
    assert_eq!(ex.tls_sans, vec!["localhost".to_string()]);
    let (hits, _) = r.engine.store.search(&Query::parse("s3cret").unwrap(), &r.engine.rules(), 10, 0).unwrap();
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn adaptive_scope_suggests_with_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    r.engine.decide("localhost", Decision::Accepted, false, "seed").unwrap();

    // In-scope page sets a session cookie and links to a CDN.
    via_proxy(r.proxy_addr, &format!("http://localhost:{}/", up.port()), &[]).await;
    // A different host (127.0.0.1) is called from that page and receives the session cookie.
    via_proxy(
        r.proxy_addr,
        &format!("http://127.0.0.1:{}/echo", up.port()),
        &[("referer", &format!("http://localhost:{}/", up.port())), ("cookie", "sid=session-abcdef123456")],
    )
    .await;
    wait_for_count(&r.engine, 2).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let sug = r.engine.store.suggestions(&r.engine.rules()).unwrap();
    let ip = sug.iter().find(|s| s.domain == "127.0.0.1").expect("127.0.0.1 suggested");
    let kinds: Vec<EvidenceKind> = ip.evidence.iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EvidenceKind::RequestedFrom));
    assert!(kinds.contains(&EvidenceKind::SharesSession));
    assert_eq!(ip.score, 7);
    assert!(sug.iter().any(|s| s.domain == "cdn.linked-assets.test" && s.evidence[0].kind == EvidenceKind::LinkedFrom));
    assert_eq!(sug[0].domain, "127.0.0.1", "strongest evidence first");

    // Rejecting hides it; removing the rule brings it back after a rescan.
    r.engine.decide("127.0.0.1", Decision::Rejected, false, "").unwrap();
    assert!(!r.engine.store.suggestions(&r.engine.rules()).unwrap().iter().any(|s| s.domain == "127.0.0.1"));
    r.engine.remove_rule("127.0.0.1").unwrap();
    assert!(r.engine.store.suggestions(&r.engine.rules()).unwrap().iter().any(|s| s.domain == "127.0.0.1"));
}

#[tokio::test]
async fn active_requests_are_refused_until_scope_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    via_proxy(r.proxy_addr, &format!("http://localhost:{}/echo", up.port()), &[("x-test", "1")]).await;
    wait_for_count(&r.engine, 1).await;

    let send = SendRequest { method: "GET".into(), url: format!("http://localhost:{}/echo", up.port()), ..Default::default() };
    let err = r.engine.send(send.clone(), "test").await.unwrap_err();
    assert!(matches!(err, SendError::OutOfScope { .. }), "{err}");
    let err = r.engine.replay(ReplayRequest { id: 1, ..Default::default() }, "test").await.unwrap_err();
    assert!(matches!(err, SendError::OutOfScope { .. }));
    assert_eq!(r.engine.store.count().unwrap(), 1, "nothing was sent");

    r.engine.decide("localhost", Decision::Rejected, false, "").unwrap();
    assert!(matches!(r.engine.send(send.clone(), "test").await, Err(SendError::OutOfScope { decision: "rejected", .. })));

    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    let ex = r.engine.send(send, "test").await.unwrap();
    assert_eq!(ex.status, Some(200));

    let replayed = r
        .engine
        .replay(
            ReplayRequest {
                id: 1,
                method: Some("PUT".into()),
                target: Some("/echo?id=2".into()),
                set_headers: vec![("X-Test".into(), "2".into())],
                body: Some("changed".into()),
                ..Default::default()
            },
            "mcp",
        )
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&replayed.resp_body).into_owned();
    assert!(text.starts_with("PUT /echo?id=2"), "{text}");
    assert!(text.contains("x-test: 2"));
    assert!(text.ends_with("changed"));
    let stored = r.engine.store.get_exchange(replayed.id).unwrap().unwrap();
    assert_eq!(stored.initiator.as_deref(), Some("mcp"));
    assert_eq!(r.engine.store.search(&Query::parse("source:replay").unwrap(), &r.engine.rules(), 10, 0).unwrap().1, 2);
}

#[tokio::test]
async fn api_requires_token_and_loopback_host() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    via_proxy(r.proxy_addr, &format!("http://localhost:{}/", up.port()), &[]).await;
    wait_for_count(&r.engine, 1).await;
    let base = format!("http://{}", r.api_addr);
    let token = r.token.clone();
    let port = up.port();

    tokio::task::spawn_blocking(move || {
        let call = |req: ureq::Request| match req.call() {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json::<serde_json::Value>().unwrap()),
            Err(e) => panic!("{e}"),
        };
        let auth = format!("Bearer {token}");
        assert_eq!(call(ureq::get(&format!("{base}/api/status"))).0, 401);
        assert_eq!(call(ureq::get(&format!("{base}/api/status")).set("Authorization", "Bearer wrong")).0, 401);
        assert_eq!(call(ureq::get(&format!("{base}/api/status")).set("Authorization", &auth).set("Host", "evil.test")).0, 403);

        let (code, status) = call(ureq::get(&format!("{base}/api/status")).set("Authorization", &auth));
        assert_eq!((code, status["exchanges"].as_i64()), (200, Some(1)));

        let (_, traffic) = call(ureq::get(&format!("{base}/api/traffic?q=welcome")).set("Authorization", &auth));
        assert_eq!(traffic["total"], 1);
        let (_, ex) = call(ureq::get(&format!("{base}/api/traffic/1")).set("Authorization", &auth));
        assert!(ex["resp_text"].as_str().unwrap().contains("welcome home"));
        let (code, insights) = call(ureq::get(&format!("{base}/api/traffic/1/insights")).set("Authorization", &auth));
        assert_eq!(code, 200);
        assert!(insights.is_array(), "{insights}");
        assert_eq!(call(ureq::get(&format!("{base}/api/traffic/99/insights")).set("Authorization", &auth)).0, 404);

        let send = serde_json::json!({ "method": "GET", "url": format!("http://localhost:{port}/echo") });
        let (code, body) = match ureq::post(&format!("{base}/api/send")).set("Authorization", &auth).send_json(send.clone()) {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json().unwrap()),
            Err(e) => panic!("{e}"),
        };
        assert_eq!((code, body["code"].as_str()), (403, Some("out_of_scope")));

        ureq::post(&format!("{base}/api/scope/accept"))
            .set("Authorization", &auth)
            .send_json(serde_json::json!({ "domain": "localhost" }))
            .unwrap();
        let r = ureq::post(&format!("{base}/api/send"))
            .set("Authorization", &auth)
            .set("X-Plonix-Client", "cli")
            .send_json(send)
            .unwrap();
        let body: serde_json::Value = r.into_json().unwrap();
        assert_eq!(body["status"], 200);
        assert_eq!(body["initiator"], "cli");

        let r = ureq::post(&format!("{base}/api/findings"))
            .set("Authorization", &auth)
            .send_json(serde_json::json!({ "title": "Reflected header", "severity": "low", "exchange_ids": [1] }))
            .unwrap();
        assert_eq!(r.status(), 201);
        let (_, findings) = call(ureq::get(&format!("{base}/api/findings")).set("Authorization", &auth));
        assert_eq!(findings[0]["title"], "Reflected header");

        // Saved view state: the Traffic filters.
        let (code, empty) = call(ureq::get(&format!("{base}/api/views/traffic")).set("Authorization", &auth));
        assert_eq!((code, empty), (200, serde_json::json!({})));
        let state = serde_json::json!({ "filters": [{ "term": "kind:static", "mode": "exclude" }], "text": "" });
        let r = ureq::put(&format!("{base}/api/views/traffic")).set("Authorization", &auth).send_json(state.clone()).unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(call(ureq::get(&format!("{base}/api/views/traffic")).set("Authorization", &auth)).1, state);
        assert_eq!(call(ureq::get(&format!("{base}/api/views/bad%20name")).set("Authorization", &auth)).0, 400);
        let bad = ureq::put(&format!("{base}/api/views/traffic")).set("Authorization", &auth).send_json(serde_json::json!([1]));
        assert!(matches!(bad, Err(ureq::Error::Status(400, _))));
        let (_, hidden) = call(ureq::get(&format!("{base}/api/traffic?q=-kind:static%20-status:2xx,3xx")).set("Authorization", &auth));
        assert_eq!(hidden["total"], 0, "{hidden}");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn proxy_serves_ca_certificate_page() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let r = start(&home, None).await;
    let (status, body) = via_proxy(r.proxy_addr, "http://plonix/ca.pem", &[]).await;
    assert_eq!(status, 200);
    assert_eq!(body, r.engine.ca.ca_pem());
    let (_, page) = via_proxy(r.proxy_addr, "http://plonix/", &[]).await;
    assert!(page.contains(&r.engine.ca.fingerprint()));
    assert_eq!(r.engine.store.count().unwrap(), 0, "local pages are not recorded");
}

#[cfg(unix)]
#[tokio::test]
async fn api_opens_the_capture_browser() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().join("home") };
    let r = start(&home, None).await;

    // A stand-in browser that records how it was launched.
    let args_file = dir.path().join("args.txt");
    let fake = dir.path().join("fake-chrome");
    std::fs::write(&fake, format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n", args_file.display())).unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // SAFETY: no other test in this binary reads or writes the environment.
    unsafe { std::env::set_var("PLONIX_BROWSER", &fake) };

    let base = format!("http://{}", r.api_addr);
    let auth = format!("Bearer {}", r.token);
    let (bad, opened) = tokio::task::spawn_blocking(move || {
        let post = |body: serde_json::Value| match ureq::post(&format!("{base}/api/browser/open")).set("Authorization", &auth).send_json(body) {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json::<serde_json::Value>().unwrap()),
            Err(e) => panic!("{e}"),
        };
        (post(serde_json::json!({ "target": "ftp://shop.test" })), post(serde_json::json!({ "target": "shop.test/login" })))
    })
    .await
    .unwrap();
    assert_eq!((bad.0, bad.1["code"].as_str()), (400, Some("bad_target")));
    assert_eq!(opened.0, 200, "{}", opened.1);
    assert_eq!(opened.1["url"], "https://shop.test/login");
    assert_eq!(opened.1["browser"], "fake-chrome");
    assert_eq!(r.engine.rules().decide("app.shop.test"), Decision::Accepted);

    let mut args = String::new();
    for _ in 0..200 {
        args = std::fs::read_to_string(&args_file).unwrap_or_default();
        if args.ends_with("https://shop.test/login\n") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(args.contains(&format!("--proxy-server=http://{}", r.proxy_addr)), "{args}");
    assert!(args.contains(&format!("--ignore-certificate-errors-spki-list={}", r.engine.ca.spki_sha256())), "{args}");
    assert!(args.ends_with("https://shop.test/login\n"), "{args}");
}
