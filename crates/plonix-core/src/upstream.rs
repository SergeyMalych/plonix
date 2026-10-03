//! Outbound HTTP/1.1 client used by the proxy and for active requests.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use rustls::ClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::model::Headers;

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
}

#[derive(Debug, Clone)]
pub struct InboundResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Bytes,
    pub tls_sans: Vec<String>,
}

pub struct Upstream {
    tls: TlsConnector,
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
}

impl Upstream {
    /// `insecure` disables certificate verification of upstream servers (useful
    /// for staging hosts with self-signed certificates). `extra_roots` are
    /// trusted in addition to the system and Mozilla roots.
    pub fn new(insecure: bool, extra_roots: Vec<CertificateDer<'static>>) -> Result<Self> {
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
        Ok(Self {
            tls: TlsConnector::from(Arc::new(config)),
            connect_timeout: Duration::from_secs(10),
            total_timeout: Duration::from_secs(120),
        })
    }

    pub async fn send(&self, req: OutboundRequest) -> Result<InboundResponse> {
        tokio::time::timeout(self.total_timeout, self.send_inner(req))
            .await
            .map_err(|_| anyhow!("upstream timed out after {:?}", self.total_timeout))?
    }

    async fn send_inner(&self, req: OutboundRequest) -> Result<InboundResponse> {
        let tcp = tokio::time::timeout(self.connect_timeout, TcpStream::connect((req.host.as_str(), req.port)))
            .await
            .map_err(|_| anyhow!("connecting to {}:{} timed out", req.host, req.port))?
            .with_context(|| format!("connecting to {}:{}", req.host, req.port))?;
        tcp.set_nodelay(true).ok();

        if req.scheme == "https" {
            let name = ServerName::try_from(req.host.clone()).map_err(|_| anyhow!("invalid server name {}", req.host))?;
            let tls = self.tls.connect(name, tcp).await.with_context(|| format!("TLS handshake with {}", req.host))?;
            let sans = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(cert_dns_names).unwrap_or_default();
            let mut resp = exchange(TokioIo::new(tls), req).await?;
            resp.tls_sans = sans;
            Ok(resp)
        } else {
            exchange(TokioIo::new(tcp), req).await
        }
    }
}

async fn exchange<T>(io: TokioIo<T>, req: OutboundRequest) -> Result<InboundResponse>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake::<_, Full<Bytes>>(io)
        .await
        .context("HTTP handshake")?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = http::Request::builder().method(req.method.as_str()).uri(&req.target);
    let mut has_host = false;
    for (k, v) in &req.headers {
        let lk = k.to_ascii_lowercase();
        // Length is recomputed from the (possibly edited) body.
        if HOP_BY_HOP.contains(&lk.as_str()) || lk == "content-length" {
            continue;
        }
        has_host |= lk == "host";
        builder = builder.header(k.as_str(), v.as_str());
    }
    if !has_host {
        let default = (req.scheme == "https" && req.port == 443) || (req.scheme == "http" && req.port == 80);
        let host = if default { req.host.clone() } else { format!("{}:{}", req.host, req.port) };
        builder = builder.header("host", host);
    }
    let send_len = !req.body.is_empty() || matches!(req.method.as_str(), "POST" | "PUT" | "PATCH");
    if send_len {
        builder = builder.header("content-length", req.body.len());
    }
    let request = builder.body(Full::new(req.body)).context("building request")?;
    let resp = sender.send_request(request).await.context("sending request")?;
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
        .collect();
    let body = resp.into_body().collect().await.context("reading response body")?.to_bytes();
    Ok(InboundResponse { status, headers, body, tls_sans: vec![] })
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
