//! Scale tests: Plonix stays fast with big projects.
//!
//! Each test fills a project with generated traffic (many hosts, cookies,
//! links between hosts, JSON and HTML bodies), then times what the window
//! does all day: the Traffic list and its filters, full-text search, filter
//! suggestions, Scope suggestions and accepting a host, the Map, and the
//! Traffic page of the local API. Every step has a time budget.
//!
//! `small_project_stays_fast` (10,000 exchanges) runs with every `cargo
//! test`. The 100,000-exchange version is ignored by default; CI runs it in
//! release mode:
//!
//! ```text
//! cargo test --release -p plonix-core --test scale -- --ignored
//! ```
//!
//! Budgets are for release builds; debug builds get several times more.

mod common;
use common::Running;

use std::time::{Duration, Instant};

use plonix_core::model::{Exchange, Source};
use plonix_core::paths::Home;
use plonix_core::scope::Decision;
use plonix_core::Engine;

/// Debug builds run SQLite and the analyzers unoptimized, and Windows CI
/// runners write to disk more slowly. The release `scale` job keeps the
/// real budgets.
const DEBUG_FACTOR: u32 = match (cfg!(debug_assertions), cfg!(windows)) {
    (false, _) => 1,
    (true, false) => 5,
    (true, true) => 12,
};

/// Times `f`, prints the result and fails when it takes longer than `budget_ms`.
fn timed<T>(what: &str, budget_ms: u64, f: impl FnOnce() -> T) -> T {
    let start = Instant::now();
    let out = f();
    let took = start.elapsed();
    let budget = Duration::from_millis(budget_ms) * DEBUG_FACTOR;
    eprintln!("  {what:<44} {:>8.1} ms  (budget {} ms)", took.as_secs_f64() * 1000.0, budget.as_millis());
    assert!(took <= budget, "{what} took {took:?}, over its budget of {budget:?}");
    out
}

/// A small, fixed random number generator, so every run sees the same traffic.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const TARGET_HOSTS: [&str; 6] = ["target.test", "www.target.test", "api.target.test", "auth.target.test", "cdn.target.test", "admin.target.test"];
const PATHS: [&str; 10] = ["/", "/login", "/api/v1/users/{n}", "/api/v1/orders/{n}", "/api/v1/search", "/static/app.js", "/static/style.css", "/img/{n}.png", "/account/settings", "/graphql"];

/// Exchange `i` of a generated project: two thirds go to the target's
/// hosts, the rest to a long tail of other hosts, some of which the target
/// links to, shares a session with or is requested from.
fn exchange(i: u64, rng: &mut Rng) -> Exchange {
    let target = rng.below(3) != 0;
    let host = if target {
        TARGET_HOSTS[rng.below(TARGET_HOSTS.len() as u64) as usize].to_string()
    } else {
        format!("svc{}.thirdparty{}.test", rng.below(4), rng.below(150))
    };
    let path = PATHS[rng.below(PATHS.len() as u64) as usize].replace("{n}", &rng.below(5000).to_string());
    let method = if path.starts_with("/api") && rng.below(4) == 0 { "POST" } else { "GET" };
    let status = match rng.below(20) {
        0 => 500,
        1 => 404,
        2 => 302,
        3 => 401,
        _ => 200,
    };
    let session = format!("sess-{:016x}", rng.below(64));
    let mut req_headers = vec![
        ("Host".to_string(), host.clone()),
        ("User-Agent".to_string(), "Mozilla/5.0 (Macintosh) scale-test".to_string()),
        ("Accept".to_string(), "*/*".to_string()),
    ];
    if target || rng.below(10) == 0 {
        req_headers.push(("Cookie".to_string(), format!("sid={session}; theme=dark")));
    }
    if !target {
        req_headers.push(("Referer".to_string(), format!("https://www.target.test/page/{}", rng.below(100))));
    }
    let (mime, body) = if path.ends_with(".js") {
        ("application/javascript", format!("console.log('bundle {i}');").repeat(8))
    } else if path.ends_with(".css") {
        ("text/css", "body{margin:0;color:#222}".repeat(8))
    } else if path.ends_with(".png") {
        ("image/png", "\u{1}PNG".repeat(32))
    } else if path.starts_with("/api") || path == "/graphql" {
        ("application/json", format!(r#"{{"id":{i},"name":"user {i}","email":"user{i}@target.test","items":[1,2,3]}}"#))
    } else {
        let link = rng.below(150);
        // Pages are a few kilobytes, more than fits next to the row's other
        // columns, as real pages are.
        let filler = "<p>Lorem ipsum dolor sit amet, consectetur adipiscing elit.</p>".repeat(rng.below(80) as usize);
        ("text/html", format!(r#"<html><body><h1>Page {i}</h1>{filler}<a href="https://partner{link}.test/x">partner</a><script src="https://svc0.thirdparty{link}.test/w.js"></script></body></html>"#))
    };
    let mut resp_headers = vec![("Content-Type".to_string(), mime.to_string()), ("Server".to_string(), "nginx".to_string())];
    // One exchange in 997 carries a rare value that full-text search must find.
    if i.is_multiple_of(997) {
        resp_headers.push(("X-Trace".to_string(), format!("needle-in-the-haystack-{i}")));
    }
    if path == "/login" {
        resp_headers.push(("Set-Cookie".to_string(), format!("sid={session}; Path=/; HttpOnly")));
    }
    Exchange {
        ts: 1_700_000_000_000 + i as i64 * 250,
        scheme: "https".into(),
        port: 443,
        method: method.into(),
        query: if path == "/api/v1/search" { format!("q=term{}&page={}", rng.below(100), rng.below(10)) } else { String::new() },
        req_body: if method == "POST" { format!(r#"{{"qty":{}}}"#, rng.below(10)).into_bytes() } else { vec![] },
        status: Some(status),
        resp_body: body.into_bytes(),
        duration_ms: rng.below(800) as i64,
        tls_sans: if host.ends_with("target.test") { vec!["target.test".into(), "*.target.test".into()] } else { vec![] },
        source: Some(if i.is_multiple_of(50) { Source::Replay } else { Source::Proxy }),
        http_version: "HTTP/2".into(),
        host,
        path,
        req_headers,
        resp_headers,
        ..Default::default()
    }
}

async fn start(home: &Home) -> Running {
    common::open(home, "scale", None).await
}

fn search(engine: &Engine, q: &str, sort: Option<&str>, limit: usize, offset: usize) -> (usize, i64) {
    let query = engine.filters().parse(q).unwrap_or_else(|e| panic!("{q}: {e}"));
    let (items, total) = engine.store.search_sorted(&query, &engine.rules(), sort, limit, offset).unwrap();
    (items.len(), total)
}

/// Fills a project with `n` exchanges and times the everyday operations on it.
/// `ingest_ms` is the budget for recording all of them.
async fn run(n: u64, ingest_ms: u64) {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::resolve(Some(dir.path())).unwrap();
    let r = start(&home).await;
    let engine = r.engine.clone();
    eprintln!("scale: {n} exchanges");

    // Recording, the way the proxy does it: exchanges are queued as they
    // complete and the recorder stores them, scope analysis included.
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let generated: Vec<Exchange> = (0..n).map(|i| exchange(i, &mut rng)).collect();
    let e = engine.clone();
    tokio::task::spawn_blocking(move || {
        timed(&format!("record {n} exchanges"), ingest_ms, || {
            for ex in generated {
                e.enqueue(ex);
            }
            while e.store.count().unwrap() < n as i64 {
                std::thread::sleep(Duration::from_millis(5));
            }
        })
    })
    .await
    .unwrap();
    assert_eq!(engine.store.count().unwrap(), n as i64);

    // Accepting the target: rebuilds scope evidence from all stored traffic.
    let e = engine.clone();
    let budget = ingest_ms / 2;
    tokio::task::spawn_blocking(move || timed("accept *.target.test (rescan)", budget, || e.decide("*.target.test", Decision::Accepted, true, "").unwrap()))
        .await
        .unwrap();

    let e = engine.clone();
    tokio::task::spawn_blocking(move || {
        let engine = &*e;
        // Traffic list and its filters.
        let (page, total) = timed("traffic: first page, no filter", 150, || search(engine, "", None, 100, 0));
        assert_eq!((page, total), (100, n as i64));
        timed("traffic: deep page (offset 90%)", 400, || search(engine, "", None, 100, (n as usize) * 9 / 10));
        let (_, in_scope) = timed("traffic: scope:in", 400, || search(engine, "scope:in", None, 100, 0));
        assert!(in_scope > 0 && in_scope < n as i64);
        timed("traffic: host + status class", 400, || search(engine, "host:api.target.test status:5xx", None, 100, 0));
        timed("traffic: method + path + negation", 400, || search(engine, "method:POST path:/api -status:200", None, 100, 0));
        timed("traffic: hide static", 400, || search(engine, "scope:in -kind:static", None, 100, 0));
        let (_, found) = timed("traffic: full-text search", 400, || search(engine, "needle-in-the-haystack", None, 100, 0));
        assert_eq!(found, n.div_ceil(997) as i64);
        timed("traffic: full-text, no match", 400, || search(engine, "nothing-like-this-anywhere", None, 100, 0));
        timed("traffic: sorted by size", 600, || search(engine, "", Some("-size"), 100, 0));
        timed("traffic: sorted by host, filtered", 600, || search(engine, "scope:in", Some("host"), 100, 0));
        let f = timed("traffic: filter suggestions (facets)", 400, || engine.store.facets(&engine.rules()).unwrap());
        assert!(f.sampled > 0);

        // Scope.
        let sug = timed("scope: suggestions", 400, || engine.store.suggestions(&engine.rules()).unwrap());
        assert!(!sug.is_empty(), "the generated traffic links to other hosts");
        timed("scope: reject a suggestion", 400, || engine.decide(&sug[0].domain, Decision::Rejected, false, "").unwrap());
        let hosts: Vec<String> = engine.store.distinct_hosts().unwrap().into_iter().filter(|h| !engine.rules().in_scope(h)).collect();
        let keep = (1..=200).collect();
        let doomed = timed("storage: count out-of-scope traffic", 600, || engine.store.count_for_hosts(&hosts, &keep).unwrap());
        assert!(doomed > 0);

        // Map: hosts, then the endpoints and technologies of the busiest one.
        let hosts = timed("map: hosts", 400, || engine.store.hosts(&engine.rules()).unwrap());
        assert!(hosts.len() > 100);
        let busiest = hosts[0].host.clone();
        let endpoints = timed("map: endpoints of the busiest host", 1500, || engine.store.endpoints(&busiest).unwrap());
        assert!(endpoints.iter().any(|e| e.path.contains("{id}")), "ids fold into {{id}}");
        timed("map: technologies of the busiest host", 600, || engine.detect_host(&busiest).unwrap());
        timed("map: technologies of every host", 8000, || engine.detect_all().unwrap());
    })
    .await
    .unwrap();

    // The Traffic page as the window loads it, over HTTP.
    for (what, path) in [
        ("api: /api/traffic first page", "/api/traffic?limit=200"),
        ("api: /api/traffic filtered + sorted", "/api/traffic?q=scope%3Ain%20-kind%3Astatic&sort=-ms&limit=200"),
        ("api: /api/traffic/facets", "/api/traffic/facets"),
        ("api: /api/scope", "/api/scope"),
        ("api: /api/hosts", "/api/hosts"),
    ] {
        let (url, auth) = (format!("http://{}{path}", r.api_addr), format!("Bearer {}", r.token));
        let status = tokio::task::spawn_blocking(move || {
            timed(what, 600, || ureq::get(&url).set("Authorization", &auth).set("X-Plonix-Client", "test").call().map(|r| r.status()).unwrap_or(0))
        })
        .await
        .unwrap();
        assert_eq!(status, 200, "{what}");
    }
    engine.request_shutdown();
}

/// Runs with every `cargo test`: a project of 10,000 exchanges.
#[tokio::test(flavor = "multi_thread")]
async fn small_project_stays_fast() {
    run(10_000, 6_000).await;
}

/// 100,000 exchanges. Run with `cargo test --release -- --ignored`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "takes a while; run with --ignored in release mode"]
async fn large_project_stays_fast() {
    run(100_000, 90_000).await;
}

