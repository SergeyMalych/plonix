//! The background watcher: while the user browses, Claude reads the new
//! in-scope traffic and leaves notes, leads and a short digest in the Agents
//! screen's inbox, so there is something new to look at each time.
//!
//! It is the same read-only Claude Code run as Ask Claude (see
//! [`crate::assistant`]): it can read the project through the Plonix MCP
//! server and nothing else. It never sends a request; every lead is
//! something for the user to look at or run themselves.
//!
//! It is off until the user turns it on. It runs only when new in-scope
//! traffic has arrived and then gone quiet for a moment (or when the user
//! asks it to look now), and never past the daily token limit the user set.
//! Its settings, counters and inbox are stored per project.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::assistant::Conversations;
use crate::engine::Engine;
use crate::model::now_ms;
use crate::paths::Home;
use crate::query::Query;

const KEY: &str = "agent_watch";
/// Most items the inbox keeps; the oldest are dropped first.
pub const MAX_ITEMS: usize = 200;
/// Most new requests listed for Claude in one look.
const MAX_LISTED: usize = 40;
/// Most items one look may add.
const MAX_PER_LOOK: usize = 6;
/// Dismissed titles remembered, so Claude stops suggesting their like.
const MAX_DISMISSED: usize = 20;
const TICK: Duration = Duration::from_secs(10);
const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WatchSettings {
    pub enabled: bool,
    /// Tokens Claude may read and write per day, all looks together.
    pub daily_tokens: u64,
    /// How long traffic must be quiet before Claude looks.
    pub idle_secs: u64,
    /// How many new in-scope requests make a look worth it.
    pub min_new: usize,
}

impl Default for WatchSettings {
    fn default() -> Self {
        WatchSettings { enabled: false, daily_tokens: 200_000, idle_secs: 45, min_new: 3 }
    }
}

impl WatchSettings {
    pub const BUDGETS: &'static [u64] = &[50_000, 100_000, 200_000, 500_000, 1_000_000];

    fn clean(mut self) -> Self {
        self.daily_tokens = self.daily_tokens.clamp(10_000, 5_000_000);
        self.idle_secs = self.idle_secs.clamp(10, 3600);
        self.min_new = self.min_new.clamp(1, 100);
        self
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Something about one request worth the user's attention.
    Note,
    /// A concrete thing the user could check next.
    Lead,
    /// What is new since the last look, in a few sentences.
    Digest,
}

/// Where a lead points the user: the screen it opens in.
const NEXT: &[&str] = &["lens", "bench", "scan", "access", "finding", "map", "scope"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub id: String,
    pub kind: ItemKind,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// The request it is about, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Where to act on it: one of [`NEXT`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
    pub at: i64,
    #[serde(default)]
    pub read: bool,
    /// The run that wrote it, to match it with the activity feed.
    #[serde(default)]
    pub run: String,
}

/// Counters and progress, stored with the settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchState {
    /// The newest exchange already looked at.
    pub last_id: i64,
    /// The day (days since 1970, UTC) `tokens_today` counts.
    pub day: i64,
    pub tokens_today: u64,
    pub looks_today: u64,
    pub last_look_at: Option<i64>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub last_error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct Data {
    settings: WatchSettings,
    state: WatchState,
    items: Vec<Item>,
    /// Titles the user dismissed, newest last.
    dismissed: Vec<String>,
}

/// What the Agents screen shows: settings, progress and the inbox.
#[derive(Serialize)]
pub struct View {
    pub settings: WatchSettings,
    pub state: WatchState,
    /// A look is running now.
    pub running: bool,
    /// Today's tokens have run out; looks resume tomorrow.
    pub capped: bool,
    /// New in-scope requests waiting for the next look.
    pub waiting: usize,
    pub unread: usize,
    pub items: Vec<Item>,
    pub budgets: &'static [u64],
}

/// One project's watcher. Shared by the API and the background loop.
pub struct Watch {
    engine: Arc<Engine>,
    home: Home,
    conversations: Arc<Conversations>,
    data: Mutex<Data>,
    running: Mutex<Option<String>>,
    /// When the newest exchange last changed, and what it was.
    seen: Mutex<(i64, i64)>,
    /// Set by "Look now"; the next tick looks whatever is waiting.
    asked: Mutex<bool>,
}

fn today() -> i64 {
    now_ms() / DAY_MS
}

impl Watch {
    pub fn new(engine: Arc<Engine>, home: Home, conversations: Arc<Conversations>) -> Arc<Self> {
        let mut data: Data = engine.store.view_state(KEY).ok().flatten().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        data.settings = data.settings.clean();
        // A new project starts watching from what is already captured.
        if data.state.last_id == 0 {
            data.state.last_id = engine.store.max_id().unwrap_or(0);
        }
        Arc::new(Watch { engine, home, conversations, data: Mutex::new(data), running: Mutex::new(None), seen: Mutex::new((0, now_ms())), asked: Mutex::new(false) })
    }

    fn save(&self, data: &Data) {
        if let Err(e) = self.engine.store.set_view_state(KEY, &json!(data)) {
            tracing::warn!("saving the watcher failed: {e:#}");
        }
    }

    fn roll_day(data: &mut Data) {
        let day = today();
        if data.state.day != day {
            data.state.day = day;
            data.state.tokens_today = 0;
            data.state.looks_today = 0;
        }
    }

    /// New in-scope requests after `after`, oldest first, at most [`MAX_LISTED`].
    fn new_requests(&self, after: i64) -> Vec<crate::model::ExchangeSummary> {
        let Ok(q) = Query::parse("scope:in") else { return vec![] };
        let Ok((items, _)) = self.engine.store.search_sorted(&q, &self.engine.rules(), None, MAX_LISTED * 3, 0) else { return vec![] };
        let mut v: Vec<_> = items.into_iter().filter(|x| x.id > after).collect();
        v.sort_by_key(|x| x.id);
        v.truncate(MAX_LISTED);
        v
    }

    pub fn view(&self) -> View {
        let mut data = self.data.lock().unwrap().clone();
        Self::roll_day(&mut data);
        let waiting = if data.settings.enabled { self.new_requests(data.state.last_id).len() } else { 0 };
        View {
            capped: data.state.tokens_today >= data.settings.daily_tokens,
            running: self.running.lock().unwrap().is_some(),
            waiting,
            unread: data.items.iter().filter(|i| !i.read).count(),
            settings: data.settings,
            state: data.state,
            items: data.items,
            budgets: WatchSettings::BUDGETS,
        }
    }

    pub fn unread(&self) -> usize {
        self.data.lock().unwrap().items.iter().filter(|i| !i.read).count()
    }

    pub fn set_settings(&self, new: WatchSettings) -> WatchSettings {
        let mut data = self.data.lock().unwrap();
        let turning_on = new.enabled && !data.settings.enabled;
        data.settings = new.clean();
        // Turning it on starts from now, not from everything captured before.
        if turning_on {
            data.state.last_id = self.engine.store.max_id().unwrap_or(data.state.last_id);
        }
        self.save(&data);
        data.settings.clone()
    }

    /// Marks items read, or removes them (dismiss). Empty `ids` means all.
    pub fn mark(&self, ids: &[String], dismiss: bool) -> usize {
        let mut data = self.data.lock().unwrap();
        let hit = |i: &Item| ids.is_empty() || ids.contains(&i.id);
        let mut n = 0;
        if dismiss {
            let gone: Vec<String> = data.items.iter().filter(|i| hit(i) && i.kind != ItemKind::Digest).map(|i| i.title.clone()).collect();
            data.dismissed.extend(gone);
            let extra = data.dismissed.len().saturating_sub(MAX_DISMISSED);
            data.dismissed.drain(..extra);
            let before = data.items.len();
            data.items.retain(|i| !hit(i));
            n = before - data.items.len();
        } else {
            for i in data.items.iter_mut().filter(|i| hit(i) && !i.read) {
                i.read = true;
                n += 1;
            }
        }
        if n > 0 {
            self.save(&data);
        }
        n
    }

    /// Asks for a look at the next tick, whatever is waiting.
    pub fn look_now(&self) {
        *self.asked.lock().unwrap() = true;
    }

    /// Runs until the engine stops: looks when new in-scope traffic has come
    /// in and gone quiet, or when the user asked.
    pub async fn run(self: Arc<Self>, agents_on: impl Fn() -> bool + Send + 'static) {
        loop {
            tokio::select! {
                _ = self.engine.stopped() => return,
                _ = tokio::time::sleep(TICK) => {}
            }
            if let Err(e) = self.tick(&agents_on).await {
                let mut data = self.data.lock().unwrap();
                data.state.last_error = format!("{e:#}");
                self.save(&data);
            }
        }
    }

    async fn tick(&self, agents_on: &impl Fn() -> bool) -> Result<()> {
        let asked = std::mem::take(&mut *self.asked.lock().unwrap());
        let (settings, last_id, capped) = {
            let mut data = self.data.lock().unwrap();
            Self::roll_day(&mut data);
            (data.settings.clone(), data.state.last_id, data.state.tokens_today >= data.settings.daily_tokens)
        };
        if !(settings.enabled || asked) || !agents_on() || capped || self.running.lock().unwrap().is_some() {
            return Ok(());
        }
        // Wait for the traffic to go quiet: the user is between steps.
        let newest = self.engine.store.max_id()?;
        let quiet = {
            let mut seen = self.seen.lock().unwrap();
            if seen.0 != newest {
                *seen = (newest, now_ms());
            }
            now_ms() - seen.1 >= settings.idle_secs as i64 * 1000
        };
        let fresh = self.new_requests(last_id);
        let worth = fresh.len() >= settings.min_new && quiet;
        if !(worth || asked) {
            return Ok(());
        }
        // Asked with nothing new: look at the latest in-scope requests again.
        let (list, again) = if fresh.is_empty() { (self.latest(), true) } else { (fresh, false) };
        if list.is_empty() {
            let mut data = self.data.lock().unwrap();
            data.state.last_error = "Nothing in scope to look at yet. Browse the target, then look again.".into();
            self.save(&data);
            return Ok(());
        }
        self.look(list, again).await
    }

    fn latest(&self) -> Vec<crate::model::ExchangeSummary> {
        let mut v = self.new_requests(0);
        let skip = v.len().saturating_sub(MAX_LISTED);
        v.drain(..skip);
        v
    }

    async fn look(&self, list: Vec<crate::model::ExchangeSummary>, again: bool) -> Result<()> {
        let newest = list.iter().map(|x| x.id).max().unwrap_or(0);
        let (dismissed, open): (Vec<String>, Vec<String>) = {
            let data = self.data.lock().unwrap();
            (data.dismissed.clone(), data.items.iter().filter(|i| i.kind != ItemKind::Digest).map(|i| i.title.clone()).collect())
        };
        let prompt = prompt(&list, again, &dismissed, &open);
        let run = match self.conversations.start_as(&self.home, prompt, None, crate::mcp::WATCH_CLIENT) {
            Ok(r) => r,
            Err(_) => anyhow::bail!("Claude Code is not installed on this Mac, so the watcher cannot look."),
        };
        *self.running.lock().unwrap() = Some(run.clone());
        let mut used = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            match self.conversations.usage(&run) {
                Some((true, t)) => used = t,
                Some((false, t)) => {
                    used = t;
                    break;
                }
                None => break,
            }
        }
        *self.running.lock().unwrap() = None;
        let out = self.conversations.outcome(&run).flatten();
        let mut data = self.data.lock().unwrap();
        Self::roll_day(&mut data);
        data.state.tokens_today += used;
        data.state.looks_today += 1;
        data.state.last_look_at = Some(now_ms());
        data.state.last_id = data.state.last_id.max(newest);
        data.state.last_error.clear();
        match out {
            Some(o) if o.ok => {
                let known: BTreeSet<i64> = list.iter().map(|x| x.id).collect();
                let fresh = parse_items(&o.answer, &run, &known);
                if fresh.is_empty() {
                    data.state.last_error = "Claude looked but had nothing to add this time.".into();
                }
                let titles: BTreeSet<String> = data.items.iter().map(|i| i.title.to_lowercase()).collect();
                let keep: Vec<Item> = fresh.into_iter().filter(|i| i.kind == ItemKind::Digest || !titles.contains(&i.title.to_lowercase())).collect();
                data.items.splice(0..0, keep);
                data.items.truncate(MAX_ITEMS);
            }
            Some(o) => data.state.last_error = if o.error.is_empty() { "The look did not finish.".into() } else { o.error },
            None => data.state.last_error = "The look was lost.".into(),
        }
        self.save(&data);
        Ok(())
    }
}

/// What Claude is asked to do on one look.
fn prompt(list: &[crate::model::ExchangeSummary], again: bool, dismissed: &[String], open: &[String]) -> String {
    let rows: Vec<String> = list
        .iter()
        .map(|x| {
            let q = if x.query.is_empty() { String::new() } else { format!("?{}", x.query.chars().take(80).collect::<String>()) };
            format!("#{} {} {}{}{} -> {}", x.id, x.method, x.host, x.path, q, x.status.map(|s| s.to_string()).unwrap_or_else(|| "-".into()))
        })
        .collect();
    let mut p = format!(
        "You are the background watcher in Plonix, a workbench a security researcher uses on a target they are authorized to test. \
         You help them notice things; they decide what to do. You can only read the project through the Plonix tools.\n\n\
         {} in-scope requests:\n{}\n\n\
         Read the ones that look most worth attention with the Plonix tools (at most 8). Then reply with ONLY a JSON array, no other text, \
         of at most {MAX_PER_LOOK} objects:\n\
         - {{\"kind\":\"note\",\"title\":…,\"detail\":…,\"request\":<id>}}: something about one request worth the researcher's attention, \
           such as data from another account, an internal hostname, a verbose error or a missing protection.\n\
         - {{\"kind\":\"lead\",\"title\":…,\"detail\":…,\"request\":<id>,\"next\":\"bench|scan|access|finding|lens\"}}: one concrete thing \
           the researcher could check next, and where in Plonix (bench = edit and resend, scan = a scan of that endpoint, access = replay \
           as other saved users, finding = write it up).\n\
         - exactly one {{\"kind\":\"digest\",\"title\":…,\"detail\":…}}: what is new, such as hosts, features and endpoints, in two or three sentences.\n\
         Titles are short, under 80 characters. Details are one to three sentences. Describe what to check and why, never attack \
         payloads or exploit steps. Only use request ids from the list. If nothing stands out, return just the digest.",
        if again { "These are the latest" } else { "These are new" },
        rows.join("\n"),
    );
    if !open.is_empty() {
        p.push_str(&format!("\n\nAlready in the researcher's inbox (do not repeat): {}", open.iter().take(20).cloned().collect::<Vec<_>>().join(" | ")));
    }
    if !dismissed.is_empty() {
        p.push_str(&format!("\n\nThe researcher dismissed these as not useful; avoid suggesting their like: {}", dismissed.join(" | ")));
    }
    p
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// The items in Claude's answer: the JSON array in it, checked field by
/// field. Anything malformed is dropped rather than shown.
pub fn parse_items(answer: &str, run: &str, known: &BTreeSet<i64>) -> Vec<Item> {
    let (Some(a), Some(b)) = (answer.find('['), answer.rfind(']')) else { return vec![] };
    if b <= a {
        return vec![];
    }
    let Ok(Value::Array(raw)) = serde_json::from_str::<Value>(&answer[a..=b]) else { return vec![] };
    let now = now_ms();
    let mut digest = false;
    let mut out = Vec::new();
    for (n, v) in raw.iter().enumerate() {
        let kind = match v["kind"].as_str() {
            Some("note") => ItemKind::Note,
            Some("lead") => ItemKind::Lead,
            Some("digest") if !digest => {
                digest = true;
                ItemKind::Digest
            }
            _ => continue,
        };
        let title = clip(v["title"].as_str().unwrap_or(""), 120);
        if title.is_empty() {
            continue;
        }
        let request = v["request"].as_i64().filter(|id| known.contains(id));
        if kind != ItemKind::Digest && request.is_none() && v["request"].is_number() {
            continue; // an id Claude made up
        }
        let next = v["next"].as_str().filter(|n| NEXT.contains(n)).map(String::from);
        out.push(Item {
            id: format!("w{now:x}{n}"),
            kind,
            title,
            detail: clip(v["detail"].as_str().unwrap_or(""), 1200),
            request,
            host: v["host"].as_str().map(|h| clip(h, 200)),
            next: if kind == ItemKind::Lead { next.or_else(|| Some("lens".into())) } else { None },
            at: now,
            read: false,
            run: run.to_string(),
        });
        if out.len() > MAX_PER_LOOK {
            break;
        }
    }
    // The digest first, then what Claude found.
    out.sort_by_key(|i| i.kind != ItemKind::Digest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_are_read_from_the_answer() {
        let known: BTreeSet<i64> = [12, 13].into();
        let answer = r#"Here you go:
```json
[
  {"kind":"note","title":"Order shows another customer's address","detail":"Request 12 returns a name and address.","request":12},
  {"kind":"lead","title":"Check orders as the second user","detail":"Replay it as Bob.","request":12,"next":"access"},
  {"kind":"lead","title":"Made-up id","request":999},
  {"kind":"lead","title":"Odd next","request":13,"next":"rm -rf"},
  {"kind":"digest","title":"Checkout and orders","detail":"Two new endpoints."},
  {"kind":"digest","title":"A second digest"},
  {"kind":"shell","title":"nope"},
  {"kind":"note","title":""}
]
```"#;
        let items = parse_items(answer, "r1", &known);
        let titles: Vec<_> = items.iter().map(|i| i.title.as_str()).collect();
        assert_eq!(titles, ["Checkout and orders", "Order shows another customer's address", "Check orders as the second user", "Odd next"]);
        assert_eq!(items[2].next.as_deref(), Some("access"));
        // An unknown destination falls back to opening the request.
        assert_eq!(items[3].next.as_deref(), Some("lens"));
        assert!(items.iter().all(|i| i.run == "r1" && !i.read));
        assert!(parse_items("no json here", "r", &known).is_empty());
        assert!(parse_items("[not json]", "r", &known).is_empty());
    }

    #[test]
    fn settings_are_kept_in_range() {
        let s = WatchSettings { enabled: true, daily_tokens: 1, idle_secs: 0, min_new: 0 }.clean();
        assert_eq!((s.daily_tokens, s.idle_secs, s.min_new), (10_000, 10, 1));
        assert!(!WatchSettings::default().enabled, "the watcher is off until the user turns it on");
    }

    #[test]
    fn the_prompt_lists_requests_and_feedback() {
        let ex = crate::model::ExchangeSummary {
            id: 7,
            ts: 0,
            method: "GET".into(),
            scheme: "https".into(),
            host: "api.example.com".into(),
            port: 443,
            path: "/v1/orders/1".into(),
            query: "x=1".into(),
            status: Some(200),
            mime: String::new(),
            resp_len: 0,
            duration_ms: 0,
            source: String::new(),
            in_scope: true,
            edited: false,
        };
        let p = prompt(&[ex], false, &["Old idea".into()], &["Open idea".into()]);
        assert!(p.contains("#7 GET api.example.com/v1/orders/1?x=1 -> 200"));
        assert!(p.contains("dismissed") && p.contains("Old idea") && p.contains("Open idea"));
    }
}
