//! Out-of-scope exclusions, grouped.
//!
//! An exclusion is just a [`Decision::Rejected`] scope rule: the engine never
//! prompts for a rejected host and never sends to it. This module adds a thin
//! layer on top so those rejections can be managed in named groups — a curated
//! set of common third parties (analytics, ads, payments, …) and groups the
//! user defines themselves.
//!
//! Group membership is tracked in the rule's `note` as `group:<id>`, so no new
//! table is needed for the built-in groups and a group can be toggled as a set.

use serde::{Deserialize, Serialize};

use crate::scope::{Decision, Rule, ScopeRules, normalize_host};

/// Marks a scope rule as belonging to an exclusion group.
pub const NOTE_PREFIX: &str = "group:";

/// A named set of domains that can be excluded together.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Group {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub domains: Vec<String>,
    /// Built-in groups ship with Plonix; custom groups are user-defined.
    #[serde(default)]
    pub builtin: bool,
}

/// The curated built-in groups, shipped with the binary.
pub fn builtin_groups() -> Vec<Group> {
    let mut groups: Vec<Group> = serde_json::from_str(include_str!("exclude_groups.json")).expect("built-in exclude_groups.json is valid");
    for g in &mut groups {
        g.builtin = true;
        g.domains = g.domains.iter().map(|d| normalize_host(d)).collect();
    }
    groups
}

/// The note stored on a rule created for group `id`.
pub fn group_note(id: &str) -> String {
    format!("{NOTE_PREFIX}{id}")
}

/// The group id a rule belongs to, if it was created for one.
pub fn rule_group(rule: &Rule) -> Option<&str> {
    rule.note.strip_prefix(NOTE_PREFIX).filter(|s| !s.is_empty())
}

/// Whether `host` is currently excluded by a group rule (a `Rejected` rule the
/// exclusion layer owns for exactly this host).
pub fn is_excluded(rules: &ScopeRules, host: &str) -> bool {
    let host = normalize_host(host);
    rules.rules.iter().any(|r| r.pattern == host && r.decision == Decision::Rejected && rule_group(r).is_some())
}

/// How much of a group is switched on.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum State {
    On,
    Partial,
    Off,
}

/// The state of a group's domains against the current rules.
pub fn group_state(rules: &ScopeRules, g: &Group) -> State {
    let on = g.domains.iter().filter(|d| is_excluded(rules, d)).count();
    if on == 0 {
        State::Off
    } else if on == g.domains.len() {
        State::On
    } else {
        State::Partial
    }
}

/// Builds the rule that excludes one host as part of a group.
pub fn exclusion_rule(host: &str, group_id: &str, now: i64) -> Rule {
    Rule { pattern: normalize_host(host), include_subdomains: true, decision: Decision::Rejected, created_at: now, note: group_note(group_id) }
}

/// A stable, url-safe id for a custom group, derived from an explicit id or
/// else the name. Lowercase, with runs of non-alphanumerics folded to `-`.
pub fn normalize_group_id(id: &str, name: &str) -> String {
    let source = if id.trim().is_empty() { name } else { id };
    let mut out = String::new();
    let mut dash = false;
    for c in source.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            dash = false;
        } else if !out.is_empty() && !dash {
            out.push('-');
            dash = true;
        }
    }
    let trimmed = out.trim_matches('-');
    trimmed.chars().take(40).collect()
}

/// One domain in a group and whether it is currently excluded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExcludedDomain {
    pub host: String,
    pub excluded: bool,
}

/// A group with its live on/off state, for the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroupStatus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub builtin: bool,
    pub state: State,
    pub domains: Vec<ExcludedDomain>,
}

/// The full exclusions snapshot: every group, and whether the one-time prompt
/// has been answered yet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Exclusions {
    pub groups: Vec<GroupStatus>,
    pub asked: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_catalog_loads_and_is_normalized() {
        let groups = builtin_groups();
        assert!(groups.len() >= 6);
        assert!(groups.iter().all(|g| g.builtin && !g.domains.is_empty()));
        // Every curated domain is a bare, lowercased host with no scheme/port.
        for g in &groups {
            for d in &g.domains {
                assert_eq!(*d, normalize_host(d), "domain {d} should already be normalized");
            }
        }
        let analytics = groups.iter().find(|g| g.id == "analytics").unwrap();
        assert!(analytics.domains.iter().any(|d| d == "google-analytics.com"));
        let tools = groups.iter().find(|g| g.id == "tools").unwrap();
        assert!(tools.domains.iter().any(|d| d == "retool.com"));
    }

    #[test]
    fn group_state_reflects_the_rules() {
        let g = Group {
            id: "analytics".into(),
            name: "Analytics".into(),
            description: String::new(),
            domains: vec!["a.com".into(), "b.com".into()],
            builtin: true,
        };
        let mut rules = ScopeRules::default();
        assert_eq!(group_state(&rules, &g), State::Off);
        rules.rules.push(exclusion_rule("a.com", "analytics", 1));
        assert_eq!(group_state(&rules, &g), State::Partial);
        assert!(is_excluded(&rules, "a.com"));
        rules.rules.push(exclusion_rule("b.com", "analytics", 2));
        assert_eq!(group_state(&rules, &g), State::On);
        // A manual rejection (no group note) does not count as a group exclusion.
        let mut manual = ScopeRules::default();
        manual.rules.push(Rule {
            pattern: "a.com".into(),
            include_subdomains: false,
            decision: Decision::Rejected,
            created_at: 1,
            note: String::new(),
        });
        assert!(!is_excluded(&manual, "a.com"));
    }
}
