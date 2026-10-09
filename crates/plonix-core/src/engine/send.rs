//! Sending requests from Bench and replays. Every send is checked against scope first.

use super::*;
use std::time::Instant;

use base64::Engine as _;
use bytes::Bytes;

use crate::replace::{Passing, Reach, Target};

impl Engine {
    /// Sends an active request. Refused unless the target host is accepted.
    pub async fn send(&self, req: SendRequest, initiator: &str) -> Result<Exchange, SendError> {
        self.send_from(req, initiator, Reach::Bench).await
    }

    /// Like [`Self::send`], for Scans, crawls and extensions: the rules that
    /// apply to them may differ from the Bench's.
    pub async fn send_scan(&self, req: SendRequest, initiator: &str) -> Result<Exchange, SendError> {
        self.send_from(req, initiator, Reach::Scans).await
    }

    async fn send_from(&self, mut req: SendRequest, initiator: &str, reach: Reach) -> Result<Exchange, SendError> {
        let url = req.url.trim();
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| SendError::BadRequest(format!("absolute URL required, got '{url}'")))?;
        let scheme = scheme.to_ascii_lowercase();
        if scheme != "http" && scheme != "https" {
            return Err(SendError::BadRequest(format!("unsupported scheme '{scheme}'")));
        }
        let (authority, target) = match rest.find(['/', '?']) {
            Some(i) if rest[i..].starts_with('/') => (&rest[..i], rest[i..].to_string()),
            Some(i) => (&rest[..i], format!("/{}", &rest[i..])),
            None => (rest, "/".to_string()),
        };
        let host = scope::normalize_host(authority);
        let port = authority
            .rsplit_once(':')
            .filter(|(h, _)| !h.contains(':') || h.ends_with(']'))
            .and_then(|(_, p)| p.parse().ok())
            .unwrap_or(if scheme == "https" { 443 } else { 80 });

        // Sending as a saved user: its cookies and headers replace the auth
        // headers the draft carried. The values come from the project
        // database, so they are never round-tripped through the client.
        let as_user = match req.as_user.take().filter(|u| !u.is_empty()) {
            Some(uid) => {
                let users = self.store.saved_users().map_err(SendError::Other)?;
                let Some(user) = users.into_iter().find(|u| u.id == uid) else {
                    return Err(SendError::BadRequest(format!("there is no saved user '{uid}'")));
                };
                req.headers.retain(|(k, _)| !crate::users::is_auth_header(k));
                req.headers.extend(user.request_headers(&host, crate::users::now_secs()));
                Some(user)
            }
            None => None,
        };

        // The single choke point for active traffic: scope is enforced here.
        let decision = self.rules().decide(&host);
        if decision != Decision::Accepted {
            return Err(SendError::OutOfScope { host, decision: decision.as_str() });
        }
        // The program's rules of engagement: the headers it asks for, at the rate it allows.
        if let Some(guard) = self.program() {
            guard.add_headers(&mut req.headers);
            guard.pace().await;
        }

        let body: Vec<u8> = match (&req.body_base64, &req.body) {
            (Some(b64), _) => base64::engine::general_purpose::STANDARD
                .decode(b64)
                .map_err(|e| SendError::BadRequest(format!("body_base64: {e}")))?,
            (None, Some(s)) => s.clone().into_bytes(),
            (None, None) => vec![],
        };
        let split = |target: &str| match target.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (target.to_string(), String::new()),
        };
        let (mut target, mut body, mut method) = (target, body, req.method.to_ascii_uppercase());
        // Rules the user set for this kind of traffic (Settings › Match and replace).
        let rules = self.replace_rules();
        let (mut replaced, mut original_request) = (vec![], None);
        if rules.reaches(reach) {
            let (path, query) = split(&target);
            let before = Exchange {
                scheme: scheme.clone(),
                host: host.clone(),
                port,
                method: method.clone(),
                path,
                query,
                req_headers: req.headers.clone(),
                req_body: body.clone(),
                source: Some(Source::Replay),
                ..Default::default()
            };
            let ctx = Passing { reach, in_scope: true, ex: &before };
            replaced.extend(rules.request_line(ctx, &mut method, &mut target));
            replaced.extend(rules.headers(Target::RequestHeader, ctx, &mut req.headers));
            replaced.extend(rules.body(Target::RequestBody, ctx, &mut body));
            if !replaced.is_empty() {
                original_request = Some(crate::ask::request_text(&before, self.body_limit()).0);
            }
        }
        let (path, query) = split(&target);
        let started = Instant::now();
        let mut ex = Exchange {
            ts: now_ms(),
            scheme: scheme.clone(),
            host: host.clone(),
            port,
            method: method.clone(),
            path,
            query,
            req_headers: req.headers.clone(),
            req_body: body.clone(),
            source: Some(Source::Replay),
            initiator: Some(initiator.to_string()),
            original_request,
            ..Default::default()
        };
        let outbound = OutboundRequest { scheme, host, port, method, target, headers: req.headers, body: Bytes::from(body), extra_headers: vec![] };
        let result = match self.responder().and_then(|r| r(&outbound)) {
            Some(resp) => Ok(resp),
            None => self.upstream().send_capped(outbound, self.body_limit()).await,
        };
        ex.duration_ms = started.elapsed().as_millis() as i64;
        match result {
            Ok(up) => {
                ex.status = Some(up.status);
                ex.resp_headers = up.headers;
                ex.resp_body = up.body.to_vec();
                if rules.reaches(reach) {
                    self.replace_response(&rules, reach, &mut ex, &mut replaced);
                }
                ex.resp_truncated = up.truncated_from.is_some();
                ex.resp_size = up.truncated_from.map(|n| n as i64);
                ex.tls_sans = up.tls_sans;
                ex.http_version = up.version;
                ex.client_cert = up.client_cert;
            }
            Err(e) => ex.error = Some(format!("{e:#}")),
        }
        ex.replaced = replaced;
        if let Some(user) = &as_user {
            ex.replaced.insert(0, format!("{}{}", crate::users::SENT_AS, user.name));
            if let Err(e) = self.store.absorb_cookies(&user.id, &ex.host, &ex.resp_headers) {
                tracing::warn!("saved user {}: could not keep its cookies: {e:#}", user.id);
            }
        }
        let id = self.record(ex.clone())?;
        ex.id = id;
        Ok(ex)
    }

    /// Applies the response rules to a response the engine received: what
    /// the Bench or Scans see is then what the rules made of it. A
    /// compressed body is matched decoded and kept decoded when changed.
    fn replace_response(&self, rules: &RuleSet, reach: Reach, ex: &mut Exchange, replaced: &mut Vec<String>) {
        let seen = ex.clone();
        let ctx = Passing { reach, in_scope: true, ex: &seen };
        replaced.extend(rules.headers(Target::ResponseHeader, ctx, &mut ex.resp_headers));
        if !rules.has(Target::ResponseBody, ctx) {
            return;
        }
        let encoded = crate::model::header(&ex.resp_headers, "content-encoding").is_some();
        let decoded = if encoded { crate::codec::decode_whole(&ex.resp_headers, &ex.resp_body, self.body_limit()) } else { Some(ex.resp_body.clone()) };
        if let Some(mut text) = decoded {
            let changed = rules.body(Target::ResponseBody, ctx, &mut text);
            if !changed.is_empty() {
                ex.resp_headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-encoding") && !k.eq_ignore_ascii_case("content-length"));
                ex.resp_headers.push(("Content-Length".into(), text.len().to_string()));
                ex.resp_body = text;
                replaced.extend(changed);
            }
        }
    }

    /// Replays a stored exchange with optional modifications (same scope rules as `send`).
    pub async fn replay(&self, req: ReplayRequest, initiator: &str) -> Result<Exchange, SendError> {
        let orig = self.store.get_exchange(req.id)?.ok_or(SendError::NotFound(req.id))?;
        let mut headers = orig.req_headers.clone();
        headers.retain(|(k, _)| !req.remove_headers.iter().any(|r| r.eq_ignore_ascii_case(k)));
        for (k, v) in &req.set_headers {
            match headers.iter_mut().find(|(hk, _)| hk.eq_ignore_ascii_case(k)) {
                Some(slot) => slot.1 = v.clone(),
                None => headers.push((k.clone(), v.clone())),
            }
        }
        let target = req.target.clone().unwrap_or_else(|| {
            if orig.query.is_empty() { orig.path.clone() } else { format!("{}?{}", orig.path, orig.query) }
        });
        if orig.req_truncated && req.body.is_none() {
            return Err(SendError::BadRequest(format!(
                "only the first {} bytes of request {}'s body were recorded, so it cannot be sent again as it was; give the body to send",
                orig.req_body.len(),
                orig.id
            )));
        }
        let url = Exchange { path: target, query: String::new(), ..orig.clone() }.url();
        let (body, body_base64) = match req.body {
            Some(b) => (Some(b), None),
            None => (None, Some(base64::engine::general_purpose::STANDARD.encode(&orig.req_body))),
        };
        self.send(
            SendRequest { method: req.method.clone().unwrap_or(orig.method.clone()), url, headers, body, body_base64, as_user: None },
            initiator,
        )
        .await
    }
}
