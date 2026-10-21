//! The gateway's TLS certificate (RC-007): either the operator's own chain
//! and key (`--web-tls-cert`, `--web-tls-key`, for example from mkcert or a
//! company CA), reloaded when the files change, or a leaf issued by the
//! per-install local CA (`--web-tls`), reissued when the LAN addresses
//! change or the leaf nears its expiry. Every certificate is validated
//! before it is served (see [`super::certs`]); a reload that fails keeps the
//! previous certificate and is reported. The leaf's SHA-256 fingerprint
//! stays the fallback check an operator can compare by eye.

use std::{
    fmt::Write as _,
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use fm_protocol::{WebCertProblem, WebTrust};
use rustls::{
    ServerConfig,
    crypto::CryptoProvider,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::{ClientHello, ResolvesServerCert},
    sign::CertifiedKey,
};

use super::{
    AppResult,
    certs::{self, LeafInfo},
    local_ca::{self, LocalCa},
    settings::WebTls,
};

/// The local CA's current leaf and its key in the state directory.
pub(super) const CERT_FILE: &str = "web-tls-cert.pem";
pub(super) const KEY_FILE: &str = "web-tls-key.pem";
/// A certificate this close to its expiry is reported as expiring.
const EXPIRING_MS: u64 = 30 * 86_400_000;

/// One validated certificate the gateway serves.
struct Served {
    key: Arc<CertifiedKey>,
    /// SHA-256 of the leaf, `AB:CD:...`.
    fingerprint: String,
    info: LeafInfo,
}

/// When the operator's files last changed, to notice a replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stamp {
    cert: Option<(SystemTime, u64)>,
    key: Option<(SystemTime, u64)>,
}

impl Stamp {
    fn of(cert: &Path, key: &Path) -> Self {
        let stamp = |path: &Path| {
            fs::metadata(path)
                .ok()
                .and_then(|metadata| Some((metadata.modified().ok()?, metadata.len())))
        };
        Self {
            cert: stamp(cert),
            key: stamp(key),
        }
    }
}

/// Where the certificate comes from.
enum Source {
    Files {
        cert: PathBuf,
        key: PathBuf,
        /// The files the served certificate was read from, and a change seen
        /// once but not yet settled (reloaded on the next look if unchanged,
        /// so a certificate and key replaced one after the other load as a
        /// pair).
        stamps: Mutex<(Stamp, Option<Stamp>)>,
    },
    LocalCa {
        directory: PathBuf,
        ca: Box<LocalCa>,
    },
}

/// The served certificate, shared by the TLS handshakes, the console's
/// status answers, and the maintenance thread.
pub(crate) struct TlsState {
    trust: WebTrust,
    current: RwLock<Arc<Served>>,
    source: Source,
    /// The operator's latest files did not validate (the previous
    /// certificate is still served).
    reload_failed: AtomicBool,
}

impl std::fmt::Debug for TlsState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TlsState")
            .field("trust", &self.trust)
            .field("fingerprint", &self.fingerprint())
            .finish_non_exhaustive()
    }
}

/// Picks the current certificate for every handshake.
#[derive(Debug)]
struct Resolver(Arc<TlsState>);

impl ResolvesServerCert for Resolver {
    fn resolve(&self, _client_hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.0.served().key))
    }
}

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Loads (or for [`WebTls::LocalCa`], creates and issues) the certificate.
///
/// `names` are the host names and addresses a local CA leaf covers.
///
/// # Errors
///
/// Unreadable or invalid PEM files, key files other users can read, a
/// certificate that fails validation, a key that does not match, or a state
/// directory that cannot be written.
pub(crate) fn load(
    tls: &WebTls,
    state_dir: Option<&Path>,
    names: &[String],
) -> AppResult<Option<Arc<TlsState>>> {
    let now = now_ms();
    let state = match tls {
        WebTls::Off => return Ok(None),
        WebTls::Files { cert, key } => {
            let stamp = Stamp::of(cert, key);
            let served = load_files(cert, key, now)?;
            TlsState {
                trust: WebTrust::Operator,
                current: RwLock::new(Arc::new(served)),
                source: Source::Files {
                    cert: cert.clone(),
                    key: key.clone(),
                    stamps: Mutex::new((stamp, None)),
                },
                reload_failed: AtomicBool::new(false),
            }
        }
        WebTls::LocalCa => {
            let directory = state_dir.ok_or(
                "--web-tls needs a state directory: pass --web-state-dir (no home directory is known)",
            )?;
            let ca = LocalCa::open_or_create(directory, now, names)?;
            let skipped: Vec<&str> = names
                .iter()
                .filter(|name| !ca.permits(name))
                .map(String::as_str)
                .collect();
            if !skipped.is_empty() {
                // Names that arrived after the root was made (a public
                // address, a new `--web-host`): the leaf leaves them out,
                // and the console reports a host mismatch for them.
                eprintln!(
                    "FREEMIXD_WEB_CA\tv=1\tstate=outside_constraints\tnames={}",
                    skipped.join(",")
                );
            }
            let names = &permitted_names(&ca, names);
            let served = match load_issued(directory, &ca, names, now) {
                Some(served) => served,
                None => issue(directory, &ca, names, now)?,
            };
            TlsState {
                trust: WebTrust::LocalCa,
                current: RwLock::new(Arc::new(served)),
                source: Source::LocalCa {
                    directory: directory.to_owned(),
                    ca: Box::new(ca),
                },
                reload_failed: AtomicBool::new(false),
            }
        }
    };
    Ok(Some(Arc::new(state)))
}

impl TlsState {
    /// The rustls configuration, resolving the current certificate on each
    /// handshake.
    ///
    /// # Errors
    ///
    /// The provider refuses the safe default protocol versions.
    pub(crate) fn server_config(self: &Arc<Self>) -> AppResult<Arc<ServerConfig>> {
        let mut config = ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_cert_resolver(Arc::new(Resolver(Arc::clone(self))));
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }

    fn served(&self) -> Arc<Served> {
        Arc::clone(&self.current.read().unwrap_or_else(PoisonError::into_inner))
    }

    pub(crate) const fn trust(&self) -> WebTrust {
        self.trust
    }

    /// SHA-256 of the served leaf, `AB:CD:...`.
    pub(crate) fn fingerprint(&self) -> String {
        self.served().fingerprint.clone()
    }

    /// When the served leaf expires, Unix milliseconds.
    pub(crate) fn not_after_ms(&self) -> u64 {
        self.served().info.not_after_ms
    }

    /// Whether the served leaf names `host`.
    #[cfg(test)]
    pub(crate) fn covers(&self, host: &str) -> bool {
        self.served().info.covers(host)
    }

    /// The local root, with [`WebTrust::LocalCa`].
    pub(crate) fn ca(&self) -> Option<&LocalCa> {
        match &self.source {
            Source::LocalCa { ca, .. } => Some(ca.as_ref()),
            Source::Files { .. } => None,
        }
    }

    /// What is wrong with the served certificate for a gateway reached at
    /// `host`, the most pressing first.
    pub(crate) fn problem(&self, host: &str, now_ms: u64) -> Option<WebCertProblem> {
        let served = self.served();
        if self.reload_failed.load(Ordering::Acquire) {
            Some(WebCertProblem::ReloadFailed)
        } else if !served.info.covers(host) {
            Some(WebCertProblem::HostMismatch)
        } else if served.info.not_after_ms.saturating_sub(now_ms) < EXPIRING_MS {
            Some(WebCertProblem::Expiring)
        } else {
            None
        }
    }

    fn replace(&self, served: Served) {
        *self.current.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(served);
    }

    /// Reloads changed operator files, or reissues the local CA leaf when
    /// it no longer covers `names` or nears its expiry.
    ///
    /// Returns whether the served certificate changed.
    ///
    /// # Errors
    ///
    /// The operator's new files did not validate (the previous certificate
    /// stays), or a new leaf could not be issued or stored. The message
    /// names files and reasons, never their contents.
    pub(crate) fn refresh(&self, names: &[String], now_ms: u64) -> Result<bool, String> {
        match &self.source {
            Source::Files { cert, key, stamps } => {
                let mut stamps = stamps.lock().unwrap_or_else(PoisonError::into_inner);
                let seen = Stamp::of(cert, key);
                let (loaded, pending) = *stamps;
                if seen == loaded {
                    stamps.1 = None;
                    return Ok(false);
                }
                if pending != Some(seen) {
                    // Changed since the last look: wait for it to settle.
                    stamps.1 = Some(seen);
                    return Ok(false);
                }
                *stamps = (seen, None);
                match load_files(cert, key, now_ms) {
                    Ok(served) => {
                        self.replace(served);
                        self.reload_failed.store(false, Ordering::Release);
                        Ok(true)
                    }
                    Err(error) => {
                        self.reload_failed.store(true, Ordering::Release);
                        Err(error.to_string())
                    }
                }
            }
            Source::LocalCa { directory, ca } => {
                let names = &permitted_names(ca, names);
                let served = self.served();
                let fresh = names.iter().all(|name| served.info.covers(name))
                    && served.info.not_after_ms.saturating_sub(now_ms) > local_ca::RENEW_BEFORE_MS;
                if fresh {
                    return Ok(false);
                }
                let served =
                    issue(directory, ca, names, now_ms).map_err(|error| error.to_string())?;
                self.replace(served);
                Ok(true)
            }
        }
    }
}

/// The operator's chain and key, validated.
fn load_files(cert: &Path, key: &Path, now_ms: u64) -> AppResult<Served> {
    let chain = read_chain(cert)?;
    check_private_key(key, false)?;
    let key_der = read_key(key)?;
    let info = certs::inspect_chain(&chain, now_ms)
        .map_err(|reason| format!("web TLS certificate {}: {reason}", cert.display()))?;
    served(chain, key_der, info).map_err(|reason| {
        format!(
            "web TLS certificate {} and key {}: {reason}",
            cert.display(),
            key.display()
        )
        .into()
    })
}

fn served(
    chain: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    info: LeafInfo,
) -> Result<Served, String> {
    let fingerprint = fingerprint(chain[0].as_ref());
    let certified = CertifiedKey::from_der(chain, key, &provider())
        .map_err(|_| "the key does not match the certificate (or is not a supported key)")?;
    // `from_der` lets an unknown consistency pass; demand a definite match.
    certified
        .keys_match()
        .map_err(|_| "the key does not match the certificate")?;
    Ok(Served {
        key: Arc::new(certified),
        fingerprint,
        info,
    })
}

/// The `names` the local root's name constraints let its leaf carry (one
/// name outside them would make browsers reject the whole leaf).
fn permitted_names(ca: &LocalCa, names: &[String]) -> Vec<String> {
    names
        .iter()
        .filter(|name| ca.permits(name))
        .cloned()
        .collect()
}

/// The local CA leaf already in `directory`, when it is valid, issued by
/// `ca`, covers `names`, and is not due for renewal.
fn load_issued(directory: &Path, ca: &LocalCa, names: &[String], now_ms: u64) -> Option<Served> {
    let cert_path = directory.join(CERT_FILE);
    let key_path = directory.join(KEY_FILE);
    check_private_key(&key_path, true).ok()?;
    let mut chain = read_chain(&cert_path).ok()?;
    chain.truncate(1);
    if !certs::signed_by(&chain[0], ca.der()) {
        return None;
    }
    chain.push(ca.der().clone());
    let info = certs::inspect_chain(&chain, now_ms).ok()?;
    let usable = names.iter().all(|name| info.covers(name))
        && info.not_after_ms.saturating_sub(now_ms) > local_ca::RENEW_BEFORE_MS;
    if !usable {
        return None;
    }
    served(chain, read_key(&key_path).ok()?, info).ok()
}

/// Issues, stores, and validates a new local CA leaf for `names`.
fn issue(directory: &Path, ca: &LocalCa, names: &[String], now_ms: u64) -> AppResult<Served> {
    let (cert_pem, key_pem) = ca.issue(names)?;
    let cert_path = directory.join(CERT_FILE);
    let key_path = directory.join(KEY_FILE);
    write_private(&key_path, key_pem.as_bytes())?;
    drop(key_pem);
    write_private(&cert_path, cert_pem.as_bytes())?;
    let mut chain = read_chain(&cert_path)?;
    chain.push(ca.der().clone());
    let info = certs::inspect_chain(&chain, now_ms)
        .map_err(|reason| format!("the issued web certificate is invalid: {reason}"))?;
    Ok(served(chain, read_key(&key_path)?, info)?)
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
    // The PEM parser's errors name the section, never the key bytes.
    PrivateKeyDer::from_pem_file(path)
        .map_err(|error| format!("cannot read web TLS key {}: {error}", path.display()).into())
}

/// Refuses key material other users can reach: any group or other access
/// for files the daemon generated, any access by other users for the
/// operator's own key (a group such as `ssl-cert` may read it).
///
/// # Errors
///
/// An unreadable file, or one with those permission bits.
pub(super) fn check_private_key(path: &Path, generated: bool) -> AppResult<()> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot read web TLS key {}: {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        let forbidden = if generated { 0o077 } else { 0o007 };
        if mode & forbidden != 0 {
            return Err(format!(
                "the key {} is accessible to other users (mode {mode:03o}); run `chmod 600` on it",
                path.display()
            )
            .into());
        }
    }
    #[cfg(not(unix))]
    let _ = (metadata, generated);
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
///
/// The temporary file is always created afresh: a leftover one (from an
/// interrupted write, or planted in a state directory others can write)
/// would keep its own mode, or be a link leading elsewhere, and the key
/// renamed into place would carry that mode or be written where it leads.
pub(super) fn write_private(path: &Path, contents: &[u8]) -> AppResult<()> {
    let temporary: PathBuf = path.with_extension("tmp");
    match fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!("cannot replace {}: {error}", temporary.display()).into());
        }
    }
    let mut options = fs::OpenOptions::new();
    // `create_new` never opens an existing file and never follows a link.
    options.write(true).create_new(true);
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
        let _ = write!(text, "{byte:02X}");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{CERT_FILE, KEY_FILE, WebTls, load, now_ms, write_private};
    use crate::web::local_ca::{CA_CERT_FILE, CA_KEY_FILE};
    use fm_protocol::{WebCertProblem, WebTrust};
    use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair};

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    /// An mkcert-like operator pair for `hosts`, written with mode 0600.
    fn operator_pair(directory: &std::path::Path, hosts: &[&str], tag: &str) -> WebTls {
        let key = KeyPair::generate().unwrap();
        let mut params = CertificateParams::new(names(hosts)).unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let certificate = params.self_signed(&key).unwrap();
        let cert = directory.join(format!("{tag}.pem"));
        let key_path = directory.join(format!("{tag}-key.pem"));
        write_private(&cert, certificate.pem().as_bytes()).unwrap();
        write_private(&key_path, key.serialize_pem().as_bytes()).unwrap();
        WebTls::Files {
            cert,
            key: key_path,
        }
    }

    #[test]
    fn the_local_ca_leaf_is_kept_then_reissued_for_new_addresses() {
        let directory = tempfile::tempdir().unwrap();
        let lan = names(&["localhost", "127.0.0.1", "192.168.1.20"]);
        let first = load(&WebTls::LocalCa, Some(directory.path()), &lan)
            .unwrap()
            .unwrap();
        assert_eq!(first.trust(), WebTrust::LocalCa);
        assert!(fm_protocol::valid_web_fingerprint(&first.fingerprint()));
        let ca = first.ca().unwrap().fingerprint().to_owned();
        assert_ne!(ca, first.fingerprint());
        for file in [CERT_FILE, KEY_FILE, CA_CERT_FILE, CA_KEY_FILE] {
            assert!(directory.path().join(file).exists(), "{file}");
        }
        // A restart with the same addresses keeps the leaf.
        let second = load(&WebTls::LocalCa, Some(directory.path()), &lan)
            .unwrap()
            .unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint());
        assert_eq!(second.problem("192.168.1.20", now_ms()), None);
        assert_eq!(
            second.problem("10.0.0.9", now_ms()),
            Some(WebCertProblem::HostMismatch)
        );
        // Unchanged names: nothing to do. A new LAN address: a new leaf
        // for the new names from the same root (phones keep trusting it).
        assert_eq!(second.refresh(&lan, now_ms()), Ok(false));
        let moved = names(&["localhost", "127.0.0.1", "10.0.0.9", "freemix-a.local"]);
        assert_eq!(second.refresh(&moved, now_ms()), Ok(true));
        assert_ne!(second.fingerprint(), first.fingerprint());
        assert_eq!(second.ca().unwrap().fingerprint(), ca);
        assert!(second.covers("10.0.0.9") && second.covers("freemix-a.local"));
        // A public address that appears after the root was made lies outside
        // its name constraints: the leaf leaves it out (one such name would
        // void the whole leaf) and is not reissued over and over for it.
        let mut public = moved.clone();
        public.push("198.51.100.4".to_owned());
        assert!(!second.ca().unwrap().permits("198.51.100.4"));
        assert_eq!(second.refresh(&public, now_ms()), Ok(false));
        assert!(!second.covers("198.51.100.4"));
        assert_eq!(
            second.problem("198.51.100.4", now_ms()),
            Some(WebCertProblem::HostMismatch)
        );
        // And a restart finds the reissued leaf.
        let third = load(&WebTls::LocalCa, Some(directory.path()), &moved)
            .unwrap()
            .unwrap();
        assert_eq!(third.fingerprint(), second.fingerprint());
        assert!(load(&WebTls::LocalCa, None, &lan).is_err());
    }

    #[test]
    fn operator_files_are_validated_and_hot_reloaded() {
        let directory = tempfile::tempdir().unwrap();
        let files = operator_pair(directory.path(), &["studio.lan"], "own");
        let state = load(&files, None, &[]).unwrap().unwrap();
        assert_eq!(state.trust(), WebTrust::Operator);
        assert!(state.ca().is_none());
        assert_eq!(state.problem("studio.lan", now_ms()), None);
        let before = state.fingerprint();
        assert_eq!(state.refresh(&[], now_ms()), Ok(false));

        // Replaced files load once they settle (the next look).
        let WebTls::Files { cert, key } = &files else {
            unreachable!()
        };
        std::thread::sleep(std::time::Duration::from_millis(20));
        let replacement = operator_pair(directory.path(), &["studio.lan", "cam.lan"], "next");
        let WebTls::Files {
            cert: next_cert,
            key: next_key,
        } = &replacement
        else {
            unreachable!()
        };
        std::fs::copy(next_key, key).unwrap();
        std::fs::copy(next_cert, cert).unwrap();
        assert_eq!(state.refresh(&[], now_ms()), Ok(false), "settling");
        assert_eq!(state.refresh(&[], now_ms()), Ok(true));
        assert_ne!(state.fingerprint(), before);
        assert!(state.covers("cam.lan"));

        // A broken replacement keeps the served certificate and says so.
        let served = state.fingerprint();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(
            cert,
            "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        assert_eq!(state.refresh(&[], now_ms()), Ok(false));
        let error = state.refresh(&[], now_ms()).unwrap_err();
        assert!(!error.contains("PRIVATE KEY"), "{error}");
        assert_eq!(state.fingerprint(), served);
        assert_eq!(
            state.problem("studio.lan", now_ms()),
            Some(WebCertProblem::ReloadFailed)
        );
    }

    /// A leftover temporary file never decides the key's mode or where the
    /// key is written: the key always lands private, at its own path.
    #[cfg(unix)]
    #[test]
    fn a_leftover_temporary_file_never_exposes_the_key() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let key = directory.path().join(KEY_FILE);
        let temporary = key.with_extension("tmp");
        // A readable leftover from an interrupted write.
        std::fs::write(&temporary, b"stale").unwrap();
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&key, b"secret key").unwrap();
        let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the key is private: {mode:o}");
        assert_eq!(std::fs::read(&key).unwrap(), b"secret key");
        assert!(!temporary.exists());
        // A link planted where the temporary file goes is not followed.
        let elsewhere = directory.path().join("elsewhere");
        std::fs::write(&elsewhere, b"untouched").unwrap();
        std::os::unix::fs::symlink(&elsewhere, &temporary).unwrap();
        write_private(&key, b"second key").unwrap();
        assert_eq!(std::fs::read(&elsewhere).unwrap(), b"untouched");
        assert_eq!(std::fs::read(&key).unwrap(), b"second key");
        let metadata = std::fs::symlink_metadata(&key).unwrap();
        assert!(metadata.file_type().is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn mismatched_or_exposed_operator_keys_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let one = operator_pair(directory.path(), &["studio.lan"], "one");
        let two = operator_pair(directory.path(), &["studio.lan"], "two");
        let (WebTls::Files { cert, .. }, WebTls::Files { key, .. }) = (&one, &two) else {
            unreachable!()
        };
        let mixed = WebTls::Files {
            cert: cert.clone(),
            key: key.clone(),
        };
        let error = load(&mixed, None, &[]).unwrap_err().to_string();
        assert!(error.contains("does not match"), "{error}");
        assert!(!error.contains("PRIVATE KEY"), "{error}");
        // A key that does not parse is refused.
        let broken = WebTls::Files {
            cert: cert.clone(),
            key: cert.clone(),
        };
        assert!(load(&broken, None, &[]).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(key, std::fs::Permissions::from_mode(0o640)).unwrap();
            // A group (such as ssl-cert) may read the operator's key...
            let WebTls::Files { cert: own, .. } = &two else {
                unreachable!()
            };
            let grouped = WebTls::Files {
                cert: own.clone(),
                key: key.clone(),
            };
            assert!(load(&grouped, None, &[]).is_ok());
            // ...but nobody else.
            std::fs::set_permissions(key, std::fs::Permissions::from_mode(0o644)).unwrap();
            let error = load(&grouped, None, &[]).unwrap_err().to_string();
            assert!(error.contains("other users"), "{error}");
        }
    }
}
