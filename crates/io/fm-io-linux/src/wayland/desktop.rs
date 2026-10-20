//! The real Wayland desktop behind a [`PortalCapture`](super::PortalCapture)
//! (IN-011): the portal conversation ([`super::portal`]) and, in builds
//! with `PipeWire`, the shared stream.

use std::cell::RefCell;

use fm_types::screen_capture::{PORTAL_BACKEND, ScreenCaptureInventory};

use super::portal::{Portal, Refusal};
use super::{PortalBackend, ShareEnd, ShareRequest, ShareSink, SourceKind};

/// Shares a screen or a window of the session's desktop through its
/// xdg-desktop-portal.
#[derive(Clone, Debug, Default)]
pub struct DesktopPortal {
    address: Option<String>,
}

impl DesktopPortal {
    /// The portal on the session bus.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The portal on the bus at `address` (a private bus in tests).
    #[must_use]
    pub fn with_address(address: impl Into<String>) -> Self {
        Self {
            address: Some(address.into()),
        }
    }
}

fn end_of(refusal: Refusal) -> ShareEnd {
    match refusal {
        Refusal::Stopped => ShareEnd::Stopped,
        Refusal::Denied(reason) => ShareEnd::Denied(reason),
        Refusal::Unsupported(reason) => ShareEnd::Unsupported(reason),
        Refusal::Failed(reason) => ShareEnd::Failed(reason),
    }
}

impl PortalBackend for DesktopPortal {
    fn share(&mut self, request: &ShareRequest, sink: &mut dyn ShareSink) -> ShareEnd {
        if !super::PIPEWIRE_BUILT {
            // Asking would show a dialog for a stream nobody can read.
            return ShareEnd::Unsupported(super::NO_PIPEWIRE.into());
        }
        let sink = RefCell::new(sink);
        let started = async_io::block_on(async {
            let mut portal = Portal::connect(self.address.as_deref())
                .await
                .map_err(|error| {
                    Refusal::Failed(format!("the session bus cannot be reached: {error}"))
                })?;
            let capabilities = portal.capabilities().await?;
            let stop = || sink.borrow().stopping();
            let mut awaiting = || sink.borrow_mut().awaiting_selection();
            let grant = portal
                .share(request, &capabilities, &stop, &mut awaiting)
                .await;
            match grant {
                Ok(grant) => Ok((portal, grant)),
                Err(refusal) => {
                    portal.close().await;
                    Err(refusal)
                }
            }
        });
        let sink = sink.into_inner();
        let (mut portal, grant) = match started {
            Ok(started) => started,
            Err(refusal) => return end_of(refusal),
        };
        sink.granted(grant.restore_token.clone(), grant.note);
        let end = stream(grant, &mut portal, sink);
        async_io::block_on(portal.close());
        end
    }
}

/// Reads the granted stream until the share ends.
#[cfg(freemix_pipewire)]
fn stream(grant: super::portal::Grant, portal: &mut Portal, sink: &mut dyn ShareSink) -> ShareEnd {
    super::pipewire::stream(grant.remote, grant.node_id, sink, &mut || {
        portal
            .closed()
            .then(|| ShareEnd::Denied(super::STOPPED_ON_DESKTOP.into()))
    })
}

/// Without `PipeWire` nothing reads the stream (never reached: a share
/// stops before asking).
#[cfg(not(freemix_pipewire))]
fn stream(grant: super::portal::Grant, portal: &mut Portal, sink: &mut dyn ShareSink) -> ShareEnd {
    let _ = (grant, portal, sink);
    ShareEnd::Unsupported(super::NO_PIPEWIRE.into())
}

/// What the desktop's portal can share, as an inventory: supported with
/// the `portal` backend and nothing listed (the dialog picks), or
/// unsupported and why. `address` names a bus other than the session bus.
#[must_use]
pub fn inventory(address: Option<&str>) -> ScreenCaptureInventory {
    if !super::PIPEWIRE_BUILT {
        return ScreenCaptureInventory::unsupported(super::NO_PIPEWIRE);
    }
    let capabilities = async_io::block_on(async {
        let portal = Portal::connect(address).await.map_err(|error| {
            format!("Wayland session: the session bus cannot be reached: {error}")
        })?;
        portal
            .capabilities()
            .await
            .map_err(|refusal| match refusal {
                Refusal::Stopped => String::new(),
                Refusal::Denied(reason)
                | Refusal::Unsupported(reason)
                | Refusal::Failed(reason) => {
                    format!("Wayland session: {reason}")
                }
            })
    });
    let capabilities = match capabilities {
        Ok(capabilities) => capabilities,
        Err(reason) => return ScreenCaptureInventory::unsupported(&reason),
    };
    let screens = capabilities.source_types & SourceKind::Monitor.portal_bit() != 0;
    let windows = capabilities.source_types & SourceKind::Window.portal_bit() != 0;
    if !screens && !windows {
        return ScreenCaptureInventory::unsupported(super::NO_SCREEN_SHARING);
    }
    ScreenCaptureInventory {
        supported: true,
        backend: PORTAL_BACKEND.into(),
        screens: Vec::new(),
        windows: Vec::new(),
        error: if !windows {
            Some(super::NO_WINDOW_SHARING.into())
        } else if !screens {
            Some(super::NO_SCREEN_SHARING.into())
        } else {
            None
        },
    }
}
