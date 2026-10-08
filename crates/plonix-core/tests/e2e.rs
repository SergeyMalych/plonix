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
        "/gz" => {
            use std::io::Write;
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            gz.write_all(b"compressed hello").unwrap();
            Response::builder()
                .header("content-type", "text/plain")
                .header("content-encoding", "gzip")
                .body(Full::new(Bytes::from(gz.finish().unwrap())))
        }
        "/site" => Response::builder().header("content-type", "text/html").body(Full::new(Bytes::from(
            "<a href=\"/site/a\">a</a> <a href='/site/b?id=1'>b</a> <a href=\"https://evil.test/x\">off</a><form method=\"post\" action=\"/login\"><input name=\"user\"></form>",
        ))),
        "/site/a" => Response::builder().header("content-type", "text/html").body(Full::new(Bytes::from("<a href=\"/site/c\">c</a>"))),
        "/site/b" => Response::builder().header("content-type", "text/html").body(Full::new(Bytes::from("<p>b</p>"))),
        "/site/c" => Response::builder().header("content-type", "text/html").body(Full::new(Bytes::from("<p>c</p>"))),
        "/.git/config" => Response::builder()
            .header("content-type", "text/plain")
            .body(Full::new(Bytes::from_static(b"[core]\n\trepositoryformatversion = 0\n"))),
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
    std::fs::create_dir_all(home.root.join("projects")).unwrap();
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
    assert_eq!(ex.http_version, "HTTP/1.1", "the server only speaks HTTP/1.1");
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
async fn crawl_discovers_linked_pages_and_stays_in_scope() {
    use plonix_core::crawl::CrawlRequest;

    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;

    // One captured request so the engine knows the host's scheme and port.
    via_proxy(r.proxy_addr, &format!("http://localhost:{}/site", up.port()), &[]).await;
    wait_for_count(&r.engine, 1).await;

    // Refused until the host is accepted.
    let refused = r.engine.crawl(CrawlRequest { host: "localhost".into(), ..Default::default() }, "crawl").await;
    assert!(matches!(refused, Err(SendError::OutOfScope { .. })));

    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    let report = r
        .engine
        .crawl(CrawlRequest { host: "localhost".into(), start: Some("/site".into()), ..Default::default() }, "crawl")
        .await
        .unwrap();

    assert!(report.pages_fetched >= 4, "should have followed links to /site/a,b,c: {report:?}");
    // The off-host link was not followed.
    let endpoints = r.engine.store.endpoints("localhost").unwrap();
    let paths: Vec<&str> = endpoints.iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"/site/a") && paths.contains(&"/site/c"), "crawl should reach linked pages: {paths:?}");
    assert!(report.forms.iter().any(|f| f.action.ends_with("/login") && f.fields.contains(&"user".to_string())));
    assert!(!r.engine.store.count().is_err());
    // evil.test was never requested.
    assert!(r.engine.rules().decide("evil.test") != Decision::Accepted);
}

#[tokio::test]
async fn active_scan_finds_a_real_exposure_and_stays_in_scope() {
    use plonix_core::scan::ScanRequest;

    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;

    // Capture one request so /echo is a discovered endpoint for the scan.
    via_proxy(r.proxy_addr, &format!("http://localhost:{}/echo?q=1", up.port()), &[]).await;
    wait_for_count(&r.engine, 1).await;

    // A scan against an un-accepted host is refused before any request.
    let out_of_scope = r.engine.scan(ScanRequest { host: "localhost".into(), ..Default::default() }, "test").await;
    assert!(matches!(out_of_scope, Err(SendError::OutOfScope { .. })), "scan must refuse an un-accepted host");

    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    let report = r.engine.scan(ScanRequest { host: "localhost".into(), ..Default::default() }, "scan").await.unwrap();

    // The fixed-path git probe and the reflected-parameter check both fire.
    let titles: Vec<&str> = report.findings.iter().map(|f| f.title.as_str()).collect();
    assert!(titles.iter().any(|t| t.contains(".git/config")), "expected a .git/config finding, got {titles:?}");
    assert!(titles.iter().any(|t| t.contains("reflected")), "expected a reflected-parameter finding, got {titles:?}");
    assert!(report.requests_sent >= 2);
    assert!(report.tactics_run.iter().any(|t| t == "exposed-git-config"));

    // Every request the scan sent was recorded against the in-scope host.
    let findings = r.engine.store.findings().unwrap();
    assert!(findings.iter().any(|f| f.title.contains(".git/config") && f.severity == "high"));

    // A rejected host is refused too, even after being known.
    r.engine.decide("localhost", Decision::Rejected, false, "").unwrap();
    assert!(matches!(
        r.engine.scan(ScanRequest { host: "localhost".into(), ..Default::default() }, "scan").await,
        Err(SendError::OutOfScope { decision: "rejected", .. })
    ));
}

#[tokio::test]
async fn a_focused_scan_only_aims_at_the_chosen_endpoint() {
    use plonix_core::scan::{EndpointSel, ScanRequest};

    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;

    // Two discovered endpoints: /echo reflects its input, /site/b does not.
    via_proxy(r.proxy_addr, &format!("http://localhost:{}/echo?q=1", up.port()), &[]).await;
    via_proxy(r.proxy_addr, &format!("http://localhost:{}/site/b?id=1", up.port()), &[]).await;
    wait_for_count(&r.engine, 2).await;
    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();

    let reflected = |rep: plonix_core::scan::ScanReport| rep.findings.into_iter().filter(|f| f.title.contains("reflected")).count();

    // Focused on the reflecting endpoint: the reflected-parameter check fires.
    let on_echo = r
        .engine
        .scan(
            ScanRequest {
                host: "localhost".into(),
                tactics: vec!["reflected-parameter".into()],
                endpoints: vec![EndpointSel { method: "GET".into(), path: "/echo".into() }],
                ..Default::default()
            },
            "scan",
        )
        .await
        .unwrap();
    assert!(reflected(on_echo) >= 1, "focusing on /echo should surface the reflected-parameter finding");

    // Focused on the other endpoint only: the check never touches /echo, so
    // nothing reflects — proof the scan was narrowed to the chosen endpoint.
    let on_other = r
        .engine
        .scan(
            ScanRequest {
                host: "localhost".into(),
                tactics: vec!["reflected-parameter".into()],
                endpoints: vec![EndpointSel { method: "GET".into(), path: "/site/b".into() }],
                ..Default::default()
            },
            "scan",
        )
        .await
        .unwrap();
    assert_eq!(reflected(on_other), 0, "focusing on /site/b must not probe /echo");
}

#[tokio::test]
async fn payload_run_feeds_positions_and_stays_in_scope() {
    use plonix_core::runs::{Payloads, RunMode, RunRequest};

    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;

    let base = format!("http://localhost:{}", up.port());
    let list = |v: &[&str]| Payloads::Values { values: v.iter().map(|s| s.to_string()).collect() };

    // A run against an un-accepted host is refused before any request goes out.
    let req = RunRequest {
        url: format!("{base}/echo?id=•1•"),
        raw: "Accept: */*\n\n".into(),
        lists: vec![list(&["1", "2", "3"])],
        mode: RunMode::Sweep,
        include_base: true,
        delay_ms: Some(0),
        ..Default::default()
    };
    assert!(matches!(r.engine.run(req.clone(), "test").await, Err(SendError::OutOfScope { .. })), "run must refuse an un-accepted host");
    assert_eq!(r.engine.store.count().unwrap(), 0, "nothing was sent before scope was accepted");

    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    let report = r.engine.run(req, "bench").await.unwrap();

    // Baseline + one request per value; the baseline carries the base value.
    assert_eq!(report.positions, 1);
    assert_eq!(report.requests_sent, 4);
    assert!(report.rows[0].baseline);
    let sent: Vec<&str> = report.rows.iter().map(|row| row.values[0].as_str()).collect();
    assert_eq!(sent, vec!["1", "1", "2", "3"]);

    // Each request actually carried its own payload value to the server.
    for row in &report.rows {
        let ex = r.engine.store.get_exchange(row.exchange_id).unwrap().unwrap();
        let text = String::from_utf8_lossy(&ex.resp_body);
        assert!(text.contains(&format!("/echo?id={}", row.values[0])), "row {} did not carry its payload to the server: {text}", row.n);
        assert_eq!(ex.initiator.as_deref(), Some("bench"));
    }

    // A budget stops the run short and says so.
    let capped = r
        .engine
        .run(
            RunRequest {
                url: format!("{base}/echo?id=•1•"),
                lists: vec![Payloads::Range { from: 1, to: 100, step: 1 }],
                mode: RunMode::Sweep,
                max_requests: Some(5),
                delay_ms: Some(0),
                ..Default::default()
            },
            "bench",
        )
        .await
        .unwrap();
    assert!(capped.truncated);
    assert_eq!(capped.requests_sent, 5);
    assert_eq!(capped.planned, 100);

    // A rejected host is refused too.
    r.engine.decide("localhost", Decision::Rejected, false, "").unwrap();
    assert!(matches!(
        r.engine
            .run(RunRequest { url: format!("{base}/echo?id=•1•"), lists: vec![list(&["1"])], mode: RunMode::Sweep, delay_ms: Some(0), ..Default::default() }, "bench")
            .await,
        Err(SendError::OutOfScope { decision: "rejected", .. })
    ));
}

#[tokio::test]
async fn demo_responder_answers_a_run_with_no_network() {
    use plonix_core::runs::{Payloads, RunMode, RunRequest};

    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    // No upstream server is started: the only way a request can get a response
    // is through the installed responder.
    let r = start(&home, None).await;
    r.engine.set_responder(plonix_core::demo::responder());
    r.engine.decide("api.brightcart.example", Decision::Accepted, false, "").unwrap();

    let report = r
        .engine
        .run(
            RunRequest {
                url: "https://api.brightcart.example/v1/orders/•1042•".into(),
                raw: "Accept: application/json\n\n".into(),
                lists: vec![Payloads::Range { from: 1038, to: 1046, step: 1 }],
                mode: RunMode::Sweep,
                delay_ms: Some(0),
                ..Default::default()
            },
            "bench",
        )
        .await
        .unwrap();

    // Every id in range answered 200 without a real network, and the responses
    // are not all identical (different ids, different customers).
    assert_eq!(report.requests_sent, 9);
    assert!(report.rows.iter().all(|row| row.status == Some(200)), "every demo id answers 200");
    let lengths: std::collections::BTreeSet<usize> = report.rows.iter().map(|row| row.length).collect();
    assert!(lengths.len() > 1, "orders differ in length across ids");
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

/// Agents sign in with their own token and can only read: everything that
/// sends traffic or changes the project is refused by the engine itself.
#[tokio::test]
async fn agent_token_is_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    via_proxy(r.proxy_addr, &format!("http://localhost:{}/", up.port()), &[]).await;
    wait_for_count(&r.engine, 1).await;
    assert_ne!(r.agent_token, r.token);
    assert_eq!(std::fs::read_to_string(home.agent_token()).unwrap().trim(), r.agent_token);
    let base = format!("http://{}", r.api_addr);
    let (user, agent) = (format!("Bearer {}", r.token), format!("Bearer {}", r.agent_token));
    let port = up.port();

    tokio::task::spawn_blocking(move || {
        let status = |r: Result<ureq::Response, ureq::Error>| match r {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json::<serde_json::Value>().unwrap()),
            Err(e) => panic!("{e}"),
        };
        let get = |path: &str, auth: &str| status(ureq::get(&format!("{base}{path}")).set("Authorization", auth).set("X-Plonix-Client", "test-agent").call());
        let post = |path: &str, auth: &str, body: serde_json::Value| {
            status(ureq::post(&format!("{base}{path}")).set("Authorization", auth).set("X-Plonix-Client", "test-agent").send_json(body))
        };

        // Agents see in-scope traffic by default, so accept the captured host first.
        post("/api/scope/accept", &user, serde_json::json!({ "domain": "localhost" }));
        for path in ["/api/status", "/api/traffic?q=welcome", "/api/traffic/1", "/api/traffic/1/insights", "/api/hosts", "/api/tech", "/api/scope", "/api/findings"] {
            assert_eq!(get(path, &agent).0, 200, "agent should read {path}");
        }
        let (_, ex) = get("/api/traffic/1", &agent);
        assert!(ex["resp_text"].as_str().unwrap().contains("welcome home"));

        // Even with the host in scope, an agent cannot send, replay or change anything.
        let refused = [
            ("/api/send", serde_json::json!({ "method": "GET", "url": format!("http://localhost:{port}/echo") })),
            ("/api/replay", serde_json::json!({ "id": 1 })),
            ("/api/scope/accept", serde_json::json!({ "domain": "evil.test" })),
            ("/api/scope/remove", serde_json::json!({ "domain": "localhost" })),
            ("/api/findings", serde_json::json!({ "title": "x", "severity": "low" })),
            ("/api/ui/launch", serde_json::json!({})),
            ("/api/browser/open", serde_json::json!({ "target": "example.com" })),
            ("/api/browser/install", serde_json::json!({})),
            ("/api/ca/trust", serde_json::json!({})),
            ("/api/shutdown", serde_json::json!({})),
        ];
        for (path, body) in refused {
            let (code, body) = post(path, &agent, body);
            assert_eq!((code, body["code"].as_str()), (403, Some("agent_not_allowed")), "{path}");
        }
        let (_, scope) = get("/api/scope", &user);
        assert_eq!(scope["rules"].as_array().unwrap().len(), 1, "scope unchanged: {scope}");
        assert_eq!(get("/api/status", &user).1["exchanges"], 1, "nothing was sent");
        assert!(get("/api/findings", &user).1.as_array().unwrap().is_empty());

        // The user sees the policy and who connected; the agent sees only the policy.
        let (_, a) = get("/api/agents", &user);
        assert_eq!(a["mode"], "read-only");
        let c = &a["clients"][0];
        assert_eq!(c["name"], "test-agent");
        assert_eq!(c["refused"], 10);
        let (_, a) = get("/api/agents", &agent);
        assert!(a["clients"].is_null() && a["capabilities"].is_array());
    })
    .await
    .unwrap();
    r.engine.shutdown.notify_waiters();
}

/// The user edits, closes, deletes and exports findings; agents can read and
/// export them but not change them, and only see in-scope evidence.
#[tokio::test]
async fn findings_can_be_finished_by_the_user_only() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    via_proxy(r.proxy_addr, &format!("http://localhost:{}/", up.port()), &[]).await;
    wait_for_count(&r.engine, 1).await;
    let base = format!("http://{}", r.api_addr);
    let (user, agent) = (format!("Bearer {}", r.token), format!("Bearer {}", r.agent_token));

    tokio::task::spawn_blocking(move || {
        let json = |r: Result<ureq::Response, ureq::Error>| match r {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json::<serde_json::Value>().unwrap()),
            Err(e) => panic!("{e}"),
        };
        let text = |path: &str, auth: &str| {
            let r = ureq::get(&format!("{base}{path}")).set("Authorization", auth).call().unwrap();
            (r.header("content-type").unwrap_or("").to_string(), r.header("content-disposition").unwrap_or("").to_string(), r.into_string().unwrap())
        };
        let req = |method: &str, path: &str, auth: &str| ureq::request(method, &format!("{base}{path}")).set("Authorization", auth);

        let (code, f) = json(req("POST", "/api/findings", &user).send_json(serde_json::json!({ "title": "Open redirect", "severity": "Medium", "exchange_ids": [1] })));
        assert_eq!((code, f["severity"].as_str(), f["status"].as_str()), (201, Some("medium"), Some("open")));
        let id = f["id"].as_i64().unwrap();
        let path = format!("/api/findings/{id}");

        // Agents read and export, and cannot edit or delete.
        assert_eq!(json(req("GET", &path, &agent).call()).1["title"], "Open redirect");
        for method in ["PATCH", "PUT", "DELETE"] {
            let (code, body) = json(req(method, &path, &agent).send_json(serde_json::json!({ "status": "fixed" })));
            assert_eq!((code, body["code"].as_str()), (403, Some("agent_not_allowed")), "{method}");
        }
        // localhost is not in scope, so an agent's report leaves the request out.
        let (_, _, md) = text("/api/findings/export?format=md", &agent);
        assert!(md.contains("Open redirect") && md.contains("agents may see in-scope traffic only") && !md.contains("welcome home"), "{md}");

        // The user edits it.
        let (code, body) = json(req("PATCH", &path, &user).send_json(serde_json::json!({ "status": "nonsense" })));
        assert_eq!((code, body["code"].as_str()), (400, Some("bad_request")));
        let (code, f) = json(req("PATCH", &path, &user).send_json(serde_json::json!({ "title": "Open redirect on /", "status": "confirmed", "description": "Steps" })));
        assert_eq!((code, f["title"].as_str(), f["status"].as_str(), f["severity"].as_str()), (200, Some("Open redirect on /"), Some("confirmed"), Some("medium")));
        assert_eq!(json(req("PATCH", "/api/findings/999", &user).send_json(serde_json::json!({ "status": "fixed" }))).0, 404);

        let (ctype, disposition, html) = text("/api/findings/export?format=html", &user);
        assert!(ctype.starts_with("text/html") && disposition.contains("attachment; filename=\"plonix-findings-"));
        assert!(html.contains("Open redirect on /") && html.contains("welcome home") && html.contains("Steps"));
        let (_, _, js) = text(&format!("/api/findings/export?format=json&ids={id}"), &user);
        let v: serde_json::Value = serde_json::from_str(&js).unwrap();
        assert_eq!(v["findings"][0]["evidence"][0]["method"], "GET");
        assert_eq!(json(req("GET", "/api/findings/export?format=pdf", &user).call()).0, 400);

        // A false positive leaves the default report.
        json(req("PATCH", &path, &user).send_json(serde_json::json!({ "status": "false positive" })));
        assert!(!text("/api/findings/export", &user).2.contains("Open redirect"));

        assert_eq!(json(req("DELETE", &path, &user).call()).1["deleted"], id);
        assert_eq!(json(req("DELETE", &path, &user).call()).0, 404);
        assert_eq!(json(req("GET", &path, &user).call()).0, 404);
        assert!(json(req("GET", "/api/findings", &user).call()).1.as_array().unwrap().is_empty());
    })
    .await
    .unwrap();
    r.engine.shutdown.notify_waiters();
}

/// The user narrows agent access with settings, and "Ask Claude Code"
/// bundles just one spot's context, clipped and within the budget.
#[tokio::test]
async fn agent_settings_and_ask_context() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    via_proxy(r.proxy_addr, &format!("http://localhost:{}/", up.port()), &[]).await;
    wait_for_count(&r.engine, 1).await;
    let base = format!("http://{}", r.api_addr);
    let (user, agent) = (format!("Bearer {}", r.token), format!("Bearer {}", r.agent_token));

    tokio::task::spawn_blocking(move || {
        let status = |r: Result<ureq::Response, ureq::Error>| match r {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json::<serde_json::Value>().unwrap()),
            Err(e) => panic!("{e}"),
        };
        let get = |path: &str, auth: &str| status(ureq::get(&format!("{base}{path}")).set("Authorization", auth).call());
        let put = |path: &str, auth: &str, b: serde_json::Value| {
            status(ureq::put(&format!("{base}{path}")).set("Authorization", auth).send_json(b))
        };
        let post = |path: &str, auth: &str, b: serde_json::Value| {
            status(ureq::post(&format!("{base}{path}")).set("Authorization", auth).send_json(b))
        };

        // localhost is not in scope yet: with the default in-scope-only data
        // scope, the agent cannot see it.
        assert_eq!(get("/api/traffic/1", &agent).0, 403);
        assert_eq!(get("/api/traffic/1", &agent).1["code"], "outside_agent_data");
        assert!(get("/api/hosts", &agent).1.as_array().unwrap().is_empty());

        // AgentSettings uses serde defaults, so a partial body sets the rest.
        put("/api/agents/settings", &user, serde_json::json!({ "data": "all" }));
        assert_eq!(get("/api/traffic/1", &agent).0, 200);

        // Agents cannot change their own settings; the route is not theirs.
        assert_eq!(put("/api/agents/settings", &agent, serde_json::json!({ "enabled": false })).0, 403);

        // Switching off the Traffic group hides those reads but not insights.
        put("/api/agents/settings", &user, serde_json::json!({ "data": "all", "off": ["traffic"] }));
        assert_eq!(get("/api/traffic/1", &agent).1["code"], "capability_off");
        assert_eq!(get("/api/traffic/1/insights", &agent).0, 200);
        put("/api/agents/settings", &user, serde_json::json!({ "data": "all" }));

        // Ask Claude Code about request #1: a bundle of named parts.
        let (code, b) = post("/api/agents/ask", &user, serde_json::json!({ "kind": "request", "id": 1 }));
        assert_eq!(code, 200);
        assert!(b["prompt"].as_str().unwrap().contains("welcome home"));
        let ids: Vec<&str> = b["parts"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
        assert!(ids.contains(&"request") && ids.contains(&"response"));
        assert!(b["tokens"].as_u64().unwrap() > 0 && b["over_budget"] == false);

        // A tiny budget trips the over-budget flag; dropping parts brings it down.
        put("/api/agents/settings", &user, serde_json::json!({ "data": "all", "context_budget": 500 }));
        let (_, big) = post("/api/agents/ask", &user, serde_json::json!({ "kind": "request", "id": 1, "max_body_chars": 100000 }));
        let (_, small) = post("/api/agents/ask", &user, serde_json::json!({ "kind": "request", "id": 1, "exclude": ["response"], "max_body_chars": 200 }));
        assert!(small["tokens"].as_u64().unwrap() < big["tokens"].as_u64().unwrap());
        assert!(!small["prompt"].as_str().unwrap().contains("## Response"));

        // Missing subject is a 404.
        assert_eq!(post("/api/agents/ask", &user, serde_json::json!({ "kind": "request", "id": 999 })).0, 404);

        // Turned off, nothing is readable and Ask is refused.
        put("/api/agents/settings", &user, serde_json::json!({ "enabled": false }));
        assert_eq!(get("/api/status", &agent).1["code"], "agents_disabled");
        assert_eq!(post("/api/agents/ask", &user, serde_json::json!({ "kind": "request", "id": 1 })).0, 403);
    })
    .await
    .unwrap();
    r.engine.shutdown.notify_waiters();
}

/// An agent suggests an edit to a Bench draft through the MCP tool. The
/// suggestion is only stored: nothing is sent, even to a host in scope, and
/// only the user can list it, compare it with their draft and drop it.
#[tokio::test]
async fn agents_suggest_bench_edits_but_never_send_them() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    // `plonix mcp` finds the engine through this file, as it does for a session.
    let info = plonix_core::paths::EngineInfo {
        pid: std::process::id(),
        api: format!("http://{}", r.api_addr),
        proxy: r.proxy_addr.to_string(),
        project: "test".into(),
        started_at: 0,
        project_id: String::new(),
        project_dir: None,
    };
    std::fs::write(home.engine_file(), serde_json::to_vec(&info).unwrap()).unwrap();
    let base = format!("http://{}", r.api_addr);
    let (user, agent) = (format!("Bearer {}", r.token), format!("Bearer {}", r.agent_token));
    let target = format!("http://localhost:{}/echo", up.port());

    tokio::task::spawn_blocking(move || {
        let status = |r: Result<ureq::Response, ureq::Error>| match r {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap_or_default()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json::<serde_json::Value>().unwrap_or_default()),
            Err(e) => panic!("{e}"),
        };
        let call = |method: &str, path: &str, auth: &str, b: serde_json::Value| {
            let req = ureq::request(method, &format!("{base}{path}")).set("Authorization", auth);
            status(if method == "GET" || method == "DELETE" { req.call() } else { req.send_json(b) })
        };
        let tool = |args: serde_json::Value| {
            let msg = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "propose_bench_edit", "arguments": args } });
            plonix_core::mcp::handle_message(&home, &msg).unwrap()["result"].clone()
        };
        let listed = || {
            let msg = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" });
            plonix_core::mcp::handle_message(&home, &msg).unwrap()["result"]["tools"].as_array().unwrap().iter().any(|t| t["name"] == "propose_bench_edit")
        };

        // The host is in scope, so a send would go through if anything tried one.
        call("POST", "/api/scope/accept", &user, serde_json::json!({ "domain": "localhost" }));
        assert!(listed());

        let out = tool(serde_json::json!({
            "draft_id": "d1", "summary": "Ask for the admin role", "method": "POST", "url": target,
            "headers": [{ "name": "Content-Type", "value": "application/json" }], "body": "{\"role\":\"admin\"}"
        }));
        assert_eq!(out["isError"], false, "{out}");
        assert!(out["content"][0]["text"].as_str().unwrap().contains("Nothing was sent"));
        let bad = tool(serde_json::json!({ "draft_id": "d1", "method": "GET", "url": "file:///etc/passwd" }));
        assert_eq!(bad["isError"], true);

        // Nothing went out: no exchange was recorded.
        assert_eq!(call("GET", "/api/status", &user, serde_json::json!(null)).1["exchanges"], 0);

        // The agent can only suggest: not list, compare, drop, run or send.
        for (method, path) in [("GET", "/api/bench/proposals"), ("POST", "/api/bench/proposals/1/diff"), ("DELETE", "/api/bench/proposals/1"), ("POST", "/api/send"), ("POST", "/api/run")] {
            let (code, v) = call(method, path, &agent, serde_json::json!({ "method": "POST", "url": target }));
            assert_eq!((code, v["code"].as_str()), (403, Some("agent_not_allowed")), "{method} {path}");
        }

        // The user sees it for that draft and compares it with the draft as it is now.
        let (_, l) = call("GET", "/api/bench/proposals?draft=d1", &user, serde_json::json!(null));
        let p = &l["proposals"][0];
        assert_eq!((p["id"].as_u64(), p["from"].as_str(), p["summary"].as_str()), (Some(1), Some("mcp"), Some("Ask for the admin role")));
        assert!(call("GET", "/api/bench/proposals?draft=other", &user, serde_json::json!(null)).1["proposals"].as_array().unwrap().is_empty());
        let draft = serde_json::json!({ "method": "GET", "url": target, "headers": [["Content-Type", "application/json"]], "body": "{\"role\":\"user\"}" });
        let (code, d) = call("POST", "/api/bench/proposals/1/diff", &user, draft.clone());
        assert_eq!(code, 200);
        assert_eq!((d["diff"]["method"]["old"].as_str(), d["diff"]["method"]["new"].as_str()), (Some("GET"), Some("POST")));
        assert_eq!(d["diff"]["body"]["view"], "json");
        assert_eq!(d["diff"]["proposed"]["body"], "{\"role\":\"admin\"}");
        assert_eq!(call("GET", "/api/status", &user, serde_json::json!(null)).1["exchanges"], 0, "comparing sends nothing either");

        // Applied or discarded, it is dropped.
        assert_eq!(call("DELETE", "/api/bench/proposals/1", &user, serde_json::json!(null)).0, 200);
        assert_eq!(call("POST", "/api/bench/proposals/1/diff", &user, draft).0, 404);

        // Switched off in Settings › AI agents, the tool disappears and is refused.
        call("PUT", "/api/agents/settings", &user, serde_json::json!({ "off": ["bench"] }));
        assert!(!listed());
        let out = tool(serde_json::json!({ "draft_id": "d1", "method": "GET", "url": target }));
        assert_eq!(out["isError"], true);
        assert!(call("GET", "/api/bench/proposals", &user, serde_json::json!(null)).1["proposals"].as_array().unwrap().is_empty());
    })
    .await
    .unwrap();
    r.engine.shutdown.notify_waiters();
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
    assert_eq!(opened.1["needs_trust"], false, "Chromium trusts the CA by its key pin");
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

#[tokio::test]
async fn exclusions_groups_domains_and_custom() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let r = start(&home, None).await;
    let base = format!("http://{}", r.api_addr);
    let token = r.token.clone();

    tokio::task::spawn_blocking(move || {
        let auth = format!("Bearer {token}");
        let get = |path: &str| match ureq::get(&format!("{base}{path}")).set("Authorization", &auth).call() {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json::<serde_json::Value>().unwrap()),
            Err(e) => panic!("{e}"),
        };
        let post = |path: &str, body: serde_json::Value| match ureq::post(&format!("{base}{path}")).set("Authorization", &auth).send_json(body) {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json::<serde_json::Value>().unwrap()),
            Err(e) => panic!("{e}"),
        };

        // The one-time prompt starts unanswered, and the built-in groups are offered.
        let (code, ex) = get("/api/scope/exclusions");
        assert_eq!(code, 200);
        assert_eq!(ex["asked"], false);
        let groups = ex["groups"].as_array().unwrap();
        let analytics = groups.iter().find(|g| g["id"] == "analytics").unwrap();
        assert_eq!(analytics["state"], "off");

        // Turning a group on excludes every member.
        let (code, ex) = post("/api/scope/exclusions/group", serde_json::json!({ "id": "analytics", "on": true }));
        assert_eq!(code, 200);
        let analytics = ex["groups"].as_array().unwrap().iter().find(|g| g["id"] == "analytics").unwrap().clone();
        assert_eq!(analytics["state"], "on");
        assert!(analytics["domains"].as_array().unwrap().iter().all(|d| d["excluded"] == true));

        // An excluded host is out of scope: the engine refuses to send to it.
        let send = serde_json::json!({ "method": "GET", "url": "https://google-analytics.com/collect" });
        let (code, body) = match ureq::post(&format!("{base}/api/send")).set("Authorization", &auth).send_json(send) {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json().unwrap()),
            Err(e) => panic!("{e}"),
        };
        assert_eq!((code, body["code"].as_str()), (403, Some("out_of_scope")));

        // Turning one member off makes the group partial.
        let (_, ex) = post("/api/scope/exclusions/domain", serde_json::json!({ "id": "analytics", "host": "google-analytics.com", "on": false }));
        let analytics = ex["groups"].as_array().unwrap().iter().find(|g| g["id"] == "analytics").unwrap().clone();
        assert_eq!(analytics["state"], "partial");

        // A custom group can be created, enabled and deleted.
        let (code, res) = post("/api/scope/exclusions/custom", serde_json::json!({ "id": "", "name": "Vendor widgets", "domains": ["widget.vendor.test", "cdn.vendor.test"] }));
        assert_eq!(code, 200);
        let gid = res["group"]["id"].as_str().unwrap().to_string();
        assert_eq!(gid, "vendor-widgets");
        assert!(res["exclusions"]["groups"].as_array().unwrap().iter().any(|g| g["id"] == "vendor-widgets" && g["builtin"] == false));

        let (_, ex) = post("/api/scope/exclusions/group", serde_json::json!({ "id": gid, "on": true }));
        let custom = ex["groups"].as_array().unwrap().iter().find(|g| g["id"] == "vendor-widgets").unwrap().clone();
        assert_eq!(custom["state"], "on");

        let del = match ureq::delete(&format!("{base}/api/scope/exclusions/custom")).set("Authorization", &auth).send_json(serde_json::json!({ "id": gid })) {
            Ok(r) => r.into_json::<serde_json::Value>().unwrap(),
            Err(ureq::Error::Status(_, r)) => r.into_json().unwrap(),
            Err(e) => panic!("{e}"),
        };
        assert!(!del["groups"].as_array().unwrap().iter().any(|g| g["id"] == "vendor-widgets"));

        // Marking the prompt answered sticks.
        assert_eq!(post("/api/scope/exclusions/asked", serde_json::json!({})).0, 200);
        assert_eq!(get("/api/scope/exclusions").1["asked"], true);
    })
    .await
    .unwrap();
}

/// A response body fed by the test, one chunk at a time.
struct ChunkBody(tokio::sync::mpsc::Receiver<Bytes>);

impl hyper::body::Body for ChunkBody {
    type Data = Bytes;
    type Error = Infallible;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Bytes>, Infallible>>> {
        self.0.poll_recv(cx).map(|c| c.map(|b| Ok(hyper::body::Frame::data(b))))
    }
}

/// Serves `/events`, an event stream that sends its first event, waits for
/// `go`, then sends the last one; and `/big`, a gzip-compressed page.
async fn serve_streams(go: Arc<tokio::sync::Notify>) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let go = go.clone();
            let svc = service_fn(move |req: Request<Incoming>| {
                let go = go.clone();
                async move {
                    let resp = match req.uri().path() {
                        "/events" => {
                            let (tx, rx) = tokio::sync::mpsc::channel(4);
                            tokio::spawn(async move {
                                tx.send(Bytes::from_static(b"data: first\n\n")).await.unwrap();
                                go.notified().await;
                                tx.send(Bytes::from_static(b"data: last\n\n")).await.unwrap();
                            });
                            Response::builder().header("content-type", "text/event-stream").body(ChunkBody(rx).boxed())
                        }
                        _ => {
                            use std::io::Write;
                            let text: String = std::iter::once("plonix-start\n".to_string()).chain((0..5000).map(|i| format!("line {i}\n"))).collect();
                            let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                            enc.write_all(text.as_bytes()).unwrap();
                            Response::builder()
                                .header("content-type", "text/plain")
                                .header("content-encoding", "gzip")
                                .body(Full::new(Bytes::from(enc.finish().unwrap())).boxed())
                        }
                    };
                    Ok::<_, Infallible>(resp.unwrap())
                }
            });
            tokio::spawn(hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(s), svc));
        }
    });
    addr
}

/// Event streams reach the client as they are sent, and the exchange is
/// recorded once the stream ends.
#[tokio::test]
async fn event_streams_pass_through_as_they_arrive() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let go = Arc::new(tokio::sync::Notify::new());
    let up = serve_streams(go.clone()).await;
    let r = start(&home, None).await;

    let tcp = TcpStream::connect(r.proxy_addr).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tcp)).await.unwrap();
    tokio::spawn(conn);
    let url = format!("http://localhost:{}/events", up.port());
    let req = Request::builder().uri(&url).header("host", format!("localhost:{}", up.port())).body(Full::new(Bytes::new())).unwrap();
    let resp = sender.send_request(req).await.unwrap();
    assert_eq!(resp.headers()["content-type"], "text/event-stream");
    let mut body = resp.into_body();
    // The server holds the last event back until the client has the first,
    // so a proxy that waits for the whole body never gets here.
    let first = tokio::time::timeout(Duration::from_secs(5), body.frame()).await.expect("first event arrives before the stream ends");
    let first = first.unwrap().unwrap().into_data().unwrap();
    assert_eq!(&first[..], b"data: first\n\n");
    assert_eq!(r.engine.store.count().unwrap(), 0, "recorded when the stream ends");
    go.notify_one();
    let rest = tokio::time::timeout(Duration::from_secs(5), body.collect()).await.unwrap().unwrap().to_bytes();
    assert_eq!(&rest[..], b"data: last\n\n");

    wait_for_count(&r.engine, 1).await;
    let ex = r.engine.store.get_exchange(1).unwrap().unwrap();
    assert_eq!(ex.resp_body, b"data: first\n\ndata: last\n\n");
    assert!(!ex.resp_truncated);
}

/// Bodies over the recording limit go through in full; the start is kept,
/// decoded and searchable, and the full size is noted.
#[tokio::test]
async fn long_bodies_pass_through_and_are_kept_in_part() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let big = serve_streams(Arc::new(tokio::sync::Notify::new())).await;
    let r = start(&home, None).await;
    r.engine.set_body_limit(1000);

    // A compressed response, cut at 1000 bytes on the wire.
    let (status, text) = via_proxy(r.proxy_addr, &format!("http://localhost:{}/big", big.port()), &[]).await;
    assert_eq!(status, 200);
    assert!(!text.is_empty(), "the client gets the whole compressed body");
    wait_for_count(&r.engine, 1).await;
    let ex = r.engine.store.get_exchange(1).unwrap().unwrap();
    assert!(ex.resp_truncated);
    assert_eq!(ex.resp_body.len(), 1000);
    assert!(ex.resp_size.unwrap() > 1000);
    let decoded = plonix_core::codec::body_text(&ex.resp_headers, &ex.resp_body).unwrap();
    assert!(decoded.starts_with("plonix-start\nline 0\n"), "{}", &decoded[..decoded.len().min(40)]);
    let (hits, _) = r.engine.store.search(&Query::parse("plonix-start").unwrap(), &r.engine.rules(), 10, 0).unwrap();
    assert_eq!(hits.len(), 1, "the kept part is searchable");
    assert_eq!(hits[0].resp_len, ex.resp_size.unwrap(), "listings show the full size");

    // A request body over the limit streams to the server in full.
    let tcp = TcpStream::connect(r.proxy_addr).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tcp)).await.unwrap();
    tokio::spawn(conn);
    let payload = "x".repeat(3000);
    let req = Request::builder()
        .method("POST")
        .uri(format!("http://localhost:{}/echo", up.port()))
        .header("host", format!("localhost:{}", up.port()))
        .body(Full::new(Bytes::from(payload.clone())))
        .unwrap();
    let echoed = sender.send_request(req).await.unwrap().into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&echoed).ends_with(&payload), "the server got the whole body");
    wait_for_count(&r.engine, 2).await;
    let ex = r.engine.store.get_exchange(2).unwrap().unwrap();
    assert!(ex.req_truncated && ex.resp_truncated);
    assert_eq!((ex.req_body.len(), ex.req_size), (1000, Some(3000)));

    // A cut request body is not re-sent as if it were whole.
    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    let err = r.engine.replay(ReplayRequest { id: 2, ..Default::default() }, "test").await.unwrap_err();
    assert!(matches!(err, SendError::BadRequest(_)), "{err}");
}

/// Writes one WebSocket frame; clients mask theirs.
async fn ws_write<S: tokio::io::AsyncWrite + Unpin>(s: &mut S, fin: bool, opcode: u8, payload: &[u8], masked: bool) {
    let mut f = vec![(fin as u8) << 7 | opcode];
    let m = if masked { 0x80 } else { 0 };
    if payload.len() < 126 {
        f.push(m | payload.len() as u8);
    } else {
        f.push(m | 126);
        f.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    if masked {
        let key = [9u8, 8, 7, 6];
        f.extend_from_slice(&key);
        f.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
    } else {
        f.extend_from_slice(payload);
    }
    s.write_all(&f).await.unwrap();
    s.flush().await.unwrap();
}

/// Reads one WebSocket frame: (fin, opcode, unmasked payload).
async fn ws_read<S: tokio::io::AsyncRead + Unpin>(s: &mut S) -> (bool, u8, Vec<u8>) {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).await.unwrap();
    let len = match h[1] & 0x7f {
        126 => {
            let mut b = [0u8; 2];
            s.read_exact(&mut b).await.unwrap();
            u16::from_be_bytes(b) as usize
        }
        127 => {
            let mut b = [0u8; 8];
            s.read_exact(&mut b).await.unwrap();
            u64::from_be_bytes(b) as usize
        }
        n => n as usize,
    };
    let mut key = [0u8; 4];
    if h[1] & 0x80 != 0 {
        s.read_exact(&mut key).await.unwrap();
    }
    let mut payload = vec![0u8; len];
    s.read_exact(&mut payload).await.unwrap();
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= key[i % 4];
    }
    (h[0] & 0x80 != 0, h[0] & 0x0f, payload)
}

/// Echoes every frame back as it came (fragments too); answers pings and closes.
async fn ws_echo<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(mut s: S) {
    loop {
        let (fin, op, payload) = ws_read(&mut s).await;
        match op {
            8 => {
                ws_write(&mut s, true, 8, &payload, false).await;
                return;
            }
            9 => ws_write(&mut s, true, 10, &payload, false).await,
            _ => ws_write(&mut s, fin, op, &payload, false).await,
        }
    }
}

/// A WebSocket echo server at `/ws`, over TLS for `localhost` when `tls` is set.
async fn serve_ws(tls: bool) -> (SocketAddr, CertificateDer<'static>) {
    let (ca_pem, ca_key) = CertAuthority::generate_pem().unwrap();
    let test_ca = CertAuthority::from_pem(&ca_pem, &ca_key).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Fixed(test_ca.leaf_for("localhost").unwrap())));
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let handler = |req: Request<Incoming>| async move {
        use base64::Engine as _;
        let key = req.headers()["sec-websocket-key"].to_str().unwrap().to_string();
        let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY, format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
        let accept = base64::engine::general_purpose::STANDARD.encode(digest.as_ref());
        tokio::spawn(async move {
            let io = hyper::upgrade::on(req).await.unwrap();
            ws_echo(TokioIo::new(io)).await;
        });
        Ok::<_, Infallible>(
            Response::builder()
                .status(101)
                .header("connection", "Upgrade")
                .header("upgrade", "websocket")
                .header("sec-websocket-accept", accept)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
    };
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let conn = hyper::server::conn::http1::Builder::new();
                if tls {
                    let Ok(t) = acceptor.accept(s).await else { return };
                    let _ = conn.serve_connection(TokioIo::new(t), service_fn(handler)).with_upgrades().await;
                } else {
                    let _ = conn.serve_connection(TokioIo::new(s), service_fn(handler)).with_upgrades().await;
                }
            });
        }
    });
    (addr, CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap())
}

/// Opens a CONNECT tunnel through the proxy and starts TLS that trusts the Plonix CA.
async fn tunnel(r: &Running, target: &str, alpn: &[&[u8]]) -> tokio_rustls::client::TlsStream<TcpStream> {
    let mut tcp = TcpStream::connect(r.proxy_addr).await.unwrap();
    tcp.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes()).await.unwrap();
    let mut buf = [0u8; 1024];
    let n = tcp.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(r.engine.ca.ca_der().clone()).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    tokio_rustls::TlsConnector::from(Arc::new(config)).connect(ServerName::try_from("localhost").unwrap(), tcp).await.unwrap()
}

/// Sends a handshake, then text in two fragments, a ping, binary and a close,
/// checking each echo.
async fn ws_session<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(s: &mut S, target: &str, host: &str) {
    let handshake = format!(
        "GET {target} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"
    );
    s.write_all(handshake.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
    assert!(head.starts_with("http/1.1 101"), "{head}");
    assert!(head.contains("sec-websocket-accept: s3pplmbitxaq9kygzzhzrbk+xoo="), "{head}");

    ws_write(s, false, 1, b"hello ", true).await;
    ws_write(s, true, 0, b"world", true).await;
    assert_eq!(ws_read(s).await, (false, 1, b"hello ".to_vec()));
    assert_eq!(ws_read(s).await, (true, 0, b"world".to_vec()));
    ws_write(s, true, 9, b"p", true).await;
    assert_eq!(ws_read(s).await, (true, 10, b"p".to_vec()));
    let big = vec![7u8; 3000];
    ws_write(s, true, 2, &big, true).await;
    assert_eq!(ws_read(s).await, (true, 2, big));
    ws_write(s, true, 8, &[0x03, 0xe8], true).await;
    assert_eq!(ws_read(s).await.1, 8);
}

/// Waits until a handshake has this many messages recorded, and returns them.
async fn wait_for_messages(engine: &Engine, id: i64, n: i64) -> Vec<plonix_core::model::WsMessage> {
    for _ in 0..300 {
        let (list, total) = engine.store.ws_messages(id, 100, 0).unwrap();
        if total >= n {
            return list;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {n} messages, have {:?}", engine.store.ws_messages(id, 100, 0).unwrap().0);
}

fn assert_captured(msgs: &[plonix_core::model::WsMessage]) {
    let side = |d: &str| msgs.iter().filter(|m| m.direction == d).map(|m| (m.opcode.as_str(), m.payload.len(), m.truncated)).collect::<Vec<_>>();
    assert_eq!(side("to_server"), vec![("text", 11, false), ("ping", 1, false), ("binary", 1000, true), ("close", 2, false)]);
    assert_eq!(side("to_client"), vec![("text", 11, false), ("pong", 1, false), ("binary", 1000, true), ("close", 2, false)]);
    let text = msgs.iter().find(|m| m.opcode == "text").unwrap();
    assert_eq!(text.text().as_deref(), Some("hello world"));
    assert!(msgs.iter().filter(|m| m.opcode == "binary").all(|m| m.size == 3000));
    assert_eq!(msgs.iter().find(|m| m.opcode == "close").unwrap().text().as_deref(), Some("1000"));
}

/// WebSockets work through the proxy, in plain HTTP and inside decrypted
/// HTTPS, and every message is recorded against its handshake.
#[tokio::test]
async fn websockets_are_relayed_and_their_messages_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let (plain, _) = serve_ws(false).await;
    let (secure, secure_root) = serve_ws(true).await;
    let r = start(&home, Some(secure_root)).await;
    r.engine.set_body_limit(1000);

    // ws:// through the proxy, in absolute form.
    let host = format!("localhost:{}", plain.port());
    let mut tcp = TcpStream::connect(r.proxy_addr).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), ws_session(&mut tcp, &format!("http://{host}/ws?room=1"), &host)).await.expect("plain session");
    wait_for_count(&r.engine, 1).await;
    let ex = r.engine.store.get_exchange(1).unwrap().unwrap();
    assert_eq!((ex.status, ex.method.as_str(), ex.path.as_str(), ex.query.as_str()), (Some(101), "GET", "/ws", "room=1"));
    assert_captured(&wait_for_messages(&r.engine, 1, 8).await);

    // wss:// inside a decrypted CONNECT tunnel.
    let host = format!("localhost:{}", secure.port());
    let mut tls = tunnel(&r, &host, &[b"http/1.1"]).await;
    tokio::time::timeout(Duration::from_secs(10), ws_session(&mut tls, "/ws", &host)).await.expect("TLS session");
    wait_for_count(&r.engine, 2).await;
    let ex = r.engine.store.get_exchange(2).unwrap().unwrap();
    assert_eq!((ex.scheme.as_str(), ex.status), ("https", Some(101)));
    assert_captured(&wait_for_messages(&r.engine, 2, 8).await);

    // The API lists them, for the user and (in scope) for agents.
    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    let base = format!("http://{}", r.api_addr);
    let (user, agent) = (format!("Bearer {}", r.token), format!("Bearer {}", r.agent_token));
    tokio::task::spawn_blocking(move || {
        for auth in [&user, &agent] {
            let v: serde_json::Value = ureq::get(&format!("{base}/api/traffic/2/messages?limit=3")).set("Authorization", auth).call().unwrap().into_json().unwrap();
            assert_eq!(v["total"], 8);
            assert_eq!(v["items"].as_array().unwrap().len(), 3);
            assert_eq!((v["items"][0]["direction"].as_str(), v["items"][0]["text"].as_str()), (Some("to_server"), Some("hello world")));
        }
        let missing = ureq::get(&format!("{base}/api/traffic/99/messages")).set("Authorization", &user).call();
        assert!(matches!(missing, Err(ureq::Error::Status(404, _))));
    })
    .await
    .unwrap();
}

/// Hosts that are never decrypted pass through untouched, WebSockets included.
#[tokio::test]
async fn websockets_in_hosts_never_decrypted_are_not_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let (secure, secure_root) = serve_ws(true).await;
    let r = start(&home, None).await;
    let settings = plonix_core::settings::ProxySettings { passthrough_hosts: vec!["localhost".into()], listen_port: 0, ..Default::default() };
    r.engine.apply_proxy_settings(&settings).await.unwrap();
    let proxy = r.engine.proxy_addr().unwrap();

    let host = format!("localhost:{}", secure.port());
    let mut tcp = TcpStream::connect(proxy).await.unwrap();
    tcp.write_all(format!("CONNECT {host} HTTP/1.1\r\nHost: {host}\r\n\r\n").as_bytes()).await.unwrap();
    let mut buf = [0u8; 1024];
    let n = tcp.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
    // The server's own certificate comes through, not one minted by Plonix.
    let mut roots = rustls::RootCertStore::empty();
    roots.add(secure_root).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut tls = tokio_rustls::TlsConnector::from(Arc::new(config)).connect(ServerName::try_from("localhost").unwrap(), tcp).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), ws_session(&mut tls, "/ws", &host)).await.expect("session");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(r.engine.store.count().unwrap(), 0);
}

/// An HTTPS server for `localhost` that speaks HTTP/2 only. `/echo` answers
/// with the protocol, request line, headers and body it received.
async fn serve_h2() -> (SocketAddr, CertificateDer<'static>) {
    let (ca_pem, ca_key) = CertAuthority::generate_pem().unwrap();
    let test_ca = CertAuthority::from_pem(&ca_pem, &ca_key).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(Fixed(test_ca.leaf_for("localhost").unwrap())));
    config.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let handler = |req: Request<Incoming>| async move {
        let (parts, body) = req.into_parts();
        let body = body.collect().await.unwrap().to_bytes();
        let mut text = format!("{:?} {} {}\n", parts.version, parts.method, parts.uri);
        for (k, v) in &parts.headers {
            text.push_str(&format!("{}: {}\n", k, v.to_str().unwrap_or("")));
        }
        text.push('\n');
        text.push_str(&String::from_utf8_lossy(&body));
        Ok::<_, Infallible>(Response::builder().header("content-type", "text/plain").body(Full::new(Bytes::from(text))).unwrap())
    };
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(tls) = acceptor.accept(s).await {
                    let _ = hyper::server::conn::http2::Builder::new(hyper_util::rt::TokioExecutor::new())
                        .serve_connection(TokioIo::new(tls), service_fn(handler))
                        .await;
                }
            });
        }
    });
    (addr, CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap())
}

/// HTTP/2 on both sides: the browser can speak it to the proxy, the proxy
/// speaks it to servers that offer it, and each exchange records which.
#[tokio::test]
async fn http2_is_spoken_on_both_sides_and_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let (up, root) = serve_h2().await;
    let r = start(&home, Some(root)).await;
    let host = format!("localhost:{}", up.port());

    // An HTTP/2 client: the proxy agrees to HTTP/2 inside the tunnel.
    let tls = tunnel(&r, &host, &[b"h2", b"http/1.1"]).await;
    assert_eq!(tls.get_ref().1.alpn_protocol(), Some(&b"h2"[..]));
    let (mut sender, conn) = hyper::client::conn::http2::handshake::<_, _, Full<Bytes>>(hyper_util::rt::TokioExecutor::new(), TokioIo::new(tls)).await.unwrap();
    tokio::spawn(conn);
    let req = Request::builder()
        .uri(format!("https://{host}/echo?x=1"))
        .header("cookie", "a=1")
        .header("cookie", "b=2")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    assert_eq!((resp.status().as_u16(), resp.version()), (200, hyper::Version::HTTP_2));
    let text = String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned();
    assert!(text.starts_with(&format!("HTTP/2.0 GET https://{host}/echo?x=1")), "{text}");
    assert!(text.contains("cookie: a=1; b=2"), "{text}");
    wait_for_count(&r.engine, 1).await;
    let ex = r.engine.store.get_exchange(1).unwrap().unwrap();
    assert_eq!((ex.http_version.as_str(), ex.status, ex.query.as_str()), ("HTTP/2", Some(200), "x=1"));
    assert_eq!(plonix_core::model::header(&ex.req_headers, "host"), Some(host.as_str()), "recorded like HTTP/1.1");

    // An HTTP/1.1 client: the server is still reached over HTTP/2.
    let tls = tunnel(&r, &host, &[b"http/1.1"]).await;
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tls)).await.unwrap();
    tokio::spawn(conn);
    let req = Request::builder()
        .method("POST")
        .uri("/echo")
        .header("host", &host)
        .header("connection", "keep-alive")
        .body(Full::new(Bytes::from_static(b"over h2")))
        .unwrap();
    let text = String::from_utf8_lossy(&sender.send_request(req).await.unwrap().into_body().collect().await.unwrap().to_bytes()).into_owned();
    assert!(text.starts_with(&format!("HTTP/2.0 POST https://{host}/echo")) && text.ends_with("over h2"), "{text}");
    assert!(!text.contains("connection:") && !text.contains("\nhost:"), "no connection headers in HTTP/2: {text}");
    wait_for_count(&r.engine, 2).await;
    assert_eq!(r.engine.store.get_exchange(2).unwrap().unwrap().http_version, "HTTP/2");

    // Replays use HTTP/2 too.
    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    let ex = r.engine.replay(ReplayRequest { id: 2, body: Some("again".into()), ..Default::default() }, "test").await.unwrap();
    assert_eq!(ex.http_version, "HTTP/2");
    assert!(String::from_utf8_lossy(&ex.resp_body).ends_with("again"));
    let ex = r
        .engine
        .send(SendRequest { method: "GET".into(), url: format!("https://{host}/echo"), headers: vec![("Host".into(), host.clone())], ..Default::default() }, "test")
        .await
        .unwrap();
    assert_eq!((ex.status, ex.http_version.as_str()), (Some(200), "HTTP/2"));
}

// ---- intercept ---------------------------------------------------------------

/// Sends one request with a body through the proxy, as a browser would.
fn send_via_proxy(proxy: SocketAddr, method: &str, url: &str, body: &str) -> impl std::future::Future<Output = (u16, String)> + Send + 'static {
    send_via_proxy_owned(proxy, method.to_string(), url.to_string(), body.to_string())
}

async fn send_via_proxy_owned(proxy: SocketAddr, method: String, url: String, body: String) -> (u16, String) {
    let (method, url, body) = (method.as_str(), url.as_str(), body.as_str());
    let tcp = TcpStream::connect(proxy).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tcp)).await.unwrap();
    tokio::spawn(conn);
    let uri: hyper::Uri = url.parse().unwrap();
    let req = Request::builder()
        .method(method)
        .uri(url)
        .header("host", uri.authority().unwrap().as_str())
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    let status = resp.status().as_u16();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// Calls the API with a token.
async fn call_api(r: &Running, token: &str, method: &str, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    let url = format!("http://{}{path}", r.api_addr);
    let (auth, method) = (format!("Bearer {token}"), method.to_string());
    tokio::task::spawn_blocking(move || {
        let req = ureq::request(&method, &url).set("Authorization", &auth).set("X-Plonix-Client", "test");
        let resp = if method == "GET" { req.call() } else { req.send_json(body) };
        match resp {
            Ok(r) => (r.status(), r.into_json::<serde_json::Value>().unwrap_or_default()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_json::<serde_json::Value>().unwrap_or_default()),
            Err(e) => panic!("{e}"),
        }
    })
    .await
    .unwrap()
}

/// Waits until `n` items are held, and returns the queue.
async fn wait_held(engine: &Engine, n: usize) -> Vec<plonix_core::intercept::HeldItem> {
    for _ in 0..500 {
        if engine.intercept.held() >= n {
            return engine.intercept.queue();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {n} held items, have {}", engine.intercept.held());
}

#[tokio::test]
async fn intercepted_requests_are_edited_forwarded_or_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    let (code, v) = call_api(&r, &r.token, "PUT", "/api/intercept", serde_json::json!({ "on": true })).await;
    assert_eq!((code, v["on"].as_bool(), v["hold"].as_str()), (200, Some(true), Some("in_scope")), "{v}");

    // Held, edited (start line, a header, the body), then forwarded.
    let url = format!("http://localhost:{}/echo?x=1", up.port());
    let client = tokio::spawn(send_via_proxy(r.proxy_addr, "POST", &url, "hello"));
    let item = wait_held(&r.engine, 1).await.remove(0);
    assert!(item.raw.starts_with("POST /echo?x=1 HTTP/1.1\n") && item.raw.ends_with("\n\nhello"), "{}", item.raw);
    assert!(item.body_editable && item.in_scope);
    let (_, v) = call_api(&r, &r.token, "GET", "/api/intercept", serde_json::json!(null)).await;
    assert_eq!(v["queue"][0]["id"], item.id);
    let (_, st) = call_api(&r, &r.token, "GET", "/api/status", serde_json::json!(null)).await;
    assert_eq!(st["intercept"]["held"], 1);
    let edited = item.raw.replace("x=1", "x=2").replace("content-type: text/plain", "content-type: text/plain\nx-edited: yes").replace("hello", "goodbye!");
    let (code, v) = call_api(&r, &r.token, "POST", &format!("/api/intercept/{}/forward", item.id), serde_json::json!({ "raw": "nonsense" })).await;
    assert_eq!((code, v["code"].as_str()), (400, Some("bad_edit")), "{v}");
    let (code, _) = call_api(&r, &r.token, "POST", &format!("/api/intercept/{}/forward", item.id), serde_json::json!({ "raw": edited })).await;
    assert_eq!(code, 200);
    let (status, body) = client.await.unwrap();
    assert_eq!(status, 200);
    assert!(body.starts_with("POST /echo?x=2\n") && body.contains("x-edited: yes") && body.ends_with("\n\ngoodbye!"), "{body}");
    wait_for_count(&r.engine, 1).await;
    let ex = r.engine.store.get_exchange(1).unwrap().unwrap();
    assert!(ex.edited);
    assert_eq!((ex.query.as_str(), ex.req_body.as_slice()), ("x=2", &b"goodbye!"[..]), "the record shows what was sent");
    let original = ex.original_request.unwrap();
    assert!(original.contains("x=1") && original.ends_with("hello"), "{original}");

    // Dropped: the server never sees it, the client gets an error page.
    let client = tokio::spawn(send_via_proxy(r.proxy_addr, "POST", &url, "drop me"));
    let item = wait_held(&r.engine, 1).await.remove(0);
    let (code, _) = call_api(&r, &r.token, "POST", &format!("/api/intercept/{}/drop", item.id), serde_json::json!({})).await;
    assert_eq!(code, 200);
    let (status, body) = client.await.unwrap();
    assert_eq!(status, 502);
    assert!(body.contains("dropped in Intercept"), "{body}");
    wait_for_count(&r.engine, 2).await;
    let ex = r.engine.store.get_exchange(2).unwrap().unwrap();
    assert!(ex.status.is_none() && ex.error.unwrap().contains("dropped"));
    let (code, _) = call_api(&r, &r.token, "POST", &format!("/api/intercept/{}/drop", item.id), serde_json::json!({})).await;
    assert_eq!(code, 404, "an answered item is gone");

    // Out-of-scope hosts go straight through by default...
    let (status, _) = send_via_proxy(r.proxy_addr, "GET", &format!("http://127.0.0.1:{}/echo", up.port()), "").await;
    assert_eq!((status, r.engine.intercept.held()), (200, 0));
    // ...and are held when the user holds everything.
    let (code, _) = call_api(&r, &r.token, "PUT", "/api/intercept", serde_json::json!({ "hold": "everything", "filter": "method:GET" })).await;
    assert_eq!(code, 200);
    let client = tokio::spawn(send_via_proxy(r.proxy_addr, "GET", &format!("http://127.0.0.1:{}/echo", up.port()), ""));
    let other = tokio::spawn(send_via_proxy(r.proxy_addr, "POST", &url, "not matching the filter"));
    assert_eq!(other.await.unwrap().0, 200, "the filter lets a POST through");
    let held = wait_held(&r.engine, 1).await;
    assert!(!held[0].in_scope);
    let (_, v) = call_api(&r, &r.token, "POST", "/api/intercept/forward-all", serde_json::json!({})).await;
    assert_eq!(v["forwarded"], 1);
    assert_eq!(client.await.unwrap().0, 200);

    // A bad filter is refused and changes nothing.
    let (code, v) = call_api(&r, &r.token, "PUT", "/api/intercept", serde_json::json!({ "filter": "status:abc" })).await;
    assert_eq!((code, v["code"].as_str()), (400, Some("bad_settings")));
    assert_eq!(r.engine.intercept.options().filter, "method:GET");
}

#[tokio::test]
async fn intercept_times_out_and_turning_it_off_releases_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    let url = format!("http://localhost:{}/echo", up.port());

    // Nobody answers: the request goes on unchanged after the timeout.
    let (code, _) = call_api(&r, &r.token, "PUT", "/api/intercept", serde_json::json!({ "on": true, "timeout_s": 1 })).await;
    assert_eq!(code, 200);
    let started = std::time::Instant::now();
    let client = tokio::spawn(send_via_proxy(r.proxy_addr, "POST", &url, "slow"));
    let item = wait_held(&r.engine, 1).await.remove(0);
    assert!(item.expires_at - item.held_at <= 1000);
    let (status, body) = client.await.unwrap();
    assert_eq!((status, body.ends_with("slow")), (200, true));
    assert!(started.elapsed() >= Duration::from_millis(900), "held until the timeout");
    assert_eq!(r.engine.intercept.held(), 0);
    wait_for_count(&r.engine, 1).await;
    assert!(!r.engine.store.get_exchange(1).unwrap().unwrap().edited);

    // Turning Intercept off sends everything held on.
    call_api(&r, &r.token, "PUT", "/api/intercept", serde_json::json!({ "timeout_s": 300 })).await;
    let a = tokio::spawn(send_via_proxy(r.proxy_addr, "POST", &url, "one"));
    let b = tokio::spawn(send_via_proxy(r.proxy_addr, "POST", &url, "two"));
    wait_held(&r.engine, 2).await;
    let (_, v) = call_api(&r, &r.token, "PUT", "/api/intercept", serde_json::json!({ "on": false })).await;
    assert_eq!((v["on"].as_bool(), v["released"].as_u64()), (Some(false), Some(2)));
    assert_eq!((a.await.unwrap().0, b.await.unwrap().0), (200, 200));
    let (status, _) = send_via_proxy(r.proxy_addr, "POST", &url, "three").await;
    assert_eq!((status, r.engine.intercept.held()), (200, 0), "nothing is held while off");
}

#[tokio::test]
async fn intercepted_responses_can_be_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    // Only responses: a filter on the status holds no request.
    let (code, _) = call_api(&r, &r.token, "PUT", "/api/intercept", serde_json::json!({ "on": true, "responses": true, "filter": "status:200" })).await;
    assert_eq!(code, 200);
    let client = tokio::spawn(send_via_proxy(r.proxy_addr, "GET", &format!("http://localhost:{}/", up.port()), ""));
    let item = wait_held(&r.engine, 1).await.remove(0);
    assert_eq!((item.status, item.body_editable), (Some(200), true));
    assert!(item.raw.starts_with("HTTP/1.1 200 OK\n") && item.raw.contains("welcome home"), "{}", item.raw);
    let edited = item.raw.replace("HTTP/1.1 200 OK", "HTTP/1.1 201 Created").replace("welcome home", "edited in flight");
    call_api(&r, &r.token, "POST", &format!("/api/intercept/{}/forward", item.id), serde_json::json!({ "raw": edited })).await;
    let (status, body) = client.await.unwrap();
    assert_eq!(status, 201);
    assert!(body.contains("edited in flight") && !body.contains("welcome"), "{body}");
    wait_for_count(&r.engine, 1).await;
    let ex = r.engine.store.get_exchange(1).unwrap().unwrap();
    assert_eq!((ex.edited, ex.status), (true, Some(201)));
    assert!(String::from_utf8_lossy(&ex.resp_body).contains("edited in flight"));
    assert!(ex.original_response.unwrap().contains("welcome home"));
}

/// Agents can neither see nor touch what is held.
#[tokio::test]
async fn agents_have_no_access_to_intercept() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let r = start(&home, None).await;
    r.engine.intercept.set_on(true);
    let agent = r.agent_token.clone();
    for (method, path) in [
        ("GET", "/api/intercept"),
        ("PUT", "/api/intercept"),
        ("POST", "/api/intercept/1/forward"),
        ("POST", "/api/intercept/1/drop"),
        ("POST", "/api/intercept/forward-all"),
    ] {
        let (code, v) = call_api(&r, &agent, method, path, serde_json::json!({ "on": false })).await;
        assert_eq!((code, v["code"].as_str()), (403, Some("agent_not_allowed")), "{method} {path}");
    }
    assert!(r.engine.intercept.is_on(), "an agent cannot turn it off");
    let (code, st) = call_api(&r, &agent, "GET", "/api/status", serde_json::json!(null)).await;
    assert_eq!(code, 200);
    assert!(st.get("intercept").is_none(), "agents learn nothing about the queue: {st}");
}

// ---- match and replace -------------------------------------------------------

#[tokio::test]
async fn match_and_replace_rules_change_traffic_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve_http().await;
    let r = start(&home, None).await;
    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();

    let add = |rule: serde_json::Value| call_api(&r, &r.token, "POST", "/api/replace", rule);
    let (code, v) = add(serde_json::json!({ "target": "request_body", "match": "(", "regex": true })).await;
    assert_eq!((code, v["code"].as_str()), (400, Some("bad_rule")), "{v}");
    let (code, v) = add(serde_json::json!({ "target": "nowhere", "match": "x" })).await;
    assert!(code == 400 || code == 422, "{code} {v}");
    let (code, line) = add(serde_json::json!({ "target": "request_line", "match": r"^POST /echo\?v=1", "replace": "POST /echo?v=2", "regex": true })).await;
    assert_eq!(code, 200, "{line}");
    add(serde_json::json!({ "target": "request_header", "match": r"(?i)^content-type: (.*)$", "replace": "Content-Type: $1+replaced\nX-Added: yes", "regex": true })).await;
    add(serde_json::json!({ "target": "request_body", "match": "secret", "replace": "public" })).await;
    let (_, home_rule) = add(serde_json::json!({ "target": "response_body", "match": "welcome home", "replace": "rewritten", "in_scope_only": true, "note": "home page" })).await;
    add(serde_json::json!({ "target": "response_body", "match": "compressed", "replace": "decoded" })).await;
    add(serde_json::json!({ "target": "response_header", "match": "^set-cookie: .*$", "replace": "", "regex": true })).await;
    let (_, list) = call_api(&r, &r.token, "GET", "/api/replace", serde_json::json!(null)).await;
    assert_eq!((list["enabled"].as_bool(), list["rules"].as_array().unwrap().len()), (Some(true), 6), "{list}");

    // The request line, a header and the body change on the way out.
    let (status, body) = send_via_proxy(r.proxy_addr, "POST", &format!("http://localhost:{}/echo?v=1", up.port()), "my secret").await;
    assert_eq!(status, 200);
    assert!(body.starts_with("POST /echo?v=2\n") && body.contains("content-type: text/plain+replaced\n") && body.contains("x-added: yes"), "{body}");
    assert!(body.ends_with("\n\nmy public"), "{body}");
    wait_for_count(&r.engine, 1).await;
    let ex = r.engine.store.get_exchange(1).unwrap().unwrap();
    assert_eq!((ex.query.as_str(), ex.req_body.as_slice(), ex.replaced.len()), ("v=2", &b"my public"[..], 3), "{:?}", ex.replaced);
    assert!(!ex.edited, "rules are not hand edits");

    // Response rules: a body (in scope only), a removed header, a compressed body.
    let (status, body) = send_via_proxy(r.proxy_addr, "GET", &format!("http://localhost:{}/", up.port()), "").await;
    assert_eq!(status, 200);
    assert!(body.contains("<h1>rewritten</h1>"), "{body}");
    wait_for_count(&r.engine, 2).await;
    let ex = r.engine.store.get_exchange(2).unwrap().unwrap();
    assert!(ex.resp_headers.iter().all(|(k, _)| !k.eq_ignore_ascii_case("set-cookie")), "{:?}", ex.resp_headers);
    assert!(ex.replaced.iter().any(|l| l == &format!("#{} home page", home_rule["id"])), "{:?}", ex.replaced);
    let (_, body) = send_via_proxy(r.proxy_addr, "GET", &format!("http://127.0.0.1:{}/", up.port()), "").await;
    assert!(body.contains("welcome home"), "out of scope: {body}");
    let (status, body) = send_via_proxy(r.proxy_addr, "GET", &format!("http://localhost:{}/gz", up.port()), "").await;
    assert_eq!((status, body.as_str()), (200, "decoded hello"));

    // Intercept sees the request after the rules.
    call_api(&r, &r.token, "PUT", "/api/intercept", serde_json::json!({ "on": true })).await;
    let client = tokio::spawn(send_via_proxy(r.proxy_addr, "POST", &format!("http://localhost:{}/echo?v=1", up.port()), "secret"));
    let item = wait_held(&r.engine, 1).await.remove(0);
    assert!(item.raw.starts_with("POST /echo?v=2 ") && item.raw.ends_with("\n\npublic"), "{}", item.raw);
    call_api(&r, &r.token, "POST", "/api/intercept/forward-all", serde_json::json!({})).await;
    assert_eq!(client.await.unwrap().0, 200);
    call_api(&r, &r.token, "PUT", "/api/intercept", serde_json::json!({ "on": false })).await;

    // A rule switched off, a rule deleted, and all rules off.
    let (code, v) = call_api(&r, &r.token, "PATCH", &format!("/api/replace/{}", line["id"]), serde_json::json!({ "enabled": false })).await;
    assert_eq!((code, v["enabled"].as_bool()), (200, Some(false)), "{v}");
    let (code, _) = call_api(&r, &r.token, "PATCH", &format!("/api/replace/{}", line["id"]), serde_json::json!({ "match": "(", "regex": true })).await;
    assert_eq!(code, 400, "an edit that does not compile is refused");
    let (_, body) = send_via_proxy(r.proxy_addr, "POST", &format!("http://localhost:{}/echo?v=1", up.port()), "secret").await;
    assert!(body.starts_with("POST /echo?v=1\n") && body.ends_with("public"), "{body}");
    let (code, _) = call_api(&r, &r.token, "DELETE", &format!("/api/replace/{}", home_rule["id"]), serde_json::json!({})).await;
    assert_eq!(code, 200);
    let (code, _) = call_api(&r, &r.token, "DELETE", &format!("/api/replace/{}", home_rule["id"]), serde_json::json!({})).await;
    assert_eq!(code, 404);
    let (_, body) = send_via_proxy(r.proxy_addr, "GET", &format!("http://localhost:{}/", up.port()), "").await;
    assert!(body.contains("welcome home"), "{body}");
    r.engine.set_replace_on(false).unwrap();
    let (_, body) = send_via_proxy(r.proxy_addr, "POST", &format!("http://localhost:{}/echo", up.port()), "secret").await;
    assert!(body.ends_with("\n\nsecret") && !body.contains("x-added"), "{body}");

    // Agents can neither read nor change rules.
    for (method, path) in [("GET", "/api/replace"), ("POST", "/api/replace"), ("PATCH", "/api/replace/1"), ("DELETE", "/api/replace/1")] {
        let (code, v) = call_api(&r, &r.agent_token, method, path, serde_json::json!({ "target": "request_body", "match": "x" })).await;
        assert_eq!((code, v["code"].as_str()), (403, Some("agent_not_allowed")), "{method} {path}");
    }
    assert_eq!(r.engine.store.replace_rules().unwrap().len(), 5);
}
