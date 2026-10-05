//! Client certificates (mutual TLS) for upstream servers.
//!
//! Some servers only talk to clients that present a certificate. Each
//! project keeps a list of certificates, each for a host or a family of
//! hosts (`api.example.com`, `*.example.com`), in its database. Whenever
//! Plonix connects to a matching host (the proxy, Bench sends, scans and
//! crawls all share one client, see [`crate::upstream`]) and the server asks
//! for a certificate, that one is presented. Exchanges note which
//! certificate went out, and the Lens and the Bench show it.
//!
//! Certificates come as PEM (a certificate chain and an unencrypted private
//! key, in one file or two) or as PKCS#12 (`.p12`/`.pfx` with a password),
//! which is turned into PEM once, when it is added, with the system's
//! `openssl` command.
//!
//! Keys stay inside the engine: the API only ever describes a certificate
//! (subject, issuer, expiry, fingerprint), the routes are the user's alone,
//! agents cannot reach them, and nothing here logs key material.

use std::io::Write;
use std::sync::Arc;

use base64::Engine as _;
use rustls::sign::CertifiedKey;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::settings::{Field, Level, Section};
use crate::upstream::host_matches;

pub const SETTINGS_SECTION: &str = "client-certs";

/// The Settings section: one switch for all certificates; the certificates
/// themselves are listed under it and kept in the project's database.
pub fn settings_section() -> Section {
    Section::new(SETTINGS_SECTION, "Client certificates", Level::Project)
        .describe(
            "Certificates Plonix presents to servers that ask for one (mutual TLS), each for a host or *.domain. \
             They apply to the proxy, the Bench, scans and crawls. Keys are kept in this project's database and never shown to agents.",
        )
        .order(17)
        .field(Field::toggle("enabled", "Present client certificates", true).help("Off keeps every certificate but sends none."))
}

/// A certificate as stored, key included. Never serialized: see [`CertInfo`].
#[derive(Clone)]
pub struct StoredCert {
    pub id: i64,
    /// `api.example.com`, or `*.example.com` for the domain and its subdomains.
    pub host: String,
    /// The certificate chain, leaf first.
    pub cert_pem: String,
    pub key_pem: String,
    pub note: String,
    pub created_at: i64,
}

impl std::fmt::Debug for StoredCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoredCert").field("id", &self.id).field("host", &self.host).field("key_pem", &"(hidden)").finish()
    }
}

/// What the API, the CLI and Settings show about a certificate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertInfo {
    pub id: i64,
    pub host: String,
    pub subject: String,
    pub issuer: String,
    /// Unix time in milliseconds.
    pub not_before: i64,
    pub not_after: i64,
    /// SHA-256 of the leaf certificate, hex with colons.
    pub fingerprint: String,
    /// Certificates in the chain, the leaf included.
    pub chain: usize,
    pub note: String,
    pub created_at: i64,
    pub expired: bool,
    /// Why the engine cannot use it, when it cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// A certificate to add, as the API takes it: PEM text, or a PKCS#12 file
/// in base64 with its password.
#[derive(Default, Deserialize)]
pub struct CertInput {
    pub host: String,
    #[serde(default)]
    pub cert_pem: Option<String>,
    /// May be left out when `cert_pem` holds the key too.
    #[serde(default)]
    pub key_pem: Option<String>,
    #[serde(default)]
    pub pkcs12_base64: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

impl std::fmt::Debug for CertInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertInput").field("host", &self.host).finish_non_exhaustive()
    }
}

impl CertInput {
    /// Checks the input and turns it into a certificate to store.
    pub fn into_stored(self, now: i64) -> Result<StoredCert, String> {
        let host = normalize_pattern(&self.host)?;
        let (cert_pem, key_pem) = match (&self.pkcs12_base64, &self.cert_pem) {
            (Some(p12), _) => {
                let der = base64::engine::general_purpose::STANDARD.decode(p12.trim()).map_err(|_| "the .p12 file did not arrive intact (bad base64)".to_string())?;
                pkcs12_to_pem(&der, self.password.as_deref().unwrap_or(""))?
            }
            (None, Some(cert)) => {
                let key = self.key_pem.clone().filter(|k| !k.trim().is_empty()).unwrap_or_else(|| cert.clone());
                let (chain, key) = parse(cert, &key)?;
                (chain_pem(&chain), key_pem(&key))
            }
            (None, None) => return Err("give a certificate and key (PEM), or a .p12 file".into()),
        };
        // Parse once more as stored, so what is kept is known to load.
        parse(&cert_pem, &key_pem)?;
        Ok(StoredCert { id: 0, host, cert_pem, key_pem, note: self.note.unwrap_or_default().trim().to_string(), created_at: now })
    }
}

/// `API.Example.com` → `api.example.com`; `*.example.com` stays a wildcard.
/// A URL is reduced to its host.
pub fn normalize_pattern(host: &str) -> Result<String, String> {
    let mut h = host.trim().to_ascii_lowercase();
    if let Some((_, rest)) = h.split_once("://") {
        h = rest.to_string();
    }
    if let Some(i) = h.find('/') {
        h.truncate(i);
    }
    // A port does not matter: the certificate goes to the host on any port.
    if !h.starts_with('[')
        && let Some((name, port)) = h.rsplit_once(':')
        && !name.contains(':')
        && port.chars().all(|c| c.is_ascii_digit())
    {
        h = name.to_string();
    }
    let h = h.trim_matches(['[', ']']).trim_end_matches('.').to_string();
    let base = h.strip_prefix("*.").unwrap_or(&h);
    if base.is_empty() || base.contains('*') {
        return Err("give a host such as api.example.com, or *.example.com for a domain and its subdomains".into());
    }
    if !base.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':')) {
        return Err(format!("'{}' is not a host name", host.trim()));
    }
    Ok(h)
}

/// Reads a certificate chain (leaf first) and its private key from PEM.
/// The key may be in the same text as the certificates.
pub fn parse(cert_pem: &str, key_pem: &str) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), String> {
    let chain: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(cert_pem.as_bytes()).collect::<Result<_, _>>().map_err(|e| format!("reading the certificate: {e}"))?;
    if chain.is_empty() {
        return Err("no certificate found: expected a PEM file with -----BEGIN CERTIFICATE-----".into());
    }
    if key_pem.contains("ENCRYPTED PRIVATE KEY") || key_pem.contains("Proc-Type: 4,ENCRYPTED") {
        return Err("the private key is protected by a password; save it without one (openssl pkey -in key.pem -out plain-key.pem) or use the .p12 file".into());
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem.as_bytes()).map_err(|_| "no private key found: expected a PEM file with -----BEGIN PRIVATE KEY----- (or RSA/EC PRIVATE KEY)".to_string())?;
    let chain = leaf_first(chain, &key)?;
    Ok((chain, key))
}

/// Puts the certificate that belongs to `key` first, as TLS needs it.
fn leaf_first(mut chain: Vec<CertificateDer<'static>>, key: &PrivateKeyDer<'static>) -> Result<Vec<CertificateDer<'static>>, String> {
    let provider = rustls::crypto::ring::default_provider();
    let signer = provider.key_provider.load_private_key(key.clone_key()).map_err(|e| format!("the private key cannot be used: {e}"))?;
    let matches = |c: &CertificateDer<'static>| CertifiedKey::new(vec![c.clone()], signer.clone()).keys_match().is_ok();
    match chain.iter().position(matches) {
        Some(0) => Ok(chain),
        Some(i) => {
            let leaf = chain.remove(i);
            chain.insert(0, leaf);
            Ok(chain)
        }
        None if signer.public_key().is_none() => Ok(chain),
        None => Err("the private key does not belong to the certificate".into()),
    }
}

fn pem_block(label: &str, der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = format!("-----BEGIN {label}-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).unwrap_or_default());
        out.push('\n');
    }
    out.push_str(&format!("-----END {label}-----\n"));
    out
}

fn chain_pem(chain: &[CertificateDer<'_>]) -> String {
    chain.iter().map(|c| pem_block("CERTIFICATE", c)).collect()
}

fn key_pem(key: &PrivateKeyDer<'_>) -> String {
    let label = match key {
        PrivateKeyDer::Pkcs1(_) => "RSA PRIVATE KEY",
        PrivateKeyDer::Sec1(_) => "EC PRIVATE KEY",
        _ => "PRIVATE KEY",
    };
    pem_block(label, key.secret_der())
}

/// Opens a PKCS#12 file with the system's `openssl` (macOS and most Linux
/// systems have one). The password goes to it through its environment,
/// never on the command line.
pub fn pkcs12_to_pem(der: &[u8], password: &str) -> Result<(String, String), String> {
    let run = |legacy: bool| -> Result<std::process::Output, String> {
        let mut cmd = std::process::Command::new(openssl());
        cmd.args(["pkcs12", "-nodes", "-passin", "env:PLONIX_P12_PASSWORD"]);
        if legacy {
            cmd.arg("-legacy");
        }
        let mut child = cmd
            .env("PLONIX_P12_PASSWORD", password)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|_| "reading .p12 files needs the openssl command, which was not found; convert it to PEM files instead".to_string())?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(der).map_err(|e| format!("reading the .p12 file: {e}"))?;
        }
        child.wait_with_output().map_err(|e| format!("reading the .p12 file: {e}"))
    };
    let mut out = run(false)?;
    // OpenSSL 3 reads older files (RC2, 3DES) only with -legacy.
    if !out.status.success() && !String::from_utf8_lossy(&out.stderr).to_ascii_lowercase().contains("mac verify") {
        if let Ok(again) = run(true) {
            out = again;
        }
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).to_ascii_lowercase();
        return Err(if err.contains("mac verify") || err.contains("password") {
            "wrong password for the .p12 file".into()
        } else {
            "could not read the .p12 file; check that it is a PKCS#12 file and the password is right".into()
        });
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (chain, key) = parse(&text, &text)?;
    Ok((chain_pem(&chain), key_pem(&key)))
}

fn openssl() -> String {
    std::env::var("PLONIX_OPENSSL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "openssl".into())
}

/// Describes a stored certificate, without its key.
pub fn describe(c: &StoredCert) -> CertInfo {
    let mut info = CertInfo {
        id: c.id,
        host: c.host.clone(),
        subject: String::new(),
        issuer: String::new(),
        not_before: 0,
        not_after: 0,
        fingerprint: String::new(),
        chain: 0,
        note: c.note.clone(),
        created_at: c.created_at,
        expired: false,
        problem: None,
    };
    match parse(&c.cert_pem, &c.key_pem) {
        Ok((chain, key)) => {
            info.chain = chain.len();
            if let Err(e) = rustls::crypto::ring::default_provider().key_provider.load_private_key(key) {
                info.problem = Some(format!("the private key cannot be used: {e}"));
            }
            let leaf = &chain[0];
            info.fingerprint = Sha256::digest(leaf.as_ref()).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":");
            if let Ok((_, x)) = x509_parser::parse_x509_certificate(leaf.as_ref()) {
                info.subject = x.subject().to_string();
                info.issuer = x.issuer().to_string();
                info.not_before = x.validity().not_before.timestamp() * 1000;
                info.not_after = x.validity().not_after.timestamp() * 1000;
                info.expired = info.not_after < crate::model::now_ms();
            }
        }
        Err(e) => info.problem = Some(e),
    }
    info
}

/// The short name of a certificate: its common name, else its whole subject.
fn short_subject(subject: &str) -> String {
    subject
        .split(',')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("CN="))
        .map(str::to_string)
        .unwrap_or_else(|| subject.to_string())
}

/// One certificate ready to present.
struct Loaded {
    pattern: String,
    /// What an exchange records: `alice for *.example.com`.
    label: String,
    key: Arc<CertifiedKey>,
}

/// The certificates in effect, matched by host.
#[derive(Default)]
pub struct ClientCerts {
    entries: Vec<Loaded>,
}

impl std::fmt::Debug for ClientCerts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.entries.iter().map(|e| &e.pattern)).finish()
    }
}

impl ClientCerts {
    /// Loads stored certificates. Ones that cannot be used are left out,
    /// with the reason (host and message only, never the key).
    pub fn load(stored: &[StoredCert]) -> (Self, Vec<String>) {
        let provider = rustls::crypto::ring::default_provider();
        let mut problems = vec![];
        let mut entries = vec![];
        for c in stored {
            let loaded = parse(&c.cert_pem, &c.key_pem).and_then(|(chain, key)| CertifiedKey::from_der(chain, key, &provider).map_err(|e| e.to_string()));
            match loaded {
                Ok(key) => {
                    let subject = describe(c).subject;
                    let name = if subject.is_empty() { format!("certificate #{}", c.id) } else { short_subject(&subject) };
                    entries.push(Loaded { pattern: c.host.clone(), label: format!("{name} for {}", c.host), key: Arc::new(key) });
                }
                Err(e) => problems.push(format!("client certificate #{} for {}: {e}", c.id, c.host)),
            }
        }
        (Self { entries }, problems)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The certificate for `host`: an exact host wins over a wildcard, a
    /// longer wildcard over a shorter one, and the earlier one breaks ties.
    pub fn for_host(&self, host: &str) -> Option<(String, Arc<CertifiedKey>)> {
        let rank = |p: &str| if p.starts_with("*.") { p.len() } else { usize::MAX };
        self.entries
            .iter()
            .filter(|e| host_matches(host, std::slice::from_ref(&e.pattern)))
            .fold(None::<&Loaded>, |best, e| match best {
                Some(b) if rank(&b.pattern) >= rank(&e.pattern) => Some(b),
                _ => Some(e),
            })
            .map(|e| (e.label.clone(), e.key.clone()))
    }
}

/// Presents the host's certificate, if it has one, when the server asks, and
/// remembers that the server asked. A server that asks and gets nothing may
/// only say so after the handshake, by closing the connection.
#[derive(Debug)]
pub(crate) struct Offer {
    pub key: Option<Arc<CertifiedKey>>,
    pub asked: Arc<std::sync::atomic::AtomicBool>,
}

impl rustls::client::ResolvesClientCert for Offer {
    fn resolve(&self, _hints: &[&[u8]], _schemes: &[rustls::SignatureScheme]) -> Option<Arc<CertifiedKey>> {
        self.asked.store(true, std::sync::atomic::Ordering::SeqCst);
        self.key.clone()
    }

    fn has_certs(&self) -> bool {
        self.key.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn self_signed(cn: &str) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![]).unwrap();
        params.distinguished_name.push(rcgen::DnType::CommonName, cn);
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    #[test]
    fn patterns_are_normalized() {
        assert_eq!(normalize_pattern(" API.Example.com ").unwrap(), "api.example.com");
        assert_eq!(normalize_pattern("*.Example.com").unwrap(), "*.example.com");
        assert_eq!(normalize_pattern("https://api.example.com:8443/x").unwrap(), "api.example.com");
        assert_eq!(normalize_pattern("[::1]").unwrap(), "::1");
        assert!(normalize_pattern("*").is_err());
        assert!(normalize_pattern("a b").is_err());
        assert!(normalize_pattern("").is_err());
    }

    #[test]
    fn pem_in_one_or_two_files_and_mismatched_keys() {
        let (cert, key) = self_signed("alice");
        let stored = CertInput { host: "*.example.com".into(), cert_pem: Some(format!("{cert}{key}")), ..Default::default() }.into_stored(1).unwrap();
        let info = describe(&stored);
        assert_eq!((info.subject.as_str(), info.chain, info.problem.is_none()), ("CN=alice", 1, true));
        assert!(!format!("{stored:?}").contains("PRIVATE"), "debug output never shows the key");
        let (_, other_key) = self_signed("bob");
        let e = CertInput { host: "x.test".into(), cert_pem: Some(cert.clone()), key_pem: Some(other_key), ..Default::default() }.into_stored(1).unwrap_err();
        assert!(e.contains("does not belong"), "{e}");
        let e = CertInput { host: "x.test".into(), cert_pem: Some(cert), key_pem: Some("nope".into()), ..Default::default() }.into_stored(1).unwrap_err();
        assert!(e.contains("no private key"), "{e}");
    }

    #[test]
    fn exact_hosts_beat_wildcards() {
        let mk = |id, host: &str, cn: &str| {
            let (c, k) = self_signed(cn);
            StoredCert { id, host: host.into(), cert_pem: c, key_pem: k, note: String::new(), created_at: 0 }
        };
        let (certs, problems) = ClientCerts::load(&[mk(1, "*.example.com", "wide"), mk(2, "api.example.com", "exact"), mk(3, "*.eu.example.com", "eu")]);
        assert!(problems.is_empty() && certs.len() == 3);
        assert_eq!(certs.for_host("api.example.com").unwrap().0, "exact for api.example.com");
        assert_eq!(certs.for_host("x.eu.example.com").unwrap().0, "eu for *.eu.example.com");
        assert_eq!(certs.for_host("example.com").unwrap().0, "wide for *.example.com");
        assert!(certs.for_host("example.org").is_none());
    }

    #[test]
    fn pkcs12_files_open_with_their_password() {
        if std::process::Command::new(openssl()).arg("version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let (cert, key) = self_signed("p12 user");
        std::fs::write(dir.path().join("c.pem"), &cert).unwrap();
        std::fs::write(dir.path().join("k.pem"), &key).unwrap();
        let out = dir.path().join("c.p12");
        let ok = std::process::Command::new(openssl())
            .args(["pkcs12", "-export", "-in"])
            .arg(dir.path().join("c.pem"))
            .arg("-inkey")
            .arg(dir.path().join("k.pem"))
            .arg("-out")
            .arg(&out)
            .args(["-passout", "pass:s3cret"])
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let p12 = base64::engine::general_purpose::STANDARD.encode(std::fs::read(&out).unwrap());
        let input = |pw: &str| CertInput { host: "mtls.test".into(), pkcs12_base64: Some(p12.clone()), password: Some(pw.into()), ..Default::default() };
        let stored = input("s3cret").into_stored(1).unwrap();
        assert_eq!(describe(&stored).subject, "CN=p12 user");
        assert!(input("wrong").into_stored(1).unwrap_err().contains("wrong password"));
    }
}
