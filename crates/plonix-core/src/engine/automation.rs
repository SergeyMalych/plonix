//! Automation: scans, the crawler, payload runs and the access check.

use super::*;
use crate::{authcheck, crawl, runs, scan};

impl Engine {
    /// The scan catalog in effect: the built-in detectors and tactics. Installed
    /// scan packs are merged here once pack pinning is wired (see
    /// `docs/scanning.md`).
    pub fn scan_catalog(&self) -> scan::Catalog {
        scan::builtin_catalog()
    }

    /// Fingerprints a host from its captured traffic and suggests a scan
    /// profile. Read-only: sends nothing, so it works for any host.
    pub fn scan_suggest(&self, host: &str) -> Result<scan::ScanSuggestion> {
        let host = scope::normalize_host(host);
        let tech = self.detect_host(&host)?;
        let exchanges = self.store.exchanges_for_host(&host, detect::HOST_SAMPLE)?;
        Ok(self.scan_catalog().suggest(&tech, &exchanges))
    }

    /// Analyzes a host and returns a reviewable scan plan: the active signals
    /// and the gated tactics turned into concrete, justified `TestProposal`s
    /// over the host's discovered endpoints, grouped by OWASP category.
    /// Read-only — sends nothing — so it works for any host and is safe to
    /// expose to an advising agent, exactly like `scan_suggest`.
    pub fn scan_plan(&self, host: &str) -> Result<scan::ScanPlan> {
        let host = scope::normalize_host(host);
        let tech = self.detect_host(&host)?;
        let exchanges = self.store.exchanges_for_host(&host, detect::HOST_SAMPLE)?;
        let endpoints: Vec<scan::PlanEndpoint> = self
            .store
            .endpoints(&host)?
            .into_iter()
            .map(|e| scan::PlanEndpoint { method: e.method, path: e.path, params: e.params, sample_id: e.sample_id })
            .collect();
        Ok(self.scan_catalog().plan(&host, &tech, &exchanges, &endpoints))
    }

    /// A path that can be requested for an endpoint: its folded path holds the
    /// literal `{id}`, so take the path of the request it was seen in.
    fn real_path(&self, e: &crate::model::Endpoint) -> String {
        if !e.path.contains("{id}") {
            return e.path.clone();
        }
        self.store.get_exchange(e.sample_id).ok().flatten().map_or_else(|| e.path.clone(), |x| x.path)
    }

    /// Runs an active scan against one accepted host. Every request goes through
    /// `send`, so the scan can only ever reach a host in accepted scope, and
    /// each request is recorded like any replay. Findings are recorded against
    /// the existing Findings store. Never fires on its own — a person starts it.
    pub async fn scan(&self, req: scan::ScanRequest, initiator: &str) -> Result<scan::ScanReport, SendError> {
        let host = scope::normalize_host(&req.host);
        // Scope is enforced again on every send below; this is the early, clear
        // refusal so a scan never even begins against an un-accepted host.
        let decision = self.rules().decide(&host);
        if decision != Decision::Accepted {
            return Err(SendError::OutOfScope { host, decision: decision.as_str() });
        }
        self.check_automation("scanning")?;
        let mut req = req;
        if self.program().is_some_and(|g| g.no_intrusive()) {
            req.include_intrusive = false;
        }
        let no_intrusive = self.program().is_some_and(|g| g.no_intrusive());

        let catalog = self.scan_catalog();
        let tech = self.detect_host(&host).map_err(SendError::Other)?;
        let exchanges = self.store.exchanges_for_host(&host, detect::HOST_SAMPLE).map_err(SendError::Other)?;
        let signals = catalog.signals(&tech, &exchanges);
        let active: std::collections::BTreeSet<String> = signals.iter().map(|s| s.signal.clone()).collect();

        // Which tactics to run: an explicit list, or the applicable ones from
        // the profile (intrusive only when asked).
        let selected = catalog.select(&active);
        let chosen: Vec<&scan::Tactic> = catalog
            .tactics
            .iter()
            .filter(|t| selected.iter().any(|s| s.id == t.def.id))
            .filter(|t| !(no_intrusive && t.def.intrusiveness == scan::Intrusiveness::Intrusive))
            .filter(|t| {
                if req.tactics.is_empty() {
                    req.include_intrusive || t.def.intrusiveness.default_on()
                } else {
                    req.tactics.iter().any(|id| id == &t.def.id)
                }
            })
            .collect();

        let mut endpoints = self.store.endpoints(&host).map_err(SendError::Other)?;
        // A focused scan aims injecting tactics at only the chosen endpoints.
        // Selector paths are folded the same way the store folds discovered
        // ones, so `/orders/123` matches the folded `/orders/{id}`. This only
        // narrows the target set; scope is still checked on every send.
        if !req.endpoints.is_empty() {
            let want: std::collections::BTreeSet<(String, String)> =
                req.endpoints.iter().map(|e| (e.method.to_ascii_uppercase(), crate::store::fold_path(&e.path))).collect();
            endpoints.retain(|e| want.contains(&(e.method.to_ascii_uppercase(), e.path.clone())));
        }
        // Build the authority from captured traffic so a non-default port is
        // kept; fall back to https:443 for a host with nothing captured yet.
        let (scheme, port) = exchanges.first().map(|e| (e.scheme.clone(), e.port)).unwrap_or_else(|| ("https".into(), 443));
        let default_port = (scheme == "https" && port == 443) || (scheme == "http" && port == 80);
        let authority = if default_port { host.clone() } else { format!("{host}:{port}") };
        let budget = req.max_requests.unwrap_or(scan::DEFAULT_REQUEST_BUDGET);

        let mut report = scan::ScanReport { host: host.clone(), signals, tactics_run: vec![], requests_sent: 0, requests: vec![], findings: vec![], notes: vec![] };
        let mut budget_hit = false;

        for t in &chosen {
            // Fixed-path tactics plan once per host; injecting tactics plan
            // against each discovered endpoint.
            let targets: Vec<Option<scan::ScanTarget>> = if t.def.check.path.is_some() {
                vec![None]
            } else {
                endpoints.iter().map(|e| Some(scan::ScanTarget { method: e.method.clone(), path: self.real_path(e) })).collect()
            };
            let mut ran = false;
            'targets: for target in &targets {
                for planned in t.plan(&scheme, &authority, target.as_ref()) {
                    if report.requests_sent >= budget {
                        budget_hit = true;
                        break 'targets;
                    }
                    ran = true;
                    report.requests_sent += 1;
                    let sent = self
                        .send_scan(
                            SendRequest { method: planned.method.clone(), url: planned.url.clone(), headers: planned.headers.clone(), body: planned.body.clone(), body_base64: None, as_user: req.as_user.clone() },
                            initiator,
                            Source::Scan,
                        )
                        .await;
                    let ex = match sent {
                        Ok(ex) => ex,
                        // A transport error on one request should not abort the
                        // whole scan; note it and move on.
                        Err(SendError::OutOfScope { .. }) => continue,
                        Err(e) => {
                            report.notes.push(format!("request failed: {}", e));
                            continue;
                        }
                    };
                    // Keep a reference to every request the scan sent so the
                    // report can show exactly what it did; each `id` opens the
                    // full request and response in the Lens.
                    // The path and query, as the user would read it, taken from
                    // the recorded exchange (so it reflects any match/replace).
                    let path = if ex.query.is_empty() { ex.path.clone() } else { format!("{}?{}", ex.path, ex.query) };
                    report.requests.push(scan::ScanSent {
                        id: ex.id,
                        tactic: t.def.id.clone(),
                        method: ex.method.clone(),
                        path,
                        status: ex.status,
                        error: ex.error.clone(),
                    });
                    if let Some(mut draft) = t.evaluate(&planned, ex.status, &ex.resp_headers, &ex.resp_body) {
                        draft.exchange_id = ex.id;
                        let f = self
                            .store
                            .add_finding(
                                &crate::model::NewFinding {
                                    title: draft.title.clone(),
                                    severity: severity_str(draft.severity).to_string(),
                                    description: draft.description.clone(),
                                    exchange_ids: vec![ex.id],
                                },
                                initiator,
                            )
                            .map_err(SendError::Other)?;
                        report.findings.push(scan::ScanFindingRef { id: f.id, title: f.title, severity: draft.severity });
                    }
                }
            }
            if ran {
                report.tactics_run.push(t.def.id.clone());
            }
            if budget_hit {
                break;
            }
        }
        if budget_hit {
            report.notes.push(format!("request budget of {budget} reached; some tactics may not have run"));
        }
        Ok(report)
    }

    /// Crawls an accepted host: fetches in-scope pages through `send`, follows
    /// the same-host links it finds, and records what it sees. GET only; it
    /// never submits a form and never leaves accepted scope. With `browser`,
    /// the walk runs in a headless browser instead (see [`crate::browser_crawl`]).
    pub async fn crawl(&self, req: crawl::CrawlRequest, initiator: &str) -> Result<crawl::CrawlReport, SendError> {
        let host = scope::normalize_host(&req.host);
        let decision = self.rules().decide(&host);
        if decision != Decision::Accepted {
            return Err(SendError::OutOfScope { host, decision: decision.as_str() });
        }
        self.check_automation("crawling")?;

        // The start may be a full URL, which also fixes the scheme and port;
        // otherwise they come from the host's captured traffic.
        let start = req.start.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("/");
        let (scheme, authority, start) = match crawl::split_url(start) {
            Some((s, auth, path)) => {
                if scope::normalize_host(auth) != host {
                    return Err(SendError::BadRequest(format!("the start URL {start} is not on {host}")));
                }
                (s.to_ascii_lowercase(), auth.to_string(), path.to_string())
            }
            None if start.contains("://") => return Err(SendError::BadRequest(format!("the start URL {start} must use http or https"))),
            None => {
                let exchanges = self.store.exchanges_for_host(&host, 50).map_err(SendError::Other)?;
                let (scheme, port) = exchanges.first().map(|e| (e.scheme.clone(), e.port)).unwrap_or_else(|| ("https".into(), 443));
                let default_port = (scheme == "https" && port == 443) || (scheme == "http" && port == 80);
                let authority = if default_port { host.clone() } else { format!("{host}:{port}") };
                (scheme, authority, if start.starts_with('/') { start.to_string() } else { format!("/{start}") })
            }
        };

        if req.browser {
            return crate::browser_crawl::run(self, &host, format!("{scheme}://{authority}{start}"), &req).await;
        }

        let max_pages = req.max_pages.unwrap_or(crawl::DEFAULT_MAX_PAGES).min(crawl::MAX_PAGES_CEIL);
        let max_depth = req.max_depth.unwrap_or(crawl::DEFAULT_MAX_DEPTH);

        let mut report = crawl::CrawlReport::new(&host);

        // Seed with the start path and any already-discovered endpoints.
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
        let enqueue = |url: String, depth: usize, seen: &mut std::collections::BTreeSet<String>, queue: &mut std::collections::VecDeque<(String, usize)>| {
            let key = crawl::dedup_key(&url);
            if seen.insert(key) {
                queue.push_back((url, depth));
            }
        };
        enqueue(format!("{scheme}://{authority}{start}"), 0, &mut seen, &mut queue);
        for e in self.store.endpoints(&host).map_err(SendError::Other)?.into_iter().filter(|e| e.method.eq_ignore_ascii_case("GET")) {
            enqueue(format!("{scheme}://{authority}{}", self.real_path(&e)), 0, &mut seen, &mut queue);
        }
        report.urls_found = seen.len();

        let mut budget_hit = false;
        while let Some((url, depth)) = queue.pop_front() {
            if report.pages_fetched >= max_pages {
                budget_hit = true;
                break;
            }
            report.pages_fetched += 1;
            let ex = match self.send_scan(SendRequest { method: "GET".into(), url: url.clone(), ..Default::default() }, initiator, Source::Replay).await {
                Ok(ex) => ex,
                Err(SendError::OutOfScope { .. }) => continue,
                Err(e) => {
                    report.notes.push(format!("fetch failed: {e}"));
                    continue;
                }
            };
            let is_html = crate::model::header(&ex.resp_headers, "content-type").is_some_and(|c| c.to_ascii_lowercase().contains("html"));
            if !is_html {
                continue;
            }
            let Some(body) = crate::codec::body_text(&ex.resp_headers, &ex.resp_body) else { continue };
            for form in crawl::extract_forms(&body) {
                let action = if form.action.is_empty() {
                    url.clone()
                } else {
                    crawl::resolve_same_host(&scheme, &authority, &ex.path, &form.action).unwrap_or(form.action.clone())
                };
                let resolved = crawl::Form { action, ..form };
                report.add_form(resolved);
            }
            if depth < max_depth {
                for raw in crawl::extract_links(&body) {
                    if let Some(next) = crawl::resolve_same_host(&scheme, &authority, &ex.path, &raw) {
                        enqueue(next, depth + 1, &mut seen, &mut queue);
                    }
                }
            }
            report.urls_found = seen.len();
        }
        if budget_hit {
            report.notes.push(format!("page budget of {max_pages} reached; more pages remain uncrawled"));
        }
        Ok(report)
    }

    /// Runs a set of payloads through the marked positions of a request. Every
    /// request it produces goes through [`Self::send`], so a run is bounded by
    /// scope exactly like a single Bench send: it can only reach accepted hosts,
    /// it is capped by a request budget, and every send is recorded. A person
    /// starts it; it never fires on its own.
    pub async fn run(&self, req: runs::RunRequest, initiator: &str) -> Result<runs::RunReport, SendError> {
        self.check_automation("Bench runs")?;
        let lists = self.lists();
        let plan = runs::plan(&req, &|id| lists.values_of(id)).map_err(SendError::BadRequest)?;
        let budget = req.max_requests.unwrap_or(runs::DEFAULT_REQUEST_BUDGET).min(runs::MAX_REQUEST_BUDGET);
        let delay = std::time::Duration::from_millis(req.delay_ms.unwrap_or(runs::DEFAULT_DELAY_MS).min(runs::MAX_DELAY_MS));
        let method = if req.method.trim().is_empty() { "GET".into() } else { req.method.to_ascii_uppercase() };

        let mut report = runs::RunReport { positions: plan.positions(), ..Default::default() };

        // Build the full list of requests to send: the baseline (if asked), then
        // one per value-assignment.
        let base = plan.base_values();
        let mut assignments: Vec<(bool, Vec<String>)> = Vec::new();
        if req.include_base {
            assignments.push((true, base.clone()));
        }
        for a in plan.assignments() {
            assignments.push((false, a));
        }
        report.planned = assignments.len();
        if assignments.len() > budget {
            assignments.truncate(budget);
            report.truncated = true;
        }

        let mut first = true;
        for (baseline, values) in assignments {
            // Pace the run; no pause before the very first request.
            if !first && !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            first = false;

            let (url, raw) = plan.render(&values);
            let (headers, body) = match runs::split_raw(&raw) {
                Ok(hb) => hb,
                Err(e) => return Err(SendError::BadRequest(e)),
            };
            let send = SendRequest {
                method: method.clone(),
                url,
                headers,
                body: if body.is_empty() { None } else { Some(body) },
                body_base64: None,
                as_user: req.as_user.clone(),
            };
            let n = report.rows.len() + 1;
            match self.send(send, initiator).await {
                Ok(ex) => report.rows.push(runs::RunRow {
                    n,
                    values: values.clone(),
                    exchange_id: ex.id,
                    status: ex.status,
                    length: ex.resp_body.len(),
                    duration_ms: ex.duration_ms,
                    error: ex.error,
                    baseline,
                }),
                // The first out-of-scope refusal stops the run with a clear
                // error, since every request targets the same host family.
                Err(e @ SendError::OutOfScope { .. }) => {
                    if report.rows.is_empty() {
                        return Err(e);
                    }
                    report.notes.push(e.to_string());
                    break;
                }
                Err(SendError::BadRequest(m)) => {
                    if report.rows.is_empty() {
                        return Err(SendError::BadRequest(m));
                    }
                    report.notes.push(format!("request {n}: {m}"));
                }
                Err(SendError::Other(e)) => return Err(SendError::Other(e)),
                Err(e) => report.notes.push(format!("request {n}: {e}")),
            }
        }
        report.requests_sent = report.rows.len();
        if report.truncated {
            report.notes.push(format!("request budget of {budget} reached; {} of {} requests were sent", report.requests_sent, report.planned));
        }
        Ok(report)
    }

    /// Replays each target request as every chosen identity and lines the
    /// responses up (see [`crate::authcheck`]). Every replay goes through the
    /// scope-gated send path, so a target outside accepted scope is refused,
    /// never sent.
    pub async fn access_check(&self, req: authcheck::AuthCheckRequest, initiator: &str) -> Result<authcheck::AuthCheckReport, SendError> {
        let mut identities: Vec<authcheck::Identity> =
            req.users.iter().map(|u| authcheck::Identity { id: u.id.clone(), label: u.name.clone(), anon: false }).collect();
        if req.include_anon {
            identities.push(authcheck::Identity { id: "signed-out".into(), label: "Signed out".into(), anon: true });
        }
        if identities.is_empty() {
            return Err(SendError::BadRequest("choose at least one saved user, or keep the signed-out check on".into()));
        }
        let targets: Vec<i64> = req.targets.iter().take(authcheck::MAX_TARGETS).copied().collect();
        if targets.is_empty() {
            return Err(SendError::BadRequest("no requests were selected to check".into()));
        }
        self.check_automation("Access check")?;

        let delay = std::time::Duration::from_millis(req.delay_ms.unwrap_or(authcheck::DEFAULT_DELAY_MS).min(authcheck::MAX_DELAY_MS));
        let mut report = authcheck::AuthCheckReport { identities: identities.clone(), planned: targets.len() * identities.len(), ..Default::default() };

        let mut attempts = 0usize;
        let mut first = true;
        'targets: for tid in targets {
            let Some(orig) = self.store.get_exchange(tid).map_err(SendError::Other)? else { continue };
            // Every credential-looking header this capture carried, not only the fixed list.
            let remove_auth: Vec<String> = crate::users::AUTH_HEADERS
                .iter()
                .map(|h| h.to_string())
                .chain(orig.req_headers.iter().map(|(k, _)| k.clone()).filter(|k| crate::users::is_auth_header(k)))
                .collect();
            let mut cells = Vec::with_capacity(identities.len());
            for ident in &identities {
                if attempts >= authcheck::MAX_REQUESTS {
                    report.truncated = true;
                    break 'targets;
                }
                attempts += 1;
                if !first && !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                first = false;

                // As a saved user: drop the known auth headers the capture
                // carried, then set this user's own. Signed out: drop them and
                // set nothing.
                let user = if ident.anon { None } else { req.users.iter().find(|u| u.id == ident.id) };
                let set_headers = user.map(|u| u.request_headers(&orig.host, crate::users::now_secs())).unwrap_or_default();
                let replay = ReplayRequest {
                    id: tid,
                    method: None,
                    target: None,
                    set_headers,
                    remove_headers: remove_auth.clone(),
                    body: None,
                };
                let cell = match self.replay(replay, initiator).await {
                    Ok(ex) => {
                        report.sent += 1;
                        if let Some(u) = user
                            && let Err(e) = self.store.absorb_cookies(&u.id, &ex.host, &ex.resp_headers)
                        {
                            tracing::warn!("saved user {}: could not keep its cookies: {e:#}", u.id);
                        }
                        authcheck::Cell {
                            identity: ident.id.clone(),
                            status: ex.status,
                            len: ex.resp_size.unwrap_or(ex.resp_body.len() as i64),
                            ms: ex.duration_ms,
                            exchange_id: ex.id,
                            error: ex.error,
                            sig: sha256_hex(&ex.resp_body),
                        }
                    }
                    // Out of scope stops the whole check: every target shares a
                    // host family, so the first refusal means none can be sent.
                    Err(e @ SendError::OutOfScope { .. }) if report.sent == 0 => return Err(e),
                    Err(e) => authcheck::Cell {
                        identity: ident.id.clone(),
                        status: None,
                        len: 0,
                        ms: 0,
                        exchange_id: tid,
                        error: Some(e.to_string()),
                        sig: String::new(),
                    },
                };
                cells.push(cell);
            }
            let notes = authcheck::notes_for(&cells, &identities);
            report.rows.push(authcheck::TargetRow {
                target_id: tid,
                method: orig.method,
                host: orig.host,
                path: orig.path,
                cells,
                notes,
            });
        }
        Ok(report)
    }
}

fn severity_str(s: scan::Severity) -> &'static str {
    match s {
        scan::Severity::Info => "info",
        scan::Severity::Low => "low",
        scan::Severity::Medium => "medium",
        scan::Severity::High => "high",
        scan::Severity::Critical => "critical",
    }
}
