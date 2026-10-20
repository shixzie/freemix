//! The per-install local certificate authority (`--web-tls`): a root
//! certificate generated once in the web state directory, which an operator
//! installs on each phone or tablet (Studio offers it for download with a
//! QR code), and the gateway's leaf certificates it issues for the LAN
//! addresses, `localhost`, the `--web-host` names, and the mDNS name.
//!
//! The root's key never leaves the state directory: both key files are
//! written with mode 0600 (in a 0700 directory), refused when other users
//! can read them, and never formatted into any message or log.

use std::{fmt::Write as _, fs, net::IpAddr, path::Path};

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, PublicKeyData, SanType,
};
use rustls::pki_types::{CertificateDer, pem::PemObject};
use x509_parser::{certificate::X509Certificate, prelude::FromDer};

use super::{AppResult, tls};

/// The root certificate and its key in the state directory.
pub(super) const CA_CERT_FILE: &str = "web-ca-cert.pem";
pub(super) const CA_KEY_FILE: &str = "web-ca-key.pem";
/// The root lasts ten years: re-installing it on every phone is the cost
/// of a new one.
const CA_VALIDITY_DAYS: i64 = 3_650;
/// Leaves last 397 days, within what browsers accept from any CA.
pub(super) const LEAF_VALIDITY_DAYS: i64 = 397;
const DAY_MS: u64 = 86_400_000;
/// A leaf this close to its expiry is reissued.
pub(super) const RENEW_BEFORE_MS: u64 = 30 * DAY_MS;

/// The loaded root: its certificate and the key that signs leaves.
pub(crate) struct LocalCa {
    der: CertificateDer<'static>,
    key: KeyPair,
    fingerprint: String,
}

impl std::fmt::Debug for LocalCa {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalCa")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

impl LocalCa {
    /// Loads the root from `directory`, generating it when neither file
    /// exists.
    ///
    /// # Errors
    ///
    /// Only one of the two files present, key files other users can read,
    /// a root that does not parse, is not a CA, has expired, or does not
    /// match its key, or a directory that cannot be written. A damaged root
    /// is never replaced silently: phones trust it, so the operator decides.
    pub(crate) fn open_or_create(directory: &Path, now_ms: u64) -> AppResult<Self> {
        tls::create_private_dir(directory)?;
        let cert_path = directory.join(CA_CERT_FILE);
        let key_path = directory.join(CA_KEY_FILE);
        match (cert_path.exists(), key_path.exists()) {
            (false, false) => {
                let (cert_pem, key_pem) = generate_root()?;
                tls::write_private(&key_path, key_pem.as_bytes())?;
                tls::write_private(&cert_path, cert_pem.as_bytes())?;
                eprintln!(
                    "FREEMIXD_WEB_CA\tv=1\tstate=created\tfingerprint={}",
                    Self::load(&cert_path, &key_path, now_ms)?.fingerprint
                );
            }
            (true, true) => {}
            _ => {
                return Err(format!(
                    "the local web CA in {} is incomplete: remove {CA_CERT_FILE} and \
                     {CA_KEY_FILE} to create a new one (phones must then install it again)",
                    directory.display()
                )
                .into());
            }
        }
        Self::load(&cert_path, &key_path, now_ms)
    }

    fn load(cert_path: &Path, key_path: &Path, now_ms: u64) -> AppResult<Self> {
        tls::check_private_key(key_path, true)?;
        let damaged = |reason: &str| -> String {
            format!(
                "the local web CA {} {reason}: remove {CA_CERT_FILE} and {CA_KEY_FILE} to create \
                 a new one (phones must then install it again)",
                cert_path.display()
            )
        };
        let der =
            CertificateDer::from_pem_file(cert_path).map_err(|_| damaged("does not parse"))?;
        let key_pem = fs::read_to_string(key_path)
            .map_err(|error| format!("cannot read {}: {error}", key_path.display()))?;
        // The key's parse error is not reported: it could quote the key.
        let key =
            KeyPair::from_pem(&key_pem).map_err(|_| damaged("has a key that does not parse"))?;
        drop(key_pem);
        let (_, certificate) =
            X509Certificate::from_der(der.as_ref()).map_err(|_| damaged("does not parse"))?;
        if !certificate.is_ca() {
            return Err(damaged("is not a CA certificate").into());
        }
        let not_after_ms =
            u64::try_from(certificate.validity().not_after.timestamp()).unwrap_or(0) * 1_000;
        if now_ms >= not_after_ms {
            return Err(damaged("has expired").into());
        }
        if certificate.public_key().raw != key.subject_public_key_info().as_slice() {
            return Err(damaged("does not match its key").into());
        }
        let fingerprint = tls::fingerprint(der.as_ref());
        Ok(Self {
            der,
            key,
            fingerprint,
        })
    }

    /// The root certificate (DER), which phones download and install.
    pub(crate) fn der(&self) -> &CertificateDer<'static> {
        &self.der
    }

    /// SHA-256 of the root certificate, `AB:CD:...`.
    pub(crate) fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Issues a leaf for `names` (DNS names or address literals): PEM
    /// certificate and PEM key.
    ///
    /// # Errors
    ///
    /// A name that is neither an address nor a valid DNS name, or a signing
    /// failure.
    pub(crate) fn issue(&self, names: &[String]) -> AppResult<(String, String)> {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::default();
        let mut distinguished = DistinguishedName::new();
        distinguished.push(DnType::CommonName, "FreeMix web gateway");
        distinguished.push(DnType::OrganizationName, "FreeMix");
        params.distinguished_name = distinguished;
        params.is_ca = IsCa::ExplicitNoCa;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params.use_authority_key_identifier_extension = true;
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now - time::Duration::days(1);
        params.not_after = now + time::Duration::days(LEAF_VALIDITY_DAYS);
        params.serial_number = Some(random_serial().into());
        let mut sans = Vec::new();
        for name in names {
            let san = match name.parse::<IpAddr>() {
                Ok(address) => SanType::IpAddress(address),
                Err(_) => SanType::DnsName(name.as_str().try_into()?),
            };
            if !sans.contains(&san) {
                sans.push(san);
            }
        }
        params.subject_alt_names = sans;
        let issuer = Issuer::from_ca_cert_der(&self.der, &self.key)?;
        let certificate = params.signed_by(&key, &issuer)?;
        Ok((certificate.pem(), key.serialize_pem()))
    }
}

/// A new root: PEM certificate and PEM key.
fn generate_root() -> AppResult<(String, String)> {
    let key = KeyPair::generate()?;
    let mut params = CertificateParams::default();
    let mut distinguished = DistinguishedName::new();
    // A short random suffix tells this install's root from another's on a
    // phone that trusts several.
    let mut name = String::from("FreeMix local CA ");
    for byte in &random_serial()[..3] {
        let _ = write!(name, "{byte:02X}");
    }
    distinguished.push(DnType::CommonName, name);
    distinguished.push(DnType::OrganizationName, "FreeMix");
    params.distinguished_name = distinguished;
    // It signs leaves only, never another CA.
    params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(CA_VALIDITY_DAYS);
    params.serial_number = Some(random_serial().into());
    let certificate = params.self_signed(&key)?;
    Ok((certificate.pem(), key.serialize_pem()))
}

/// Sixteen random bytes with the top bit clear (a positive serial).
fn random_serial() -> Vec<u8> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).expect("the operating system random source is available");
    bytes[0] &= 0x7f;
    bytes.to_vec()
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::CA_KEY_FILE;
    use super::{CA_CERT_FILE, LocalCa};
    use crate::web::certs;
    use rustls::client::danger::ServerCertVerifier;
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime, pem::PemObject};

    fn now_ms() -> u64 {
        u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp()).unwrap() * 1_000
    }

    #[test]
    fn the_root_is_created_once_and_kept_private() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("web");
        let first = LocalCa::open_or_create(&state, now_ms()).unwrap();
        let second = LocalCa::open_or_create(&state, now_ms()).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert!(fm_protocol::valid_web_fingerprint(first.fingerprint()));
        assert!(!format!("{first:?}").contains("PRIVATE"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for file in [CA_KEY_FILE, CA_CERT_FILE] {
                let mode = std::fs::metadata(state.join(file))
                    .unwrap()
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o077, 0, "{file} is private: {mode:o}");
            }
            let mode = std::fs::metadata(&state).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "the directory is private: {mode:o}");
            // A key other users can read is refused, never used.
            std::fs::set_permissions(
                state.join(CA_KEY_FILE),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            let error = LocalCa::open_or_create(&state, now_ms())
                .unwrap_err()
                .to_string();
            assert!(error.contains("other users"), "{error}");
            assert!(!error.contains("PRIVATE KEY"), "{error}");
            std::fs::set_permissions(
                state.join(CA_KEY_FILE),
                std::fs::Permissions::from_mode(0o600),
            )
            .unwrap();
        }
        // A damaged or half-removed root is reported, never replaced.
        std::fs::write(state.join(CA_CERT_FILE), "not a certificate").unwrap();
        let error = LocalCa::open_or_create(&state, now_ms())
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not parse"), "{error}");
        std::fs::remove_file(state.join(CA_CERT_FILE)).unwrap();
        let error = LocalCa::open_or_create(&state, now_ms())
            .unwrap_err()
            .to_string();
        assert!(error.contains("incomplete"), "{error}");
    }

    #[test]
    fn issued_leaves_chain_to_the_root_for_every_name() {
        let directory = tempfile::tempdir().unwrap();
        let ca = LocalCa::open_or_create(directory.path(), now_ms()).unwrap();
        let names = [
            "freemix-studio.local".to_owned(),
            "localhost".to_owned(),
            "192.168.1.20".to_owned(),
            "fd00::20".to_owned(),
        ];
        let (cert_pem, key_pem) = ca.issue(&names).unwrap();
        assert!(key_pem.contains("PRIVATE KEY"));
        let leaf = CertificateDer::from_pem_slice(cert_pem.as_bytes()).unwrap();
        assert!(certs::signed_by(&leaf, ca.der()));
        let info = certs::inspect_chain(&[leaf.clone(), ca.der().clone()], now_ms()).unwrap();
        for name in &names {
            assert!(info.covers(name), "{name}");
        }
        assert!(!info.covers("192.168.1.21"));
        // A browser that installed the root accepts the leaf for each name,
        // exactly as WebPKI checks it.
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
            std::sync::Arc::new(roots),
            std::sync::Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .unwrap();
        let now = UnixTime::now();
        for name in [
            "freemix-studio.local",
            "localhost",
            "192.168.1.20",
            "fd00::20",
        ] {
            let server = ServerName::try_from(name).unwrap();
            verifier
                .verify_server_cert(&leaf, &[], &server, &[], now)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        let elsewhere = ServerName::try_from("10.0.0.9").unwrap();
        assert!(
            verifier
                .verify_server_cert(&leaf, &[], &elsewhere, &[], now)
                .is_err()
        );
        // Another install's root does not vouch for this leaf.
        let other = tempfile::tempdir().unwrap();
        let stranger = LocalCa::open_or_create(other.path(), now_ms()).unwrap();
        assert!(!certs::signed_by(&leaf, stranger.der()));
        assert_ne!(stranger.fingerprint(), ca.fingerprint());
    }
}
