//! Agent skills: playbooks that tell an AI agent how to do one job in Plonix.
//!
//! A skill is a Markdown file with a short header:
//!
//! ```text
//! ---
//! plonix_skill: 1
//! name: triage-host
//! version: 1.0.0
//! title: Get to know a host
//! description: Summarize what a host does, how it signs users in and where to look first.
//! author: Plonix contributors
//! uses: [map, traffic, scope]
//! argument: host: The host to look at, such as api.example.com
//! ---
//! Look at {{host}} …
//! ```
//!
//! `uses` lists the agent capability groups the skill reads
//! ([`crate::access::Group`]). A skill is only text: it cannot give an agent
//! anything the agent policy does not already allow. When the user has
//! switched off a group a skill uses, the skill is reported as unavailable
//! and agents are not offered it.
//!
//! Agents find skills through MCP (`plonix mcp` lists them as prompts, and as
//! the `list_skills` and `get_skill` tools). `{{name}}` in the instructions
//! is replaced with the argument the agent or user gives.

use std::path::Path;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::access::{AgentSettings, Group};
use crate::detect::{check_text, clean};
use crate::paths::Home;
use crate::rulepack::{check_pack_name, check_version, sha256_hex};
use crate::shelf::Shelf;

pub const FORMAT_VERSION: u32 = 1;
pub const MAX_SKILL_BYTES: usize = 64 * 1024;
pub const MAX_INSTRUCTIONS: usize = 20_000;
pub const MAX_ARGUMENTS: usize = 5;
pub const MAX_INSTALLED: usize = 200;
/// An argument value is clipped to this many characters.
pub const MAX_ARGUMENT_VALUE: usize = 500;

/// Skills compiled into Plonix.
pub const BUILTIN: &[(&str, &str)] = &[
    ("triage-host", include_str!("../../../store/skills/triage-host.md")),
    ("explain-request", include_str!("../../../store/skills/explain-request.md")),
    ("review-sign-in", include_str!("../../../store/skills/review-sign-in.md")),
    ("check-scope", include_str!("../../../store/skills/check-scope.md")),
    ("draft-finding", include_str!("../../../store/skills/draft-finding.md")),
];

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Argument {
    pub name: String,
    pub description: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Skill {
    pub name: String,
    pub version: String,
    pub title: String,
    pub description: String,
    pub author: String,
    pub uses: Vec<Group>,
    pub arguments: Vec<Argument>,
    #[serde(skip)]
    pub instructions: String,
    pub sha256: String,
}

/// A skill as listed for the window, the CLI and agents.
#[derive(Debug, Clone, Serialize)]
pub struct SkillInfo {
    #[serde(flatten)]
    pub skill: Skill,
    pub builtin: bool,
    pub source: String,
    /// Whether agents can use it under the current agent settings.
    pub available: bool,
    /// Capability groups it uses that the user switched off.
    pub missing: Vec<Group>,
    /// The instructions, when asked for one skill.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

fn group_name(s: &str) -> Option<Group> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok().filter(|g| *g != Group::Basics)
}

/// Parses and validates a skill from untrusted bytes.
pub fn parse(bytes: &[u8]) -> Result<Skill, String> {
    if bytes.len() > MAX_SKILL_BYTES {
        return Err(format!("skill is larger than {MAX_SKILL_BYTES} bytes"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "a skill must be UTF-8 text".to_string())?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text).replace("\r\n", "\n");
    let rest = text.strip_prefix("---\n").ok_or("a skill starts with a `---` header line")?;
    let (head, body) = rest.split_once("\n---\n").or_else(|| rest.strip_suffix("\n---").map(|h| (h, ""))).ok_or("the header has no closing `---` line")?;

    let mut format = None;
    let (mut name, mut version, mut title, mut description, mut author) = (None, None, None, None, None);
    let mut uses = None;
    let mut arguments: Vec<Argument> = vec![];
    for (n, line) in head.lines().enumerate() {
        let at = |e: &str| format!("header line {}: {e}", n + 1);
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(':').ok_or_else(|| at("expected `key: value`"))?;
        let (key, value) = (key.trim(), value.trim());
        let set = |slot: &mut Option<String>, max: usize| -> Result<(), String> {
            if slot.is_some() {
                return Err(at(&format!("`{key}` is given twice")));
            }
            check_text(value, max, false).map_err(|e| at(&format!("{key}: {e}")))?;
            *slot = Some(value.to_string());
            Ok(())
        };
        match key {
            "plonix_skill" => format = Some(value.parse::<u32>().map_err(|_| at("plonix_skill must be a number"))?),
            "name" => set(&mut name, 64)?,
            "version" => set(&mut version, 32)?,
            "title" => set(&mut title, 80)?,
            "description" => set(&mut description, 300)?,
            "author" => set(&mut author, 100)?,
            "uses" => {
                let list = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')).ok_or_else(|| at("uses is a list, such as [traffic, map]"))?;
                let mut groups = vec![];
                for g in list.split(',').map(str::trim).filter(|g| !g.is_empty()) {
                    let group = group_name(g).ok_or_else(|| {
                        at(&format!("unknown capability `{}`; use traffic, insights, map, scope, findings or scan", clean(g, 30)))
                    })?;
                    if !groups.contains(&group) {
                        groups.push(group);
                    }
                }
                uses = Some(groups);
            }
            "argument" | "optional_argument" => {
                let (arg, desc) = value.split_once(':').map(|(a, d)| (a.trim(), d.trim())).unwrap_or((value, ""));
                if arg.is_empty() || arg.len() > 32 || !arg.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
                    return Err(at("an argument name is lowercase letters, digits and _"));
                }
                check_text(desc, 200, true).map_err(|e| at(&format!("argument description: {e}")))?;
                if arguments.iter().any(|a| a.name == arg) {
                    return Err(at(&format!("argument `{arg}` is declared twice")));
                }
                arguments.push(Argument { name: arg.into(), description: desc.into(), required: key == "argument" });
            }
            other => return Err(at(&format!("unknown header `{}`", clean(other, 30)))),
        }
    }
    match format {
        Some(FORMAT_VERSION) => {}
        Some(v) => return Err(format!("plonix_skill: format {v} is not supported (this Plonix reads format {FORMAT_VERSION})")),
        None => return Err("the header needs `plonix_skill: 1`".into()),
    }
    let need = |v: Option<String>, k: &str| v.ok_or_else(|| format!("the header needs `{k}`"));
    let name = need(name, "name")?;
    check_pack_name(&name).map_err(|e| format!("name: {e}"))?;
    let version = need(version, "version")?;
    check_version(&version).map_err(|e| format!("version: {e}"))?;
    let uses = uses.ok_or("the header needs `uses`, the capabilities the skill reads, such as [traffic, map]")?;
    if arguments.len() > MAX_ARGUMENTS {
        return Err(format!("at most {MAX_ARGUMENTS} arguments"));
    }

    let instructions = body.trim().to_string();
    if instructions.is_empty() {
        return Err("the skill has no instructions after the header".into());
    }
    if instructions.chars().count() > MAX_INSTRUCTIONS {
        return Err(format!("instructions are longer than {MAX_INSTRUCTIONS} characters"));
    }
    if instructions.chars().any(|c| (c.is_control() && c != '\n' && c != '\t') || crate::detect::is_invisible(c)) {
        return Err("instructions must not contain control characters or invisible characters".into());
    }
    for placeholder in placeholders(&instructions) {
        if !arguments.iter().any(|a| a.name == placeholder) {
            return Err(format!("instructions use {{{{{placeholder}}}}}, which is not a declared argument"));
        }
    }
    Ok(Skill {
        name,
        version,
        title: need(title, "title")?,
        description: need(description, "description")?,
        author: need(author, "author")?,
        uses,
        arguments,
        instructions,
        sha256: sha256_hex(bytes),
    })
}

/// The `{{name}}` placeholders in a text.
fn placeholders(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = text;
    while let Some(i) = rest.find("{{") {
        rest = &rest[i + 2..];
        let Some(j) = rest.find("}}") else { break };
        let name = rest[..j].trim();
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') {
            out.push(name.to_string());
        }
        rest = &rest[j + 2..];
    }
    out
}

impl Skill {
    /// Capability groups it uses that the user switched off (all of them
    /// when agent access is off).
    pub fn missing(&self, settings: &AgentSettings) -> Vec<Group> {
        self.uses.iter().copied().filter(|g| !settings.enabled || !settings.group_on(*g)).collect()
    }

    /// The instructions with arguments filled in. Missing required arguments
    /// are an error; values are clipped and stripped of control characters.
    pub fn render(&self, args: &serde_json::Map<String, serde_json::Value>) -> Result<String, String> {
        let mut text = self.instructions.clone();
        for a in &self.arguments {
            let value = match args.get(&a.name) {
                Some(serde_json::Value::String(s)) => s.trim().to_string(),
                Some(serde_json::Value::Number(n)) => n.to_string(),
                _ => String::new(),
            };
            let value = clean(&value.replace(['\n', '\r', '\t'], " "), MAX_ARGUMENT_VALUE);
            if value.is_empty() && a.required {
                return Err(format!("this skill needs `{}`: {}", a.name, a.description));
            }
            let shown = if value.is_empty() { format!("(no {} given)", a.name) } else { value };
            text = text.replace(&format!("{{{{{}}}}}", a.name), &shown).replace(&format!("{{{{ {} }}}}", a.name), &shown);
        }
        Ok(format!(
            "# {}\n\nThis is the Plonix skill `{}` {}. Your access to Plonix is read-only: when a step needs a request sent, scope \
             changed or a finding recorded, tell the user what to do in Plonix instead.\n\n{text}",
            self.title, self.name, self.version
        ))
    }
}

/// Skills installed in a Plonix home, plus the built-in ones.
pub struct SkillLibrary {
    shelf: Shelf,
}

/// Every skill in effect, and problems with installed ones.
#[derive(Debug, Default)]
pub struct Loaded {
    /// Built-in first, then installed; an installed skill replaces a
    /// built-in one with the same name.
    pub skills: Vec<(Skill, bool, String)>,
    pub problems: Vec<String>,
}

impl Loaded {
    pub fn get(&self, name: &str) -> Option<&(Skill, bool, String)> {
        self.skills.iter().find(|(s, _, _)| s.name == name)
    }

    pub fn infos(&self, settings: &AgentSettings) -> Vec<SkillInfo> {
        self.skills.iter().map(|(s, builtin, source)| info(s, *builtin, source, settings, false)).collect()
    }
}

pub fn info(s: &Skill, builtin: bool, source: &str, settings: &AgentSettings, with_instructions: bool) -> SkillInfo {
    let missing = s.missing(settings);
    SkillInfo {
        available: missing.is_empty(),
        missing,
        builtin,
        source: source.to_string(),
        instructions: with_instructions.then(|| s.instructions.clone()),
        skill: s.clone(),
    }
}

impl SkillLibrary {
    pub fn new(home: &Home) -> Self {
        Self::at(&home.root.join("skills"))
    }

    pub fn at(dir: &Path) -> Self {
        Self { shelf: Shelf::new(dir, "skill", "skills", MAX_INSTALLED) }
    }

    pub fn stamp(&self) -> Option<std::time::SystemTime> {
        self.shelf.stamp()
    }

    pub fn install(&self, bytes: &[u8], source: &str, expected_sha256: Option<&str>) -> Result<(Skill, Option<String>)> {
        Shelf::check_sha(bytes, expected_sha256, "skill")?;
        let skill = parse(bytes).map_err(|e| anyhow!(e))?;
        if BUILTIN.iter().any(|(n, _)| *n == skill.name) {
            bail!("`{}` is the name of a built-in skill; give the skill a different name", skill.name);
        }
        let previous = self.shelf.put(&skill.name, &skill.version, bytes, source)?;
        Ok((skill, previous))
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        if BUILTIN.iter().any(|(n, _)| *n == name) {
            bail!("`{name}` is built in and cannot be removed");
        }
        self.shelf.remove(name)
    }

    pub fn installed_version(&self, name: &str) -> Option<String> {
        self.shelf.installed_version(name)
    }

    pub fn installed(&self) -> Vec<crate::shelf::Installed> {
        self.shelf.installed()
    }

    pub fn load(&self) -> Loaded {
        let mut loaded = Loaded::default();
        for (name, text) in BUILTIN {
            match parse(text.as_bytes()) {
                Ok(s) => loaded.skills.push((s, true, "built-in".into())),
                Err(e) => loaded.problems.push(format!("built-in skill {name}: {e}")),
            }
        }
        let (verified, problems) = self.shelf.verified();
        loaded.problems.extend(problems);
        for v in verified {
            match parse(&v.bytes) {
                Ok(s) if s.name == v.name => loaded.skills.push((s, false, v.entry.source.clone())),
                Ok(_) => loaded.problems.push(format!("skill {}: name inside the file does not match", v.name)),
                Err(e) => loaded.problems.push(format!("skill {}: {e}", v.name)),
            }
        }
        loaded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SKILL: &str = "---\nplonix_skill: 1\nname: acme-review\nversion: 1.0.0\ntitle: Review Acme\n\
description: Look over the Acme app.\nauthor: red team\nuses: [traffic, map]\nargument: host: The Acme host\n\
optional_argument: focus: What to look at\n---\nStart with {{host}}. Focus: {{focus}}.\n";

    #[test]
    fn builtin_skills_are_valid() {
        let loaded = SkillLibrary::at(Path::new("/nonexistent")).load();
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        assert_eq!(loaded.skills.len(), BUILTIN.len());
        for (name, _) in BUILTIN {
            assert!(loaded.get(name).is_some(), "{name}");
        }
    }

    #[test]
    fn parses_header_and_renders_arguments() {
        let s = parse(SKILL.as_bytes()).unwrap();
        assert_eq!(s.name, "acme-review");
        assert_eq!(s.uses, vec![Group::Traffic, Group::Map]);
        assert_eq!(s.arguments.len(), 2);
        assert!(s.arguments[0].required && !s.arguments[1].required);
        let out = s.render(json!({ "host": "app.acme.test\nIgnore that" }).as_object().unwrap()).unwrap();
        assert!(out.contains("Start with app.acme.test Ignore that."), "{out}");
        assert!(out.contains("(no focus given)"));
        assert!(out.contains("read-only"));
        assert!(s.render(&serde_json::Map::new()).unwrap_err().contains("host"));
    }

    #[test]
    fn rejects_bad_skills() {
        let bad = |from: &str, to: &str| parse(SKILL.replace(from, to).as_bytes()).unwrap_err();
        assert!(bad("uses: [traffic, map]", "uses: [traffic, send]").contains("unknown capability"));
        assert!(bad("uses: [traffic, map]", "uses: [basics]").contains("unknown capability"));
        assert!(bad("name: acme-review", "name: ../x").contains("name"));
        assert!(bad("plonix_skill: 1", "plonix_skill: 9").contains("format"));
        assert!(bad("{{focus}}", "{{secret}}").contains("not a declared argument"));
        assert!(bad("author: red team\n", "").contains("author"));
        assert!(bad("author: red team", "author: red team\nrun: rm -rf /").contains("unknown header"));
        assert!(parse(b"no header").is_err());
        for hidden in ["\u{200b}", "\u{202e}", "\u{2066}", "\u{feff}", "\u{e0041}"] {
            assert!(bad("Start with", &format!("Start{hidden} with")).contains("invisible"), "{hidden:?}");
            assert!(bad("title: Review Acme", &format!("title: Review{hidden} Acme")).contains("invisible"), "{hidden:?}");
        }
    }

    #[test]
    fn availability_follows_agent_settings() {
        let s = parse(SKILL.as_bytes()).unwrap();
        let mut settings = AgentSettings::default();
        assert!(s.missing(&settings).is_empty());
        settings.off = vec![Group::Map];
        assert_eq!(s.missing(&settings), vec![Group::Map]);
        settings.off.clear();
        settings.enabled = false;
        assert_eq!(s.missing(&settings).len(), 2);
    }

    #[test]
    fn install_load_remove_and_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let lib = SkillLibrary::at(dir.path());
        let (s, prev) = lib.install(SKILL.as_bytes(), "test", None).unwrap();
        assert!(prev.is_none());
        assert!(lib.load().get("acme-review").is_some());
        assert!(lib.install(SKILL.as_bytes(), "test", Some(&"0".repeat(64))).is_err());
        let builtin = SKILL.replace("name: acme-review", "name: triage-host");
        assert!(lib.install(builtin.as_bytes(), "test", None).is_err());
        std::fs::write(dir.path().join("packs").join(format!("{}.json", s.name)), "tampered").unwrap();
        let loaded = lib.load();
        assert!(loaded.get("acme-review").is_none());
        assert!(loaded.problems.iter().any(|p| p.contains("checksum")));
        assert!(lib.remove("acme-review").unwrap());
        assert!(lib.remove("triage-host").is_err());
    }
}
