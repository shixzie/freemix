//! OMT source discovery (IN-019): browses `_omt._tcp` so the Add Input
//! network form names the senders on the LAN instead of the operator typing
//! an address.
//!
//! A browse listens for a short window every few seconds, because a DNS-SD
//! query only learns what answers while it listens. The latest complete
//! result replaces the list; a failed browse (no multicast responder) keeps
//! the previous one, so a transient network change never empties the form.

use std::net::IpAddr;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use fm_ui_egui::OmtSource;

/// How long one browse listens.
const BROWSE_WINDOW: Duration = Duration::from_millis(800);
/// The pause between browses.
const BROWSE_INTERVAL: Duration = Duration::from_secs(3);

/// The running browse; dropping it stops the next round.
pub(crate) struct OmtDiscovery {
    found: Receiver<Vec<OmtSource>>,
    stop: Arc<Mutex<bool>>,
    latest: Vec<OmtSource>,
}

impl OmtDiscovery {
    /// Starts browsing `_omt._tcp` in the background.
    pub(crate) fn start() -> Self {
        let (sender, found) = mpsc::channel();
        let stop = Arc::new(Mutex::new(false));
        let thread_stop = Arc::clone(&stop);
        let _ = thread::Builder::new()
            .name("freemix-omt-discovery".to_owned())
            .spawn(move || {
                while !*thread_stop.lock().unwrap_or_else(PoisonError::into_inner) {
                    if let Ok(addresses) = fm_io_omt::browse(BROWSE_WINDOW) {
                        let sources = addresses.into_iter().filter_map(source).collect();
                        if sender.send(sources).is_err() {
                            return;
                        }
                    }
                    thread::sleep(BROWSE_INTERVAL);
                }
            });
        Self {
            found,
            stop,
            latest: Vec::new(),
        }
    }

    /// The sources the latest completed browse found.
    pub(crate) fn latest(&mut self) -> &[OmtSource] {
        while let Ok(sources) = self.found.try_recv() {
            self.latest = sources;
        }
        &self.latest
    }

    /// A reader that browses nothing, for a test console.
    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        let (_sender, found) = mpsc::channel();
        Self {
            found,
            stop: Arc::new(Mutex::new(false)),
            latest: Vec::new(),
        }
    }
}

impl Drop for OmtDiscovery {
    fn drop(&mut self) {
        *self.stop.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }
}

/// One discovered address as a stream source, preferring IPv4.
fn source(address: fm_io_omt::OmtAddress) -> Option<OmtSource> {
    if address.removed {
        return None;
    }
    let host = address
        .addresses
        .iter()
        .find(|address| address.is_ipv4())
        .or_else(|| address.addresses.first())?;
    let host = match host {
        IpAddr::V4(address) => address.to_string(),
        IpAddr::V6(address) => format!("[{address}]"),
    };
    Some(OmtSource {
        name: address.name,
        url: format!("omt://{host}:{}", address.port),
    })
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    fn address(name: &str, addresses: Vec<IpAddr>, port: u16) -> fm_io_omt::OmtAddress {
        fm_io_omt::OmtAddress {
            name: name.to_owned(),
            port,
            addresses,
            removed: false,
        }
    }

    #[test]
    fn a_source_url_prefers_ipv4_and_brackets_ipv6() {
        let found = source(address(
            "STUDIO (Camera 1)",
            vec![
                IpAddr::V6(Ipv6Addr::LOCALHOST),
                IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)),
            ],
            6500,
        ))
        .unwrap();
        assert_eq!(found.name, "STUDIO (Camera 1)");
        assert_eq!(found.url, "omt://192.168.1.20:6500");
        let v6 = source(address(
            "STUDIO (Camera 1)",
            vec![IpAddr::V6(Ipv6Addr::LOCALHOST)],
            6500,
        ))
        .unwrap();
        assert_eq!(v6.url, "omt://[::1]:6500");
        // A record with no address yet, or a withdrawal, lists nothing.
        assert_eq!(source(address("STUDIO", Vec::new(), 6500)), None);
        let mut removed = address("STUDIO", vec![IpAddr::V4(Ipv4Addr::LOCALHOST)], 6500);
        removed.removed = true;
        assert_eq!(source(removed), None);
    }
}
