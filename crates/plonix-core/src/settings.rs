//! Settings: a registry of sections that features add to.
//!
//! A section describes its own fields (type, default, help, limits). The
//! Settings screens are drawn from these descriptions, so a feature that
//! needs settings registers a [`Section`] and gets a form, validation and
//! storage without touching any UI code:
//!
//! ```ignore
//! plonix_core::settings::register(Section::new("my-feature", "My feature", Level::Global)
//!     .describe("What this section controls.")
//!     .field(Field::toggle("enabled", "Turn it on", false)));
//! ```
//!
//! Values live in two places:
//! - [`Level::Global`] sections in `$PLONIX_HOME/settings.json`, shared by
//!   every project;
//! - [`Level::Project`] sections in the project's `plonix-project.json`, so
//!   they travel with the project folder.
//!
//! Sections that are not registered (for example, written by a newer
//! Plonix) are kept as they are when other sections are saved.

use std::net::IpAddr;
use std::path::Path;
use std::sync::{OnceLock, RwLock};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::paths::{Home, write_atomic};

/// Values of one section, by field key.
pub type Values = Map<String, Value>;

/// Where a section's values are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// One value for all projects.
    Global,
    /// Each project has its own value.
    Project,
}

/// When a saved change takes effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Applies {
    /// Right away, in running sessions too.
    Now,
    /// The next time a project opens.
    NextOpen,
}

#[derive(Debug, Clone, Serialize)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

/// The type of a field, which also decides how it is edited.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Kind {
    Text {
        #[serde(skip_serializing_if = "String::is_empty")]
        placeholder: String,
    },
    /// A text that is shown masked, such as a password.
    Secret,
    Number {
        min: i64,
        max: i64,
        #[serde(skip_serializing_if = "String::is_empty")]
        unit: String,
    },
    Toggle,
    Choice {
        options: Vec<Choice>,
    },
    /// A list of strings, edited one per line.
    List {
        #[serde(skip_serializing_if = "String::is_empty")]
        placeholder: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub help: String,
    /// Fields with the same group are shown together under that heading.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub group: String,
    #[serde(flatten)]
    pub kind: Kind,
    pub default: Value,
}

impl Field {
    fn new(key: &str, label: &str, kind: Kind, default: Value) -> Self {
        Self { key: key.into(), label: label.into(), help: String::new(), group: String::new(), kind, default }
    }
    pub fn text(key: &str, label: &str, default: &str) -> Self {
        Self::new(key, label, Kind::Text { placeholder: String::new() }, json!(default))
    }
    pub fn secret(key: &str, label: &str) -> Self {
        Self::new(key, label, Kind::Secret, json!(""))
    }
    pub fn number(key: &str, label: &str, default: i64, min: i64, max: i64) -> Self {
        Self::new(key, label, Kind::Number { min, max, unit: String::new() }, json!(default))
    }
    pub fn toggle(key: &str, label: &str, default: bool) -> Self {
        Self::new(key, label, Kind::Toggle, json!(default))
    }
    pub fn choice(key: &str, label: &str, default: &str, options: &[(&str, &str)]) -> Self {
        let options = options.iter().map(|(v, l)| Choice { value: (*v).into(), label: (*l).into() }).collect();
        Self::new(key, label, Kind::Choice { options }, json!(default))
    }
    pub fn list(key: &str, label: &str, default: &[&str]) -> Self {
        Self::new(key, label, Kind::List { placeholder: String::new() }, json!(default))
    }
    pub fn help(mut self, help: &str) -> Self {
        self.help = help.into();
        self
    }
    pub fn group(mut self, group: &str) -> Self {
        self.group = group.into();
        self
    }
    pub fn placeholder(mut self, p: &str) -> Self {
        match &mut self.kind {
            Kind::Text { placeholder } | Kind::List { placeholder } => *placeholder = p.into(),
            _ => {}
        }
        self
    }
    pub fn unit(mut self, u: &str) -> Self {
        if let Kind::Number { unit, .. } = &mut self.kind {
            *unit = u.into();
        }
        self
    }
}

/// A problem with one field's value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Problem {
    pub field: String,
    pub message: String,
}

impl Problem {
    pub fn new(field: &str, message: impl Into<String>) -> Self {
        Self { field: field.into(), message: message.into() }
    }
}

/// Cross-field checks a section runs after each field is type-checked.
pub type Validator = fn(&Values) -> Vec<Problem>;

/// Where a global section keeps its values when it is not `settings.json`:
/// a feature that already has its own file reads and writes it here.
#[derive(Clone, Copy)]
pub struct Storage {
    pub load: fn(&Home) -> Values,
    pub save: fn(&Home, &Values) -> Result<()>,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Storage")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub level: Level,
    pub applies: Applies,
    /// Lower comes first.
    pub order: i32,
    pub fields: Vec<Field>,
    #[serde(skip)]
    pub validate: Option<Validator>,
    #[serde(skip)]
    pub storage: Option<Storage>,
}

impl Section {
    pub fn new(id: &str, title: &str, level: Level) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            description: String::new(),
            level,
            applies: Applies::Now,
            order: 100,
            fields: vec![],
            validate: None,
            storage: None,
        }
    }
    pub fn describe(mut self, d: &str) -> Self {
        self.description = d.into();
        self
    }
    pub fn applies(mut self, a: Applies) -> Self {
        self.applies = a;
        self
    }
    pub fn order(mut self, o: i32) -> Self {
        self.order = o;
        self
    }
    pub fn field(mut self, f: Field) -> Self {
        self.fields.push(f);
        self
    }
    pub fn validator(mut self, v: Validator) -> Self {
        self.validate = Some(v);
        self
    }
    /// Keeps this global section's values in the feature's own file.
    pub fn stored_by(mut self, load: fn(&Home) -> Values, save: fn(&Home, &Values) -> Result<()>) -> Self {
        self.storage = Some(Storage { load, save });
        self
    }

    /// Stored values with defaults filled in. Values of the wrong type are
    /// replaced by the default, so a hand-edited file never breaks a session.
    pub fn resolve(&self, stored: Option<&Value>) -> Values {
        let stored = stored.and_then(Value::as_object);
        self.fields
            .iter()
            .map(|f| {
                let v = stored.and_then(|s| s.get(&f.key)).filter(|v| check_field(f, v).is_ok()).cloned();
                (f.key.clone(), v.unwrap_or_else(|| f.default.clone()))
            })
            .collect()
    }

    /// Checks new values. Missing fields keep their `current` value; unknown
    /// keys are dropped.
    pub fn check(&self, input: &Value, current: &Values) -> Result<Values, Vec<Problem>> {
        let Some(input) = input.as_object() else {
            return Err(vec![Problem::new("", "settings must be a JSON object")]);
        };
        let mut out = Values::new();
        let mut problems = vec![];
        for f in &self.fields {
            match input.get(&f.key) {
                Some(v) => match check_field(f, v) {
                    Ok(v) => {
                        out.insert(f.key.clone(), v);
                    }
                    Err(m) => problems.push(Problem::new(&f.key, m)),
                },
                None => {
                    out.insert(f.key.clone(), current.get(&f.key).cloned().unwrap_or_else(|| f.default.clone()));
                }
            }
        }
        if problems.is_empty()
            && let Some(validate) = self.validate
        {
            problems = validate(&out);
        }
        if problems.is_empty() { Ok(out) } else { Err(problems) }
    }
}

const MAX_TEXT: usize = 2000;
const MAX_LIST: usize = 500;

fn check_field(f: &Field, v: &Value) -> Result<Value, String> {
    match &f.kind {
        Kind::Text { .. } | Kind::Secret => match v.as_str() {
            Some(s) if s.len() > MAX_TEXT => Err(format!("must be at most {MAX_TEXT} characters")),
            Some(s) if s.chars().any(|c| c.is_control()) => Err("must not contain control characters".into()),
            Some(s) => Ok(json!(s.trim())),
            None => Err("must be text".into()),
        },
        Kind::Number { min, max, .. } => match v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())) {
            Some(n) if n < *min || n > *max => Err(format!("must be between {min} and {max}")),
            Some(n) => Ok(json!(n)),
            None => Err("must be a whole number".into()),
        },
        Kind::Toggle => v.as_bool().map(Value::from).ok_or_else(|| "must be on or off".into()),
        Kind::Choice { options } => match v.as_str() {
            Some(s) if options.iter().any(|o| o.value == s) => Ok(json!(s)),
            _ => Err(format!("must be one of {}", options.iter().map(|o| o.value.as_str()).collect::<Vec<_>>().join(", "))),
        },
        Kind::List { .. } => {
            let items: Vec<String> = match v {
                Value::Array(a) => a.iter().map(|i| i.as_str().map(str::to_string)).collect::<Option<_>>().ok_or("must be a list of text")?,
                Value::String(s) => s.lines().map(str::to_string).collect(),
                _ => return Err("must be a list of text".into()),
            };
            let items: Vec<String> = items.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            if items.len() > MAX_LIST {
                return Err(format!("must have at most {MAX_LIST} entries"));
            }
            if items.iter().any(|s| s.len() > 300 || s.chars().any(|c| c.is_control())) {
                return Err("each entry must be one short line".into());
            }
            Ok(json!(items))
        }
    }
}

// ---- registry ------------------------------------------------------------

fn registry() -> &'static RwLock<Vec<Section>> {
    static REGISTRY: OnceLock<RwLock<Vec<Section>>> = OnceLock::new();
    REGISTRY.get_or_init(|| RwLock::new(vec![proxy_section(), storage_section(), interface_section(), crate::access::settings_section(), crate::market::settings_section(), crate::intercept::settings_section(), crate::replace::settings_section()]))
}

/// Adds a section, or replaces the one with the same id.
pub fn register(section: Section) {
    let mut r = registry().write().unwrap();
    r.retain(|s| s.id != section.id);
    r.push(section);
}

/// All sections, in display order.
pub fn sections() -> Vec<Section> {
    let mut v = registry().read().unwrap().clone();
    v.sort_by(|a, b| a.order.cmp(&b.order).then(a.title.cmp(&b.title)));
    v
}

pub fn section(id: &str) -> Option<Section> {
    registry().read().unwrap().iter().find(|s| s.id == id).cloned()
}

// ---- global storage ------------------------------------------------------

#[derive(Debug, Default, Serialize, Deserialize)]
struct GlobalFile {
    #[serde(default)]
    sections: Map<String, Value>,
}

fn global_path(home: &Home) -> std::path::PathBuf {
    home.root.join("settings.json")
}

fn read_global(home: &Home) -> GlobalFile {
    std::fs::read(global_path(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// The stored values of every global section, as written.
pub fn global_values(home: &Home) -> Map<String, Value> {
    let mut all = read_global(home).sections;
    for s in sections() {
        if let Some(st) = s.storage {
            all.insert(s.id.clone(), Value::Object((st.load)(home)));
        }
    }
    all
}

/// One global section's values, defaults filled in.
pub fn global(home: &Home, id: &str) -> Values {
    let Some(s) = section(id) else { return Values::new() };
    match s.storage {
        Some(st) => s.resolve(Some(&Value::Object((st.load)(home)))),
        None => s.resolve(read_global(home).sections.get(id)),
    }
}

pub fn save_global(home: &Home, id: &str, values: &Values) -> Result<()> {
    home.ensure()?;
    if let Some(st) = section(id).and_then(|s| s.storage) {
        return (st.save)(home, values);
    }
    let mut file = read_global(home);
    file.sections.insert(id.to_string(), Value::Object(values.clone()));
    write_atomic(&global_path(home), &serde_json::to_vec_pretty(&file)?)
}

// ---- built-in sections ---------------------------------------------------

pub const PROXY: &str = "proxy";
pub const STORAGE: &str = "storage";
pub const INTERFACE: &str = "interface";

fn proxy_section() -> Section {
    Section::new(PROXY, "Proxy", Level::Project)
        .describe("How this project's proxy listens, decrypts HTTPS and reaches the internet.")
        .order(10)
        .field(
            Field::text("listen_host", "Listen address", "127.0.0.1")
                .group("Listener")
                .help("127.0.0.1 accepts connections from this computer only. 0.0.0.0 lets phones and other devices on your network use the proxy."),
        )
        .field(Field::number("listen_port", "Port", 8080, 0, 65535).group("Listener").help("0 picks any free port."))
        .field(
            Field::toggle("port_fallback", "Use the next free port if this one is taken", true)
                .group("Listener")
                .help("Lets several projects run at once without clashing."),
        )
        .field(
            Field::toggle("intercept_tls", "Decrypt HTTPS", true)
                .group("HTTPS")
                .help("When off, HTTPS passes through untouched and is not recorded."),
        )
        .field(
            Field::list("passthrough_hosts", "Never decrypt these hosts", &[])
                .group("HTTPS")
                .placeholder("*.pinned.example")
                .help("One host per line; *.example.com includes subdomains. Use it for apps that pin certificates."),
        )
        .field(
            Field::toggle("verify_upstream_tls", "Check server certificates", true)
                .group("HTTPS")
                .help("Turn off for staging servers with self-signed certificates."),
        )
        .field(
            Field::text("upstream_proxy", "Upstream proxy", "")
                .group("Upstream proxy")
                .placeholder("http://proxy.example:3128 or socks5://127.0.0.1:1080")
                .help("Send all traffic through another proxy. Leave empty to connect directly."),
        )
        .field(Field::text("upstream_username", "Username", "").group("Upstream proxy"))
        .field(Field::secret("upstream_password", "Password").group("Upstream proxy").help("Stored in the project file."))
        .field(
            Field::list("upstream_bypass", "Connect directly to", &["localhost", "127.0.0.1", "::1"])
                .group("Upstream proxy")
                .placeholder("*.internal.example")
                .help("Hosts that skip the upstream proxy, one per line."),
        )
        .field(Field::number("connect_timeout_s", "Connect timeout", 10, 1, 300).unit("seconds").group("Timeouts"))
        .field(
            Field::number("request_timeout_s", "Request timeout", 120, 1, 3600)
                .unit("seconds")
                .group("Timeouts")
                .help("How long a server may take to answer. Through the proxy, event streams and downloads keep going once the answer has started."),
        )
        .field(
            Field::number("max_body_mb", "Keep bodies up to", 10, 1, 1024)
                .unit("MB")
                .group("Recording")
                .help("Each request and response body passes through in full; Plonix keeps the start of a longer one and notes its full size."),
        )
        .validator(|v| {
            let mut p = vec![];
            let host = v.get("listen_host").and_then(Value::as_str).unwrap_or("");
            if host.parse::<IpAddr>().is_err() {
                p.push(Problem::new("listen_host", "must be an IP address, such as 127.0.0.1 or 0.0.0.0"));
            }
            let up = v.get("upstream_proxy").and_then(Value::as_str).unwrap_or("");
            if !up.is_empty()
                && let Err(e) = crate::upstream::ProxyServer::parse(up)
            {
                p.push(Problem::new("upstream_proxy", e));
            }
            p
        })
}

fn storage_section() -> Section {
    Section::new(STORAGE, "Storage", Level::Project)
        .describe("What this project keeps on disk.")
        .order(20)
        .field(Field::toggle("keep_only_in_scope", "Keep only in-scope traffic", false).help(
            "When the project closes, Plonix deletes traffic to every host that is not in scope (rejected, suggested or never decided) \
             and compacts the file so the data is gone from disk. Findings keep the requests they point to. If nothing is in scope yet, \
             nothing is deleted.",
        ))
}

fn interface_section() -> Section {
    Section::new(INTERFACE, "Interface", Level::Global)
        .describe("How Plonix opens projects. Applies to all projects.")
        .order(30)
        .field(
            Field::choice("open_projects_in", "Open projects in", "window", &[("window", "A Plonix window"), ("browser", "My web browser")])
                .help("The web version is the same Plonix, at a local address only this computer can open."),
        )
}

// ---- typed views -----------------------------------------------------------

fn s(v: &Values, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}
fn b(v: &Values, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}
fn n(v: &Values, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}
fn l(v: &Values, k: &str) -> Vec<String> {
    v.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(|i| i.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProxySettings {
    pub listen_host: IpAddr,
    pub listen_port: u16,
    pub port_fallback: bool,
    pub intercept_tls: bool,
    pub passthrough_hosts: Vec<String>,
    pub verify_upstream_tls: bool,
    pub upstream_proxy: String,
    pub upstream_username: String,
    pub upstream_password: String,
    pub upstream_bypass: Vec<String>,
    pub connect_timeout_s: u64,
    pub request_timeout_s: u64,
    /// Bodies are recorded up to this many megabytes each.
    pub max_body_mb: u64,
}

impl ProxySettings {
    pub fn from_values(v: &Values) -> Self {
        Self {
            listen_host: s(v, "listen_host").parse().unwrap_or(IpAddr::from([127, 0, 0, 1])),
            listen_port: n(v, "listen_port").clamp(0, 65535) as u16,
            port_fallback: b(v, "port_fallback"),
            intercept_tls: b(v, "intercept_tls"),
            passthrough_hosts: l(v, "passthrough_hosts"),
            verify_upstream_tls: b(v, "verify_upstream_tls"),
            upstream_proxy: s(v, "upstream_proxy"),
            upstream_username: s(v, "upstream_username"),
            upstream_password: s(v, "upstream_password"),
            upstream_bypass: l(v, "upstream_bypass"),
            connect_timeout_s: n(v, "connect_timeout_s").max(1) as u64,
            request_timeout_s: n(v, "request_timeout_s").max(1) as u64,
            max_body_mb: n(v, "max_body_mb").max(1) as u64,
        }
    }
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self::from_values(&proxy_section().resolve(None))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StorageSettings {
    pub keep_only_in_scope: bool,
}

impl StorageSettings {
    pub fn from_values(v: &Values) -> Self {
        Self { keep_only_in_scope: b(v, "keep_only_in_scope") }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InterfaceSettings {
    /// True: open projects in the user's web browser instead of a window.
    pub open_in_browser: bool,
}

impl InterfaceSettings {
    pub fn from_values(v: &Values) -> Self {
        Self { open_in_browser: s(v, "open_projects_in") == "browser" }
    }
    pub fn load(home: &Home) -> Self {
        Self::from_values(&global(home, INTERFACE))
    }
}

/// Every section with its current values, for a Settings screen. Project
/// sections are included when `project` is given.
pub fn describe(home: &Home, project: Option<&Map<String, Value>>) -> Value {
    let globals = global_values(home);
    let list: Vec<Value> = sections()
        .into_iter()
        .filter(|s| s.level == Level::Global || project.is_some())
        .map(|s| {
            let stored = match s.level {
                Level::Global => globals.get(&s.id),
                Level::Project => project.and_then(|p| p.get(&s.id)),
            };
            let mut v = serde_json::to_value(&s).unwrap_or(Value::Null);
            v["values"] = Value::Object(s.resolve(stored));
            v
        })
        .collect();
    json!({ "sections": list })
}

/// Whether a path looks like it is inside a file-syncing folder, where a
/// growing database is a bad idea.
pub fn synced_folder_warning(path: &Path) -> Option<&'static str> {
    let p = path.to_string_lossy();
    if p.contains("/Library/Mobile Documents/") || p.contains("/Library/CloudStorage/") {
        Some("This folder is synced to the cloud. Captured traffic changes constantly, which syncs poorly; a local folder works best.")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fill_in_and_bad_values_fall_back() {
        let s = section(PROXY).unwrap();
        let v = s.resolve(Some(&json!({ "listen_port": "nope", "intercept_tls": false })));
        assert_eq!(v["listen_port"], json!(8080));
        assert_eq!(v["intercept_tls"], json!(false));
        let p = ProxySettings::from_values(&v);
        assert_eq!(p.listen_host.to_string(), "127.0.0.1");
        assert!(!p.intercept_tls);
        assert_eq!(p.upstream_bypass, vec!["localhost", "127.0.0.1", "::1"]);
        assert_eq!(p.max_body_mb, 10);
    }

    #[test]
    fn check_reports_each_bad_field() {
        let s = section(PROXY).unwrap();
        let cur = s.resolve(None);
        let err = s.check(&json!({ "listen_port": 70000, "verify_upstream_tls": "yes" }), &cur).unwrap_err();
        let fields: Vec<_> = err.iter().map(|p| p.field.as_str()).collect();
        assert_eq!(fields, vec!["listen_port", "verify_upstream_tls"]);
        let err = s.check(&json!({ "listen_host": "localhost" }), &cur).unwrap_err();
        assert_eq!(err[0].field, "listen_host");
        let err = s.check(&json!({ "upstream_proxy": "ftp://x:1" }), &cur).unwrap_err();
        assert_eq!(err[0].field, "upstream_proxy");
        let ok = s.check(&json!({ "listen_port": "9000", "passthrough_hosts": "a.test\n\n *.b.test " }), &cur).unwrap();
        assert_eq!(ok["listen_port"], json!(9000));
        assert_eq!(ok["passthrough_hosts"], json!(["a.test", "*.b.test"]));
        assert_eq!(ok["listen_host"], json!("127.0.0.1"), "untouched fields keep their value");
    }

    #[test]
    fn features_register_their_own_sections() {
        register(
            Section::new("test-feature", "Test feature", Level::Global)
                .order(500)
                .field(Field::choice("mode", "Mode", "a", &[("a", "A"), ("b", "B")])),
        );
        let s = section("test-feature").unwrap();
        assert!(s.check(&json!({ "mode": "c" }), &s.resolve(None)).is_err());
        assert!(sections().iter().any(|s| s.id == "test-feature"));
        let home = Home { root: tempfile::tempdir().unwrap().keep() };
        save_global(&home, "test-feature", &s.check(&json!({ "mode": "b" }), &s.resolve(None)).unwrap()).unwrap();
        save_global(&home, INTERFACE, &section(INTERFACE).unwrap().resolve(None)).unwrap();
        assert_eq!(global(&home, "test-feature")["mode"], json!("b"), "saving one section keeps the others");
    }

    #[test]
    fn interface_defaults_to_a_window() {
        let home = Home { root: tempfile::tempdir().unwrap().keep() };
        let i = InterfaceSettings::load(&home);
        assert!(!i.open_in_browser);
    }
}
