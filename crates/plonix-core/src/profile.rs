//! Starter profiles: the kind of work someone does, and the Market items
//! that suit it.
//!
//! A profile never names items. It weighs tags, and every Market item
//! carries a few tags and a noise level (`store/profiles.json`), so a new
//! item shows up for the right people as soon as it is tagged. The ranking
//! is [`recommend`]; it is pure and works on any list of candidates.

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::extension::Consent;
use crate::market::{Catalog, Change, Market, Status};
use crate::paths::Home;
use crate::project::Project;
use crate::registry::Kind;
use crate::settings::{self, Field, Level, Section};

/// The profiles, the tag list and every item's tags, shipped with Plonix.
pub const DATA: &str = include_str!("../../../store/profiles.json");

/// Items that score below this are not for the profile.
pub const MIN_SCORE: i32 = 4;
/// Installable items ticked in a starter set.
pub const STARTER: usize = 5;
/// Items already in Plonix shown alongside the starter set.
pub const INCLUDED: usize = 3;
/// Items listed under "Also for you".
pub const ALSO: usize = 3;

/// How much traffic an item sends to the target on its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Noise {
    /// Reads captured traffic only.
    #[default]
    Passive,
    /// Sends a few requests when you start it.
    Light,
    /// Sends many requests by itself.
    Active,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub title: String,
    /// One sentence: who this is for.
    pub line: String,
    pub weights: BTreeMap<String, i32>,
    /// Items that always go first when they are offered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pin: Vec<String>,
    /// Noisier items are suggested but never ticked.
    pub max_noise: Noise,
    /// Why a tag matters to this profile, used when an item has no line of its own.
    #[serde(default)]
    pub reasons: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemInfo {
    pub tags: Vec<String>,
    #[serde(default)]
    pub noise: Noise,
    /// The item's own reason, per profile.
    #[serde(default)]
    pub why: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {
    pub plonix_profiles: u32,
    pub tags: BTreeMap<String, String>,
    pub profiles: Vec<Profile>,
    pub items: BTreeMap<String, ItemInfo>,
}

/// Parses and checks profile data: every tag used is in the tag list, ids are
/// unique, and every profile can explain each tag it weighs.
pub fn parse(text: &str) -> Result<Data, String> {
    let d: Data = serde_json::from_str(text).map_err(|e| format!("profiles: {e}"))?;
    if d.plonix_profiles != 1 {
        return Err(format!("profiles: format {} is not supported", d.plonix_profiles));
    }
    let known = |t: &String| d.tags.contains_key(t);
    let mut ids = HashSet::new();
    for p in &d.profiles {
        if !ids.insert(&p.id) {
            return Err(format!("profile {} is listed twice", p.id));
        }
        for t in p.weights.keys() {
            if !known(t) {
                return Err(format!("profile {}: unknown tag `{t}`", p.id));
            }
            if !p.reasons.contains_key(t) {
                return Err(format!("profile {}: no reason for tag `{t}`", p.id));
            }
        }
    }
    for (name, it) in &d.items {
        if let Some(t) = it.tags.iter().find(|t| !known(t)) {
            return Err(format!("item {name}: unknown tag `{t}`"));
        }
        if let Some(p) = it.why.keys().find(|p| !ids.contains(p)) {
            return Err(format!("item {name}: no profile `{p}`"));
        }
    }
    Ok(d)
}

pub fn data() -> &'static Data {
    static D: OnceLock<Data> = OnceLock::new();
    D.get_or_init(|| parse(DATA).expect("store/profiles.json is checked by the tests"))
}

pub fn profiles() -> &'static [Profile] {
    &data().profiles
}

pub fn get(id: &str) -> Option<&'static Profile> {
    profiles().iter().find(|p| p.id == id)
}

/// Who a profile is for, without its weights.
pub fn summaries() -> Value {
    json!(profiles().iter().map(|p| json!({ "id": p.id, "title": p.title, "line": p.line })).collect::<Vec<_>>())
}

// ---- ranking -----------------------------------------------------------------------

/// Where a candidate stands for this user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Have {
    /// Can be installed.
    Available,
    /// Already installed.
    Installed,
    /// Ships inside Plonix.
    BuiltIn,
}

/// One Market item, as far as ranking needs to know.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub kind: Kind,
    pub have: Have,
    /// For a bundle: the items it installs.
    pub includes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Pick {
    pub name: String,
    pub kind: Kind,
    pub have: Have,
    pub why: String,
    pub score: i32,
    pub tags: Vec<String>,
    pub noise: Noise,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Recommendation {
    /// Ticked for the user: installable, quiet enough, best first.
    pub starter: Vec<Pick>,
    /// Already in Plonix or already installed.
    pub included: Vec<Pick>,
    /// Good matches left unticked: too noisy, or past the starter size.
    pub also: Vec<Pick>,
}

/// Ranks candidates for a profile. Items without tags are left out.
///
/// 1. Score: the sum of the profile's weight for each of the item's tags;
///    below [`MIN_SCORE`] it is not for this profile.
/// 2. Pinned items first, then highest score, then catalog order.
/// 3. A bundle replaces the items inside it, unless one of them already
///    scored higher on its own; then the bundle goes under "also".
/// 4. Items noisier than the profile allows go under "also".
pub fn recommend(profile: &Profile, items: &BTreeMap<String, ItemInfo>, candidates: &[Candidate]) -> Recommendation {
    let mut scored: Vec<(usize, &Candidate, &ItemInfo, i32, Option<&String>)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let info = items.get(&c.name)?;
            let mut score = 0;
            let mut best: Option<(&String, i32)> = None;
            for t in &info.tags {
                let w = profile.weights.get(t).copied().unwrap_or(0);
                score += w;
                if w > 0 && best.is_none_or(|(_, bw)| w > bw) {
                    best = Some((t, w));
                }
            }
            (score >= MIN_SCORE).then_some((i, c, info, score, best.map(|b| b.0)))
        })
        .collect();
    let pinned = |c: &Candidate| profile.pin.contains(&c.name);
    scored.sort_by(|a, b| pinned(b.1).cmp(&pinned(a.1)).then(b.3.cmp(&a.3)).then(a.0.cmp(&b.0)));

    let mut out = Recommendation::default();
    let mut covered: HashSet<&str> = HashSet::new();
    for (_, c, info, score, best) in scored {
        if covered.contains(c.name.as_str()) {
            continue;
        }
        let why = info.why.get(&profile.id).or_else(|| best.and_then(|t| profile.reasons.get(t))).cloned().unwrap_or_default();
        let pick = Pick { name: c.name.clone(), kind: c.kind, have: c.have, why, score, tags: info.tags.clone(), noise: info.noise };
        if c.have != Have::Available {
            out.included.push(pick);
            continue;
        }
        if info.noise > profile.max_noise || out.starter.len() >= STARTER {
            out.also.push(pick);
            continue;
        }
        if !c.includes.is_empty() {
            if out.starter.iter().any(|p| c.includes.contains(&p.name) && p.score > score) {
                out.also.push(pick);
                continue;
            }
            out.starter.retain(|p| !c.includes.contains(&p.name));
            covered.extend(c.includes.iter().map(String::as_str));
        }
        out.starter.push(pick);
    }
    out.included.truncate(INCLUDED);
    out.also.truncate(ALSO);
    out
}

/// The Market's items as candidates.
pub fn candidates(market: &Market, cat: &Catalog) -> Vec<Candidate> {
    market
        .listing(cat)
        .into_iter()
        .filter(|l| !l.local)
        .filter_map(|l| {
            let have = match l.status {
                Status::Available => Have::Available,
                Status::Installed { .. } | Status::Update { .. } => Have::Installed,
                Status::BuiltIn => Have::BuiltIn,
                Status::NeedsRuntime => return None,
            };
            Some(Candidate { name: l.package.name, kind: l.package.kind, have, includes: l.includes })
        })
        .collect()
}

pub fn recommend_from(market: &Market, cat: &Catalog, profile: &Profile) -> Recommendation {
    recommend(profile, &data().items, &candidates(market, cat))
}

/// What installing a starter set did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StarterResult {
    pub changes: Vec<Change>,
    /// Extensions in the set: they install from the Market, where you see what they may do first.
    pub waiting: Vec<Pick>,
    /// Items that could not be installed, with why.
    pub failed: Vec<(String, String)>,
}

/// Installs a profile's starter set. Extensions are left for the user to
/// install from the Market, where they see what each one may do first.
pub fn install_starter(market: &Market, cat: &Catalog, profile: &Profile) -> StarterResult {
    let rec = recommend_from(market, cat, profile);
    let mut out = StarterResult::default();
    for p in rec.starter {
        if p.kind == Kind::Extension {
            out.waiting.push(p);
            continue;
        }
        match market.install_with(cat, &p.name, &Consent::default()) {
            Ok(c) => out.changes.extend(c),
            Err(e) => out.failed.push((p.name, format!("{e:#}"))),
        }
    }
    out
}

// ---- which profile is in effect ------------------------------------------------------

/// Settings › Market › Your work, and the project override.
pub const KEY: &str = "profile";
pub const PROJECT_SECTION: &str = "work";

fn options(none: &str) -> Vec<(String, String)> {
    std::iter::once((String::new(), none.to_string())).chain(profiles().iter().map(|p| (p.id.clone(), p.title.clone()))).collect()
}

/// The field that goes in Settings › Market.
pub fn global_field() -> Field {
    let o = options("Not set");
    let o: Vec<(&str, &str)> = o.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    Field::choice(KEY, "Your work", "", &o).help("Plonix suggests Market items that suit this work. Changing it never removes anything.")
}

/// A project can suit a different kind of work than the rest.
pub fn project_section() -> Section {
    let o = options("Same as Settings › Market");
    let o: Vec<(&str, &str)> = o.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    Section::new(PROJECT_SECTION, "Your work", Level::Project)
        .describe("The kind of work you do in this project, for Market suggestions.")
        .order(90)
        .field(Field::choice(KEY, "Your work in this project", "", &o))
}

/// The profile in effect: the project's, else the one in Settings › Market.
pub fn current(home: &Home, project: Option<&Project>) -> Option<&'static Profile> {
    let id = project.map(|p| p.settings(PROJECT_SECTION)).and_then(|v| v.get(KEY).and_then(Value::as_str).map(str::to_string)).filter(|s| !s.is_empty());
    let id = id.or_else(|| settings::global(home, crate::market::SETTINGS).get(KEY).and_then(Value::as_str).map(str::to_string));
    id.and_then(|id| get(&id))
}

/// Saves the profile in Settings › Market; an empty id clears it.
pub fn set_global(home: &Home, id: &str) -> Result<()> {
    if !id.is_empty() && get(id).is_none() {
        anyhow::bail!("`{}` is not a profile ({})", crate::detect::clean(id, 40), profiles().iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(", "));
    }
    let mut v = settings::global(home, crate::market::SETTINGS);
    v.insert(KEY.into(), json!(id));
    settings::save_global(home, crate::market::SETTINGS, &v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, kind: Kind, have: Have) -> Candidate {
        Candidate { name: name.into(), kind, have, includes: vec![] }
    }

    fn names(p: &[Pick]) -> Vec<&str> {
        p.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn shipped_data_is_valid() {
        let d = parse(DATA).unwrap();
        assert_eq!(d.profiles.len(), 3);
        // Every item in the Market has tags, so new ones are not forgotten.
        let index = crate::registry::parse(crate::market::SNAPSHOT[0].1.as_bytes()).unwrap();
        for p in &index.packages {
            assert!(d.items.contains_key(&p.name), "{} has no entry in store/profiles.json", p.name);
        }
    }

    #[test]
    fn bad_data_is_refused() {
        let ok = r#"{"plonix_profiles":1,"tags":{"a":"A"},"profiles":[{"id":"p","title":"P","line":"l","weights":{"a":4},"max_noise":"passive","reasons":{"a":"why"}}],"items":{"x":{"tags":["a"]}}}"#;
        assert!(parse(ok).is_ok());
        assert!(parse(&ok.replace(r#""tags":["a"]"#, r#""tags":["b"]"#)).unwrap_err().contains("unknown tag"));
        assert!(parse(&ok.replace(r#""reasons":{"a":"why"}"#, r#""reasons":{}"#)).unwrap_err().contains("no reason"));
        assert!(parse(&ok.replace(r#"{"tags":["a"]}"#, r#"{"tags":["a"],"why":{"q":"x"}}"#)).unwrap_err().contains("no profile"));
    }

    #[test]
    fn ranks_by_tags_with_bundles_noise_and_limits() {
        let d = data();
        let bug = get("bug-hunter").unwrap();
        let mut kit = c("api-kit", Kind::Bundle, Have::Available);
        kit.includes = vec!["api-inventory".into(), "leaks".into(), "admin-panels".into()];
        let cands = vec![
            c("secret-sweep", Kind::Extension, Have::Available),
            c("access-check", Kind::Extension, Have::Available),
            c("saved-users", Kind::Extension, Have::Available),
            kit.clone(),
            c("api-inventory", Kind::Skill, Have::Available),
            c("leaks", Kind::Filters, Have::Available),
            c("admin-panels", Kind::Rules, Have::Available),
            c("extra-wordlists", Kind::List, Have::Available),
            c("draft-finding", Kind::Skill, Have::BuiltIn),
            c("parameter-probe", Kind::Extension, Have::Available),
            c("not-tagged", Kind::Skill, Have::Available),
        ];
        let r = recommend(bug, &d.items, &cands);
        assert_eq!(names(&r.starter), ["api-kit", "secret-sweep", "access-check", "saved-users", "extra-wordlists"]);
        assert_eq!(names(&r.included), ["draft-finding"]);
        // Too noisy for a bug hunter's defaults.
        assert!(names(&r.also).contains(&"parameter-probe"));
        // What the bundle installs is not suggested again.
        assert!(!names(&r.starter).contains(&"leaks"));
        assert!(r.starter.iter().all(|p| !p.why.is_empty()));

        // A red teamer: admin-panels scores higher alone than the bundle, so the bundle steps aside.
        let red = get("red-teamer").unwrap();
        let r = recommend(red, &d.items, &cands);
        assert_eq!(r.starter[0].name, "admin-panels");
        assert!(names(&r.also).contains(&"api-kit"));
        assert!(!names(&r.starter).contains(&"access-check"), "light noise is above a red teamer's limit");
        assert_eq!(r.starter[0].why, "Exposed consoles are the fastest way in on a wide target.");

        // Installed items are never ticked again.
        let mut cands2 = cands.clone();
        cands2[0].have = Have::Installed;
        let r = recommend(bug, &d.items, &cands2);
        assert!(!names(&r.starter).contains(&"secret-sweep"));
        assert!(names(&r.included).contains(&"secret-sweep"));
    }

    #[test]
    fn researcher_gets_cloud_and_analysis_first() {
        let d = data();
        let res = get("researcher").unwrap();
        let cands: Vec<Candidate> = ["graphql-explorer", "cloud-identity", "value-trace", "token-randomness", "cloud-map", "script-insights"].iter().map(|n| c(n, Kind::Extension, Have::Available)).collect();
        let r = recommend(res, &d.items, &cands);
        assert_eq!(names(&r.starter), ["cloud-identity", "value-trace", "script-insights", "token-randomness", "cloud-map"]);
        assert_eq!(names(&r.also), ["graphql-explorer"]);
    }
}
