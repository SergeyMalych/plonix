//! A project that follows a program: its scope, headers, request rate and
//! testing rules hold for every request Plonix sends.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use plonix_core::Engine;
use plonix_core::ca::CertAuthority;
use plonix_core::engine::{self, EngineConfig, Running, SendError, SendRequest};
use plonix_core::paths::Home;
use plonix_core::bounty::{Asset, AssetKind, Program, RequiredHeader, Rules};
use plonix_core::scope::Decision;
use plonix_core::store::Store;
use plonix_core::upstream::Upstream;
use tokio::net::{TcpListener, TcpStream};

/// Answers with the request's headers.
async fn echo(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let mut text = String::new();
    for (k, v) in req.headers() {
        text.push_str(&format!("{}: {}\n", k, v.to_str().unwrap_or("")));
    }
    Ok(Response::builder().header("content-type", "text/plain").body(Full::new(Bytes::from(text))).unwrap())
}

async fn serve() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            tokio::spawn(hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(s), service_fn(echo)));
        }
    });
    addr
}

async fn start(home: &Home) -> Running {
    home.ensure().unwrap();
    std::fs::create_dir_all(home.root.join("projects")).unwrap();
    let ca = Arc::new(CertAuthority::load_or_create(home).unwrap());
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

async fn via_proxy(proxy: SocketAddr, url: &str) -> String {
    let tcp = TcpStream::connect(proxy).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tcp)).await.unwrap();
    tokio::spawn(conn);
    let uri: hyper::Uri = url.parse().unwrap();
    let req = Request::builder().uri(url).header("host", uri.authority().unwrap().as_str()).body(Full::new(Bytes::new())).unwrap();
    let resp = sender.send_request(req).await.unwrap();
    String::from_utf8_lossy(&resp.into_body().collect().await.unwrap().to_bytes()).into_owned()
}

fn program() -> Program {
    Program {
        id: "acme".into(),
        name: "Acme Cloud".into(),
        platform: "pasted".into(),
        url: String::new(),
        bounty: true,
        assets: vec![
            Asset { identifier: "127.0.0.0/8".into(), kind: AssetKind::Cidr, in_scope: true, bounty: true, instruction: String::new(), max_severity: String::new() },
            Asset { identifier: "127.0.0.9".into(), kind: AssetKind::Ip, in_scope: false, bounty: false, instruction: "Third party".into(), max_severity: String::new() },
        ],
        rules: Rules {
            rate_per_second: Some(20.0),
            headers: vec![
                RequiredHeader { name: "X-Bug-Bounty".into(), value: "neo".into(), needs_value: false },
                RequiredHeader { name: "X-Unfilled".into(), value: "<your username>".into(), needs_value: true },
            ],
            no_automation: true,
            no_intrusive: true,
            not_accepted: vec!["Missing security headers".into()],
        },
        synced_at: 0,
    }
}

#[tokio::test]
async fn a_followed_program_is_enforced_on_every_send() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let up = serve().await;
    let r = start(&home).await;
    let url = format!("http://127.0.0.1:{}/x", up.port());

    // Out of scope until the program is applied.
    assert!(matches!(r.engine.send(SendRequest { method: "GET".into(), url: url.clone(), ..Default::default() }, "t").await, Err(SendError::OutOfScope { .. })));

    let preview = r.engine.program_preview(program()).unwrap();
    assert_eq!(preview.scope.iter().map(|c| (c.pattern.as_str(), c.change)).collect::<Vec<_>>(), vec![("127.0.0.0/8", "add"), ("127.0.0.9", "add")]);
    assert!(r.engine.rules().decide("127.0.0.1") == Decision::Unknown, "a preview changes nothing");

    r.engine.apply_program(program()).unwrap();
    assert_eq!(r.engine.rules().decide("127.0.0.1"), Decision::Accepted);
    assert_eq!(r.engine.rules().decide("127.0.0.9"), Decision::Rejected, "the exclusion beats the range");

    // Required headers ride on every send, at the program's rate.
    let started = Instant::now();
    for _ in 0..4 {
        let ex = r.engine.send(SendRequest { method: "GET".into(), url: url.clone(), ..Default::default() }, "bench").await.unwrap();
        let body = String::from_utf8_lossy(&ex.resp_body).into_owned();
        assert!(body.contains("x-bug-bounty: neo"), "{body}");
        assert!(!body.contains("x-unfilled"), "a header with a placeholder is not sent");
    }
    assert!(started.elapsed() >= Duration::from_millis(140), "four sends at 20/s take at least 150ms, took {:?}", started.elapsed());

    // A header the user set is kept as it is.
    let ex = r.engine.send(SendRequest { method: "GET".into(), url: url.clone(), headers: vec![("X-Bug-Bounty".into(), "mine".into())], ..Default::default() }, "bench").await.unwrap();
    assert!(String::from_utf8_lossy(&ex.resp_body).contains("x-bug-bounty: mine"));

    // Browser traffic to the program's hosts carries the header too.
    assert!(via_proxy(r.proxy_addr, &url).await.contains("x-bug-bounty: neo"));

    // Automated testing is off.
    let scan = r.engine.scan(plonix_core::scan::ScanRequest { host: "127.0.0.1".into(), ..Default::default() }, "scan").await;
    assert!(matches!(&scan, Err(SendError::NotAllowed(m)) if m.contains("Acme Cloud does not allow automated testing")), "{scan:?}");
    let crawl = r.engine.crawl(plonix_core::crawl::CrawlRequest { host: "127.0.0.1".into(), ..Default::default() }, "crawl").await;
    assert!(matches!(crawl, Err(SendError::NotAllowed(_))));
    let run = r.engine.run(plonix_core::runs::RunRequest::default(), "run").await;
    assert!(matches!(run, Err(SendError::NotAllowed(_))));

    // The program survives a restart of the project.
    let store = Store::open(&home.project_db("test")).unwrap();
    let again = Engine::new("test", store, Arc::new(CertAuthority::load_or_create(&home).unwrap()), Upstream::new(false, vec![]).unwrap()).unwrap();
    assert_eq!(again.program().unwrap().program.name, "Acme Cloud");

    // Re-applying a changed program replaces its old rules.
    let mut changed = program();
    changed.assets.pop();
    let preview = r.engine.program_preview(changed.clone()).unwrap();
    assert_eq!(preview.scope.iter().map(|c| (c.pattern.as_str(), c.change)).collect::<Vec<_>>(), vec![("127.0.0.0/8", "same"), ("127.0.0.9", "remove")]);
    r.engine.apply_program(changed).unwrap();
    assert_eq!(r.engine.rules().decide("127.0.0.9"), Decision::Accepted);

    // Stopping can take the scope with it.
    assert!(r.engine.clear_program(true).unwrap());
    assert!(r.engine.program().is_none());
    assert_eq!(r.engine.rules().decide("127.0.0.1"), Decision::Unknown);
}

#[tokio::test]
async fn program_routes_are_for_the_user_only() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().into() };
    let r = start(&home).await;
    let base = format!("http://{}", r.api_addr);
    let user = home.load_or_create_token().unwrap();
    let agent = home.load_or_create_agent_token().unwrap();
    let call = move |method: &str, path: &str, token: &str, body: Option<serde_json::Value>| {
        let req = ureq::request(method, &format!("{base}{path}")).set("Authorization", &format!("Bearer {token}"));
        match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        }
    };
    tokio::task::spawn_blocking(move || {
        let read = call("POST", "/api/program/read", &user, Some(serde_json::json!({ "text": "In scope\n- app.acme.io\nOut of scope\n- Self-XSS\nLimit to 2 requests per second." })))
            .unwrap()
            .into_json::<serde_json::Value>()
            .unwrap();
        assert_eq!(read["program"]["assets"][0]["identifier"], "app.acme.io");
        assert_eq!(read["program"]["rules"]["rate_per_second"], 2.0);
        let applied = call("POST", "/api/program/apply", &user, Some(serde_json::json!({ "program": read["program"] }))).unwrap().into_json::<serde_json::Value>().unwrap();
        assert_eq!(applied["scope"][0]["change"], "add");
        let platforms = call("GET", "/api/platforms", &user, None).unwrap().into_json::<serde_json::Value>().unwrap();
        assert!(platforms["platforms"].as_array().unwrap().iter().any(|p| p["name"] == "hackerone" && p["connected"] == false));
        let not_connected = call("GET", "/api/platforms/hackerone/programs", &user, None).unwrap_err();
        assert!(matches!(not_connected, ureq::Error::Status(409, _)));
        // Agents can read neither programs nor platform tokens, and change nothing.
        for (m, p) in [("GET", "/api/program"), ("GET", "/api/platforms"), ("POST", "/api/program/clear")] {
            let e = call(m, p, &agent, if m == "POST" { Some(serde_json::json!({})) } else { None }).unwrap_err();
            assert!(matches!(e, ureq::Error::Status(403, _)), "{p}: {e:?}");
        }
    })
    .await
    .unwrap();
}
