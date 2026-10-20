//! mDNS / DNS-SD advertisement of the web gateway (RC-007), so a phone on
//! the LAN opens `https://<name>.local` instead of an address.
//!
//! The gateway registers two services on the same host name: the `FreeMix`
//! service type `_freemix._tcp` (for FreeMix-aware browsers and tools) and
//! `_https._tcp` (so generic service browsers list it as a web page). Both
//! carry TXT records naming the pages and the protocol; none carries a
//! secret, a pairing code, or a fingerprint. The responder (`mdns-sd`, pure
//! Rust, no async runtime) also answers A and AAAA queries for the host name
//! and follows interface address changes.

use std::net::IpAddr;

use mdns_sd::{IfKind, ServiceDaemon, ServiceInfo};

/// The `FreeMix` web gateway service type.
pub(crate) const FREEMIX_SERVICE: &str = "_freemix._tcp.local.";
/// The generic web service type.
pub(crate) const HTTPS_SERVICE: &str = "_https._tcp.local.";
/// The default label when the computer's name gives none.
const DEFAULT_LABEL: &str = "freemix";
const MAX_LABEL_BYTES: usize = 63;

/// Whether `label` is one DNS label of lowercase letters, digits, and
/// hyphens.
pub(crate) fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= MAX_LABEL_BYTES
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// `freemix-<computer name>`, reduced to one valid label: the computer's
/// own `.local` name belongs to the operating system's responder, so the
/// gateway never claims it.
pub(crate) fn default_label(hostname: Option<&str>) -> String {
    let mut label = String::from(DEFAULT_LABEL);
    let host = hostname
        .unwrap_or_default()
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut cleaned = String::new();
    for character in host.chars() {
        let character = if character.is_ascii_alphanumeric() {
            character
        } else {
            '-'
        };
        if !(character == '-' && (cleaned.is_empty() || cleaned.ends_with('-'))) {
            cleaned.push(character);
        }
    }
    let cleaned = cleaned.trim_end_matches('-');
    if !cleaned.is_empty() {
        label.push('-');
        label.push_str(cleaned);
    }
    label.truncate(MAX_LABEL_BYTES);
    label.trim_end_matches('-').to_owned()
}

/// The computer's name, if the operating system gives one.
pub(crate) fn hostname() -> Option<String> {
    gethostname::gethostname().into_string().ok()
}

/// What is advertised: the `.local` host name, the instance name people
/// see in service browsers, the port, and the addresses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Advertisement {
    /// `<label>.local`.
    pub(crate) host: String,
    pub(crate) instance: String,
    pub(crate) port: u16,
    /// Empty: every interface's addresses, kept current by the responder.
    pub(crate) addresses: Vec<IpAddr>,
}

impl Advertisement {
    pub(crate) fn new(
        label: &str,
        computer: Option<&str>,
        port: u16,
        addresses: Vec<IpAddr>,
    ) -> Self {
        let instance = computer
            .and_then(|name| name.split('.').next())
            .filter(|name| !name.is_empty())
            .map_or_else(|| "FreeMix".to_owned(), |name| format!("FreeMix on {name}"));
        Self {
            host: format!("{label}.local"),
            instance,
            port,
            addresses,
        }
    }

    /// The service records: `_freemix._tcp` then `_https._tcp`.
    ///
    /// # Errors
    ///
    /// A name the DNS-SD library refuses.
    pub(crate) fn records(&self) -> Result<Vec<ServiceInfo>, String> {
        let host = format!("{}.", self.host);
        let properties: [(&str, &str); 5] = [
            ("path", "/"),
            ("tally", "/tally"),
            ("operator", "/operator"),
            ("titles", "/titles"),
            ("ca", fm_protocol::WEB_CA_PATH),
        ];
        let mut records = Vec::with_capacity(2);
        for service in [FREEMIX_SERVICE, HTTPS_SERVICE] {
            let info = ServiceInfo::new(
                service,
                &self.instance,
                &host,
                self.addresses.as_slice(),
                self.port,
                &properties[..],
            )
            .map_err(|error| format!("cannot describe the mDNS service: {error}"))?;
            records.push(if self.addresses.is_empty() {
                info.enable_addr_auto()
            } else {
                info
            });
        }
        Ok(records)
    }
}

/// The running responder; dropping it withdraws the services.
pub(crate) struct Mdns {
    daemon: ServiceDaemon,
    fullnames: Vec<String>,
}

impl Mdns {
    /// Starts the responder and registers `advertisement`.
    ///
    /// # Errors
    ///
    /// No multicast socket could be opened, or a record was refused. The
    /// gateway keeps serving by address either way.
    pub(crate) fn start(advertisement: &Advertisement, ipv4_only: bool) -> Result<Self, String> {
        let records = advertisement.records()?;
        let daemon = ServiceDaemon::new().map_err(|error| format!("cannot start mDNS: {error}"))?;
        // Loopback is never advertised, nor IPv6 when the gateway listens
        // on IPv4 only, so a phone never resolves an address it cannot use.
        let mut silent = vec![IfKind::LoopbackV4, IfKind::LoopbackV6];
        if ipv4_only {
            silent.push(IfKind::IPv6);
        }
        if let Err(error) = daemon.disable_interface(silent) {
            let _ = daemon.shutdown();
            return Err(format!("cannot configure mDNS: {error}"));
        }
        let mut fullnames = Vec::new();
        for record in records {
            fullnames.push(record.get_fullname().to_owned());
            if let Err(error) = daemon.register(record) {
                let _ = daemon.shutdown();
                return Err(format!("cannot advertise over mDNS: {error}"));
            }
        }
        Ok(Self { daemon, fullnames })
    }
}

impl Drop for Mdns {
    fn drop(&mut self) {
        // Goodbye packets tell browsers the gateway left; best effort.
        for name in &self.fullnames {
            let _ = self.daemon.unregister(name);
        }
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::{Advertisement, FREEMIX_SERVICE, HTTPS_SERVICE, default_label, valid_label};

    #[test]
    fn the_default_label_derives_from_the_computer_name() {
        assert_eq!(default_label(Some("Studio-PC")), "freemix-studio-pc");
        assert_eq!(default_label(Some("studio.example.lan")), "freemix-studio");
        assert_eq!(
            default_label(Some("Juan's MacBook Pro")),
            "freemix-juan-s-macbook-pro"
        );
        assert_eq!(default_label(Some("--")), "freemix");
        assert_eq!(default_label(None), "freemix");
        let long = default_label(Some(&"x".repeat(100)));
        assert_eq!(long.len(), 63);
        for name in ["Studio-PC", "a_b", "ñandú", "x-"] {
            assert!(valid_label(&default_label(Some(name))), "{name}");
        }
        assert!(!valid_label("a.b") && !valid_label("-a") && !valid_label("A"));
    }

    #[test]
    fn records_advertise_both_services_on_one_host() {
        let advertisement = Advertisement::new(
            "freemix-studio",
            Some("studio.example.lan"),
            8443,
            vec!["192.168.1.20".parse().unwrap()],
        );
        assert_eq!(advertisement.host, "freemix-studio.local");
        assert_eq!(advertisement.instance, "FreeMix on studio");
        let records = advertisement.records().unwrap();
        assert_eq!(records.len(), 2);
        for (record, service) in records.iter().zip([FREEMIX_SERVICE, HTTPS_SERVICE]) {
            assert_eq!(record.get_type(), service);
            assert_eq!(
                record.get_fullname(),
                format!("FreeMix on studio.{service}")
            );
            assert_eq!(record.get_hostname(), "freemix-studio.local.");
            assert_eq!(record.get_port(), 8443);
            assert_eq!(record.get_property_val_str("path"), Some("/"));
            assert_eq!(record.get_property_val_str("tally"), Some("/tally"));
            assert_eq!(record.get_property_val_str("operator"), Some("/operator"));
            assert_eq!(
                record.get_property_val_str("ca"),
                Some(fm_protocol::WEB_CA_PATH)
            );
            assert!(!record.is_addr_auto());
            let addresses: Vec<_> = record.get_addresses_v4().into_iter().copied().collect();
            assert_eq!(
                addresses,
                ["192.168.1.20".parse::<std::net::Ipv4Addr>().unwrap()]
            );
            // Nothing secret is advertised.
            for property in record.get_properties().iter() {
                assert!(!["fp", "token", "code"].contains(&property.key()));
            }
        }
        // Without fixed addresses the responder follows the interfaces.
        let every = Advertisement::new("freemix", None, 443, Vec::new());
        assert_eq!(every.instance, "FreeMix");
        assert!(
            every
                .records()
                .unwrap()
                .iter()
                .all(mdns_sd::ServiceInfo::is_addr_auto)
        );
    }
}
