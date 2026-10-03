//! Intercepting forward proxy.
//!
//! Plain HTTP requests arrive in absolute form and are forwarded. `CONNECT`
//! tunnels are terminated with a leaf certificate minted by the local CA, and
//! the HTTP/1.1 requests inside are forwarded over a fresh TLS connection.
//! Every request/response pair is recorded, regardless of scope.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use http::{Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

use crate::ca::LeafResolver;
use crate::engine::Engine;
use crate::model::{Exchange, Headers, Source, now_ms};
use crate::upstream::{HOP_BY_HOP, OutboundRequest};

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
    if let Err(e) = hyper::server::conn::http1::Builder::new()
        .preserve_header_case(true)
        .serve_connection(io, svc)
        .with_upgrades()
        .await
    {
        tracing::debug!("proxy connection ended: {e}");
    }
}

async fn handle(req: Request<Incoming>, ctx: Ctx) -> Result<Response<Full<Bytes>>, Infallible> {
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
    let req_headers: Headers = req
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    let req_body = match req.into_body().collect().await {
        Ok(b) => b.to_bytes(),
        Err(e) => return Ok(error_page(StatusCode::BAD_REQUEST, &format!("could not read request body: {e}"))),
    };

    let outbound = OutboundRequest {
        scheme: scheme.clone(),
        host: host.clone(),
        port,
        method: method.clone(),
        target,
        headers: req_headers.clone(),
        body: req_body.clone(),
    };
    let result = ctx.engine.upstream.send(outbound).await;
    let mut ex = Exchange {
        ts,
        scheme,
        host,
        port,
        method,
        path,
        query,
        req_headers,
        req_body: req_body.to_vec(),
        source: Some(Source::Proxy),
        ..Default::default()
    };
    ex.duration_ms = started.elapsed().as_millis() as i64;

    let response = match result {
        Ok(up) => {
            ex.status = Some(up.status);
            ex.resp_headers = up.headers.clone();
            ex.resp_body = up.body.to_vec();
            ex.tls_sans = up.tls_sans;
            let mut builder = Response::builder().status(up.status);
            for (k, v) in &up.headers {
                if !HOP_BY_HOP.contains(&k.to_ascii_lowercase().as_str()) {
                    builder = builder.header(k.as_str(), v.as_str());
                }
            }
            builder
                .body(Full::new(up.body))
                .unwrap_or_else(|e| error_page(StatusCode::BAD_GATEWAY, &format!("invalid upstream response: {e}")))
        }
        Err(e) => {
            let msg = format!("{e:#}");
            ex.error = Some(msg.clone());
            let hint = if msg.contains("certificate") || msg.contains("UnknownIssuer") {
                "\n\nThe upstream certificate is not trusted. For staging hosts with self-signed certificates, \
                 restart the engine with `plonix start --insecure-upstream`."
            } else {
                ""
            };
            error_page(StatusCode::BAD_GATEWAY, &format!("Plonix could not reach {}: {msg}{hint}", ex.host))
        }
    };

    ctx.engine.enqueue(ex);
    Ok(response)
}

fn connect(req: Request<Incoming>, ctx: Ctx) -> Response<Full<Bytes>> {
    let Some(authority) = req.uri().authority().cloned() else {
        return error_page(StatusCode::BAD_REQUEST, "CONNECT needs host:port");
    };
    let host = authority.host().trim_matches(['[', ']']).to_ascii_lowercase();
    let port = authority.port_u16().unwrap_or(443);
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
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
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
    Response::new(Full::new(Bytes::new()))
}

fn is_self(host: &str, port: u16, local: SocketAddr) -> bool {
    port == local.port() && matches!(host, "127.0.0.1" | "localhost" | "::1" | "0.0.0.0")
}

fn local_page(path: &str, ctx: &Ctx) -> Response<Full<Bytes>> {
    match path {
        "/ca.pem" | "/ca.crt" | "/cert" => Response::builder()
            .header("content-type", "application/x-x509-ca-cert")
            .header("content-disposition", "attachment; filename=\"plonix-ca.pem\"")
            .body(Full::new(Bytes::from(ctx.engine.ca.ca_pem().to_string())))
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
            Response::builder().header("content-type", "text/html; charset=utf-8").body(Full::new(Bytes::from(html))).unwrap()
        }
    }
}

fn error_page(status: StatusCode, msg: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .header("x-plonix-error", "1")
        .body(Full::new(Bytes::from(msg.to_string())))
        .unwrap()
}
