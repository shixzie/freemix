//! The xdg-desktop-portal `ScreenCast` conversation over D-Bus (IN-011).
//!
//! [`Portal::connect`] opens a connection to the session bus (or a given
//! bus address); [`Portal::capabilities`] reads what the desktop's
//! `ScreenCast` portal offers; [`Portal::share`] runs `CreateSession`,
//! `SelectSources`, and `Start` (the sharing dialog) as portal requests,
//! each answered by its `org.freedesktop.portal.Request::Response` signal
//! (subscribed before the call, at the path the request will have), then
//! `OpenPipeWireRemote` for the `PipeWire` connection of the grant. Every
//! wait polls a stop check at least every tenth of a second; a stopped
//! wait closes the request, which dismisses the dialog.
//!
//! The session lives as long as the [`Portal`]: [`Portal::closed`] polls,
//! without waiting, whether the desktop closed it (the operator stopped
//! sharing on the desktop), and [`Portal::close`] ends it.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_io::Timer;
use futures_lite::{StreamExt, future};
use zbus::proxy::SignalStream;
use zbus::zvariant::{self, ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, Proxy};

use super::{DENIED_CANCELLED, DENIED_REFUSED, ShareRequest, SourceKind, cursor_mode};

/// The portal's bus name, object, and interfaces.
pub const DESTINATION: &str = "org.freedesktop.portal.Desktop";
pub const PATH: &str = "/org/freedesktop/portal/desktop";
pub const SCREEN_CAST: &str = "org.freedesktop.portal.ScreenCast";
pub const REQUEST: &str = "org.freedesktop.portal.Request";
pub const SESSION: &str = "org.freedesktop.portal.Session";
/// `persist_mode` 2: the grant lasts until the operator revokes it.
pub const PERSIST_UNTIL_REVOKED: u32 = 2;
/// How long a call that shows no dialog may take.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a wait checks whether to stop.
const POLL: Duration = Duration::from_millis(100);

/// What the desktop's `ScreenCast` portal offers.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Capabilities {
    /// The interface version (restore tokens since 4).
    pub version: u32,
    /// The source type bits it shares (`1` monitor, `2` window, `4`
    /// virtual).
    pub source_types: u32,
    /// The cursor mode bits it offers (since version 2).
    pub cursor_modes: Option<u32>,
}

/// A granted share.
#[derive(Debug)]
pub struct Grant {
    /// The `PipeWire` node of the shared stream.
    pub node_id: u32,
    /// The token that restores this grant without the dialog, when the
    /// desktop keeps one.
    pub restore_token: Option<String>,
    /// The `PipeWire` connection the grant opened.
    pub remote: OwnedFd,
    /// What the desktop could not honour, if anything.
    pub note: Option<&'static str>,
}

/// Why a share did not start.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// The stop check asked to stop.
    Stopped,
    /// The operator or the desktop refused.
    Denied(String),
    /// The desktop cannot share what was asked for.
    Unsupported(String),
    /// Anything else.
    Failed(String),
}

fn failed(context: &str, error: impl std::fmt::Display) -> Refusal {
    Refusal::Failed(format!("{context}: {error}"))
}

/// A connection to the desktop portal with at most one screen cast session.
pub struct Portal {
    connection: Connection,
    session: Option<OwnedObjectPath>,
    closed: Option<SignalStream<'static>>,
}

/// A unique token for request and session handles.
fn handle_token(kind: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    format!(
        "freemix_{kind}_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// `future` unless `timeout` passes first.
async fn within<T>(
    timeout: Duration,
    what: &str,
    future: impl Future<Output = Result<T, Refusal>>,
) -> Result<T, Refusal> {
    future::or(future, async {
        Timer::after(timeout).await;
        Err(Refusal::Failed(format!(
            "the desktop portal did not answer {what} within {} s",
            timeout.as_secs()
        )))
    })
    .await
}

impl Portal {
    /// Connects to the session bus, or to the bus at `address`.
    ///
    /// # Errors
    ///
    /// Returns why the bus cannot be reached.
    pub async fn connect(address: Option<&str>) -> Result<Self, String> {
        let connection = match address {
            Some(address) => {
                zbus::connection::Builder::address(address)
                    .map_err(|error| error.to_string())?
                    .build()
                    .await
            }
            None => Connection::session().await,
        }
        .map_err(|error| error.to_string())?;
        Ok(Self {
            connection,
            session: None,
            closed: None,
        })
    }

    async fn screen_cast(&self) -> Result<Proxy<'static>, Refusal> {
        Proxy::new(&self.connection, DESTINATION, PATH, SCREEN_CAST)
            .await
            .map_err(|error| failed("the desktop portal cannot be reached", error))
    }

    /// What the desktop's `ScreenCast` portal offers.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal::Unsupported`] when no `ScreenCast` portal answers.
    pub async fn capabilities(&self) -> Result<Capabilities, Refusal> {
        within(CALL_TIMEOUT, "its capabilities", async {
            let proxy = self.screen_cast().await?;
            let unsupported = |error: zbus::Error| {
                Refusal::Unsupported(format!(
                    "the desktop offers no screen sharing portal \
                     (org.freedesktop.portal.ScreenCast): {error}"
                ))
            };
            let source_types = proxy
                .get_property::<u32>("AvailableSourceTypes")
                .await
                .map_err(unsupported)?;
            let version = proxy.get_property::<u32>("version").await.unwrap_or(1);
            let cursor_modes = if version >= 2 {
                proxy.get_property::<u32>("AvailableCursorModes").await.ok()
            } else {
                None
            };
            Ok(Capabilities {
                version,
                source_types,
                cursor_modes,
            })
        })
        .await
    }

    /// The object path a request with `token` gets.
    fn request_path(&self, token: &str) -> Result<String, Refusal> {
        let sender = self
            .connection
            .unique_name()
            .ok_or_else(|| Refusal::Failed("the bus connection has no name".into()))?;
        let sender = sender.as_str().trim_start_matches(':').replace('.', "_");
        Ok(format!("{PATH}/request/{sender}/{token}"))
    }

    async fn responses(&self, path: &str) -> Result<SignalStream<'static>, Refusal> {
        let proxy = Proxy::new(&self.connection, DESTINATION, path.to_owned(), REQUEST)
            .await
            .map_err(|error| failed("the portal request cannot be watched", error))?;
        proxy
            .receive_signal("Response")
            .await
            .map_err(|error| failed("the portal request cannot be watched", error))
    }

    /// Calls `method` with `body` (whose options carry `handle_token`) and
    /// waits for its response, checking `stop` meanwhile; `on_call` runs
    /// once the call is placed. A dialog response other than success is
    /// [`Refusal::Denied`].
    async fn request<B>(
        &self,
        method: &str,
        body: &B,
        token: &str,
        stop: &dyn Fn() -> bool,
        on_call: &mut dyn FnMut(),
        timeout: Option<Duration>,
    ) -> Result<HashMap<String, OwnedValue>, Refusal>
    where
        B: zbus::export::serde::Serialize + zvariant::DynamicType,
    {
        let path = self.request_path(token)?;
        // Subscribed before the call: the response may follow at once.
        let mut responses = self.responses(&path).await?;
        let proxy = self.screen_cast().await?;
        let handle: OwnedObjectPath = within(CALL_TIMEOUT, method, async {
            proxy
                .call(method, body)
                .await
                .map_err(|error| failed(&format!("{method} failed"), error))
        })
        .await?;
        if handle.as_str() != path {
            // An old portal chose another path.
            responses = self.responses(handle.as_str()).await?;
        }
        on_call();
        let started = std::time::Instant::now();
        loop {
            if stop() {
                self.close_request(handle.as_str()).await;
                return Err(Refusal::Stopped);
            }
            if timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
                self.close_request(handle.as_str()).await;
                return Err(Refusal::Failed(format!(
                    "the desktop portal did not answer {method}"
                )));
            }
            let next = future::or(async { Some(responses.next().await) }, async {
                Timer::after(POLL).await;
                None
            })
            .await;
            let Some(message) = next else {
                continue;
            };
            let Some(message) = message else {
                return Err(Refusal::Failed(
                    "the desktop portal connection closed".into(),
                ));
            };
            let (response, results): (u32, HashMap<String, OwnedValue>) = message
                .body()
                .deserialize()
                .map_err(|error| failed(&format!("{method} answered oddly"), error))?;
            return match response {
                0 => Ok(results),
                1 => Err(Refusal::Denied(DENIED_CANCELLED.into())),
                _ => Err(Refusal::Denied(DENIED_REFUSED.into())),
            };
        }
    }

    async fn close_request(&self, path: &str) {
        if let Ok(proxy) = Proxy::new(&self.connection, DESTINATION, path.to_owned(), REQUEST).await
        {
            let _ = proxy.call::<_, _, ()>("Close", &()).await;
        }
    }

    /// Asks the desktop to share what `request` names: creates a session,
    /// selects the sources (with the restore token and `persist_mode` when
    /// the portal keeps grants), and starts it, which may show the sharing
    /// dialog (`awaiting` runs once the dialog may be up), then opens the
    /// `PipeWire` connection of the grant.
    ///
    /// # Errors
    ///
    /// Returns why nothing is shared.
    pub async fn share(
        &mut self,
        request: &ShareRequest,
        capabilities: &Capabilities,
        stop: &dyn Fn() -> bool,
        awaiting: &mut dyn FnMut(),
    ) -> Result<Grant, Refusal> {
        if capabilities.source_types & request.kind.portal_bit() == 0 {
            return Err(Refusal::Unsupported(
                match request.kind {
                    SourceKind::Monitor => super::NO_SCREEN_SHARING,
                    SourceKind::Window => super::NO_WINDOW_SHARING,
                }
                .into(),
            ));
        }
        let mut nothing = || {};

        // CreateSession.
        let token = handle_token("request");
        let session_token = handle_token("session");
        let options: HashMap<&str, Value<'_>> = HashMap::from([
            ("handle_token", Value::from(token.as_str())),
            ("session_handle_token", Value::from(session_token.as_str())),
        ]);
        let results = self
            .request(
                "CreateSession",
                &(options,),
                &token,
                stop,
                &mut nothing,
                Some(CALL_TIMEOUT),
            )
            .await?;
        let session = results
            .get("session_handle")
            .and_then(|value| value.downcast_ref::<&str>().ok().map(str::to_owned))
            .and_then(|path| OwnedObjectPath::try_from(path).ok())
            .ok_or_else(|| Refusal::Failed("CreateSession named no session".into()))?;
        self.watch_session(&session).await?;
        self.session = Some(session.clone());

        // SelectSources.
        let token = handle_token("request");
        let (cursor, cursor_honoured) = cursor_mode(capabilities.cursor_modes, request.cursor);
        let mut options: HashMap<&str, Value<'_>> = HashMap::from([
            ("handle_token", Value::from(token.as_str())),
            ("types", Value::from(request.kind.portal_bit())),
            ("multiple", Value::from(false)),
        ]);
        if let Some(cursor) = cursor {
            options.insert("cursor_mode", Value::from(cursor));
        }
        if capabilities.version >= 4 {
            options.insert("persist_mode", Value::from(PERSIST_UNTIL_REVOKED));
            if let Some(restore) = &request.restore_token {
                options.insert("restore_token", Value::from(restore.as_str()));
            }
        }
        let session_path = ObjectPath::from(&session);
        self.request(
            "SelectSources",
            &(&session_path, options),
            &token,
            stop,
            &mut nothing,
            Some(CALL_TIMEOUT),
        )
        .await?;

        // Start: the sharing dialog waits for the operator.
        let token = handle_token("request");
        let options: HashMap<&str, Value<'_>> =
            HashMap::from([("handle_token", Value::from(token.as_str()))]);
        let results = self
            .request(
                "Start",
                &(&session_path, "", options),
                &token,
                stop,
                awaiting,
                None,
            )
            .await?;
        let node_id = first_stream(&results)
            .ok_or_else(|| Refusal::Failed("the desktop granted no stream".into()))?;
        let restore_token = results
            .get("restore_token")
            .and_then(|value| value.downcast_ref::<&str>().ok().map(str::to_owned));

        // OpenPipeWireRemote.
        let proxy = self.screen_cast().await?;
        let options: HashMap<&str, Value<'_>> = HashMap::new();
        let remote: zvariant::OwnedFd = within(CALL_TIMEOUT, "OpenPipeWireRemote", async {
            proxy
                .call("OpenPipeWireRemote", &(&session_path, options))
                .await
                .map_err(|error| failed("OpenPipeWireRemote failed", error))
        })
        .await?;
        Ok(Grant {
            node_id,
            restore_token,
            remote: OwnedFd::from(remote),
            note: (!cursor_honoured).then_some(super::NO_EMBEDDED_CURSOR),
        })
    }

    async fn watch_session(&mut self, session: &OwnedObjectPath) -> Result<(), Refusal> {
        let proxy = Proxy::new(
            &self.connection,
            DESTINATION,
            session.as_str().to_owned(),
            SESSION,
        )
        .await
        .map_err(|error| failed("the portal session cannot be watched", error))?;
        self.closed = Some(
            proxy
                .receive_signal("Closed")
                .await
                .map_err(|error| failed("the portal session cannot be watched", error))?,
        );
        Ok(())
    }

    /// Whether the desktop closed the session (sharing stopped on the
    /// desktop). Never waits.
    pub fn closed(&mut self) -> bool {
        let Some(closed) = &mut self.closed else {
            return false;
        };
        match future::block_on(future::poll_once(closed.next())) {
            // A `Closed` signal, or the connection is gone.
            Some(_) => {
                self.closed = None;
                self.session = None;
                true
            }
            None => false,
        }
    }

    /// Ends the session, if one is open.
    pub async fn close(&mut self) {
        self.closed = None;
        if let Some(session) = self.session.take()
            && let Ok(proxy) = Proxy::new(
                &self.connection,
                DESTINATION,
                session.as_str().to_owned(),
                SESSION,
            )
            .await
        {
            let _ = within(CALL_TIMEOUT, "Close", async {
                proxy
                    .call::<_, _, ()>("Close", &())
                    .await
                    .map_err(|error| failed("Close failed", error))
            })
            .await;
        }
    }
}

/// The node of the first stream a `Start` response lists (`streams`:
/// `a(ua{sv})`).
fn first_stream(results: &HashMap<String, OwnedValue>) -> Option<u32> {
    let streams: &Value<'_> = results.get("streams")?;
    let streams = match streams {
        Value::Value(inner) => inner,
        other => other,
    };
    let Value::Array(streams) = streams else {
        return None;
    };
    streams.inner().iter().find_map(|stream| match stream {
        Value::Structure(stream) => match stream.fields().first() {
            Some(Value::U32(node)) => Some(*node),
            _ => None,
        },
        _ => None,
    })
}
