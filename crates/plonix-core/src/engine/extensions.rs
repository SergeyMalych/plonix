//! Extensions: loading them, feeding them traffic, running their programs
//! and the parameter probe.

use super::*;
use crate::extension::{self, Capability, ExtensionLibrary, Loaded, LoadedSet, Runner};
use crate::insight::{Category as InsightCategory, Insight, Side as InsightSide};

/// Extensions in effect, reloaded when they are installed, removed, enabled
/// or disabled.
#[derive(Default)]
pub(super) struct ExtensionState {
    library: Option<ExtensionLibrary>,
    loaded_stamp: Option<extension::Stamp>,
    set: Arc<LoadedSet>,
}

/// Exchanges handed to an extension in one call.
const EXTENSION_BATCH: usize = 10;

/// What running an extension over captured traffic did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ExtensionRun {
    pub extension: String,
    /// Exchanges it was given.
    pub exchanges: usize,
    pub notes: usize,
    /// Findings it proposed that were not already there.
    pub proposed: usize,
    /// For a subdomain finder: new subdomains it added to Scope as suggestions.
    pub suggested: usize,
    /// Captured requests to hosts outside scope that it was not allowed to read.
    pub skipped: usize,
    /// Lines it logged, capped.
    pub logs: Vec<String>,
    /// Set when it was stopped and switched off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stopped: Option<String>,
    /// Set when a program extension could not finish, e.g. its program is
    /// not installed. It stays on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// The outcome of a parameter probe (see [`Engine::run_param_probe`]).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ParamProbeReport {
    pub target: String,
    /// The baseline request's exchange id.
    pub baseline_exchange: i64,
    /// Candidate parameters sent, not counting the baseline and calibration.
    pub sent: usize,
    /// Candidates whose request could not be sent.
    pub errors: usize,
    /// Parameter names that changed the response.
    pub influential: Vec<String>,
    /// Whether a finding was proposed (false when one was already there).
    pub proposed: bool,
}

/// A probe request: the target endpoint, optionally with one extra query
/// parameter `name=<marker>` appended. Only a parameter name and a benign
/// marker are added; the probe sends no payload of its own.
fn probe_request(url: &str, param: Option<&str>, marker: &str) -> SendRequest {
    let url = match param {
        Some(p) => format!("{url}{}{p}={marker}", if url.contains('?') { '&' } else { '?' }),
        None => url.to_string(),
    };
    SendRequest { method: "GET".into(), url, headers: vec![], body: None, body_base64: None, as_user: None }
}

impl Engine {
    /// Uses this library for extensions and starts feeding newly captured
    /// traffic to the enabled ones.
    pub fn set_extension_library(self: &Arc<Self>, library: ExtensionLibrary) {
        {
            let mut e = self.extensions.lock().unwrap();
            e.library = Some(library);
            e.loaded_stamp = None;
        }
        let mut feed = self.extension_feed.lock().unwrap();
        if feed.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel::<i64>();
        let engine = Arc::downgrade(self);
        let started = std::thread::Builder::new().name("plonix-extensions".into()).spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut ids = vec![first];
                while ids.len() < EXTENSION_BATCH * 5
                    && let Ok(id) = rx.try_recv()
                {
                    ids.push(id);
                }
                let Some(engine) = engine.upgrade() else { break };
                engine.feed_extensions(&ids);
            }
        });
        if started.is_ok() {
            *feed = Some(tx);
        }
    }

    /// The enabled extensions, reloading them if anything changed.
    pub fn extensions(&self) -> Arc<LoadedSet> {
        let mut e = self.extensions.lock().unwrap();
        let Some(stamp) = e.library.as_ref().map(ExtensionLibrary::stamp) else { return e.set.clone() };
        if e.loaded_stamp != Some(stamp) {
            let set = e.library.as_ref().map(ExtensionLibrary::load).unwrap_or_default();
            for p in &set.problems {
                tracing::warn!("extensions: {p}");
            }
            e.set = Arc::new(set);
            e.loaded_stamp = Some(stamp);
        }
        e.set.clone()
    }

    /// Runs one extension over exchanges it may see: in-scope ones, and the
    /// rest only with `read-out-of-scope`. A fault switches it off with the
    /// reason, and never reaches further than this call.
    fn run_extension(&self, ext: &Loaded, exchanges: &[Exchange]) -> Result<(sandbox::Output, usize), sandbox::Fault> {
        let Runner::Wasm(compiled) = &ext.runner else { return Ok((sandbox::Output::default(), 0)) };
        let rules = self.rules();
        let out_of_scope = ext.granted.contains(&Capability::ReadOutOfScope);
        let visible: Vec<(&Exchange, bool)> =
            exchanges.iter().map(|ex| (ex, rules.in_scope(&ex.host))).filter(|(_, in_scope)| *in_scope || out_of_scope).collect();
        if visible.is_empty() || !ext.granted.contains(&Capability::ReadTraffic) {
            return Ok((sandbox::Output::default(), 0));
        }
        let ids: Vec<i64> = visible.iter().map(|(ex, _)| ex.id).collect();
        let batch = sandbox::batch_json(&visible);
        match sandbox::analyze(compiled, &ext.granted, &batch, &ids, *self.extension_limits.read().unwrap()) {
            Ok(out) => {
                for line in &out.logs {
                    tracing::info!("extension {}: {line}", ext.name);
                }
                Ok((out, visible.len()))
            }
            Err(fault) => {
                let reason = format!("Plonix stopped it and switched it off: {fault}. Turn it back on once it is fixed.");
                tracing::warn!("extension {} {}: {reason}", ext.name, ext.version);
                let mut e = self.extensions.lock().unwrap();
                if let Some(lib) = &e.library
                    && let Err(err) = lib.disable(&ext.name, &reason)
                {
                    tracing::error!("extension {}: could not record that it is switched off: {err:#}", ext.name);
                }
                // Drop it now, whatever the file system's clock says.
                let rest = e.set.extensions.iter().filter(|x| x.name != ext.name).cloned().collect();
                e.set = Arc::new(LoadedSet { extensions: rest, problems: e.set.problems.clone() });
                Err(fault)
            }
        }
    }

    /// Stores what an extension proposed as open findings attributed to it,
    /// skipping ones it already proposed. Returns how many are new.
    fn store_proposals(&self, ext: &Loaded, proposals: &[sandbox::Proposal]) -> Result<usize> {
        if proposals.is_empty() {
            return Ok(0);
        }
        let note = format!("Proposed by the extension {} {}. Not confirmed: check it before you rely on it.", ext.name, ext.version);
        let findings = proposals.iter().map(|p| {
            let description = if p.description.is_empty() { note.clone() } else { format!("{}\n\n{note}", p.description) };
            crate::model::NewFinding { title: p.title.clone(), severity: p.severity.clone(), description, exchange_ids: p.exchange_ids.clone() }
        });
        self.add_extension_findings(&ext.name, findings)
    }

    /// Hands newly recorded exchanges to every enabled extension.
    fn feed_extensions(&self, ids: &[i64]) {
        let set = self.extensions();
        if set.extensions.is_empty() {
            return;
        }
        let exchanges: Vec<Exchange> = ids.iter().filter_map(|id| self.store.get_exchange(*id).ok().flatten()).collect();
        for ext in &set.extensions {
            if let Runner::Program(program) = &ext.runner {
                if let Err(e) = self.run_program(ext, program, &exchanges) {
                    self.program_problem(ext, &e);
                }
                continue;
            }
            for chunk in exchanges.chunks(EXTENSION_BATCH) {
                match self.run_extension(ext, chunk) {
                    Ok((out, _)) => {
                        if let Err(e) = self.store_proposals(ext, &out.proposals) {
                            tracing::error!("extension {}: storing its findings: {e:#}", ext.name);
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }

    /// Queues a recorded exchange for the enabled extensions.
    pub(super) fn queue_for_extensions(&self, id: i64) {
        if let Some(tx) = self.extension_feed.lock().unwrap().as_ref() {
            let _ = tx.send(id);
        }
    }

    /// Runs one extension over everything captured so far (it may only see
    /// what its capabilities allow), storing the findings it proposes.
    pub fn run_extension_on_traffic(&self, name: &str) -> Result<ExtensionRun> {
        let set = self.extensions();
        let Some(ext) = set.extensions.iter().find(|e| e.name == name) else {
            anyhow::bail!("no enabled extension named `{}` (see `plonix extensions`)", crate::detect::clean(name, 64));
        };
        let mut run = ExtensionRun { extension: ext.name.clone(), ..Default::default() };
        let mut last = 0;
        if let Runner::Program(program) = &ext.runner {
            match crate::program::get(program).map(|p| p.kind) {
                Some(crate::program::Kind::Enumerate) => {
                    match self.discover_subdomains(&ext) {
                        Ok(found) => run.suggested += found,
                        Err(e) => run.problem = Some(e),
                    }
                    return Ok(run);
                }
                Some(crate::program::Kind::Probe) => {
                    run.problem = Some(format!(
                        "{} probes one address at a time. Pick an in-scope request in Traffic and choose Probe parameters, or run `plonix extensions probe {} <url>`.",
                        ext.name, ext.name
                    ));
                    return Ok(run);
                }
                _ => {}
            }
            if !ext.granted.contains(&Capability::RunProgram) {
                run.problem = Some(not_allowed(ext, program));
                return Ok(run);
            }
            loop {
                let batch = self.store.exchanges_after(last, crate::program::BATCH)?;
                let Some(tail) = batch.last() else { break };
                last = tail.id;
                run.skipped += self.out_of_scope_for(ext, &batch);
                match self.run_program(ext, program, &batch) {
                    Ok((seen, hits)) => {
                        run.exchanges += seen;
                        run.notes += hits;
                    }
                    Err(e) => {
                        run.problem = Some(e);
                        break;
                    }
                }
            }
            return Ok(run);
        }
        loop {
            let batch = self.store.exchanges_after(last, EXTENSION_BATCH)?;
            let Some(tail) = batch.last() else { break };
            last = tail.id;
            run.skipped += self.out_of_scope_for(ext, &batch);
            match self.run_extension(ext, &batch) {
                Ok((out, seen)) => {
                    run.exchanges += seen;
                    run.notes += out.notes.len();
                    run.proposed += self.store_proposals(ext, &out.proposals)?;
                    run.logs.extend(out.logs.into_iter().take(50usize.saturating_sub(run.logs.len())));
                }
                Err(fault) => {
                    run.stopped = Some(format!("Stopped and switched off: {fault}."));
                    break;
                }
            }
        }
        Ok(run)
    }

    /// How many of these exchanges an extension may not read because their
    /// host is outside scope.
    fn out_of_scope_for(&self, ext: &Loaded, exchanges: &[Exchange]) -> usize {
        if ext.granted.contains(&Capability::ReadOutOfScope) {
            return 0;
        }
        let rules = self.rules();
        exchanges.iter().filter(|ex| !rules.in_scope(&ex.host)).count()
    }

    /// Notes from the enabled extensions on one exchange, shown in the Lens
    /// next to what Plonix spotted and labelled with the extension's name.
    /// Findings they would propose are shown, not stored.
    pub fn extension_insights(&self, ex: &Exchange) -> Vec<Insight> {
        let set = self.extensions();
        let mut out = vec![];
        let stored = self.store.extension_hits(ex.id).unwrap_or_default();
        for ext in &set.extensions {
            if let Runner::Program(_) = &ext.runner {
                match stored.iter().find(|(name, version, _)| *name == ext.name && *version == ext.version) {
                    Some((_, _, json)) => out.extend(program_insights(ext, json)),
                    // Not scanned yet: queue it, and its hits show next time.
                    None => self.queue_for_extensions(ex.id),
                }
                continue;
            }
            let Ok((result, _)) = self.run_extension(ext, std::slice::from_ref(ex)) else { continue };
            let from = format!("From the extension {} {}, not from Plonix", ext.name, ext.version);
            let notes = result.notes.into_iter().map(|n| (n.tag, n.text));
            let proposals = result.proposals.into_iter().map(|p| (format!("proposes: {}", p.severity), p.title));
            for (tag, text) in notes.chain(proposals).take(20) {
                out.push(Insight {
                    kind: format!("extension:{}", ext.name),
                    category: InsightCategory::Extension,
                    label: tag,
                    side: InsightSide::Response,
                    location: format!("extension {}", ext.name),
                    value: text,
                    decoded: None,
                    notes: vec![from.clone()],
                    count: 1,
                });
            }
        }
        out
    }

    /// Runs a program extension over the exchanges it may see that this
    /// version has not scanned yet, storing what it found. Returns how many
    /// exchanges it scanned and how many secrets it found.
    fn run_program(&self, ext: &Loaded, program: &str, exchanges: &[Exchange]) -> std::result::Result<(usize, usize), String> {
        if !ext.granted.contains(&Capability::ReadTraffic) || !ext.granted.contains(&Capability::RunProgram) {
            return Ok((0, 0));
        }
        let rules = self.rules();
        let out_of_scope = ext.granted.contains(&Capability::ReadOutOfScope);
        let visible: Vec<&Exchange> = exchanges.iter().filter(|ex| out_of_scope || rules.in_scope(&ex.host)).collect();
        let ids: Vec<i64> = visible.iter().map(|ex| ex.id).collect();
        let done = self.store.extension_scanned(&ext.name, &ext.version, &ids).map_err(|e| e.to_string())?;
        let todo: Vec<&Exchange> = visible.into_iter().filter(|ex| !done.contains(&ex.id)).collect();
        let mut hits = 0;
        for chunk in todo.chunks(crate::program::BATCH) {
            let found = crate::program::scan(program, chunk)?;
            hits += found.values().map(Vec::len).sum::<usize>();
            let rows: Vec<(i64, String)> = found.iter().map(|(id, h)| (*id, serde_json::to_string(h).unwrap_or_else(|_| "[]".into()))).collect();
            self.store.put_extension_hits(&ext.name, &ext.version, &rows).map_err(|e| e.to_string())?;
        }
        if !todo.is_empty() {
            self.program_problems.lock().unwrap().remove(&ext.name);
        }
        Ok((todo.len(), hits))
    }

    /// Runs the extension's enumeration tool over every accepted scope domain
    /// and records the subdomains it finds as scope *suggestions*. Returns how
    /// many it suggested. The tool reads public sources; Plonix sends nothing
    /// to the target and brings nothing into scope — each suggestion waits for
    /// the user's own accept/reject decision.
    fn discover_subdomains(&self, ext: &Loaded) -> std::result::Result<usize, String> {
        let Runner::Program(program) = &ext.runner else { return Ok(0) };
        if !ext.granted.contains(&Capability::RunProgram) || !ext.granted.contains(&Capability::SuggestScope) {
            return Err(not_allowed(ext, program));
        }
        let rules = self.rules();
        // The accepted domains to enumerate, deduplicated so a seed and one of
        // its own subdomains are not both enumerated.
        let mut domains: Vec<String> = rules
            .rules
            .iter()
            .filter(|r| r.decision == Decision::Accepted)
            .map(|r| crate::scope::normalize_host(&r.pattern))
            .filter(|h| !h.is_empty())
            .collect();
        domains.sort();
        domains.dedup();
        let all = domains.clone();
        domains.retain(|d| !all.iter().any(|other| other != d && crate::scope::is_subdomain_of(d, other)));
        if domains.is_empty() {
            return Err("no accepted scope domain to enumerate yet. Accept a domain in Scope first.".into());
        }
        let now = crate::model::now_ms();
        let mut suggested = 0;
        for domain in domains {
            let found = crate::program::enumerate(program, &domain)?;
            let evidence: Vec<crate::scope::NewEvidence> = found
                .into_iter()
                .filter(|h| rules.decide_domain(h) == Decision::Unknown && !crate::scope::is_noise(h))
                .map(|h| crate::scope::NewEvidence {
                    domain: h,
                    kind: crate::scope::EvidenceKind::Discovered,
                    via: domain.clone(),
                    detail: format!("looked up by {}", ext.name),
                })
                .collect();
            suggested += evidence.len();
            self.store.add_suggestions(&evidence, now).map_err(|e| e.to_string())?;
        }
        Ok(suggested)
    }

    /// Probes one in-scope endpoint for undocumented query parameters. Plonix
    /// sends every request itself through [`Engine::send`], so each is
    /// scope-enforced and recorded; the extension supplies only the candidate
    /// names, never a payload. Parameters that change the response become one
    /// unconfirmed finding for a person to review.
    pub async fn run_param_probe(&self, name: &str, target_url: &str) -> std::result::Result<ParamProbeReport, SendError> {
        let set = self.extensions();
        let ext = set
            .extensions
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| SendError::BadRequest(format!("no enabled extension named `{}` (see `plonix extensions`)", crate::detect::clean(name, 64))))?
            .clone();
        let is_probe = matches!(&ext.runner, Runner::Program(p) if crate::program::get(p).map(|p| p.kind) == Some(crate::program::Kind::Probe));
        if !is_probe {
            return Err(SendError::BadRequest(format!("{name} is not a parameter probe")));
        }
        if !ext.granted.contains(&Capability::ScopedRequests) || !ext.granted.contains(&Capability::RunProgram) {
            return Err(SendError::BadRequest(match &ext.runner {
                Runner::Program(p) => not_allowed(&ext, p),
                Runner::Wasm(_) => format!("{name} is not allowed to send requests"),
            }));
        }
        self.check_automation("parameter probes")?;
        let initiator = extension_author(name);
        const MARKER: &str = "plnxprobe7q";

        // Baseline, then a calibration probe with a name nothing should read,
        // to tell whether responses are stable enough to compare by length.
        let baseline = self.send_scan(probe_request(target_url, None, MARKER), &initiator).await?;
        let calib = self.send_scan(probe_request(target_url, Some("plnxcalib9z"), MARKER), &initiator).await.ok();
        let base_len = baseline.resp_body.len() as i64;
        let base_status = baseline.status;
        let calib_len = calib.as_ref().map(|c| c.resp_body.len() as i64);
        let length_reliable = calib.as_ref().map_or(true, |c| c.status == base_status && (c.resp_body.len() as i64 - base_len).abs() <= 8);

        let mut report = ParamProbeReport { target: target_url.to_string(), baseline_exchange: baseline.id, ..Default::default() };
        let mut influential: Vec<(String, i64)> = vec![];
        for cand in crate::program::PARAM_NAMES.iter().take(crate::program::MAX_PROBES) {
            let ex = match self.send_scan(probe_request(target_url, Some(cand), MARKER), &initiator).await {
                Ok(ex) => ex,
                // Scope is the same host throughout: a refusal stops the probe.
                Err(e @ SendError::OutOfScope { .. }) => return Err(e),
                Err(_) => {
                    report.errors += 1;
                    continue;
                }
            };
            report.sent += 1;
            let len = ex.resp_body.len() as i64;
            let reflected = String::from_utf8_lossy(&ex.resp_body).contains(MARKER);
            let status_changed = ex.status != base_status;
            let len_changed = length_reliable && (len - base_len).abs() > 64 && Some(len) != calib_len;
            if reflected || status_changed || len_changed {
                influential.push((cand.to_string(), ex.id));
            }
        }
        report.influential = influential.iter().map(|(n, _)| n.clone()).collect();

        if !influential.is_empty() {
            let host = crate::scope::normalize_host(target_url);
            let names = report.influential.join(", ");
            let title = format!("Undocumented parameters on {host}");
            let mut ids = vec![baseline.id];
            ids.extend(influential.iter().map(|(_, id)| *id));
            let description = format!(
                "The parameter probe found query parameters that change this endpoint's response, so the application reads them although they are not documented here: {names}.\n\n\
                 Each was sent once as `name={MARKER}` through Plonix's scope-gated send path and compared with the baseline (exchange {}). Review whether any reach data or behaviour that should not be.\n\n\
                 Proposed by the extension {} {}. Not confirmed: check it before you rely on it.",
                baseline.id, ext.name, ext.version
            );
            let f = crate::model::NewFinding { title, severity: "info".into(), description, exchange_ids: ids };
            report.proposed = self.add_extension_findings(&ext.name, [f])? > 0;
        }
        Ok(report)
    }

    /// Adds findings an extension proposed, attributed to it, skipping
    /// titles it already proposed and ones that do not check out. Returns
    /// how many are new.
    fn add_extension_findings(&self, name: &str, findings: impl IntoIterator<Item = crate::model::NewFinding>) -> Result<usize> {
        let by = extension_author(name);
        let mut existing: BTreeSet<String> = self.store.findings()?.into_iter().filter(|f| f.created_by == by).map(|f| f.title).collect();
        let mut added = 0;
        for f in findings.into_iter().filter_map(|f| f.checked().ok()) {
            if existing.insert(f.title.clone()) {
                self.store.add_finding(&f, &by)?;
                added += 1;
            }
        }
        Ok(added)
    }

    /// Logs why a program extension could not run, once until it changes.
    fn program_problem(&self, ext: &Loaded, problem: &str) {
        let mut seen = self.program_problems.lock().unwrap();
        if seen.get(&ext.name).map(String::as_str) != Some(problem) {
            tracing::warn!("extension {} {}: {problem}", ext.name, ext.version);
            seen.insert(ext.name.clone(), problem.to_string());
        }
    }

    /// The limits each extension call runs under.
    pub fn set_extension_limits(&self, limits: sandbox::Limits) {
        *self.extension_limits.write().unwrap() = limits;
    }
}

/// What a program extension found in one exchange, as Lens insights: exposed
/// secrets, labelled with the extension's name.
fn program_insights(ext: &Loaded, json: &str) -> Vec<Insight> {
    let hits: Vec<crate::program::Hit> = serde_json::from_str(json).unwrap_or_default();
    hits.into_iter()
        .map(|h| {
            let mut notes = vec![format!("Found by the extension {} {}. Not checked with the service it belongs to.", ext.name, ext.version)];
            notes.extend(h.notes);
            Insight {
                kind: format!("extension:{}", ext.name),
                category: InsightCategory::Secret,
                label: crate::program::label(&h.detector),
                side: h.side,
                location: h.location,
                value: h.value,
                decoded: None,
                notes,
                count: 1,
            }
        })
        .collect()
}

/// Why a program extension did not run: the user has not allowed its
/// program yet, and how to allow it.
fn not_allowed(ext: &Loaded, program: &str) -> String {
    let what = match crate::program::get(program) {
        Some(p) if p.builtin => "send its requests".to_string(),
        _ => format!("run {program}"),
    };
    format!(
        "{} is not allowed to {what} yet. Allow it on its page in the Market, or run `plonix extensions allow {}`.",
        ext.name, ext.name
    )
}

/// How findings an extension proposed are attributed.
pub fn extension_author(name: &str) -> String {
    format!("extension:{name}")
}
