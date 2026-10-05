//! Saved users: named sets of request headers (cookies and auth tokens) that
//! stand for the different people an application knows. They are the "cookie
//! jar" the Bench switches between, and the identities the access check
//! replays a request as.
//!
//! A saved user is only data: a label and a list of headers to apply to a
//! request before it is sent. Applying a user never widens scope — every
//! request still goes through the one scope-gated send path in
//! [`crate::engine::Engine::send`]. Users are stored per project (see
//! [`crate::store::Store::saved_users`]).

use serde::{Deserialize, Serialize};

use crate::detect::clean;
use crate::model::Headers;

/// Most users can hold at once. Enough for every role an application has,
/// without the switcher becoming a list to scroll.
pub const MAX_USERS: usize = 50;
/// Most headers one saved user may carry.
pub const MAX_HEADERS: usize = 40;
const MAX_NAME: usize = 60;
const MAX_NOTE: usize = 200;
const MAX_HEADER_NAME: usize = 120;
const MAX_HEADER_VALUE: usize = 8_192;

/// The request headers stripped when a request is sent with no user (the
/// "signed out" identity), and the ones a saved user replaces rather than
/// adds to. Matched without regard to case.
pub const AUTH_HEADERS: &[&str] =
    &["cookie", "authorization", "x-api-key", "x-auth-token", "x-csrf-token", "x-xsrf-token", "x-session-token"];

/// Whether a header name is one the identities own, so that switching user
/// replaces it instead of leaving two copies.
pub fn is_auth_header(name: &str) -> bool {
    AUTH_HEADERS.iter().any(|h| name.eq_ignore_ascii_case(h))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedUser {
    /// Stable id the Bench and the access check refer to. `[a-z0-9-]`.
    pub id: String,
    /// What the person calls this user, e.g. "Alice (admin)".
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// The headers applied to a request sent as this user. Usually one
    /// `Cookie` line, sometimes an `Authorization` or an API-key header.
    #[serde(default)]
    pub headers: Headers,
}

/// Checks and tidies a list of saved users from the client. Returns the
/// cleaned list or the first problem, so one bad entry cannot corrupt the
/// store. Ids are filled in and de-duplicated here.
pub fn check(users: &[SavedUser]) -> Result<Vec<SavedUser>, String> {
    if users.len() > MAX_USERS {
        return Err(format!("at most {MAX_USERS} saved users"));
    }
    let mut out: Vec<SavedUser> = Vec::with_capacity(users.len());
    for (i, u) in users.iter().enumerate() {
        let name = u.name.trim();
        if name.is_empty() {
            return Err(format!("user {}: a name is required", i + 1));
        }
        if name.chars().count() > MAX_NAME {
            return Err(format!("user {}: the name is longer than {MAX_NAME} characters", i + 1));
        }
        if u.note.chars().count() > MAX_NOTE {
            return Err(format!("{name}: the note is longer than {MAX_NOTE} characters"));
        }
        if u.headers.len() > MAX_HEADERS {
            return Err(format!("{name}: at most {MAX_HEADERS} headers"));
        }
        let mut headers: Headers = Vec::with_capacity(u.headers.len());
        for (k, v) in &u.headers {
            let k = k.trim();
            if k.is_empty() {
                continue; // an empty row from the editor: drop it
            }
            if k.len() > MAX_HEADER_NAME || !k.bytes().all(is_header_name_byte) {
                return Err(format!("{name}: `{}` is not a valid header name", clean(k, 40)));
            }
            if v.len() > MAX_HEADER_VALUE {
                return Err(format!("{name}: the value of `{k}` is longer than {MAX_HEADER_VALUE} bytes"));
            }
            if v.bytes().any(|b| b == b'\r' || b == b'\n') {
                return Err(format!("{name}: the value of `{k}` contains a line break"));
            }
            headers.push((k.to_string(), v.clone()));
        }
        let id = make_id(&u.id, name, &out);
        out.push(SavedUser { id, name: name.to_string(), note: u.note.trim().to_string(), headers });
    }
    Ok(out)
}

fn is_header_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-_".contains(&b)
}

/// Keeps a client-supplied id when it is well formed and unused, otherwise
/// derives a fresh one from the name.
fn make_id(given: &str, name: &str, taken: &[SavedUser]) -> String {
    let used = |id: &str| taken.iter().any(|u| u.id == id);
    let g = given.trim();
    if !g.is_empty() && g.len() <= 40 && g.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !used(g) {
        return g.to_string();
    }
    let mut base = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if !base.ends_with('-') {
            base.push('-');
        }
    }
    let base: String = base.trim_matches('-').chars().take(32).collect();
    let base = if base.is_empty() { "user".to_string() } else { base };
    if !used(&base) {
        return base;
    }
    (2..).map(|n| format!("{base}-{n}")).find(|id| !used(id)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(name: &str, headers: &[(&str, &str)]) -> SavedUser {
        SavedUser {
            id: String::new(),
            name: name.into(),
            note: String::new(),
            headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        }
    }

    #[test]
    fn ids_are_derived_and_unique() {
        let out = check(&[u("Alice (admin)", &[("Cookie", "s=1")]), u("Alice (admin)", &[]), u("Bob", &[])]).unwrap();
        assert_eq!(out[0].id, "alice-admin");
        assert_eq!(out[1].id, "alice-admin-2");
        assert_eq!(out[2].id, "bob");
        assert_eq!(out[0].headers, vec![("Cookie".to_string(), "s=1".to_string())]);
    }

    #[test]
    fn a_given_id_is_kept_when_free() {
        let out = check(&[SavedUser { id: "carol".into(), ..u("Carol", &[]) }]).unwrap();
        assert_eq!(out[0].id, "carol");
    }

    #[test]
    fn empty_header_rows_are_dropped_and_bad_ones_rejected() {
        let out = check(&[u("A", &[("", ""), ("Cookie", "x")])]).unwrap();
        assert_eq!(out[0].headers, vec![("Cookie".to_string(), "x".to_string())]);
        assert!(check(&[u("A", &[("bad header", "x")])]).unwrap_err().contains("not a valid header name"));
        assert!(check(&[u("A", &[("Cookie", "a\r\nb")])]).unwrap_err().contains("line break"));
        assert!(check(&[u("", &[])]).unwrap_err().contains("name is required"));
    }

    #[test]
    fn auth_headers_recognised() {
        assert!(is_auth_header("cookie") && is_auth_header("Authorization") && is_auth_header("X-API-Key"));
        assert!(!is_auth_header("Accept"));
    }
}
