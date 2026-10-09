//! Crash reports, kept on this computer.
//!
//! When Plonix panics (the app, the `plonix` command or any engine thread),
//! a hook writes a plain-text report to `$PLONIX_HOME/crashes/`: the version,
//! the system, where it happened and the backtrace. Before anything is
//! written, the report is scrubbed of what could identify the user or their
//! targets: URL queries and fragments, header and cookie values, tokens, keys
//! and passwords, email addresses and the home folder.
//!
//! Nothing is sent anywhere. The app offers, on its next launch, to show the
//! report or to open a new GitHub issue with it filled in, for the user to
//! read and submit themselves (see [`issue_url`]). The command line prints
//! where the report is.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use regex::Regex;

use crate::paths::Home;

/// Where crash reports are filed, as a new GitHub issue.
pub const NEW_ISSUE_URL: &str = "https://github.com/SergeyMalych/plonix/issues/new";
/// Longest issue link to open: longer ones are refused by GitHub or browsers.
const URL_LIMIT: usize = 8000;
/// Reports kept on disk; older ones are removed.
const KEEP: usize = 20;
/// Reports one process writes at most, so a panic that repeats on every
/// request does not fill the folder.
const PER_PROCESS: usize = 3;
/// Backtrace lines kept in a report.
const BACKTRACE_LINES: usize = 150;
/// Lists the reports the user has already been asked about.
const SEEN_FILE: &str = ".seen";

static WRITTEN: AtomicUsize = AtomicUsize::new(0);

/// Installs the panic hook. `program` names what crashed (`app`, `cli`).
/// The standard hook still runs first, so the panic is printed as usual;
/// `saved` is called with the path of each report written.
pub fn install(home: &Home, program: &'static str, saved: fn(&Path)) {
    let dir = home.crashes_dir();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        if WRITTEN.fetch_add(1, Ordering::SeqCst) >= PER_PROCESS {
            return;
        }
        let message = match (info.payload().downcast_ref::<&str>(), info.payload().downcast_ref::<String>()) {
            (Some(s), _) => s.to_string(),
            (_, Some(s)) => s.clone(),
            _ => "(no message)".to_string(),
        };
        let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let report = Crash { program, message, location, thread, backtrace }.render();
        if let Ok(path) = save(&dir, &report) {
            saved(&path);
        }
    }));
}

/// What a panic hook knows about one crash.
pub struct Crash<'a> {
    pub program: &'a str,
    pub message: String,
    pub location: String,
    pub thread: String,
    pub backtrace: String,
}

impl Crash<'_> {
    /// The scrubbed report, as saved and as shown to the user.
    pub fn render(&self) -> String {
        let mut backtrace: Vec<&str> = self.backtrace.lines().collect();
        let cut = backtrace.len() > BACKTRACE_LINES;
        backtrace.truncate(BACKTRACE_LINES);
        let text = format!(
            "Plonix crash report\n\n\
             version:  {}\n\
             program:  {}\n\
             os:       {} ({})\n\
             arch:     {}\n\
             time:     {}\n\
             thread:   {}\n\
             location: {}\n\n\
             message:\n{}\n\n\
             backtrace:\n{}{}\n",
            env!("CARGO_PKG_VERSION"),
            self.program,
            std::env::consts::OS,
            std::env::consts::FAMILY,
            std::env::consts::ARCH,
            crate::report::date(crate::model::now_ms()),
            self.thread,
            self.location,
            self.message.trim_end(),
            backtrace.join("\n"),
            if cut { "\n…" } else { "" },
        );
        scrub(&text)
    }
}

/// Writes a report, private to the user, and keeps only the newest ones.
pub fn save(dir: &Path, report: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = time::OffsetDateTime::now_utc();
    let name = format!(
        "crash-{:04}{:02}{:02}-{:02}{:02}{:02}-{}.txt",
        stamp.year(),
        u8::from(stamp.month()),
        stamp.day(),
        stamp.hour(),
        stamp.minute(),
        stamp.second(),
        std::process::id()
    );
    let path = dir.join(name);
    crate::paths::write_private(&path, report.as_bytes()).map_err(std::io::Error::other)?;
    let mut all = reports(dir);
    while all.len() > KEEP {
        let _ = std::fs::remove_file(all.remove(all.len() - 1));
    }
    Ok(path)
}

/// Every report in `dir`, newest first.
fn reports(dir: &Path) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("crash-") && n.ends_with(".txt")))
        .collect();
    // Names start with the time, so name order is time order.
    all.sort();
    all.reverse();
    all
}

fn seen(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join(SEEN_FILE)).unwrap_or_default().lines().map(str::to_string).collect()
}

/// Reports the user has not been asked about yet, newest first.
pub fn unseen(home: &Home) -> Vec<PathBuf> {
    let dir = home.crashes_dir();
    let seen = seen(&dir);
    reports(&dir)
        .into_iter()
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| !seen.iter().any(|s| s == n)))
        .collect()
}

/// Remembers that the user was asked about these reports, whatever they chose.
pub fn mark_seen(home: &Home, paths: &[PathBuf]) -> std::io::Result<()> {
    let dir = home.crashes_dir();
    let present: Vec<String> = reports(&dir).iter().filter_map(|p| p.file_name()?.to_str().map(str::to_string)).collect();
    let mut seen: Vec<String> = seen(&dir).into_iter().filter(|s| present.contains(s)).collect();
    for p in paths {
        if let Some(n) = p.file_name().and_then(|n| n.to_str())
            && !seen.iter().any(|s| s == n)
        {
            seen.push(n.to_string());
        }
    }
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(SEEN_FILE), seen.join("\n") + "\n")
}

/// A link that opens a new GitHub issue with the report filled in. Nothing
/// is sent until the user submits the issue. Long reports are cut to fit in
/// a link, keeping their start (the message and the first frames).
pub fn issue_url(report: &str, file_name: &str) -> String {
    let message = report.split("message:\n").nth(1).and_then(|m| m.lines().next()).unwrap_or("").trim();
    let mut title: String = message.chars().take(80).collect();
    if title.is_empty() {
        title = "Plonix crashed".into();
    }
    let head = format!(
        "{NEW_ISSUE_URL}?title={}&body=",
        encode(&format!("Crash: {title}"))
    );
    let intro = encode(
        "<!-- Plonix filled this in from a crash report on your computer. Please read it before you submit, \
         and add what you were doing when it happened. -->\n\n**What I was doing:**\n\n\n**Report:**\n\n```\n",
    );
    let tail = encode("\n```\n");
    let cut_note = encode(&format!("\n… (cut to fit; the full report is ~/.plonix/crashes/{file_name})"));
    let room = URL_LIMIT.saturating_sub(head.len() + intro.len() + tail.len());
    let full = encode(report);
    let body = if full.len() <= room {
        full
    } else {
        let mut out = String::new();
        for c in report.chars() {
            let e = encode(c.encode_utf8(&mut [0; 4]));
            if out.len() + e.len() + cut_note.len() > room {
                break;
            }
            out.push_str(&e);
        }
        out + &cut_note
    };
    format!("{head}{intro}{body}{tail}")
}

/// Percent-encodes everything but unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

const REDACTED: &str = "<redacted>";

/// Names whose values are secret: headers, cookies, query and JSON keys.
const SECRET_NAME: &str = r"[a-z0-9_-]*(?:token|secret|passw(?:or)?d|auth|session|cookie|api[_-]?key|credential|signature)[a-z0-9_-]*";

static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([a-z][a-z0-9+.-]*://)(?:[^\s/?#@]*@)?([^\s/?#]*)([^\s?#]*)(\?[^\s#]*)?(#\S*)?").unwrap()
});
static QUERY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\?[^\s?#]*=[^\s#]*").unwrap());
static HEADER_PAIR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\(\s*"([A-Za-z0-9_-]+)"\s*,\s*"(?:[^"\\]|\\.)*"\s*\)"#).unwrap());
static NAMED_EQ: LazyLock<Regex> = LazyLock::new(|| Regex::new(&format!(r#"(?i)\b({SECRET_NAME})(\s*=\s*)("(?:[^"\\]|\\.)*"|[^\s&,;)"']+)"#)).unwrap());
/// `Cookie: a=1; b=2` hides the whole value, to the end of the line. A
/// colon right after the name (`session::open`) is a path, not a value.
static NAMED_COLON: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r#"(?i)\b({SECRET_NAME})("?\s*:[ \t]*)("(?:[^"\\]|\\.)*"|[^\s,;)"':][^\r\n]*)"#)).unwrap());
static SCHEME_AUTH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(Bearer|Basic|Digest)\s+[A-Za-z0-9._~+/=-]+").unwrap());
static JWT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*").unwrap());
static OPAQUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9+=]{24,}").unwrap());
static EMAIL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+").unwrap());
static HOME_DIR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:/Users/|/home/|(?i:[a-z]:\\Users\\))[^/\\\s:]+").unwrap());

/// Removes what could identify the user or their targets from report text:
/// URL queries, fragments and user names, header and cookie values, tokens,
/// keys and passwords, email addresses and the home folder.
pub fn scrub(text: &str) -> String {
    let mut s = text.to_string();
    if let Some(home) = crate::paths::user_home().map(|h| h.to_string_lossy().trim_end_matches(['/', '\\']).to_string())
        && home.len() > 1
    {
        s = s.replace(&home, "~");
    }
    s = HOME_DIR.replace_all(&s, "~").into_owned();
    s = URL
        .replace_all(&s, |c: &regex::Captures| {
            let mut u = format!("{}{}{}", &c[1], &c[2], &c[3]);
            if c.get(4).is_some() {
                u.push('?');
                u.push_str(REDACTED);
            }
            if c.get(5).is_some() {
                u.push('#');
                u.push_str(REDACTED);
            }
            u
        })
        .into_owned();
    s = QUERY.replace_all(&s, format!("?{REDACTED}")).into_owned();
    s = HEADER_PAIR.replace_all(&s, format!(r#"("$1", "{REDACTED}")"#)).into_owned();
    s = SCHEME_AUTH.replace_all(&s, format!("$1 {REDACTED}")).into_owned();
    s = NAMED_EQ.replace_all(&s, format!("$1$2{REDACTED}")).into_owned();
    s = NAMED_COLON.replace_all(&s, format!("$1$2{REDACTED}")).into_owned();
    s = JWT.replace_all(&s, REDACTED).into_owned();
    s = EMAIL.replace_all(&s, "<email>").into_owned();
    // Long runs of letters and digits mixed are keys and tokens; names and
    // paths have separators, and plain numbers or words are kept.
    s = OPAQUE
        .replace_all(&s, |c: &regex::Captures| {
            let v = &c[0];
            let mixed = v.bytes().any(|b| b.is_ascii_digit()) && v.bytes().any(|b| b.is_ascii_alphabetic());
            if mixed { REDACTED.to_string() } else { v.to_string() }
        })
        .into_owned();
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_queries_headers_tokens_and_home() {
        let home = std::env::var("HOME").unwrap_or_default();
        let text = format!(
            "request to https://user:pw@shop.example.com/api/v1/orders?id=7&token=abc123#frag failed\n\
             Authorization: Bearer abcdef0123456789abcdef0123456789\n\
             Cookie: sid=s3cr3t-value; theme=dark\n\
             headers: [(\"Host\", \"shop.example.com\"), (\"X-Api-Key\", \"k-12345\")]\n\
             body {{\"password\": \"hunter2\", \"user\": \"me@example.com\"}}\n\
             also /search?q=private+words here, api_key=XYZ789&x=1\n\
             jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl\n\
             key AKIAIOSFODNN7EXAMPLE0000 stays out\n\
             at {home}/src/plonix/crates/plonix-core/src/session.rs:12:5\n\
             at /Users/someone/.cargo/registry/src/index/tokio-1.0/src/lib.rs:1:1\n\
             in plonix_core::session::open and plonix_core::auth::check"
        );
        let s = scrub(&text);
        for gone in ["user:pw", "id=7", "abc123", "frag", "abcdef0123456789", "s3cr3t", "theme=dark", "k-12345", "hunter2", "me@example.com", "private+words", "XYZ789", "eyJhbGci", "AKIAIOSFODNN7EXAMPLE0000", "someone"] {
            assert!(!s.contains(gone), "{gone} survived:\n{s}");
        }
        if home.len() > 1 {
            assert!(!s.contains(&home), "home survived:\n{s}");
        }
        for kept in [
            "https://shop.example.com/api/v1/orders?<redacted>",
            "(\"Host\", \"<redacted>\")",
            "~/src/plonix/crates/plonix-core/src/session.rs:12:5",
            "~/.cargo/registry",
            "plonix_core::session::open and plonix_core::auth::check",
        ] {
            assert!(s.contains(kept), "{kept} is missing:\n{s}");
        }
    }

    #[test]
    fn reports_are_saved_listed_and_marked_seen() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::resolve(Some(dir.path())).unwrap();
        let report = Crash {
            program: "cli",
            message: "boom at https://x.test/?k=v".into(),
            location: "crates/plonix-core/src/store.rs:1:1".into(),
            thread: "plonix-recorder".into(),
            backtrace: "   0: plonix_core::store::Store::open".into(),
        }
        .render();
        assert!(report.contains("version:") && report.contains("thread:   plonix-recorder") && report.contains("https://x.test/?<redacted>"));
        let path = save(&home.crashes_dir(), &report).unwrap();
        assert_eq!(unseen(&home), vec![path.clone()]);
        mark_seen(&home, std::slice::from_ref(&path)).unwrap();
        assert!(unseen(&home).is_empty());
        for i in 0..KEEP + 5 {
            std::fs::write(home.crashes_dir().join(format!("crash-2000{i:04}-000000-1.txt")), "x").unwrap();
        }
        save(&home.crashes_dir(), "y").unwrap();
        assert_eq!(reports(&home.crashes_dir()).len(), KEEP, "old reports are removed");
    }

    #[test]
    fn issue_link_is_prefilled_and_fits() {
        let report = Crash {
            program: "app",
            message: "index out of bounds".into(),
            location: "a.rs:1:1".into(),
            thread: "main".into(),
            backtrace: (0..2000).map(|i| format!("  {i}: plonix_core::engine::Engine::frame_{i}\n             at crates/plonix-core/src/engine.rs:{i}:1")).collect::<Vec<_>>().join("\n"),
        }
        .render();
        let url = issue_url(&report, "crash-1.txt");
        assert!(url.starts_with("https://github.com/SergeyMalych/plonix/issues/new?title=Crash%3A%20index%20out%20of%20bounds&body="), "{url}");
        assert!(url.len() <= URL_LIMIT, "{}", url.len());
        assert!(url.contains("cut%20to%20fit") && url.ends_with(&encode("\n```\n")));
        let short = issue_url("Plonix crash report\n\nmessage:\nboom\n", "c.txt");
        assert!(!short.contains("cut%20to%20fit"));
    }
}
