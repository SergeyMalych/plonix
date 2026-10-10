//! Captured traffic: the list, one exchange, saved views, hosts, endpoints,
//! technologies, and HAR import and export.

use super::*;
use crate::scope::Decision;

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/traffic", get(traffic))
        .route("/api/traffic/facets", get(facets))
        .route("/api/traffic/{id}", get(exchange))
        .route("/api/traffic/{id}/insights", get(insights))
        .route("/api/traffic/{id}/messages", get(messages))
        .route("/api/traffic/{id}/spec", get(exchange_spec))
        .route("/api/views/{view}", get(view_state).put(set_view_state))
        .route("/api/hosts", get(hosts))
        .route("/api/hosts/{host}/endpoints", get(endpoints))
        .route("/api/hosts/{host}/spec", get(host_spec))
        .route("/api/tech", get(tech_all))
        .route("/api/tech/{host}", get(tech_host))
        .route("/api/har", get(har_export))
        .route("/api/har/import", post(har_import))
        .route("/api/har/export-file", post(har_export_file))
        .route("/api/har/import-file", post(har_import_file))
}

#[derive(Deserialize, Default)]
struct HarParams {
    /// A Traffic search; everything when empty.
    #[serde(default)]
    q: String,
    /// Comma-separated exchange ids; when given, `q` is ignored.
    #[serde(default)]
    ids: String,
}

impl HarParams {
    fn ids(&self) -> Result<Vec<i64>, Response> {
        self.ids
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().map_err(|_| err(StatusCode::BAD_REQUEST, "bad_request", &format!("'{s}' is not an exchange id"))))
            .collect()
    }
}

/// The exchanges an export holds, or why the request is wrong.
async fn har_selection(s: &AppState, p: &HarParams) -> Result<Vec<i64>, Response> {
    let ids = p.ids()?;
    let (engine, q) = (s.engine.clone(), p.q.clone());
    match tokio::task::spawn_blocking(move || engine.har_selection(&q, &ids)).await {
        Ok(Ok(ids)) => Ok(ids),
        Ok(Err(e)) => Err(err(StatusCode::BAD_REQUEST, "bad_query", &format!("{e:#}"))),
        Err(e) => Err(internal(e.into())),
    }
}

/// Captured traffic as a HAR file: everything, a Traffic search (`q`) or
/// chosen exchanges (`ids`). The file streams as it is written. HAR files
/// hold whole requests, cookies and tokens included, so this is the user's
/// alone.
async fn har_export(State(s): State<AppState>, caller: MaybeCaller, Query(p): Query<HarParams>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let ids = match har_selection(&s, &p).await {
        Ok(ids) => ids,
        Err(r) => return r,
    };
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let engine = s.engine.clone();
    tokio::task::spawn_blocking(move || {
        let mut out = ChannelWriter { tx, buf: Vec::with_capacity(CHUNK) };
        if let Err(e) = engine.write_har(&ids, &mut out) {
            // Cuts the download short, so a broken file is not taken for a whole one.
            let _ = out.tx.blocking_send(Err(std::io::Error::other(format!("{e:#}"))));
        }
    });
    (
        [
            ("content-type", "application/json; charset=utf-8".to_string()),
            ("content-disposition", format!("attachment; filename=\"{}\"", crate::har::file_name(&s.engine.project))),
            ("x-content-type-options", "nosniff".to_string()),
        ],
        axum::body::Body::new(ChannelBody(rx)),
    )
        .into_response()
}

/// Bytes go to the response in chunks of this size.
const CHUNK: usize = 64 * 1024;

/// Writes into a streaming response from a blocking task.
struct ChannelWriter {
    tx: tokio::sync::mpsc::Sender<std::io::Result<bytes::Bytes>>,
    buf: Vec<u8>,
}

impl std::io::Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.flush()?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = bytes::Bytes::from(std::mem::replace(&mut self.buf, Vec::with_capacity(CHUNK)));
        self.tx.blocking_send(Ok(chunk)).map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "the download was cancelled"))
    }
}

/// A response body fed by a [`ChannelWriter`].
struct ChannelBody(tokio::sync::mpsc::Receiver<std::io::Result<bytes::Bytes>>);

impl hyper::body::Body for ChannelBody {
    type Data = bytes::Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<bytes::Bytes>, Self::Error>>> {
        self.0.poll_recv(cx).map(|chunk| chunk.map(|r| r.map(hyper::body::Frame::data)))
    }
}

#[derive(Deserialize, Default)]
struct ImportParams {
    /// A HAR file on this computer to read, instead of the request body.
    #[serde(default)]
    path: Option<String>,
}

/// Imports a HAR file into this project: the request body, or the file at
/// `path` (which may be larger than a request body can be).
async fn har_import(State(s): State<AppState>, caller: MaybeCaller, Query(p): Query<ImportParams>, body: axum::body::Bytes) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let engine = s.engine.clone();
    let done = match p.path.filter(|p| !p.trim().is_empty()) {
        Some(path) => {
            let path = std::path::PathBuf::from(path.trim());
            if !path.is_absolute() {
                return err(StatusCode::BAD_REQUEST, "bad_request", "path must be absolute");
            }
            tokio::task::spawn_blocking(move || crate::har::open(&path).and_then(|f| engine.import_har(f))).await
        }
        None if body.is_empty() => return err(StatusCode::BAD_REQUEST, "bad_request", "send the HAR file as the request body, or give ?path="),
        None => tokio::task::spawn_blocking(move || engine.import_har(&body[..])).await,
    };
    match done {
        Ok(Ok(report)) => Json(report).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "bad_har", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

#[derive(Deserialize, Default)]
struct ExportFileBody {
    #[serde(default)]
    q: String,
    #[serde(default)]
    ids: Vec<i64>,
}

/// In the app: asks where to save with the system's Save dialog, then writes
/// the HAR file there. `{"cancelled": true}` when the user cancels.
async fn har_export_file(State(s): State<AppState>, caller: MaybeCaller, Json(b): Json<ExportFileBody>) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let Some(d) = dialogs::get() else { return no_dialogs() };
    let p = HarParams { q: b.q, ids: b.ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",") };
    let ids = match har_selection(&s, &p).await {
        Ok(ids) => ids,
        Err(r) => return r,
    };
    let engine = s.engine.clone();
    let done = tokio::task::spawn_blocking(move || -> anyhow::Result<Value> {
        let name = crate::har::file_name(&engine.project);
        let Some(path) = d.save("Export Traffic as HAR", &name, &[("HAR file", &["har"])]) else {
            return Ok(json!({ "cancelled": true }));
        };
        let file = std::fs::File::create(&path).map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
        let entries = engine.write_har(&ids, std::io::BufWriter::new(file))?;
        Ok(json!({ "path": path, "entries": entries }))
    })
    .await;
    match done {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

/// In the app: asks for a HAR file with the system's Open dialog and imports it.
async fn har_import_file(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    if let Some(r) = user_only(&caller) {
        return r;
    }
    let Some(d) = dialogs::get() else { return no_dialogs() };
    let picked = tokio::task::spawn_blocking(move || d.open("Import a HAR File", &[("HAR file", &["har", "json"])])).await;
    let Some(path) = picked.ok().flatten() else {
        return Json(json!({ "cancelled": true })).into_response();
    };
    let engine = s.engine.clone();
    let file = path.clone();
    match tokio::task::spawn_blocking(move || crate::har::open(&file).and_then(|f| engine.import_har(f))).await {
        Ok(Ok(report)) => {
            let mut v = json!(report);
            v["path"] = json!(path);
            Json(v).into_response()
        }
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, "bad_har", &format!("{e:#}")),
        Err(e) => internal(e.into()),
    }
}

#[derive(Deserialize)]
struct TrafficParams {
    #[serde(default)]
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
    /// A Traffic column, `-` first for descending; newest first when absent.
    #[serde(default)]
    sort: Option<String>,
}

fn default_limit() -> usize {
    100
}

async fn traffic(State(s): State<AppState>, caller: MaybeCaller, Query(p): Query<TrafficParams>) -> Response {
    let mut q = match s.engine.filters().parse(&p.q) {
        Ok(q) => q,
        Err(e) => return err(StatusCode::BAD_REQUEST, "bad_query", &e.to_string()),
    };
    // Anonymous usage statistics count the kinds of the terms, never the query.
    if is_user(&caller) {
        crate::usage::record_filters(&p.q);
    }
    // Added as a term, not as text, so nothing in the agent's query can swallow it.
    if agent_in_scope_only(&s, &caller) {
        q.terms.push(crate::query::Term { negate: false, field: crate::query::Field::Scope(true) });
    }
    match s.engine.store.search_sorted(&q, &s.engine.rules(), p.sort.as_deref(), p.limit.min(5000), p.offset) {
        Ok((items, total)) => {
            // The same folding the Map uses, for the Traffic "Short path" view.
            let items: Vec<Value> = items
                .into_iter()
                .map(|ex| {
                    let short = crate::store::fold_path(&ex.path);
                    let mut v = json!(ex);
                    v["short_path"] = json!(short);
                    v
                })
                .collect();
            Json(json!({ "total": total, "items": items })).into_response()
        }
        Err(e) => internal(e),
    }
}

async fn exchange(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<i64>) -> Response {
    match s.engine.store.get_exchange(id) {
        Ok(Some(ex)) => {
            let in_scope = s.engine.rules().in_scope(&ex.host);
            if !in_scope && agent_in_scope_only(&s, &caller) {
                return outside_agent_data();
            }
            Json(view(ex, in_scope)).into_response()
        }
        Ok(None) => err(StatusCode::NOT_FOUND, "not_found", &format!("exchange {id} not found")),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
struct PageParams {
    #[serde(default = "default_messages")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_messages() -> usize {
    500
}

/// A message as the API shows it: the stored message plus its text.
#[derive(Serialize)]
pub struct MessageView {
    #[serde(flatten)]
    pub message: crate::model::WsMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// WebSocket messages sent over the connection one handshake opened, oldest first.
async fn messages(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<i64>, Query(p): Query<PageParams>) -> Response {
    let ex = match s.engine.store.get_exchange(id) {
        Ok(Some(ex)) => ex,
        Ok(None) => return err(StatusCode::NOT_FOUND, "not_found", &format!("exchange {id} not found")),
        Err(e) => return internal(e),
    };
    if agent_in_scope_only(&s, &caller) && !s.engine.rules().in_scope(&ex.host) {
        return outside_agent_data();
    }
    match s.engine.store.ws_messages(id, p.limit.min(5000), p.offset) {
        Ok((items, total)) => {
            let items: Vec<MessageView> = items.into_iter().map(|m| MessageView { text: m.text(), message: m }).collect();
            Json(json!({ "total": total, "items": items })).into_response()
        }
        Err(e) => internal(e),
    }
}

/// What stands out in one exchange: tokens to decode, personal data, secrets.
async fn insights(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<i64>) -> Response {
    if agent_in_scope_only(&s, &caller)
        && let Ok(Some(ex)) = s.engine.store.get_exchange(id)
        && !s.engine.rules().in_scope(&ex.host)
    {
        return outside_agent_data();
    }
    let engine = s.engine.clone();
    let found = tokio::task::spawn_blocking(move || {
        engine.store.get_exchange(id).map(|ex| {
            ex.map(|ex| {
                let mut list = crate::insight::analyze(&ex, crate::insight::detectors());
                for i in engine.extension_insights(&ex) {
                    // A secret Plonix already spotted is shown once.
                    let seen = i.category == crate::insight::Category::Secret && list.iter().any(|b| b.category == i.category && b.value == i.value);
                    if !seen {
                        list.push(i);
                    }
                }
                list
            })
        })
    })
    .await;
    match found {
        Ok(Ok(Some(list))) => Json(list).into_response(),
        Ok(Ok(None)) => err(StatusCode::NOT_FOUND, "not_found", &format!("exchange {id} not found")),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

/// UI state saved with the project, such as a view's include/exclude filters,
/// so it survives reloads and is the same in every window.
async fn view_state(State(s): State<AppState>, Path(view): Path<String>) -> Response {
    if !valid_view(&view) {
        return err(StatusCode::BAD_REQUEST, "bad_request", "unknown view name");
    }
    match s.engine.store.view_state(&view) {
        Ok(state) => Json(state.unwrap_or_else(|| json!({}))).into_response(),
        Err(e) => internal(e),
    }
}

async fn set_view_state(State(s): State<AppState>, Path(view): Path<String>, Json(state): Json<Value>) -> Response {
    if !valid_view(&view) {
        return err(StatusCode::BAD_REQUEST, "bad_request", "unknown view name");
    }
    if !state.is_object() || state.to_string().len() > 64 * 1024 {
        return err(StatusCode::BAD_REQUEST, "bad_request", "state must be a JSON object under 64 KB");
    }
    match s.engine.store.set_view_state(&view, &state) {
        Ok(()) => Json(state).into_response(),
        Err(e) => internal(e),
    }
}

fn valid_view(view: &str) -> bool {
    // These names hold engine state (saved users, the program's guard, the exclusions prompt), not UI state.
    !matches!(view, "users" | "program" | "exclusions")
        && !view.is_empty() && view.len() <= 40 && view.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn hosts(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    match s.engine.store.hosts(&s.engine.rules()) {
        Ok(mut h) => {
            if agent_in_scope_only(&s, &caller) {
                h.retain(|h| h.scope == Decision::Accepted);
            }
            Json(h).into_response()
        }
        Err(e) => internal(e),
    }
}

async fn facets(State(s): State<AppState>) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.store.facets(&engine.rules())).await {
        Ok(Ok(f)) => Json(f).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn endpoints(State(s): State<AppState>, caller: MaybeCaller, Path(host): Path<String>) -> Response {
    if agent_in_scope_only(&s, &caller) && !s.engine.rules().in_scope(&host) {
        return outside_agent_data();
    }
    match s.engine.store.endpoints(&host) {
        Ok(e) => Json(e).into_response(),
        Err(e) => internal(e),
    }
}

/// The API description an exchange's response holds, with the endpoints
/// captured traffic already visited marked.
async fn exchange_spec(State(s): State<AppState>, caller: MaybeCaller, Path(id): Path<i64>) -> Response {
    let engine = s.engine.clone();
    let found = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<crate::apispec::ApiSpec>> {
        let Some(ex) = engine.store.get_exchange(id)? else { return Ok(None) };
        let Some(mut spec) = crate::apispec::parse(&ex) else { return Ok(None) };
        let seen = engine.store.endpoints(&spec.host)?;
        crate::apispec::mark_visited(&mut spec, &seen);
        Ok(Some(spec))
    })
    .await;
    match found {
        Ok(Ok(Some(spec))) => {
            if agent_in_scope_only(&s, &caller) && !s.engine.rules().in_scope(&spec.host) {
                return outside_agent_data();
            }
            Json(spec).into_response()
        }
        Ok(Ok(None)) => err(StatusCode::NOT_FOUND, "not_found", &format!("exchange {id} is not an API description")),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

/// The newest API description captured for a host (served by it, or
/// describing it), or null.
async fn host_spec(State(s): State<AppState>, caller: MaybeCaller, Path(host): Path<String>) -> Response {
    if agent_in_scope_only(&s, &caller) && !s.engine.rules().in_scope(&host) {
        return outside_agent_data();
    }
    let engine = s.engine.clone();
    let found = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<crate::apispec::ApiSpec>> {
        let host = host.to_ascii_lowercase();
        let mut spec = engine.store.spec_candidates(&host, 50)?.iter().find_map(crate::apispec::parse);
        if spec.is_none() {
            // Described here but served from another host, e.g. a docs site.
            let rules = engine.rules();
            for other in engine.store.hosts(&rules)?.into_iter().take(100) {
                if other.host == host {
                    continue;
                }
                spec = engine.store.spec_candidates(&other.host, 10)?.iter().filter_map(crate::apispec::parse).find(|s| s.host == host);
                if spec.is_some() {
                    break;
                }
            }
        }
        let Some(mut spec) = spec else { return Ok(None) };
        let seen = engine.store.endpoints(&spec.host)?;
        crate::apispec::mark_visited(&mut spec, &seen);
        Ok(Some(spec))
    })
    .await;
    match found {
        Ok(Ok(spec)) => Json(spec).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn tech_all(State(s): State<AppState>, caller: MaybeCaller) -> Response {
    let engine = s.engine.clone();
    match tokio::task::spawn_blocking(move || engine.detect_all()).await {
        Ok(Ok(mut hosts)) => {
            if agent_in_scope_only(&s, &caller) {
                let rules = s.engine.rules();
                hosts.retain(|h| rules.in_scope(&h.host));
            }
            Json(hosts).into_response()
        }
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}

async fn tech_host(State(s): State<AppState>, caller: MaybeCaller, Path(host): Path<String>) -> Response {
    if agent_in_scope_only(&s, &caller) && !s.engine.rules().in_scope(&host) {
        return outside_agent_data();
    }
    let engine = s.engine.clone();
    let h = host.clone();
    match tokio::task::spawn_blocking(move || engine.detect_host(&h)).await {
        Ok(Ok(tech)) => Json(json!({ "host": host.to_ascii_lowercase(), "tech": tech })).into_response(),
        Ok(Err(e)) => internal(e),
        Err(e) => internal(e.into()),
    }
}
