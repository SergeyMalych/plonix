//! Payload runs on the Bench: mark positions in a request, feed one or more
//! lists of values through them, and send the whole set at once.
//!
//! A run is just a lot of Bench sends. Every request it produces goes through
//! [`crate::engine::Engine::send`], the single choke point for active traffic,
//! so a run can only ever reach a host that is accepted into scope, is bounded
//! by a request budget, and is recorded like any other replay. A run never
//! starts on its own — a person launches it — and agents cannot start one (the
//! route is in no agent mode's capabilities).
//!
//! # Positions
//!
//! A position is a span of the request that the run replaces with each value.
//! Positions are marked in the URL and in the raw headers/body with the
//! [`MARKER`] character around the span, e.g. `id=•1•`. Positions are numbered
//! in order: those in the URL first, then those in the raw request.
//!
//! # Modes
//!
//! * [`RunMode::Sweep`] — one position changes at a time while the others keep
//!   their base value. With one list it is used for every position; with one
//!   list per position each position is swept through its own list. This is the
//!   single-position mode.
//! * [`RunMode::Parallel`] — every position changes together, stepping through
//!   its own list by index. The run stops at the shortest list.
//! * [`RunMode::Matrix`] — every combination of values across the positions.
//!
//! [`RunMode::Parallel`] and [`RunMode::Matrix`] are the multi-position modes.

use serde::{Deserialize, Serialize};

use crate::model::Headers;

/// The character that marks a position, placed on both sides of the span.
pub const MARKER: char = '•';

/// Most positions a single run may mark.
pub const MAX_POSITIONS: usize = 20;
/// Most values one list may hold.
pub const MAX_LIST_VALUES: usize = 50_000;
/// Largest raw request a run template may be.
pub const MAX_RAW_BYTES: usize = 1024 * 1024;
/// Default ceiling on how many requests one run may send.
pub const DEFAULT_REQUEST_BUDGET: usize = 1_000;
/// Hard ceiling on the request budget, whatever the caller asks for.
pub const MAX_REQUEST_BUDGET: usize = 50_000;
/// Default pause between requests, so a run stays polite by default.
pub const DEFAULT_DELAY_MS: u64 = 50;
/// Largest pause the caller may set between requests.
pub const MAX_DELAY_MS: u64 = 60_000;

/// How the values are spread across the marked positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum RunMode {
    /// One position changes at a time (single-position).
    #[default]
    Sweep,
    /// Every position changes together, by list index (multi-position).
    Parallel,
    /// Every combination across the positions (multi-position).
    Matrix,
}

/// A list of values for a position.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Payloads {
    /// An explicit list of values.
    Values { values: Vec<String> },
    /// A numeric range, `from` through `to` inclusive, stepping by `step`.
    Range { from: i64, to: i64, #[serde(default = "one")] step: i64 },
    /// A list by id: a built-in list or one installed from the Market.
    Builtin { id: String },
}

fn one() -> i64 {
    1
}

impl Payloads {
    /// Resolves this list to its values, capped at [`MAX_LIST_VALUES`]. A
    /// built-in list is looked up through `named` — the engine's list library,
    /// which holds the lists that ship with Plonix plus any installed from the
    /// Market.
    pub fn resolve(&self, named: &dyn Fn(&str) -> Option<Vec<String>>) -> Result<Vec<String>, String> {
        let values = match self {
            Payloads::Values { values } => values.clone(),
            Payloads::Range { from, to, step } => {
                if *step == 0 {
                    return Err("a range step cannot be zero".into());
                }
                let mut out = Vec::new();
                let mut v = *from;
                // Walk toward `to` in the direction the step points.
                while (*step > 0 && v <= *to) || (*step < 0 && v >= *to) {
                    out.push(v.to_string());
                    if out.len() > MAX_LIST_VALUES {
                        return Err(format!("a range of more than {MAX_LIST_VALUES} values is too large"));
                    }
                    v += *step;
                }
                out
            }
            Payloads::Builtin { id } => named(id).ok_or_else(|| format!("no list named '{id}'"))?,
        };
        if values.len() > MAX_LIST_VALUES {
            return Err(format!("a list of more than {MAX_LIST_VALUES} values is too large"));
        }
        Ok(values)
    }
}

/// What a person asks for when launching a run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunRequest {
    #[serde(default = "get")]
    pub method: String,
    /// The request URL, which may contain marked positions.
    pub url: String,
    /// Raw headers, a blank line, then the body — may contain marked positions.
    #[serde(default)]
    pub raw: String,
    /// The lists to feed through the positions (see [`RunMode`] for how).
    #[serde(default)]
    pub lists: Vec<Payloads>,
    #[serde(default)]
    pub mode: RunMode,
    /// A ceiling on how many requests the run may send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_requests: Option<usize>,
    /// Pause between requests, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    /// Send the unmodified request first, as a baseline to compare against.
    #[serde(default)]
    pub include_base: bool,
}

fn get() -> String {
    "GET".into()
}

/// One request a run sent, and how it came back.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRow {
    /// 1-based order within the run.
    pub n: usize,
    /// The value put in each position for this request.
    pub values: Vec<String>,
    pub exchange_id: i64,
    pub status: Option<u16>,
    /// Response body length in bytes.
    pub length: usize,
    pub duration_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True for the unmodified baseline request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub baseline: bool,
}

/// The outcome of a run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunReport {
    /// How many positions the template marked.
    pub positions: usize,
    /// How many requests the run planned (before the budget was applied).
    pub planned: usize,
    /// How many requests the run actually sent.
    pub requests_sent: usize,
    pub rows: Vec<RunRow>,
    /// True when the budget stopped the run short.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

/// A template parsed into its literal segments and the base value of each
/// marked position. Rebuilding with `n` values interleaves them back:
/// `segments[0] value[0] segments[1] value[1] … segments[n]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    pub segments: Vec<String>,
    pub bases: Vec<String>,
}

impl Template {
    /// Parses a string, treating `•…•` pairs as positions.
    pub fn parse(s: &str) -> Result<Template, String> {
        let mut segments = vec![String::new()];
        let mut bases = Vec::new();
        let mut in_pos = false;
        for ch in s.chars() {
            if ch == MARKER {
                if in_pos {
                    in_pos = false;
                    segments.push(String::new());
                } else {
                    in_pos = true;
                    bases.push(String::new());
                }
            } else if in_pos {
                bases.last_mut().unwrap().push(ch);
            } else {
                segments.last_mut().unwrap().push(ch);
            }
        }
        if in_pos {
            return Err(format!("a position is not closed: every '{MARKER}' needs a matching '{MARKER}'"));
        }
        Ok(Template { segments, bases })
    }

    /// Number of marked positions.
    pub fn positions(&self) -> usize {
        self.bases.len()
    }

    /// Rebuilds the string, filling each position with the matching value.
    /// `values` must have one entry per position.
    pub fn fill(&self, values: &[&str]) -> String {
        let mut out = String::with_capacity(self.segments.iter().map(|s| s.len()).sum());
        for (i, seg) in self.segments.iter().enumerate() {
            out.push_str(seg);
            if let Some(v) = values.get(i) {
                out.push_str(v);
            }
        }
        out
    }
}

/// The URL and raw templates together, with a stable global position order
/// (URL positions first, then raw positions).
#[derive(Debug, Clone)]
pub struct Plan {
    pub url: Template,
    pub raw: Template,
    /// The value lists, one per global position.
    pub lists: Vec<Vec<String>>,
    pub mode: RunMode,
}

impl Plan {
    pub fn positions(&self) -> usize {
        self.url.positions() + self.raw.positions()
    }

    /// The default value for every position, used for the positions a sweep is
    /// not currently changing and for the baseline request.
    pub fn base_values(&self) -> Vec<String> {
        self.url.bases.iter().chain(self.raw.bases.iter()).cloned().collect()
    }

    /// Fills the template from a full set of values (one per position) into a
    /// concrete `(url, raw)` pair.
    pub fn render(&self, values: &[String]) -> (String, String) {
        let n_url = self.url.positions();
        let url_vals: Vec<&str> = values[..n_url].iter().map(|s| s.as_str()).collect();
        let raw_vals: Vec<&str> = values[n_url..].iter().map(|s| s.as_str()).collect();
        (self.url.fill(&url_vals), self.raw.fill(&raw_vals))
    }

    /// The ordered list of value-assignments this run will send, before the
    /// budget is applied. Each assignment has one value per position.
    pub fn assignments(&self) -> Vec<Vec<String>> {
        let p = self.positions();
        let base = self.base_values();
        match self.mode {
            RunMode::Sweep => {
                let mut out = Vec::new();
                for i in 0..p {
                    // One list for all positions, or one list per position.
                    let list = if self.lists.len() == 1 { &self.lists[0] } else { self.lists.get(i).map(|l| l.as_slice()).unwrap_or(&[]) };
                    for v in list {
                        let mut a = base.clone();
                        a[i] = v.clone();
                        out.push(a);
                    }
                }
                out
            }
            RunMode::Parallel => {
                let steps = (0..p).map(|i| self.lists.get(i).map(|l| l.len()).unwrap_or(0)).min().unwrap_or(0);
                (0..steps)
                    .map(|k| (0..p).map(|i| self.lists[i][k].clone()).collect())
                    .collect()
            }
            RunMode::Matrix => {
                let mut out: Vec<Vec<String>> = vec![vec![]];
                for i in 0..p {
                    let list = self.lists.get(i).cloned().unwrap_or_default();
                    let mut next = Vec::new();
                    for prefix in &out {
                        for v in &list {
                            let mut a = prefix.clone();
                            a.push(v.clone());
                            next.push(a);
                        }
                    }
                    out = next;
                    // Guard against a product that would blow past any budget
                    // before we even start sending.
                    if out.len() > MAX_REQUEST_BUDGET {
                        out.truncate(MAX_REQUEST_BUDGET);
                        break;
                    }
                }
                // An empty template (no positions) yields a single assignment.
                if p == 0 { vec![] } else { out }
            }
        }
    }
}

/// Validates a request and builds its [`Plan`]. Does not send anything.
pub fn plan(req: &RunRequest, named: &dyn Fn(&str) -> Option<Vec<String>>) -> Result<Plan, String> {
    if req.url.trim().is_empty() {
        return Err("a run needs a URL".into());
    }
    if req.raw.len() > MAX_RAW_BYTES {
        return Err("the request is too large".into());
    }
    let url = Template::parse(&req.url)?;
    let raw = Template::parse(&req.raw)?;
    let positions = url.positions() + raw.positions();
    if positions == 0 {
        return Err(format!("mark at least one position by wrapping it in '{MARKER}', e.g. id={MARKER}1{MARKER}"));
    }
    if positions > MAX_POSITIONS {
        return Err(format!("too many positions ({positions}); at most {MAX_POSITIONS}"));
    }

    let lists: Vec<Vec<String>> = req.lists.iter().map(|l| l.resolve(named)).collect::<Result<_, _>>()?;
    if lists.is_empty() {
        return Err("choose at least one list of values".into());
    }
    if lists.iter().all(|l| l.is_empty()) {
        return Err("the chosen lists are empty".into());
    }

    match req.mode {
        RunMode::Sweep => {
            if lists.len() != 1 && lists.len() != positions {
                return Err(format!(
                    "single-position mode takes one list for all positions, or one list per position ({positions}); got {}",
                    lists.len()
                ));
            }
        }
        RunMode::Parallel | RunMode::Matrix => {
            if lists.len() != positions {
                return Err(format!("multi-position mode needs one list per position ({positions}); got {}", lists.len()));
            }
        }
    }

    Ok(Plan { url, raw, lists, mode: req.mode })
}

/// Splits a filled raw request into headers and a body. Mirrors the Bench
/// editor: header lines, a blank line, then the body.
pub fn split_raw(raw: &str) -> Result<(Headers, String), String> {
    let text = raw.replace("\r\n", "\n");
    let (head, body) = match text.find("\n\n") {
        Some(i) => (&text[..i], text[i + 2..].to_string()),
        None => (text.as_str(), String::new()),
    };
    let mut headers = Vec::new();
    for line in head.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        match line.find(':') {
            Some(c) if c > 0 => headers.push((line[..c].trim().to_string(), line[c + 1..].trim().to_string())),
            _ => return Err(format!("not a header line: '{line}' (use \"Name: value\", then a blank line before the body)")),
        }
    }
    Ok((headers, body))
}


#[cfg(test)]
mod tests {
    use super::*;

    fn vals(v: &[&str]) -> Payloads {
        Payloads::Values { values: v.iter().map(|s| s.to_string()).collect() }
    }

    /// A resolver with no named lists, for tests that use only Values/Range.
    fn none(_: &str) -> Option<Vec<String>> {
        None
    }

    #[test]
    fn parses_positions_and_fills_them() {
        let t = Template::parse("GET /api?id=•1•&p=•2•").unwrap();
        assert_eq!(t.positions(), 2);
        assert_eq!(t.bases, vec!["1", "2"]);
        assert_eq!(t.fill(&["9", "x"]), "GET /api?id=9&p=x");
        // No positions is a valid template with a single literal segment.
        assert_eq!(Template::parse("plain").unwrap().positions(), 0);
    }

    #[test]
    fn unbalanced_marker_is_an_error() {
        assert!(Template::parse("id=•1").is_err());
    }

    #[test]
    fn sweep_changes_one_position_at_a_time() {
        let req = RunRequest {
            url: "https://h/a?x=•0•&y=•0•".into(),
            lists: vec![vals(&["1", "2"])],
            mode: RunMode::Sweep,
            ..Default::default()
        };
        let p = plan(&req, &none).unwrap();
        let a = p.assignments();
        // 2 positions * 2 values = 4, each with exactly one position off base.
        assert_eq!(a, vec![
            vec!["1".to_string(), "0".to_string()],
            vec!["2".to_string(), "0".to_string()],
            vec!["0".to_string(), "1".to_string()],
            vec!["0".to_string(), "2".to_string()],
        ]);
    }

    #[test]
    fn parallel_steps_lists_together_and_stops_at_shortest() {
        let req = RunRequest {
            url: "https://h/?a=••&b=••".into(),
            lists: vec![vals(&["1", "2", "3"]), vals(&["x", "y"])],
            mode: RunMode::Parallel,
            ..Default::default()
        };
        let a = plan(&req, &none).unwrap().assignments();
        assert_eq!(a, vec![vec!["1".to_string(), "x".to_string()], vec!["2".to_string(), "y".to_string()]]);
    }

    #[test]
    fn matrix_is_every_combination() {
        let req = RunRequest {
            url: "https://h/?a=••&b=••".into(),
            lists: vec![vals(&["1", "2"]), vals(&["x", "y"])],
            mode: RunMode::Matrix,
            ..Default::default()
        };
        let a = plan(&req, &none).unwrap().assignments();
        assert_eq!(a.len(), 4);
        assert!(a.contains(&vec!["1".to_string(), "x".to_string()]));
        assert!(a.contains(&vec!["2".to_string(), "y".to_string()]));
    }

    #[test]
    fn multi_position_mode_needs_one_list_per_position() {
        let req = RunRequest {
            url: "https://h/?a=••&b=••".into(),
            lists: vec![vals(&["1"])],
            mode: RunMode::Parallel,
            ..Default::default()
        };
        assert!(plan(&req, &none).is_err());
    }

    #[test]
    fn a_run_with_no_positions_is_rejected() {
        let req = RunRequest { url: "https://h/".into(), lists: vec![vals(&["1"])], ..Default::default() };
        assert!(plan(&req, &none).is_err());
    }

    #[test]
    fn range_resolves_in_both_directions() {
        assert_eq!(Payloads::Range { from: 1, to: 3, step: 1 }.resolve(&none).unwrap(), vec!["1", "2", "3"]);
        assert_eq!(Payloads::Range { from: 5, to: 1, step: -2 }.resolve(&none).unwrap(), vec!["5", "3", "1"]);
        assert!(Payloads::Range { from: 1, to: 3, step: 0 }.resolve(&none).is_err());
    }

    #[test]
    fn named_lists_resolve_through_the_library() {
        let lib = |id: &str| (id == "ids").then(|| vec!["1".to_string(), "2".to_string()]);
        assert_eq!(Payloads::Builtin { id: "ids".into() }.resolve(&lib).unwrap(), vec!["1", "2"]);
        assert!(Payloads::Builtin { id: "nope".into() }.resolve(&lib).is_err());
        // A run can be planned from a named list resolved through the library.
        let req = RunRequest { url: "https://h/?x=••".into(), lists: vec![Payloads::Builtin { id: "ids".into() }], mode: RunMode::Sweep, ..Default::default() };
        assert_eq!(plan(&req, &lib).unwrap().assignments().len(), 2);
    }

    #[test]
    fn url_positions_come_before_raw_positions() {
        let req = RunRequest {
            url: "https://h/?u=•U•".into(),
            raw: "X-Test: •R•\n\n".into(),
            lists: vec![vals(&["a"]), vals(&["b"])],
            mode: RunMode::Parallel,
            ..Default::default()
        };
        let p = plan(&req, &none).unwrap();
        assert_eq!(p.positions(), 2);
        let (url, raw) = p.render(&["1".into(), "2".into()]);
        assert_eq!(url, "https://h/?u=1");
        assert_eq!(raw, "X-Test: 2\n\n");
    }

    #[test]
    fn split_raw_separates_headers_from_body() {
        let (h, b) = split_raw("A: 1\r\nB: 2\r\n\r\nthe body").unwrap();
        assert_eq!(h, vec![("A".to_string(), "1".to_string()), ("B".to_string(), "2".to_string())]);
        assert_eq!(b, "the body");
        assert!(split_raw("not a header\n\n").is_err());
    }
}

