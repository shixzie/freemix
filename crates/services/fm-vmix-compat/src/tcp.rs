//! Transport-independent vMix TCP API session state machine.
//!
//! A transport feeds one decoded line at a time to [`TcpSession::handle_line`]
//! (or [`TcpHub::handle_line`]) and writes the returned bytes verbatim. No
//! sockets are opened here. All responses are single CRLF-terminated lines
//! except `XML`, which is `XML <len>\r\n` followed by exactly `<len>` UTF-8
//! bytes of payload with no trailing terminator.
//!
//! `FUNCTION` lines are translated and handed back as
//! [`TcpReply::Dispatch`]; the caller executes the translation and then calls
//! [`TcpSession::complete_function`] to obtain the `FUNCTION OK`/`FUNCTION ER`
//! line. While a function is pending, the session refuses further lines so
//! responses stay in request order.

use std::{error::Error, fmt, fmt::Write as _};

use super::{CompatState, MAX_TCP_LINE_BYTES, OVERLAY_CHANNELS, Translation, translate_function};

/// Maximum number of concurrently open sessions in one [`TcpHub`].
pub const MAX_TCP_SESSIONS: usize = 64;
/// Maximum byte length of an operator-facing error message in a response.
pub const MAX_TCP_MESSAGE_BYTES: usize = 256;

/// One parsed TCP API request line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TcpRequest {
    /// `FUNCTION <name> [query]`.
    Function {
        /// vMix function name.
        function: String,
        /// Raw form-encoded query, possibly empty.
        query: String,
    },
    /// `TALLY`.
    Tally,
    /// `XML`.
    Xml,
    /// `XMLTEXT <xpath>`.
    XmlText {
        /// `XPath` subset expression; see [`CompatState::xml_text`].
        xpath: String,
    },
    /// `SUBSCRIBE <topic>`.
    Subscribe(SubscriptionTopic),
    /// `UNSUBSCRIBE <topic>`.
    Unsubscribe(SubscriptionTopic),
    /// `ACTS <activator> [input]`.
    Acts {
        /// Case-sensitive activator name, such as `Input` or `Recording`.
        activator: String,
        /// Optional input reference, resolved like HTTP `Input`.
        input: Option<String>,
    },
    /// `VERSION`.
    Version,
    /// `QUIT`.
    Quit,
}

/// Event streams a session may subscribe to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubscriptionTopic {
    /// `TALLY OK ...` lines whenever the tally string changes.
    Tally,
    /// `ACTS OK ...` lines whenever an activator value changes.
    Acts,
}

impl SubscriptionTopic {
    /// Canonical wire keyword.
    #[must_use]
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Tally => "TALLY",
            Self::Acts => "ACTS",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("TALLY") {
            Some(Self::Tally)
        } else if value.eq_ignore_ascii_case("ACTS") {
            Some(Self::Acts)
        } else {
            None
        }
    }
}

/// A line that cannot be parsed into a [`TcpRequest`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TcpRequestError {
    /// The line exceeded [`MAX_TCP_LINE_BYTES`].
    LineTooLong,
    /// The line contained an embedded CR or LF.
    EmbeddedLineEnding,
    /// The command keyword is not part of the TCP API.
    UnknownCommand,
    /// A known command had missing or extra arguments.
    InvalidArguments {
        /// Canonical command keyword.
        command: &'static str,
        /// Stable reason.
        reason: &'static str,
    },
}

impl fmt::Display for TcpRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineTooLong => formatter.write_str("Line too long"),
            Self::EmbeddedLineEnding => formatter.write_str("Embedded line ending"),
            Self::UnknownCommand => formatter.write_str("Unknown command"),
            Self::InvalidArguments { reason, .. } => formatter.write_str(reason),
        }
    }
}

impl Error for TcpRequestError {}

impl TcpRequestError {
    /// Renders the CRLF-terminated error response for this parse failure.
    #[must_use]
    pub fn response(&self) -> String {
        match self {
            Self::InvalidArguments { command, reason } => error_line(command, reason),
            other => {
                let mut output = String::from("ERROR ");
                push_single_line(&mut output, &other.to_string(), MAX_TCP_MESSAGE_BYTES);
                output.push_str("\r\n");
                output
            }
        }
    }
}

/// Parses one TCP API line. Command keywords match ASCII case-insensitively;
/// one trailing CRLF is accepted.
///
/// # Errors
///
/// Returns [`TcpRequestError`] for over-limit, multi-line, unknown, or
/// incorrectly argued commands.
pub fn parse_tcp_request(line: &str) -> Result<TcpRequest, TcpRequestError> {
    let line = line.strip_suffix("\r\n").unwrap_or(line);
    if line.len() > MAX_TCP_LINE_BYTES {
        return Err(TcpRequestError::LineTooLong);
    }
    if line.contains(['\r', '\n']) {
        return Err(TcpRequestError::EmbeddedLineEnding);
    }
    let (keyword, rest) = line.split_once(' ').unwrap_or((line, ""));
    let invalid = |command, reason| TcpRequestError::InvalidArguments { command, reason };
    let no_arguments = |command, request| {
        if rest.is_empty() {
            Ok(request)
        } else {
            Err(invalid(command, "Command takes no arguments"))
        }
    };
    let keyword = keyword.to_ascii_uppercase();
    match keyword.as_str() {
        "FUNCTION" => {
            let (function, query) = rest.split_once(' ').unwrap_or((rest, ""));
            if function.is_empty() {
                return Err(invalid("FUNCTION", "Missing function name"));
            }
            Ok(TcpRequest::Function {
                function: function.to_owned(),
                query: query.to_owned(),
            })
        }
        "TALLY" => no_arguments("TALLY", TcpRequest::Tally),
        "XML" => no_arguments("XML", TcpRequest::Xml),
        "VERSION" => no_arguments("VERSION", TcpRequest::Version),
        "QUIT" => no_arguments("QUIT", TcpRequest::Quit),
        "XMLTEXT" => {
            if rest.is_empty() {
                Err(invalid("XMLTEXT", "Missing XPath"))
            } else {
                Ok(TcpRequest::XmlText {
                    xpath: rest.to_owned(),
                })
            }
        }
        "SUBSCRIBE" | "UNSUBSCRIBE" => {
            let command = if keyword == "SUBSCRIBE" {
                "SUBSCRIBE"
            } else {
                "UNSUBSCRIBE"
            };
            let topic =
                SubscriptionTopic::parse(rest).ok_or(invalid(command, "Unknown subscription"))?;
            Ok(if command == "SUBSCRIBE" {
                TcpRequest::Subscribe(topic)
            } else {
                TcpRequest::Unsubscribe(topic)
            })
        }
        "ACTS" => {
            let (activator, input) = match rest.split_once(' ') {
                Some((activator, input)) => (activator, Some(input.to_owned())),
                None => (rest, None),
            };
            if activator.is_empty() {
                return Err(invalid("ACTS", "Missing activator"));
            }
            Ok(TcpRequest::Acts {
                activator: activator.to_owned(),
                input,
            })
        }
        _ => Err(TcpRequestError::UnknownCommand),
    }
}

/// Session-level misuse by the transport, distinct from client protocol errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TcpSessionError {
    /// The session already processed `QUIT` or was closed.
    Closed,
    /// A dispatched `FUNCTION` has not been completed.
    FunctionPending,
    /// [`TcpSession::complete_function`] was called with nothing pending.
    NoPendingFunction,
    /// The hub already holds [`MAX_TCP_SESSIONS`] sessions.
    TooManySessions,
    /// The hub has no session with the supplied ID.
    UnknownSession,
}

impl fmt::Display for TcpSessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Closed => "TCP session is closed",
            Self::FunctionPending => "a FUNCTION response is still pending",
            Self::NoPendingFunction => "no FUNCTION is pending",
            Self::TooManySessions => "TCP session limit reached",
            Self::UnknownSession => "unknown TCP session",
        })
    }
}

impl Error for TcpSessionError {}

/// What the transport must do after one request line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TcpReply {
    /// Write `payload` verbatim; close the connection afterwards if `close`.
    Respond {
        /// Exact response bytes.
        payload: String,
        /// `true` only after `QUIT`.
        close: bool,
    },
    /// Execute the translation, then call [`TcpSession::complete_function`].
    /// Only [`Translation::Protocol`] and [`Translation::Compat`] are dispatched;
    /// unsupported functions are answered immediately with `FUNCTION ER`.
    Dispatch(Translation),
}

impl TcpReply {
    fn respond(payload: String) -> Self {
        Self::Respond {
            payload,
            close: false,
        }
    }
}

/// Activator values for one state, indexed by zero-based input ordinal.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ActsSnapshot {
    /// Bit 0 Program, bit 1 Preview, bits 2..10 overlay channels 1..8.
    inputs: Vec<u16>,
    recording: bool,
    streaming: bool,
}

const INPUT_ACTIVATORS: [&str; 2 + OVERLAY_CHANNELS] = [
    "Input",
    "InputPreview",
    "Overlay1",
    "Overlay2",
    "Overlay3",
    "Overlay4",
    "Overlay5",
    "Overlay6",
    "Overlay7",
    "Overlay8",
];

impl ActsSnapshot {
    fn capture(state: &CompatState) -> Self {
        Self {
            inputs: state
                .inputs
                .iter()
                .map(|input| {
                    let mut bits = u16::from(input.id == state.active)
                        | (u16::from(input.id == state.preview) << 1);
                    for (channel, overlay) in state.overlays.iter().enumerate() {
                        if *overlay == Some(input.id) {
                            bits |= 1 << (channel + 2);
                        }
                    }
                    bits
                })
                .collect(),
            recording: state.recording,
            streaming: state.streaming,
        }
    }

    /// Emits deterministic change lines: per input activator in fixed order,
    /// deactivations (ascending ordinal) before activations, then Recording
    /// and Streaming.
    fn diff(&self, next: &Self, output: &mut Vec<String>) {
        let width = self.inputs.len().max(next.inputs.len());
        for (bit, activator) in INPUT_ACTIVATORS.iter().enumerate() {
            let value = |snapshot: &Self, index: usize| {
                snapshot
                    .inputs
                    .get(index)
                    .is_some_and(|bits| bits & (1 << bit) != 0)
            };
            for target in [false, true] {
                for index in 0..width {
                    let (before, after) = (value(self, index), value(next, index));
                    if before != after && after == target {
                        output.push(format!(
                            "ACTS OK {activator} {} {}\r\n",
                            index + 1,
                            u8::from(after)
                        ));
                    }
                }
            }
        }
        if self.recording != next.recording {
            output.push(format!(
                "ACTS OK Recording {}\r\n",
                u8::from(next.recording)
            ));
        }
        if self.streaming != next.streaming {
            output.push(format!(
                "ACTS OK Streaming {}\r\n",
                u8::from(next.streaming)
            ));
        }
    }
}

/// Per-connection TCP API state machine.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TcpSession {
    tally_baseline: Option<String>,
    acts_baseline: Option<ActsSnapshot>,
    function_pending: bool,
    closed: bool,
}

impl TcpSession {
    /// Creates an open session without subscriptions.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns whether the session is subscribed to `topic`.
    #[must_use]
    pub const fn is_subscribed(&self, topic: SubscriptionTopic) -> bool {
        match topic {
            SubscriptionTopic::Tally => self.tally_baseline.is_some(),
            SubscriptionTopic::Acts => self.acts_baseline.is_some(),
        }
    }

    /// Returns whether a dispatched `FUNCTION` awaits completion.
    #[must_use]
    pub const fn function_pending(&self) -> bool {
        self.function_pending
    }

    /// Returns whether `QUIT` was processed.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        self.closed
    }

    /// Handles one request line against the current state.
    ///
    /// Client protocol errors are answered in-band (`<COMMAND> ER <message>`
    /// or `ERROR <message>`); only transport misuse returns `Err`.
    ///
    /// # Errors
    ///
    /// Returns [`TcpSessionError::Closed`] after `QUIT` and
    /// [`TcpSessionError::FunctionPending`] while a function is outstanding.
    pub fn handle_line(
        &mut self,
        line: &str,
        state: &CompatState,
    ) -> Result<TcpReply, TcpSessionError> {
        if self.closed {
            return Err(TcpSessionError::Closed);
        }
        if self.function_pending {
            return Err(TcpSessionError::FunctionPending);
        }
        let request = match parse_tcp_request(line) {
            Ok(request) => request,
            Err(error) => return Ok(TcpReply::respond(error.response())),
        };
        Ok(match request {
            TcpRequest::Function { function, query } => {
                match translate_function(&function, &query, state) {
                    Ok(Translation::Unsupported(report)) => {
                        TcpReply::respond(error_line("FUNCTION", report.reason))
                    }
                    Ok(translation) => {
                        self.function_pending = true;
                        TcpReply::Dispatch(translation)
                    }
                    Err(error) => TcpReply::respond(error_line("FUNCTION", &error.to_string())),
                }
            }
            TcpRequest::Tally => TcpReply::respond(state.tally_response()),
            TcpRequest::Xml => TcpReply::respond(xml_response(state)),
            TcpRequest::XmlText { xpath } => TcpReply::respond(match state.xml_text(&xpath) {
                Ok(value) => ok_line("XMLTEXT", &value),
                Err(error) => error_line("XMLTEXT", &error.to_string()),
            }),
            TcpRequest::Subscribe(topic) => {
                match topic {
                    SubscriptionTopic::Tally => {
                        if self.tally_baseline.is_none() {
                            self.tally_baseline = Some(state.tally());
                        }
                    }
                    SubscriptionTopic::Acts => {
                        if self.acts_baseline.is_none() {
                            self.acts_baseline = Some(ActsSnapshot::capture(state));
                        }
                    }
                }
                TcpReply::respond(format!("SUBSCRIBE OK {}\r\n", topic.keyword()))
            }
            TcpRequest::Unsubscribe(topic) => {
                match topic {
                    SubscriptionTopic::Tally => self.tally_baseline = None,
                    SubscriptionTopic::Acts => self.acts_baseline = None,
                }
                TcpReply::respond(format!("UNSUBSCRIBE OK {}\r\n", topic.keyword()))
            }
            TcpRequest::Acts { activator, input } => {
                TcpReply::respond(acts_response(state, &activator, input.as_deref()))
            }
            TcpRequest::Version => TcpReply::respond(ok_line("VERSION", &state.version)),
            TcpRequest::Quit => {
                self.closed = true;
                self.tally_baseline = None;
                self.acts_baseline = None;
                TcpReply::Respond {
                    payload: "QUIT OK Bye\r\n".to_owned(),
                    close: true,
                }
            }
        })
    }

    /// Completes the pending `FUNCTION`: `Ok(())` renders
    /// `FUNCTION OK Completed`, `Err(message)` renders `FUNCTION ER <message>`.
    ///
    /// # Errors
    ///
    /// Returns [`TcpSessionError::NoPendingFunction`] when nothing was dispatched.
    pub fn complete_function(
        &mut self,
        result: Result<(), &str>,
    ) -> Result<String, TcpSessionError> {
        if !self.function_pending {
            return Err(TcpSessionError::NoPendingFunction);
        }
        self.function_pending = false;
        Ok(match result {
            Ok(()) => "FUNCTION OK Completed\r\n".to_owned(),
            Err(message) => error_line("FUNCTION", message),
        })
    }

    /// Returns subscription event lines for changes since the previous call
    /// (or since subscribing) and advances the baselines. `TALLY` precedes
    /// `ACTS` lines.
    pub fn events(&mut self, state: &CompatState) -> Vec<String> {
        let mut output = Vec::new();
        if let Some(baseline) = &mut self.tally_baseline {
            let tally = state.tally();
            if *baseline != tally {
                output.push(format!("TALLY OK {tally}\r\n"));
                *baseline = tally;
            }
        }
        if let Some(baseline) = &mut self.acts_baseline {
            let next = ActsSnapshot::capture(state);
            baseline.diff(&next, &mut output);
            *baseline = next;
        }
        output
    }
}

/// Opaque identifier for a session in a [`TcpHub`]. Never reused by one hub.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TcpSessionId(u64);

impl TcpSessionId {
    /// Raw monotonically increasing value, starting at 1.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// One subscription event addressed to a session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TcpEvent {
    /// Destination session.
    pub session: TcpSessionId,
    /// Exact CRLF-terminated line.
    pub payload: String,
}

/// Bounded set of TCP sessions with deterministic event fan-out.
#[derive(Clone, Debug, Default)]
pub struct TcpHub {
    sessions: Vec<(TcpSessionId, TcpSession)>,
    last_id: u64,
}

impl TcpHub {
    /// Creates an empty hub.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of open sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    /// Returns whether no sessions are open.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    /// Returns a session for inspection.
    #[must_use]
    pub fn session(&self, id: TcpSessionId) -> Option<&TcpSession> {
        self.sessions
            .iter()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, session)| session)
    }

    /// Opens a session.
    ///
    /// # Errors
    ///
    /// Returns [`TcpSessionError::TooManySessions`] at [`MAX_TCP_SESSIONS`].
    pub fn open(&mut self) -> Result<TcpSessionId, TcpSessionError> {
        if self.sessions.len() >= MAX_TCP_SESSIONS {
            return Err(TcpSessionError::TooManySessions);
        }
        self.last_id += 1;
        let id = TcpSessionId(self.last_id);
        self.sessions.push((id, TcpSession::new()));
        Ok(id)
    }

    /// Removes a session, returning whether it existed.
    pub fn close(&mut self, id: TcpSessionId) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|(candidate, _)| *candidate != id);
        before != self.sessions.len()
    }

    /// Handles one line for a session. A `QUIT` removes the session.
    ///
    /// # Errors
    ///
    /// Returns [`TcpSessionError::UnknownSession`] or any error from
    /// [`TcpSession::handle_line`].
    pub fn handle_line(
        &mut self,
        id: TcpSessionId,
        line: &str,
        state: &CompatState,
    ) -> Result<TcpReply, TcpSessionError> {
        let session = self.session_mut(id)?;
        let reply = session.handle_line(line, state)?;
        if matches!(reply, TcpReply::Respond { close: true, .. }) {
            self.close(id);
        }
        Ok(reply)
    }

    /// Completes a session's pending `FUNCTION`.
    ///
    /// # Errors
    ///
    /// Returns [`TcpSessionError::UnknownSession`] or
    /// [`TcpSessionError::NoPendingFunction`].
    pub fn complete_function(
        &mut self,
        id: TcpSessionId,
        result: Result<(), &str>,
    ) -> Result<String, TcpSessionError> {
        self.session_mut(id)?.complete_function(result)
    }

    /// Collects subscription events for every session, in ascending session
    /// ID order and per-session event order.
    pub fn publish(&mut self, state: &CompatState) -> Vec<TcpEvent> {
        let mut events = Vec::new();
        for (id, session) in &mut self.sessions {
            events.extend(session.events(state).into_iter().map(|payload| TcpEvent {
                session: *id,
                payload,
            }));
        }
        events
    }

    fn session_mut(&mut self, id: TcpSessionId) -> Result<&mut TcpSession, TcpSessionError> {
        self.sessions
            .iter_mut()
            .find(|(candidate, _)| *candidate == id)
            .map(|(_, session)| session)
            .ok_or(TcpSessionError::UnknownSession)
    }
}

/// Renders `XML <len>\r\n<payload>` where `len` is the payload's UTF-8 byte length.
#[must_use]
pub fn xml_response(state: &CompatState) -> String {
    let xml = state.xml();
    let mut output = String::with_capacity(xml.len() + 16);
    write!(output, "XML {}\r\n", xml.len()).expect("writing to a String cannot fail");
    output.push_str(&xml);
    output
}

fn acts_response(state: &CompatState, activator: &str, input: Option<&str>) -> String {
    let channel = INPUT_ACTIVATORS
        .iter()
        .position(|candidate| *candidate == activator);
    match (channel, input) {
        (Some(bit), Some(reference)) => match state.resolve_input(reference) {
            Ok(id) => {
                let index = state
                    .ordinal(id)
                    .expect("resolved inputs are present in state")
                    - 1;
                let bits = ActsSnapshot::capture(state).inputs[index];
                format!(
                    "ACTS OK {activator} {} {}\r\n",
                    index + 1,
                    u8::from(bits & (1 << bit) != 0)
                )
            }
            Err(error) => error_line("ACTS", &error.to_string()),
        },
        (Some(_), None) => error_line("ACTS", "Missing input"),
        (None, None) if activator == "Recording" => {
            format!("ACTS OK Recording {}\r\n", u8::from(state.recording))
        }
        (None, None) if activator == "Streaming" => {
            format!("ACTS OK Streaming {}\r\n", u8::from(state.streaming))
        }
        (None, Some(_)) if matches!(activator, "Recording" | "Streaming") => {
            error_line("ACTS", "Activator takes no input")
        }
        _ => error_line("ACTS", "Unknown activator"),
    }
}

fn ok_line(command: &str, value: &str) -> String {
    let mut output = format!("{command} OK ");
    push_single_line(&mut output, value, usize::MAX);
    output.push_str("\r\n");
    output
}

fn error_line(command: &str, message: &str) -> String {
    let mut output = format!("{command} ER ");
    push_single_line(&mut output, message, MAX_TCP_MESSAGE_BYTES);
    output.push_str("\r\n");
    output
}

/// Appends `value` with CR/LF replaced by spaces, truncated on a character
/// boundary to at most `limit` bytes.
fn push_single_line(output: &mut String, value: &str, limit: usize) {
    let mut written = 0;
    for character in value.chars() {
        let character = if matches!(character, '\r' | '\n') {
            ' '
        } else {
            character
        };
        written += character.len_utf8();
        if written > limit {
            break;
        }
        output.push(character);
    }
}
