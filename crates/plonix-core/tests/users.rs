//! Saved users: acting as one in the browser, the Bench sending as one, and
//! cookies the server sets kept with the user.

use std::convert::Infallible;
use std::net::SocketAddr;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use plonix_core::engine::SendRequest;
use plonix_core::paths::Home;
use plonix_core::project::{self, Project};
use plonix_core::scope::Decision;
use plonix_core::session::{self, OpenOptions, Session};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};

/// Echoes the Cookie header it got; `/rotate` also sets a new session.
async fn upstream_handler(req: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let cookie = req.headers().get("cookie").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    let mut b = Response::builder().header("content-type", "text/plain");
    if req.uri().path() == "/rotate" {
        b = b.header("set-cookie", "s=rotated; Path=/; HttpOnly");
    }
    Ok(b.body(Full::new(Bytes::from(cookie))).unwrap())
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

/// A browser request through the proxy carrying its own cookie. Returns the
/// body and whether the browser was handed a Set-Cookie.
async fn browse(proxy: SocketAddr, url: &str) -> (String, bool) {
    let tcp = TcpStream::connect(proxy).await.unwrap();
    let (mut sender, conn) = hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(tcp)).await.unwrap();
    tokio::spawn(conn);
    let uri: hyper::Uri = url.parse().unwrap();
    let req = Request::builder()
        .uri(url)
        .header("host", uri.authority().unwrap().as_str())
        .header("cookie", "s=browser")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let resp = sender.send_request(req).await.unwrap();
    let set = resp.headers().contains_key("set-cookie");
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    (String::from_utf8_lossy(&body).into_owned(), set)
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

fn cookie(s: &Session, user: &str, name: &str) -> Option<String> {
    let users = s.engine.store.saved_users().unwrap();
    let u = users.into_iter().find(|u| u.id == user)?;
    u.cookies.into_iter().find(|c| c.name == name).map(|c| c.value)
}

#[tokio::test(flavor = "multi_thread")]
async fn acting_as_a_saved_user_swaps_the_browser_session() {
    let dir = tempfile::tempdir().unwrap();
    let home = Home { root: dir.path().join("home") };
    let p = Project::create(&home.root.join("work").join(project::slug("Users")), "Users").unwrap();
    project::remember(&home, &p, false).unwrap();
    let s = std::sync::Arc::new(session::open(&home, p, OpenOptions { api_port: Some(0), ..Default::default() }).await.unwrap());
    let up = serve_http().await;
    let url = |path: &str| format!("http://localhost:{}{path}", up.port());

    let s2 = s.clone();
    let saved = blocking(move || {
        api(&s2, "PUT", "/api/users", Some(json!({ "users": [
            { "name": "Maya", "headers": [["Cookie", "s=maya; theme=dark"]] },
            { "name": "Dana", "headers": [["Cookie", "s=dana"]], "keep_fresh": false },
        ] })))
    })
    .await
    .unwrap();
    assert_eq!(saved["users"][0]["cookies"][1]["name"], "theme", "the Cookie header is split into cookies");

    // No user picked: the browser's own session passes.
    s.engine.decide("localhost", Decision::Accepted, false, "").unwrap();
    assert_eq!(browse(s.proxy_addr(), &url("/")).await.0, "s=browser");

    // Acting as Maya: in-scope browser traffic carries Maya's cookies, and a
    // cookie the server sets goes to Maya rather than to the browser.
    let s2 = s.clone();
    blocking(move || api(&s2, "PUT", "/api/users/acting", Some(json!({ "id": "maya" })))).await.unwrap();
    assert_eq!(browse(s.proxy_addr(), &url("/")).await.0, "s=maya; theme=dark");
    let (_, handed_to_browser) = browse(s.proxy_addr(), &url("/rotate")).await;
    assert!(!handed_to_browser);
    assert_eq!(cookie(&s, "maya", "s").as_deref(), Some("rotated"));
    assert_eq!(browse(s.proxy_addr(), &url("/")).await.0, "s=rotated; theme=dark");

    // Hosts outside scope are left alone.
    s.engine.decide("localhost", Decision::Rejected, false, "").unwrap();
    assert_eq!(browse(s.proxy_addr(), &url("/")).await.0, "s=browser");
    s.engine.decide("localhost", Decision::Accepted, false, "").unwrap();

    // The Bench sends as a chosen user; a user that does not keep its cookies
    // fresh is left as it was.
    let send = |as_user: &str| SendRequest {
        method: "GET".into(),
        url: url("/rotate"),
        headers: vec![("Cookie".into(), "s=draft".into())],
        body: None,
        body_base64: None,
        as_user: Some(as_user.into()),
    };
    let ex = s.engine.send(send("dana"), "test").await.unwrap();
    assert_eq!(String::from_utf8_lossy(&ex.resp_body), "s=dana");
    assert!(ex.replaced.iter().any(|r| r.ends_with("Dana")));
    assert_eq!(cookie(&s, "dana", "s").as_deref(), Some("dana"));

    // Back to the browser's own session; an unknown user is refused.
    let s2 = s.clone();
    let bad = blocking(move || api(&s2, "PUT", "/api/users/acting", Some(json!({ "id": "nobody" })))).await;
    assert_eq!(bad.unwrap_err().0, 400);
    let s2 = s.clone();
    blocking(move || api(&s2, "PUT", "/api/users/acting", Some(json!({ "id": null })))).await.unwrap();
    assert_eq!(browse(s.proxy_addr(), &url("/")).await.0, "s=browser");
}
