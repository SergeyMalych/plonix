//! The capture browser: a separate browser that routes through the proxy.
//!
//! Chromium-based browsers are launched with an isolated profile that routes
//! through the proxy and trusts the Plonix CA by its key pin
//! (`--ignore-certificate-errors-spki-list`), so HTTPS works without touching
//! the system keychain. Without one installed, Plonix can download its own
//! Chromium (see [`crate::chromium`]). Firefox gets an isolated profile with
//! proxy settings that follows the macOS keychain, so it needs the CA trusted
//! there once (see [`crate::trust`]).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};

use crate::paths::Home;
use crate::scope::normalize_host;

/// A target as typed by the user, resolved to a URL and a host.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub url: String,
    pub host: String,
}

/// `example.com` → `https://example.com/`; `localhost:3000` → `http://localhost:3000/`.
pub fn parse_target(input: &str) -> Result<Target> {
    let input = input.trim();
    let (scheme, rest) = match input.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => (String::new(), input),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let host = normalize_host(authority);
    if host.is_empty() || host.contains(char::is_whitespace) {
        bail!("'{input}' is not a host name or URL (for example: example.com)");
    }
    let scheme = match scheme.as_str() {
        "" if is_local(&host) => "http".to_string(),
        "" => "https".to_string(),
        "http" | "https" => scheme,
        other => bail!("unsupported scheme '{other}': use http or https"),
    };
    let tail = if tail.is_empty() || !tail.starts_with('/') { format!("/{tail}") } else { tail.to_string() };
    Ok(Target { url: format!("{scheme}://{authority}{tail}"), host })
}

/// Local development targets usually speak plain HTTP.
fn is_local(host: &str) -> bool {
    host == "localhost" || host.ends_with(".localhost") || host.parse::<std::net::IpAddr>().is_ok()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Chromium,
    Firefox,
}

#[derive(Debug, Clone)]
pub struct Browser {
    pub name: String,
    pub exe: PathBuf,
    pub kind: Kind,
}

/// `$PLONIX_BROWSER` if set, else the first Chromium-based browser, else
/// the Plonix browser if it was downloaded, else Firefox.
pub fn detect(home: &Home) -> Option<Browser> {
    if let Some(exe) = std::env::var_os("PLONIX_BROWSER").filter(|v| !v.is_empty()) {
        let exe = PathBuf::from(exe);
        let lower = exe.to_string_lossy().to_ascii_lowercase();
        let kind = if lower.contains("firefox") { Kind::Firefox } else { Kind::Chromium };
        let name = exe.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        return Some(Browser { name, exe, kind });
    }
    let found: Vec<Browser> =
        candidates().into_iter().filter_map(|(name, exe, kind)| exe.map(|exe| Browser { name: name.to_string(), exe, kind })).collect();
    let own = || crate::chromium::installed(home).map(|i| Browser { name: crate::chromium::NAME.to_string(), exe: i.exe, kind: Kind::Chromium });
    found.iter().find(|b| b.kind == Kind::Chromium).cloned().or_else(own).or_else(|| found.into_iter().next())
}

#[cfg(target_os = "macos")]
fn candidates() -> Vec<(&'static str, Option<PathBuf>, Kind)> {
    let apps: [(&str, &str, Kind); 6] = [
        ("Google Chrome", "Google Chrome", Kind::Chromium),
        ("Chromium", "Chromium", Kind::Chromium),
        ("Brave Browser", "Brave Browser", Kind::Chromium),
        ("Microsoft Edge", "Microsoft Edge", Kind::Chromium),
        ("Google Chrome Canary", "Google Chrome Canary", Kind::Chromium),
        ("Firefox", "firefox", Kind::Firefox),
    ];
    let user_apps = std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Applications"));
    apps.into_iter()
        .map(|(app, bin, kind)| {
            let found = [Some(PathBuf::from("/Applications")), user_apps.clone()]
                .into_iter()
                .flatten()
                .map(|dir| dir.join(format!("{app}.app/Contents/MacOS/{bin}")))
                .find(|p| p.is_file());
            (app, found, kind)
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn candidates() -> Vec<(&'static str, Option<PathBuf>, Kind)> {
    [
        ("Google Chrome", "google-chrome", Kind::Chromium),
        ("Google Chrome", "google-chrome-stable", Kind::Chromium),
        ("Chromium", "chromium", Kind::Chromium),
        ("Chromium", "chromium-browser", Kind::Chromium),
        ("Brave", "brave-browser", Kind::Chromium),
        ("Microsoft Edge", "microsoft-edge", Kind::Chromium),
        ("Firefox", "firefox", Kind::Firefox),
    ]
    .into_iter()
    .map(|(name, bin, kind)| (name, which(bin), kind))
    .collect()
}

#[cfg(not(target_os = "macos"))]
fn which(bin: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(bin)).find(|p| p.is_file())
}

/// Command-line flags for a Chromium-based browser.
pub fn chromium_args(profile: &Path, proxy: &str, spki: &str, url: &str) -> Vec<String> {
    let mut args = proxied_chromium(profile, proxy, spki);
    args.extend(["--new-window".into(), url.into()]);
    args
}

/// Flags for the browser crawl: a headless Chromium on its own throwaway
/// profile, routed through the proxy, with a DevTools endpoint on a free
/// loopback port (written to `DevToolsActivePort` in the profile) and no
/// popups.
pub fn headless_args(profile: &Path, proxy: &str, spki: &str) -> Vec<String> {
    let mut args = proxied_chromium(profile, proxy, spki);
    args.extend(
        ["--headless=new", "--remote-debugging-address=127.0.0.1", "--remote-debugging-port=0", "--block-new-web-contents", "--mute-audio", "--hide-scrollbars"]
            .map(String::from),
    );
    // Chromium refuses to start its sandbox as root (containers, CI).
    if running_as_root() {
        args.push("--no-sandbox".into());
    }
    args.push("about:blank".into());
    args
}

#[cfg(target_os = "linux")]
fn running_as_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

#[cfg(not(target_os = "linux"))]
fn running_as_root() -> bool {
    false
}

fn proxied_chromium(profile: &Path, proxy: &str, spki: &str) -> Vec<String> {
    vec![
        format!("--user-data-dir={}", profile.display()),
        format!("--proxy-server=http://{proxy}"),
        // Chromium never proxies loopback by default; local targets should be captured too.
        "--proxy-bypass-list=<-loopback>".into(),
        format!("--ignore-certificate-errors-spki-list={spki}"),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        // Keep the capture about the target, not the browser's own chatter.
        "--disable-background-networking".into(),
        "--disable-component-update".into(),
        "--disable-sync".into(),
        "--no-pings".into(),
        "--disable-domain-reliability".into(),
        "--disable-client-side-phishing-detection".into(),
        "--disable-features=OptimizationHints,MediaRouter,Translate".into(),
    ]
}

/// Writes `user.js` for an isolated Firefox profile that uses the proxy and
/// trusts certificates from the macOS keychain.
pub fn firefox_profile(profile: &Path, proxy: &str) -> Result<()> {
    let (host, port) = proxy.rsplit_once(':').unwrap_or((proxy, "8080"));
    std::fs::create_dir_all(profile)?;
    let prefs = [
        ("network.proxy.type", "1".to_string()),
        ("network.proxy.http", format!("\"{host}\"")),
        ("network.proxy.http_port", port.to_string()),
        ("network.proxy.ssl", format!("\"{host}\"")),
        ("network.proxy.ssl_port", port.to_string()),
        ("network.proxy.no_proxies_on", "\"\"".to_string()),
        ("network.proxy.allow_hijacking_localhost", "true".to_string()),
        ("security.enterprise_roots.enabled", "true".to_string()),
        ("browser.shell.checkDefaultBrowser", "false".to_string()),
        ("app.update.enabled", "false".to_string()),
        ("datareporting.policy.dataSubmissionEnabled", "false".to_string()),
    ];
    let js: String = prefs.iter().map(|(k, v)| format!("user_pref(\"{k}\", {v});\n")).collect();
    std::fs::write(profile.join("user.js"), js)?;
    Ok(())
}

/// Where the capture browser keeps its profile for a project. Each project
/// has its own, so capture browsers of projects open at the same time do
/// not share cookies or proxy settings.
pub fn profile_dir(home: &Home, project_dir: Option<&Path>) -> PathBuf {
    match project_dir {
        Some(d) => d.join("browser"),
        None => home.browser_profile(),
    }
}

/// Launches `browser` through the proxy at `url`, with its profile in
/// `profile`. Returns the arguments used.
pub fn launch(profile: &Path, browser: &Browser, proxy: &str, spki: &str, url: &str) -> Result<Vec<String>> {
    let mut args = match browser.kind {
        Kind::Chromium => chromium_args(profile, proxy, spki, url),
        Kind::Firefox => {
            let profile = PathBuf::from(format!("{}-firefox", profile.display()));
            firefox_profile(&profile, proxy)?;
            vec!["-profile".into(), profile.display().to_string(), "-no-remote".into(), url.into()]
        }
    };
    // Extra flags, e.g. PLONIX_BROWSER_ARGS="--headless=new" for unattended runs.
    if let Ok(extra) = std::env::var("PLONIX_BROWSER_ARGS") {
        let url = args.pop();
        args.extend(extra.split_whitespace().map(String::from));
        args.extend(url);
    }
    spawn_detached(Command::new(&browser.exe).args(&args))
        .map_err(|e| anyhow::anyhow!("could not start {} ({}): {e}", browser.name, browser.exe.display()))?;
    Ok(args)
}

/// Starts a process detached from the caller: its own process group, no
/// inherited stdio, so stopping the caller does not stop it.
pub fn spawn_detached(cmd: &mut Command) -> std::io::Result<std::process::Child> {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    detach(cmd);
    cmd.spawn()
}

/// Puts a child in its own process group.
pub fn detach(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_resolve_to_urls() {
        let t = parse_target("example.com").unwrap();
        assert_eq!(t, Target { url: "https://example.com/".into(), host: "example.com".into() });
        assert_eq!(parse_target("Example.com/login?x=1").unwrap().url, "https://Example.com/login?x=1");
        assert_eq!(parse_target("example.com/login").unwrap().host, "example.com");
        let local = parse_target("localhost:3000").unwrap();
        assert_eq!(local.url, "http://localhost:3000/");
        assert_eq!(local.host, "localhost");
        assert_eq!(parse_target("127.0.0.1:8000/app").unwrap().url, "http://127.0.0.1:8000/app");
        assert_eq!(parse_target("http://shop.test").unwrap().url, "http://shop.test/");
        assert_eq!(parse_target("https://app.acme.com?next=/").unwrap().url, "https://app.acme.com/?next=/");
        assert!(parse_target("ftp://example.com").is_err());
        assert!(parse_target("").is_err());
    }

    #[test]
    fn chromium_goes_through_the_proxy_and_pins_the_ca() {
        let args = chromium_args(Path::new("/tmp/p"), "127.0.0.1:8080", "AbC=", "https://example.com/");
        assert!(args.contains(&"--user-data-dir=/tmp/p".to_string()));
        assert!(args.contains(&"--proxy-server=http://127.0.0.1:8080".to_string()));
        assert!(args.contains(&"--proxy-bypass-list=<-loopback>".to_string()));
        assert!(args.contains(&"--ignore-certificate-errors-spki-list=AbC=".to_string()));
        assert_eq!(args.last().unwrap(), "https://example.com/");
    }

    #[test]
    fn headless_crawl_browser_is_proxied_and_debuggable_on_loopback() {
        let args = headless_args(Path::new("/tmp/c"), "127.0.0.1:8080", "AbC=");
        assert!(args.contains(&"--proxy-server=http://127.0.0.1:8080".to_string()));
        assert!(args.contains(&"--ignore-certificate-errors-spki-list=AbC=".to_string()));
        assert!(args.contains(&"--headless=new".to_string()));
        assert!(args.contains(&"--remote-debugging-address=127.0.0.1".to_string()));
        assert!(args.contains(&"--remote-debugging-port=0".to_string()));
        assert_eq!(args.last().unwrap(), "about:blank");
    }

    #[test]
    fn firefox_profile_sets_the_proxy() {
        let dir = tempfile::tempdir().unwrap();
        firefox_profile(dir.path(), "127.0.0.1:8081").unwrap();
        let js = std::fs::read_to_string(dir.path().join("user.js")).unwrap();
        assert!(js.contains("user_pref(\"network.proxy.ssl\", \"127.0.0.1\");"));
        assert!(js.contains("user_pref(\"network.proxy.ssl_port\", 8081);"));
        assert!(js.contains("user_pref(\"security.enterprise_roots.enabled\", true);"));
    }
}
