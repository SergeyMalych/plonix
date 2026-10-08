//! HAR import and export through the API, and client certificates presented
//! to a local server that requires them.

mod common;
use common::Running;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use plonix_core::ca::CertAuthority;
use plonix_core::engine::{SendRequest};
use plonix_core::model::Source;
use plonix_core::paths::Home;
use plonix_core::scope::Decision;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const FIXTURE: &str = include_str!("fixtures/sample.har");

async fn start(home: &Home, extra_root: Option<CertificateDer<'static>>) -> Running {
    common::open(home, "test", extra_root).await
}

/// Calls the API; the body is JSON, or raw bytes when `raw` is given.
async fn call(r: &Running, token: &str, method: &str, path: &str, body: Value, raw: Option<Vec<u8>>) -> (u16, String) {
    let url = format!("http://{}{path}", r.api_addr);
    let (auth, method) = (format!("Bearer {token}"), method.to_string());
    tokio::task::spawn_blocking(move || {
        let req = ureq::request(&method, &url).set("Authorization", &auth).set("X-Plonix-Client", "test");
        let resp = match (method.as_str(), raw) {
            ("GET" | "DELETE", _) => req.call(),
            (_, Some(bytes)) => req.send_bytes(&bytes),
            (_, None) => req.send_json(body),
        };
        match resp {
            Ok(r) => (r.status(), r.into_string().unwrap()),
            Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
            Err(e) => panic!("{e}"),
        }
    })
    .await
    .unwrap()
}

fn json_of(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|e| panic!("{e}: {s}"))
}

/// The fixture is imported once, its duplicates are left out the second
/// time, it exports as HAR 1.2, and that export imports into another project
/// with the same requests, bodies and WebSocket messages.
#[tokio::test(flavor = "multi_thread")]
async fn har_files_round_trip_through_the_api() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().join("a") };
    let r = start(&home, None).await;
    r.engine.decide("shop.example.test", Decision::Accepted, false, "").unwrap();

    let (code, body) = call(&r, &r.token, "POST", "/api/har/import", Value::Null, Some(FIXTURE.as_bytes().to_vec())).await;
    assert_eq!(code, 200, "{body}");
    let report = json_of(&body);
    assert_eq!((report["imported"].as_u64(), report["duplicates"].as_u64(), report["skipped"].as_u64()), (Some(6), Some(0), Some(1)), "{report}");
    assert!(report["problems"][0].as_str().unwrap().contains("entry 7"), "{report}");

    // Imported traffic is stored and analyzed like captured traffic.
    let store = &r.engine.store;
    let first = store.get_exchange(report["first_id"].as_i64().unwrap()).unwrap().unwrap();
    assert_eq!((first.source, first.initiator.as_deref(), first.http_version.as_str()), (Some(Source::Import), Some("har"), "HTTP/2"));
    assert_eq!((first.path.as_str(), first.query.as_str(), first.ts, first.duration_ms), ("/", "ref=home&q=red%20shoes", 1_714_566_600_000, 120));
    assert!(first.req_headers.iter().all(|(k, _)| !k.starts_with(':')), "pseudo-headers are dropped");
    assert!(first.resp_headers.iter().all(|(k, _)| !k.eq_ignore_ascii_case("content-encoding")), "the body is stored decoded");
    let (hits, total) = store.search(&r.engine.filters().parse("source:import red shoes").unwrap(), &r.engine.rules(), 10, 0).unwrap();
    assert!(total >= 1 && hits.iter().all(|h| h.in_scope), "{hits:?}");
    let login = store.search(&r.engine.filters().parse("path:/api/login").unwrap(), &r.engine.rules(), 10, 0).unwrap().0;
    let login = store.get_exchange(login[0].id).unwrap().unwrap();
    assert_eq!((login.ts, login.req_body.as_slice()), (1_714_566_600_250, &b"{\"user\":\"alice\",\"password\":\"s3cret\"}"[..]));
    let failed = store.search(&r.engine.filters().parse("host:down.example.test").unwrap(), &r.engine.rules(), 10, 0).unwrap().0;
    let failed = store.get_exchange(failed[0].id).unwrap().unwrap();
    assert_eq!((failed.status, failed.error.as_deref()), (None, Some("net::ERR_NAME_NOT_RESOLVED")));
    let ws = store.search(&r.engine.filters().parse("status:101").unwrap(), &r.engine.rules(), 10, 0).unwrap().0;
    let (messages, n) = store.ws_messages(ws[0].id, 10, 0).unwrap();
    assert_eq!((n, messages[2].opcode.as_str(), messages[2].payload.as_slice()), (3, "binary", &[0u8, 1, 2][..]));

    // The same file again adds nothing.
    let (_, body) = call(&r, &r.token, "POST", "/api/har/import", Value::Null, Some(FIXTURE.as_bytes().to_vec())).await;
    let again = json_of(&body);
    assert_eq!((again["imported"].as_u64(), again["duplicates"].as_u64()), (Some(0), Some(6)), "{again}");
    assert_eq!(store.count().unwrap(), 6);

    // Export: everything, a search, or chosen rows.
    let (code, all) = call(&r, &r.token, "GET", "/api/har", Value::Null, None).await;
    assert_eq!(code, 200);
    let har = json_of(&all);
    assert_eq!((har["log"]["version"].as_str(), har["log"]["creator"]["name"].as_str()), (Some("1.2"), Some("Plonix")));
    let entries = har["log"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 6);
    let png = entries.iter().find(|e| e["request"]["url"] == "https://shop.example.test/logo.png").unwrap();
    assert_eq!((png["response"]["content"]["encoding"].as_str(), png["response"]["content"]["text"].as_str()), (Some("base64"), Some("iVBORw0KGgo=")));
    assert_eq!(entries[0]["startedDateTime"], "2024-05-01T12:30:00.000Z");
    assert_eq!(entries[0]["request"]["cookies"][0]["value"], "abc123");
    let socket = entries.iter().find(|e| e["response"]["status"] == 101).unwrap();
    assert_eq!(socket["_webSocketMessages"].as_array().unwrap().len(), 3);
    let (_, some) = call(&r, &r.token, "GET", "/api/har?q=method%3APOST", Value::Null, None).await;
    assert_eq!(json_of(&some)["log"]["entries"].as_array().unwrap().len(), 2);
    let (_, picked) = call(&r, &r.token, "GET", &format!("/api/har?ids={},{}", first.id, login.id), Value::Null, None).await;
    assert_eq!(json_of(&picked)["log"]["entries"].as_array().unwrap().len(), 2);
    let (code, bad) = call(&r, &r.token, "GET", "/api/har?q=status%3Anope", Value::Null, None).await;
    assert_eq!((code, json_of(&bad)["code"].as_str()), (400, Some("bad_query")));

    // HAR files carry whole requests: agents get none of it.
    for (method, path) in [("GET", "/api/har"), ("POST", "/api/har/import"), ("POST", "/api/har/export-file")] {
        let (code, _) = call(&r, &r.agent_token, method, path, json!({}), None).await;
        assert_eq!(code, 403, "{method} {path}");
    }
    // Outside the app there are no file dialogs.
    let (code, body) = call(&r, &r.token, "POST", "/api/har/export-file", json!({}), None).await;
    assert_eq!((code, json_of(&body)["code"].as_str()), (501, Some("no_dialogs")));

    // The export imports into a fresh project, from a file on disk.
    let file = dir.path().join("export.har");
    std::fs::write(&file, &all).unwrap();
    let home_b = Home { root: dir.path().join("b") };
    let b = start(&home_b, None).await;
    let path = format!("/api/har/import?path={}", plonix_core::client::encode(&file.to_string_lossy()));
    let (code, body) = call(&b, &b.token, "POST", &path, Value::Null, None).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(json_of(&body)["imported"].as_u64(), Some(6));
    let mut a_rows: Vec<_> = (1..=6).map(|id| r.engine.store.get_exchange(id).unwrap().unwrap()).collect();
    let mut b_rows: Vec<_> = (1..=6).map(|id| b.engine.store.get_exchange(id).unwrap().unwrap()).collect();
    a_rows.sort_by_key(|e| (e.ts, e.url()));
    b_rows.sort_by_key(|e| (e.ts, e.url()));
    for (x, y) in a_rows.iter().zip(&b_rows) {
        assert_eq!((x.ts, x.url(), &x.method, x.status), (y.ts, y.url(), &y.method, y.status));
        assert_eq!((&x.req_body, &x.resp_body), (&y.req_body, &y.resp_body), "{}", x.url());
    }
    r.engine.request_shutdown();
    b.engine.request_shutdown();
}

/// A CA for client certificates and one certificate it signed for `cn`.
fn client_ca_and_cert(cn: &str) -> (CertificateDer<'static>, String, String) {
    let (ca_pem, ca_key) = CertAuthority::generate_pem().unwrap();
    let issuer = rcgen::Issuer::from_ca_cert_pem(&ca_pem, rcgen::KeyPair::from_pem(&ca_key).unwrap()).unwrap();
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec![]).unwrap();
    params.distinguished_name.push(rcgen::DnType::CommonName, cn);
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ClientAuth];
    let cert = params.signed_by(&key, &issuer).unwrap();
    (CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap(), cert.pem(), key.serialize_pem())
}

#[derive(Debug)]
struct Fixed(Arc<rustls::sign::CertifiedKey>);
impl rustls::server::ResolvesServerCert for Fixed {
    fn resolve(&self, _: rustls::server::ClientHello<'_>) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(self.0.clone())
    }
}

/// An HTTPS server for `localhost` that only talks to clients presenting a
/// certificate from `client_ca`, and greets them by their common name.
async fn serve_mtls(client_ca: CertificateDer<'static>) -> (SocketAddr, CertificateDer<'static>) {
    let (ca_pem, ca_key) = CertAuthority::generate_pem().unwrap();
    let server_ca = CertAuthority::from_pem(&ca_pem, &ca_key).unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.add(client_ca).unwrap();
    let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone()).build().unwrap();
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(Arc::new(Fixed(server_ca.leaf_for("localhost").unwrap())));
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(s).await else { return };
                let peer = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| c.clone().into_owned());
                let cn = peer
                    .and_then(|c| x509_parser::parse_x509_certificate(c.as_ref()).ok().map(|(_, x)| x.subject().to_string()))
                    .unwrap_or_default();
                let svc = service_fn(move |_req: Request<Incoming>| {
                    let cn = cn.clone();
                    async move { Ok::<_, Infallible>(Response::new(Full::new(Bytes::from(format!("hello {cn}"))))) }
                });
                let _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(tls), svc).await;
            });
        }
    });
    (addr, CertificateDer::from_pem_slice(ca_pem.as_bytes()).unwrap())
}

/// Through the proxy, over a tunnel that trusts the Plonix CA.
async fn get_via_proxy(r: &Running, port: u16) -> (u16, String) {
    let target = format!("localhost:{port}");
    let mut tcp = TcpStream::connect(r.proxy_addr).await.unwrap();
    tcp.write_all(format!("CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n\r\n").as_bytes()).await.unwrap();
    let mut buf = [0u8; 1024];
    let n = tcp.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 200"));
    let mut roots = rustls::RootCertStore::empty();
    roots.add(r.engine.ca.ca_der().clone()).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(config)).connect(ServerName::try_from("localhost").unwrap(), tcp).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tls)).await.unwrap();
    tokio::spawn(conn);
    let req = Request::builder().uri("/").header("host", &target).body(Full::new(Bytes::new())).unwrap();
    let resp = sender.send_request(req).await.unwrap();
    let status = resp.status().as_u16();
    (status, String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned())
}

#[tokio::test(flavor = "multi_thread")]
async fn client_certificates_reach_servers_that_require_them() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let (client_ca, cert_pem, key_pem) = client_ca_and_cert("alice");
    let (up, server_root) = serve_mtls(client_ca).await;
    let r = start(&home, Some(server_root)).await;
    r.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    let url = format!("https://localhost:{}/", up.port());
    let send = || r.engine.send(SendRequest { method: "GET".into(), url: url.clone(), ..Default::default() }, "test");

    // Without a certificate the server refuses, and the error says what to do.
    let ex = send().await.unwrap();
    let error = ex.error.unwrap_or_default();
    assert!(ex.status.is_none() && error.contains("client certificate"), "{error}");

    // Added through the API (the user's alone), it is described, never shown.
    let (code, _) = call(&r, &r.agent_token, "GET", "/api/client-certs", Value::Null, None).await;
    assert_eq!(code, 403);
    let input = json!({ "host": "LOCALHOST", "cert_pem": cert_pem, "key_pem": key_pem, "note": "staging" });
    let (code, _) = call(&r, &r.agent_token, "POST", "/api/client-certs", input.clone(), None).await;
    assert_eq!(code, 403);
    let (code, body) = call(&r, &r.token, "POST", "/api/client-certs", json!({ "host": "localhost", "cert_pem": cert_pem }), None).await;
    assert_eq!((code, json_of(&body)["code"].as_str()), (400, Some("bad_cert")), "a certificate without its key is refused");
    let (code, body) = call(&r, &r.token, "POST", "/api/client-certs", input, None).await;
    assert_eq!(code, 200, "{body}");
    let info = json_of(&body);
    assert_eq!((info["host"].as_str(), info["subject"].as_str(), info["note"].as_str()), (Some("localhost"), Some("CN=alice"), Some("staging")));
    let (_, list) = call(&r, &r.token, "GET", "/api/client-certs", Value::Null, None).await;
    assert!(!list.contains("PRIVATE KEY") && !list.contains(&key_pem[40..80]), "keys never leave the engine: {list}");
    assert_eq!(json_of(&list)["certs"].as_array().unwrap().len(), 1);

    // Bench sends and the proxy both present it, and note it on the exchange.
    let ex = send().await.unwrap();
    assert_eq!((ex.status, ex.resp_body.as_slice()), (Some(200), &b"hello CN=alice"[..]), "{:?}", ex.error);
    assert_eq!(ex.client_cert.as_deref(), Some("alice for localhost"));
    let stored = r.engine.store.get_exchange(ex.id).unwrap().unwrap();
    assert_eq!(stored.client_cert.as_deref(), Some("alice for localhost"));
    let (status, body) = get_via_proxy(&r, up.port()).await;
    assert_eq!((status, body.as_str()), (200, "hello CN=alice"));
    let latest = loop {
        let n = r.engine.store.count().unwrap();
        if n >= 3 {
            break r.engine.store.get_exchange(n).unwrap().unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert_eq!((latest.source, latest.client_cert.as_deref()), (Some(Source::Proxy), Some("alice for localhost")));
    let (_, view) = call(&r, &r.token, "GET", &format!("/api/traffic/{}", latest.id), Value::Null, None).await;
    assert_eq!(json_of(&view)["client_cert"], "alice for localhost");

    // Switched off, nothing is presented; removed, it is gone.
    r.engine.set_client_certs_on(false).unwrap();
    assert!(send().await.unwrap().status.is_none());
    r.engine.set_client_certs_on(true).unwrap();
    assert_eq!(send().await.unwrap().status, Some(200));
    let id = info["id"].as_i64().unwrap();
    let (code, _) = call(&r, &r.token, "DELETE", &format!("/api/client-certs/{id}"), Value::Null, None).await;
    assert_eq!(code, 200);
    assert!(send().await.unwrap().status.is_none());
    r.engine.request_shutdown();
}
