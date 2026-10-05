//! Outbound HTTP client used by the proxy and for active requests.
//!
//! Connects directly, or through an upstream proxy (HTTP `CONNECT` or
//! SOCKS5) when the project's proxy settings name one. HTTPS servers that
//! offer HTTP/2 are spoken to in HTTP/2, others in HTTP/1.1; plain HTTP is
//! always HTTP/1.1.
//!
//! [`Upstream::send`] reads the whole response; [`Upstream::open`] returns
//! as soon as the response head arrives and leaves the body streaming, which
//! the proxy uses so event streams and downloads reach the client as they come.
//!
//! When a server asks for a client certificate and the project has one for
//! its host (see [`crate::clientcert`]), it is presented, and the response
//! notes which one went out.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine as _;
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::clientcert::{ClientCerts, Offer};
use crate::model::Headers;
use crate::settings::ProxySettings;

/// Headers that describe a single hop and must not be forwarded.
pub const HOP_BY_HOP: &[&str] = &[
    "connection",
    "proxy-connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "proxy-authorization",
    "proxy-authenticate",
];

#[derive(Debug, Clone)]
pub struct OutboundRequest {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub method: String,
    /// Path plus optional `?query`.
    pub target: String,
    pub headers: Headers,
    pub body: Bytes,
    /// Added after hop-by-hop headers are removed (credentials for the next hop).
    pub extra_headers: Headers,
}

#[derive(Debug, Clone)]
pub struct InboundResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Bytes,
    pub tls_sans: Vec<String>,
    /// The protocol spoken with the server, such as `HTTP/2`.
    pub version: String,
    /// The client certificate presented, when the server asked for one.
    pub client_cert: Option<String>,
}

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// A request or response body that may still be arriving.
pub type StreamBody = BoxBody<Bytes, BoxError>;

/// A whole body, as a [`StreamBody`].
pub fn full_body(b: impl Into<Bytes>) -> StreamBody {
    Full::new(b.into()).map_err(|never| match never {}).boxed()
}

/// The answer to a request to switch protocols (a WebSocket handshake).
pub struct UpgradeResponse {
    pub status: u16,
    pub headers: Headers,
    pub tls_sans: Vec<String>,
    pub client_cert: Option<String>,
    pub outcome: Upgrade,
}

pub enum Upgrade {
    /// The server switched: the connection now carries the new protocol.
    Switched(hyper::upgrade::Upgraded),
    /// The server answered with an ordinary response instead.
    Refused(Incoming),
}

/// A response whose head has arrived; the body is read as it comes.
#[derive(Debug)]
pub struct StreamingResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Incoming,
    pub tls_sans: Vec<String>,
    pub version: String,
    pub client_cert: Option<String>,
}

/// How a protocol version is shown and recorded.
pub fn version_name(v: http::Version) -> &'static str {
    match v {
        http::Version::HTTP_09 => "HTTP/0.9",
        http::Version::HTTP_10 => "HTTP/1.0",
        http::Version::HTTP_2 => "HTTP/2",
        http::Version::HTTP_3 => "HTTP/3",
        _ => "HTTP/1.1",
    }
}

/// Another proxy that outbound connections go through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyServer {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    /// An HTTP proxy, reached with `CONNECT` for every request.
    Http,
    Socks5,
}

impl ProxyServer {
    /// Parses `http://host:port` or `socks5://host:port` (credentials may be
    /// given in the URL or separately).
    pub fn parse(url: &str) -> Result<Self, String> {
        let url = url.trim();
        let (scheme, rest) = url.split_once("://").ok_or("must start with http:// or socks5://")?;
        let kind = match scheme.to_ascii_lowercase().as_str() {
            "http" => ProxyKind::Http,
            "socks5" | "socks5h" => ProxyKind::Socks5,
            other => return Err(format!("{other}:// proxies are not supported; use http:// or socks5://")),
        };
        let rest = rest.trim_end_matches('/');
        let (creds, authority) = match rest.rsplit_once('@') {
            Some((c, a)) => (Some(c), a),
            None => (None, rest),
        };
        let (host, port) = authority.rsplit_once(':').ok_or("needs a port, such as :3128")?;
        let host = host.trim_matches(['[', ']']).to_string();
        let port: u16 = port.parse().map_err(|_| "the port must be a number")?;
        if host.is_empty() || host.contains(['/', ' ']) || port == 0 {
            return Err("needs a host and port, such as proxy.example:3128".into());
        }
        let (username, password) = match creds.map(|c| c.split_once(':').unwrap_or((c, ""))) {
            Some((u, p)) => (u.to_string(), p.to_string()),
            None => (String::new(), String::new()),
        };
        Ok(Self { kind, host, port, username, password })
    }
}

/// How the client reaches servers.
#[derive(Debug, Clone)]
pub struct UpstreamOptions {
    /// Skip certificate verification (staging hosts with self-signed certificates).
    pub insecure: bool,
    /// Trusted in addition to the system and Mozilla roots.
    pub extra_roots: Vec<CertificateDer<'static>>,
    pub proxy: Option<ProxyServer>,
    /// Hosts reached directly even when `proxy` is set (`*.x` matches subdomains).
    pub bypass: Vec<String>,
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
}

impl Default for UpstreamOptions {
    fn default() -> Self {
        Self {
            insecure: false,
            extra_roots: vec![],
            proxy: None,
            bypass: vec![],
            connect_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(120),
        }
    }
}

impl UpstreamOptions {
    pub fn from_settings(p: &ProxySettings) -> Result<Self> {
        let proxy = if p.upstream_proxy.trim().is_empty() {
            None
        } else {
            let mut server = ProxyServer::parse(&p.upstream_proxy).map_err(|e| anyhow!("upstream proxy: {e}"))?;
            if !p.upstream_username.is_empty() {
                server.username = p.upstream_username.clone();
                server.password = p.upstream_password.clone();
            }
            Some(server)
        };
        Ok(Self {
            insecure: !p.verify_upstream_tls,
            extra_roots: vec![],
            proxy,
            bypass: p.upstream_bypass.clone(),
            connect_timeout: Duration::from_secs(p.connect_timeout_s),
            total_timeout: Duration::from_secs(p.request_timeout_s),
        })
    }
}

/// True when `host` matches one of `patterns` (`example.com`, `*.example.com`).
pub fn host_matches(host: &str, patterns: &[String]) -> bool {
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    patterns.iter().any(|p| {
        let p = p.trim().trim_matches(['[', ']']).to_ascii_lowercase();
        match p.strip_prefix("*.") {
            Some(base) => host == base || host.ends_with(&format!(".{base}")),
            None => !p.is_empty() && host == p,
        }
    })
}

pub struct Upstream {
    /// Offers HTTP/2 and HTTP/1.1.
    tls: Arc<ClientConfig>,
    /// Offers HTTP/1.1 only, for connections that switch protocols.
    tls_http1: Arc<ClientConfig>,
    /// Client certificates by host, presented when a server asks.
    client_certs: RwLock<Arc<ClientCerts>>,
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
    proxy: Option<ProxyServer>,
    bypass: Vec<String>,
    pub insecure: bool,
}

impl Upstream {
    /// `insecure` disables certificate verification of upstream servers (useful
    /// for staging hosts with self-signed certificates). `extra_roots` are
    /// trusted in addition to the system and Mozilla roots.
    pub fn new(insecure: bool, extra_roots: Vec<CertificateDer<'static>>) -> Result<Self> {
        Self::with_options(UpstreamOptions { insecure, extra_roots, ..Default::default() })
    }

    pub fn with_options(o: UpstreamOptions) -> Result<Self> {
        let UpstreamOptions { insecure, extra_roots, proxy, bypass, connect_timeout, total_timeout } = o;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let builder = ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;
        let mut config = if insecure {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
                .with_no_client_auth()
        } else {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            for cert in rustls_native_certs::load_native_certs().certs {
                let _ = roots.add(cert);
            }
            for cert in extra_roots {
                roots.add(cert).context("adding extra root")?;
            }
            builder.with_root_certificates(roots).with_no_client_auth()
        };
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tls_http1 = Arc::new(config.clone());
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        Ok(Self { tls: Arc::new(config), tls_http1, client_certs: RwLock::default(), connect_timeout, total_timeout, proxy, bypass, insecure })
    }

    /// Replaces the client certificates presented to servers.
    pub fn set_client_certs(&self, certs: Arc<ClientCerts>) {
        *self.client_certs.write().unwrap() = certs;
    }

    pub fn client_certs(&self) -> Arc<ClientCerts> {
        self.client_certs.read().unwrap().clone()
    }

    /// Opens TLS to `host` over `tcp`, offering the host's client
    /// certificate if it has one. Returns the stream, the server's DNS names
    /// and the certificate presented, if the server asked for it.
    async fn tls_connect(&self, host: &str, port: u16, tcp: TcpStream, http1: bool) -> Result<(tokio_rustls::client::TlsStream<TcpStream>, Vec<String>, Option<String>)> {
        let name = ServerName::try_from(host.to_string()).map_err(|_| anyhow!("invalid server name {host}"))?;
        let base = if http1 { &self.tls_http1 } else { &self.tls };
        let (config, offered) = match self.client_certs().for_host(host) {
            None => (base.clone(), None),
            Some((label, key)) => {
                let used = Arc::new(AtomicBool::new(false));
                let mut config = (**base).clone();
                config.client_auth_cert_resolver = Arc::new(Offer { key, used: used.clone() });
                // A resumed session skips the certificate request, which
                // would hide that the certificate is in use.
                config.resumption = rustls::client::Resumption::disabled();
                (Arc::new(config), Some((label, used)))
            }
        };
        let tls = TlsConnector::from(config).connect(name, tcp).await.map_err(|e| tls_error(host, port, e, offered.is_some()))?;
        let sans = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(cert_dns_names).unwrap_or_default();
        let presented = offered.filter(|(_, used)| used.load(Ordering::SeqCst)).map(|(label, _)| label);
        Ok((tls, sans, presented))
    }

    /// The upstream proxy used for `host`, if any.
    pub fn proxy_for(&self, host: &str) -> Option<&ProxyServer> {
        self.proxy.as_ref().filter(|_| !host_matches(host, &self.bypass))
    }

    /// Opens a TCP stream to `host:port`, through the upstream proxy if one
    /// applies. Also used for tunnels that are not decrypted.
    pub async fn connect(&self, host: &str, port: u16) -> Result<TcpStream> {
        let fut = async {
            match self.proxy_for(host) {
                None => TcpStream::connect((host, port)).await.with_context(|| format!("connecting to {host}:{port}")),
                Some(p) => {
                    let mut tcp = TcpStream::connect((p.host.as_str(), p.port))
                        .await
                        .with_context(|| format!("connecting to the upstream proxy {}:{}", p.host, p.port))?;
                    match p.kind {
                        ProxyKind::Http => http_connect(&mut tcp, p, host, port).await?,
                        ProxyKind::Socks5 => socks5_connect(&mut tcp, p, host, port).await?,
                    }
                    Ok(tcp)
                }
            }
        };
        let tcp = tokio::time::timeout(self.connect_timeout, fut)
            .await
            .map_err(|_| anyhow!("connecting to {host}:{port} timed out"))??;
        tcp.set_nodelay(true).ok();
        Ok(tcp)
    }

    /// Sends a request and reads the whole response, all within the request timeout.
    pub async fn send(&self, req: OutboundRequest) -> Result<InboundResponse> {
        let fut = async {
            let r = self.open_inner(req, None).await?;
            let body = r.body.collect().await.context("reading response body")?.to_bytes();
            Ok(InboundResponse { status: r.status, headers: r.headers, body, tls_sans: r.tls_sans, version: r.version, client_cert: r.client_cert })
        };
        tokio::time::timeout(self.total_timeout, fut)
            .await
            .map_err(|_| anyhow!("upstream timed out after {:?}", self.total_timeout))?
    }

    /// Sends a request and returns once the response head arrives; the
    /// request timeout covers only that part, so long streams are not cut.
    /// With `body`, the request body streams from it instead of `req.body`,
    /// and the request's own `Content-Length` (if any) is kept.
    pub async fn open(&self, req: OutboundRequest, body: Option<StreamBody>) -> Result<StreamingResponse> {
        tokio::time::timeout(self.total_timeout, self.open_inner(req, body))
            .await
            .map_err(|_| anyhow!("upstream timed out after {:?}", self.total_timeout))?
    }

    /// Sends a request that asks to switch protocols (`Connection: Upgrade`
    /// and `Upgrade: <protocol>` are added to `req`). The connection always
    /// speaks HTTP/1.1 and, behind an HTTP upstream proxy, is tunneled with
    /// `CONNECT` so the switch reaches the server.
    pub async fn upgrade(&self, mut req: OutboundRequest, protocol: &str) -> Result<UpgradeResponse> {
        req.extra_headers.push(("Connection".into(), "Upgrade".into()));
        req.extra_headers.push(("Upgrade".into(), protocol.into()));
        let fut = async {
            let tcp = self.connect(&req.host, req.port).await?;
            let (resp, tls_sans, client_cert) = if req.scheme == "https" {
                let (tls, sans, cert) = self.tls_connect(&req.host, req.port, tcp, true).await?;
                let (host, offered) = (req.host.clone(), self.client_certs().for_host(&req.host).is_some());
                (send_http1(TokioIo::new(tls), req, None).await.map_err(|e| with_cert_hint(e, &host, offered))?, sans, cert)
            } else {
                (send_http1(TokioIo::new(tcp), req, None).await?, vec![], None)
            };
            let (status, headers) = head_of(&resp);
            let outcome = if status == 101 {
                Upgrade::Switched(hyper::upgrade::on(resp).await.context("switching protocols")?)
            } else {
                Upgrade::Refused(resp.into_body())
            };
            Ok(UpgradeResponse { status, headers, tls_sans, client_cert, outcome })
        };
        tokio::time::timeout(self.total_timeout, fut)
            .await
            .map_err(|_| anyhow!("upstream timed out after {:?}", self.total_timeout))?
    }

    async fn open_inner(&self, mut req: OutboundRequest, body: Option<StreamBody>) -> Result<StreamingResponse> {
        // Plain HTTP through an HTTP proxy uses absolute-form requests, which
        // every HTTP proxy accepts (not all allow CONNECT to port 80).
        if req.scheme == "http"
            && let Some(p) = self.proxy_for(&req.host).filter(|p| p.kind == ProxyKind::Http).cloned()
        {
            let tcp = tokio::time::timeout(self.connect_timeout, TcpStream::connect((p.host.as_str(), p.port)))
                .await
                .map_err(|_| anyhow!("connecting to the upstream proxy {}:{} timed out", p.host, p.port))?
                .with_context(|| format!("connecting to the upstream proxy {}:{}", p.host, p.port))?;
            let host = if req.host.contains(':') { format!("[{}]", req.host) } else { req.host.clone() };
            req.target = format!("http://{host}:{}{}", req.port, req.target);
            if !p.username.is_empty() {
                let creds = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", p.username, p.password));
                req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case("proxy-authorization"));
                req.extra_headers.push(("Proxy-Authorization".into(), format!("Basic {creds}")));
            }
            return exchange(TokioIo::new(tcp), req, body).await;
        }
        let tcp = self.connect(&req.host, req.port).await?;

        if req.scheme == "https" {
            let (tls, sans, client_cert) = self.tls_connect(&req.host, req.port, tcp, false).await?;
            let (host, offered) = (req.host.clone(), self.client_certs().for_host(&req.host).is_some());
            let hint = |e| with_cert_hint(e, &host, offered);
            let mut resp = if tls.get_ref().1.alpn_protocol() == Some(b"h2") {
                let resp = send_http2(TokioIo::new(tls), req, body).await.map_err(hint)?;
                let (status, headers) = head_of(&resp);
                StreamingResponse { status, headers, body: resp.into_body(), tls_sans: vec![], version: "HTTP/2".into(), client_cert: None }
            } else {
                exchange(TokioIo::new(tls), req, body).await.map_err(hint)?
            };
            resp.tls_sans = sans;
            resp.client_cert = client_cert;
            Ok(resp)
        } else {
            exchange(TokioIo::new(tcp), req, body).await
        }
    }
}

/// Describes a failed TLS handshake. A server that drops the connection as
/// soon as the handshake starts usually serves no HTTPS on that port, so the
/// message says so instead of only naming the socket error.
fn tls_error(host: &str, port: u16, e: std::io::Error, offered_cert: bool) -> anyhow::Error {
    use std::io::ErrorKind as K;
    let hint = match client_cert_hint(host, &e.to_string(), offered_cert) {
        Some(h) => h,
        None if matches!(e.kind(), K::ConnectionReset | K::ConnectionAborted | K::UnexpectedEof) => {
            let plain = if port == 443 { format!("http://{host}") } else { format!("http://{host}:{port}") };
            format!(" (the server closed the connection before any TLS reply, so it may not serve HTTPS on port {port}; try {plain})")
        }
        None => String::new(),
    };
    anyhow!("TLS handshake with {host}: {e}{hint}")
}

/// With TLS 1.3 a server checks the client's certificate after the client
/// considers the handshake done, so a refusal arrives with the first
/// request. Either way, say what to do about it.
fn client_cert_hint(host: &str, text: &str, offered_cert: bool) -> Option<String> {
    let alert = text.contains("received fatal alert");
    if text.contains("CertificateRequired") || (alert && text.contains("Certificate") && !offered_cert) {
        Some(format!(" ({host} asks for a client certificate; add one in Settings › Client certificates or with `plonix certs add {host}`)"))
    } else if alert && offered_cert {
        Some(" (the server did not accept the client certificate Plonix presented)".to_string())
    } else {
        None
    }
}

/// Adds [`client_cert_hint`] to an error from a request sent over TLS.
fn with_cert_hint(e: anyhow::Error, host: &str, offered_cert: bool) -> anyhow::Error {
    match client_cert_hint(host, &format!("{e:#}"), offered_cert) {
        Some(h) => anyhow!("{e:#}{h}"),
        None => e,
    }
}

async fn exchange<T>(io: TokioIo<T>, req: OutboundRequest, body: Option<StreamBody>) -> Result<StreamingResponse>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let resp = send_http1(io, req, body).await?;
    let (status, headers) = head_of(&resp);
    let version = version_name(resp.version()).to_string();
    Ok(StreamingResponse { status, headers, body: resp.into_body(), tls_sans: vec![], version, client_cert: None })
}

fn head_of<B>(resp: &http::Response<B>) -> (u16, Headers) {
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    (resp.status().as_u16(), headers)
}

async fn send_http1<T>(io: TokioIo<T>, req: OutboundRequest, body: Option<StreamBody>) -> Result<http::Response<Incoming>>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake::<_, StreamBody>(io)
        .await
        .context("HTTP handshake")?;
    tokio::spawn(async move {
        let _ = conn.with_upgrades().await;
    });
    sender.send_request(build_request(req, body, false)?).await.context("sending request")
}

async fn send_http2<T>(io: TokioIo<T>, req: OutboundRequest, body: Option<StreamBody>) -> Result<http::Response<Incoming>>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http2::Builder::new(TokioExecutor::new())
        .handshake::<_, StreamBody>(io)
        .await
        .context("HTTP/2 handshake")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });
    sender.send_request(build_request(req, body, true)?).await.context("sending request")
}

/// `host`, or `host:port` when the port is not the scheme's default.
fn authority(scheme: &str, host: &str, port: u16) -> String {
    let host = if host.contains(':') { format!("[{host}]") } else { host.to_string() };
    let default = (scheme == "https" && port == 443) || (scheme == "http" && port == 80);
    if default { host } else { format!("{host}:{port}") }
}

/// The request as sent. Headers that only describe one connection are left
/// out. In HTTP/2 the `Host` header (as captured or edited) becomes the
/// `:authority`, and is not sent as a header.
fn build_request(req: OutboundRequest, body: Option<StreamBody>, http2: bool) -> Result<http::Request<StreamBody>> {
    let streaming = body.is_some();
    let host = crate::model::header(&req.headers, "host").map(str::to_string).unwrap_or_else(|| authority(&req.scheme, &req.host, req.port));
    let mut builder = if http2 {
        http::Request::builder().version(http::Version::HTTP_2).uri(format!("{}://{host}{}", req.scheme, req.target))
    } else {
        http::Request::builder().uri(&req.target)
    }
    .method(req.method.as_str());
    let mut has_host = false;
    for (k, v) in &req.headers {
        let lk = k.to_ascii_lowercase();
        // Length is recomputed from the (possibly edited) body; a streamed
        // body is passed on unchanged, so its length still holds.
        if HOP_BY_HOP.contains(&lk.as_str()) || (lk == "content-length" && !streaming) || (lk == "host" && http2) || lk.starts_with(':') {
            continue;
        }
        has_host |= lk == "host";
        builder = builder.header(k.as_str(), v.as_str());
    }
    for (k, v) in &req.extra_headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    if !http2 && !has_host {
        builder = builder.header("host", host);
    }
    let send_len = !req.body.is_empty() || matches!(req.method.as_str(), "POST" | "PUT" | "PATCH");
    if send_len && !streaming {
        builder = builder.header("content-length", req.body.len());
    }
    let body = body.unwrap_or_else(|| full_body(req.body));
    builder.body(body).context("building request")
}

/// Opens a tunnel through an HTTP proxy with `CONNECT`.
async fn http_connect(tcp: &mut TcpStream, p: &ProxyServer, host: &str, port: u16) -> Result<()> {
    let authority = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
    let mut req = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if !p.username.is_empty() {
        let creds = base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", p.username, p.password));
        req.push_str(&format!("Proxy-Authorization: Basic {creds}\r\n"));
    }
    req.push_str("\r\n");
    tcp.write_all(req.as_bytes()).await?;
    // Read the response head one byte at a time so nothing past it is consumed.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if tcp.read(&mut byte).await? == 0 {
            bail!("the upstream proxy closed the connection");
        }
        head.push(byte[0]);
        if head.len() > 16 * 1024 {
            bail!("the upstream proxy sent an oversized response");
        }
    }
    let status_line = String::from_utf8_lossy(&head).lines().next().unwrap_or("").to_string();
    let code = status_line.split_whitespace().nth(1).unwrap_or("");
    if code != "200" {
        bail!("the upstream proxy refused {authority}: {status_line}");
    }
    Ok(())
}

/// Opens a tunnel through a SOCKS5 proxy. The proxy resolves the host name.
async fn socks5_connect(tcp: &mut TcpStream, p: &ProxyServer, host: &str, port: u16) -> Result<()> {
    let auth = !p.username.is_empty();
    tcp.write_all(if auth { &[5, 2, 0, 2] } else { &[5, 1, 0] }).await?;
    let mut reply = [0u8; 2];
    tcp.read_exact(&mut reply).await?;
    match reply {
        [5, 0] => {}
        [5, 2] if auth => {
            let (u, pw) = (p.username.as_bytes(), p.password.as_bytes());
            anyhow::ensure!(u.len() < 256 && pw.len() < 256, "SOCKS5 credentials are too long");
            let mut msg = vec![1, u.len() as u8];
            msg.extend_from_slice(u);
            msg.push(pw.len() as u8);
            msg.extend_from_slice(pw);
            tcp.write_all(&msg).await?;
            tcp.read_exact(&mut reply).await?;
            anyhow::ensure!(reply[1] == 0, "the SOCKS5 proxy rejected the username or password");
        }
        _ => bail!("the SOCKS5 proxy does not accept {}", if auth { "username/password login" } else { "connections without login" }),
    }
    let mut msg = vec![5, 1, 0];
    match host.trim_matches(['[', ']']).parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            msg.push(1);
            msg.extend_from_slice(&ip.octets());
        }
        Ok(std::net::IpAddr::V6(ip)) => {
            msg.push(4);
            msg.extend_from_slice(&ip.octets());
        }
        Err(_) => {
            anyhow::ensure!(host.len() < 256, "host name too long");
            msg.push(3);
            msg.push(host.len() as u8);
            msg.extend_from_slice(host.as_bytes());
        }
    }
    msg.extend_from_slice(&port.to_be_bytes());
    tcp.write_all(&msg).await?;
    let mut head = [0u8; 4];
    tcp.read_exact(&mut head).await?;
    if head[1] != 0 {
        bail!("the SOCKS5 proxy could not reach {host}:{port} (code {})", head[1]);
    }
    let skip = match head[3] {
        1 => 4,
        4 => 16,
        3 => {
            let mut len = [0u8; 1];
            tcp.read_exact(&mut len).await?;
            len[0] as usize
        }
        _ => bail!("the SOCKS5 proxy sent an invalid reply"),
    };
    let mut rest = vec![0u8; skip + 2];
    tcp.read_exact(&mut rest).await?;
    Ok(())
}

/// DNS names (and IP addresses) from a certificate's subjectAltName.
pub fn cert_dns_names(cert: &CertificateDer<'_>) -> Vec<String> {
    use x509_parser::extensions::GeneralName;
    let Ok((_, parsed)) = x509_parser::parse_x509_certificate(cert.as_ref()) else { return vec![] };
    let Ok(Some(san)) = parsed.subject_alternative_name() else { return vec![] };
    san.value
        .general_names
        .iter()
        .filter_map(|n| match n {
            GeneralName::DNSName(d) => Some(d.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_urls() {
        let p = ProxyServer::parse("http://user:pa:ss@proxy.example:3128/").unwrap();
        assert_eq!((p.kind, p.host.as_str(), p.port), (ProxyKind::Http, "proxy.example", 3128));
        assert_eq!((p.username.as_str(), p.password.as_str()), ("user", "pa:ss"));
        assert_eq!(ProxyServer::parse("socks5://[::1]:1080").unwrap().host, "::1");
        assert!(ProxyServer::parse("proxy:3128").is_err());
        assert!(ProxyServer::parse("https://proxy:3128").is_err());
        assert!(ProxyServer::parse("http://proxy").is_err());
    }

    #[tokio::test]
    async fn closed_handshake_suggests_plain_http() {
        // A port that accepts TCP but drops every TLS handshake, like a host
        // that serves only plain HTTP.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf).await;
            }
        });
        let up = Upstream::new(false, vec![]).unwrap();
        let req = OutboundRequest {
            scheme: "https".into(),
            host: "127.0.0.1".into(),
            port,
            method: "GET".into(),
            target: "/".into(),
            headers: vec![],
            body: Bytes::new(),
            extra_headers: vec![],
        };
        let err = up.send(req).await.unwrap_err().to_string();
        assert!(err.starts_with("TLS handshake with 127.0.0.1: "), "{err}");
        assert!(err.contains(&format!("may not serve HTTPS on port {port}; try http://127.0.0.1:{port}")), "{err}");
    }

    #[test]
    fn host_patterns() {
        let pats = vec!["localhost".to_string(), "*.corp.example".to_string()];
        assert!(host_matches("LOCALHOST", &pats));
        assert!(host_matches("corp.example", &pats));
        assert!(host_matches("a.b.corp.example", &pats));
        assert!(!host_matches("evilcorp.example", &pats));
    }
}

#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
