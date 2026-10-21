//! The per-install local certificate authority (`--web-tls`): a root
//! certificate generated once in the web state directory, which an operator
//! installs on each phone or tablet (Studio offers it for download with a
//! QR code), and the gateway's leaf certificates it issues for the LAN
//! addresses, `localhost`, the `--web-host` names, and the mDNS name.
//!
//! The root carries critical X.509 name constraints: it vouches only for
//! private and link-local addresses, loopback, the `.local`, `localhost`,
//! `home.arpa`, and `.internal` names, and the host names and addresses the
//! gateway had when the root was made, so a stolen root cannot mint a
//! certificate any phone would accept for a public site. A root made before
//! the constraints keeps working unconstrained (phones trust it, so it is
//! never replaced silently); removing its two files makes a constrained one.
//!
//! The root's key never leaves the state directory: both key files are
//! written with mode 0600 (in a 0700 directory), refused when other users
//! can read them, and never formatted into any message or log.

use std::{
    fmt::Write as _,
    fs,
    net::{IpAddr, Ipv6Addr},
    path::Path,
};

use rcgen::{
    BasicConstraints, CertificateParams, CidrSubnet, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, GeneralSubtree, IsCa, Issuer, KeyPair, KeyUsagePurpose,
    NameConstraints, PublicKeyData, SanType,
};
use rustls::pki_types::{CertificateDer, pem::PemObject};
use x509_parser::{certificate::X509Certificate, extensions::GeneralName, prelude::FromDer};

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

/// DNS names (and every name under them) the root vouches for besides the
/// gateway's own host names: mDNS, loopback, and the names reserved for
/// private networks (RFC 6762, RFC 6761, RFC 8375, ICANN's `.internal`).
const PERMITTED_DNS: [&str; 4] = ["local", "localhost", "home.arpa", "internal"];
/// IPv4 ranges the root vouches for: RFC 1918 private, carrier-grade NAT
/// (overlay VPNs such as Tailscale), link-local, and loopback.
const PERMITTED_V4: [([u8; 4], u8); 6] = [
    ([10, 0, 0, 0], 8),
    ([172, 16, 0, 0], 12),
    ([192, 168, 0, 0], 16),
    ([100, 64, 0, 0], 10),
    ([169, 254, 0, 0], 16),
    ([127, 0, 0, 0], 8),
];
/// IPv6 ranges: unique-local (`fc00::/7`), link-local (`fe80::/10`), and
/// loopback.
const PERMITTED_V6: [(Ipv6Addr, u8); 3] = [
    (Ipv6Addr::new(0xfc00, 0, 0, 0, 0, 0, 0, 0), 7),
    (Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 0), 10),
    (Ipv6Addr::LOCALHOST, 128),
];

/// One permitted subtree of the root's name constraints.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Permitted {
    /// A DNS name and every name under it, lowercase.
    Dns(String),
    /// An address range: network and mask, both 4 or 16 bytes.
    Ip(Vec<u8>, Vec<u8>),
}

/// The permitted subtrees of a new root: the fixed private ranges and
/// names, plus each of `names` (the gateway's host names and addresses)
/// they do not already cover.
fn permitted_subtrees(names: &[String]) -> Vec<GeneralSubtree> {
    let mut subtrees: Vec<GeneralSubtree> = PERMITTED_DNS
        .iter()
        .map(|name| GeneralSubtree::DnsName((*name).to_owned()))
        .collect();
    subtrees.extend(PERMITTED_V4.iter().map(|(network, prefix)| {
        GeneralSubtree::IpAddress(CidrSubnet::from_v4_prefix(*network, *prefix))
    }));
    subtrees.extend(PERMITTED_V6.iter().map(|(network, prefix)| {
        GeneralSubtree::IpAddress(CidrSubnet::from_v6_prefix(network.octets(), *prefix))
    }));
    let fixed = permitted_of(&subtrees);
    for name in names {
        let name = name.trim_end_matches('.').to_ascii_lowercase();
        if name.is_empty() || permits(Some(&fixed), &name) {
            continue;
        }
        let subtree = match name.trim_matches(['[', ']']).parse::<IpAddr>() {
            Ok(address) => GeneralSubtree::IpAddress(CidrSubnet::from_addr_prefix(
                address,
                if address.is_ipv4() { 32 } else { 128 },
            )),
            Err(_) => GeneralSubtree::DnsName(name.trim_start_matches("*.").to_owned()),
        };
        if !subtrees.contains(&subtree) {
            subtrees.push(subtree);
        }
    }
    subtrees
}

/// [`Permitted`] subtrees from rcgen's.
fn permitted_of(subtrees: &[GeneralSubtree]) -> Vec<Permitted> {
    subtrees
        .iter()
        .filter_map(|subtree| match subtree {
            GeneralSubtree::DnsName(name) => Some(Permitted::Dns(name.to_ascii_lowercase())),
            GeneralSubtree::IpAddress(CidrSubnet::V4(network, mask)) => {
                Some(Permitted::Ip(network.to_vec(), mask.to_vec()))
            }
            GeneralSubtree::IpAddress(CidrSubnet::V6(network, mask)) => {
                Some(Permitted::Ip(network.to_vec(), mask.to_vec()))
            }
            _ => None,
        })
        .collect()
}

/// The permitted subtrees in a root's name constraints, `None` when it has
/// none (it vouches for any name), or `Err` when they do not parse.
fn permitted_in(certificate: &X509Certificate<'_>) -> Result<Option<Vec<Permitted>>, ()> {
    let Some(constraints) = certificate.name_constraints().map_err(|_| ())? else {
        return Ok(None);
    };
    let Some(subtrees) = &constraints.value.permitted_subtrees else {
        return Ok(None);
    };
    Ok(Some(
        subtrees
            .iter()
            .filter_map(|subtree| match &subtree.base {
                GeneralName::DNSName(name) => Some(Permitted::Dns(name.to_ascii_lowercase())),
                GeneralName::IPAddress(bytes) if bytes.len() == 8 || bytes.len() == 32 => {
                    let (network, mask) = bytes.split_at(bytes.len() / 2);
                    Some(Permitted::Ip(network.to_vec(), mask.to_vec()))
                }
                _ => None,
            })
            .collect(),
    ))
}

/// Whether a root with `permitted` subtrees vouches for `name` (a DNS name
/// or an address literal), matched as RFC 5280 and browsers match them: a DNS
/// subtree covers the name itself and every name under it, and a name of a
/// kind no subtree constrains is allowed.
fn permits(permitted: Option<&Vec<Permitted>>, name: &str) -> bool {
    let Some(permitted) = permitted else {
        return true;
    };
    if let Ok(address) = name.trim_matches(['[', ']']).parse::<IpAddr>() {
        let octets = match address {
            IpAddr::V4(address) => address.octets().to_vec(),
            IpAddr::V6(address) => address.octets().to_vec(),
        };
        let mut ranges = permitted
            .iter()
            .filter_map(|subtree| match subtree {
                Permitted::Ip(network, mask) => Some((network, mask)),
                Permitted::Dns(_) => None,
            })
            .peekable();
        if ranges.peek().is_none() {
            return true;
        }
        return ranges.any(|(network, mask)| {
            network.len() == octets.len()
                && octets
                    .iter()
                    .zip(network.iter().zip(mask))
                    .all(|(byte, (network, mask))| byte & mask == network & mask)
        });
    }
    let name = name.trim_end_matches('.').to_ascii_lowercase();
    let mut suffixes = permitted
        .iter()
        .filter_map(|subtree| match subtree {
            Permitted::Dns(suffix) => Some(suffix.trim_end_matches('.')),
            Permitted::Ip(..) => None,
        })
        .peekable();
    if suffixes.peek().is_none() {
        return true;
    }
    let below = |suffix: &str| {
        name.strip_suffix(suffix)
            .is_some_and(|rest| rest.len() > 1 && rest.ends_with('.'))
    };
    suffixes.any(|suffix| {
        if suffix.is_empty() {
            true
        } else if let Some(parent) = suffix.strip_prefix('.') {
            below(parent)
        } else {
            name == suffix || below(suffix)
        }
    })
}

/// The loaded root: its certificate and the key that signs leaves.
pub(crate) struct LocalCa {
    der: CertificateDer<'static>,
    key: KeyPair,
    fingerprint: String,
    /// The root's permitted name subtrees; `None` for a root made before
    /// name constraints, which vouches for any name.
    permitted: Option<Vec<Permitted>>,
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
    /// exists, constrained to the private ranges and to `names` (the
    /// gateway's host names and addresses now).
    ///
    /// # Errors
    ///
    /// Only one of the two files present, key files other users can read,
    /// a root that does not parse, is not a CA, has expired, or does not
    /// match its key, or a directory that cannot be written. A damaged root
    /// is never replaced silently: phones trust it, so the operator decides.
    pub(crate) fn open_or_create(
        directory: &Path,
        now_ms: u64,
        names: &[String],
    ) -> AppResult<Self> {
        tls::create_private_dir(directory)?;
        let cert_path = directory.join(CA_CERT_FILE);
        let key_path = directory.join(CA_KEY_FILE);
        match (cert_path.exists(), key_path.exists()) {
            (false, false) => {
                let (cert_pem, key_pem) = generate_root(names)?;
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
        let permitted = permitted_in(&certificate)
            .map_err(|()| damaged("has name constraints that do not parse"))?;
        let fingerprint = tls::fingerprint(der.as_ref());
        Ok(Self {
            der,
            key,
            fingerprint,
            permitted,
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

    /// Whether the root's name constraints let it vouch for `name` (a DNS
    /// name or an address literal).
    pub(crate) fn permits(&self, name: &str) -> bool {
        permits(self.permitted.as_ref(), name)
    }

    /// Whether the root carries name constraints.
    #[cfg(test)]
    pub(crate) const fn constrained(&self) -> bool {
        self.permitted.is_some()
    }

    /// Issues a leaf for `names` (DNS names or address literals): PEM
    /// certificate and PEM key.
    ///
    /// # Errors
    ///
    /// A name that is neither an address nor a valid DNS name, a name the
    /// root's constraints do not permit (browsers would reject the whole
    /// leaf), or a signing failure.
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
            if !self.permits(name) {
                return Err(format!(
                    "the local web CA cannot vouch for `{name}` (outside its name constraints)"
                )
                .into());
            }
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

/// A new root constrained to the private ranges and `names`: PEM
/// certificate and PEM key.
fn generate_root(names: &[String]) -> AppResult<(String, String)> {
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
    // rcgen marks name constraints critical, so a verifier that cannot
    // enforce them rejects the root instead of ignoring them.
    params.name_constraints = Some(NameConstraints {
        permitted_subtrees: permitted_subtrees(names),
        excluded_subtrees: Vec::new(),
    });
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
    use x509_parser::prelude::FromDer;

    fn now_ms() -> u64 {
        u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp()).unwrap() * 1_000
    }

    #[test]
    fn the_root_is_created_once_and_kept_private() {
        let directory = tempfile::tempdir().unwrap();
        let state = directory.path().join("web");
        let first = LocalCa::open_or_create(&state, now_ms(), &[]).unwrap();
        let second = LocalCa::open_or_create(&state, now_ms(), &[]).unwrap();
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
            let error = LocalCa::open_or_create(&state, now_ms(), &[])
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
        let error = LocalCa::open_or_create(&state, now_ms(), &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("does not parse"), "{error}");
        std::fs::remove_file(state.join(CA_CERT_FILE)).unwrap();
        let error = LocalCa::open_or_create(&state, now_ms(), &[])
            .unwrap_err()
            .to_string();
        assert!(error.contains("incomplete"), "{error}");
    }

    #[test]
    fn issued_leaves_chain_to_the_root_for_every_name() {
        let directory = tempfile::tempdir().unwrap();
        let ca = LocalCa::open_or_create(directory.path(), now_ms(), &[]).unwrap();
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
        let stranger = LocalCa::open_or_create(other.path(), now_ms(), &[]).unwrap();
        assert!(!certs::signed_by(&leaf, stranger.der()));
        assert_ne!(stranger.fingerprint(), ca.fingerprint());
    }

    fn verifier(
        root: &CertificateDer<'static>,
    ) -> std::sync::Arc<rustls::client::WebPkiServerVerifier> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(root.clone()).unwrap();
        rustls::client::WebPkiServerVerifier::builder_with_provider(
            std::sync::Arc::new(roots),
            std::sync::Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .unwrap()
    }

    /// A leaf for `names` signed with the root's key, bypassing
    /// [`LocalCa::issue`]'s own check: what a thief holding the key mints.
    fn minted(ca: &LocalCa, names: &[&str]) -> CertificateDer<'static> {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        params.is_ca = rcgen::IsCa::ExplicitNoCa;
        params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
        let issuer = rcgen::Issuer::from_ca_cert_der(ca.der(), &ca.key).unwrap();
        params.signed_by(&key, &issuer).unwrap().der().clone()
    }

    #[test]
    fn the_root_is_name_constrained_to_the_lan() {
        let directory = tempfile::tempdir().unwrap();
        let hosts = [
            "studio.example.lan".to_owned(),
            "freemix-a.local".to_owned(),
            "203.0.113.7".to_owned(),
            "192.168.1.20".to_owned(),
        ];
        let ca = LocalCa::open_or_create(directory.path(), now_ms(), &hosts).unwrap();
        assert!(ca.constrained());
        // The extension is there and critical.
        let (_, root) =
            x509_parser::certificate::X509Certificate::from_der(ca.der().as_ref()).unwrap();
        let constraints = root.name_constraints().unwrap().unwrap();
        assert!(constraints.critical);
        assert!(constraints.value.excluded_subtrees.is_none());
        for name in [
            "freemix-studio.local",
            "localhost",
            "studio.example.lan",
            "cam.studio.example.lan",
            "box.home.arpa",
            "studio.internal",
            "10.1.2.3",
            "172.31.255.1",
            "192.168.1.20",
            "100.100.1.2",
            "169.254.3.4",
            "127.0.0.1",
            "203.0.113.7",
            "::1",
            "fd00::20",
            "fe80::1",
        ] {
            assert!(ca.permits(name), "{name}");
        }
        for name in [
            "example.com",
            "accounts.google.com",
            "local.example.com",
            "notlocal",
            "example.lan",
            "8.8.8.8",
            "172.32.0.1",
            "203.0.113.8",
            "2001:db8::1",
        ] {
            assert!(!ca.permits(name), "{name}");
        }
        assert!(ca.issue(&["example.com".to_owned()]).is_err());

        // The leaf it issues for the gateway's names validates under the
        // constraints, exactly as WebPKI checks a phone's installed root.
        let names = [
            "freemix-a.local",
            "localhost",
            "127.0.0.1",
            "::1",
            "studio.example.lan",
            "192.168.1.20",
            "10.0.0.9",
            "fd00::20",
        ];
        let owned: Vec<String> = names.iter().map(|name| (*name).to_owned()).collect();
        let (cert_pem, _) = ca.issue(&owned).unwrap();
        let leaf = CertificateDer::from_pem_slice(cert_pem.as_bytes()).unwrap();
        let verifier = verifier(ca.der());
        let now = UnixTime::now();
        for name in names {
            let server = ServerName::try_from(name).unwrap();
            verifier
                .verify_server_cert(&leaf, &[], &server, &[], now)
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }

        // A stolen root key mints nothing a phone accepts for a public
        // site: the constraints reject the whole leaf.
        for public in ["example.com", "8.8.8.8", "2001:db8::1"] {
            let forged = minted(&ca, &[public]);
            let server = ServerName::try_from(public).unwrap();
            let error = verifier
                .verify_server_cert(&forged, &[], &server, &[], now)
                .unwrap_err();
            assert!(
                format!("{error:?}").contains("NameConstraint"),
                "{public}: {error:?}"
            );
        }
        // Nor a leaf that hides a public name among LAN ones.
        let mixed = minted(&ca, &["freemix-a.local", "example.com"]);
        let server = ServerName::try_from("freemix-a.local").unwrap();
        assert!(
            verifier
                .verify_server_cert(&mixed, &[], &server, &[], now)
                .is_err()
        );
    }

    /// A root made before name constraints keeps working (phones trust it):
    /// it is loaded unconstrained, never replaced.
    #[test]
    fn a_root_without_constraints_is_kept() {
        let directory = tempfile::tempdir().unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::default();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "FreeMix local CA 0A0B0C");
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
        params.key_usages = vec![
            rcgen::KeyUsagePurpose::KeyCertSign,
            rcgen::KeyUsagePurpose::DigitalSignature,
        ];
        let root = params.self_signed(&key).unwrap();
        crate::web::tls::write_private(
            &directory.path().join(super::CA_KEY_FILE),
            key.serialize_pem().as_bytes(),
        )
        .unwrap();
        crate::web::tls::write_private(&directory.path().join(CA_CERT_FILE), root.pem().as_bytes())
            .unwrap();
        let ca = LocalCa::open_or_create(directory.path(), now_ms(), &[]).unwrap();
        assert_eq!(ca.der().as_ref(), root.der().as_ref());
        assert!(!ca.constrained());
        assert!(ca.permits("203.0.113.7") && ca.permits("studio.example.com"));
        assert!(ca.issue(&["203.0.113.7".to_owned()]).is_ok());
    }
}
