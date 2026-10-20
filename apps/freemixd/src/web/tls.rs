//! The gateway's TLS certificate: the operator's own chain and key, or a
//! self-signed certificate generated once and kept in the state directory,
//! identified to operators by its SHA-256 fingerprint.

use std::{
    fs,
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};

use super::{AppResult, settings::WebTls};

/// The generated certificate and key in the state directory.
pub(super) const CERT_FILE: &str = "web-tls-cert.pem";
pub(super) const KEY_FILE: &str = "web-tls-key.pem";
/// Generated certificates last 800 days: Apple platforms refuse TLS server
/// certificates valid for longer than 825 days, even self-signed ones.
const VALIDITY_DAYS: u64 = 800;
/// A generated certificate this close to its expiry is replaced at startup.
const RENEW_BEFORE_DAYS: u64 = 30;
const DAY: Duration = Duration::from_secs(86_400);

/// The rustls server configuration and the certificate's fingerprint.
pub(crate) struct TlsMaterial {
    pub(crate) config: Arc<ServerConfig>,
    /// SHA-256 of the leaf certificate, `AB:CD:...`.
    pub(crate) fingerprint: String,
}

impl std::fmt::Debug for TlsMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TlsMaterial")
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// Loads (or for [`WebTls::SelfSigned`], creates) the certificate.
///
/// `names` are the host names and addresses a generated certificate covers.
///
/// # Errors
///
/// Unreadable or invalid PEM files, a key that does not match, or a state
/// directory that cannot be written.
pub(crate) fn load(
    tls: &WebTls,
    state_dir: Option<&Path>,
    names: &[String],
) -> AppResult<Option<TlsMaterial>> {
    let (certs, key) = match tls {
        WebTls::Off => return Ok(None),
        WebTls::Files { cert, key } => (read_chain(cert)?, read_key(key)?),
        WebTls::SelfSigned => {
            let directory = state_dir.ok_or(
                "--web-tls needs a state directory: pass --web-state-dir (no home directory is known)",
            )?;
            self_signed(directory, names)?
        }
    };
    let fingerprint = fingerprint(&certs[0]);
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|error| {
                format!("web TLS certificate and key do not form a usable pair: {error}")
            })?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Some(TlsMaterial {
        config: Arc::new(config),
        fingerprint,
    }))
}

fn read_chain(path: &Path) -> AppResult<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_file_iter(path)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .map_err(|error| {
            format!(
                "cannot read web TLS certificate {}: {error}",
                path.display()
            )
        })?;
    if certs.is_empty() {
        return Err(format!("{} holds no certificate", path.display()).into());
    }
    Ok(certs)
}

fn read_key(path: &Path) -> AppResult<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_file(path)
        .map_err(|error| format!("cannot read web TLS key {}: {error}", path.display()).into())
}

/// The kept self-signed certificate, generated (or renewed) when missing,
/// unreadable, or near its expiry.
fn self_signed(
    directory: &Path,
    names: &[String],
) -> AppResult<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    create_private_dir(directory)?;
    let cert_path = directory.join(CERT_FILE);
    let key_path = directory.join(KEY_FILE);
    let fresh = fs::metadata(&cert_path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| {
            age < DAY * u32::try_from(VALIDITY_DAYS - RENEW_BEFORE_DAYS).unwrap_or(0)
        });
    if fresh && let (Ok(certs), Ok(key)) = (read_chain(&cert_path), read_key(&key_path)) {
        return Ok((certs, key));
    }
    generate(names, &cert_path, &key_path)?;
    Ok((read_chain(&cert_path)?, read_key(&key_path)?))
}

fn generate(names: &[String], cert_path: &Path, key_path: &Path) -> AppResult<()> {
    use rcgen::{
        CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
        KeyUsagePurpose, SanType,
    };
    let key_pair = KeyPair::generate()?;
    let mut params = CertificateParams::default();
    let mut distinguished = DistinguishedName::new();
    distinguished.push(DnType::CommonName, "FreeMix web gateway");
    params.distinguished_name = distinguished;
    params.is_ca = IsCa::NoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(i64::try_from(VALIDITY_DAYS).unwrap_or(800));
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
    let certificate = params.self_signed(&key_pair)?;
    write_private(key_path, key_pair.serialize_pem().as_bytes())?;
    write_private(cert_path, certificate.pem().as_bytes())?;
    Ok(())
}

/// Creates `directory` readable only by this user.
pub(super) fn create_private_dir(directory: &Path) -> AppResult<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory).map_err(|error| {
        format!(
            "cannot create web state directory {}: {error}",
            directory.display()
        )
    })?;
    Ok(())
}

/// Replaces `path` with `contents`, readable only by this user, through a
/// temporary file renamed into place.
pub(super) fn write_private(path: &Path, contents: &[u8]) -> AppResult<()> {
    let temporary: PathBuf = path.with_extension("tmp");
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)
        .map_err(|error| format!("cannot replace {}: {error}", path.display()))?;
    Ok(())
}

/// SHA-256 of `der` as 32 colon-separated uppercase hex pairs.
pub(crate) fn fingerprint(der: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, der);
    let mut text = String::with_capacity(95);
    for (index, byte) in digest.as_ref().iter().enumerate() {
        if index > 0 {
            text.push(':');
        }
        text.push_str(&format!("{byte:02X}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{CERT_FILE, KEY_FILE, WebTls, load};

    #[test]
    fn a_generated_certificate_is_kept_and_reused() {
        let directory = tempfile::tempdir().unwrap();
        let names = ["localhost".to_owned(), "127.0.0.1".to_owned()];
        let first = load(&WebTls::SelfSigned, Some(directory.path()), &names)
            .unwrap()
            .unwrap();
        assert!(fm_protocol::valid_web_fingerprint(&first.fingerprint));
        let second = load(&WebTls::SelfSigned, Some(directory.path()), &names)
            .unwrap()
            .unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(directory.path().join(KEY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "the key is private: {mode:o}");
        }
        // The operator's own files load the same way.
        let files = WebTls::Files {
            cert: directory.path().join(CERT_FILE),
            key: directory.path().join(KEY_FILE),
        };
        let own = load(&files, None, &[]).unwrap().unwrap();
        assert_eq!(own.fingerprint, first.fingerprint);
        // A key that does not parse is refused.
        let broken = WebTls::Files {
            cert: directory.path().join(CERT_FILE),
            key: directory.path().join(CERT_FILE),
        };
        assert!(load(&broken, None, &[]).is_err());
        assert!(load(&WebTls::SelfSigned, None, &names).is_err());
    }
}
