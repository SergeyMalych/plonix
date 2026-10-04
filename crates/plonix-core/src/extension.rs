//! Extension manifests and the capability model (see `docs/extensions.md`).
//!
//! This is the seam for third-party extensions. Plonix does not run
//! extension code yet: the runtime is planned as a WebAssembly sandbox, and
//! until it exists the only installable extension content is declarative
//! (rule packs). What is here is the part that must be right first: the
//! manifest an extension ships, and the closed list of capabilities it may
//! ask for.
//!
//! The capability list is deliberately missing things. There is no
//! capability for network access, file system access, process spawning,
//! changing scope, or sending requests to hosts outside accepted scope, and
//! the manifest parser rejects anything not on the list. A capability that
//! does not exist cannot be granted by mistake.

use serde::{Deserialize, Serialize};

use crate::detect::{check_text, clean};
use crate::rulepack::{check_pack_name, check_version};

pub const FORMAT_VERSION: u32 = 1;

/// What an extension may do. Every capability is mediated by the engine:
/// the extension never gets a raw socket, file handle or database handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// Read captured exchanges, delivered by the engine. In-scope hosts only
    /// unless the user also grants `read-out-of-scope`.
    ReadTraffic,
    /// Also see traffic for hosts that are not accepted into scope.
    ReadOutOfScope,
    /// Read scope rules and suggestions (never change them).
    ReadScope,
    /// Contribute detection rules (same validation as rule packs).
    DetectionRules,
    /// Contribute named Traffic filters (same validation as filter packs).
    NamedFilters,
    /// Contribute panels to existing screens (the Traffic inspector, a
    /// host in the Map), drawn with Plonix's own components from a view
    /// tree the extension returns. No HTML, scripts or styles.
    UiPanels,
    /// Contribute one tab of its own to the sidebar, drawn the same way.
    UiTab,
    /// Return passive observations (tags, notes) for exchanges it was given.
    PassiveAnalysis,
    /// Propose findings. They are recorded as created by the extension and
    /// marked unconfirmed until a person confirms them.
    ProposeFindings,
    /// Ask the engine to send requests. The engine applies scope enforcement
    /// to every request exactly as it does for the CLI and agents, so this
    /// can only ever reach accepted hosts, and each request is recorded.
    ScopedRequests,
}

impl Capability {
    pub fn describe(self) -> &'static str {
        match self {
            Capability::ReadTraffic => "read captured traffic for in-scope hosts",
            Capability::ReadOutOfScope => "read captured traffic for hosts outside scope",
            Capability::ReadScope => "read scope rules and suggestions",
            Capability::DetectionRules => "add technology detection rules",
            Capability::NamedFilters => "add named Traffic filters",
            Capability::UiPanels => "show panels inside Plonix screens, with Plonix's own components",
            Capability::UiTab => "add a sidebar tab, with Plonix's own components",
            Capability::PassiveAnalysis => "annotate traffic it was given",
            Capability::ProposeFindings => "propose findings (unconfirmed until you confirm)",
            Capability::ScopedRequests => "send requests to accepted hosts only (scope-enforced, recorded)",
        }
    }

    /// Capabilities that need an explicit "yes" at install time rather than
    /// being granted with the rest.
    pub fn sensitive(self) -> bool {
        matches!(self, Capability::ReadOutOfScope | Capability::ScopedRequests)
    }
}

/// How the extension runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    /// Declarative only: ships rule packs, no code.
    Declarative,
    /// A WebAssembly component run in the engine's sandbox (planned).
    Wasm,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub plonix_extension: u32,
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    pub runtime: Runtime,
    /// For `wasm`: the module file inside the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
    /// Rule pack files inside the package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rule_packs: Vec<String>,
    /// Filter pack files inside the package.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter_packs: Vec<String>,
    pub capabilities: Vec<Capability>,
}

pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    if bytes.len() > 64 * 1024 {
        return Err("manifest is larger than 64 KiB".into());
    }
    let m: Manifest = serde_json::from_slice(bytes).map_err(|e| format!("invalid extension manifest: {}", clean(&e.to_string(), 300)))?;
    if m.plonix_extension != FORMAT_VERSION {
        return Err(format!("plonix_extension: format {} is not supported", m.plonix_extension));
    }
    check_pack_name(&m.name).map_err(|e| format!("name: {e}"))?;
    check_version(&m.version).map_err(|e| format!("version: {e}"))?;
    check_text(&m.description, 300, false).map_err(|e| format!("description: {e}"))?;
    check_text(&m.author, 100, false).map_err(|e| format!("author: {e}"))?;
    check_text(&m.homepage, 200, true).map_err(|e| format!("homepage: {e}"))?;
    for f in m.entry.iter().chain(&m.rule_packs).chain(&m.filter_packs) {
        check_package_path(f)?;
    }
    match m.runtime {
        Runtime::Declarative => {
            if m.entry.is_some() {
                return Err("entry: a declarative extension has no code entry point".into());
            }
            if m.capabilities.iter().any(|c| !matches!(c, Capability::DetectionRules | Capability::NamedFilters)) {
                return Err("capabilities: a declarative extension can only ask for detection-rules and named-filters".into());
            }
        }
        Runtime::Wasm => {
            if !m.entry.as_deref().is_some_and(|e| e.ends_with(".wasm")) {
                return Err("entry: a wasm extension needs an entry ending in .wasm".into());
            }
        }
    }
    let mut caps = m.capabilities.clone();
    caps.sort();
    caps.dedup();
    if caps.len() != m.capabilities.len() {
        return Err("capabilities: listed twice".into());
    }
    if caps.contains(&Capability::ReadOutOfScope) && !caps.contains(&Capability::ReadTraffic) {
        return Err("capabilities: read-out-of-scope needs read-traffic".into());
    }
    Ok(m)
}

/// Paths inside a package: relative, no `..`, no hidden tricks.
fn check_package_path(p: &str) -> Result<(), String> {
    let ok = !p.is_empty()
        && p.len() <= 200
        && !p.starts_with('/')
        && p.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != "..")
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
    if ok { Ok(()) } else { Err(format!("`{}` must be a relative path inside the package", clean(p, 100))) }
}

/// Whether this build can install the extension. Only declarative
/// extensions can be installed until the sandbox ships.
pub fn installable(m: &Manifest) -> Result<(), String> {
    match m.runtime {
        Runtime::Declarative => Ok(()),
        Runtime::Wasm => Err(format!(
            "{} needs the WebAssembly extension runtime, which this version of Plonix does not have yet",
            m.name
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(runtime: &str, caps: &str, extra: &str) -> String {
        format!(
            r#"{{"plonix_extension":1,"name":"jwt-tools","version":"0.1.0","description":"d","author":"a",
                "runtime":"{runtime}","capabilities":[{caps}]{extra}}}"#
        )
    }

    #[test]
    fn capabilities_are_a_closed_list() {
        for forbidden in ["\"network\"", "\"filesystem\"", "\"exec\"", "\"modify-scope\"", "\"send-anywhere\""] {
            let m = manifest("wasm", forbidden, r#","entry":"x.wasm""#);
            assert!(parse_manifest(m.as_bytes()).unwrap_err().contains("unknown variant"), "{forbidden} was accepted");
        }
        let ok = manifest("wasm", r#""read-traffic","propose-findings""#, r#","entry":"plugin.wasm""#);
        let m = parse_manifest(ok.as_bytes()).unwrap();
        assert!(installable(&m).is_err(), "wasm extensions are not installable yet");
    }

    #[test]
    fn declarative_extensions_cannot_ask_for_more() {
        let m = manifest("declarative", r#""read-traffic""#, "");
        assert!(parse_manifest(m.as_bytes()).is_err());
        let m = manifest("declarative", r#""ui-tab""#, "");
        assert!(parse_manifest(m.as_bytes()).is_err(), "UI needs code, so the sandbox");
        let m = manifest("declarative", r#""detection-rules","named-filters""#, r#","rule_packs":["rules/a.json"],"filter_packs":["filters/a.json"]"#);
        assert!(installable(&parse_manifest(m.as_bytes()).unwrap()).is_ok());
    }

    #[test]
    fn package_paths_stay_inside() {
        for bad in ["../x.wasm", "/etc/x.wasm", "a/../../x.wasm", "a//x.wasm"] {
            let m = manifest("wasm", r#""read-traffic""#, &format!(r#","entry":"{bad}""#));
            assert!(parse_manifest(m.as_bytes()).is_err(), "{bad}");
        }
    }
}
