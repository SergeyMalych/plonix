//! Saved Ask Claude conversations, so the Agents screen can list them and
//! the user can pick one up again later, with Claude's context intact.
//!
//! A chat is what the user asked and what Claude answered, turn by turn,
//! plus the Claude session id that a follow-up resumes. Chats are stored per
//! project, in the project database next to everything else it holds, and
//! only the user's own clients can read them: they quote captured traffic.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::now_ms;
use crate::store::Store;

/// The view-state key chats are kept under.
const KEY: &str = "agent_chats";
/// Most chats kept; the ones least recently used are dropped first.
pub const MAX_CHATS: usize = 60;
/// Most turns kept per chat; the earliest are dropped first.
pub const MAX_TURNS: usize = 40;
const MAX_TITLE: usize = 90;
const MAX_ASK: usize = 4_000;
const MAX_ANSWER: usize = 40_000;
const MAX_TOOLS: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Chat {
    pub id: String,
    pub title: String,
    /// What the chat is about when it was started from one place in the
    /// app, e.g. `{"kind": "request", "id": 42}`. Absent for a chat started
    /// on the Agents screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Claude's session id from the latest finished turn; a follow-up resumes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default)]
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Turn {
    /// What the user typed (not the full prompt with its context).
    pub ask: String,
    #[serde(default)]
    pub answer: String,
    /// What Claude looked at while answering, as the conversation named it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    pub status: TurnStatus,
    /// The run that answered this turn, to match it with the activity feed.
    #[serde(default)]
    pub run: String,
    pub at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Running,
    Done,
    Error,
}

/// A chat as listed on the Agents screen, without its full text.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
    pub turns: usize,
    pub running: bool,
    /// The start of Claude's latest answer.
    pub preview: String,
    /// The runs that answered its turns, to name them in the activity feed.
    pub runs: Vec<String>,
}

impl Chat {
    fn summary(&self) -> Summary {
        let last = self.turns.iter().rev().find(|t| !t.answer.is_empty());
        Summary {
            id: self.id.clone(),
            title: self.title.clone(),
            subject: self.subject.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            turns: self.turns.len(),
            running: self.turns.last().is_some_and(|t| t.status == TurnStatus::Running),
            preview: last.map(|t| preview(&t.answer)).unwrap_or_default(),
            runs: self.turns.iter().map(|t| t.run.clone()).filter(|r| !r.is_empty()).collect(),
        }
    }
}

/// What a finished run produced, to save into its turn.
pub struct Outcome {
    pub answer: String,
    pub tools: Vec<String>,
    pub error: String,
    pub ok: bool,
    pub session_id: Option<String>,
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The first line or two of an answer, without Markdown marks, for the list.
fn preview(answer: &str) -> String {
    let mut fenced = false;
    let text: Vec<&str> = answer
        .lines()
        .filter(|l| {
            if l.trim_start().starts_with("```") {
                fenced = !fenced;
                return false;
            }
            // Code and tables read badly as one line of text.
            !fenced && !l.trim_start().starts_with('|')
        })
        .map(|l| l.trim().trim_start_matches(['#', '>', '-', '*', '|', ' ']).trim())
        .filter(|l| !l.is_empty())
        .take(3)
        .collect();
    clip(&text.join(" ").replace("**", "").replace('`', ""), 160)
}

/// A title from the user's first question: its first line, shortened.
pub fn title_from(ask: &str) -> String {
    let first = ask.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Conversation");
    clip(first, MAX_TITLE)
}

fn load(store: &Store) -> Result<Vec<Chat>> {
    let Some(v) = store.view_state(KEY)? else { return Ok(vec![]) };
    Ok(v.get("chats").and_then(|c| serde_json::from_value(c.clone()).ok()).unwrap_or_default())
}

fn save(store: &Store, mut chats: Vec<Chat>) -> Result<()> {
    chats.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
    chats.truncate(MAX_CHATS);
    store.set_view_state(KEY, &json!({ "chats": chats }))
}

/// Chats, most recently used first.
pub fn list(store: &Store) -> Result<Vec<Summary>> {
    let mut chats = load(store)?;
    chats.sort_by_key(|c| std::cmp::Reverse(c.updated_at));
    Ok(chats.iter().map(Chat::summary).collect())
}

pub fn get(store: &Store, id: &str) -> Result<Option<Chat>> {
    Ok(load(store)?.into_iter().find(|c| c.id == id))
}

/// Removes a chat. Returns whether it was there.
pub fn delete(store: &Store, id: &str) -> Result<bool> {
    let chats = load(store)?;
    let before = chats.len();
    let chats: Vec<Chat> = chats.into_iter().filter(|c| c.id != id).collect();
    let found = chats.len() != before;
    if found {
        save(store, chats)?;
    }
    Ok(found)
}

/// Adds a running turn for `run`, to the chat `chat` or to a new one (which
/// takes `title` or one made from the question). Returns the chat id and the
/// session to resume, if the chat has one.
pub fn begin(store: &Store, chat: Option<&str>, title: Option<&str>, subject: Option<Value>, ask: &str, run: &str) -> Result<(String, Option<String>)> {
    let mut chats = load(store)?;
    let now = now_ms();
    let idx = match chat.and_then(|id| chats.iter().position(|c| c.id == id)) {
        Some(i) => i,
        None => {
            let id = format!("k{now:x}{}", chats.len());
            let title = title.map(str::trim).filter(|t| !t.is_empty()).map(|t| clip(t, MAX_TITLE)).unwrap_or_else(|| title_from(ask));
            chats.push(Chat { id, title, subject, created_at: now, updated_at: now, session_id: None, turns: vec![] });
            chats.len() - 1
        }
    };
    let c = &mut chats[idx];
    c.updated_at = now;
    c.turns.push(Turn { ask: clip(ask.trim(), MAX_ASK), answer: String::new(), tools: vec![], error: String::new(), status: TurnStatus::Running, run: run.to_string(), at: now });
    if c.turns.len() > MAX_TURNS {
        let extra = c.turns.len() - MAX_TURNS;
        c.turns.drain(..extra);
    }
    let out = (c.id.clone(), c.session_id.clone());
    save(store, chats)?;
    Ok(out)
}

/// Saves what the run answering a turn produced.
pub fn finish(store: &Store, chat: &str, run: &str, out: Outcome) -> Result<()> {
    let mut chats = load(store)?;
    let Some(c) = chats.iter_mut().find(|c| c.id == chat) else { return Ok(()) };
    let Some(t) = c.turns.iter_mut().rev().find(|t| t.run == run) else { return Ok(()) };
    t.answer = clip(out.answer.trim(), MAX_ANSWER);
    t.tools = out.tools.into_iter().take(MAX_TOOLS).collect();
    t.error = clip(&out.error, 500);
    t.status = if out.ok { TurnStatus::Done } else { TurnStatus::Error };
    if out.session_id.is_some() {
        c.session_id = out.session_id;
    }
    c.updated_at = now_ms();
    save(store, chats)
}

/// Marks turns left running by an engine that stopped as interrupted.
pub fn settle(store: &Store) -> Result<()> {
    let mut chats = load(store)?;
    let mut changed = false;
    for t in chats.iter_mut().flat_map(|c| c.turns.iter_mut()).filter(|t| t.status == TurnStatus::Running) {
        t.status = TurnStatus::Error;
        t.error = "Plonix closed before Claude answered.".into();
        changed = true;
    }
    if changed {
        save(store, chats)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_in_memory().unwrap()
    }

    fn done(answer: &str, sid: &str) -> Outcome {
        Outcome { answer: answer.into(), tools: vec!["Using traffic".into()], error: String::new(), ok: true, session_id: Some(sid.into()) }
    }

    #[test]
    fn a_chat_keeps_its_turns_and_session() {
        let s = store();
        let (id, resume) = begin(&s, None, None, Some(json!({"kind":"request","id":42})), "What does this login do?\nmore", "c1").unwrap();
        assert!(resume.is_none());
        let listed = list(&s).unwrap();
        assert_eq!(listed[0].title, "What does this login do?");
        assert!(listed[0].running);
        finish(&s, &id, "c1", done("## Summary\nIt **posts** the password.", "sess-1")).unwrap();

        // A follow-up goes into the same chat and resumes Claude's session.
        let (same, resume) = begin(&s, Some(&id), None, None, "And the cookie?", "c2").unwrap();
        assert_eq!((same.as_str(), resume.as_deref()), (id.as_str(), Some("sess-1")));
        finish(&s, &id, "c2", done("It is HttpOnly.", "sess-2")).unwrap();

        let chat = get(&s, &id).unwrap().unwrap();
        assert_eq!(chat.turns.len(), 2);
        assert_eq!(chat.session_id.as_deref(), Some("sess-2"));
        assert_eq!(chat.turns[0].tools, ["Using traffic"]);
        assert_eq!(chat.subject, Some(json!({"kind":"request","id":42})));
        let sum = &list(&s).unwrap()[0];
        assert_eq!((sum.turns, sum.running, sum.preview.as_str()), (2, false, "It is HttpOnly."));
    }

    #[test]
    fn chats_are_bounded_and_can_be_deleted() {
        let s = store();
        let mut first = String::new();
        for i in 0..MAX_CHATS + 3 {
            let (id, _) = begin(&s, None, Some("t"), None, &format!("q{i}"), "r").unwrap();
            if i == 0 {
                first = id;
            }
        }
        assert_eq!(list(&s).unwrap().len(), MAX_CHATS);
        assert!(get(&s, &first).unwrap().is_none(), "the oldest chat is dropped");
        let keep = list(&s).unwrap()[0].id.clone();
        assert!(delete(&s, &keep).unwrap());
        assert!(!delete(&s, &keep).unwrap());
    }

    #[test]
    fn unfinished_turns_are_settled() {
        let s = store();
        let (id, _) = begin(&s, None, None, None, "q", "c1").unwrap();
        settle(&s).unwrap();
        let t = &get(&s, &id).unwrap().unwrap().turns[0];
        assert_eq!(t.status, TurnStatus::Error);
        assert!(!list(&s).unwrap()[0].running);
    }

    #[test]
    fn previews_drop_markdown() {
        assert_eq!(preview("# Title\n\n- **one** `two`\n```\ncode\n```"), "Title one two");
        assert_eq!(preview("Ids:\n\n| a | b |\n|---|---|\n| 1 | 2 |\nDone."), "Ids: Done.");
    }
}
