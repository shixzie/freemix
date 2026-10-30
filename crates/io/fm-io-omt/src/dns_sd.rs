//! DNS-SD advertisement and browsing for OMT sources (`_omt._tcp`).
//!
//! The discovery server (see [`crate::discovery`]) works without multicast;
//! this module adds the record a LAN service browser sees:
//! `HOSTNAME (Source Name)._omt._tcp.local` with the sender's port and
//! addresses. Dropping an [`OmtAdvertiser`] withdraws its record.

use std::net::IpAddr;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};

use crate::discovery::{DiscoveryError, OmtAddress};

/// The service type with its DNS-SD domain.
pub const OMT_DNS_SD_SERVICE: &str = "_omt._tcp.local.";

/// The DNS-SD record for `source` on `hostname` at `port`.
///
/// # Errors
///
/// The names must be valid DNS-SD names.
pub fn service_info(
    hostname: &str,
    source: &str,
    port: u16,
    addresses: &[IpAddr],
) -> Result<ServiceInfo, DiscoveryError> {
    let instance = format!("{hostname} ({source})");
    if !OmtAddress::name_is_valid(&instance) {
        return Err(DiscoveryError::Protocol("the source name is not usable"));
    }
    let host = format!("{}.local.", hostname.to_ascii_lowercase());
    ServiceInfo::new(
        OMT_DNS_SD_SERVICE,
        &instance,
        &host,
        addresses,
        port,
        &[] as &[(&str, &str)],
    )
    .map(|info| {
        if addresses.is_empty() {
            info.enable_addr_auto()
        } else {
            info
        }
    })
    .map_err(|error| DiscoveryError::Mdns(error.to_string()))
}

/// A running advert: dropping it withdraws the service.
pub struct OmtAdvertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl OmtAdvertiser {
    /// Advertises `source` on `_omt._tcp`.
    ///
    /// # Errors
    ///
    /// No multicast responder could start or the record was refused.
    pub fn start(
        hostname: &str,
        source: &str,
        port: u16,
        addresses: &[IpAddr],
    ) -> Result<Self, DiscoveryError> {
        let info = service_info(hostname, source, port, addresses)?;
        let fullname = info.get_fullname().to_owned();
        let daemon = ServiceDaemon::new().map_err(|error| DiscoveryError::Io(error.to_string()))?;
        daemon
            .register(info)
            .map_err(|error| DiscoveryError::Io(error.to_string()))?;
        Ok(Self { daemon, fullname })
    }

    /// The advertised full name.
    #[must_use]
    pub fn fullname(&self) -> &str {
        &self.fullname
    }
}

impl Drop for OmtAdvertiser {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// Browses `_omt._tcp` for `duration` and returns every resolved source.
///
/// # Errors
///
/// The responder could not start or browse.
pub fn browse(duration: Duration) -> Result<Vec<OmtAddress>, DiscoveryError> {
    let daemon = ServiceDaemon::new().map_err(|error| DiscoveryError::Io(error.to_string()))?;
    let receiver = daemon
        .browse(OMT_DNS_SD_SERVICE)
        .map_err(|error| DiscoveryError::Io(error.to_string()))?;
    let deadline = Instant::now() + duration;
    let mut sources = Vec::new();
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let Ok(event) = receiver.recv_timeout(left) else {
            break;
        };
        let ServiceEvent::ServiceResolved(info) = event else {
            continue;
        };
        let name = info
            .get_fullname()
            .strip_suffix(OMT_DNS_SD_SERVICE)
            .map_or_else(
                || info.get_fullname().to_owned(),
                |name| name.trim_end_matches('.').to_owned(),
            );
        if !OmtAddress::name_is_valid(&name) {
            continue;
        }
        sources.push(OmtAddress {
            name,
            port: info.get_port(),
            addresses: info
                .get_addresses_v4()
                .into_iter()
                .map(IpAddr::V4)
                .collect(),
            removed: false,
        });
    }
    let _ = daemon.shutdown();
    Ok(sources)
}
