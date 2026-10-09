//! Extensions that run a scanner program the user installed themselves.
//!
//! Plonix knows how to drive each program on [`PROGRAMS`] and nothing else:
//! a manifest names one by id, never a path or a command line. The program
//! gets copies of requests and responses written to a private temporary
//! folder, which is deleted when it finishes, and its output is read back
//! as [`Hit`]s against the exchanges they came from. For a scan, the flags
//! Plonix passes keep it local: it checks nothing with outside services and
//! does not update itself. An enumeration program instead gets only an
//! accepted scope domain and asks public sources about it.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codec;
use crate::detect::clean;
use crate::insight::Side;
use crate::model::Exchange;

/// What driving a program does, so the engine knows how to run it and the
/// manifest parser knows which capabilities it must ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Reads copies of captured traffic and reports what it found in each
    /// exchange (e.g. secrets). Passive; sends nothing.
    Scan,
    /// Takes an in-scope domain and enumerates its subdomains, which Plonix
    /// records as scope suggestions to review. Reads public sources; does not
    /// touch the target.
    Enumerate,
    /// Probes an in-scope endpoint with a bounded set of candidate inputs.
    /// Built in: Plonix sends every request itself through the scope choke
    /// point ([`crate::engine::Engine::send`]); no external process sends.
    Probe,
}

/// A program Plonix can drive: a tool the user installs, or a built-in
/// operation Plonix performs itself.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Program {
    /// The id a manifest uses. For an installed tool it is also the
    /// executable's name.
    pub id: &'static str,
    pub kind: Kind,
    /// How to install it on a Mac, or "" for a built-in.
    pub install: &'static str,
    pub homepage: &'static str,
    /// True when Plonix performs this itself, with no external program.
    pub builtin: bool,
}

pub const PROGRAMS: &[Program] = &[
    Program {
        id: "trufflehog",
        kind: Kind::Scan,
        install: "brew install trufflehog",
        homepage: "https://github.com/trufflesecurity/trufflehog",
        builtin: false,
    },
    Program {
        id: "subfinder",
        kind: Kind::Enumerate,
        install: "brew install subfinder",
        homepage: "https://github.com/projectdiscovery/subfinder",
        builtin: false,
    },
    Program { id: "param-probe", kind: Kind::Probe, install: "", homepage: "", builtin: true },
];

/// The installed tool enumeration tries first (by program id), then these,
/// so the extension still works when only another recon tool is present.
const ENUMERATE_FALLBACKS: &[&str] = &["bbot"];

pub fn get(id: &str) -> Option<&'static Program> {
    PROGRAMS.iter().find(|p| p.id == id)
}

/// Longest a program may run over one batch.
pub const TIMEOUT: Duration = Duration::from_secs(180);
/// Exchanges handed to the program in one run.
pub const BATCH: usize = 200;
/// Hits kept per exchange.
const MAX_HITS: usize = 40;
/// Longest value a hit carries.
const MAX_VALUE: usize = 4096;

/// One secret the program found in an exchange.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub side: Side,
    /// Where in the exchange, e.g. `header Authorization`, `response body`.
    pub location: String,
    /// What kind of secret, as the program names it, e.g. `AWS`.
    pub detector: String,
    /// The text as it appears in the request or response.
    pub value: String,
    /// Anything else worth knowing the program said about it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// The folders an executable might be in. Apps started from the Dock do not
/// get the shell's `PATH`, so the usual install folders are tried too.
fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/home/linuxbrew/.linuxbrew/bin"].map(PathBuf::from));
    if let Some(home) = crate::paths::user_home() {
        dirs.extend([home.join("go/bin"), home.join(".local/bin"), home.join("bin")]);
    }
    dirs
}

/// Where an executable named `name` is installed, if it is.
pub fn locate_exe(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    search_dirs().into_iter().map(|d| d.join(&file)).find(|f| f.is_file())
}

/// Where the program is installed, if it is. A built-in has no executable.
pub fn locate(id: &str) -> Option<PathBuf> {
    let p = get(id)?;
    if p.builtin {
        return None;
    }
    locate_exe(p.id)
}

/// Whether this program is available to run: built-ins always are; an
/// enumeration program is available if its tool or any fallback is installed.
pub fn available(id: &str) -> bool {
    match get(id) {
        Some(p) if p.builtin => true,
        Some(p) if p.kind == Kind::Enumerate => locate(id).is_some() || ENUMERATE_FALLBACKS.iter().any(|f| locate_exe(f).is_some()),
        _ => locate(id).is_some(),
    }
}

/// What a file the program reads holds: one side of one exchange.
struct Written {
    id: i64,
    side: Side,
    /// The location of each line, from line 1.
    lines: Vec<String>,
}

/// One side of an exchange as text: the request or status line, headers,
/// then the body, one header per line.
fn render(ex: &Exchange, side: Side) -> (String, Vec<String>) {
    let one = |s: &str| s.replace(['\r', '\n'], " ");
    let (first, first_loc, headers, body, body_loc) = match side {
        Side::Request => {
            let target = if ex.query.is_empty() { ex.path.clone() } else { format!("{}?{}", ex.path, ex.query) };
            (format!("{} {}", ex.method, one(&target)), "URL", &ex.req_headers, codec::body_text(&ex.req_headers, &ex.req_body), "request body")
        }
        Side::Response => (format!("{}", ex.status.unwrap_or(0)), "status line", &ex.resp_headers, codec::body_text(&ex.resp_headers, &ex.resp_body), "response body"),
    };
    let mut text = first + "\n";
    let mut lines = vec![first_loc.to_string()];
    for (name, value) in headers {
        text.push_str(&format!("{}: {}\n", one(name), one(value)));
        lines.push(format!("header {name}"));
    }
    text.push('\n');
    lines.push(body_loc.to_string());
    if let Some(b) = body {
        text.push_str(&b);
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    (text, lines)
}

/// Runs the program over these exchanges and returns what it found in
/// each. Every exchange given is in the result, with no hits when it found
/// nothing, so callers can tell scanned from not yet scanned.
pub fn scan(id: &str, exchanges: &[&Exchange]) -> Result<HashMap<i64, Vec<Hit>>, String> {
    let program = get(id).ok_or_else(|| format!("Plonix does not know how to run `{}`", clean(id, 40)))?;
    let exe = locate(id).ok_or_else(|| format!("{} is not installed. Install it with `{}`, then run this again.", program.id, program.install))?;
    let mut out: HashMap<i64, Vec<Hit>> = exchanges.iter().map(|ex| (ex.id, vec![])).collect();
    if exchanges.is_empty() {
        return Ok(out);
    }
    let dir = TempDir::new()?;
    let mut files = HashMap::new();
    for ex in exchanges {
        for side in [Side::Request, Side::Response] {
            let (text, lines) = render(ex, side);
            let name = format!("{}-{}.txt", ex.id, if side == Side::Request { "request" } else { "response" });
            crate::paths::write_private(&dir.0.join(&name), text.as_bytes()).map_err(|e| format!("writing a copy for {}: {e}", program.id))?;
            files.insert(name, Written { id: ex.id, side, lines });
        }
    }
    let stdout = run(&exe, &trufflehog_args(&dir.0), TIMEOUT)?;
    for (name, hit) in parse_trufflehog(&stdout, &files) {
        let Some(w) = files.get(&name) else { continue };
        let list = out.entry(w.id).or_default();
        if list.len() < MAX_HITS && !list.iter().any(|h| h.value == hit.value && h.location == hit.location) {
            list.push(hit);
        }
    }
    Ok(out)
}

/// Local only: no checks against the services the keys belong to, and no
/// self-update.
fn trufflehog_args(dir: &Path) -> Vec<String> {
    ["filesystem", &dir.to_string_lossy(), "--json", "--no-verification", "--no-update", "--no-color"].map(String::from).to_vec()
}

/// Runs a program, giving up after `timeout`. Returns what it printed.
fn run(exe: &Path, args: &[String], timeout: Duration) -> Result<String, String> {
    let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut child = Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start {name}: {e}"))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr = child.stderr.take().expect("stderr is piped");
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{name} took longer than {} seconds and was stopped", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("{name}: {e}")),
        }
    };
    let out = reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    if !status.success() {
        let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(format!("{name} stopped with {status}: {}", clean(last, 300)));
    }
    Ok(out)
}

/// Reads the program's JSON lines into hits, keyed by the file each came from.
fn parse_trufflehog(stdout: &str, files: &HashMap<String, Written>) -> Vec<(String, Hit)> {
    let mut out = vec![];
    for line in stdout.lines().filter(|l| l.starts_with('{')) {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let fs = &v["SourceMetadata"]["Data"]["Filesystem"];
        let Some(file) = fs["file"].as_str().and_then(|f| Path::new(f).file_name()).map(|f| f.to_string_lossy().into_owned()) else { continue };
        let Some(w) = files.get(&file) else { continue };
        let raw = v["Raw"].as_str().unwrap_or("");
        let detector = v["DetectorName"].as_str().unwrap_or("");
        if raw.is_empty() || detector.is_empty() {
            continue;
        }
        let line_no = fs["line"].as_u64().unwrap_or(0) as usize;
        let location = w.lines.get(line_no.saturating_sub(1)).or(w.lines.last()).cloned().unwrap_or_default();
        let mut notes = vec![];
        if let Some(m) = v["ExtraData"]["message"].as_str() {
            notes.push(clean(m, 200));
        }
        let value: String = if raw.len() > MAX_VALUE { raw.chars().take(MAX_VALUE / 4).collect() } else { raw.to_string() };
        out.push((file, Hit { side: w.side, location, detector: clean(detector, 60), value, notes }));
    }
    out
}

/// Candidate query-parameter names the built-in probe ([`Kind::Probe`]) tries
/// on an in-scope endpoint. These are only parameter *names*, not payloads:
/// the probe sends each as `name=<marker>` and reports names that change the
/// response, so a person can see which undocumented inputs an endpoint reads.
/// A modest, bundled list — the user installs nothing.
pub const PARAM_NAMES: &[&str] = &[
    "id", "user", "user_id", "userid", "uid", "account", "account_id", "customer", "customer_id", "profile", "email", "username",
    "name", "page", "per_page", "limit", "offset", "start", "count", "size", "sort", "order", "order_by", "q", "query", "search",
    "filter", "fields", "field", "format", "type", "mode", "view", "lang", "locale", "country", "region", "currency", "callback",
    "redirect", "redirect_uri", "return", "return_url", "next", "url", "uri", "path", "file", "filename", "dir", "folder", "download",
    "token", "access_token", "api_key", "apikey", "key", "secret", "session", "sid", "auth", "code", "state", "nonce", "signature",
    "role", "admin", "is_admin", "debug", "test", "trace", "verbose", "preview", "draft", "status", "active", "enabled", "force",
    "include", "exclude", "expand", "with", "embed", "version", "v", "ref", "source", "utm_source", "tag", "category", "group",
    "parent", "parent_id", "org", "org_id", "team", "team_id", "project", "project_id", "action", "op", "cmd", "method", "target",
];
/// Most candidates the probe sends over one run (plus the baseline).
pub const MAX_PROBES: usize = 128;

/// Longest an enumeration may run over one domain.
pub const ENUMERATE_TIMEOUT: Duration = Duration::from_secs(120);
/// Most subdomains kept from one enumeration.
pub const MAX_DISCOVERED: usize = 500;

/// Enumerates subdomains of `domain` with the program `id`, or a fallback
/// tool if its own is not installed (see [`ENUMERATE_FALLBACKS`]). Returns
/// hostnames under `domain`, deduplicated. The tool reads public sources; it
/// does not connect to the target, and the caller gates `domain` on scope.
pub fn enumerate(id: &str, domain: &str) -> Result<Vec<String>, String> {
    let program = get(id).filter(|p| p.kind == Kind::Enumerate).ok_or_else(|| format!("`{}` does not enumerate subdomains", clean(id, 40)))?;
    let domain = crate::scope::normalize_host(domain);
    if domain.is_empty() || !is_hostname(&domain) {
        return Err(format!("`{}` is not a domain to enumerate", clean(&domain, 60)));
    }
    let (exe, name) = locate(id)
        .map(|e| (e, program.id.to_string()))
        .or_else(|| ENUMERATE_FALLBACKS.iter().find_map(|f| locate_exe(f).map(|e| (e, (*f).to_string()))))
        .ok_or_else(|| format!("{} is not installed. Install it with `{}`, then run this again.", program.id, program.install))?;
    let stdout = run(&exe, &enumerate_args(&name, &domain), ENUMERATE_TIMEOUT)?;
    Ok(parse_hosts(&stdout, &domain))
}

/// Keeps the tool quiet and inside Plonix's time limit. subfinder prints one
/// hostname per line with `-silent`, skips its update check with `-duc`, and
/// is told to wrap up after a minute (`-max-time` is in minutes; its own
/// default of ten would run past [`ENUMERATE_TIMEOUT`] and lose every result).
/// A fallback is asked for the same, and read the same tolerant way.
fn enumerate_args(name: &str, domain: &str) -> Vec<String> {
    match name {
        "bbot" => ["-t", domain, "-f", "subdomain-enum", "-y", "--silent"].map(String::from).to_vec(),
        // subfinder and anything else: bare hostnames, one per line.
        _ => ["-d", domain, "-silent", "-duc", "-max-time", "1", "-timeout", "20"].map(String::from).to_vec(),
    }
}

/// Pulls hostnames under `domain` out of a tool's output, whatever its exact
/// shape: each token may be a bare hostname (subfinder `-silent`) or sit
/// inside JSON. Only valid hostnames that are `domain` or a subdomain of it
/// are kept, so a tool that wanders off the target cannot widen the result.
fn parse_hosts(stdout: &str, domain: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut seen = std::collections::HashSet::new();
    for token in stdout.split(|c: char| c.is_whitespace() || matches!(c, '"' | ',' | '{' | '}' | '[' | ']' | '=' | ':' | '/' | '\\')) {
        let t = token.trim().trim_matches('.').to_ascii_lowercase();
        let h = t.strip_prefix("*.").unwrap_or(&t);
        if h != domain && !crate::scope::is_subdomain_of(h, domain) {
            continue;
        }
        if !is_hostname(h) || !seen.insert(h.to_string()) {
            continue;
        }
        out.push(h.to_string());
        if out.len() >= MAX_DISCOVERED {
            break;
        }
    }
    out
}

fn is_hostname(h: &str) -> bool {
    let labels: Vec<&str> = h.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| !l.is_empty() && l.len() <= 63 && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') && !l.starts_with('-') && !l.ends_with('-'))
        && labels.last().is_some_and(|tld| tld.len() >= 2 && tld.bytes().all(|b| b.is_ascii_alphabetic()))
}

/// A label for a hit: `SendGrid` becomes `SendGrid secret`, `PrivateKey`
/// becomes `Private key`. Names are kept as the program writes them.
pub fn label(detector: &str) -> String {
    for suffix in ["Webhook", "Key", "Token", "Secret", "Password"] {
        if let Some(rest) = detector.strip_suffix(suffix).filter(|r| !r.is_empty() && !r.ends_with(' ')) {
            return format!("{rest} {}", suffix.to_lowercase());
        }
    }
    format!("{detector} secret")
}

/// A private temporary folder, deleted when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self, String> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("plonix-scan-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&dir).map_err(|e| format!("making a temporary folder: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        Ok(Self(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exchange() -> Exchange {
        Exchange {
            id: 7,
            method: "GET".into(),
            path: "/api".into(),
            query: "k=1".into(),
            req_headers: vec![("Host".into(), "a.test".into()), ("X-Key".into(), "AKIAQYLPMN5HHHFPZAM2".into())],
            status: Some(200),
            resp_headers: vec![("Content-Type".into(), "application/json".into())],
            resp_body: b"{\n\"token\": \"ghp_x\"\n}".to_vec(),
            ..Default::default()
        }
    }

    #[test]
    fn lines_map_to_where_they_came_from() {
        let ex = exchange();
        let (text, lines) = render(&ex, Side::Request);
        assert_eq!(text.lines().next(), Some("GET /api?k=1"));
        assert_eq!(lines, ["URL", "header Host", "header X-Key", "request body"]);
        let (text, lines) = render(&ex, Side::Response);
        assert_eq!(text.lines().nth(3), Some("{"));
        assert_eq!(lines, ["status line", "header Content-Type", "response body"]);
    }

    #[test]
    fn reads_the_output_back_onto_exchanges() {
        let ex = exchange();
        let mut files = HashMap::new();
        for side in [Side::Request, Side::Response] {
            let name = format!("7-{}.txt", if side == Side::Request { "request" } else { "response" });
            files.insert(name, Written { id: ex.id, side, lines: render(&ex, side).1 });
        }
        let out = r#"noise that is not JSON
{"SourceMetadata":{"Data":{"Filesystem":{"file":"/tmp/x/7-request.txt","line":3}}},"DetectorName":"AWS","Raw":"AKIAQYLPMN5HHHFPZAM2","ExtraData":{"message":"a canary"}}
{"SourceMetadata":{"Data":{"Filesystem":{"file":"/tmp/x/7-response.txt","line":5}}},"DetectorName":"Github","Raw":"ghp_x"}
{"SourceMetadata":{"Data":{"Filesystem":{"file":"/tmp/x/elsewhere.txt","line":1}}},"DetectorName":"Github","Raw":"ghp_y"}"#;
        let hits = parse_trufflehog(out, &files);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].1, Hit { side: Side::Request, location: "header X-Key".into(), detector: "AWS".into(), value: "AKIAQYLPMN5HHHFPZAM2".into(), notes: vec!["a canary".into()] });
        assert_eq!((hits[1].1.side, hits[1].1.location.as_str()), (Side::Response, "response body"));
    }

    #[test]
    fn labels_read_like_words() {
        assert_eq!(label("AWS"), "AWS secret");
        assert_eq!(label("PrivateKey"), "Private key");
        assert_eq!(label("SendGrid"), "SendGrid secret");
        assert_eq!(label("SlackWebhook"), "Slack webhook");
        assert_eq!(label("JDBC"), "JDBC secret");
    }

    #[test]
    fn only_known_programs_run() {
        assert!(scan("sh", &[]).unwrap_err().contains("does not know"));
        assert!(trufflehog_args(Path::new("/t")).contains(&"--no-verification".to_string()));
    }

    #[test]
    fn enumeration_keeps_only_hostnames_under_the_target() {
        // Bare lines (subfinder -silent) and hostnames embedded in JSON are
        // read the same way; anything not under the target is dropped.
        let out = "www.example.com\napi.example.com\n{\"host\":\"dev.example.com\",\"source\":\"crtsh\"}\nevil.com\nexample.com.attacker.net\nwww.example.com\n";
        let hosts = parse_hosts(out, "example.com");
        assert!(hosts.contains(&"www.example.com".to_string()));
        assert!(hosts.contains(&"api.example.com".to_string()));
        assert!(hosts.contains(&"dev.example.com".to_string()), "pulled out of JSON too");
        assert_eq!(hosts.iter().filter(|h| *h == "www.example.com").count(), 1, "deduplicated");
        assert!(!hosts.contains(&"evil.com".to_string()));
        assert!(!hosts.iter().any(|h| h.contains("attacker")), "example.com.attacker.net is not a subdomain of example.com");
    }

    #[test]
    fn only_enumerate_programs_enumerate_and_the_domain_is_checked() {
        assert!(enumerate("trufflehog", "example.com").unwrap_err().contains("does not enumerate"));
        assert!(enumerate("subfinder", "not a domain").unwrap_err().contains("not a domain"));
        assert_eq!(enumerate_args("subfinder", "example.com"), ["-d", "example.com", "-silent", "-duc", "-max-time", "1", "-timeout", "20"]);
        assert!(get("param-probe").unwrap().builtin && available("param-probe"));
    }
}
