//! Storage for installed community packs (rule packs, filter packs).
//!
//! ```text
//! <dir>/
//! ├── lock.json          name → version, sha256, source, installed_at
//! └── packs/<name>.json  the exact bytes that were verified
//! ```
//!
//! A shelf stores bytes its caller has already validated, pins them by
//! SHA-256, and hands back only bytes that still match on read.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::detect::clean;
use crate::paths::write_private;
use crate::rulepack::{check_pack_name, sha256_hex};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Lock {
    packs: BTreeMap<String, LockEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockEntry {
    pub version: String,
    pub sha256: String,
    pub source: String,
    pub installed_at: i64,
}

pub struct Shelf {
    dir: PathBuf,
    /// What is stored here, for messages: "rule pack", "filter pack".
    what: &'static str,
    /// The CLI command that manages it: "rules", "filters".
    cmd: &'static str,
    max: usize,
}

/// An installed pack whose bytes still match the pinned checksum.
pub struct Verified {
    pub name: String,
    pub entry: LockEntry,
    pub bytes: Vec<u8>,
}

/// An installed pack and whether its file still matches the pinned checksum.
#[derive(Debug, Clone)]
pub struct Installed {
    pub name: String,
    pub entry: LockEntry,
    pub intact: bool,
}

impl Shelf {
    pub fn new(dir: &Path, what: &'static str, cmd: &'static str, max: usize) -> Self {
        Self { dir: dir.to_path_buf(), what, cmd, max }
    }

    fn lock_path(&self) -> PathBuf {
        self.dir.join("lock.json")
    }

    fn pack_path(&self, name: &str) -> PathBuf {
        // `name` is validated by check_pack_name, so it cannot contain `/` or `..`.
        self.dir.join("packs").join(format!("{name}.json"))
    }

    fn read_lock(&self) -> Result<Lock> {
        match std::fs::read(self.lock_path()) {
            Ok(b) => serde_json::from_slice(&b).with_context(|| format!("reading {}", self.lock_path().display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Lock::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.lock_path().display())),
        }
    }

    fn write_lock(&self, lock: &Lock) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join("lock.json.tmp");
        write_private(&tmp, &serde_json::to_vec_pretty(lock)?)?;
        std::fs::rename(&tmp, self.lock_path())?;
        Ok(())
    }

    /// A value that changes whenever installed packs change, for caching.
    pub fn stamp(&self) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.lock_path()).and_then(|m| m.modified()).ok()
    }

    /// Checks `bytes` against an expected checksum, if one is given.
    pub fn check_sha(bytes: &[u8], expected: Option<&str>, what: &str) -> Result<String> {
        let actual = sha256_hex(bytes);
        if let Some(want) = expected {
            let want = want.trim().to_ascii_lowercase();
            if want != actual {
                bail!("checksum mismatch: expected sha256 {want}, got {actual}. The {what} was not installed.");
            }
        }
        Ok(actual)
    }

    /// Stores already-validated bytes. Returns the version it replaced.
    pub fn put(&self, name: &str, version: &str, bytes: &[u8], source: &str) -> Result<Option<String>> {
        check_pack_name(name).map_err(|e| anyhow!(e))?;
        let mut lock = self.read_lock()?;
        if !lock.packs.contains_key(name) && lock.packs.len() >= self.max {
            bail!("too many {}s installed (limit {}); remove some first", self.what, self.max);
        }
        std::fs::create_dir_all(self.dir.join("packs"))?;
        let tmp = self.dir.join("packs").join(format!(".{name}.tmp"));
        write_private(&tmp, bytes)?;
        std::fs::rename(&tmp, self.pack_path(name))?;
        let previous = lock.packs.insert(
            name.to_string(),
            LockEntry { version: version.to_string(), sha256: sha256_hex(bytes), source: source.to_string(), installed_at: crate::model::now_ms() },
        );
        self.write_lock(&lock)?;
        Ok(previous.map(|p| p.version))
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        check_pack_name(name).map_err(|e| anyhow!(e))?;
        let mut lock = self.read_lock()?;
        let existed = lock.packs.remove(name).is_some();
        let _ = std::fs::remove_file(self.pack_path(name));
        if existed {
            self.write_lock(&lock)?;
        }
        Ok(existed)
    }

    pub fn installed_version(&self, name: &str) -> Option<String> {
        self.read_lock().ok()?.packs.get(name).map(|e| e.version.clone())
    }

    /// Everything installed here, with whether each file is still the one
    /// that was verified.
    pub fn installed(&self) -> Vec<Installed> {
        let Ok(lock) = self.read_lock() else { return vec![] };
        lock.packs
            .into_iter()
            .filter(|(name, _)| check_pack_name(name).is_ok())
            .map(|(name, entry)| {
                let intact = std::fs::read(self.pack_path(&name)).is_ok_and(|b| sha256_hex(&b) == entry.sha256);
                Installed { name, entry, intact }
            })
            .collect()
    }

    /// Installed packs whose files still match their pinned checksums, and
    /// a problem line for each one that does not.
    pub fn verified(&self) -> (Vec<Verified>, Vec<String>) {
        let mut ok = vec![];
        let mut problems = vec![];
        let lock = match self.read_lock() {
            Ok(l) => l,
            Err(e) => return (ok, vec![format!("{e:#}")]),
        };
        for (name, entry) in lock.packs {
            if check_pack_name(&name).is_err() {
                problems.push(format!("lock.json lists an invalid pack name `{}`", clean(&name, 64)));
                continue;
            }
            match std::fs::read(self.pack_path(&name)) {
                Ok(bytes) if sha256_hex(&bytes) == entry.sha256 => ok.push(Verified { name, entry, bytes }),
                Ok(_) => problems.push(format!(
                    "{} {name}: file changed since it was installed (checksum mismatch); not loaded. Reinstall it with `plonix {} add`.",
                    self.what, self.cmd
                )),
                Err(e) => problems.push(format!("{} {name}: {e}", self.what)),
            }
        }
        (ok, problems)
    }
}
