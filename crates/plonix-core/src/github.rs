//! Adding a package from a GitHub repository.
//!
//! An author publishes a package as a file on a GitHub release: a
//! `.plonixext` extension, a skill (`.md`) or a pack (`.json`). Plonix reads
//! the release through GitHub's public API, downloads that one file and
//! hands it to the same checks as any file from outside the Market. Nothing
//! is built or run: a repository's source code never reaches this machine.
//!
//! A repository is written `github:owner/repo`, `github.com/owner/repo` or
//! as its https address. `@tag` picks a release (the latest by default) and
//! `#file` picks one file when a release has several packages.
//!
//! What was added is recorded as `github:owner/repo@tag`, so the Market can
//! say when a newer release is out. It never updates one on its own.

use anyhow::{Result, anyhow, bail};
use serde::Deserialize;

use crate::detect::clean;
use crate::registry::{self, Location};

/// GitHub's API, unless a test points Plonix elsewhere.
fn api_base() -> String {
    std::env::var("PLONIX_GITHUB_API").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "https://api.github.com".into())
}

const MAX_RELEASE_BYTES: usize = 1024 * 1024;

/// A repository, and optionally which release and which file of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub owner: String,
    pub repo: String,
    pub tag: Option<String>,
    pub file: Option<String>,
}

impl Repo {
    /// How an added package records where it came from.
    pub fn label(&self, tag: &str) -> String {
        let file = self.file.as_ref().map(|f| format!("#{f}")).unwrap_or_default();
        format!("github:{}/{}@{tag}{file}", self.owner, self.repo)
    }

    /// The same repository without a pinned release: what to check for updates.
    pub fn latest(&self) -> Repo {
        Repo { tag: None, ..self.clone() }
    }

    pub fn page(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }
}

fn check_part(s: &str, what: &str) -> Result<(), String> {
    let ok = !s.is_empty() && s.len() <= 100 && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
    if ok { Ok(()) } else { Err(format!("`{}` is not a GitHub {what}", clean(s, 60))) }
}

fn check_tag(s: &str) -> Result<(), String> {
    let ok = !s.is_empty() && s.len() <= 100 && !s.contains("..") && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./+".contains(&b));
    if ok { Ok(()) } else { Err(format!("`{}` is not a release tag", clean(s, 60))) }
}

/// Reads a repository reference. `None` when `s` is not one, so it is
/// treated as an ordinary path or address.
pub fn parse(s: &str) -> Option<Result<Repo, String>> {
    let s = s.trim();
    let rest = if let Some(r) = s.strip_prefix("github:") {
        r
    } else {
        let r = s.strip_prefix("https://").unwrap_or(s);
        let r = r.strip_prefix("www.").unwrap_or(r);
        let r = r.strip_prefix("github.com/")?;
        // A direct link to one release file is an ordinary download.
        if r.split('/').nth(2) == Some("releases") && r.split('/').nth(3) == Some("download") {
            return None;
        }
        r
    };
    Some(parse_rest(rest))
}

fn parse_rest(rest: &str) -> Result<Repo, String> {
    let (rest, file) = match rest.split_once('#') {
        Some((r, f)) => (r, Some(f.to_string())),
        None => (rest, None),
    };
    let (path, at_tag) = match rest.split_once('@') {
        Some((p, t)) => (p, Some(t.to_string())),
        None => (rest, None),
    };
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    let (owner, repo, tag) = match parts.as_slice() {
        [o, r] => (*o, *r, at_tag),
        // github.com/owner/repo/releases/tag/v1.2.0
        [o, r, "releases", "tag", t] if at_tag.is_none() => (*o, *r, Some(t.to_string())),
        [o, r, "releases"] | [o, r, "releases", "latest"] => (*o, *r, at_tag),
        _ => return Err("write a repository as github:owner/repo, optionally with @tag".into()),
    };
    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    check_part(owner, "owner")?;
    check_part(repo, "repository")?;
    if let Some(t) = &tag {
        check_tag(t)?;
    }
    if let Some(f) = &file {
        check_part(f, "release file name")?;
    }
    Ok(Repo { owner: owner.into(), repo: repo.into(), tag, file })
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Clone, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Which file of a release is the package: the one named, else the only
/// extension, else the only skill or pack.
fn pick<'a>(names: &[&'a str], wanted: Option<&str>) -> Result<&'a str, String> {
    if let Some(w) = wanted {
        return names.iter().find(|n| **n == w).copied().ok_or_else(|| format!("the release has no file named {}", clean(w, 80)));
    }
    let ext: Vec<&str> = names.iter().copied().filter(|n| n.ends_with(crate::extension::PACKAGE_SUFFIX)).collect();
    let other: Vec<&str> = names.iter().copied().filter(|n| n.ends_with(".md") || n.ends_with(".json")).collect();
    let group = if ext.is_empty() { other } else { ext };
    match group.as_slice() {
        [one] => Ok(one),
        [] => Err("the release has no Plonix package: attach a .plonixext extension, a skill (.md) or a pack (.json) to it".into()),
        many => Err(format!("the release has several packages ({}); pick one by adding #<file name>", many.join(", "))),
    }
}

fn release(r: &Repo) -> Result<Release> {
    let url = match &r.tag {
        Some(t) => format!("{}/repos/{}/{}/releases/tags/{t}", api_base(), r.owner, r.repo),
        None => format!("{}/repos/{}/{}/releases/latest", api_base(), r.owner, r.repo),
    };
    let loc = registry::location(&url).map_err(|e| anyhow!(e))?;
    let bytes = registry::fetch(&loc, MAX_RELEASE_BYTES).map_err(|e| {
        let e = format!("{e:#}");
        if e.contains("HTTP 404") {
            match &r.tag {
                Some(t) => anyhow!("{} has no release {t}", r.page()),
                None => anyhow!("{} has no published release (or is not public)", r.page()),
            }
        } else {
            anyhow!("reading the releases of {}: {e}", r.page())
        }
    })?;
    let rel: Release = serde_json::from_slice(&bytes).map_err(|e| anyhow!("GitHub answered with something unexpected: {}", clean(&e.to_string(), 200)))?;
    if rel.draft {
        bail!("that release is a draft");
    }
    check_tag(&rel.tag_name).map_err(|e| anyhow!(e))?;
    Ok(rel)
}

/// The newest release's tag.
pub fn latest_tag(r: &Repo) -> Result<String> {
    Ok(release(&r.latest())?.tag_name)
}

/// Downloads the package file of a release. Returns its bytes and the
/// `github:owner/repo@tag` it is recorded as.
pub fn download(r: &Repo, max: usize) -> Result<(Vec<u8>, String)> {
    let rel = release(r)?;
    let names: Vec<&str> = rel.assets.iter().map(|a| a.name.as_str()).collect();
    let name = pick(&names, r.file.as_deref()).map_err(|e| anyhow!("{}@{}: {e}", r.page(), rel.tag_name))?;
    let asset = rel.assets.iter().find(|a| a.name == name).expect("picked from the list");
    let loc = registry::location(&asset.browser_download_url).map_err(|e| anyhow!(e))?;
    if !matches!(loc, Location::Url(_)) {
        bail!("GitHub gave a download address that is not https");
    }
    let bytes = registry::fetch(&loc, max)?;
    let label = Repo { file: r.file.clone(), ..r.clone() }.label(&rel.tag_name);
    Ok((bytes, label))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(s: &str) -> Repo {
        parse(s).expect("a repository").expect("valid")
    }

    #[test]
    fn reads_the_ways_people_write_a_repository() {
        let plain = Repo { owner: "jsmith".into(), repo: "graphql-notes".into(), tag: None, file: None };
        for s in ["github:jsmith/graphql-notes", "github.com/jsmith/graphql-notes", "https://github.com/jsmith/graphql-notes/", "https://www.github.com/jsmith/graphql-notes.git"] {
            assert_eq!(repo(s), plain, "{s}");
        }
        assert_eq!(repo("github:jsmith/graphql-notes@v1.2.0").tag.as_deref(), Some("v1.2.0"));
        assert_eq!(repo("https://github.com/jsmith/graphql-notes/releases/tag/v1.2.0").tag.as_deref(), Some("v1.2.0"));
        let f = repo("github:jsmith/kit@v2#notes.plonixext");
        assert_eq!((f.tag.as_deref(), f.file.as_deref()), (Some("v2"), Some("notes.plonixext")));
        assert_eq!(f.label("v2"), "github:jsmith/kit@v2#notes.plonixext");
        assert_eq!(repo(&f.label("v2")), f);
    }

    #[test]
    fn leaves_other_addresses_alone_and_refuses_odd_ones() {
        assert!(parse("https://example.com/x.plonixext").is_none());
        assert!(parse("./my-extension").is_none());
        assert!(parse("https://github.com/jsmith/kit/releases/download/v1/kit.plonixext").is_none());
        for bad in ["github:jsmith", "github:jsmith/kit/extra", "github:../kit", "github:jsmith/kit@../x", "github:js mith/kit", "github:jsmith/kit#../x"] {
            assert!(parse(bad).unwrap().is_err(), "{bad}");
        }
    }

    #[test]
    fn picks_the_package_file_of_a_release() {
        assert_eq!(pick(&["notes.plonixext", "README.md", "source.zip"], None), Ok("notes.plonixext"));
        assert_eq!(pick(&["triage.md", "source.zip"], None), Ok("triage.md"));
        assert!(pick(&["a.plonixext", "b.plonixext"], None).unwrap_err().contains("#<file name>"));
        assert_eq!(pick(&["a.plonixext", "b.plonixext"], Some("b.plonixext")), Ok("b.plonixext"));
        assert!(pick(&["source.zip"], None).unwrap_err().contains("no Plonix package"));
    }
}
