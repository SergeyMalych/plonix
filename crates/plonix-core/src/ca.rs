//! Local certificate authority that signs per-host leaf certificates on the fly.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use base64::Engine as _;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer,
    KeyPair, KeyUsagePurpose, PublicKeyData, SanType,
};
use rustls::sign::CertifiedKey;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime};

use crate::paths::{Home, write_private};

pub struct CertAuthority {
    ca_pem: String,
    ca_der: CertificateDer<'static>,
    issuer: Issuer<'static, KeyPair>,
    /// One key pair shared by all leaf certificates; generating keys is the slow part.
    leaf_key: KeyPair,
    cache: Mutex<HashMap<String, Arc<CertifiedKey>>>,
}

impl CertAuthority {
    /// Loads the CA from the Plonix home directory, creating it on first run.
    pub fn load_or_create(home: &Home) -> Result<Self> {
        let (cert_path, key_path) = (home.ca_cert(), home.ca_key());
        if cert_path.exists() && key_path.exists() {
            let cert = std::fs::read_to_string(&cert_path)?;
            let key = std::fs::read_to_string(&key_path)?;
            return Self::from_pem(&cert, &key);
        }
        let (cert, key) = Self::generate_pem()?;
        write_private(&key_path, key.as_bytes())?;
        std::fs::write(&cert_path, cert.as_bytes())?;
        Self::from_pem(&cert, &key)
    }

    /// Creates a brand-new CA certificate and key, both PEM encoded.
    pub fn generate_pem() -> Result<(String, String)> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        let host = std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty());
        let now = OffsetDateTime::now_utc();
        let cn = match host {
            Some(h) => format!("Plonix CA ({h}, {})", now.date()),
            None => format!("Plonix CA ({})", now.date()),
        };
        dn.push(DnType::CommonName, cn);
        dn.push(DnType::OrganizationName, "Plonix local CA");
        params.distinguished_name = dn;
        params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign, KeyUsagePurpose::DigitalSignature];
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(3650);
        let cert = params.self_signed(&key)?;
        Ok((cert.pem(), key.serialize_pem()))
    }

    pub fn from_pem(cert_pem: &str, key_pem: &str) -> Result<Self> {
        let key = KeyPair::from_pem(key_pem).context("parsing CA key")?;
        let issuer = Issuer::from_ca_cert_pem(cert_pem, key).context("parsing CA certificate")?;
        let ca_der = rustls_pemfile_der(cert_pem)?;
        Ok(Self {
            ca_pem: cert_pem.to_string(),
            ca_der,
            issuer,
            leaf_key: KeyPair::generate()?,
            cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }

    pub fn ca_der(&self) -> &CertificateDer<'static> {
        &self.ca_der
    }

    /// base64(sha256(SubjectPublicKeyInfo)) of the CA, as accepted by Chromium's
    /// `--ignore-certificate-errors-spki-list`.
    pub fn spki_sha256(&self) -> String {
        let spki = self.issuer.key().subject_public_key_info();
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(&spki))
    }

    /// SHA-256 fingerprint of the CA certificate, colon separated.
    pub fn fingerprint(&self) -> String {
        Sha256::digest(self.ca_der.as_ref())
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":")
    }

    /// Returns a leaf certificate (plus the CA in the chain) for `host`.
    pub fn leaf_for(&self, host: &str) -> Result<Arc<CertifiedKey>> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if let Some(c) = self.cache.lock().unwrap().get(&host) {
            return Ok(c.clone());
        }
        let mut params = CertificateParams::default();
        let mut dn = DistinguishedName::new();
        dn.push(DnType::CommonName, host.clone());
        params.distinguished_name = dn;
        params.subject_alt_names = vec![match host.parse::<IpAddr>() {
            Ok(ip) => SanType::IpAddress(ip),
            Err(_) => SanType::DnsName(host.clone().try_into()?),
        }];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature, KeyUsagePurpose::KeyEncipherment];
        params.use_authority_key_identifier_extension = true;
        let now = OffsetDateTime::now_utc();
        // Apple platforms reject leaf certificates valid for more than 825 days.
        params.not_before = now - Duration::days(1);
        params.not_after = now + Duration::days(365);
        let mut serial = [0u8; 16];
        ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut serial)
            .map_err(|_| anyhow::anyhow!("no system randomness"))?;
        serial[0] &= 0x7f;
        params.serial_number = Some(serial.to_vec().into());
        let cert = params.signed_by(&self.leaf_key, &self.issuer)?;

        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.leaf_key.serialize_der()));
        let signer = rustls::crypto::ring::sign::any_supported_type(&key_der)
            .map_err(|e| anyhow::anyhow!("unsupported leaf key: {e}"))?;
        let ck = Arc::new(CertifiedKey::new(vec![cert.der().clone(), self.ca_der.clone()], signer));
        self.cache.lock().unwrap().insert(host, ck.clone());
        Ok(ck)
    }
}

fn rustls_pemfile_der(pem: &str) -> Result<CertificateDer<'static>> {
    use rustls_pki_types::pem::PemObject;
    CertificateDer::from_pem_slice(pem.as_bytes()).map_err(|e| anyhow::anyhow!("bad CA PEM: {e:?}"))
}

/// rustls certificate resolver: picks the SNI name, or the CONNECT target when
/// the client sent no SNI (e.g. connecting to an IP address).
#[derive(Debug)]
pub struct LeafResolver {
    pub ca: Arc<CertAuthority>,
    pub fallback_host: String,
}

impl std::fmt::Debug for CertAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertAuthority").finish_non_exhaustive()
    }
}

impl rustls::server::ResolvesServerCert for LeafResolver {
    fn resolve(&self, hello: rustls::server::ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let host = hello.server_name().unwrap_or(&self.fallback_host);
        match self.ca.leaf_for(host) {
            Ok(c) => Some(c),
            Err(e) => {
                tracing::warn!("cannot mint certificate for {host}: {e:#}");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaf_chains_to_ca() {
        let (cert, key) = CertAuthority::generate_pem().unwrap();
        let ca = CertAuthority::from_pem(&cert, &key).unwrap();
        let leaf = ca.leaf_for("Example.COM").unwrap();
        assert_eq!(leaf.cert.len(), 2);
        let (_, parsed) = x509_parser::parse_x509_certificate(leaf.cert[0].as_ref()).unwrap();
        let (_, root) = x509_parser::parse_x509_certificate(ca.ca_der().as_ref()).unwrap();
        assert_eq!(parsed.issuer(), root.subject());
        parsed.verify_signature(Some(root.public_key())).unwrap();
        let sans = parsed.subject_alternative_name().unwrap().unwrap();
        assert!(format!("{:?}", sans.value).contains("example.com"));
        // Cached on second use.
        assert!(Arc::ptr_eq(&leaf, &ca.leaf_for("example.com").unwrap()));
    }

    #[test]
    fn ca_survives_reload() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home { root: dir.path().to_path_buf() };
        let a = CertAuthority::load_or_create(&home).unwrap();
        let b = CertAuthority::load_or_create(&home).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_eq!(a.spki_sha256(), b.spki_sha256());
        let leaf = b.leaf_for("10.0.0.1").unwrap();
        let (_, parsed) = x509_parser::parse_x509_certificate(leaf.cert[0].as_ref()).unwrap();
        let (_, root) = x509_parser::parse_x509_certificate(a.ca_der().as_ref()).unwrap();
        parsed.verify_signature(Some(root.public_key())).unwrap();
    }
}
