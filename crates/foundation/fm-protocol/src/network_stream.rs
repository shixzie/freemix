//! Network stream inputs (Protocol 2.46, IN-015; Protocol 2.49, IN-016 and
//! IN-017).
//!
//! A network stream input receives an RTSP, RTP, UDP, TCP, or HTTP(S)
//! MPEG-TS stream, an HLS playlist, an RTMP(S) stream, or an SRT stream, by
//! URL, through a bounded jitter buffer, and reconnects with bounded
//! exponential backoff. RTMP and SRT inputs call out or listen for a
//! publisher (`mode`); SRT also meets in rendezvous and carries a
//! passphrase, key length, stream ID, and latency. Any input may hear one
//! stereo pair of a multichannel source (`audio`). Two inputs never listen
//! on the same local port: the second is refused with the field issue
//! `url`/`listen_port_in_use`.
//!
//! | Command | Payload | Fields | Class |
//! |---|---|---|---|
//! | [`crate::CommandPayload::AddInput`] | `input_add` | `input`, `name`, `kind=network_stream`, the config fields | project edit |
//! | [`crate::CommandPayload::SetNetworkStream`] | `input_stream_set` | `input`, optional `url` (absent keeps the URL), the settings fields | project edit |
//!
//! Config fields: `url`, `transport` (`auto|tcp|udp`, RTSP only),
//! `jitter_ms`, `backoff_min_ms`, `backoff_max_ms`, `give_up` (`never` or an
//! attempt count), `timeout_ms`, `stall` (`freeze|black`), `mode`
//! (`caller|listener|rendezvous`), `latency_ms` (SRT, 20..=8000),
//! `key_length` (`16|24|32`, SRT), `audio` (`mix|1-2|3-4|5-6|7-8`), and the
//! optional `stream_id` (SRT), `stream_key` (RTMP, secret), and `passphrase`
//! (SRT, secret). `input_stream_set` keeps a secret it does not carry,
//! clears one it carries empty, and replaces one it carries with a value.
//!
//! Both are undoable project edits that honour the project and input
//! padlocks; `input_stream_set` is announced by the durable
//! `event=network_stream_changed` (`input` and the config). A daemon applies
//! it live by reconnecting the stream with its new configuration.
//!
//! **Secrets.** A URL may carry credentials, secret query parameters, or an
//! RTMP stream key in its path; the stream key and passphrase fields are
//! secrets too. Commands carry them as the operator typed them; everything
//! a server reports (the snapshot's `network_streams`,
//! `network_stream_changed`, `input_added`, and status errors) carries only
//! the redacted URL ([`fm_types::network_stream::redact_stream_url`]) and
//! `***` (or, in a snapshot, a set flag) for each secret field.
//!
//! Runtime state is not durable. `runtime_event` with
//! `event=network_stream_status` reports one input's
//! [`NetworkStreamStatus`] whenever its state changes and about once a
//! second while it receives. A snapshot carries `network_streams`: every
//! network stream input, ascending, with its redacted config and its latest
//! status (without the error text).

pub use fm_types::network_stream::{
    AudioChannels, GiveUp, ListenEndpoint, ListenTransport, MAX_NETWORK_STREAM_INPUTS,
    MAX_STREAM_ERROR_BYTES, MAX_STREAM_URL_BYTES, NetworkStreamConfig, NetworkStreamError,
    NetworkStreamSettings, NetworkStreamState, NetworkStreamStatus, ReconnectPolicy, RtspTransport,
    SecretEdit, SrtKeyLength, StallSlate, StreamMode, StreamProtocol, StreamUrl, redact_stream_url,
    validate_stream_url,
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
