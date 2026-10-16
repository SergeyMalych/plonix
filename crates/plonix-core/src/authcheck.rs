//! The access check: replay the same captured requests as each saved user,
//! and once signed out, then line the responses up so a person can see where
//! an application treats the identities differently — or fails to.
//!
//! The check sends nothing new: it re-sends requests already captured, each
//! one as a different [`crate::users::SavedUser`], through the one
//! scope-gated send path ([`crate::engine::Engine::send`]). It draws no
//! conclusions on its own; it gathers each response's status, size and a
//! content fingerprint and points out where responses match or where a
//! signed-out request still succeeded. The person decides what it means.

use serde::{Deserialize, Serialize};

use crate::users::SavedUser;

/// Most requests one check will replay, before identities are multiplied in.
pub const MAX_TARGETS: usize = 60;
/// A hard ceiling on the requests one check may send, matching the Bench run
/// budget in spirit: targets × identities is capped here.
pub const MAX_REQUESTS: usize = 400;
pub const DEFAULT_DELAY_MS: u64 = 0;
pub const MAX_DELAY_MS: u64 = 10_000;

/// What to check and as whom.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuthCheckRequest {
    /// The captured exchanges to replay. Resolved from a selection in Traffic
    /// or the Map, or from a host and path prefix, before it reaches here.
    pub targets: Vec<i64>,
    /// The saved users to replay each target as.
    #[serde(default)]
    pub users: Vec<SavedUser>,
    /// Also replay each target with the known auth headers stripped, as a
    /// signed-out visitor. On by default.
    #[serde(default = "yes")]
    pub include_anon: bool,
    #[serde(default)]
    pub delay_ms: Option<u64>,
}

fn yes() -> bool {
    true
}

/// One identity a target was replayed as, for display. Header values are
/// never included.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Identity {
    pub id: String,
    pub label: String,
    /// The signed-out identity, shown apart from the saved users.
    pub anon: bool,
}

/// One response in the grid: a target replayed as one identity.
#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    pub identity: String,
    pub status: Option<u16>,
    pub len: i64,
    pub ms: i64,
    /// The exchange this replay was recorded as, so the person can open it.
    pub exchange_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A fingerprint of the response body, so identical responses can be
    /// spotted without shipping the bodies here. Not shown to the person.
    #[serde(skip)]
    pub sig: String,
}

/// One target request and how each identity was answered.
#[derive(Debug, Clone, Serialize)]
pub struct TargetRow {
    pub target_id: i64,
    pub method: String,
    pub host: String,
    pub path: String,
    pub cells: Vec<Cell>,
    /// Plain-language observations about this row, e.g. that a signed-out
    /// request still succeeded. Never a verdict.
    pub notes: Vec<String>,
    /// Everyone, signed out too, got the same successful page: a public
    /// page, noted but not counted among the rows to look at.
    pub public: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AuthCheckReport {
    pub identities: Vec<Identity>,
    pub rows: Vec<TargetRow>,
    /// How many requests the check meant to send.
    pub planned: usize,
    /// How many it sent (the plan, capped at [`MAX_REQUESTS`]).
    pub sent: usize,
    pub truncated: bool,
    /// Requests on the branch that change data (POST, PUT, PATCH, DELETE…),
    /// left out unless the person asked for them, as "METHOD /path".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skipped: Vec<String>,
}

/// Methods a branch check replays without asking: ones that only read.
pub fn reads_only(method: &str) -> bool {
    matches!(method.to_ascii_uppercase().as_str(), "GET" | "HEAD" | "OPTIONS")
}

/// Whether every identity, the signed-out one included, got the same
/// successful response: a public page rather than an access question.
pub fn is_public(cells: &[Cell], identities: &[Identity]) -> bool {
    let anon_ok = cells.iter().any(|c| ok(c.status) && identities.iter().any(|i| i.anon && i.id == c.identity));
    anon_ok && cells.len() > 1 && cells.iter().all(|c| ok(c.status) && c.sig == cells[0].sig)
}

/// Whether a status is a success, which is what "the identity was let in"
/// means for the notes below.
fn ok(status: Option<u16>) -> bool {
    matches!(status, Some(s) if (200..300).contains(&s))
}

/// Looks over one row's cells and writes the plain observations. Pure, so it
/// is tested without sending anything. The signed-out cell, when present, is
/// expected last (as the engine orders it).
pub fn notes_for(cells: &[Cell], identities: &[Identity]) -> Vec<String> {
    fn label<'a>(identities: &'a [Identity], id: &str) -> &'a str {
        identities.iter().find(|i| i.id == id).map(|i| i.label.as_str()).unwrap_or("that identity")
    }
    let is_anon = |id: &str| identities.iter().find(|i| i.id == id).is_some_and(|i| i.anon);
    if is_public(cells, identities) {
        return vec!["Public: everyone got the same page, signed out too.".into()];
    }
    let mut notes = Vec::new();

    // A signed-out request that still came back as a success.
    if let Some(a) = cells.iter().find(|c| is_anon(&c.identity) && ok(c.status) && c.len > 0) {
        notes.push(format!("Signed out: the request still succeeded ({} bytes).", a.len));
    }

    // Identities that got byte-for-byte the same successful response. When
    // different people see the same bytes on a request that carried their own
    // session, the response may not be theirs alone.
    let mut groups: Vec<(String, Vec<&Cell>)> = Vec::new();
    for c in cells.iter().filter(|c| ok(c.status) && c.len > 0) {
        match groups.iter_mut().find(|(sig, _)| *sig == c.sig) {
            Some((_, g)) => g.push(c),
            None => groups.push((c.sig.clone(), vec![c])),
        }
    }
    for (_, g) in groups.iter().filter(|(_, g)| g.len() > 1) {
        let names: Vec<&str> = g.iter().map(|c| label(identities, &c.identity)).collect();
        notes.push(format!("Same response for {}.", join(&names)));
    }

    notes
}

fn join(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [a] => a.to_string(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(identity: &str, status: u16, len: i64, sig: &str) -> Cell {
        Cell { identity: identity.into(), status: Some(status), len, ms: 1, exchange_id: 1, error: None, sig: sig.into() }
    }

    fn idents() -> Vec<Identity> {
        vec![
            Identity { id: "alice".into(), label: "Alice".into(), anon: false },
            Identity { id: "bob".into(), label: "Bob".into(), anon: false },
            Identity { id: "anon".into(), label: "Signed out".into(), anon: true },
        ]
    }

    #[test]
    fn flags_anonymous_success() {
        let cells = vec![cell("alice", 200, 500, "a"), cell("bob", 403, 20, "b"), cell("anon", 200, 400, "c")];
        let notes = notes_for(&cells, &idents());
        assert!(notes.iter().any(|n| n.contains("Signed out") && n.contains("succeeded")), "{notes:?}");
    }

    #[test]
    fn flags_identical_responses_across_users() {
        let cells = vec![cell("alice", 200, 500, "same"), cell("bob", 200, 500, "same"), cell("anon", 403, 0, "x")];
        let notes = notes_for(&cells, &idents());
        assert!(notes.iter().any(|n| n == "Same response for Alice and Bob."), "{notes:?}");
    }

    #[test]
    fn quiet_when_each_identity_differs() {
        let cells = vec![cell("alice", 200, 500, "a"), cell("bob", 200, 480, "b"), cell("anon", 401, 0, "c")];
        assert!(notes_for(&cells, &idents()).is_empty());
    }

    #[test]
    fn public_pages_get_one_quiet_note() {
        let cells = vec![cell("alice", 200, 300, "p"), cell("bob", 200, 300, "p"), cell("anon", 200, 300, "p")];
        assert!(is_public(&cells, &idents()));
        assert_eq!(notes_for(&cells, &idents()), vec!["Public: everyone got the same page, signed out too.".to_string()]);
        // A user seeing the admin's response while signed-out is refused is not public.
        let cells = vec![cell("alice", 200, 300, "p"), cell("bob", 200, 300, "p"), cell("anon", 401, 10, "x")];
        assert!(!is_public(&cells, &idents()));
    }

    #[test]
    fn only_reads_are_replayed_by_default() {
        assert!(reads_only("get") && reads_only("HEAD") && reads_only("OPTIONS"));
        assert!(!reads_only("POST") && !reads_only("DELETE") && !reads_only("PATCH") && !reads_only("PUT"));
    }

    #[test]
    fn join_reads_naturally() {
        assert_eq!(join(&["A"]), "A");
        assert_eq!(join(&["A", "B"]), "A and B");
        assert_eq!(join(&["A", "B", "C"]), "A, B, and C");
    }
}
