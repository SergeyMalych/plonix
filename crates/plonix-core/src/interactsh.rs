//! interactsh-client, downloaded on demand for the Callbacks tool.
//!
//! Callbacks drive `interactsh-client` (see [`crate::callbacks`]), the
//! open-source callback tool by ProjectDiscovery (MIT license). Building it
//! from source needs Go, so Plonix offers to fetch the prebuilt client from
//! its GitHub release instead: one pinned version, checked against the
//! release's published SHA-256 checksums before it is unpacked.
//!
//! ```text
//! $PLONIX_HOME/tools/
//! ├── interactsh-client          (interactsh-client.exe on Windows)
//! ├── interactsh-client.json     version of this copy
//! └── interactsh-LICENSE.md      the client's license, as shipped
//! ```

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::chromium::{self, Progress, Stage};
use crate::paths::Home;

/// The release Plonix downloads. Pinned, so every copy is one that was tried.
pub const VERSION: &str = "1.4.1";

/// Where the release's files are.
const RELEASES: &str = "https://github.com/projectdiscovery/interactsh/releases/download";

/// What the download weighs, for the button.
pub const SIZE: &str = "about 10 MB";

/// The client's zip is around 10 MB; anything much larger is refused.
const MAX_DOWNLOAD: u64 = 100 * 1024 * 1024;
const MAX_CHECKSUMS: u64 = 64 * 1024;

/// Written next to the client once it checked out.
const MANIFEST: &str = "interactsh-client.json";

/// The release's name for this machine: (os, arch), as in
/// `interactsh-client_<version>_<os>_<arch>.zip`.
pub fn platform() -> Option<(&'static str, &'static str)> {
    if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") { Some(("macOS", "arm64")) } else { Some(("macOS", "amd64")) }
    } else if cfg!(windows) {
        // There is no Windows arm64 build; Windows on Arm runs the x64 one.
        cfg!(any(target_arch = "x86_64", target_arch = "aarch64")).then_some(("windows", "amd64"))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("linux", "amd64"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(("linux", "arm64"))
    } else {
        None
    }
}

/// Where the downloaded client is kept.
pub fn exe_path(home: &Home) -> PathBuf {
    home.tools_dir().join(format!("{}{}", crate::callbacks::PROGRAM, std::env::consts::EXE_SUFFIX))
}

/// A downloaded client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Installed {
    pub version: String,
    #[serde(skip_deserializing)]
    pub exe: PathBuf,
}

/// The client Plonix downloaded, when there is one.
pub fn installed(home: &Home) -> Option<Installed> {
    let exe = exe_path(home);
    if !exe.is_file() {
        return None;
    }
    let version = std::fs::read(home.tools_dir().join(MANIFEST))
        .ok()
        .and_then(|b| serde_json::from_slice::<Installed>(&b).ok())
        .map(|i| i.version)
        .unwrap_or_default();
    Some(Installed { version, exe })
}

/// Deletes the downloaded client.
pub fn remove(home: &Home) -> Result<bool> {
    let exe = exe_path(home);
    if !exe.exists() {
        return Ok(false);
    }
    std::fs::remove_file(&exe).with_context(|| format!("removing {}", exe.display()))?;
    let _ = std::fs::remove_file(home.tools_dir().join(MANIFEST));
    let _ = std::fs::remove_file(home.tools_dir().join("interactsh-LICENSE.md"));
    Ok(true)
}

/// A checksums file: `<sha256>  <file name>` per line → (name, sha256).
/// Lines that are not a checksum are skipped.
pub fn parse_checksums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let (sum, name) = (parts.next()?, parts.next()?.trim_start_matches('*'));
            let ok = sum.len() == 64 && sum.bytes().all(|b| b.is_ascii_hexdigit()) && !name.is_empty() && parts.next().is_none();
            ok.then(|| (name.to_string(), sum.to_ascii_lowercase()))
        })
        .collect()
}

/// The client's zip for `os` and `arch` among the release's files, with its
/// checksum. Matched by shape, so a renamed version part still matches.
pub fn pick_asset<'a>(sums: &'a [(String, String)], os: &str, arch: &str) -> Option<&'a (String, String)> {
    let tail = format!("_{}_{}.zip", os.to_ascii_lowercase(), arch.to_ascii_lowercase());
    sums.iter().find(|(name, _)| {
        let n = name.to_ascii_lowercase();
        n.starts_with("interactsh-client_") && n.ends_with(&tail)
    })
}

/// Refuses a file whose SHA-256 is not the published one.
pub fn check_digest(name: &str, expected: &str, actual: &str) -> Result<()> {
    if !expected.eq_ignore_ascii_case(actual) {
        bail!("{name} does not match its published checksum (expected {expected}, got {actual}); nothing was installed");
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads the pinned client, checks it against the release's checksums,
/// and puts it in the tools folder. `on` hears about every step. A failed
/// install leaves an earlier copy (if any) in place.
pub fn install(home: &Home, on: &dyn Fn(&Progress)) -> Result<Installed> {
    let (os, arch) = platform().ok_or_else(|| anyhow!("there is no interactsh-client download for this computer; install it with Go instead"))?;
    let mut p = Progress { stage: Stage::Looking, version: VERSION.to_string(), ..Progress::default() };
    on(&p);
    let sums_url = format!("{RELEASES}/v{VERSION}/interactsh_{VERSION}_checksums.txt");
    let agent = chromium::agent(&sums_url)?;
    let mut text = String::new();
    chromium::get(&agent, &sums_url)?.into_reader().take(MAX_CHECKSUMS).read_to_string(&mut text).context("reading the release's checksums")?;
    let sums = parse_checksums(&text);
    let (name, expected) = pick_asset(&sums, os, arch).ok_or_else(|| anyhow!("interactsh {VERSION} has no client for {os} {arch}"))?.clone();

    let dir = home.tools_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let id = std::process::id();
    let zip = dir.join(format!("interactsh-download-{id}.zip"));
    let staging = dir.join(format!("interactsh-unpack-{id}"));
    let result = (|| -> Result<Installed> {
        p.stage = Stage::Downloading;
        on(&p);
        let mut hash = Sha256::new();
        chromium::save(chromium::get(&agent, &format!("{RELEASES}/v{VERSION}/{name}"))?, &zip, MAX_DOWNLOAD, &mut p, on, &mut |chunk| hash.update(chunk))?;

        p.stage = Stage::Verifying;
        on(&p);
        check_digest(&name, &expected, &hex(&hash.finalize()))?;

        p.stage = Stage::Unpacking;
        on(&p);
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        chromium::unzip(&zip, &staging)?;
        let file = format!("{}{}", crate::callbacks::PROGRAM, std::env::consts::EXE_SUFFIX);
        let exe = staging.join(&file);
        if !exe.is_file() {
            bail!("the download did not contain {file}");
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))?;
        }
        chromium::unquarantine(&staging);

        p.stage = Stage::Verifying;
        on(&p);
        check_starts(&exe)?;
        let dest = exe_path(home);
        std::fs::rename(&exe, &dest).with_context(|| format!("putting {file} in place (is it running? stop listening first)"))?;
        let _ = std::fs::rename(staging.join("LICENSE.md"), dir.join("interactsh-LICENSE.md"));
        std::fs::write(dir.join(MANIFEST), serde_json::to_vec_pretty(&serde_json::json!({ "version": VERSION }))?)?;
        installed(home).ok_or_else(|| anyhow!("{file} is not where it was put"))
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

/// Runs `interactsh-client -version`: proves the binary is complete and runs
/// on this machine. Nothing is sent anywhere.
fn check_starts(exe: &Path) -> Result<()> {
    let mut child = Command::new(exe)
        .args(["-version", "-duc"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("interactsh-client does not start ({})", exe.display()))?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("interactsh-client did not answer when started; try again");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        bail!("interactsh-client was downloaded but does not run on this machine: {}", err.trim().lines().last().unwrap_or("no output"));
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
    let first = Progress { stage: Stage::Looking, version: VERSION.to_string(), ..Progress::default() };
    *job = Some(first.clone());
    drop(job);
    let home = home.clone();
    let spawned = std::thread::Builder::new().name("interactsh-install".into()).spawn(move || {
        if let Err(e) = install(&home, &|p| *JOB.lock().unwrap() = Some(p.clone())) {
            tracing::warn!("downloading interactsh-client: {e:#}");
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

    // From the v1.4.1 release, trimmed.
    const SUMS: &str = "\
9b30809bd52b09c80a2df84c7dca629ef66bb7d12afe4a5753d685c1f24fbd0c  interactsh-client_1.4.1_linux_386.zip
1b7e2f1c2f61b36b8d3ce9857f0f338ad7dba47c32274465c77e868bcbc5eeeb  interactsh-client_1.4.1_linux_amd64.zip
53cb19b97fa39ffe152903f61ca0359a2a98b413e9d1d78250d604f0f4308579  interactsh-client_1.4.1_linux_arm64.zip
699b387c510b4676f5a35b300e8cae3a1e7b2d6e5e033d7cb467a74af6b25786  interactsh-client_1.4.1_macOS_amd64.zip
e5087016fc7401e21d8fa32e87a2946b290bee5793c6ccb33dbe06d2f93bbd29  interactsh-client_1.4.1_macOS_arm64.zip
fba02aa31b223975cfd01958b417f8c270777d60bebef3fb606ba0c6983e8d4d  interactsh-client_1.4.1_windows_amd64.zip
ee5d3a0a4cfd6df1067548a487d78d2fa9e1a1d1b0a2bf00baa085ede5ff25d2  interactsh-server_1.4.1_windows_amd64.zip
not a checksum line
";

    #[test]
    fn reads_the_checksums_file() {
        let sums = parse_checksums(SUMS);
        assert_eq!(sums.len(), 7);
        assert_eq!(sums[0], ("interactsh-client_1.4.1_linux_386.zip".to_string(), "9b30809bd52b09c80a2df84c7dca629ef66bb7d12afe4a5753d685c1f24fbd0c".to_string()));
        // sha256sum's binary marker, upper-case hex.
        let star = parse_checksums("ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789 *x.zip\nshort x.zip\n");
        assert_eq!(star, [("x.zip".to_string(), "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789".to_string())]);
        assert!(parse_checksums("").is_empty());
    }

    #[test]
    fn picks_the_client_for_each_platform() {
        let sums = parse_checksums(SUMS);
        let name = |os, arch| pick_asset(&sums, os, arch).map(|(n, _)| n.as_str());
        assert_eq!(name("macOS", "arm64"), Some("interactsh-client_1.4.1_macOS_arm64.zip"));
        assert_eq!(name("macos", "amd64"), Some("interactsh-client_1.4.1_macOS_amd64.zip"));
        assert_eq!(name("windows", "amd64"), Some("interactsh-client_1.4.1_windows_amd64.zip"), "never the server");
        assert_eq!(name("linux", "amd64"), Some("interactsh-client_1.4.1_linux_amd64.zip"));
        assert_eq!(name("windows", "arm64"), None);
        assert_eq!(pick_asset(&sums, "linux", "arm64").unwrap().1, "53cb19b97fa39ffe152903f61ca0359a2a98b413e9d1d78250d604f0f4308579");
        // This machine has a download, and it is in the release.
        if let Some((os, arch)) = platform() {
            assert!(pick_asset(&sums, os, arch).is_some(), "{os} {arch}");
        }
    }

    #[test]
    fn refuses_a_download_that_does_not_match() {
        let good = hex(&Sha256::digest(b"zip bytes"));
        assert!(check_digest("c.zip", &good.to_ascii_uppercase(), &good).is_ok());
        let e = check_digest("c.zip", &good, &hex(&Sha256::digest(b"other bytes"))).unwrap_err();
        assert!(e.to_string().contains("does not match its published checksum"), "{e}");
    }

    #[test]
    fn a_downloaded_client_is_found_in_the_tools_folder() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home { root: dir.path().to_path_buf() };
        assert!(installed(&home).is_none());
        std::fs::create_dir_all(home.tools_dir()).unwrap();
        std::fs::write(exe_path(&home), b"").unwrap();
        std::fs::write(home.tools_dir().join(MANIFEST), br#"{"version":"1.4.1"}"#).unwrap();
        let found = installed(&home).unwrap();
        assert_eq!(found.version, "1.4.1");
        assert_eq!(crate::callbacks::locate(&home), Some(exe_path(&home)));
        assert!(remove(&home).unwrap());
        assert!(installed(&home).is_none());
    }
}
