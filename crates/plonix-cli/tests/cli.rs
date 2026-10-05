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
    /// A fresh home. Tests accept the terms per run, the way scripts and CI
    /// do, so nothing is recorded and no usage statistics are kept.
    fn new() -> Self {
        Self { home: tempfile::tempdir().unwrap(), env: vec![("PLONIX_ACCEPT_TERMS".into(), "1".into())] }
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        // Never open a real browser window from tests.
        c.env("PLONIX_HOME", self.home.path()).env_remove("PLONIX_BROWSER").env("PLONIX_UI_BROWSER", "true").args(args);
        for (k, v) in &self.env {
            c.env(k, v);
        }
        c
    }

    fn run(&self, args: &[&str]) -> Run {
        let r = Run(self.cmd(args).stdin(Stdio::null()).output().unwrap());
        if r.code() != 0 {
            // Shown only when the test fails: what the engines logged.
            eprintln!("--- engine log after `plonix {}`:\n{}", args.join(" "), std::fs::read_to_string(self.home.path().join("logs/engine.log")).unwrap_or_default());
        }
        r
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
        let _ = self.cmd(&["stop", "--all"]).output();
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
fn the_terms_are_accepted_once_or_per_run() {
    let mut p = Plonix::new();
    p.env.clear();
    let mut cmd = p.cmd(&["projects"]);
    let r = Run(cmd.env_remove("PLONIX_ACCEPT_TERMS").stdin(Stdio::null()).output().unwrap());
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("--accept-terms"), "{}", r.stderr());
    p.run(&["projects", "--accept-terms"]).ok();
    p.env.push(("PLONIX_ACCEPT_TERMS".into(), "1".into()));
    p.run(&["projects"]).ok();
    assert!(!p.home.path().join("terms.json").exists(), "accepting for one run records nothing");
    let r = p.run(&["usage"]);
    r.ok();
    assert!(r.stdout().contains("statistics: off"), "{}", r.stdout());
    assert!(!p.home.path().join("usage.json").exists());
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

#[test]
fn findings_from_record_to_report() {
    let p = Plonix::new();
    let target = serve_target();
    let proxy = p.start();
    via_proxy(&proxy, &format!("http://localhost:{target}/echo?id=7"), &[]);
    let id = p.search_until("", 1)["items"][0]["id"].as_i64().unwrap().to_string();

    assert!(p.run(&["findings"]).ok().stdout().contains("No findings yet"));
    let out = p.run(&["findings", "add", "IDOR on /echo", "-s", "high", "-r", &id, "-d", "Change the id"]).ok().stdout();
    assert!(out.contains("Recorded finding #1: IDOR on /echo"), "{out}");
    p.run(&["findings", "add", "Verbose banner", "-s", "low"]).ok();

    let out = p.run(&["findings"]).ok().stdout();
    let lines: Vec<&str> = out.lines().collect();
    assert!(lines[1].contains("high") && lines[1].contains("IDOR on /echo") && lines[2].contains("Verbose banner"), "most severe first:\n{out}");

    let out = p.run(&["findings", "show", "1"]).ok().stdout();
    assert!(out.contains("#1 IDOR on /echo") && out.contains("Change the id") && out.contains("GET /echo?id=7 HTTP/1.1") && out.contains("HTTP/1.1 200"), "{out}");
    assert_eq!(p.run(&["findings", "show", "9"]).code(), 5);

    let out = p.run(&["findings", "edit", "1", "--title", "IDOR on /echo?id=", "-s", "critical"]).ok().stdout();
    assert!(out.contains("Updated finding #1: IDOR on /echo?id= (critical, open)"), "{out}");
    assert_eq!(p.run(&["findings", "edit", "1", "-s", "urgent"]).code(), 2);
    assert_eq!(p.run(&["findings", "edit", "1"]).code(), 1);
    assert!(p.run(&["findings", "status", "1", "confirmed"]).ok().stdout().contains("Finding #1 is now confirmed."));
    assert!(p.run(&["findings", "status", "2", "false-positive"]).ok().stdout().contains("now false positive"));
    assert_eq!(p.run(&["findings", "status", "1", "later"]).code(), 2);
    assert_eq!(p.run(&["findings", "status", "9", "fixed"]).code(), 5);
    let out = p.run(&["findings", "list", "--status", "confirmed"]).ok().stdout();
    assert!(out.contains("IDOR") && !out.contains("Verbose banner"), "{out}");

    // Reports: false positives stay out unless asked for.
    let dir = tempfile::tempdir().unwrap();
    let html = dir.path().join("report.html");
    p.run(&["findings", "export", "-o", html.to_str().unwrap()]).ok();
    let doc = std::fs::read_to_string(&html).unwrap();
    assert!(doc.starts_with("<!doctype html>") && doc.contains("IDOR on /echo?id=") && doc.contains("GET /echo?id=7") && !doc.contains("Verbose banner"), "{doc}");
    let md = p.run(&["findings", "export", "--status", "false-positive"]).ok().stdout();
    assert!(md.starts_with("# Findings: cli-test") && md.contains("Verbose banner") && !md.contains("IDOR"), "{md}");
    let v: serde_json::Value = serde_json::from_str(&p.run(&["--json", "findings", "export", "1"]).ok().stdout()).unwrap();
    assert_eq!(v["findings"][0]["evidence"][0]["method"], "GET");
    assert_eq!(p.run(&["findings", "export", "-f", "pdf"]).code(), 2);

    let out = p.run(&["findings", "rm", "2"]).ok().stdout();
    assert!(out.contains("Deleted finding #2."), "{out}");
    assert_eq!(p.run(&["findings", "rm", "2"]).code(), 5);
    let v: serde_json::Value = serde_json::from_str(&p.run(&["--json", "findings"]).ok().stdout()).unwrap();
    assert_eq!(v.as_array().unwrap().len(), 1);
}

fn fake_browser(dir: &Path) -> PathBuf {
    fake_program(dir, "fake-chrome", "browser-args.txt")
}

/// A stand-in for a program that records its arguments, one per line.
fn fake_program(dir: &Path, name: &str, log: &str) -> PathBuf {
    let script = dir.join(name);
    let log = dir.join(log);
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
fn bench_runs_payloads_through_marked_positions_and_stays_in_scope() {
    let p = Plonix::new();
    let target = serve_target();
    let proxy = p.start();

    // Capture one request so the host is known, then accept it.
    via_proxy(&proxy, &format!("http://localhost:{target}/echo?id=1"), &[]);
    p.search_until("", 1);

    // The built-in lists are listed.
    let out = p.run(&["bench", "lists"]).ok().stdout();
    assert!(out.contains("numbers-1-100") && out.contains("input-probes"), "{out}");

    let url = format!("http://localhost:{target}/echo?id=\u{2022}1\u{2022}");

    // A run against a host that is not accepted is refused, and sends nothing.
    let r = p.run(&["bench", "run", &url, "--list", "range:1-3", "--delay-ms", "0"]);
    assert_eq!(r.code(), 4, "stdout:\n{}\nstderr:\n{}", r.stdout(), r.stderr());
    assert!(r.stderr().to_lowercase().contains("scope"), "{}", r.stderr());

    p.run(&["scope", "accept", "localhost"]).ok();

    // A single-position sweep over a range, with a baseline.
    let out = p
        .run(&["bench", "run", &url, "--list", "range:1-3", "--base", "--delay-ms", "0"])
        .ok()
        .stdout();
    assert!(out.contains("1 position(s) in sweep mode"), "{out}");
    assert!(out.contains("4 request(s) sent"), "{out}"); // baseline + 3
    assert!(out.contains("(baseline)"), "{out}");

    // A budget stops the run short and says so.
    let out = p
        .run(&["bench", "run", &url, "--list", "builtin:numbers-1-100", "--max-requests", "5", "--delay-ms", "0"])
        .ok()
        .stdout();
    assert!(out.contains("5 request(s) sent"), "{out}");
    assert!(out.to_lowercase().contains("budget"), "{out}");

    // Every request the run sent is in Traffic as a Bench send.
    let v = p.search_until("source:replay", 4);
    assert!(v["items"].as_array().unwrap().len() >= 4, "runs are recorded: {v}");
}

#[test]
fn market_installs_a_list_pack_and_the_bench_can_use_it() {
    let p = Plonix::new();
    let proxy = p.start();
    let _ = proxy;

    // The built-in starter lists are there from the start.
    let out = p.run(&["bench", "lists"]).ok().stdout();
    assert!(out.contains("numbers-1-100"), "{out}");
    assert!(!out.contains("id-formats"), "extra lists are not present until installed:\n{out}");

    // Install the extra list pack from the repository's signed Market index.
    let index = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../store/index.json");
    let index = index.to_str().unwrap();
    let r = p.run(&["market", "--index", index, "install", "extra-wordlists"]);
    let out = r.ok().stdout();
    assert!(out.to_lowercase().contains("extra-wordlists"), "{out}");

    // Its lists now show up in the Bench and resolve in a run.
    let out = p.run(&["bench", "lists"]).ok().stdout();
    assert!(out.contains("id-formats"), "installed list should be offered:\n{out}");

    p.run(&["scope", "accept", "localhost"]).ok();
    let v = p.run(&["bench", "run", "http://localhost:1/x?id=\u{2022}1\u{2022}", "--list", "builtin:id-formats", "--max-requests", "3", "--delay-ms", "0", "--json"]);
    // The host does not answer, but the run still plans from the installed list
    // and reports requests attempted against the accepted host.
    let out = v.ok().stdout();
    assert!(out.contains("\"positions\": 1") || out.contains("\"positions\":1"), "{out}");

    // Removing it takes its lists away again.
    p.run(&["market", "remove", "extra-wordlists"]).ok();
    assert!(!p.run(&["bench", "lists"]).ok().stdout().contains("id-formats"));
}

#[test]
fn open_starts_everything_and_launches_the_browser_through_the_proxy() {
    let mut p = Plonix::new();
    let bin = tempfile::tempdir().unwrap();
    let browser = fake_browser(bin.path());
    p.env.push(("PLONIX_BROWSER".into(), browser.display().to_string()));
    let window = fake_program(bin.path(), "fake-default-browser", "ui-args.txt");
    p.env.push(("PLONIX_UI_BROWSER".into(), window.display().to_string()));
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
    // Each project has its own capture-browser profile, in its folder.
    let profile = std::fs::canonicalize(p.home.path().join("projects/127.0.0.1")).unwrap().join("browser");
    assert!(args.contains(&format!("--user-data-dir={}", profile.display()).as_str()), "{args:?}");
    assert!(args.iter().any(|a| a.starts_with("--ignore-certificate-errors-spki-list=") && a.len() > 40), "{args:?}");
    assert_eq!(args.last().unwrap(), &format!("http://127.0.0.1:{target}/app"));

    // The Plonix window opens in the default browser with a one-time link.
    assert!(out.contains("✓ Window"), "{out}");
    let link = read_when_ready(&bin.path().join("ui-args.txt")).trim().to_string();
    let api = api_base(&p);
    assert!(link.starts_with(&format!("{api}/#code=")), "{link}");
    let code = link.rsplit_once("#code=").unwrap().1;
    let token = std::fs::read_to_string(p.home.path().join("api-token")).unwrap();
    assert_eq!(redeem(&api, code).unwrap()["token"], token.trim());
    assert_eq!(redeem(&api, code).unwrap_err(), 401, "a link works only once");

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

fn api_base(p: &Plonix) -> String {
    let v: serde_json::Value = serde_json::from_str(&p.run(&["status", "--json"]).stdout()).unwrap();
    format!("http://{}", v["status"]["api"].as_str().unwrap())
}

/// Trades a UI launch code for the API token, as the page does.
fn redeem(api: &str, code: &str) -> Result<serde_json::Value, u16> {
    match ureq::post(&format!("{api}/ui/session")).send_json(serde_json::json!({ "code": code })) {
        Ok(r) => Ok(r.into_json().unwrap()),
        Err(ureq::Error::Status(s, _)) => Err(s),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn ui_serves_the_window_and_signs_in_with_a_one_time_link() {
    let p = Plonix::new();
    let r = p.run(&["ui", "--no-open", "--json", "--port", "0", "--api-port", "0"]);
    let v: serde_json::Value = serde_json::from_str(&r.ok().stdout()).unwrap();
    assert_eq!(v["engine_started"], true);
    assert_eq!(v["opened"], false);
    let api = api_base(&p);
    let link = v["url"].as_str().unwrap();
    assert!(link.starts_with(&format!("{api}/#code=")), "{link}");

    // The page and its assets load without a token, locked down by CSP.
    let page = ureq::get(&format!("{api}/")).call().unwrap();
    let csp = page.header("content-security-policy").unwrap().to_string();
    assert!(csp.contains("script-src 'self'") && csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(page.into_string().unwrap().contains("/ui/app.js"));
    for asset in ["/ui/app.js", "/ui/app.css", "/ui/icon.svg"] {
        assert_eq!(ureq::get(&format!("{api}{asset}")).call().unwrap().status(), 200, "{asset}");
    }
    // The API itself still needs the token.
    match ureq::get(&format!("{api}/api/status")).call() {
        Err(ureq::Error::Status(401, _)) => {}
        other => panic!("expected 401, got {other:?}"),
    }

    // Other origins cannot redeem a link; a wrong code gets nothing.
    let code = link.rsplit_once("#code=").unwrap().1;
    let foreign = ureq::post(&format!("{api}/ui/session")).set("Origin", "https://evil.example").send_json(serde_json::json!({ "code": code }));
    assert!(matches!(foreign, Err(ureq::Error::Status(403, _))), "{foreign:?}");
    assert_eq!(redeem(&api, "0000").unwrap_err(), 401);
    let token = redeem(&api, code).unwrap()["token"].as_str().unwrap().to_string();
    let status = ureq::get(&format!("{api}/api/status")).set("Authorization", &format!("Bearer {token}")).call().unwrap();
    assert_eq!(status.status(), 200);

    // Text mode prints the link when not opening it; a running engine is reused.
    let out = p.run(&["ui", "--no-open"]).ok().stdout();
    assert!(out.contains(&format!("{api}/#code=")), "{out}");
    assert!(!out.contains("engine started"), "{out}");
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

    let out = p.run(&["store", "--index", &url, "--allow-unsigned"]).ok().stdout();
    assert!(out.contains("Test store"), "{out}");
    for (name, status) in [("acme-internal", "available"), ("web-servers", "built-in"), ("jwt-workbench", "coming soon")] {
        let line = out.lines().find(|l| l.starts_with(name)).unwrap_or_else(|| panic!("{name} missing:\n{out}"));
        assert!(line.contains(status), "{line}");
    }

    let r = p.run(&["store", "install", "jwt-workbench", "--index", &url, "--allow-unsigned"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("not published"), "{}", r.stderr());

    // A tampered download is refused and nothing is installed.
    *pack_bytes.lock().unwrap() = ACME_PACK.replace("Acme Gateway", "Evil Gateway").into_bytes();
    let r = p.run(&["store", "install", "acme-internal", "--index", &url, "--allow-unsigned"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("checksum mismatch"), "{}", r.stderr());
    assert!(!p.run(&["rules"]).ok().stdout().contains("acme-internal"));

    *pack_bytes.lock().unwrap() = ACME_PACK.as_bytes().to_vec();
    let out = p.run(&["store", "install", "acme-internal", "--index", &url, "--allow-unsigned"]).ok().stdout();
    assert!(out.contains("Installed acme-internal 1.0.0 (rule pack, sha256 verified)."), "{out}");
    assert!(p.run(&["store", "update", "--index", &url, "--allow-unsigned"]).ok().stdout().contains("up to date"));

    // A new version in the store shows up as an update.
    let v2 = ACME_PACK.replace("1.0.0", "1.1.0");
    *pack_bytes.lock().unwrap() = v2.clone().into_bytes();
    *index.lock().unwrap() = make_index("1.1.0", &sha256(v2.as_bytes()));
    let out = p.run(&["store", "list", "acme", "--index", &url, "--allow-unsigned"]).ok().stdout();
    assert!(out.contains("update 1.0.0→1.1.0"), "{out}");
    let out = p.run(&["store", "update", "--index", &url, "--allow-unsigned"]).ok().stdout();
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

#[test]
fn two_projects_run_side_by_side() {
    let p = Plonix::new();
    let target = serve_target();
    let made = p.run(&["projects", "new", "Acme"]).ok().stdout();
    assert!(made.contains("Acme"), "{made}");
    for name in ["Acme", "Shop"] {
        let r = p.run(&["-p", name, "start", "--port", "0", "--api-port", "0"]);
        assert!(r.ok().stdout().contains("Plonix engine started."), "{}", r.stdout());
    }
    let proxy_of = |name: &str| -> String {
        let v: serde_json::Value = serde_json::from_str(&p.run(&["-p", name, "status", "--json"]).ok().stdout()).unwrap();
        v["status"]["proxy"].as_str().unwrap().to_string()
    };
    let (acme, shop) = (proxy_of("Acme"), proxy_of("Shop"));
    assert_ne!(acme, shop);

    let sessions = p.run(&["sessions"]).ok().stdout();
    assert!(sessions.contains("Acme") && sessions.contains("Shop") && sessions.contains(&acme) && sessions.contains(&shop), "{sessions}");

    // Traffic through one project's proxy stays in that project.
    via_proxy(&acme, &format!("http://localhost:{target}/only-acme"), &[]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !p.run(&["-p", "Acme", "search", "path:/only-acme"]).ok().stdout().contains("/only-acme") {
        assert!(Instant::now() < deadline, "Acme never recorded its request");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(p.run(&["-p", "Shop", "search"]).ok().stdout().contains("Nothing captured yet"));

    let listed = p.run(&["projects"]).ok().stdout();
    assert!(listed.contains("Acme") && listed.contains("Shop"), "{listed}");

    p.run(&["-p", "Shop", "stop"]).ok();
    let sessions = p.run(&["sessions"]).ok().stdout();
    assert!(sessions.contains("Acme") && !sessions.contains("Shop"), "{sessions}");
    p.run(&["stop", "--all"]).ok();
    assert!(!p.run(&["sessions"]).stdout().contains("Acme"));
}

/// A client session with `plonix mcp` over stdio, one JSON-RPC message per line.
struct Mcp {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    next_id: i64,
}

impl Mcp {
    fn start(p: &Plonix) -> Self {
        let mut child = p.cmd(&["mcp"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        Self { child, out, next_id: 0 }
    }

    fn send(&mut self, msg: serde_json::Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        self.send(serde_json::json!({ "jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params }));
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad line {line:?}: {e}"));
        assert_eq!(v["id"], self.next_id);
        v
    }

    fn tool(&mut self, name: &str, args: serde_json::Value) -> (bool, String) {
        let v = self.request("tools/call", serde_json::json!({ "name": name, "arguments": args }));
        let r = &v["result"];
        (r["isError"] == true, r["content"][0]["text"].as_str().unwrap_or("").to_string())
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_server_gives_agents_read_only_access() {
    let p = Plonix::new();
    let target = serve_target();
    let proxy = p.start();
    via_proxy(&proxy, &format!("http://localhost:{target}/"), &[]);
    via_proxy(&proxy, &format!("http://localhost:{target}/echo?user=7"), &[("Authorization", "Bearer secret-token")]);
    p.search_until("", 2);
    p.run(&["scope", "accept", "localhost"]).ok();

    let mut m = Mcp::start(&p);
    let init = m.request("initialize", serde_json::json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "test-agent", "version": "1" } }));
    assert_eq!(init["result"]["serverInfo"]["name"], "plonix");
    assert!(init["result"]["instructions"].as_str().unwrap().contains("read-only"));
    m.send(serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    let tools = m.request("tools/list", serde_json::json!({}));
    let names: Vec<&str> = tools["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"search_traffic") && names.contains(&"get_request") && names.contains(&"get_scope"), "{names:?}");

    let (err, text) = m.tool("search_traffic", serde_json::json!({ "query": "path:/echo" }));
    assert!(!err, "{text}");
    assert!(text.contains("/echo?user=7") && text.contains("1 of 1 match(es)"), "{text}");
    let id: i64 = text.lines().nth(1).unwrap().split_whitespace().next().unwrap().parse().unwrap();

    let (err, text) = m.tool("get_request", serde_json::json!({ "id": id }));
    assert!(!err && text.contains("GET /echo?user=7 HTTP/1.1") && text.contains("HTTP/1.1 200"), "{text}");
    let (err, text) = m.tool("get_scope", serde_json::json!({}));
    assert!(!err && text.contains("\"localhost\""), "{text}");
    let (err, text) = m.tool("list_hosts", serde_json::json!({}));
    assert!(!err && text.contains("localhost"), "{text}");
    let (err, text) = m.tool("get_request", serde_json::json!({ "id": 999 }));
    assert!(err && text.contains("not found"), "{text}");

    // Findings the user recorded can be read as a report, with their evidence.
    p.run(&["findings", "add", "Echo leaks the user id", "-s", "high", "-r", &id.to_string()]).ok();
    let (err, text) = m.tool("findings_report", serde_json::json!({}));
    assert!(!err && text.contains("## #1 Echo leaks the user id") && text.contains("GET /echo?user=7 HTTP/1.1"), "{text}");

    // There is no tool that changes anything, and the engine refuses the agent token for it anyway.
    let v = m.request("tools/call", serde_json::json!({ "name": "replay", "arguments": { "id": id } }));
    assert_eq!(v["error"]["code"], -32602);
    let token = std::fs::read_to_string(p.home.path().join("agent-token")).unwrap();
    let err = ureq::post(&format!("{}/api/replay", api_base(&p)))
        .set("Authorization", &format!("Bearer {}", token.trim()))
        .send_json(serde_json::json!({ "id": id }))
        .unwrap_err();
    assert!(matches!(err, ureq::Error::Status(403, _)), "{err}");

    // The window's Agents screen sees the connection.
    let user_token = std::fs::read_to_string(p.home.path().join("api-token")).unwrap();
    let agents: serde_json::Value = ureq::get(&format!("{}/api/agents", api_base(&p)))
        .set("Authorization", &format!("Bearer {}", user_token.trim()))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    let c = agents["clients"].as_array().unwrap().iter().find(|c| c["name"] == "test-agent").expect("test-agent connected");
    assert!(c["requests"].as_u64().unwrap() >= 5, "{c}");
}

#[test]
fn connect_claude_adds_the_mcp_server() {
    let mut p = Plonix::new();
    let dir = tempfile::tempdir().unwrap();
    let claude = fake_program(dir.path(), "claude", "claude-args.txt");
    p.env.push(("PLONIX_CLAUDE".into(), claude.display().to_string()));

    let out = p.run(&["connect", "claude"]).ok().stdout();
    assert!(out.contains("added MCP server \"plonix\" for all your projects"), "{out}");
    assert!(out.contains("search_traffic") && out.contains("Send or replay requests"), "{out}");
    assert!(p.home.path().join("agent-token").exists());
    // The fake records its last call: the add.
    let args = read_when_ready(&dir.path().join("claude-args.txt"));
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(&args[..5], ["mcp", "add-json", "--scope", "user", "plonix"]);
    let cfg: serde_json::Value = serde_json::from_str(args[5]).unwrap();
    assert_eq!(cfg["args"], serde_json::json!(["mcp"]));
    assert!(cfg["command"].as_str().unwrap().ends_with("plonix"));
    assert_eq!(cfg["env"]["PLONIX_HOME"], p.home.path().display().to_string());

    // --print changes nothing and shows the config to paste.
    std::fs::remove_file(dir.path().join("claude-args.txt")).unwrap();
    let out = p.run(&["connect", "claude", "--print"]).ok().stdout();
    assert!(out.contains("claude mcp add --scope user plonix --") && out.contains("\"mcpServers\""), "{out}");
    assert!(!dir.path().join("claude-args.txt").exists());
}

const ACME_FILTERS: &str = r#"{
  "plonix_filters": 1, "name": "acme-filters", "version": "1.0.0",
  "description": "Acme console traffic", "author": "acme red team",
  "filters": [{"id": "acme-console", "label": "Acme console", "query": "path:/console method:GET"}]
}"#;

#[test]
fn filter_packs_add_named_filters_to_search() {
    let p = Plonix::new();
    let dir = tempfile::tempdir().unwrap();
    let pack = dir.path().join("acme-filters.json");
    std::fs::write(&pack, ACME_FILTERS).unwrap();
    let pack = pack.to_str().unwrap();

    let out = p.run(&["filters"]).ok().stdout();
    assert!(out.contains("is:graphql") && out.contains("is:trackers"), "{out}");
    assert!(p.run(&["filters", "check", pack]).ok().stdout().contains("acme-filters 1.0.0 is valid: 1 filters."));
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, ACME_FILTERS.replace("path:/console method:GET", "is:graphql")).unwrap();
    let r = p.run(&["filters", "check", bad.to_str().unwrap()]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("unknown filter is:graphql"), "{}", r.stderr());

    let target = serve(std::sync::Arc::new(|path: &str| {
        let ctype = if path.starts_with("/graphql") { "application/json" } else { "text/html" };
        (200, vec![("Content-Type".to_string(), ctype.to_string())], b"{}".to_vec())
    }));
    p.start();
    let proxy = p.proxy();
    for path in ["/graphql", "/console", "/home"] {
        via_proxy(&proxy, &format!("http://localhost:{target}{path}"), &[]);
    }
    p.search_until("host:localhost", 3);

    let paths = |q: &str| -> Vec<String> {
        let v: serde_json::Value = serde_json::from_str(&p.run(&["search", "--json", q]).ok().stdout()).unwrap();
        v["items"].as_array().unwrap().iter().map(|i| i["path"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(paths("is:graphql"), ["/graphql"]);
    assert_eq!(paths("is:api"), ["/graphql"]);
    let mut hidden = paths("-is:graphql host:localhost");
    hidden.sort();
    assert_eq!(hidden, ["/console", "/home"]);
    let r = p.run(&["search", "is:acme-console"]);
    assert_eq!(r.code(), 2);
    assert!(r.stderr().contains("unknown filter is:acme-console"), "{}", r.stderr());

    // Installed while the engine runs: picked up without a restart.
    assert!(p.run(&["filters", "add", pack]).ok().stdout().contains("Installed acme-filters 1.0.0: 1 filters"));
    assert_eq!(paths("is:acme-console"), ["/console"]);
    assert_eq!(paths("is:acme-console,graphql").len(), 2);

    assert_eq!(p.run(&["filters", "remove", "common"]).code(), 1);
    p.run(&["filters", "remove", "acme-filters"]).ok();
    assert_eq!(p.run(&["search", "is:acme-console"]).code(), 2);
}

#[test]
fn store_installs_filter_packs() {
    let p = Plonix::new();
    let index = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../store/index.json");
    let index = index.to_str().unwrap();
    let out = p.run(&["store", "--index", index]).ok().stdout();
    let line = |name: &str| out.lines().find(|l| l.starts_with(name)).unwrap_or_else(|| panic!("{name} missing:\n{out}")).to_string();
    assert!(line("common").contains("filter") && line("common").contains("built-in"));
    assert!(line("leaks").contains("available"));
    let out = p.run(&["store", "install", "leaks", "--index", index]).ok().stdout();
    assert!(out.contains("Installed leaks 1.0.0 (filter pack, sha256 verified)."), "{out}");
    assert!(p.run(&["filters"]).ok().stdout().contains("is:aws-keys"));
}

// ---- the Market and skills ----------------------------------------------------

/// Copies the repository's Market folder so a test can change it.
fn copy_market(dir: &Path) -> PathBuf {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../store");
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap() {
            let e = e.unwrap();
            if e.file_type().unwrap().is_dir() {
                copy(&e.path(), &to.join(e.file_name()));
            } else {
                std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
            }
        }
    }
    copy(&src, &dir.join("store"));
    dir.join("store/index.json")
}

#[test]
fn market_installs_bundles_and_skills_from_a_signed_index() {
    let p = Plonix::new();
    let index = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../store/index.json");
    let index = index.to_str().unwrap();
    let out = p.run(&["market", "--index", index]).ok().stdout();
    assert!(out.contains("signed by Plonix maintainers"), "{out}");
    let line = |out: &str, name: &str| out.lines().find(|l| l.starts_with(name)).unwrap_or_else(|| panic!("{name} missing:\n{out}")).to_string();
    assert!(line(&out, "triage-host").contains("built-in"));
    assert!(line(&out, "api-kit").contains("bundle") && line(&out, "api-kit").contains("available"));
    assert!(line(&out, "graphql-explorer").contains("coming soon"));
    let skills_only = p.run(&["market", "--index", index, "--kind", "skill"]).ok().stdout();
    assert!(!skills_only.contains("web-servers") && skills_only.contains("api-inventory"), "{skills_only}");

    let out = p.run(&["market", "show", "api-kit", "--index", index]).ok().stdout();
    assert!(out.contains("Includes: api-inventory, leaks, admin-panels"), "{out}");
    let out = p.run(&["market", "install", "api-kit", "--index", index]).ok().stdout();
    // Everything from the signed Market is marked verified; a skill added by hand is not.
    let listing = p.run(&["market", "--index", index]).ok().stdout();
    assert!(line(&listing, "api-inventory").contains("✓ verified"), "{listing}");
    assert!(line(&listing, "triage-host").contains("built-in"), "{listing}");
    let mine = p.home.path().join("mine.md");
    std::fs::write(&mine, "---\nplonix_skill: 1\nname: mine\nversion: 1.0.0\ntitle: Mine\ndescription: My skill.\nauthor: me\nuses: [traffic]\n---\nLook at traffic.\n").unwrap();
    p.run(&["skills", "add", mine.to_str().unwrap()]).ok();
    let listing = p.run(&["market", "--index", index]).ok().stdout();
    assert!(line(&listing, "mine").contains("NOT VERIFIED") && line(&listing, "mine").contains("skill"), "{listing}");
    assert!(listing.contains("1 not verified"), "{listing}");
    assert!(p.run(&["market", "show", "mine", "--index", index]).ok().stdout().contains("You added this yourself"));
    assert!(line(&p.run(&["skills"]).ok().stdout(), "mine").contains("NOT VERIFIED"));
    p.run(&["skills", "remove", "mine"]).ok();
    for name in ["api-inventory", "leaks", "admin-panels", "api-kit"] {
        assert!(out.contains(&format!("Installed {name} 1.0.0")), "{out}");
    }
    assert!(p.run(&["skills"]).ok().stdout().contains("api-inventory"));
    assert!(p.run(&["filters"]).ok().stdout().contains("is:aws-keys"));
    let out = p.run(&["skills", "show", "api-inventory", "--arg", "host=api.example.test"]).ok().stdout();
    assert!(out.contains("Build an inventory of the API on api.example.test") && out.contains("read-only"), "{out}");

    let out = p.run(&["market", "remove", "api-kit"]).ok().stdout();
    assert!(out.contains("Removed api-inventory (skill).") && out.contains("Removed api-kit (bundle)."), "{out}");
    assert!(!p.run(&["skills"]).ok().stdout().contains("api-inventory"));
    let r = p.run(&["market", "remove", "triage-host"]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("built-in skill"), "{}", r.stderr());
}

#[test]
fn market_refuses_unsigned_and_tampered_indexes() {
    let p = Plonix::new();
    let dir = tempfile::tempdir().unwrap();
    let index_path = copy_market(dir.path());
    let index = index_path.to_str().unwrap();
    assert!(p.run(&["market", "--index", index]).ok().stdout().contains("signed by Plonix maintainers"));

    // Changing a single character after signing is caught.
    let text = std::fs::read_to_string(&index_path).unwrap();
    std::fs::write(&index_path, text.replace("Plonix contributors", "Plonix contributers")).unwrap();
    let r = p.run(&["market", "install", "api-inventory", "--index", index]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("changed after it was signed"), "{}", r.stderr());

    // Without a signature it is refused unless the author asks.
    std::fs::remove_file(dir.path().join("store/index.json.sig")).unwrap();
    let r = p.run(&["market", "--index", index]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("not signed"), "{}", r.stderr());

    // A Market author signs with their own key; users who trust it can install.
    let key = dir.path().join("author.key");
    let out = p.run(&["market", "keygen", key.to_str().unwrap()]).ok().stdout();
    let public = out.lines().find_map(|l| l.strip_prefix("Public key: ")).unwrap().to_string();
    p.run(&["market", "sign", index, "--key", key.to_str().unwrap()]).ok();
    let r = p.run(&["market", "--index", index]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("not a key you trust"), "{}", r.stderr());
    p.run(&["market", "trust", &public]).ok();
    let out = p.run(&["market", "install", "api-inventory", "--index", index]).ok().stdout();
    assert!(out.contains("Installed api-inventory 1.0.0 (skill, sha256 verified)."), "{out}");

    // A swapped package file is refused even from a trusted index.
    std::fs::write(dir.path().join("store/skills/check-scope.md"), "tampered").unwrap();
    p.run(&["market", "remove", "api-inventory"]).ok();
    let skill = std::fs::read_to_string(dir.path().join("store/skills/api-inventory.md")).unwrap();
    std::fs::write(dir.path().join("store/skills/api-inventory.md"), skill.replace("Inventory an API", "Something else")).unwrap();
    let r = p.run(&["market", "install", "api-inventory", "--index", index]);
    assert_eq!(r.code(), 1);
    assert!(r.stderr().contains("checksum mismatch"), "{}", r.stderr());
    assert!(!p.run(&["skills"]).ok().stdout().contains("api-inventory"));
}

#[test]
fn agents_get_skills_as_mcp_prompts_within_their_settings() {
    let p = Plonix::new();
    p.start();
    let mut m = Mcp::start(&p);
    m.request("initialize", serde_json::json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "skill-agent" } }));
    let prompts = m.request("prompts/list", serde_json::json!({}));
    let names: Vec<&str> = prompts["result"]["prompts"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"triage-host") && names.contains(&"check-scope"), "{names:?}");

    let got = m.request("prompts/get", serde_json::json!({ "name": "triage-host", "arguments": { "host": "shop.example.test" } }));
    let text = got["result"]["messages"][0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("Build a short briefing on shop.example.test"), "{text}");
    let missing = m.request("prompts/get", serde_json::json!({ "name": "triage-host", "arguments": {} }));
    assert!(missing["error"]["message"].as_str().unwrap().contains("host"), "{missing}");

    let (err, text) = m.tool("list_skills", serde_json::json!({}));
    assert!(!err && text.contains("explain-request(id)"), "{text}");
    let (err, text) = m.tool("get_skill", serde_json::json!({ "name": "explain-request", "arguments": { "id": "7" } }));
    assert!(!err && text.contains("Explain request #7"), "{text}");

    // Switching off a capability a skill uses takes the skill away from agents.
    let user_token = std::fs::read_to_string(p.home.path().join("api-token")).unwrap();
    let mut settings: serde_json::Value = ureq::get(&format!("{}/api/agents/settings", api_base(&p)))
        .set("Authorization", &format!("Bearer {}", user_token.trim()))
        .call()
        .unwrap()
        .into_json()
        .unwrap();
    settings["off"] = serde_json::json!(["scope"]);
    ureq::put(&format!("{}/api/agents/settings", api_base(&p)))
        .set("Authorization", &format!("Bearer {}", user_token.trim()))
        .send_json(settings)
        .unwrap();
    let prompts = m.request("prompts/list", serde_json::json!({}));
    let names: Vec<&str> = prompts["result"]["prompts"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert!(!names.contains(&"triage-host") && !names.contains(&"check-scope") && names.contains(&"explain-request"), "{names:?}");
    let (err, text) = m.tool("get_skill", serde_json::json!({ "name": "check-scope" }));
    assert!(err && text.contains("switched off"), "{text}");

    // Agents cannot install anything from the Market.
    let token = std::fs::read_to_string(p.home.path().join("agent-token")).unwrap();
    let err = ureq::post(&format!("{}/api/market/install", api_base(&p)))
        .set("Authorization", &format!("Bearer {}", token.trim()))
        .send_json(serde_json::json!({ "name": "api-kit" }))
        .unwrap_err();
    assert!(matches!(err, ureq::Error::Status(403, _)), "{err}");
}

#[test]
fn external_files_can_be_added_but_stay_unverified() {
    let p = Plonix::new();
    let dir = tempfile::tempdir().unwrap();
    let skill = dir.path().join("notes.md");
    std::fs::write(&skill, "---\nplonix_skill: 1\nname: acme-notes\nversion: 1.0.0\ntitle: Notes\ndescription: My notes.\nauthor: me\nuses: [traffic]\n---\nLook at traffic.\n").unwrap();
    let path = skill.to_str().unwrap();

    // Without --yes it only shows what it would do.
    let r = p.run(&["market", "add", path]);
    assert_eq!(r.code(), 1);
    assert!(r.stdout().contains("NOT VERIFIED") && r.stdout().contains("agents stay read-only") || r.stdout().contains("Agents stay read-only"), "{}", r.stdout());
    assert!(!p.run(&["skills"]).ok().stdout().contains("acme-notes"));

    let out = p.run(&["market", "add", path, "--yes"]).ok().stdout();
    assert!(out.contains("Installed acme-notes"), "{out}");
    assert!(p.run(&["skills"]).ok().stdout().lines().find(|l| l.starts_with("acme-notes")).unwrap().contains("NOT VERIFIED"));

    // Packs are detected by their contents; anything else is refused with a reason.
    let pack = dir.path().join("pack.json");
    std::fs::write(&pack, ACME_PACK).unwrap();
    assert!(p.run(&["market", "add", pack.to_str().unwrap(), "--yes"]).ok().stdout().contains("rule pack"));
    let junk = dir.path().join("junk.json");
    std::fs::write(&junk, "{\"hello\": 1}").unwrap();
    assert!(p.run(&["market", "add", junk.to_str().unwrap(), "--yes"]).stderr().contains("not a Plonix skill or pack"));
    let index = dir.path().join("index.json");
    std::fs::write(&index, "{\"plonix_index\": 2, \"packages\": []}").unwrap();
    assert!(p.run(&["market", "add", index.to_str().unwrap(), "--yes"]).stderr().contains("Market list"));
    assert_eq!(p.run(&["market", "add", "http://example.com/x.md", "--yes"]).code(), 1);

    // The agent prompt for an unverified skill says so.
    p.start();
    let mut m = Mcp::start(&p);
    m.request("initialize", serde_json::json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "a" } }));
    let (err, text) = m.tool("get_skill", serde_json::json!({ "name": "acme-notes" }));
    assert!(!err && text.contains("this skill is not verified"), "{text}");
    let (_, text) = m.tool("get_skill", serde_json::json!({ "name": "explain-request", "arguments": { "id": "1" } }));
    assert!(!text.contains("not verified"), "{text}");

    // Agents cannot add anything.
    let token = std::fs::read_to_string(p.home.path().join("agent-token")).unwrap();
    let err = ureq::post(&format!("{}/api/market/add", api_base(&p)))
        .set("Authorization", &format!("Bearer {}", token.trim()))
        .send_json(serde_json::json!({ "source": path, "confirm": true }))
        .unwrap_err();
    assert!(matches!(err, ureq::Error::Status(403, _)), "{err}");
}

#[test]
fn replace_rules_change_traffic_through_the_proxy() {
    let p = Plonix::new();
    let target = serve_target();
    let proxy = p.start();
    assert!(p.run(&["replace"]).ok().stdout().contains("No match-and-replace rules"));

    p.run(&["replace", "add", "request-header", "(?i)^user-agent: .*$", "User-Agent: plonix-test", "--regex", "--note", "agent"]).ok();
    p.run(&["replace", "add", "response-body", "welcome", "hello"]).ok();
    let r = p.run(&["replace", "add", "request-body", "(", "--regex"]);
    assert_ne!(r.code(), 0);
    assert!(r.stderr().contains("regular expression"), "{}", r.stderr());
    let out = p.run(&["replace", "list"]).ok().stdout();
    assert!(out.contains("request-header") && out.contains("[regex]") && out.contains("# agent") && out.contains("response-body"), "{out}");

    let echo = format!("http://localhost:{target}/echo");
    assert!(via_proxy(&proxy, &echo, &[]).to_ascii_lowercase().contains("user-agent: plonix-test"));
    assert!(via_proxy(&proxy, &format!("http://localhost:{target}/"), &[]).contains("hello to the target"));
    let v: serde_json::Value = serde_json::from_str(&p.run(&["replace", "--json"]).ok().stdout()).unwrap();
    let first = v["rules"][0]["id"].as_i64().unwrap().to_string();

    assert!(p.run(&["replace", "disable", &first]).ok().stdout().contains("is off"));
    assert!(p.run(&["replace"]).ok().stdout().contains("[off, regex]"));
    assert!(!via_proxy(&proxy, &echo, &[]).contains("plonix-test"));
    p.run(&["replace", "rm", &first]).ok();
    assert_eq!(p.run(&["replace", "rm", &first]).code(), 5, "not found");
}

#[test]
fn extensions_install_switch_off_and_remove() {
    let p = Plonix::new();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/extensions/security-headers");
    let dir = dir.to_str().unwrap();
    let r = p.run(&["extensions", "add", dir]);
    assert_eq!(r.code(), 1, "asks before installing");
    assert!(r.stdout().contains("propose findings") && r.stdout().contains("sandbox"), "{}", r.stdout());
    p.run(&["extensions", "add", dir, "--yes"]).ok();
    let out = p.run(&["extensions"]).ok().stdout();
    assert!(out.lines().any(|l| l.starts_with("security-headers") && l.contains(" on ")), "{out}");
    p.run(&["extensions", "disable", "security-headers"]).ok();
    let out = p.run(&["ext", "list"]).ok().stdout();
    assert!(out.lines().any(|l| l.starts_with("security-headers") && l.contains(" off ")), "{out}");
    let packed = p.run(&["extensions", "check", dir]).ok().stdout();
    assert!(packed.contains("is valid"), "{packed}");
    p.run(&["extensions", "remove", "security-headers"]).ok();
    assert!(p.run(&["extensions"]).ok().stdout().contains("No extensions installed"));
}
