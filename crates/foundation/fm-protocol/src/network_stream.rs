//! Network stream inputs (Protocol 2.46, IN-015).
//!
//! A network stream input receives an RTSP, RTP, UDP, TCP, or HTTP(S)
//! MPEG-TS stream, or an HLS playlist, by URL, through a bounded jitter
//! buffer, and reconnects with bounded exponential backoff.
//!
//! | Command | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`crate::CommandPayload::AddInput`] | `input_add` | `input`, `name`, `kind=network_stream`, the config fields | project edit |
//! | [`crate::CommandPayload::SetNetworkStream`] | `input_stream_set` | `input`, optional `url` (absent keeps the URL), the settings fields | project edit |
//!
//! Config fields: `url`, `transport` (`auto|tcp|udp`, RTSP only),
//! `jitter_ms`, `backoff_min_ms`, `backoff_max_ms`, `give_up` (`never` or an
//! attempt count), `timeout_ms`, and `stall` (`freeze|black`).
//!
//! Both are undoable project edits that honour the project and input
//! padlocks; `input_stream_set` is announced by the durable
//! `event=network_stream_changed` (`input` and the config). A daemon applies
//! it live by reconnecting the stream with its new configuration.
//!
//! **Secrets.** A URL may carry credentials or secret query parameters.
//! Commands carry the URL as the operator typed it; everything a server
//! reports (the snapshot's `network_streams`, `network_stream_changed`, and
//! status errors) carries only the redacted URL
//! ([`fm_types::network_stream::redact_stream_url`]).
//!
//! Runtime state is not durable. `runtime_event` with
//! `event=network_stream_status` reports one input's
//! [`NetworkStreamStatus`] whenever its state changes and about once a
//! second while it receives. A snapshot carries `network_streams`: every
//! network stream input, ascending, with its redacted config and its latest
//! status (without the error text).

pub use fm_types::network_stream::{
    GiveUp, MAX_NETWORK_STREAM_INPUTS, MAX_STREAM_ERROR_BYTES, MAX_STREAM_URL_BYTES,
    NetworkStreamConfig, NetworkStreamError, NetworkStreamSettings, NetworkStreamState,
    NetworkStreamStatus, ReconnectPolicy, RtspTransport, StallSlate, StreamProtocol, StreamUrl,
    redact_stream_url, validate_stream_url,
};

use crate::WireInputId;

/// One network stream input in a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkStreamEntry {
    pub input: WireInputId,
    /// The configuration with its URL redacted.
    pub config: NetworkStreamConfig,
    /// The latest status; a snapshot carries no error text.
    pub status: NetworkStreamStatus,
}
