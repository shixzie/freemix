//! Certificate checks made before the gateway serves a certificate: the
//! chain parses, the leaf is a TLS server certificate that is valid now and
//! names at least one host, each certificate is signed by the next, and the
//! key matches the leaf. Nothing here ever formats key material.

use std::net::IpAddr;

use rustls::pki_types::CertificateDer;
use x509_parser::{certificate::X509Certificate, extensions::GeneralName, prelude::FromDer};

/// What the gateway needs to know about a validated leaf.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LeafInfo {
    /// Unix milliseconds.
    pub(crate) not_before_ms: u64,
    pub(crate) not_after_ms: u64,
    /// DNS names (lowercase, wildcards kept as `*.example.lan`).
    pub(crate) dns_names: Vec<String>,
    pub(crate) addresses: Vec<IpAddr>,
}

impl LeafInfo {
    /// Whether the leaf names `host` (a DNS name or an address literal),
    /// with a wildcard covering exactly one leftmost label.
    pub(crate) fn covers(&self, host: &str) -> bool {
        if let Ok(address) = host.trim_matches(['[', ']']).parse::<IpAddr>() {
            return self.addresses.contains(&address);
        }
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        self.dns_names.iter().any(|name| {
            if let Some(suffix) = name.strip_prefix("*.") {
                host.split_once('.')
                    .is_some_and(|(label, rest)| !label.is_empty() && rest == suffix)
            } else {
                *name == host
            }
        })
    }
}

fn parse<'a>(der: &'a CertificateDer<'_>, what: &str) -> Result<X509Certificate<'a>, String> {
    match X509Certificate::from_der(der.as_ref()) {
        Ok(([], certificate)) => Ok(certificate),
        Ok(_) => Err(format!("the {what} has trailing data")),
        Err(error) => Err(format!("the {what} does not parse: {error}")),
    }
}

fn millis(seconds: i64) -> u64 {
    u64::try_from(seconds).unwrap_or(0).saturating_mul(1_000)
}

/// Checks `chain` (leaf first) for serving at `now_ms`.
///
/// # Errors
///
/// A certificate that does not parse, a leaf that is a CA, lacks the TLS
/// server purpose, names no host, or is not valid at `now_ms`, or a chain
/// whose certificates are not each signed by the next.
pub(crate) fn inspect_chain(chain: &[CertificateDer<'_>], now_ms: u64) -> Result<LeafInfo, String> {
    let leaf_der = chain
        .first()
        .ok_or("the certificate file holds no certificate")?;
    let leaf = parse(leaf_der, "certificate")?;
    if leaf.is_ca() {
        return Err("the first certificate is a CA, not a server certificate".into());
    }
    let usage = leaf
        .extended_key_usage()
        .map_err(|error| format!("the certificate's extended key usage is invalid: {error}"))?;
    if let Some(usage) = usage
        && !(usage.value.server_auth || usage.value.any)
    {
        return Err("the certificate is not for TLS servers (no serverAuth usage)".into());
    }
    let validity = leaf.validity();
    let not_before_ms = millis(validity.not_before.timestamp());
    let not_after_ms = millis(validity.not_after.timestamp());
    if now_ms < not_before_ms {
        return Err("the certificate is not valid yet".into());
    }
    if now_ms >= not_after_ms {
        return Err("the certificate has expired".into());
    }
    let mut dns_names = Vec::new();
    let mut addresses = Vec::new();
    let names = leaf.subject_alternative_name().map_err(|error| {
        format!("the certificate's subject alternative names are invalid: {error}")
    })?;
    for name in names.iter().flat_map(|names| &names.value.general_names) {
        match name {
            GeneralName::DNSName(name) => dns_names.push(name.to_ascii_lowercase()),
            GeneralName::IPAddress(bytes) => match bytes.len() {
                4 => addresses.push(IpAddr::from(<[u8; 4]>::try_from(*bytes).expect("4 bytes"))),
                16 => {
                    addresses.push(IpAddr::from(
                        <[u8; 16]>::try_from(*bytes).expect("16 bytes"),
                    ));
                }
                _ => {}
            },
            _ => {}
        }
    }
    if dns_names.is_empty() && addresses.is_empty() {
        return Err(
            "the certificate names no host (browsers need a subject alternative name)".into(),
        );
    }
    // Each certificate must be signed by the one after it.
    let mut child = leaf;
    for (index, der) in chain.iter().enumerate().skip(1) {
        let issuer = parse(der, &format!("certificate {}", index + 1))?;
        if child.issuer().as_raw() != issuer.subject().as_raw()
            || child.verify_signature(Some(issuer.public_key())).is_err()
        {
            return Err(format!(
                "certificate {index} is not signed by certificate {} of the chain",
                index + 1
            ));
        }
        child = issuer;
    }
    Ok(LeafInfo {
        not_before_ms,
        not_after_ms,
        dns_names,
        addresses,
    })
}

/// Whether `der` is signed by `issuer` (both DER certificates).
pub(crate) fn signed_by(der: &CertificateDer<'_>, issuer: &CertificateDer<'_>) -> bool {
    let (Ok(child), Ok(issuer)) = (parse(der, "certificate"), parse(issuer, "issuer")) else {
        return false;
    };
    child.issuer().as_raw() == issuer.subject().as_raw()
        && child.verify_signature(Some(issuer.public_key())).is_ok()
}

#[cfg(test)]
mod tests {
    use super::{LeafInfo, inspect_chain};
    use rcgen::{
        BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
        KeyUsagePurpose,
    };
    use rustls::pki_types::CertificateDer;

    const DAY_MS: u64 = 86_400_000;

    fn now_ms() -> u64 {
        u64::try_from(time::OffsetDateTime::now_utc().unix_timestamp()).unwrap() * 1_000
    }

    fn leaf_params(names: &[&str]) -> CertificateParams {
        let mut params = CertificateParams::new(
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        params
    }

    #[test]
    fn a_ca_signed_leaf_validates_with_its_names() {
        let ca_key = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        let ca = ca_params.self_signed(&ca_key).unwrap();
        let issuer = Issuer::new(ca_params, &ca_key);
        let leaf_key = KeyPair::generate().unwrap();
        let leaf = leaf_params(&["studio.lan", "*.show.lan", "192.168.1.20"])
            .signed_by(&leaf_key, &issuer)
            .unwrap();
        let chain = [leaf.der().clone(), ca.der().clone()];
        let info = inspect_chain(&chain, now_ms()).unwrap();
        assert!(info.covers("studio.lan") && info.covers("STUDIO.lan."));
        assert!(info.covers("cam.show.lan") && !info.covers("a.cam.show.lan"));
        assert!(!info.covers("show.lan"));
        assert!(info.covers("192.168.1.20") && !info.covers("192.168.1.21"));
        assert!(info.not_before_ms < now_ms() && info.not_after_ms > now_ms());

        // Out of its validity window.
        let error = inspect_chain(&chain, info.not_after_ms).unwrap_err();
        assert!(error.contains("expired"), "{error}");
        let error = inspect_chain(&chain, info.not_before_ms - DAY_MS).unwrap_err();
        assert!(error.contains("not valid yet"), "{error}");

        // A chain whose second certificate did not sign the first.
        let stranger_key = KeyPair::generate().unwrap();
        let stranger = ca_params_self_signed(&stranger_key);
        let error = inspect_chain(&[leaf.der().clone(), stranger], now_ms()).unwrap_err();
        assert!(error.contains("not signed"), "{error}");

        // The CA itself is no server certificate.
        let error = inspect_chain(&[ca.der().clone()], now_ms()).unwrap_err();
        assert!(error.contains("is a CA"), "{error}");
    }

    fn ca_params_self_signed(key: &KeyPair) -> CertificateDer<'static> {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.self_signed(key).unwrap().der().clone()
    }

    #[test]
    fn client_only_and_nameless_leaves_are_refused() {
        let key = KeyPair::generate().unwrap();
        let mut client = leaf_params(&["studio.lan"]);
        client.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
        let der = client.self_signed(&key).unwrap().der().clone();
        let error = inspect_chain(&[der], now_ms()).unwrap_err();
        assert!(error.contains("serverAuth"), "{error}");

        let nameless = leaf_params(&[]).self_signed(&key).unwrap().der().clone();
        let error = inspect_chain(&[nameless], now_ms()).unwrap_err();
        assert!(error.contains("names no host"), "{error}");

        let garbage = CertificateDer::from(vec![0x30, 0x03, 0x02, 0x01, 0x00]);
        assert!(inspect_chain(&[garbage], now_ms()).is_err());
        assert!(inspect_chain(&[], now_ms()).is_err());
    }

    #[test]
    fn coverage_matches_hosts_exactly() {
        let info = LeafInfo {
            not_before_ms: 0,
            not_after_ms: 1,
            dns_names: vec!["freemix-studio.local".into()],
            addresses: vec!["::1".parse().unwrap()],
        };
        assert!(info.covers("freemix-studio.local"));
        assert!(info.covers("[::1]"));
        assert!(!info.covers("freemix-studio.local.evil"));
        assert!(!info.covers("127.0.0.1"));
    }
}
