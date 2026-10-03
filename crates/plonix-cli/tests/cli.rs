//! End-to-end tests of the `plonix` binary: it starts a real engine in the
//! background, traffic flows through the real proxy to a local target, and
//! every command is run the way a user would run it.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_plonix");

struct Plonix {
    home: tempfile::TempDir,
    env: Vec<(String, String)>,
}

impl Plonix {
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap(), env: vec![] }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.env("PLONIX_HOME", self.home.path()).env_remove("PLONIX_BROWSER").args(args);
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }

    fn run(&self, args: &[&str]) -> Run {
        Run(self.cmd(args).stdin(Stdio::null()).output().unwrap())
    }

    fn run_with_input(&self, args: &[&str], input: &str) -> Run {
        let mut child = self.cmd(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        Run(child.wait_with_output().unwrap())
    }

    fn start(&self) -> String {
        let r = self.run(&["start", "--port", "0", "--api-port", "0", "--project", "cli-test"]);
        r.ok();
        assert!(r.stdout().contains("Plonix engine started."), "{}", r.stdout());
        self.proxy()
    }

    fn proxy(&self) -> String {
        let r = self.run(&["status", "--json"]);
        let v: serde_json::Value = serde_json::from_str(&r.stdout()).unwrap();
        v["status"]["proxy"].as_str().unwrap().to_string()
    }

    /// Searches until at least `n` results show up (recording is asynchronous).
    fn search_until(&self, query: &str, n: usize) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let r = self.run(&["search", "--json", query]);
            let v: serde_json::Value = serde_json::from_str(&r.stdout()).unwrap();
            if v["items"].as_array().unwrap().len() >= n || Instant::now() > deadline {
                return v;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Plonix {
    fn drop(&mut self) {
        let _ = self.cmd(&["stop"]).output();
    }
}

struct Run(Output);

impl Run {
    fn stdout(&self) -> String {
        String::from_utf8_lossy(&self.0.stdout).into_owned()
    }
    fn stderr(&self) -> String {
        String::from_utf8_lossy(&self.0.stderr).into_owned()
    }
    fn code(&self) -> i32 {
        self.0.status.code().unwrap_or(-1)
    }
    fn ok(&self) -> &Self {
        assert_eq!(self.code(), 0, "stdout:\n{}\nstderr:\n{}", self.stdout(), self.stderr());
        self
    }
}

/// A tiny HTTP target. `/` is an HTML page; `/echo` echoes the request.
fn serve_target() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut head = String::new();
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                    if line == "\r\n" {
                        break;
                    }
                    head.push_str(&line);
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let first = head.lines().next().unwrap_or("").to_string();
                let (ctype, text) = if first.contains(" /echo") {
                    ("text/plain", format!("{head}\n{}", String::from_utf8_lossy(&body)))
                } else {
                    ("text/html", "<h1>welcome to the target</h1>".to_string())
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                );
                let mut s = stream;
                let _ = s.write_all(resp.as_bytes());
            });
        }
    });
    port
}

fn via_proxy(proxy: &str, url: &str, headers: &[(&str, &str)]) -> String {
    let agent = ureq::AgentBuilder::new().proxy(ureq::Proxy::new(format!("http://{proxy}")).unwrap()).build();
    let mut req = agent.get(url);
    for (k, v) in headers {
        req = req.set(k, v);
    }
    req.call().unwrap().into_string().unwrap()
}

#[test]
fn help_explains_the_tool() {
    let p = Plonix::new();
    let r = p.run(&["--help"]);
    r.ok();
    for needle in ["open", "search", "scope", "replay", "Exit codes", "plonix open example.com"] {
        assert!(r.stdout().contains(needle), "help is missing {needle}:\n{}", r.stdout());
    }
    let r = p.run(&["search", "--help"]);
    assert!(r.stdout().contains("status:404|5xx|none"));
    assert_eq!(p.run(&["bogus"]).code(), 2);
}

#[test]
fn commands_explain_when_the_engine_is_not_running() {
    let p = Plonix::new();
    let r = p.run(&["status"]);
    assert_eq!(r.code(), 3);
    assert!(r.stdout().contains("not running"), "{}", r.stdout());
    let r = p.run(&["search", "host:example.com"]);
    assert_eq!(r.code(), 3);
    assert!(r.stderr().contains("plonix start"), "{}", r.stderr());
    let r = p.run(&["stop"]);
    r.ok();
    assert!(r.stdout().contains("not running"));
}

#[test]
fn capture_search_show_scope_and_replay() {
    let p = Plonix::new();
    let target = serve_target();
    let proxy = p.start();

    // Starting again is harmless.
    assert!(p.run(&["start", "--port", "0", "--api-port", "0"]).ok().stdout().contains("already running"));
    assert!(p.run(&["search"]).ok().stdout().contains("Nothing captured yet"));

    // Browse: a page on localhost, which pulls something from 127.0.0.1.
    let page = format!("http://localhost:{target}/");
    assert!(via_proxy(&proxy, &page, &[]).contains("welcome"));
    via_proxy(&proxy, &format!("http://127.0.0.1:{target}/echo?x=1"), &[("Referer", &page)]);
    let v = p.search_until("", 2);
    assert_eq!(v["total"], 2);

    // Text search output and filters.
    let r = p.run(&["search", "host:localhost"]);
    let out = r.ok().stdout();
    assert!(out.contains(&format!("http://localhost:{target}/")), "{out}");
    assert!(out.contains("1 of 1 match(es)"), "{out}");
    let r = p.run(&["search", "path:/echo", "method:GET", "status:2xx", "mime:plain"]);
    assert!(r.ok().stdout().contains("/echo?x=1"));
    assert!(p.run(&["search", "status:404"]).ok().stdout().contains("No traffic matches"));
    let out = p.run(&["search", "-n", "5", "status:2xx", "-path:/echo"]).ok().stdout();
    assert!(out.contains("1 of 1 match(es)") && !out.contains("/echo"), "negated terms work:\n{out}");
    let r = p.run(&["search", "status:abc"]);
    assert_eq!(r.code(), 2);
    assert!(r.stderr().contains("bad status"), "{}", r.stderr());

    // One exchange in full.
    let echo_id = v["items"][0]["id"].as_i64().unwrap().to_string();
    let r = p.run(&["show", &echo_id]);
    let out = r.ok().stdout();
    assert!(out.contains("GET /echo?x=1 HTTP/1.1"), "{out}");
    assert!(out.contains("HTTP/1.1 200"), "{out}");
    assert_eq!(p.run(&["show", "999999"]).code(), 5);

    // Nothing is sent to a host that is not accepted.
    let r = p.run(&["replay", &echo_id]);
    assert_eq!(r.code(), 4, "{}", r.stdout());
    assert!(r.stderr().contains("plonix scope accept 127.0.0.1"), "{}", r.stderr());

    // Accepting localhost surfaces 127.0.0.1 as a suggestion, with evidence.
    let r = p.run(&["scope", "accept", "localhost"]);
    let out = r.ok().stdout();
    assert!(out.contains("✓ in scope:     localhost"), "{out}");
    assert!(out.contains("1 suggested domain(s) waiting"), "{out}");
    let out = p.run(&["scope"]).ok().stdout();
    assert!(out.contains("✓ in   localhost"), "{out}");
    assert!(out.contains("? 127.0.0.1"), "{out}");
    assert!(out.contains(&format!("#{echo_id}")), "evidence should cite the exchange:\n{out}");

    // Interactive review: accept the suggestion.
    let r = p.run_with_input(&["scope", "review"], "a\n");
    let out = r.ok().stdout();
    assert!(out.contains("? 127.0.0.1") && out.contains("✓ in scope: 127.0.0.1"), "{out}");
    assert!(p.run(&["scope", "review"]).ok().stdout().contains("No suggestions to review"));

    // Replay, modified.
    let r = p.run(&["replay", &echo_id, "-X", "POST", "-t", "/echo?y=2", "-H", "X-Plonix-Test: yes", "-d", "hello"]);
    let out = r.ok().stdout();
    assert!(out.contains(&format!("Replayed #{echo_id} as #")), "{out}");
    assert!(out.contains("POST /echo?y=2"), "{out}");
    assert!(out.to_ascii_lowercase().contains("x-plonix-test: yes"), "upstream should echo the header:\n{out}");
    assert!(out.contains("hello"), "{out}");
    let v = p.search_until("source:replay", 1);
    assert_eq!(v["items"][0]["method"], "POST");
    assert!(p.run(&["replay", &echo_id, "-H", "broken"]).code() == 1);

    // Reject and remove.
    assert!(p.run(&["scope", "reject", "127.0.0.1"]).ok().stdout().contains("✗ out of scope: 127.0.0.1"));
    assert_eq!(p.run(&["replay", &echo_id]).code(), 4);
    assert!(p.run(&["scope", "remove", "127.0.0.1"]).ok().stdout().contains("removed rule for 127.0.0.1"));

    let out = p.run(&["hosts"]).ok().stdout();
    assert!(out.contains("127.0.0.1") && out.contains("localhost"), "{out}");

    let out = p.run(&["status"]).ok().stdout();
    assert!(out.contains("Project    cli-test") && out.contains("Captured   3 exchange(s)"), "{out}");

    assert!(p.run(&["stop"]).ok().stdout().contains("stopped"));
    assert_eq!(p.run(&["status"]).code(), 3);
    assert!(!p.home.path().join("engine.json").exists());
}

fn fake_browser(dir: &Path) -> PathBuf {
    let script = dir.join("fake-chrome");
    let log = dir.join("browser-args.txt");
    std::fs::write(&script, format!("#!/bin/sh\nfor a in \"$@\"; do echo \"$a\"; done > '{}'\n", log.display())).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    script
}

fn read_when_ready(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match std::fs::read_to_string(path) {
            Ok(s) if !s.is_empty() => return s,
            _ if Instant::now() > deadline => panic!("{} was never written", path.display()),
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[test]
fn open_starts_everything_and_launches_the_browser_through_the_proxy() {
    let mut p = Plonix::new();
    let bin = tempfile::tempdir().unwrap();
    let browser = fake_browser(bin.path());
    p.env.push(("PLONIX_BROWSER".into(), browser.display().to_string()));
    let target = serve_target();

    let r = p.run(&["open", &format!("127.0.0.1:{target}/app"), "--no-watch", "--port", "0", "--api-port", "0"]);
    let out = r.ok().stdout();
    assert!(out.contains("(created)"), "{out}");
    assert!(out.contains("(started, project 127.0.0.1)"), "{out}");
    assert!(out.contains("✓ Scope        127.0.0.1 (+ subdomains)"), "{out}");
    assert!(out.contains("✓ Browser      fake-chrome (isolated profile, trusts Plonix)"), "{out}");
    assert!(out.contains("plonix ca trust"), "first run shows trust guidance:\n{out}");
    assert!(p.home.path().join("ca.pem").exists());

    let proxy = p.proxy();
    let args = read_when_ready(&bin.path().join("browser-args.txt"));
    let args: Vec<&str> = args.lines().collect();
    assert!(args.contains(&format!("--proxy-server=http://{proxy}").as_str()), "{args:?}");
    assert!(args.contains(&format!("--user-data-dir={}", p.home.path().join("browser").display()).as_str()), "{args:?}");
    assert!(args.iter().any(|a| a.starts_with("--ignore-certificate-errors-spki-list=") && a.len() > 40), "{args:?}");
    assert_eq!(args.last().unwrap(), &format!("http://127.0.0.1:{target}/app"));

    // What the browser would do: load the target through the proxy.
    via_proxy(&proxy, &format!("http://127.0.0.1:{target}/app"), &[]);
    let v = p.search_until("scope:in", 1);
    assert_eq!(v["items"][0]["path"], "/app");

    // Second run: reuses the engine, no repeated guidance.
    std::fs::remove_file(bin.path().join("browser-args.txt")).unwrap();
    let out = p.run(&["open", &format!("http://127.0.0.1:{target}/"), "--no-watch"]).ok().stdout();
    assert!(out.contains("already running"), "{out}");
    assert!(!out.contains("plonix ca trust"), "{out}");
    read_when_ready(&bin.path().join("browser-args.txt"));

    // --json for scripts.
    let r = p.run(&["open", "localhost:1", "--no-browser", "--no-scope", "--json"]);
    let v: serde_json::Value = serde_json::from_str(&r.ok().stdout()).unwrap();
    assert_eq!(v["target"], "http://localhost:1/");
    assert_eq!(v["proxy"], proxy.as_str());
    assert_eq!(v["scope"], serde_json::Value::Null);

    assert_eq!(p.run(&["open", "ftp://x"]).code(), 1);
}

#[test]
fn ca_command_works_without_an_engine() {
    let p = Plonix::new();
    let out = p.run(&["ca"]).ok().stdout();
    assert!(out.contains("SHA-256") && out.contains("ca.pem"), "{out}");
    assert!(p.run(&["ca", "pem"]).ok().stdout().starts_with("-----BEGIN CERTIFICATE-----"));
}

// ---- detection rules and the store ------------------------------------------

type Handler = dyn Fn(&str) -> (u16, Vec<(String, String)>, Vec<u8>) + Send + Sync;

/// A tiny HTTP server that answers GETs from `handler(path)`.
fn serve(handler: std::sync::Arc<Handler>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in l.incoming().flatten() {
            let handler = handler.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                if reader.read_line(&mut first).unwrap_or(0) == 0 {
                    return;
                }
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_string();
                let path = path.split_once("://").map(|(_, rest)| rest.find('/').map(|i| &rest[i..]).unwrap_or("/").to_string()).unwrap_or(path);
                let (status, headers, body) = handler(&path);
                let mut resp = format!("HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n", body.len());
                for (k, v) in headers {
                    resp.push_str(&format!("{k}: {v}\r\n"));
                }
                resp.push_str("\r\n");
                let mut s = stream;
                let _ = s.write_all(resp.as_bytes());
                let _ = s.write_all(&body);
            });
        }
    });
    port
}

fn sha256(data: &[u8]) -> String {
    plonix_core::rulepack::sha256_hex(data)
}

const ACME_PACK: &str = r#"{
  "plonix_pack": 1, "name": "acme-internal", "version": "1.0.0",
  "description": "Acme's in-house gateway", "author": "acme red team",
  "rules": [
    {"id": "acme-gateway", "name": "Acme Gateway", "category": "load-balancer",
     "conditions": [{"header": "X-Acme-Gateway", "regex": "^v([\\d.]+)", "version": "$1"}]}
  ]
}"#;

#[test]
fn rules_check_add_list_remove() {
    let p = Plonix::new();
    let dir = tempfile::tempdir().unwrap();
    let pack = dir.path().join("acme.json");
    std::fs::write(&pack, ACME_PACK).unwrap();
    let pack = pack.to_str().unwrap();

    let out = p.run(&["rules", "check", pack]).ok().stdout();
    assert!(out.contains("acme-internal 1.0.0 is valid: 1 rules"), "{out}");
    assert!(out.contains(&sha256(ACME_PACK.as_bytes())), "{out}");

    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, ACME_PACK.replace("load-balancer", "rootkit").replace("\"$1\"", "\"$7\"")).unwrap();
    let r = p.run(&["rules", "check", bad.to_str().unwrap()]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("category: `rootkit`"), "{}", r.stderr());

    // Built-in packs are there before anything is installed.
    let out = p.run(&["rules"]).ok().stdout();
    assert!(out.contains("web-servers") && out.contains("built-in"), "{out}");

    let r = p.run(&["rules", "add", pack, "--sha256", &"0".repeat(64)]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("checksum mismatch"), "{}", r.stderr());
    assert!(!p.run(&["rules"]).ok().stdout().contains("acme-internal"));

    let out = p.run(&["rules", "add", pack]).ok().stdout();
    assert!(out.contains("Installed acme-internal 1.0.0: 1 rules by acme red team."), "{out}");
    let out = p.run(&["rules", "list"]).ok().stdout();
    assert!(out.contains("acme-internal") && out.contains("acme.json"), "{out}");

    assert!(p.run(&["rules", "add", "http://example.com/pack.json"]).stderr().contains("https"));
    assert_eq!(p.run(&["rules", "remove", "web-servers"]).code(), 1);
    assert!(p.run(&["rules", "remove", "acme-internal"]).ok().stdout().contains("Removed acme-internal."));
    assert!(!p.run(&["rules"]).ok().stdout().contains("acme-internal"));
}

#[test]
fn store_installs_only_verified_packs() {
    let p = Plonix::new();
    let pack_bytes = std::sync::Arc::new(std::sync::Mutex::new(ACME_PACK.as_bytes().to_vec()));
    let index = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let make_index = |version: &str, sha: &str| {
        format!(
            r#"{{"plonix_index":1,"name":"Test store","packages":[
                {{"name":"acme-internal","kind":"rules","version":"{version}","description":"Acme gateway rules","author":"acme","url":"packs/acme.json","sha256":"{sha}"}},
                {{"name":"web-servers","kind":"rules","version":"1.0.0","description":"Built in","author":"plonix","url":"packs/ws.json","sha256":"{}"}},
                {{"name":"jwt-workbench","kind":"extension","version":"0.1.0","description":"Needs the sandbox","author":"x","url":"ext/jwt.json","sha256":"{}"}}]}}"#,
            "1".repeat(64),
            "2".repeat(64)
        )
    };
    *index.lock().unwrap() = make_index("1.0.0", &sha256(ACME_PACK.as_bytes()));
    let (pb, ix) = (pack_bytes.clone(), index.clone());
    let port = serve(std::sync::Arc::new(move |path: &str| match path {
        "/store/index.json" => (200, vec![], ix.lock().unwrap().clone().into_bytes()),
        "/store/packs/acme.json" => (200, vec![], pb.lock().unwrap().clone()),
        _ => (404, vec![], vec![]),
    }));
    let url = format!("http://127.0.0.1:{port}/store/index.json");

    let out = p.run(&["store", "--index", &url]).ok().stdout();
    assert!(out.contains("Test store"), "{out}");
    for (name, status) in [("acme-internal", "available"), ("web-servers", "built-in"), ("jwt-workbench", "needs runtime")] {
        let line = out.lines().find(|l| l.starts_with(name)).unwrap_or_else(|| panic!("{name} missing:\n{out}"));
        assert!(line.contains(status), "{line}");
    }

    let r = p.run(&["store", "install", "jwt-workbench", "--index", &url]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("sandboxed extension runtime"), "{}", r.stderr());

    // A tampered download is refused and nothing is installed.
    *pack_bytes.lock().unwrap() = ACME_PACK.replace("Acme Gateway", "Evil Gateway").into_bytes();
    let r = p.run(&["store", "install", "acme-internal", "--index", &url]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("checksum mismatch"), "{}", r.stderr());
    assert!(!p.run(&["rules"]).ok().stdout().contains("acme-internal"));

    *pack_bytes.lock().unwrap() = ACME_PACK.as_bytes().to_vec();
    let out = p.run(&["store", "install", "acme-internal", "--index", &url]).ok().stdout();
    assert!(out.contains("Installed acme-internal 1.0.0 (1 rules, sha256 verified)."), "{out}");
    assert!(p.run(&["store", "update", "--index", &url]).ok().stdout().contains("up to date"));

    // A new version in the store shows up as an update.
    let v2 = ACME_PACK.replace("1.0.0", "1.1.0");
    *pack_bytes.lock().unwrap() = v2.clone().into_bytes();
    *index.lock().unwrap() = make_index("1.1.0", &sha256(v2.as_bytes()));
    let out = p.run(&["store", "list", "acme", "--index", &url]).ok().stdout();
    assert!(out.contains("update 1.0.0→1.1.0"), "{out}");
    let out = p.run(&["store", "update", "--index", &url]).ok().stdout();
    assert!(out.contains("Updated acme-internal 1.0.0 → 1.1.0"), "{out}");
}

#[test]
fn repository_store_index_installs_from_disk() {
    let p = Plonix::new();
    let index = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../store/index.json");
    let index = index.to_str().unwrap();
    let out = p.run(&["store", "--index", index]).ok().stdout();
    assert!(out.contains("admin-panels") && out.contains("available"), "{out}");
    let out = p.run(&["store", "install", "admin-panels", "--index", index]).ok().stdout();
    assert!(out.contains("Installed admin-panels"), "{out}");
}

#[test]
fn tech_detects_from_captured_traffic() {
    let p = Plonix::new();
    let target = serve(std::sync::Arc::new(|path: &str| {
        let mut h = vec![
            ("Server".to_string(), "nginx/1.25.3".to_string()),
            ("X-Powered-By".to_string(), "PHP/8.2.12".to_string()),
            ("X-Acme-Gateway".to_string(), "v3.4".to_string()),
            ("Content-Type".to_string(), "text/html".to_string()),
        ];
        if path == "/" {
            h.push(("Set-Cookie".to_string(), "wordpress_test_cookie=WP+Cookie+check; path=/".to_string()));
        }
        (200, h, br#"<html><head><meta name="generator" content="WordPress 6.4.2"></head></html>"#.to_vec())
    }));
    let proxy = p.start();
    via_proxy(&proxy, &format!("http://localhost:{target}/"), &[]);
    via_proxy(&proxy, &format!("http://localhost:{target}/wp-json/wp/v2/posts"), &[]);
    p.search_until("host:localhost", 2);

    let out = p.run(&["tech"]).ok().stdout();
    for want in ["web-server     nginx 1.25.3", "language       PHP 8.2.12", "cms            WordPress 6.4.2"] {
        assert!(out.contains(want), "missing `{want}`:\n{out}");
    }
    assert!(!out.contains("Acme Gateway"));

    // Rules installed later apply to traffic that was already captured.
    let dir = tempfile::tempdir().unwrap();
    let pack = dir.path().join("acme.json");
    std::fs::write(&pack, ACME_PACK).unwrap();
    p.run(&["rules", "add", pack.to_str().unwrap()]).ok();
    let out = p.run(&["tech", "localhost"]).ok().stdout();
    assert!(out.contains("Acme Gateway 3.4"), "{out}");

    let v: serde_json::Value = serde_json::from_str(&p.run(&["tech", "--json"]).ok().stdout()).unwrap();
    let tech = v[0]["tech"].as_array().unwrap();
    let one: serde_json::Value = serde_json::from_str(&p.run(&["tech", "--json", "localhost"]).ok().stdout()).unwrap();
    assert_eq!(one["host"], "localhost");
    let wp = tech.iter().find(|t| t["id"] == "wordpress").unwrap();
    assert_eq!(wp["version"], "6.4.2");
    assert!(wp["exchange_id"].as_i64().is_some());
}
