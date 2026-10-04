//! Intercepting forward proxy.
//!
//! Plain HTTP requests arrive in absolute form and are forwarded. `CONNECT`
//! tunnels are terminated with a leaf certificate minted by the local CA, and
//! the requests inside (HTTP/2 or HTTP/1.1, as the client chooses) are
//! forwarded over a fresh TLS connection, in HTTP/2 when the server offers it.
//! Every request/response pair is recorded, regardless of scope.
//!
//! Bodies stream through: the client gets each part of a response as the
//! server sends it (event streams, long polls, large downloads), while the
//! first [`Engine::body_limit`] bytes of each body are kept for the record.
//! An exchange is recorded once its response has ended.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::Instant;

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::BodyExt;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::ca::LeafResolver;
use crate::engine::Engine;
use crate::model::{Exchange, Headers, Source, now_ms};
use crate::upstream::{BoxError, HOP_BY_HOP, OutboundRequest, StreamBody, Upgrade, full_body};

type ProxyResponse = Response<StreamBody>;

/// Requests to this host through the proxy are answered by Plonix itself.
pub const MAGIC_HOST: &str = "plonix";

#[derive(Clone)]
struct Ctx {
    engine: Arc<Engine>,
    local: SocketAddr,
    /// Set inside a CONNECT tunnel: the target host and port.
    tunnel: Option<(String, u16)>,
}

pub async fn serve(listener: TcpListener, engine: Arc<Engine>) {
    let local = listener.local_addr().expect("bound listener");
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("proxy accept failed: {e}");
                continue;
            }
        };
        stream.set_nodelay(true).ok();
        let ctx = Ctx { engine: engine.clone(), local, tunnel: None };
        tokio::spawn(serve_conn(TokioIo::new(stream), ctx));
    }
}

async fn serve_conn<I>(io: I, ctx: Ctx)
where
    I: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let svc = service_fn(move |req| handle(req, ctx.clone()));
    let mut builder = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new());
    builder.http1().preserve_header_case(true);
    if let Err(e) = builder.serve_connection_with_upgrades(io, svc).await {
        tracing::debug!("proxy connection ended: {e}");
    }
}

async fn handle(req: Request<Incoming>, ctx: Ctx) -> Result<ProxyResponse, Infallible> {
    if req.method() == Method::CONNECT && ctx.tunnel.is_none() {
        return Ok(connect(req, ctx));
    }

    let (scheme, host, port) = match &ctx.tunnel {
        Some((h, p)) => ("https".to_string(), h.clone(), *p),
        None => match (req.uri().scheme_str(), req.uri().host()) {
            (Some(s), Some(h)) => {
                let s = s.to_ascii_lowercase();
                let port = req.uri().port_u16().unwrap_or(if s == "https" { 443 } else { 80 });
                (s, h.trim_matches(['[', ']']).to_ascii_lowercase(), port)
            }
            // Origin-form request: the client is talking to the proxy directly.
            _ => return Ok(local_page(req.uri().path(), &ctx)),
        },
    };
    if host == MAGIC_HOST || (is_self(&host, port, ctx.local) && ctx.tunnel.is_none()) {
        return Ok(local_page(req.uri().path(), &ctx));
    }

    let started = Instant::now();
    let ts = now_ms();
    let method = req.method().as_str().to_string();
    let path = req.uri().path().to_string();
    let query = req.uri().query().unwrap_or("").to_string();
    let target = match req.uri().path_and_query() {
        Some(pq) => pq.as_str().to_string(),
        None => "/".to_string(),
    };
    let mut req_headers: Headers = req
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    if req.version() == http::Version::HTTP_2 {
        from_http2(&mut req_headers, req.uri().authority().map(|a| a.as_str()));
    }
    let client_version = crate::upstream::version_name(req.version()).to_string();
    let limit = ctx.engine.body_limit();
    let req_cap = Arc::new(Mutex::new(Captured::new(limit)));
    let mut outbound = OutboundRequest {
        scheme: scheme.clone(),
        host: host.clone(),
        port,
        method: method.clone(),
        target,
        headers: req_headers.clone(),
        body: Bytes::new(),
        extra_headers: vec![],
    };
    let mut pending = Pending {
        engine: ctx.engine.clone(),
        started,
        req: req_cap.clone(),
        resp: Arc::new(Mutex::new(Captured::new(limit))),
        ex: Exchange {
            ts,
            scheme,
            host,
            port,
            method,
            path,
            query,
            req_headers,
            source: Some(Source::Proxy),
            http_version: client_version,
            ..Default::default()
        },
        taken: false,
    };

    if req.version() <= http::Version::HTTP_11 && crate::websocket::is_handshake(req.headers()) {
        return Ok(websocket(req, ctx, outbound, pending).await);
    }

    // A body of known length within the limit is read first, as before, so a
    // request that fails still shows what it carried. Longer and open-ended
    // bodies (uploads, streams) go through as they arrive.
    let stream = match req.body().size_hint().exact() {
        Some(n) if n <= limit as u64 => match req.into_body().collect().await {
            Ok(b) => {
                outbound.body = b.to_bytes();
                let mut cap = req_cap.lock().unwrap();
                cap.add(&outbound.body);
                cap.done = true;
                None
            }
            Err(e) => {
                pending.ex.error = Some(format!("could not read the request body: {e}"));
                return Ok(error_page(StatusCode::BAD_REQUEST, &format!("could not read request body: {e}")));
            }
        },
        _ => Some(Tee::new(req.into_body(), req_cap, None).boxed()),
    };

    Ok(match ctx.engine.upstream().open(outbound, stream).await {
        Ok(up) => {
            pending.ex.tls_sans = up.tls_sans;
            pending.ex.http_version = up.version;
            forward(up.status, up.headers, up.body, pending)
        }
        Err(e) => unreachable(e, pending),
    })
}

/// Records an HTTP/2 request's headers the way HTTP/1.1 writes them, which is
/// also how they are forwarded to an HTTP/1.1 server and replayed: the
/// `:authority` becomes `Host`, and cookies split over several fields are
/// joined into one `Cookie` header.
fn from_http2(headers: &mut Headers, authority: Option<&str>) {
    let cookies: Vec<String> = headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("cookie")).map(|(_, v)| v.clone()).collect();
    if cookies.len() > 1 {
        let at = headers.iter().position(|(k, _)| k.eq_ignore_ascii_case("cookie")).unwrap_or(0);
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("cookie"));
        headers.insert(at.min(headers.len()), ("cookie".into(), cookies.join("; ")));
    }
    if let Some(a) = authority
        && !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("host"))
    {
        headers.insert(0, ("host".into(), a.to_string()));
    }
}

/// Answers the client with the server's response, streaming its body.
fn forward(status: u16, headers: Headers, body: Incoming, mut pending: Pending) -> ProxyResponse {
    pending.ex.status = Some(status);
    let mut builder = Response::builder().status(status);
    for (k, v) in &headers {
        if !HOP_BY_HOP.contains(&k.to_ascii_lowercase().as_str()) {
            builder = builder.header(k.as_str(), v.as_str());
        }
    }
    pending.ex.resp_headers = headers;
    let resp_cap = pending.resp.clone();
    builder
        .body(Tee::new(body, resp_cap, Some(pending)).boxed())
        .unwrap_or_else(|e| error_page(StatusCode::BAD_GATEWAY, &format!("invalid upstream response: {e}")))
}

/// Answers the client when the server could not be reached.
fn unreachable(e: anyhow::Error, mut pending: Pending) -> ProxyResponse {
    let msg = format!("{e:#}");
    pending.ex.error = Some(msg.clone());
    let hint = if msg.contains("certificate") || msg.contains("UnknownIssuer") {
        "\n\nThe upstream certificate is not trusted. For staging hosts with self-signed certificates, \
         turn off Settings > Proxy > Check server certificates."
    } else {
        ""
    };
    error_page(StatusCode::BAD_GATEWAY, &format!("Plonix could not reach {}: {msg}{hint}", pending.ex.host))
}

/// Forwards a WebSocket handshake. When the server switches protocols, the
/// handshake is recorded right away and the connection is relayed, with its
/// messages recorded against it (see [`crate::websocket`]).
async fn websocket(mut req: Request<Incoming>, ctx: Ctx, outbound: OutboundRequest, mut pending: Pending) -> ProxyResponse {
    let client_upgrade = hyper::upgrade::on(&mut req);
    let protocol = crate::websocket::requested_protocol(&pending.ex.req_headers);
    let up = match ctx.engine.upstream().upgrade(outbound, &protocol).await {
        Ok(up) => up,
        Err(e) => return unreachable(e, pending),
    };
    pending.ex.tls_sans = up.tls_sans;
    pending.ex.http_version = "HTTP/1.1".into();
    let server = match up.outcome {
        Upgrade::Refused(body) => return forward(up.status, up.headers, body, pending),
        Upgrade::Switched(server) => server,
    };
    let mut builder = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
    for (k, v) in &up.headers {
        if !HOP_BY_HOP.contains(&k.to_ascii_lowercase().as_str()) {
            builder = builder.header(k.as_str(), v.as_str());
        }
    }
    let switched_to = crate::model::header(&up.headers, "upgrade").unwrap_or(&protocol).to_string();
    let response = builder.header("connection", "upgrade").header("upgrade", switched_to).body(full_body(Bytes::new()));
    let response = match response {
        Ok(r) => r,
        Err(e) => return error_page(StatusCode::BAD_GATEWAY, &format!("invalid upstream response: {e}")),
    };
    pending.ex.status = Some(up.status);
    pending.ex.resp_headers = up.headers.clone();
    // Record the handshake now: the connection can stay open for hours.
    let engine = ctx.engine.clone();
    let Some(ex) = pending.take() else { return response };
    let exchange_id = engine.enqueue_for_id(ex);
    tokio::spawn(async move {
        match client_upgrade.await {
            Ok(client) => crate::websocket::relay(client, server, engine, exchange_id, &up.headers).await,
            Err(e) => tracing::debug!("WebSocket upgrade with the client failed: {e}"),
        }
    });
    response
}

/// The part of a body kept for the record, and how much went through.
#[derive(Debug, Default)]
struct Captured {
    data: Vec<u8>,
    limit: usize,
    seen: u64,
    truncated: bool,
    /// The body ended normally, so `seen` is its full size.
    done: bool,
}

impl Captured {
    fn new(limit: usize) -> Self {
        Self { limit, ..Default::default() }
    }

    fn add(&mut self, chunk: &[u8]) {
        self.seen += chunk.len() as u64;
        let room = self.limit.saturating_sub(self.data.len());
        if chunk.len() > room {
            self.truncated = true;
        }
        self.data.extend_from_slice(&chunk[..chunk.len().min(room)]);
    }

    /// Moves the kept part into an exchange: (body, truncated, full size).
    fn take(&mut self) -> (Vec<u8>, bool, Option<i64>) {
        let size = (self.truncated && self.done).then_some(self.seen as i64);
        (std::mem::take(&mut self.data), self.truncated, size)
    }
}

/// An exchange waiting for its response to end. Dropping it records the
/// exchange, so it is recorded however the stream ends: normally, with an
/// error, or because the client went away.
struct Pending {
    engine: Arc<Engine>,
    started: Instant,
    req: Arc<Mutex<Captured>>,
    resp: Arc<Mutex<Captured>>,
    ex: Exchange,
    taken: bool,
}

impl Pending {
    /// The exchange as it stands. Once taken, dropping records nothing.
    fn take(&mut self) -> Option<Exchange> {
        if std::mem::replace(&mut self.taken, true) {
            return None;
        }
        let mut ex = std::mem::take(&mut self.ex);
        ex.duration_ms = self.started.elapsed().as_millis() as i64;
        (ex.req_body, ex.req_truncated, ex.req_size) = self.req.lock().unwrap().take();
        (ex.resp_body, ex.resp_truncated, ex.resp_size) = self.resp.lock().unwrap().take();
        Some(ex)
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(ex) = self.take() {
            self.engine.enqueue(ex);
        }
    }
}

/// Passes a body through unchanged while keeping its start in a [`Captured`].
struct Tee<B> {
    inner: B,
    cap: Arc<Mutex<Captured>>,
    /// Recorded when the body ends (response bodies only).
    pending: Option<Pending>,
}

impl<B> Tee<B> {
    fn new(inner: B, cap: Arc<Mutex<Captured>>, pending: Option<Pending>) -> Self {
        Self { inner, cap, pending }
    }
}

impl<B> Body for Tee<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Into<BoxError>,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        match ready!(Pin::new(&mut this.inner).poll_frame(cx)) {
            Some(Ok(frame)) => {
                if let Some(data) = frame.data_ref() {
                    this.cap.lock().unwrap().add(data);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Some(Err(e)) => {
                let e = e.into();
                if let Some(mut p) = this.pending.take() {
                    p.ex.error = Some(format!("the response stopped early: {e}"));
                }
                Poll::Ready(Some(Err(e)))
            }
            None => {
                this.cap.lock().unwrap().done = true;
                this.pending.take();
                Poll::Ready(None)
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        let end = self.inner.is_end_stream();
        if end {
            self.cap.lock().unwrap().done = true;
        }
        end
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

fn connect(req: Request<Incoming>, ctx: Ctx) -> ProxyResponse {
    let Some(authority) = req.uri().authority().cloned() else {
        return error_page(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
    };
    let host = authority.host().trim_matches(['[', ']']).to_ascii_lowercase();
    let port = authority.port_u16().unwrap_or(443);
    if !ctx.engine.decrypts(&host) {
        return passthrough(req, ctx, host, port);
    }
    tokio::spawn(async move {
        let upgraded = match hyper::upgrade::on(req).await {
            Ok(u) => u,
            Err(e) => {
                tracing::debug!("CONNECT upgrade failed: {e}");
                return;
            }
        };
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = match rustls::ServerConfig::builder_with_provider(provider).with_safe_default_protocol_versions() {
            Ok(b) => b
                .with_no_client_auth()
                .with_cert_resolver(Arc::new(LeafResolver { ca: ctx.engine.ca.clone(), fallback_host: host.clone() })),
            Err(e) => {
                tracing::error!("TLS config: {e}");
                return;
            }
        };
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        let tls = match TlsAcceptor::from(Arc::new(config)).accept(TokioIo::new(upgraded)).await {
            Ok(t) => t,
            Err(e) => {
                // Usually the client does not trust the Plonix CA, or pins certificates.
                tracing::info!("TLS handshake with client for {host} failed: {e}");
                return;
            }
        };
        let inner = Ctx { tunnel: Some((host, port)), ..ctx };
        serve_conn(TokioIo::new(tls), inner).await;
    });
    Response::new(full_body(Bytes::new()))
}

/// Tunnels a CONNECT without decrypting it. Nothing inside is recorded.
fn passthrough(req: Request<Incoming>, ctx: Ctx, host: String, port: u16) -> ProxyResponse {
    tokio::spawn(async move {
        let mut server = match ctx.engine.upstream().connect(&host, port).await {
            Ok(s) => s,
            Err(e) => {
                tracing::info!("tunnel to {host}:{port} failed: {e:#}");
                return;
            }
        };
        let Ok(upgraded) = hyper::upgrade::on(req).await else { return };
        let mut client = TokioIo::new(upgraded);
        let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
    });
    Response::new(full_body(Bytes::new()))
}

fn is_self(host: &str, port: u16, local: SocketAddr) -> bool {
    port == local.port() && matches!(host, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0")
}

fn local_page(path: &str, ctx: &Ctx) -> ProxyResponse {
    match path {
        "/ca.pem" | "/ca.crt" | "/cert" => Response::builder()
            .header("content-type", "application/x-x509-ca-cert")
            .header("content-disposition", "attachment; filename=\"plonix-ca.pem\"")
            .body(full_body(ctx.engine.ca.ca_pem().to_string()))
            .unwrap(),
        _ => {
            let html = format!(
                "<!doctype html><meta charset=utf-8><title>Plonix</title>\
                 <body style=\"font:16px -apple-system,system-ui,sans-serif;max-width:40em;margin:3em auto\">\
                 <h1>Plonix proxy is running</h1><p>Project <b>{}</b>, {} requests captured.</p>\
                 <p><a href=\"/ca.pem\">Download the Plonix CA certificate</a> and trust it to intercept HTTPS.</p>\
                 <p>Fingerprint (SHA-256):<br><code>{}</code></p></body>",
                ctx.engine.project,
                ctx.engine.store.count().unwrap_or(0),
                ctx.engine.ca.fingerprint()
            );
            Response::builder().header("content-type", "text/html; charset=utf-8").body(full_body(html)).unwrap()
        }
    }
}

fn error_page(status: StatusCode, msg: &str) -> ProxyResponse {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("x-plonix-error", "1")
        .body(full_body(msg.to_string()))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_keeps_the_start_and_counts_the_rest() {
        let mut c = Captured::new(5);
        c.add(b"abc");
        c.add(b"defg");
        c.add(b"hi");
        assert_eq!(c.take(), (b"abcde".to_vec(), true, None), "size unknown until the body ends");
        let mut c = Captured::new(5);
        c.add(b"abcdefgh");
        c.done = true;
        assert_eq!(c.take(), (b"abcde".to_vec(), true, Some(8)));
        let mut c = Captured::new(5);
        c.add(b"abcde");
        c.done = true;
        assert_eq!(c.take(), (b"abcde".to_vec(), false, None), "a body that fits is not cut");
    }

    #[test]
    fn http2_headers_are_recorded_like_http1() {
        let mut h: Headers = vec![("cookie".into(), "a=1".into()), ("accept".into(), "*/*".into()), ("cookie".into(), "b=2".into())];
        from_http2(&mut h, Some("app.test:8443"));
        assert_eq!(
            h,
            vec![("host".into(), "app.test:8443".into()), ("cookie".into(), "a=1; b=2".into()), ("accept".into(), "*/*".into())]
        );
    }
}
