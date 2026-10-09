//! AI agents: the access policy, Ask Claude runs and conversations, the
//! watcher that fills the inbox, and the edits agents suggest for Bench drafts.

use super::*;
use crate::access::AgentSettings;
use crate::ask::{self, AskError, AskRequest};
use crate::assistant::StartError;
use crate::proposal::{self, DraftRequest, NewProposal};

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/agents", get(agents))
        .route("/api/agents/settings", get(agent_settings).put(put_agent_settings))
        .route("/api/agents/ask", post(agent_ask))
        .route("/api/agents/launch", post(agent_launch))
        .route("/api/agents/run", post(agent_run))
        .route("/api/agents/run/{id}", get(agent_run_poll).delete(agent_run_cancel))
        .route("/api/agents/activity", get(agent_activity))
        .route("/api/agents/watch", get(watch_view).put(watch_settings))
        .route("/api/agents/watch/look", post(watch_look))
        .route("/api/agents/watch/items", post(watch_items))
        .route("/api/agents/chats", get(agent_chats))
        .route("/api/agents/chats/{id}", get(agent_chat).delete(delete_agent_chat))
        .route("/api/bench/proposals", get(list_proposals).post(add_proposal))
        .route("/api/bench/proposals/{id}", axum::routing::delete(discard_proposal))
        .route("/api/bench/proposals/{id}/diff", post(proposal_diff))
}

/// The agent access policy and which agents have connected.
async fn agents(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    let mode = AgentMode::current();
    let settings = s.agent_settings.get();
    let mut v = json!({
        "mode": mode,
        "enabled": settings.enabled,
        "data": settings.data,
        "capabilities": access::effective(mode, &settings),
        "not_allowed": access::not_allowed(mode),
        "connect": {
            "command": "plonix connect claude",
            "server": { "command": "plonix", "args": ["mcp"] },
        },
        // Whether "Ask Claude Code" can run inside Plonix: true when the
        // `claude` CLI is installed on this machine.
        "ask_in_app": Conversations::cli_available(),
    });
    // Only the user sees who else is connected.
    if caller.is_some_and(|c| c.0 == Caller::User) {
        v["clients"] = json!(s.agents.clients());
    }
    Json(v).into_response()
}

async fn agent_settings(State(s): State<AppState>) -> Response {
    let settings = s.agent_settings.get();
    let groups: Vec<Value> = Group::SWITCHABLE.iter().map(|(g, label)| json!({ "group": g, "label": label, "on": settings.group_on(*g) })).collect();
    Json(json!({ "settings": settings, "groups": groups, "budgets": AgentSettings::BUDGETS })).into_response()
}

/// Changes agent access. Agents cannot reach this route (it is in no mode's
/// capabilities), so only the user changes what agents may see.
async fn put_agent_settings(State(s): State<AppState>, Json(new): Json<AgentSettings>) -> Response {
    if let Err(e) = s.agent_settings.set(new) {
        return internal(e);
    }
    agent_settings(State(s)).await
}

/// Builds the context for "Ask Claude Code" about one request, finding or host.
async fn agent_ask(State(s): State<AppState>, Json(req): Json<AskRequest>) -> Response {
    let settings = s.agent_settings.get();
    if !settings.enabled {
        return err(StatusCode::FORBIDDEN, "agents_disabled", "agent access is turned off in Settings › AI agents");
    }
    crate::usage::record("ask_claude");
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || ask::build(&engine, &req, &settings)).await {
        Ok(Ok(bundle)) => Json(bundle).into_response(),
        Ok(Err(AskError::NotFound(what))) => err(StatusCode::NOT_FOUND, "not_found", &format!("{what} not found")),
        Ok(Err(AskError::Other(e))) => internal(e),
        Err(e) => internal(e.into()),
    }
}

#[derive(Deserialize)]
struct LaunchBody {
    prompt: String,
}

/// Opens Claude Code in Terminal with the prompt the user reviewed.
async fn agent_launch(State(s): State<AppState>, Json(b): Json<LaunchBody>) -> Response {
    if b.prompt.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "bad_request", "the prompt is empty");
    }
    crate::usage::record("agent_launch");
    let home = s.home.clone();
    match tokio::task::spawn_blocking(move || ask::launch_in_terminal(&home, &b.prompt)).await {
        Ok(Ok(_)) => Json(json!({ "ok": true })).into_response(),
        Ok(Err(e)) if format!("{e:#}").starts_with("unsupported") => err(StatusCode::NOT_IMPLEMENTED, "unsupported", &format!("{e:#}")),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

#[derive(Deserialize)]
struct RunBody {
    /// The prompt for a first turn, or the follow-up message when `resume` is set.
    prompt: String,
    /// Claude session id to continue, for a follow-up turn in the same chat.
    #[serde(default)]
    resume: Option<String>,
    /// What the user typed, when the turn should be saved as a chat the
    /// Agents screen lists. Without it, nothing is saved (e.g. a finding
    /// written with Claude).
    #[serde(default)]
    ask: Option<String>,
    /// The saved chat this turn continues; its session is resumed.
    #[serde(default)]
    chat: Option<String>,
    /// A title for a new chat; by default its first question.
    #[serde(default)]
    title: Option<String>,
    /// What a new chat is about, e.g. `{"kind": "request", "id": 42}`.
    #[serde(default)]
    subject: Option<Value>,
}

/// Starts an in-app "Ask Claude Code" conversation: runs `claude` headless,
/// wired to the read-only Plonix MCP, and streams the answer into the panel.
async fn agent_run(State(s): State<AppState>, Json(b): Json<RunBody>) -> Response {
    let settings = s.agent_settings.get();
    if !settings.enabled {
        return err(StatusCode::FORBIDDEN, "agents_disabled", "agent access is turned off in Settings › AI agents");
    }
    if b.prompt.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "bad_request", "the message is empty");
    }
    let ask = b.ask.as_deref().map(str::trim).filter(|a| !a.is_empty()).map(String::from);
    // A follow-up in a saved chat resumes its Claude session.
    let saved = match (&ask, &b.chat) {
        (Some(_), Some(id)) => match chats::get(&s.engine.store, id) {
            Ok(Some(c)) => Some(c),
            Ok(None) => return err(StatusCode::NOT_FOUND, "not_found", "no such conversation"),
            Err(e) => return internal(e),
        },
        _ => None,
    };
    let resume = b.resume.or_else(|| saved.as_ref().and_then(|c| c.session_id.clone()));
    match s.conversations.start(&s.home, b.prompt, resume) {
        Ok(id) => {
            let Some(ask) = ask else { return Json(json!({ "id": id })).into_response() };
            let _guard = s.chats_lock.lock().await;
            match chats::begin(&s.engine.store, saved.as_ref().map(|c| c.id.as_str()), b.title.as_deref(), b.subject, &ask, &id) {
                Ok((chat, _)) => {
                    tokio::spawn(save_when_done(s.clone(), chat.clone(), id.clone()));
                    Json(json!({ "id": id, "chat": chat })).into_response()
                }
                Err(e) => internal(e),
            }
        }
        Err(StartError::NoCli) => err(
            StatusCode::NOT_IMPLEMENTED,
            "no_cli",
            "Claude Code is not installed on this machine. Install it from claude.com/claude-code, or use \"Open in Terminal\".",
        ),
    }
}

#[derive(Deserialize)]
struct PollQuery {
    #[serde(default)]
    since: usize,
}

/// Returns whatever is new in a conversation since the last poll.
async fn agent_run_poll(State(s): State<AppState>, Path(id): Path<String>, Query(q): Query<PollQuery>) -> Response {
    match s.conversations.poll(&id, q.since) {
        Some(snap) => Json(snap).into_response(),
        None => err(StatusCode::NOT_FOUND, "not_found", "no such conversation"),
    }
}

/// Waits for a run to finish and saves its answer into its chat.
async fn save_when_done(s: AppState, chat: String, run: String) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let out = match s.conversations.outcome(&run) {
            Some(None) => continue,
            Some(Some(out)) => out,
            None => chats::Outcome { answer: String::new(), tools: vec![], error: "The answer was lost.".into(), ok: false, session_id: None },
        };
        let _guard = s.chats_lock.lock().await;
        if let Err(e) = chats::finish(&s.engine.store, &chat, &run, out) {
            tracing::warn!("saving the conversation failed: {e:#}");
        }
        return;
    }
}

#[derive(Deserialize)]
struct ActivityQuery {
    #[serde(default)]
    since: u64,
    #[serde(default)]
    limit: Option<usize>,
}

/// What agents read lately, newest first. User-only: agents cannot reach it.
async fn agent_activity(State(s): State<AppState>, Query(q): Query<ActivityQuery>) -> Response {
    Json(json!({ "hits": s.agents.hits(q.since, q.limit.unwrap_or(200).min(access::MAX_HITS)), "clients": s.agents.clients() })).into_response()
}

/// The watcher: its settings, today's use and the inbox. User-only.
async fn watch_view(State(s): State<AppState>) -> Response {
    Json(s.watch.view()).into_response()
}

async fn watch_settings(State(s): State<AppState>, Json(new): Json<crate::watch::WatchSettings>) -> Response {
    s.watch.set_settings(new);
    Json(s.watch.view()).into_response()
}

/// Asks the watcher to look at the latest traffic now.
async fn watch_look(State(s): State<AppState>) -> Response {
    if !s.agent_settings.get().enabled {
        return err(StatusCode::FORBIDDEN, "agents_disabled", "agent access is turned off in Settings › AI agents");
    }
    s.watch.look_now();
    Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
struct WatchItemsBody {
    /// Items to change; empty means all of them.
    #[serde(default)]
    ids: Vec<String>,
    /// Remove them, and steer the watcher away from their like.
    #[serde(default)]
    dismiss: bool,
}

/// Marks inbox items read, or dismisses them.
async fn watch_items(State(s): State<AppState>, Json(b): Json<WatchItemsBody>) -> Response {
    let n = s.watch.mark(&b.ids, b.dismiss);
    Json(json!({ "changed": n, "unread": s.watch.unread() })).into_response()
}

/// Saved Ask Claude conversations, most recent first. User-only.
async fn agent_chats(State(s): State<AppState>) -> Response {
    match chats::list(&s.engine.store) {
        Ok(list) => Json(json!({ "chats": list })).into_response(),
        Err(e) => internal(e),
    }
}

async fn agent_chat(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    match chats::get(&s.engine.store, &id) {
        Ok(Some(c)) => Json(c).into_response(),
        Ok(None) => err(StatusCode::NOT_FOUND, "not_found", "no such conversation"),
        Err(e) => internal(e),
    }
}

async fn delete_agent_chat(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    let _guard = s.chats_lock.lock().await;
    match chats::delete(&s.engine.store, &id) {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => err(StatusCode::NOT_FOUND, "not_found", "no such conversation"),
        Err(e) => internal(e),
    }
}

/// Stops a running conversation.
async fn agent_run_cancel(State(s): State<AppState>, Path(id): Path<String>) -> Response {
    if s.conversations.cancel(&id) {
        Json(json!({ "ok": true })).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "not_found", "no such conversation")
    }
}

// ---- suggested Bench edits -----------------------------------------------------

/// Keeps an edit an agent suggests for a Bench draft. This is the one route
/// agents may write to, and all it does is store the suggestion: nothing is
/// sent and the draft is untouched until the user applies it on the Bench.
async fn add_proposal(State(s): State<AppState>, caller: MaybeCaller, headers: HeaderMap, Json(new): Json<NewProposal>) -> Response {
    let from = if caller.is_some_and(|c| c.0 == Caller::Agent) { initiator(&headers) } else { "you".into() };
    match s.proposals.add(new, &from) {
        Ok(p) => Json(p).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, "bad_request", &e),
    }
}

#[derive(Deserialize)]
struct ProposalQuery {
    #[serde(default)]
    draft: Option<String>,
}

/// Suggestions waiting on the Bench, newest first (user only).
async fn list_proposals(State(s): State<AppState>, Query(q): Query<ProposalQuery>) -> Response {
    Json(json!({ "proposals": s.proposals.list(q.draft.as_deref().filter(|d| !d.is_empty())) })).into_response()
}

/// A suggestion compared with the draft as the Bench holds it now (user only).
async fn proposal_diff(State(s): State<AppState>, Path(id): Path<u64>, Json(current): Json<DraftRequest>) -> Response {
    match s.proposals.get(id) {
        Some(p) => {
            let diff = proposal::diff(&current, &p.request);
            Json(json!({ "proposal": p, "diff": diff })).into_response()
        }
        None => err(StatusCode::NOT_FOUND, "not_found", "that suggestion is gone"),
    }
}

/// Drops a suggestion once the user applied or discarded it (user only).
async fn discard_proposal(State(s): State<AppState>, Path(id): Path<u64>) -> Response {
    match s.proposals.remove(id) {
        Some(_) => Json(json!({ "ok": true })).into_response(),
        None => err(StatusCode::NOT_FOUND, "not_found", "that suggestion is gone"),
    }
}
