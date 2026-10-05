//! The Plonix browser: Chromium, downloaded on demand.
//!
//! A Mac with only Safari (or a Linux machine with no Chromium-based
//! browser) has nothing Plonix can open with an isolated profile. Instead of
//! shipping a browser inside Plonix, which would make every download hundreds
//! of megabytes larger, Plonix fetches Google's "Chrome for Testing" stable
//! build when the user asks for it, into `$PLONIX_HOME/chromium/current`, and
//! uses it as the capture browser like any other Chromium.
//!
//! ```text
//! $PLONIX_HOME/chromium/
//! └── current/
//!     ├── plonix-browser.json        version and platform of this copy
//!     └── chrome-mac-arm64/Google Chrome for Testing.app
//! ```

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::Home;

/// Google's list of the current Chrome for Testing builds and their downloads.
pub const VERSIONS_URL: &str = "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";

/// How the downloaded browser is called in the UI and the CLI.
pub const NAME: &str = "Plonix browser";

/// Written next to the unpacked browser once it checked out.
const MANIFEST: &str = "plonix-browser.json";

/// No Chromium download is anywhere near this; anything larger is refused.
const MAX_DOWNLOAD: u64 = 1024 * 1024 * 1024;

/// The Chrome for Testing platform for this machine, if there is one.
pub fn platform() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("mac-arm64")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("mac-x64")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("linux64")
    } else {
        None
    }
}

/// Where downloaded browsers live.
pub fn root(home: &Home) -> PathBuf {
    home.root.join("chromium")
}

fn current(home: &Home) -> PathBuf {
    root(home).join("current")
}

/// The browser executable inside an unpacked download.
fn exe_in(dir: &Path, platform: &str) -> PathBuf {
    let top = dir.join(format!("chrome-{platform}"));
    if platform.starts_with("mac") {
        top.join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing")
    } else {
        top.join("chrome")
    }
}

/// A downloaded browser that is ready to launch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Installed {
    pub version: String,
    pub platform: String,
    #[serde(skip_deserializing)]
    pub exe: PathBuf,
    #[serde(skip_deserializing)]
    pub dir: PathBuf,
}

/// The downloaded browser, when there is one and it is complete.
pub fn installed(home: &Home) -> Option<Installed> {
    let dir = current(home);
    let mut found: Installed = serde_json::from_slice(&std::fs::read(dir.join(MANIFEST)).ok()?).ok()?;
    found.exe = exe_in(&dir, &found.platform);
    found.dir = dir;
    found.exe.is_file().then_some(found)
}

/// Deletes the downloaded browser. Its capture profiles are kept.
pub fn remove(home: &Home) -> Result<bool> {
    let dir = root(home);
    if !dir.exists() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    Ok(true)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    #[default]
    Idle,
    /// Asking Google for the current stable build.
    Looking,
    Downloading,
    Unpacking,
    /// Checking that the browser unpacked completely and starts.
    Verifying,
    Done,
    Failed,
}

impl Stage {
    pub fn busy(self) -> bool {
        matches!(self, Stage::Looking | Stage::Downloading | Stage::Unpacking | Stage::Verifying)
    }
}

/// How far an install has come.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Progress {
    pub stage: Stage,
    pub version: String,
    /// Bytes downloaded so far, and in all (0 while unknown).
    pub received: u64,
    pub total: u64,
    pub error: Option<String>,
}

/// The stable build for `platform` in Google's version list: (version, url).
pub fn stable_download(list: &Value, platform: &str) -> Result<(String, String)> {
    let stable = &list["channels"]["Stable"];
    let version = stable["version"].as_str().filter(|v| !v.is_empty()).ok_or_else(|| anyhow!("the version list has no stable build"))?;
    let url = stable["downloads"]["chrome"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|d| d["platform"] == platform)
        .and_then(|d| d["url"].as_str())
        .ok_or_else(|| anyhow!("there is no stable build for {platform}"))?;
    if !url.starts_with("https://") {
        bail!("refusing a download that is not https: {url}");
    }
    Ok((version.to_string(), url.to_string()))
}

fn agent() -> Result<ureq::Agent> {
    let mut b = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout_read(Duration::from_secs(60)).redirects(3);
    if let Some(p) = std::env::var("HTTPS_PROXY").ok().or_else(|| std::env::var("https_proxy").ok()).filter(|p| !p.is_empty()) {
        b = b.proxy(ureq::Proxy::new(p).context("invalid HTTPS_PROXY")?);
    }
    Ok(b.build())
}

fn get(agent: &ureq::Agent, url: &str) -> Result<ureq::Response> {
    agent.get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => anyhow!("{url}: HTTP {code}"),
        ureq::Error::Transport(t) => anyhow!("could not download it ({t}); check your internet connection and try again"),
    })
}

/// Downloads, unpacks and checks the current stable build, then makes it the
/// Plonix browser. `on` hears about every step. A failed install leaves the
/// previous copy (if any) in place.
pub fn install(home: &Home, on: &dyn Fn(&Progress)) -> Result<Installed> {
    let platform = platform().ok_or_else(|| anyhow!("the Plonix browser is available for macOS and 64-bit Linux only"))?;
    let mut p = Progress { stage: Stage::Looking, ..Progress::default() };
    on(&p);
    let agent = agent()?;
    let list: Value = get(&agent, VERSIONS_URL)?.into_json().context("reading the Chrome for Testing version list")?;
    let (version, url) = stable_download(&list, platform)?;
    p.version = version.clone();

    let root = root(home);
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let id = std::process::id();
    let zip = root.join(format!("download-{id}.zip"));
    let staging = root.join(format!("unpack-{id}"));
    let result = (|| -> Result<Installed> {
        p.stage = Stage::Downloading;
        on(&p);
        let resp = get(&agent, &url)?;
        p.total = resp.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
        if p.total > MAX_DOWNLOAD {
            bail!("the download is unexpectedly large ({} bytes); refusing it", p.total);
        }
        let mut reader = resp.into_reader().take(MAX_DOWNLOAD + 1);
        let mut file = std::fs::File::create(&zip).with_context(|| format!("creating {}", zip.display()))?;
        let mut buf = vec![0u8; 256 * 1024];
        let mut told = Instant::now();
        loop {
            let n = reader.read(&mut buf).context("downloading the browser")?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).context("saving the download")?;
            p.received += n as u64;
            if told.elapsed() > Duration::from_millis(150) {
                on(&p);
                told = Instant::now();
            }
        }
        file.flush()?;
        drop(file);
        if p.received > MAX_DOWNLOAD || (p.total > 0 && p.received != p.total) {
            bail!("the download was cut off ({} of {} bytes); try again", p.received, p.total);
        }
        on(&p);

        p.stage = Stage::Unpacking;
        on(&p);
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        unzip(&zip, &staging)?;

        p.stage = Stage::Verifying;
        on(&p);
        let exe = exe_in(&staging, platform);
        if !exe.is_file() {
            bail!("the download did not contain the browser ({} is missing)", exe.display());
        }
        unquarantine(&staging);
        check_starts(&exe)?;
        let manifest = serde_json::json!({ "version": version, "platform": platform });
        std::fs::write(staging.join(MANIFEST), serde_json::to_vec_pretty(&manifest)?)?;

        // Swap it in: the old copy goes only once the new one is in place.
        let dest = current(home);
        let old = root.join(format!("old-{id}"));
        if dest.exists() {
            std::fs::rename(&dest, &old).context("moving the previous Plonix browser aside")?;
        }
        std::fs::rename(&staging, &dest).context("putting the Plonix browser in place")?;
        let _ = std::fs::remove_dir_all(&old);
        installed(home).ok_or_else(|| anyhow!("the Plonix browser is not where it was put"))
    })();
    let _ = std::fs::remove_file(&zip);
    let _ = std::fs::remove_dir_all(&staging);
    match result {
        Ok(found) => {
            p.stage = Stage::Done;
            on(&p);
            Ok(found)
        }
        Err(e) => {
            p.stage = Stage::Failed;
            p.error = Some(format!("{e:#}"));
            on(&p);
            Err(e)
        }
    }
}

/// Unpacks a zip with the system's own tool, which keeps the symlinks and
/// permissions a macOS app bundle relies on.
fn unzip(zip: &Path, dest: &Path) -> Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("/usr/bin/ditto");
        c.args(["-x", "-k"]).arg(zip).arg(dest);
        c
    } else {
        let mut c = Command::new("unzip");
        c.args(["-q", "-o"]).arg(zip).arg("-d").arg(dest);
        c
    };
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => anyhow!("`unzip` is not installed; install it and try again"),
        _ => anyhow!("unpacking the browser: {e}"),
    })?;
    if !out.status.success() {
        bail!("unpacking the browser failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// macOS refuses to open downloaded apps that carry the quarantine mark
/// without asking the user; this copy was fetched on purpose.
fn unquarantine(dir: &Path) {
    if cfg!(target_os = "macos") {
        let _ = Command::new("/usr/bin/xattr").args(["-dr", "com.apple.quarantine"]).arg(dir).stdin(Stdio::null()).stderr(Stdio::null()).status();
    }
}

/// Runs `<browser> --version` and expects it to answer like Chrome for
/// Testing: proves the binary is complete and can run on this machine.
fn check_starts(exe: &Path) -> Result<()> {
    let mut child = Command::new(exe)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("the browser does not start ({})", exe.display()))?;
    let deadline = Instant::now() + Duration::from_secs(90);
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("the browser did not answer when started; try again");
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let (mut out, mut err) = (String::new(), String::new());
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_string(&mut out);
    }
    if let Some(mut e) = child.stderr.take() {
        let _ = e.read_to_string(&mut err);
    }
    if !status.success() || !out.contains("Chrome") {
        let detail = if err.trim().is_empty() { out.trim() } else { err.trim() };
        bail!("the browser was downloaded but does not run on this machine: {}", detail.lines().next().unwrap_or("no output"));
    }
    Ok(())
}

// ---- one install at a time, for the window ------------------------------------

static JOB: Mutex<Option<Progress>> = Mutex::new(None);

/// Where the install started from the window stands.
pub fn progress() -> Progress {
    JOB.lock().unwrap().clone().unwrap_or_default()
}

/// Starts installing in the background, unless an install is already running.
pub fn start_install(home: &Home) -> Progress {
    let mut job = JOB.lock().unwrap();
    if job.as_ref().is_some_and(|p| p.stage.busy()) {
        return job.clone().unwrap_or_default();
    }
    let first = Progress { stage: Stage::Looking, ..Progress::default() };
    *job = Some(first.clone());
    drop(job);
    let home = home.clone();
    let spawned = std::thread::Builder::new().name("plonix-browser-install".into()).spawn(move || {
        let r = install(&home, &|p| *JOB.lock().unwrap() = Some(p.clone()));
        if let Err(e) = r {
            tracing::warn!("installing the Plonix browser: {e:#}");
        }
    });
    if let Err(e) = spawned {
        let failed = Progress { stage: Stage::Failed, error: Some(e.to_string()), ..Progress::default() };
        *JOB.lock().unwrap() = Some(failed.clone());
        return failed;
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_stable_build_for_the_platform() {
        let list = serde_json::json!({
            "channels": {
                "Beta": { "version": "200.0.1.0", "downloads": { "chrome": [{ "platform": "mac-arm64", "url": "https://x/beta.zip" }] } },
                "Stable": { "version": "199.0.7.12", "downloads": { "chrome": [
                    { "platform": "linux64", "url": "https://storage.googleapis.com/x/linux64/chrome-linux64.zip" },
                    { "platform": "mac-arm64", "url": "https://storage.googleapis.com/x/mac-arm64/chrome-mac-arm64.zip" }
                ] } }
            }
        });
        let (v, url) = stable_download(&list, "mac-arm64").unwrap();
        assert_eq!(v, "199.0.7.12");
        assert!(url.ends_with("chrome-mac-arm64.zip"));
        assert!(stable_download(&list, "mac-x64").is_err());
        let plain = serde_json::json!({ "channels": { "Stable": { "version": "1", "downloads": { "chrome": [{ "platform": "linux64", "url": "http://x/c.zip" }] } } } });
        assert!(stable_download(&plain, "linux64").is_err());
        assert!(stable_download(&serde_json::json!({}), "linux64").is_err());
    }

    #[test]
    fn an_installed_copy_is_found_only_when_complete() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home { root: dir.path().to_path_buf() };
        assert!(installed(&home).is_none());
        let cur = current(&home);
        std::fs::create_dir_all(&cur).unwrap();
        std::fs::write(cur.join(MANIFEST), r#"{"version":"199.0.7.12","platform":"linux64"}"#).unwrap();
        assert!(installed(&home).is_none(), "no executable yet");
        std::fs::create_dir_all(cur.join("chrome-linux64")).unwrap();
        std::fs::write(cur.join("chrome-linux64/chrome"), b"").unwrap();
        let found = installed(&home).unwrap();
        assert_eq!(found.version, "199.0.7.12");
        assert_eq!(found.exe, cur.join("chrome-linux64/chrome"));
        assert!(remove(&home).unwrap());
        assert!(installed(&home).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn a_browser_must_answer_like_chrome() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = |name: &str, body: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        assert!(check_starts(&script("good", "echo 'Google Chrome for Testing 199.0.7.12'")).is_ok());
        let e = check_starts(&script("libs", "echo 'error while loading shared libraries: libnss3.so' >&2; exit 127")).unwrap_err();
        assert!(e.to_string().contains("libnss3"), "{e}");
        assert!(check_starts(&script("other", "echo hello")).is_err());
        assert!(check_starts(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn mac_builds_point_inside_the_app_bundle() {
        let exe = exe_in(Path::new("/x"), "mac-arm64");
        assert_eq!(exe, Path::new("/x/chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing"));
    }
}
