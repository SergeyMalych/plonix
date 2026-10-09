//! Projects: one folder per target, anywhere on disk.
//!
//! ```text
//! <project folder>/
//! ├── plonix-project.json   name, id and the project's own settings
//! ├── traffic.db            captured traffic, scope and findings (SQLite)
//! ├── browser/              the capture browser's profile for this project
//! └── .plonix.lock          held while a session has the project open
//! ```
//!
//! `$PLONIX_HOME/projects.json` remembers which folders are projects, so
//! the Start screen and `plonix projects` can list them. The folder is the
//! source of truth: moving it and opening it again just works.

use std::fs::File;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::model::now_ms;
use crate::paths::{Home, write_atomic};
use crate::settings::{self, Level, Values};

pub const PROJECT_FILE: &str = "plonix-project.json";
const FORMAT: u32 = 1;

/// Contents of `plonix-project.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectFile {
    pub plonix_project: u32,
    pub id: String,
    pub name: String,
    pub created_at: i64,
    /// Values of project-level settings sections, by section id.
    #[serde(default)]
    pub settings: Map<String, Value>,
    /// The API port of the last session, reused when free so the window
    /// keeps its address (and the per-window state the browser keeps for it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_api_port: Option<u16>,
    /// What "keep only in-scope traffic" removed the last time it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_prune: Option<PruneReport>,
    /// The ready-made demo project (see [`crate::demo`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub demo: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    pub at: i64,
    /// Exchanges deleted.
    pub removed: i64,
    /// Exchanges kept.
    pub kept: i64,
    /// Why nothing was removed, when nothing was.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub skipped: String,
}

#[derive(Debug, Clone)]
pub struct Project {
    pub dir: PathBuf,
    pub file: ProjectFile,
}

impl Project {
    /// Creates a project in `dir`, which must not exist yet or be empty.
    pub fn create(dir: &Path, name: &str) -> Result<Self> {
        let name = clean_name(name)?;
        if dir.join(PROJECT_FILE).exists() {
            bail!("{} is already a Plonix project", dir.display());
        }
        if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
            bail!("{} is not empty. Choose a new or empty folder for the project", dir.display());
        }
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let file = ProjectFile {
            plonix_project: FORMAT,
            id: new_id()?,
            name,
            created_at: now_ms(),
            settings: Map::new(),
            last_api_port: None,
            last_prune: None,
            demo: false,
        };
        let project = Self { dir: absolute(dir), file };
        project.save()?;
        Ok(project)
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join(PROJECT_FILE);
        let bytes = std::fs::read(&path).with_context(|| format!("{} is not a Plonix project (no {PROJECT_FILE})", dir.display()))?;
        let file: ProjectFile = serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?;
        if file.plonix_project > FORMAT {
            bail!("{} was made by a newer Plonix; update Plonix to open it", dir.display());
        }
        Ok(Self { dir: absolute(dir), file })
    }

    pub fn save(&self) -> Result<()> {
        write_atomic(&self.dir.join(PROJECT_FILE), &serde_json::to_vec_pretty(&self.file)?)
    }

    /// Re-reads the file, applies `f` and saves, so concurrent writers
    /// (a session and the Start screen) do not undo each other's changes.
    pub fn update(&mut self, f: impl FnOnce(&mut ProjectFile)) -> Result<()> {
        if let Ok(fresh) = Self::load(&self.dir) {
            self.file = fresh.file;
        }
        f(&mut self.file);
        self.save()
    }

    pub fn id(&self) -> &str {
        &self.file.id
    }
    pub fn name(&self) -> &str {
        &self.file.name
    }
    pub fn db_path(&self) -> PathBuf {
        self.dir.join("traffic.db")
    }
    pub fn browser_profile(&self) -> PathBuf {
        self.dir.join("browser")
    }
    pub fn lock_path(&self) -> PathBuf {
        self.dir.join(".plonix.lock")
    }
    /// Present while a session is open; left behind if it did not close cleanly.
    pub fn open_marker(&self) -> PathBuf {
        self.dir.join(".plonix-open")
    }

    /// A project-level section's values, defaults filled in.
    pub fn settings(&self, section_id: &str) -> Values {
        settings::section(section_id).map(|s| s.resolve(self.file.settings.get(section_id))).unwrap_or_default()
    }

    pub fn save_settings(&mut self, section_id: &str, values: Values) -> Result<()> {
        match settings::section(section_id) {
            Some(s) if s.level == Level::Project => {}
            _ => bail!("{section_id} is not a project settings section"),
        }
        self.update(|f| {
            f.settings.insert(section_id.to_string(), Value::Object(values));
        })
    }

    /// Takes the project's lock. Fails when another session has it open.
    pub fn lock(&self) -> Result<File, AlreadyOpen> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.lock_path())
            .map_err(|e| AlreadyOpen::Io(e.to_string()))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => Err(AlreadyOpen::Busy { name: self.name().to_string() }),
            Err(std::fs::TryLockError::Error(e)) => Err(AlreadyOpen::Io(e.to_string())),
        }
    }

    /// True when a session (in any process) has the project open.
    pub fn is_open(&self) -> bool {
        matches!(self.lock(), Err(AlreadyOpen::Busy { .. }))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AlreadyOpen {
    #[error("project '{name}' is already open in another Plonix session")]
    Busy { name: String },
    #[error("could not lock the project folder: {0}")]
    Io(String),
}

fn absolute(dir: &Path) -> PathBuf {
    crate::paths::canonical(dir).unwrap_or_else(|_| std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf()))
}

fn new_id() -> Result<String> {
    let mut buf = [0u8; 6];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut buf).map_err(|_| anyhow::anyhow!("no system randomness"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn clean_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("the project needs a name");
    }
    if name.chars().count() > 80 || name.chars().any(char::is_control) {
        bail!("the project name must be one line of at most 80 characters");
    }
    Ok(name.to_string())
}

/// A folder name for a project name: `Shop (staging)` → `shop-staging`.
pub fn slug(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let mut out = String::new();
    for c in s.chars() {
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    let out = out.trim_matches(['-', '.']).to_string();
    if out.is_empty() { "project".into() } else { out }
}

// ---- the list of known projects -------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub last_opened: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    projects: Vec<Entry>,
}

fn read_registry(home: &Home) -> Registry {
    std::fs::read(home.projects_file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// The list as stored, for a change that rewrites it.
fn read_registry_for_update(home: &Home) -> Registry {
    crate::paths::set_aside_unreadable::<Registry>(&home.projects_file());
    read_registry(home)
}

fn write_registry(home: &Home, r: &Registry) -> Result<()> {
    write_atomic(&home.projects_file(), &serde_json::to_vec_pretty(r)?)
}

/// Known projects, most recently opened first.
pub fn list(home: &Home) -> Vec<Entry> {
    let mut v = read_registry(home).projects;
    v.sort_by(|a, b| b.last_opened.cmp(&a.last_opened).then(a.name.cmp(&b.name)));
    v
}

/// Adds or refreshes a project in the list.
pub fn remember(home: &Home, p: &Project, opened: bool) -> Result<()> {
    let mut r = read_registry_for_update(home);
    let last_opened = r.projects.iter().find(|e| e.id == p.id() || e.path == p.dir).map_or(0, |e| e.last_opened);
    r.projects.retain(|e| e.id != p.id() && e.path != p.dir);
    r.projects.push(Entry {
        id: p.id().into(),
        name: p.name().into(),
        path: p.dir.clone(),
        last_opened: if opened { now_ms() } else { last_opened },
    });
    write_registry(home, &r)
}

/// Removes a project from the list. Its folder is left alone.
pub fn forget(home: &Home, id: &str) -> Result<bool> {
    let mut r = read_registry_for_update(home);
    let before = r.projects.len();
    r.projects.retain(|e| e.id != id);
    write_registry(home, &r)?;
    Ok(r.projects.len() != before)
}

pub fn find(home: &Home, id: &str) -> Option<Entry> {
    read_registry(home).projects.into_iter().find(|e| e.id == id)
}

/// Finds a project by id, name or folder path. A plain name that matches
/// nothing creates a project of that name in the default projects folder
/// (moving in a database an earlier Plonix kept under that name).
pub fn resolve(home: &Home, selector: &str) -> Result<Project> {
    let sel = selector.trim();
    if sel.is_empty() {
        bail!("empty project name");
    }
    let as_path = Path::new(sel);
    if sel.contains(['/', std::path::MAIN_SEPARATOR]) || as_path.is_absolute() || sel.starts_with('.') || sel.starts_with('~') {
        let dir = expand_tilde(as_path);
        let p = if dir.join(PROJECT_FILE).exists() {
            Project::load(&dir)?
        } else {
            let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
            Project::create(&dir, &name)?
        };
        remember(home, &p, false)?;
        return Ok(p);
    }
    let known = list(home);
    let hit = known
        .iter()
        .find(|e| e.id == sel)
        .or_else(|| known.iter().find(|e| e.name == sel))
        .or_else(|| known.iter().find(|e| e.name.eq_ignore_ascii_case(sel) || slug(&e.name) == slug(sel)));
    if let Some(e) = hit {
        return Project::load(&e.path).with_context(|| format!("project '{}' is no longer at {}", e.name, e.path.display()));
    }
    let dir = home.default_projects_dir().join(slug(sel));
    let p = if dir.join(PROJECT_FILE).exists() {
        Project::load(&dir)?
    } else {
        let legacy = home.project_db(&slug(sel));
        let p = Project::create(&dir, sel)?;
        if legacy.exists() {
            adopt_legacy_db(&legacy, &p.db_path())?;
        }
        p
    };
    remember(home, &p, false)?;
    Ok(p)
}

/// Moves a database an earlier Plonix kept in `$PLONIX_HOME/projects/`.
fn adopt_legacy_db(from: &Path, to: &Path) -> Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let src = PathBuf::from(format!("{}{suffix}", from.display()));
        if src.exists() {
            let dst = PathBuf::from(format!("{}{suffix}", to.display()));
            std::fs::rename(&src, &dst).with_context(|| format!("moving {} into the project folder", src.display()))?;
        }
    }
    tracing::info!("moved {} into {}", from.display(), to.display());
    Ok(())
}

pub fn expand_tilde(p: &Path) -> PathBuf {
    match (p.strip_prefix("~"), crate::paths::user_home()) {
        (Ok(rest), Some(h)) => h.join(rest),
        _ => p.to_path_buf(),
    }
}

/// A list entry with what the Start screen shows about it.
#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    #[serde(flatten)]
    pub entry: Entry,
    /// False when the folder is gone or no longer a project.
    pub available: bool,
    pub open: bool,
    /// The ready-made demo project.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub demo: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<&'static str>,
}

pub fn listings(home: &Home) -> Vec<Listing> {
    list(home)
        .into_iter()
        .map(|mut entry| {
            let loaded = Project::load(&entry.path).ok();
            if let Some(p) = &loaded {
                entry.name = p.name().to_string();
            }
            let size_bytes = loaded.as_ref().map(|p| {
                ["", "-wal"].iter().filter_map(|s| std::fs::metadata(format!("{}{s}", p.db_path().display())).ok()).map(|m| m.len()).sum()
            });
            Listing {
                available: loaded.is_some(),
                open: loaded.as_ref().is_some_and(Project::is_open),
                demo: loaded.as_ref().is_some_and(|p| p.file.demo),
                size_bytes,
                warning: settings::synced_folder_warning(&entry.path),
                entry,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Home {
        Home { root: tempfile::tempdir().unwrap().keep() }
    }

    #[test]
    fn create_load_and_list() {
        let home = home();
        let dir = home.root.join("work/Shop");
        let p = Project::create(&dir, "Shop (staging)").unwrap();
        remember(&home, &p, true).unwrap();
        assert!(Project::create(&dir, "again").is_err(), "a folder holds one project");
        let loaded = Project::load(&dir).unwrap();
        assert_eq!(loaded.id(), p.id());
        assert_eq!(list(&home).len(), 1);
        assert_eq!(resolve(&home, "shop (staging)").unwrap().id(), p.id(), "names match without case");
        assert_eq!(resolve(&home, p.id()).unwrap().dir, p.dir);
        assert!(forget(&home, p.id()).unwrap());
        assert!(dir.join(PROJECT_FILE).exists(), "forgetting leaves the folder alone");
    }

    #[test]
    fn non_empty_folders_are_refused() {
        let home = home();
        let dir = home.root.join("busy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        assert!(Project::create(&dir, "busy").unwrap_err().to_string().contains("not empty"));
    }

    #[test]
    fn names_become_folders_and_legacy_databases_move_in() {
        let home = home();
        std::fs::create_dir_all(home.root.join("projects")).unwrap();
        std::fs::write(home.project_db("example.com"), b"old").unwrap();
        let p = resolve(&home, "example.com").unwrap();
        assert_eq!(p.dir, absolute(&home.root.join("projects/example.com")));
        assert_eq!(std::fs::read(p.db_path()).unwrap(), b"old");
        assert!(!home.project_db("example.com").exists());
        assert_eq!(slug("Shop (staging)"), "shop-staging");
        assert_eq!(slug("../.."), "project");
    }

    #[test]
    fn one_session_per_project() {
        let home = home();
        let p = Project::create(&home.root.join("p"), "p").unwrap();
        let held = p.lock().unwrap();
        assert!(p.is_open());
        assert!(matches!(p.lock(), Err(AlreadyOpen::Busy { .. })));
        drop(held);
        assert!(!p.is_open());
    }

    #[test]
    fn settings_round_trip_and_stay_with_the_folder() {
        let home = home();
        let mut p = Project::create(&home.root.join("p"), "p").unwrap();
        let s = settings::section(settings::STORAGE).unwrap();
        let v = s.check(&serde_json::json!({ "keep_only_in_scope": true }), &p.settings(settings::STORAGE)).unwrap();
        p.save_settings(settings::STORAGE, v).unwrap();
        let again = Project::load(&p.dir).unwrap();
        assert!(settings::StorageSettings::from_values(&again.settings(settings::STORAGE)).keep_only_in_scope);
        assert!(p.save_settings(settings::INTERFACE, Values::new()).is_err(), "global sections are not stored in projects");
    }
}
