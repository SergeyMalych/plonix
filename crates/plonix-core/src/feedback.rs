//! Bug reports and feature ideas, filed by the user as GitHub issues.
//!
//! Plonix sends nothing itself. It opens a new issue form in the browser
//! with only the Plonix version and the system filled in, never traffic,
//! hosts, tokens or anything from a project; the user writes the rest and
//! presses Submit.

use std::sync::LazyLock;

use crate::crash::{NEW_ISSUE_URL, encode};

/// Open issues, for searching before filing a new one.
pub const ISSUES_URL: &str = "https://github.com/SergeyMalych/plonix/issues";

/// What the user wants to file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bug,
    Feature,
}

impl Kind {
    /// The issue form in `.github/ISSUE_TEMPLATE`.
    fn template(self) -> &'static str {
        match self {
            Kind::Bug => "bug_report.yml",
            Kind::Feature => "feature_request.yml",
        }
    }
}

/// The system in words, such as "macOS 15.1 (Apple silicon)" or
/// "Windows 11 (build 22631, x86_64)". Worked out once.
pub fn system() -> &'static str {
    static SYSTEM: LazyLock<String> = LazyLock::new(|| describe(std::env::consts::OS, std::env::consts::ARCH, &crate::usage::os_release()));
    &SYSTEM
}

fn describe(os: &str, arch: &str, release: &str) -> String {
    match os {
        "macos" => {
            let chip = if arch == "aarch64" { "Apple silicon" } else { "Intel" };
            if release.is_empty() { format!("macOS ({chip})") } else { format!("macOS {release} ({chip})") }
        }
        "windows" => {
            // 10.0.22631.4317: builds from 22000 are Windows 11.
            let build: u32 = release.split('.').nth(2).and_then(|b| b.parse().ok()).unwrap_or(0);
            match build {
                0 => format!("Windows ({arch})"),
                b if b >= 22000 => format!("Windows 11 (build {b}, {arch})"),
                b => format!("Windows 10 (build {b}, {arch})"),
            }
        }
        _ if release.is_empty() => format!("{os} ({arch})"),
        _ => format!("{os} {release} ({arch})"),
    }
}

/// A link to a new issue of this kind, with the version and system filled in.
pub fn issue_url(kind: Kind) -> String {
    let mut url = format!("{NEW_ISSUE_URL}?template={}&version={}", kind.template(), encode(env!("CARGO_PKG_VERSION")));
    if kind == Kind::Bug {
        url.push_str(&format!("&os={}", encode(system())));
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systems_read_well() {
        assert_eq!(describe("macos", "aarch64", "15.1.1"), "macOS 15.1.1 (Apple silicon)");
        assert_eq!(describe("macos", "x86_64", ""), "macOS (Intel)");
        assert_eq!(describe("windows", "x86_64", "10.0.22631.4317"), "Windows 11 (build 22631, x86_64)");
        assert_eq!(describe("windows", "x86_64", "10.0.19045.5011"), "Windows 10 (build 19045, x86_64)");
        assert_eq!(describe("windows", "aarch64", ""), "Windows (aarch64)");
        assert_eq!(describe("linux", "x86_64", "24.04"), "linux 24.04 (x86_64)");
    }

    #[test]
    fn links_fill_in_only_version_and_system() {
        let bug = issue_url(Kind::Bug);
        let v = env!("CARGO_PKG_VERSION");
        assert!(bug.starts_with(&format!("{NEW_ISSUE_URL}?template=bug_report.yml&version={v}&os=")), "{bug}");
        assert!(!bug.contains(' '), "{bug}");
        let idea = issue_url(Kind::Feature);
        assert_eq!(idea, format!("{NEW_ISSUE_URL}?template=feature_request.yml&version={v}"));
    }
}
