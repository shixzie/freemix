//! Stream destinations in the console (OR-006): the Recording & streams
//! panel's group keys and per-row Retry / Edit / Remove keys, and the Add /
//! Edit destination dialog with its Remove confirmation.
//!
//! A destination's stream key is a secret. The replicated view carries only
//! whether one is stored; the dialog types a new key into a masked field
//! (with the same eye key as the Add input dialog's secrets) and sends it as
//! a [`SecretEdit`]: leaving the field empty while editing keeps the stored
//! key. No tooltip, label, or status line ever shows a key: destinations are
//! shown by [`fm_protocol::StreamTargetConfig::display_url`], which has none.
//! An SRT destination's optional encryption passphrase (Protocol 2.54) is a
//! secret in exactly the same way, typed into its own masked field.
//!
//! The dialog validates as the operator types, with the bounds the daemon
//! applies, so its Add / Save key is only enabled for a destination the
//! daemon accepts; a refusal the console cannot foresee (a name taken by an
//! edit that has not arrived yet) comes back as a refusal toast.

use core::fmt;
use core::num::NonZeroU128;

use egui::{Button, DragValue, RichText, Sense, Stroke, Ui, Vec2};
use fm_protocol::{
    DEFAULT_STREAM_TARGET_SRT_LATENCY_MS, MAX_STREAM_RECONNECT_ATTEMPTS,
    MAX_STREAM_RECONNECT_DELAY_MS, MAX_STREAM_TARGET_BITRATE_KBPS,
    MAX_STREAM_TARGET_ENDPOINT_BYTES, MAX_STREAM_TARGET_KEY_BYTES, MAX_STREAM_TARGET_NAME_BYTES,
    MAX_STREAM_TARGET_PASSPHRASE_BYTES, MAX_STREAM_TARGET_SRT_LATENCY_MS, MAX_STREAM_TARGETS,
    MIN_STREAM_RECONNECT_DELAY_MS, MIN_STREAM_TARGET_BITRATE_KBPS,
    MIN_STREAM_TARGET_SRT_LATENCY_MS, SecretEdit, StreamReconnect, StreamTargetConfig,
    StreamTargetEntry, StreamTargetProtocol, WireCueAction, WireOutputId, WireStreamTargetId,
    srt_passphrase_is_valid, stream_key_is_valid,
};

use crate::edit_history::stinger_edit_lock_reason;
use crate::gating::{Capability, Hint, unavailable_reason};
use crate::input_catalogue::{anchored_modal, capitalized, kind_key};
use crate::l10n::{self, Arg, Locale, Msg, with_key};
use crate::network_stream_ui::{
    FieldProblem, field_error, hint_slot, mark_invalid, reveal_key, section_heading, sub_label,
};
use crate::stream_control::{
    StreamAction, StreamGroupAction, group_unavailable_reason, retry_unavailable_reason,
    stream_action, stream_command_pending, stream_group_action,
};
use crate::theme::{
    ACCENT_FILL, AMBER, KeyIcon, PROGRAM, PROGRAM_FILL, Tone, accent_color, add_icon_button,
    caption, paint_lamp, small_button, text_color, text_dim_color,
};
use crate::{StudioIntent, StudioShell, StudioUiState};

/// Default video bitrate of a new destination, in kilobits per second.
const DEFAULT_BITRATE_KBPS: u32 = 6_000;
/// Shortest stream key the daemon accepts (`fm_model::MIN_STREAM_KEY_BYTES`,
/// which this UI crate does not depend on).
const MIN_STREAM_KEY_BYTES: usize = 4;
/// The redaction marker, which the codec refuses as a key.
const REDACTED_KEY: &str = "***";
/// Width of the Add / Edit destination dialog.
const DIALOG_WIDTH: f32 = 480.0;

// ---------------------------------------------------------------------------
// Gating
// ---------------------------------------------------------------------------

/// Why project edits of destinations are unavailable this frame: a session
/// gate (connection, synchronized state, the project-edit permission), then
/// the project padlock.
fn edit_session_reason(state: &StudioUiState) -> Option<&'static str> {
    if state.connection_status.controls_enabled() && state.view.is_some() && state.can_edit_project
    {
        stinger_edit_lock_reason(state)
    } else {
        Some(unavailable_reason(state, Capability::EditProject))
    }
}

/// Every destination id the console knows: the roster, the configured
/// entries, and targets of commands still in flight (a new destination's
/// add is pending before it is in the roster).
fn known_targets(state: &StudioUiState) -> Vec<NonZeroU128> {
    let mut ids: Vec<NonZeroU128> = state.streams.iter().map(|stream| stream.target).collect();
    if let Some(view) = &state.view {
        ids.extend(view.stream_targets.iter().map(|entry| entry.target.get()));
    }
    ids.extend(state.stream_pending.iter().map(|target| target.get()));
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Why Add destination is unavailable, if it is.
pub(crate) fn add_block(state: &StudioUiState) -> Option<&'static str> {
    if let Some(reason) = edit_session_reason(state) {
        return Some(reason);
    }
    if state
        .view
        .as_ref()
        .is_some_and(|view| view.outputs.is_empty())
    {
        return Some(Msg::ReasonStreamNoOutputs.en());
    }
    (known_targets(state).len() >= MAX_STREAM_TARGETS).then_some(Msg::ReasonStreamTargetsFull.en())
}

/// Why destination `target` cannot be edited, if it cannot: a session gate,
/// the padlock, an unresolved command for it, settings not replicated yet,
/// or the destination is desired running (the daemon refuses a running
/// destination's edit with `stream_running`).
pub(crate) fn edit_block(
    state: &StudioUiState,
    target: WireStreamTargetId,
) -> Option<&'static str> {
    if let Some(reason) = edit_session_reason(state) {
        return Some(reason);
    }
    if stream_command_pending(state, target) {
        return Some(Msg::ReasonStreamPending.en());
    }
    let configured = state
        .view
        .as_ref()
        .and_then(|view| view.stream_target(target.get()))
        .is_some();
    if !configured {
        return Some(Msg::ReasonStreamNoConfig.en());
    }
    state
        .streams
        .iter()
        .any(|stream| stream.target == target.get() && stream.desired_running)
        .then_some(Msg::ReasonStreamRunning.en())
}

/// Whether a cue list starts or stops destination `target`.
fn used_by_cue(state: &StudioUiState, target: WireStreamTargetId) -> bool {
    state.project_state.as_ref().is_some_and(|project| {
        project.cue_lists().iter().any(|list| {
            list.spec.cues.iter().any(|cue| {
                cue.actions.iter().any(|action| {
                    matches!(action, WireCueAction::Stream { target: used, .. } if *used == target)
                })
            })
        })
    })
}

/// Why destination `target` cannot be removed, if it cannot: as for an
/// edit, and also while a cue list names it (`stream_in_use`).
pub(crate) fn remove_block(
    state: &StudioUiState,
    target: WireStreamTargetId,
) -> Option<&'static str> {
    if let Some(reason) = edit_block(state, target) {
        return Some(reason);
    }
    used_by_cue(state, target).then_some(Msg::ReasonStreamInCue.en())
}

/// The id a new destination takes: one past the highest id the console
/// knows, so it never collides with a destination or an add in flight.
pub(crate) fn next_target_id(state: &StudioUiState) -> Option<WireStreamTargetId> {
    let next = known_targets(state)
        .last()
        .map_or(Some(1), |highest| highest.get().checked_add(1))?;
    NonZeroU128::new(next).map(WireStreamTargetId::new)
}

// ---------------------------------------------------------------------------
// The dialog's draft and its validation
// ---------------------------------------------------------------------------

/// A field of the destination form, so a problem shows under it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TargetField {
    Name,
    Endpoint,
    Backup,
    Key,
    Output,
    Bitrate,
    /// The first and longest reconnect waits, checked together.
    Delays,
    Attempts,
    /// The SRT send latency.
    Latency,
    /// The SRT encryption passphrase.
    Passphrase,
}

impl TargetField {
    /// The field's label, as the form shows it.
    pub(crate) const fn label(self, protocol: StreamTargetProtocol) -> Msg {
        match self {
            Self::Name => Msg::AddInputName,
            Self::Endpoint if matches!(protocol, StreamTargetProtocol::Hls) => {
                Msg::StreamFieldDirectory
            }
            Self::Endpoint => Msg::StreamFieldEndpoint,
            Self::Backup => Msg::StreamFieldBackup,
            Self::Key => Msg::NsStreamKey,
            Self::Output => Msg::MenuOutput,
            Self::Bitrate => Msg::StreamFieldBitrate,
            Self::Delays => Msg::StreamFieldMaxDelay,
            Self::Attempts => Msg::StreamFieldAttempts,
            Self::Latency => Msg::NsLatency,
            Self::Passphrase => Msg::NsPassphrase,
        }
    }
}

/// The Add / Edit destination form. `Debug` never prints the typed key.
#[allow(clippy::struct_excessive_bools)] // each bool is one form toggle
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct TargetDraft {
    pub(crate) target: WireStreamTargetId,
    /// The configuration being edited; `None` while adding.
    pub(crate) original: Option<StreamTargetConfig>,
    /// Whether the edited destination has a stored key.
    pub(crate) stored_key: bool,
    pub(crate) name: String,
    pub(crate) protocol: StreamTargetProtocol,
    pub(crate) endpoint: String,
    pub(crate) backup: String,
    pub(crate) output: Option<WireOutputId>,
    pub(crate) reconcile_on_startup: bool,
    pub(crate) bitrate_kbps: u32,
    pub(crate) initial_delay_ms: u32,
    pub(crate) max_delay_ms: u32,
    pub(crate) attempts: u32,
    pub(crate) unlimited: bool,
    /// A newly typed stream key; empty keeps a stored one when editing.
    pub(crate) key: String,
    /// Whether the key field shows what was typed.
    pub(crate) reveal_key: bool,
    /// The SRT send latency, in milliseconds.
    pub(crate) srt_latency_ms: u32,
    /// Whether the edited SRT destination has a stored passphrase.
    pub(crate) stored_passphrase: bool,
    /// Whether the SRT destination is encrypted.
    pub(crate) encrypt: bool,
    /// A newly typed SRT passphrase; empty keeps a stored one.
    pub(crate) passphrase: String,
    /// Whether the passphrase field shows what was typed.
    pub(crate) reveal_passphrase: bool,
}

impl fmt::Debug for TargetDraft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TargetDraft")
            .field("target", &self.target)
            .field("name", &self.name)
            .field("protocol", &self.protocol)
            .field("endpoint", &self.endpoint)
            .field("stored_key", &self.stored_key)
            .field("key", &if self.key.is_empty() { "" } else { "***" })
            .field("stored_passphrase", &self.stored_passphrase)
            .field(
                "passphrase",
                &if self.passphrase.is_empty() {
                    ""
                } else {
                    "***"
                },
            )
            .finish_non_exhaustive()
    }
}

impl TargetDraft {
    /// A fresh destination with id `target`, publishing `output`.
    pub(crate) fn new(
        l: Locale,
        target: WireStreamTargetId,
        output: Option<WireOutputId>,
        position: usize,
    ) -> Self {
        let reconnect = StreamReconnect::DEFAULT;
        Self {
            target,
            original: None,
            stored_key: false,
            name: format!("{} {position}", l.text(Msg::StreamSectionDestination)),
            protocol: StreamTargetProtocol::Rtmp,
            endpoint: String::new(),
            backup: String::new(),
            output,
            reconcile_on_startup: false,
            bitrate_kbps: DEFAULT_BITRATE_KBPS,
            initial_delay_ms: reconnect.initial_delay_ms,
            max_delay_ms: reconnect.max_delay_ms,
            attempts: reconnect.max_attempts.unwrap_or(40),
            unlimited: reconnect.max_attempts.is_none(),
            key: String::new(),
            reveal_key: false,
            srt_latency_ms: DEFAULT_STREAM_TARGET_SRT_LATENCY_MS,
            stored_passphrase: false,
            encrypt: false,
            passphrase: String::new(),
            reveal_passphrase: false,
        }
    }

    /// The form for editing one configured destination.
    pub(crate) fn edit(entry: &StreamTargetEntry) -> Self {
        let config = &entry.config;
        Self {
            target: entry.target,
            original: Some(config.clone()),
            stored_key: entry.has_key,
            name: config.name.clone(),
            protocol: config.protocol,
            endpoint: config.endpoint.clone(),
            backup: config.backup_endpoint.clone().unwrap_or_default(),
            output: Some(config.output),
            reconcile_on_startup: config.reconcile_on_startup,
            bitrate_kbps: config.video_bitrate_kbps,
            initial_delay_ms: config.reconnect.initial_delay_ms,
            max_delay_ms: config.reconnect.max_delay_ms,
            attempts: config.reconnect.max_attempts.unwrap_or(40),
            unlimited: config.reconnect.max_attempts.is_none(),
            key: String::new(),
            reveal_key: false,
            srt_latency_ms: config
                .srt_latency_ms
                .unwrap_or(DEFAULT_STREAM_TARGET_SRT_LATENCY_MS),
            stored_passphrase: entry.has_passphrase,
            encrypt: entry.has_passphrase,
            passphrase: String::new(),
            reveal_passphrase: false,
        }
    }

    /// Whether this form edits an existing destination.
    pub(crate) const fn editing(&self) -> bool {
        self.original.is_some()
    }

    /// Switches the protocol; HLS keeps neither a backup server nor a key.
    pub(crate) fn set_protocol(&mut self, protocol: StreamTargetProtocol) {
        self.protocol = protocol;
        if !protocol.carries_key() {
            self.key.clear();
            self.backup.clear();
        }
        if protocol != StreamTargetProtocol::Srt {
            self.passphrase.clear();
        }
    }

    /// The SRT passphrase edit the form describes, or why the daemon would
    /// refuse it. Only an encrypted SRT destination keeps or sets one.
    pub(crate) fn passphrase_edit(&self) -> Result<SecretEdit, FieldProblem<TargetField>> {
        if self.protocol != StreamTargetProtocol::Srt || !self.encrypt {
            return Ok(if self.stored_passphrase {
                SecretEdit::Clear
            } else {
                SecretEdit::Keep
            });
        }
        if self.passphrase.is_empty() {
            return if self.editing() && self.stored_passphrase {
                Ok(SecretEdit::Keep)
            } else {
                Err(FieldProblem::prompt(
                    TargetField::Passphrase,
                    Msg::StreamErrPassphraseRequired.en(),
                ))
            };
        }
        if !srt_passphrase_is_valid(&self.passphrase) || self.passphrase == REDACTED_KEY {
            return Err(FieldProblem::error(
                TargetField::Passphrase,
                Msg::StreamErrPassphraseChars.en(),
            ));
        }
        Ok(SecretEdit::Set(self.passphrase.clone()))
    }

    /// The key edit the form describes, or why the daemon would refuse it.
    fn key_edit(&self) -> Result<SecretEdit, FieldProblem<TargetField>> {
        if !self.protocol.carries_key() {
            return Ok(if self.stored_key {
                SecretEdit::Clear
            } else {
                SecretEdit::Keep
            });
        }
        if self.key.is_empty() {
            return if self.editing() && self.stored_key {
                Ok(SecretEdit::Keep)
            } else {
                Err(FieldProblem::prompt(
                    TargetField::Key,
                    Msg::StreamErrKeyRequired.en(),
                ))
            };
        }
        // The daemon validates through `fm_model::StreamKey::parse`, which
        // also refuses a `/`, a key under `MIN_STREAM_KEY_BYTES`, and the
        // codec refuses the redaction marker itself.
        if !stream_key_is_valid(&self.key)
            || self.key.len() > MAX_STREAM_TARGET_KEY_BYTES
            || self.key.len() < MIN_STREAM_KEY_BYTES
            || self.key.contains('/')
            || self.key == REDACTED_KEY
        {
            return Err(FieldProblem::error(
                TargetField::Key,
                Msg::StreamErrKeyChars.en(),
            ));
        }
        if self.protocol == StreamTargetProtocol::Srt && self.key.contains(['&', '?']) {
            return Err(FieldProblem::error(
                TargetField::Key,
                Msg::StreamErrKeySrt.en(),
            ));
        }
        Ok(SecretEdit::Set(self.key.clone()))
    }

    /// The configuration and key edit the form describes, or the first
    /// problem (top to bottom) the daemon would refuse. `taken` holds the
    /// other destinations' names; `outputs` the show's outputs.
    pub(crate) fn checked(
        &self,
        taken: &[&str],
        outputs: &[WireOutputId],
    ) -> Result<(StreamTargetConfig, SecretEdit), FieldProblem<TargetField>> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err(FieldProblem::prompt(
                TargetField::Name,
                Msg::StreamErrNameEmpty.en(),
            ));
        }
        if name.len() > MAX_STREAM_TARGET_NAME_BYTES {
            return Err(FieldProblem::error(
                TargetField::Name,
                Msg::StreamErrNameLong.en(),
            ));
        }
        if taken.iter().any(|other| other.eq_ignore_ascii_case(name)) {
            return Err(FieldProblem::error(
                TargetField::Name,
                Msg::StreamErrNameTaken.en(),
            ));
        }
        let endpoint = self.endpoint.trim();
        if let Some(reason) = endpoint_problem(self.protocol, endpoint) {
            return Err(if endpoint.is_empty() {
                FieldProblem::prompt(TargetField::Endpoint, reason.en())
            } else {
                FieldProblem::error(TargetField::Endpoint, reason.en())
            });
        }
        let backup = self.backup.trim();
        let backup_endpoint = if self.protocol.carries_key() && !backup.is_empty() {
            if let Some(reason) = endpoint_problem(self.protocol, backup) {
                return Err(FieldProblem::error(TargetField::Backup, reason.en()));
            }
            Some(backup.to_owned())
        } else {
            None
        };
        let stream_key = self.key_edit()?;
        let srt = self.protocol == StreamTargetProtocol::Srt;
        if srt
            && !(MIN_STREAM_TARGET_SRT_LATENCY_MS..=MAX_STREAM_TARGET_SRT_LATENCY_MS)
                .contains(&self.srt_latency_ms)
        {
            return Err(FieldProblem::error(
                TargetField::Latency,
                Msg::StreamErrLatency.en(),
            ));
        }
        self.passphrase_edit()?;
        let output = self
            .output
            .filter(|output| outputs.contains(output))
            .ok_or(FieldProblem::prompt(
                TargetField::Output,
                Msg::StreamErrOutput.en(),
            ))?;
        if !(MIN_STREAM_TARGET_BITRATE_KBPS..=MAX_STREAM_TARGET_BITRATE_KBPS)
            .contains(&self.bitrate_kbps)
        {
            return Err(FieldProblem::error(
                TargetField::Bitrate,
                Msg::StreamErrBitrate.en(),
            ));
        }
        let reconnect = StreamReconnect {
            initial_delay_ms: self.initial_delay_ms,
            max_delay_ms: self.max_delay_ms,
            max_attempts: (!self.unlimited).then_some(self.attempts),
        };
        if !(StreamReconnect {
            max_attempts: None,
            ..reconnect
        })
        .is_valid()
        {
            return Err(FieldProblem::error(
                TargetField::Delays,
                Msg::StreamErrDelays.en(),
            ));
        }
        if !reconnect.is_valid() {
            return Err(FieldProblem::error(
                TargetField::Attempts,
                Msg::StreamErrAttempts.en(),
            ));
        }
        let config = StreamTargetConfig {
            name: name.to_owned(),
            protocol: self.protocol,
            endpoint: endpoint.to_owned(),
            backup_endpoint,
            output,
            reconcile_on_startup: self.reconcile_on_startup,
            video_bitrate_kbps: self.bitrate_kbps,
            reconnect,
            srt_latency_ms: srt.then_some(self.srt_latency_ms),
        };
        debug_assert!(config.is_valid());
        Ok((config, stream_key))
    }
}

/// Why `endpoint` is not a valid scheme-relative endpoint for `protocol`,
/// mirroring the daemon's rules: `host[:port]/app[/path]` for RTMP/RTMPS,
/// `host[:port]` for SRT, and an absolute directory for HLS.
pub(crate) fn endpoint_problem(protocol: StreamTargetProtocol, endpoint: &str) -> Option<Msg> {
    if endpoint.is_empty() {
        return Some(Msg::StreamErrEndpointEmpty);
    }
    if endpoint.len() > MAX_STREAM_TARGET_ENDPOINT_BYTES {
        return Some(Msg::StreamErrEndpointLong);
    }
    if !endpoint.bytes().all(|byte| (0x21..=0x7e).contains(&byte)) {
        return Some(Msg::StreamErrEndpointChars);
    }
    if endpoint.contains("://") {
        return Some(Msg::StreamErrEndpointScheme);
    }
    if endpoint.contains(['?', '#']) {
        return Some(Msg::StreamErrEndpointQuery);
    }
    match protocol {
        StreamTargetProtocol::Rtmp | StreamTargetProtocol::Rtmps => {
            let Some((authority, path)) = endpoint.split_once('/') else {
                return Some(Msg::StreamErrEndpointApp);
            };
            if authority.is_empty() || authority.starts_with(':') {
                return Some(Msg::StreamErrEndpointHost);
            }
            if authority.contains('@') {
                return Some(Msg::StreamErrEndpointCredentials);
            }
            if path.split('/').any(str::is_empty) {
                return Some(Msg::StreamErrEndpointSegments);
            }
        }
        StreamTargetProtocol::Srt => {
            if endpoint.contains('/') {
                return Some(Msg::StreamErrEndpointSrtPath);
            }
            if endpoint.split(':').next().is_none_or(str::is_empty) {
                return Some(Msg::StreamErrEndpointHost);
            }
            if endpoint.contains('@') {
                return Some(Msg::StreamErrEndpointCredentials);
            }
        }
        StreamTargetProtocol::Hls => {
            let Some(path) = endpoint.strip_prefix('/') else {
                return Some(Msg::StreamErrEndpointAbsolute);
            };
            if path
                .split('/')
                .any(|segment| segment.is_empty() || segment == "." || segment == "..")
            {
                return Some(Msg::StreamErrEndpointSegments);
            }
        }
    }
    None
}

/// The names of every destination but `target`.
fn other_names(state: &StudioUiState, target: WireStreamTargetId) -> Vec<String> {
    let mut names: Vec<String> = state
        .streams
        .iter()
        .filter(|stream| stream.target != target.get())
        .map(|stream| stream.name.clone())
        .collect();
    if let Some(view) = &state.view {
        names.extend(
            view.stream_targets
                .iter()
                .filter(|entry| entry.target != target)
                .map(|entry| entry.config.name.clone()),
        );
    }
    names
}

/// The show's output ids.
fn output_ids(state: &StudioUiState) -> Vec<WireOutputId> {
    state.view.as_ref().map_or_else(Vec::new, |view| {
        view.outputs
            .iter()
            .map(|status| WireOutputId::from_domain(status.output))
            .collect()
    })
}

/// Why the dialog cannot submit, in the order the operator fixes it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Blocked {
    /// A session gate, the padlock, or the destination's state.
    Gate(&'static str),
    /// A form field to fill in or fix.
    Field(FieldProblem<TargetField>),
    /// An edit that changes nothing.
    Unchanged,
}

impl Blocked {
    /// The English catalog reason.
    pub(crate) const fn reason(self) -> &'static str {
        match self {
            Self::Gate(reason) => reason,
            Self::Field(problem) => problem.reason,
            Self::Unchanged => Msg::ReasonStreamUnchanged.en(),
        }
    }
}

/// The intent the dialog submits this frame, or why it cannot.
pub(crate) fn submission(
    draft: &TargetDraft,
    state: &StudioUiState,
) -> Result<StudioIntent, Blocked> {
    let gate = if draft.editing() {
        edit_block(state, draft.target)
    } else {
        add_block(state)
    };
    if let Some(reason) = gate {
        return Err(Blocked::Gate(reason));
    }
    // An add whose id another console took meanwhile would reconfigure
    // that destination; [`StreamTargetsUi::reconcile`] moves the draft to a
    // free id before it can submit.
    if !draft.editing() && known_targets(state).contains(&draft.target.get()) {
        return Err(Blocked::Gate(Msg::ReasonStreamPending.en()));
    }
    let names = other_names(state, draft.target);
    let taken: Vec<&str> = names.iter().map(String::as_str).collect();
    let (config, stream_key) = draft
        .checked(&taken, &output_ids(state))
        .map_err(Blocked::Field)?;
    let srt_passphrase = draft.passphrase_edit().map_err(Blocked::Field)?;
    if draft.original.as_ref() == Some(&config)
        && stream_key == SecretEdit::Keep
        && srt_passphrase == SecretEdit::Keep
    {
        return Err(Blocked::Unchanged);
    }
    Ok(StudioIntent::SetStreamTarget {
        target: draft.target,
        config: Box::new(config),
        stream_key,
        srt_passphrase,
    })
}

// ---------------------------------------------------------------------------
// Console state
// ---------------------------------------------------------------------------

/// A Remove confirmation for one destination.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoveConfirm {
    pub(crate) target: WireStreamTargetId,
    pub(crate) name: String,
    /// The destination's URL without its key.
    pub(crate) url: String,
}

/// The destination dialogs open in the console.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct StreamTargetsUi {
    pub(crate) dialog: Option<TargetDraft>,
    pub(crate) remove: Option<RemoveConfirm>,
}

impl StreamTargetsUi {
    /// Opens the Add destination dialog when adding is available.
    pub(crate) fn open_add(&mut self, l: Locale, state: &StudioUiState) {
        if add_block(state).is_some() {
            return;
        }
        let Some(target) = next_target_id(state) else {
            return;
        };
        let output = output_ids(state).first().copied();
        let names = other_names(state, target);
        let mut draft = TargetDraft::new(l, target, output, known_targets(state).len() + 1);
        // The first default name no other destination uses.
        for position in known_targets(state).len() + 1.. {
            draft.name = format!("{} {position}", l.text(Msg::StreamSectionDestination));
            if !names
                .iter()
                .any(|name| name.trim().eq_ignore_ascii_case(draft.name.trim()))
            {
                break;
            }
        }
        self.dialog = Some(draft);
    }

    /// Opens the Edit dialog for `target` when editing is available.
    pub(crate) fn open_edit(&mut self, state: &StudioUiState, target: WireStreamTargetId) {
        if edit_block(state, target).is_some() {
            return;
        }
        if let Some(entry) = state
            .view
            .as_ref()
            .and_then(|view| view.stream_target(target.get()))
        {
            self.dialog = Some(TargetDraft::edit(entry));
        }
    }

    /// Opens the Remove confirmation for `target`.
    pub(crate) fn open_remove(&mut self, state: &StudioUiState, target: WireStreamTargetId) {
        let Some(view) = state.view.as_ref() else {
            return;
        };
        let name = state
            .streams
            .iter()
            .find(|stream| stream.target == target.get())
            .map(|stream| stream.name.clone());
        let entry = view.stream_target(target.get());
        let Some(name) = name.or_else(|| entry.map(|entry| entry.config.name.clone())) else {
            return;
        };
        self.remove = Some(RemoveConfirm {
            target,
            name,
            url: entry.map_or_else(String::new, |entry| entry.config.display_url()),
        });
    }

    /// Closes dialogs whose destination is gone.
    fn reconcile(&mut self, state: &StudioUiState) {
        let Some(view) = state.view.as_ref() else {
            return;
        };
        let gone = |target: WireStreamTargetId| {
            view.stream_target(target.get()).is_none()
                && !state
                    .streams
                    .iter()
                    .any(|stream| stream.target == target.get())
        };
        if self
            .dialog
            .as_ref()
            .is_some_and(|draft| draft.editing() && gone(draft.target))
        {
            self.dialog = None;
        }
        // Another console added a destination under this add's id: the add
        // moves to the next free id instead of reconfiguring that one.
        if let Some(draft) = self.dialog.as_mut()
            && !draft.editing()
            && known_targets(state).contains(&draft.target.get())
        {
            match next_target_id(state) {
                Some(next) => draft.target = next,
                None => self.dialog = None,
            }
        }
        if self
            .remove
            .as_ref()
            .is_some_and(|confirm| gone(confirm.target))
        {
            self.remove = None;
        }
    }
}

// ---------------------------------------------------------------------------
// The panel's keys
// ---------------------------------------------------------------------------

/// The Start all / Stop all / Add keys under the Streams heading.
pub(crate) fn draw_group_keys(
    ui: &mut Ui,
    state: &StudioUiState,
    dialogs: &mut StreamTargetsUi,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 4.0);
        for (action, label, hover, keys) in [
            (
                StreamGroupAction::StartAll,
                Msg::StreamStartAllKey,
                Msg::StreamStartAllHover,
                crate::keymap::START_STREAMS_SHORTCUT,
            ),
            (
                StreamGroupAction::StopAll,
                Msg::StreamStopAllKey,
                Msg::StreamStopAllHover,
                crate::keymap::STOP_STREAMS_SHORTCUT,
            ),
        ] {
            let reason = group_unavailable_reason(state, action);
            if ui
                .add_enabled(reason.is_none(), small_button(l.text(label)))
                .hint(
                    &crate::l10n::with_key(l, l.text(hover), keys),
                    reason.unwrap_or(""),
                )
                .clicked()
                && let Some(action) = stream_group_action(state, action)
            {
                intents.push(StudioIntent::StreamGroup(action));
            }
        }
        let reason = add_block(state);
        if ui
            .add_enabled(reason.is_none(), small_button(l.text(Msg::StreamAddKey)))
            .hint(
                &l.format(
                    Msg::StreamAddHover,
                    &[("limit", Arg::from(MAX_STREAM_TARGETS))],
                ),
                reason.unwrap_or(""),
            )
            .clicked()
        {
            dialogs.open_add(l, state);
        }
    });
    ui.add_space(4.0);
}

/// Gap between a destination row's Retry / Edit / Remove keys.
const ROW_KEY_GAP: f32 = 4.0;
/// Width of the collapsed `···` key that holds them in a narrow panel.
pub(crate) const ROW_MENU_WIDTH: f32 = 30.0;

/// Width the Retry / Edit / Remove keys take side by side in a row.
pub(crate) fn row_keys_width(ui: &Ui) -> f32 {
    let l = l10n::locale(ui.ctx());
    let padding = ui.spacing().button_padding.x * 2.0;
    let text = |msg: Msg| {
        ui.painter()
            .layout_no_wrap(
                l.text(msg).to_owned(),
                egui::FontId::proportional(crate::theme::legible(11.0)),
                text_color(),
            )
            .size()
            .x
            + padding
    };
    (text(Msg::StreamRetryKey)
        + text(Msg::StreamEditKey)
        + crate::theme::MIN_TARGET
        + ROW_KEY_GAP * 2.0)
        .ceil()
}

/// The Retry / Edit / Remove keys inside one destination's row, laid out
/// left to right in `ui` (`height` points tall); `collapsed` puts them
/// behind one `···` menu key for a narrow panel.
pub(crate) fn draw_row_keys(
    ui: &mut Ui,
    state: &StudioUiState,
    (target, name): (WireStreamTargetId, &str),
    (collapsed, height): (bool, f32),
    dialogs: &mut StreamTargetsUi,
    intents: &mut Vec<StudioIntent>,
) {
    let l = l10n::locale(ui.ctx());
    let Some(stream) = state
        .streams
        .iter()
        .find(|stream| stream.target == target.get())
    else {
        return;
    };
    let name_arg = [("name", Arg::Str(name))];
    let retry = stream_action(state, StreamAction::Retry(target));
    let edit = edit_block(state, target);
    let remove = remove_block(state, target);
    if collapsed {
        let (more, _) = egui::containers::menu::MenuButton::from_button(
            small_button("···").min_size(Vec2::new(ROW_MENU_WIDTH, height)),
        )
        .ui(ui, |ui| {
            if ui
                .add_enabled(retry.is_some(), Button::new(l.text(Msg::StreamRetryKey)))
                .hint(
                    &l.format(Msg::StreamRetryHover, &name_arg),
                    retry_unavailable_reason(state, stream),
                )
                .clicked()
                && let Some(action) = retry
            {
                intents.push(StudioIntent::Stream(action));
            }
            if ui
                .add_enabled(edit.is_none(), Button::new(l.text(Msg::StreamEditKey)))
                .hint(
                    &l.format(Msg::StreamEditHover, &name_arg),
                    edit.unwrap_or(""),
                )
                .clicked()
            {
                dialogs.open_edit(state, target);
            }
            if ui
                .add_enabled(
                    remove.is_none(),
                    Button::new(l.text(Msg::StreamRemoveMenuItem)),
                )
                .hint(
                    &l.format(Msg::StreamRemoveHover, &name_arg),
                    remove.unwrap_or(""),
                )
                .clicked()
            {
                dialogs.open_remove(state, target);
            }
        });
        more.on_hover_text(l.format(Msg::StreamRowMoreHover, &name_arg));
        return;
    }
    ui.spacing_mut().item_spacing.x = ROW_KEY_GAP;
    let key = |text: &str| small_button(text).min_size(Vec2::new(0.0, height));
    if ui
        .add_enabled(retry.is_some(), key(l.text(Msg::StreamRetryKey)))
        .hint(
            &l.format(Msg::StreamRetryHover, &name_arg),
            retry_unavailable_reason(state, stream),
        )
        .clicked()
        && let Some(action) = retry
    {
        intents.push(StudioIntent::Stream(action));
    }
    if ui
        .add_enabled(edit.is_none(), key(l.text(Msg::StreamEditKey)))
        .hint(
            &l.format(Msg::StreamEditHover, &name_arg),
            edit.unwrap_or(""),
        )
        .clicked()
    {
        dialogs.open_edit(state, target);
    }
    if add_icon_button(
        ui,
        remove.is_none(),
        Button::new("").min_size(Vec2::new(crate::theme::MIN_TARGET, height)),
        KeyIcon::Remove,
        l.text(Msg::StreamRemoveName),
    )
    .hint(
        &l.format(Msg::StreamRemoveHover, &name_arg),
        remove.unwrap_or(""),
    )
    .clicked()
    {
        dialogs.open_remove(state, target);
    }
}

// ---------------------------------------------------------------------------
// The dialogs
// ---------------------------------------------------------------------------

/// The key a protocol button shows, and its hover help.
const fn protocol_key(protocol: StreamTargetProtocol) -> (&'static str, Msg) {
    match protocol {
        StreamTargetProtocol::Rtmp => ("RTMP", Msg::StreamProtoRtmpHover),
        StreamTargetProtocol::Rtmps => ("RTMPS", Msg::StreamProtoRtmpsHover),
        StreamTargetProtocol::Srt => ("SRT", Msg::StreamProtoSrtHover),
        StreamTargetProtocol::Hls => ("HLS", Msg::StreamProtoHlsHover),
    }
}

/// The endpoint field's hover help and placeholder for `protocol`.
const fn endpoint_help(protocol: StreamTargetProtocol) -> (Msg, &'static str) {
    match protocol {
        StreamTargetProtocol::Rtmp | StreamTargetProtocol::Rtmps => {
            (Msg::StreamEndpointRtmpHover, "live.example.com/app")
        }
        StreamTargetProtocol::Srt => (Msg::StreamEndpointSrtHover, "srt.example.com:9000"),
        StreamTargetProtocol::Hls => (Msg::StreamEndpointHlsHover, "/var/www/hls/show"),
    }
}

/// The caption under a field: its problem in red, a prompt in grey, or
/// `fallback`.
fn field_hint(
    l: Locale,
    problem: Option<FieldProblem<TargetField>>,
    field: TargetField,
    fallback: Option<RichText>,
) -> Option<RichText> {
    match problem {
        Some(problem) if problem.field == field => Some(if problem.prompt {
            caption(capitalized(l.tr(problem.reason)))
        } else {
            field_error(l, problem.reason)
        }),
        _ => fallback,
    }
}

/// A labelled number field in a grid; a problem outlines it in red.
#[allow(clippy::too_many_arguments)] // one grid row
fn number_row(
    ui: &mut Ui,
    l: Locale,
    (label, hover): (Msg, Msg),
    value: &mut u32,
    range: core::ops::RangeInclusive<u32>,
    invalid: bool,
    disabled: Option<&str>,
) {
    ui.label(
        RichText::new(l.text(label))
            .size(crate::theme::legible(11.0))
            .color(text_dim_color()),
    )
    .on_hover_text(l.text(hover));
    let response = ui
        .add_enabled(
            disabled.is_none(),
            DragValue::new(value).range(range).speed(10.0),
        )
        .hint(l.text(hover), disabled.unwrap_or(""));
    if invalid {
        mark_invalid(ui, response.rect);
    }
    ui.end_row();
}

/// The dialog's body: the Destination, Output and video, and Reconnect
/// sections.
#[allow(clippy::too_many_lines)] // one form
fn draw_form(ui: &mut Ui, l: Locale, state: &StudioUiState, draft: &mut TargetDraft) {
    let names = other_names(state, draft.target);
    let taken: Vec<&str> = names.iter().map(String::as_str).collect();
    let problem = draft.checked(&taken, &output_ids(state)).err();
    let error = problem.filter(|problem| !problem.prompt);
    let invalid = |field: TargetField| error.is_some_and(|problem| problem.field == field);
    ui.add(
        egui::Label::new(
            RichText::new(l.text(Msg::StreamDialogIntro))
                .size(crate::theme::legible(11.0))
                .color(text_dim_color()),
        )
        .wrap(),
    );

    section_heading(ui, l, Msg::StreamSectionDestination, None);
    sub_label(ui, l, Msg::AddInputName);
    let response = ui
        .add(
            egui::TextEdit::singleline(&mut draft.name)
                .id(egui::Id::new("studio-stream-target-name"))
                .char_limit(MAX_STREAM_TARGET_NAME_BYTES)
                .desired_width(ui.available_width()),
        )
        .on_hover_text(l.text(Msg::StreamFieldNameHover));
    if invalid(TargetField::Name) {
        mark_invalid(ui, response.rect);
    }
    hint_slot(ui, field_hint(l, problem, TargetField::Name, None));

    sub_label(ui, l, Msg::StreamFieldProtocol);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
        for protocol in StreamTargetProtocol::ALL {
            let (label, hover) = protocol_key(protocol);
            let selected = draft.protocol == protocol;
            if ui
                .add(kind_key(label, selected).min_size(Vec2::new(72.0, 26.0)))
                .on_hover_text(l.text(hover))
                .clicked()
                && !selected
            {
                draft.set_protocol(protocol);
            }
        }
    });
    ui.add_space(4.0);

    let hls = !draft.protocol.carries_key();
    let (endpoint_hover, placeholder) = endpoint_help(draft.protocol);
    sub_label(ui, l, TargetField::Endpoint.label(draft.protocol));
    let response = ui
        .add(
            egui::TextEdit::singleline(&mut draft.endpoint)
                .id(egui::Id::new("studio-stream-target-endpoint"))
                .hint_text(placeholder)
                .char_limit(MAX_STREAM_TARGET_ENDPOINT_BYTES)
                .desired_width(ui.available_width()),
        )
        .on_hover_text(l.text(endpoint_hover));
    if invalid(TargetField::Endpoint) {
        mark_invalid(ui, response.rect);
    }
    let url = (!draft.endpoint.trim().is_empty()).then(|| {
        caption(format!(
            "{}://{}",
            draft.protocol.token(),
            draft.endpoint.trim()
        ))
    });
    hint_slot(ui, field_hint(l, problem, TargetField::Endpoint, url));

    sub_label(ui, l, Msg::StreamFieldBackup);
    let response = ui
        .add_enabled(
            !hls,
            egui::TextEdit::singleline(&mut draft.backup)
                .id(egui::Id::new("studio-stream-target-backup"))
                .hint_text(if hls { "" } else { placeholder })
                .char_limit(MAX_STREAM_TARGET_ENDPOINT_BYTES)
                .desired_width(ui.available_width()),
        )
        .hint(
            l.text(Msg::StreamFieldBackupHover),
            Msg::ReasonHlsNoBackup.en(),
        );
    if invalid(TargetField::Backup) {
        mark_invalid(ui, response.rect);
    }
    hint_slot(ui, field_hint(l, problem, TargetField::Backup, None));

    sub_label(ui, l, Msg::NsStreamKey);
    ui.horizontal(|ui| {
        let width = (ui.available_width() - crate::theme::MIN_TARGET - ui.spacing().item_spacing.x)
            .max(60.0);
        let keep = draft.editing() && draft.stored_key;
        let response = key_field(ui, l, draft, hls, keep, width)
            .hint(l.text(Msg::StreamFieldKeyHover), Msg::ReasonHlsNoKey.en());
        if invalid(TargetField::Key) {
            mark_invalid(ui, response.rect);
        }
        if hls {
            ui.allocate_exact_size(Vec2::splat(crate::theme::MIN_TARGET), Sense::hover());
        } else {
            reveal_key(ui, l, &mut draft.reveal_key);
        }
    });
    let key_caption = if hls && draft.stored_key {
        Some(caption(l.text(Msg::StreamKeyClearCaption)))
    } else if !hls && draft.editing() && draft.stored_key && draft.key.is_empty() {
        Some(caption(l.text(Msg::StreamKeyKeepCaption)))
    } else if !hls && draft.editing() && draft.stored_key {
        // Typing over a stored key says what Save does instead of leaving
        // the caption's slot empty.
        Some(caption(l.text(Msg::StreamKeyReplaceCaption)))
    } else {
        None
    };
    hint_slot(ui, field_hint(l, problem, TargetField::Key, key_caption));
    if draft.protocol == StreamTargetProtocol::Srt {
        draw_srt_fields(ui, l, draft, problem);
    }

    section_heading(ui, l, Msg::StreamSectionVideo, None);
    sub_label(ui, l, Msg::MenuOutput);
    let outputs: Vec<(WireOutputId, String)> = state.view.as_ref().map_or_else(Vec::new, |view| {
        view.outputs
            .iter()
            .map(|status| {
                (
                    WireOutputId::from_domain(status.output),
                    status.name.clone(),
                )
            })
            .collect()
    });
    let row = ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
        for (output, name) in &outputs {
            let selected = draft.output == Some(*output);
            if ui
                .add(kind_key(name, selected).min_size(Vec2::new(96.0, 26.0)))
                .on_hover_text(l.text(Msg::StreamFieldOutputHover))
                .clicked()
            {
                draft.output = Some(*output);
            }
        }
    });
    if invalid(TargetField::Output) {
        mark_invalid(ui, row.response.rect);
    }
    hint_slot(ui, field_hint(l, problem, TargetField::Output, None));
    egui::Grid::new("studio-stream-target-video")
        .num_columns(2)
        .spacing(Vec2::new(10.0, 4.0))
        .show(ui, |ui| {
            number_row(
                ui,
                l,
                (Msg::StreamFieldBitrate, Msg::StreamFieldBitrateHover),
                &mut draft.bitrate_kbps,
                MIN_STREAM_TARGET_BITRATE_KBPS..=MAX_STREAM_TARGET_BITRATE_KBPS,
                invalid(TargetField::Bitrate),
                None,
            );
        });
    if let Some(problem) = error.filter(|problem| problem.field == TargetField::Bitrate) {
        ui.add(egui::Label::new(field_error(l, problem.reason)).wrap());
    }
    ui.checkbox(
        &mut draft.reconcile_on_startup,
        l.text(Msg::StreamFieldStartup),
    )
    .on_hover_text(l.text(Msg::StreamFieldStartupHover));

    section_heading(ui, l, Msg::StreamSectionReconnect, None);
    egui::Grid::new("studio-stream-target-reconnect")
        .num_columns(2)
        .spacing(Vec2::new(10.0, 4.0))
        .show(ui, |ui| {
            let delays = invalid(TargetField::Delays);
            number_row(
                ui,
                l,
                (Msg::NsBackoffMin, Msg::StreamFieldFirstDelayHover),
                &mut draft.initial_delay_ms,
                MIN_STREAM_RECONNECT_DELAY_MS..=MAX_STREAM_RECONNECT_DELAY_MS,
                delays,
                None,
            );
            number_row(
                ui,
                l,
                (Msg::StreamFieldMaxDelay, Msg::StreamFieldMaxDelayHover),
                &mut draft.max_delay_ms,
                MIN_STREAM_RECONNECT_DELAY_MS..=MAX_STREAM_RECONNECT_DELAY_MS,
                delays,
                None,
            );
            number_row(
                ui,
                l,
                (Msg::StreamFieldAttempts, Msg::StreamFieldAttemptsHover),
                &mut draft.attempts,
                1..=MAX_STREAM_RECONNECT_ATTEMPTS,
                invalid(TargetField::Attempts),
                draft.unlimited.then_some(Msg::ReasonStreamUnlimited.en()),
            );
        });
    ui.checkbox(&mut draft.unlimited, l.text(Msg::StreamFieldUnlimited))
        .on_hover_text(l.text(Msg::StreamFieldUnlimitedHover));
    if let Some(problem) =
        error.filter(|problem| matches!(problem.field, TargetField::Delays | TargetField::Attempts))
    {
        ui.add(egui::Label::new(field_error(l, problem.reason)).wrap());
    }
}

/// An SRT destination's send latency, Encrypt toggle, and masked passphrase
/// (Protocol 2.54).
fn draw_srt_fields(
    ui: &mut Ui,
    l: Locale,
    draft: &mut TargetDraft,
    problem: Option<FieldProblem<TargetField>>,
) {
    let invalid = |field: TargetField| {
        problem.is_some_and(|problem| !problem.prompt && problem.field == field)
    };
    egui::Grid::new("studio-stream-target-srt")
        .num_columns(2)
        .spacing(Vec2::new(10.0, 4.0))
        .show(ui, |ui| {
            number_row(
                ui,
                l,
                (Msg::NsLatency, Msg::NsLatencyHover),
                &mut draft.srt_latency_ms,
                MIN_STREAM_TARGET_SRT_LATENCY_MS..=MAX_STREAM_TARGET_SRT_LATENCY_MS,
                invalid(TargetField::Latency),
                None,
            );
        });
    hint_slot(ui, field_hint(l, problem, TargetField::Latency, None));
    ui.checkbox(&mut draft.encrypt, l.text(Msg::StreamFieldEncrypt))
        .on_hover_text(l.text(Msg::StreamFieldEncryptHover));
    sub_label(ui, l, Msg::NsPassphrase);
    let encrypt = draft.encrypt;
    let keep = draft.editing() && draft.stored_passphrase;
    ui.horizontal(|ui| {
        let width = (ui.available_width() - crate::theme::MIN_TARGET - ui.spacing().item_spacing.x)
            .max(60.0);
        let hint = if !draft.passphrase.is_empty() && !draft.reveal_passphrase {
            KEY_MASK
        } else if keep && encrypt {
            l.text(Msg::StreamKeyKeepHint)
        } else {
            ""
        };
        let response = masked_secret_field(
            ui,
            &mut draft.passphrase,
            (draft.reveal_passphrase, encrypt),
            passphrase_field_id(),
            MAX_STREAM_TARGET_PASSPHRASE_BYTES,
            hint,
            width,
        )
        .hint(
            l.text(Msg::StreamFieldPassphraseHover),
            Msg::ReasonStreamNotEncrypted.en(),
        );
        if invalid(TargetField::Passphrase) {
            mark_invalid(ui, response.rect);
        }
        reveal_key(ui, l, &mut draft.reveal_passphrase);
    });
    let passphrase_caption = if !encrypt && draft.stored_passphrase {
        Some(caption(l.text(Msg::StreamPassphraseClearCaption)))
    } else if encrypt && keep && draft.passphrase.is_empty() {
        Some(caption(l.text(Msg::StreamPassphraseKeepCaption)))
    } else if encrypt && keep {
        Some(caption(l.text(Msg::StreamPassphraseReplaceCaption)))
    } else {
        None
    };
    hint_slot(
        ui,
        field_hint(l, problem, TargetField::Passphrase, passphrase_caption),
    );
}

/// The footer line for a blocked submission: a short pointer and the full
/// reason on hover.
fn footer_text(l: Locale, protocol: StreamTargetProtocol, blocked: Blocked) -> (String, String) {
    match blocked {
        Blocked::Gate(reason) => {
            let text = l10n::unavailable(l, reason);
            (text.clone(), text)
        }
        Blocked::Field(problem) => {
            let full = capitalized(l.tr(problem.reason));
            let short = if problem.prompt {
                full.clone()
            } else {
                l.format(
                    Msg::AddInputFixField,
                    &[("field", Arg::Str(l.text(problem.field.label(protocol))))],
                )
            };
            (short, full)
        }
        Blocked::Unchanged => {
            let text = capitalized(l.tr(Msg::ReasonStreamUnchanged.en()));
            (text.clone(), text)
        }
    }
}

/// The masked stream key as the field shows it: a fixed run of dots, so
/// the mask never tells how long the key is.
const KEY_MASK: &str = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";

/// The stream key field. Revealed, it edits the key directly. Masked, it
/// edits an empty buffer each frame (typed or pasted text is appended to
/// the key, Backspace drops its last character) and shows [`KEY_MASK`]
/// whatever the key's length, instead of one dot per character.
fn key_field(
    ui: &mut Ui,
    l: Locale,
    draft: &mut TargetDraft,
    hls: bool,
    keep: bool,
    width: f32,
) -> egui::Response {
    let hint = if hls {
        ""
    } else if !draft.key.is_empty() && !draft.reveal_key {
        KEY_MASK
    } else if keep {
        l.text(Msg::StreamKeyKeepHint)
    } else {
        ""
    };
    masked_secret_field(
        ui,
        &mut draft.key,
        (draft.reveal_key, !hls),
        key_field_id(),
        MAX_STREAM_TARGET_KEY_BYTES,
        hint,
        width,
    )
}

/// One secret field (the stream key or SRT passphrase), `(revealed,
/// enabled)`. Revealed, it edits `secret` directly. Masked, it edits an
/// empty buffer each frame (typed or pasted text is appended, Backspace drops
/// the last character) and shows `hint` instead of one dot per character.
fn masked_secret_field(
    ui: &mut Ui,
    secret: &mut String,
    (revealed, enabled): (bool, bool),
    id: egui::Id,
    limit: usize,
    hint: &str,
    width: f32,
) -> egui::Response {
    if revealed {
        return ui.add_enabled(
            enabled,
            egui::TextEdit::singleline(secret)
                .id(id)
                .hint_text(hint)
                .char_limit(limit)
                .desired_width(width),
        );
    }
    let mut typed = String::new();
    let response = ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(&mut typed)
            .id(id)
            .password(true)
            .hint_text(egui::RichText::new(hint).color(if secret.is_empty() {
                ui.visuals().weak_text_color()
            } else {
                ui.visuals().text_color()
            }))
            .desired_width(width),
    );
    if response.has_focus() {
        if !typed.is_empty() {
            let room = limit.saturating_sub(secret.len());
            let mut end = typed.len().min(room);
            while !typed.is_char_boundary(end) {
                end -= 1;
            }
            secret.push_str(&typed[..end]);
        } else if ui.input(|input| input.key_pressed(egui::Key::Backspace)) {
            secret.pop();
        }
    }
    response
}

/// The SRT passphrase field's widget id.
fn passphrase_field_id() -> egui::Id {
    egui::Id::new("studio-stream-target-passphrase")
}

/// The stream key field's widget id.
fn key_field_id() -> egui::Id {
    egui::Id::new("studio-stream-target-key")
}

/// Drops the key field's widget state once no dialog shows it: egui keeps a
/// text field's undo history in context memory under its id, so a later
/// dialog (for any destination) could otherwise bring a typed key back with
/// one undo, and send it for the wrong destination.
fn forget_key_field(context: &egui::Context) {
    context.data_mut(|data| {
        data.remove::<egui::text_edit::TextEditState>(key_field_id());
        data.remove::<egui::text_edit::TextEditState>(passphrase_field_id());
    });
}

impl StudioShell {
    /// Draws the Add / Edit destination dialog and the Remove confirmation
    /// when open.
    pub(crate) fn draw_stream_target_dialogs(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        self.stream_targets.reconcile(state);
        self.draw_stream_target_dialog(context, state, intents);
        self.draw_stream_remove_dialog(context, state, intents);
    }

    #[allow(clippy::too_many_lines)] // one modal dialog
    fn draw_stream_target_dialog(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let Some(mut draft) = self.stream_targets.dialog.take() else {
            forget_key_field(context);
            return;
        };
        let l = self.locale;
        let editing = draft.editing();
        let mut submit = false;
        let mut cancel = false;
        let closed = anchored_modal(
            context,
            "studio-stream-target-window",
            l.text(if editing {
                Msg::StreamDialogEditTitle
            } else {
                Msg::StreamDialogAddTitle
            }),
            DIALOG_WIDTH,
            |ui, body_height| {
                egui::ScrollArea::vertical()
                    .id_salt("studio-stream-target-body")
                    .max_height(body_height)
                    .auto_shrink([false, true])
                    .show(ui, |ui| draw_form(ui, l, state, &mut draft));
                let result = submission(&draft, state);
                ui.add_space(6.0);
                ui.separator();
                ui.horizontal_top(|ui| match result {
                    Err(blocked) => {
                        let (short, full) = footer_text(l, draft.protocol, blocked);
                        let (lamp, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                        paint_lamp(ui.painter(), lamp.center(), 4.0, Tone::Pending);
                        ui.add(
                            egui::Label::new(
                                RichText::new(short)
                                    .size(crate::theme::legible(10.5))
                                    .color(AMBER),
                            )
                            .wrap(),
                        )
                        .on_hover_text(full);
                    }
                    Ok(_) => {
                        ui.add(egui::Label::new(caption(l.text(Msg::StreamDialogNote))).wrap());
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // The submit key sits right of Cancel but is created
                        // after it, so Tab runs Cancel, then submit.
                        let slot = ui
                            .allocate_exact_size(Vec2::new(104.0, 26.0), Sense::hover())
                            .0;
                        if ui
                            .add(
                                Button::new(
                                    RichText::new(l.text(Msg::CancelKey))
                                        .size(12.0)
                                        .color(text_dim_color()),
                                )
                                .min_size(Vec2::new(84.0, 26.0)),
                            )
                            .on_hover_text(with_key(l, l.text(Msg::StreamDialogCancelHover), "Esc"))
                            .clicked()
                        {
                            cancel = true;
                        }
                        let (label, hover) = if editing {
                            (Msg::LayoutNameConfirm, Msg::StreamDialogSaveKeyHover)
                        } else {
                            (Msg::StreamDialogAddKey, Msg::StreamDialogAddKeyHover)
                        };
                        let key = ui
                            .add_enabled_ui(result.is_ok(), |ui| {
                                ui.put(
                                    slot,
                                    Button::new(
                                        RichText::new(l.text(label))
                                            .size(12.0)
                                            .strong()
                                            .color(text_color()),
                                    )
                                    .fill(ACCENT_FILL)
                                    .stroke(Stroke::new(1.0, accent_color()))
                                    .min_size(Vec2::new(104.0, 26.0)),
                                )
                            })
                            .inner
                            .hint(
                                l.text(hover),
                                result.as_ref().err().map_or("", |blocked| blocked.reason()),
                            );
                        if key.clicked() {
                            submit = true;
                        }
                    });
                });
            },
        );
        if submit && let Ok(intent) = submission(&draft, state) {
            intents.push(intent);
            forget_key_field(context);
            return;
        }
        if closed || cancel {
            forget_key_field(context);
        } else {
            self.stream_targets.dialog = Some(draft);
        }
    }

    #[allow(clippy::too_many_lines)] // one modal dialog
    fn draw_stream_remove_dialog(
        &mut self,
        context: &egui::Context,
        state: &StudioUiState,
        intents: &mut Vec<StudioIntent>,
    ) {
        let Some(confirm) = self.stream_targets.remove.clone() else {
            return;
        };
        let l = self.locale;
        let blocked = remove_block(state, confirm.target);
        let mut remove = false;
        let mut cancel = false;
        let question = l.format(
            Msg::StreamRemoveQuestion,
            &[
                ("name", Arg::Str(&confirm.name)),
                ("url", Arg::Str(&confirm.url)),
            ],
        );
        let closed = crate::menu::help_modal(
            context,
            "studio-stream-remove-window",
            l.text(Msg::StreamRemoveName),
            420.0,
            |ui| {
                ui.add(
                    egui::Label::new(RichText::new(question).size(13.0).color(text_color())).wrap(),
                );
                ui.add_space(4.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(l.text(Msg::StreamRemoveWarning))
                            .size(crate::theme::legible(11.0))
                            .color(text_dim_color()),
                    )
                    .wrap(),
                );
                ui.add_space(8.0);
                ui.separator();
                if let Some(reason) = blocked {
                    ui.horizontal_top(|ui| {
                        let (lamp, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                        paint_lamp(ui.painter(), lamp.center(), 4.0, Tone::Pending);
                        ui.add(
                            egui::Label::new(
                                RichText::new(l10n::unavailable(l, reason))
                                    .size(crate::theme::legible(10.5))
                                    .color(AMBER),
                            )
                            .wrap(),
                        );
                    });
                }
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Remove is created after Cancel, which takes the
                        // focus, so a stray Enter never removes.
                        let slot = ui
                            .allocate_exact_size(Vec2::new(96.0, 26.0), Sense::hover())
                            .0;
                        let cancel_key = ui
                            .add(
                                Button::new(
                                    RichText::new(l.text(Msg::CancelKey))
                                        .size(12.0)
                                        .color(text_dim_color()),
                                )
                                .min_size(Vec2::new(84.0, 26.0)),
                            )
                            .on_hover_text(with_key(
                                l,
                                l.text(Msg::StreamRemoveCancelHover),
                                "Esc",
                            ));
                        if cancel_key.clicked() {
                            cancel = true;
                        }
                        if ui.ctx().memory(|memory| memory.focused().is_none()) {
                            cancel_key.request_focus();
                        }
                        if ui
                            .add_enabled_ui(blocked.is_none(), |ui| {
                                ui.put(
                                    slot,
                                    Button::new(
                                        RichText::new(l.text(Msg::RemoveKey))
                                            .size(12.0)
                                            .strong()
                                            .color(text_color()),
                                    )
                                    .fill(PROGRAM_FILL)
                                    .stroke(Stroke::new(1.0, PROGRAM))
                                    .min_size(Vec2::new(96.0, 26.0)),
                                )
                            })
                            .inner
                            .hint(l.text(Msg::StreamRemoveKeyHover), blocked.unwrap_or(""))
                            .clicked()
                        {
                            remove = true;
                        }
                    });
                });
            },
        );
        if remove && blocked.is_none() {
            intents.push(StudioIntent::RemoveStreamTarget {
                target: confirm.target,
            });
            self.stream_targets.remove = None;
        } else if closed || cancel {
            self.stream_targets.remove = None;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use fm_protocol::{StreamRealizedState, StreamTargetEntry};
    use fm_types::OutputId;
    use fm_ui_model::{OutputStatus, StreamStatus};

    use super::*;
    use crate::StudioConnectionStatus;

    const SECRET: &str = "sk_live_TOPSECRET_42";
    const PASSPHRASE: &str = "srt-PASSPHRASE-secret";

    fn target(value: u128) -> WireStreamTargetId {
        WireStreamTargetId::new(NonZeroU128::new(value).unwrap())
    }

    fn output(value: u128) -> WireOutputId {
        WireOutputId::new(NonZeroU128::new(value).unwrap())
    }

    pub(crate) fn entry(
        value: u128,
        name: &str,
        protocol: StreamTargetProtocol,
    ) -> StreamTargetEntry {
        StreamTargetEntry {
            target: target(value),
            config: StreamTargetConfig {
                name: name.to_owned(),
                protocol,
                endpoint: if protocol == StreamTargetProtocol::Hls {
                    "/var/hls/show".to_owned()
                } else if protocol == StreamTargetProtocol::Srt {
                    "srt.example:9000".to_owned()
                } else {
                    "ingest.example/live".to_owned()
                },
                backup_endpoint: None,
                output: output(1),
                reconcile_on_startup: false,
                video_bitrate_kbps: 6_000,
                reconnect: StreamReconnect::DEFAULT,
                srt_latency_ms: (protocol == StreamTargetProtocol::Srt)
                    .then_some(DEFAULT_STREAM_TARGET_SRT_LATENCY_MS),
            },
            has_key: protocol.carries_key(),
            ladder_rungs: 0,
            has_passphrase: false,
        }
    }

    /// A ready operator session with one output and `targets` configured
    /// (stopped).
    pub(crate) fn fixture(targets: &[StreamTargetEntry]) -> StudioUiState {
        let mut view = crate::tests::recording_view([false, false]);
        view.outputs = vec![OutputStatus {
            output: OutputId::new(NonZeroU128::new(1).unwrap()),
            name: "Program".to_owned(),
        }];
        view.stream_targets = targets.to_vec();
        view.streams = targets
            .iter()
            .map(|entry| StreamStatus {
                target: entry.target.get(),
                name: entry.config.name.clone(),
                desired_running: false,
                realized: StreamRealizedState::Stopped,
                detail: None,
            })
            .collect();
        let mut state = StudioUiState::new(StudioConnectionStatus::Ready)
            .with_switcher_permissions(true, true)
            .with_edit_project_permission(true);
        state.streams.clone_from(&view.streams);
        state.view = Some(view);
        state
    }

    fn set_running(state: &mut StudioUiState, value: u128, running: bool) {
        for streams in [
            &mut state.streams,
            &mut state.view.as_mut().unwrap().streams,
        ] {
            for stream in streams.iter_mut() {
                if stream.target.get() == value {
                    stream.desired_running = running;
                }
            }
        }
    }

    #[test]
    fn edit_and_remove_are_refused_while_running_pending_or_without_permission() {
        let mut state = fixture(&[entry(1, "Twitch", StreamTargetProtocol::Rtmp)]);
        assert_eq!(edit_block(&state, target(1)), None);
        assert_eq!(remove_block(&state, target(1)), None);
        set_running(&mut state, 1, true);
        assert_eq!(
            edit_block(&state, target(1)),
            Some(Msg::ReasonStreamRunning.en())
        );
        assert_eq!(
            remove_block(&state, target(1)),
            Some(Msg::ReasonStreamRunning.en())
        );
        set_running(&mut state, 1, false);
        state.stream_pending = vec![target(1)];
        assert_eq!(
            edit_block(&state, target(1)),
            Some(Msg::ReasonStreamPending.en())
        );
        state.stream_pending.clear();
        state.can_edit_project = false;
        assert_eq!(
            remove_block(&state, target(1)),
            Some(Msg::ReasonNoEditProject.en())
        );
        state.can_edit_project = true;
        state.view.as_mut().unwrap().edit_locks.project = true;
        assert_eq!(
            edit_block(&state, target(1)),
            Some(Msg::ReasonProjectLocked.en())
        );
        state.view.as_mut().unwrap().edit_locks.project = false;
        // A roster entry whose settings have not arrived cannot be edited.
        assert_eq!(
            edit_block(&state, target(9)),
            Some(Msg::ReasonStreamNoConfig.en())
        );
        // Opening respects the gate.
        let mut ui = StreamTargetsUi::default();
        set_running(&mut state, 1, true);
        ui.open_edit(&state, target(1));
        assert!(ui.dialog.is_none());
        set_running(&mut state, 1, false);
        ui.open_edit(&state, target(1));
        assert_eq!(ui.dialog.as_ref().unwrap().name, "Twitch");
    }

    #[test]
    fn add_is_refused_at_five_destinations_and_ids_never_collide() {
        let four: Vec<_> = (1..=4)
            .map(|value| entry(value, &format!("T{value}"), StreamTargetProtocol::Rtmp))
            .collect();
        let mut state = fixture(&four);
        assert_eq!(add_block(&state), None);
        assert_eq!(next_target_id(&state), Some(target(5)));
        // An add in flight counts against the limit and its id is taken.
        state.stream_pending = vec![target(7)];
        assert_eq!(add_block(&state), Some(Msg::ReasonStreamTargetsFull.en()));
        assert_eq!(next_target_id(&state), Some(target(8)));
        state.stream_pending.clear();
        let five: Vec<_> = (1..=5)
            .map(|value| entry(value, &format!("T{value}"), StreamTargetProtocol::Rtmp))
            .collect();
        let full = fixture(&five);
        assert_eq!(add_block(&full), Some(Msg::ReasonStreamTargetsFull.en()));
        let mut ui = StreamTargetsUi::default();
        ui.open_add(Locale::ENGLISH, &full);
        assert!(ui.dialog.is_none());
        ui.open_add(Locale::ENGLISH, &state);
        let draft = ui.dialog.unwrap();
        assert_eq!(draft.target, target(5));
        assert_eq!(draft.output, Some(output(1)));
        let mut no_outputs = fixture(&[]);
        no_outputs.view.as_mut().unwrap().outputs.clear();
        assert_eq!(
            add_block(&no_outputs),
            Some(Msg::ReasonStreamNoOutputs.en())
        );
    }

    #[test]
    fn a_cue_naming_the_destination_blocks_its_removal_only() {
        let state = fixture(&[entry(1, "Twitch", StreamTargetProtocol::Rtmp)]);
        assert!(!used_by_cue(&state, target(1)));
        // Without replicated cue lists nothing blocks; the daemon still
        // refuses with `stream_in_use`.
        assert_eq!(remove_block(&state, target(1)), None);
    }

    fn new_draft(protocol: StreamTargetProtocol, endpoint: &str, key: &str) -> TargetDraft {
        let mut draft = TargetDraft::new(Locale::ENGLISH, target(3), Some(output(1)), 3);
        draft.set_protocol(protocol);
        draft.endpoint = endpoint.to_owned();
        draft.key = key.to_owned();
        draft
    }

    fn check(
        draft: &TargetDraft,
    ) -> Result<(StreamTargetConfig, SecretEdit), FieldProblem<TargetField>> {
        draft.checked(&["Twitch"], &[output(1)])
    }

    #[test]
    fn a_new_network_destination_needs_a_key_and_hls_takes_none() {
        let missing = check(&new_draft(
            StreamTargetProtocol::Rtmp,
            "ingest.example/live",
            "",
        ))
        .unwrap_err();
        assert_eq!(missing.field, TargetField::Key);
        assert!(missing.prompt);
        let (config, key) = check(&new_draft(
            StreamTargetProtocol::Rtmps,
            "ingest.example/live",
            SECRET,
        ))
        .unwrap();
        assert_eq!(key, SecretEdit::Set(SECRET.to_owned()));
        assert_eq!(config.display_url(), "rtmps://ingest.example/live");
        assert!(!config.display_url().contains(SECRET));
        // HLS: switching clears a typed key and the backup server.
        let mut hls = new_draft(StreamTargetProtocol::Rtmp, "/var/hls", SECRET);
        hls.backup = "backup.example/live".into();
        hls.set_protocol(StreamTargetProtocol::Hls);
        assert!(hls.key.is_empty() && hls.backup.is_empty());
        let (config, key) = check(&hls).unwrap();
        assert_eq!(key, SecretEdit::Keep);
        assert_eq!(config.backup_endpoint, None);
        // An SRT key cannot carry query separators.
        assert_eq!(
            check(&new_draft(
                StreamTargetProtocol::Srt,
                "srt.example:9000",
                "key&b"
            ))
            .unwrap_err()
            .reason,
            Msg::StreamErrKeySrt.en()
        );
        assert_eq!(
            check(&new_draft(
                StreamTargetProtocol::Rtmp,
                "ingest.example/live",
                "has space"
            ))
            .unwrap_err()
            .field,
            TargetField::Key
        );
    }

    #[test]
    fn editing_a_keyed_destination_keeps_its_key_unless_one_is_typed() {
        let stored = entry(1, "Twitch", StreamTargetProtocol::Rtmp);
        let state = fixture(core::slice::from_ref(&stored));
        let mut draft = TargetDraft::edit(&stored);
        assert_eq!(draft.key, "");
        // Nothing changed: Save is refused with a reason.
        assert_eq!(submission(&draft, &state), Err(Blocked::Unchanged));
        draft.bitrate_kbps = 8_000;
        let Ok(StudioIntent::SetStreamTarget {
            target: sent,
            config,
            stream_key,
            ..
        }) = submission(&draft, &state)
        else {
            panic!("an edited destination submits");
        };
        assert_eq!(sent, target(1));
        assert_eq!(config.video_bitrate_kbps, 8_000);
        assert_eq!(stream_key, SecretEdit::Keep);
        draft.key = SECRET.into();
        let Ok(StudioIntent::SetStreamTarget { stream_key, .. }) = submission(&draft, &state)
        else {
            panic!("a new key submits");
        };
        assert_eq!(stream_key, SecretEdit::Set(SECRET.into()));
        // Switching a keyed destination to HLS clears its key.
        draft.set_protocol(StreamTargetProtocol::Hls);
        draft.endpoint = "/var/hls".into();
        let Ok(StudioIntent::SetStreamTarget { stream_key, .. }) = submission(&draft, &state)
        else {
            panic!("an HLS edit submits");
        };
        assert_eq!(stream_key, SecretEdit::Clear);
        // An HLS destination becoming RTMP needs a key.
        let hls = entry(2, "Local", StreamTargetProtocol::Hls);
        let mut draft = TargetDraft::edit(&hls);
        draft.set_protocol(StreamTargetProtocol::Rtmp);
        draft.endpoint = "ingest.example/live".into();
        assert_eq!(check(&draft).unwrap_err().field, TargetField::Key);
        // The draft's debug form never shows a typed key.
        draft.key = SECRET.into();
        assert!(!format!("{draft:?}").contains(SECRET));
    }

    /// The form refuses every key `fm_model::StreamKey::parse` or the codec
    /// refuses, so Save is never enabled for a key the daemon rejects after
    /// the dialog closed (OR-006).
    #[test]
    fn keys_the_daemon_refuses_are_refused_in_the_form() {
        for refused in ["a/b-key", "abc", "***"] {
            let problem = check(&new_draft(
                StreamTargetProtocol::Rtmp,
                "ingest.example/live",
                refused,
            ))
            .unwrap_err();
            assert_eq!(problem.field, TargetField::Key, "{refused}");
            assert_eq!(problem.reason, Msg::StreamErrKeyChars.en(), "{refused}");
        }
        assert!(
            check(&new_draft(
                StreamTargetProtocol::Rtmp,
                "ingest.example/live",
                "abcd"
            ))
            .is_ok()
        );
    }

    /// An open Add dialog whose id another console takes meanwhile moves to
    /// a free id instead of reconfiguring (and overwriting the key of) that
    /// destination (OR-006).
    #[test]
    fn an_add_whose_id_is_taken_meanwhile_never_overwrites_that_destination() {
        let mut state = fixture(&[
            entry(1, "T1", StreamTargetProtocol::Rtmp),
            entry(2, "T2", StreamTargetProtocol::Rtmp),
        ]);
        let mut ui = StreamTargetsUi::default();
        ui.open_add(Locale::ENGLISH, &state);
        let mut draft = ui.dialog.clone().unwrap();
        assert_eq!(draft.target, target(3));
        draft.endpoint = "ingest.example/live".into();
        draft.key = SECRET.into();
        ui.dialog = Some(draft.clone());
        // Another console adds destination 3.
        let theirs = entry(3, "Theirs", StreamTargetProtocol::Rtmp);
        let view = state.view.as_mut().unwrap();
        view.stream_targets.push(theirs);
        view.streams.push(StreamStatus {
            target: target(3).get(),
            name: "Theirs".into(),
            desired_running: false,
            realized: StreamRealizedState::Stopped,
            detail: None,
        });
        state.streams.clone_from(&view.streams);
        assert!(matches!(submission(&draft, &state), Err(Blocked::Gate(_))));
        ui.reconcile(&state);
        let moved = ui.dialog.as_ref().unwrap();
        assert_eq!(moved.target, target(4));
        assert_eq!(moved.key, SECRET);
        let Ok(StudioIntent::SetStreamTarget { target: sent, .. }) = submission(moved, &state)
        else {
            panic!("the moved add submits");
        };
        assert_eq!(sent, target(4));
    }

    /// A new destination's default name is one no destination uses, so the
    /// dialog never opens already refusing its own name.
    #[test]
    fn a_new_destination_defaults_to_a_free_name() {
        let state = fixture(&[
            entry(1, "Destination 1", StreamTargetProtocol::Rtmp),
            entry(3, "Destination 3", StreamTargetProtocol::Rtmp),
        ]);
        let mut ui = StreamTargetsUi::default();
        ui.open_add(Locale::ENGLISH, &state);
        let mut draft = ui.dialog.unwrap();
        assert_eq!(draft.name, "Destination 4");
        draft.endpoint = "ingest.example/live".into();
        draft.key = SECRET.into();
        assert!(submission(&draft, &state).is_ok());
    }

    #[test]
    fn running_destinations_cannot_be_saved() {
        let stored = entry(1, "Twitch", StreamTargetProtocol::Rtmp);
        let mut state = fixture(core::slice::from_ref(&stored));
        let mut draft = TargetDraft::edit(&stored);
        draft.bitrate_kbps = 9_000;
        set_running(&mut state, 1, true);
        assert_eq!(
            submission(&draft, &state),
            Err(Blocked::Gate(Msg::ReasonStreamRunning.en()))
        );
    }

    #[test]
    fn form_problems_name_their_field_with_the_daemons_bounds() {
        let ok = new_draft(
            StreamTargetProtocol::Rtmp,
            "ingest.example/live",
            "live-key",
        );
        assert!(check(&ok).is_ok());
        let mut taken = ok.clone();
        taken.name = "TWITCH".into();
        assert_eq!(
            check(&taken).unwrap_err().reason,
            Msg::StreamErrNameTaken.en()
        );
        let mut blank = ok.clone();
        blank.name = "  ".into();
        assert!(check(&blank).unwrap_err().prompt);
        for (protocol, endpoint, reason) in [
            (
                StreamTargetProtocol::Rtmp,
                "ingest.example",
                Msg::StreamErrEndpointApp,
            ),
            (
                StreamTargetProtocol::Rtmp,
                "rtmp://x/live",
                Msg::StreamErrEndpointScheme,
            ),
            (
                StreamTargetProtocol::Rtmp,
                "u:p@x/live",
                Msg::StreamErrEndpointCredentials,
            ),
            (
                StreamTargetProtocol::Rtmp,
                "x/live?token",
                Msg::StreamErrEndpointQuery,
            ),
            (
                StreamTargetProtocol::Rtmp,
                "x//live",
                Msg::StreamErrEndpointSegments,
            ),
            (
                StreamTargetProtocol::Srt,
                "x:9000/app",
                Msg::StreamErrEndpointSrtPath,
            ),
            (
                StreamTargetProtocol::Srt,
                ":9000",
                Msg::StreamErrEndpointHost,
            ),
            (
                StreamTargetProtocol::Hls,
                "var/hls",
                Msg::StreamErrEndpointAbsolute,
            ),
            (
                StreamTargetProtocol::Hls,
                "/var/../etc",
                Msg::StreamErrEndpointSegments,
            ),
            (
                StreamTargetProtocol::Hls,
                "/var/hls dir",
                Msg::StreamErrEndpointChars,
            ),
        ] {
            let problem = check(&new_draft(protocol, endpoint, "k")).unwrap_err();
            assert_eq!(
                (problem.field, problem.reason),
                (TargetField::Endpoint, reason.en()),
                "{endpoint}"
            );
        }
        let mut backup = ok.clone();
        backup.backup = "backup.example".into();
        assert_eq!(check(&backup).unwrap_err().field, TargetField::Backup);
        let mut bitrate = ok.clone();
        bitrate.bitrate_kbps = 999;
        assert_eq!(check(&bitrate).unwrap_err().field, TargetField::Bitrate);
        let mut delays = ok.clone();
        delays.initial_delay_ms = 5_000;
        delays.max_delay_ms = 1_000;
        assert_eq!(check(&delays).unwrap_err().field, TargetField::Delays);
        let mut attempts = ok.clone();
        attempts.attempts = 0;
        assert_eq!(check(&attempts).unwrap_err().field, TargetField::Attempts);
        // Unlimited ignores the count and retries forever.
        attempts.unlimited = true;
        assert_eq!(check(&attempts).unwrap().0.reconnect.max_attempts, None);
        let mut no_output = ok;
        no_output.output = Some(output(9));
        assert_eq!(check(&no_output).unwrap_err().field, TargetField::Output);
    }

    fn painted_texts(output: &egui::FullOutput) -> Vec<String> {
        fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => out.push(text.galley.text().to_owned()),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut out);
        }
        out
    }

    /// A typed key's widget state, which holds its undo history, is dropped
    /// once the dialog closes, so no later dialog can undo back to it.
    #[test]
    fn a_closed_dialog_forgets_the_typed_key() {
        let stored = entry(1, "Twitch", StreamTargetProtocol::Rtmp);
        let state = fixture(core::slice::from_ref(&stored));
        let context = egui::Context::default();
        let mut shell = StudioShell::default();
        shell.stream_targets.dialog = Some(TargetDraft::edit(&stored));
        let pass = |shell: &mut StudioShell, events: Vec<egui::Event>| {
            context.begin_pass(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1_920.0, 1_080.0),
                )),
                events,
                ..egui::RawInput::default()
            });
            let mut ui = Ui::new(
                context.clone(),
                egui::Id::new("fm-stream-target-test"),
                egui::UiBuilder::new()
                    .layer_id(egui::LayerId::background())
                    .max_rect(context.viewport_rect()),
            );
            let _ = shell.draw(&mut ui, &state);
            drop(ui);
            let _ = context.end_pass();
        };
        pass(&mut shell, Vec::new());
        context.memory_mut(|memory| memory.request_focus(key_field_id()));
        pass(&mut shell, Vec::new());
        pass(&mut shell, vec![egui::Event::Text(SECRET.to_owned())]);
        assert_eq!(shell.stream_targets.dialog.as_ref().unwrap().key, SECRET);
        assert!(egui::text_edit::TextEditState::load(&context, key_field_id()).is_some());
        shell.stream_targets.dialog = None;
        pass(&mut shell, Vec::new());
        assert!(egui::text_edit::TextEditState::load(&context, key_field_id()).is_none());
    }

    /// The masked key paints a fixed run of dots whatever its length, and
    /// typing over a stored key swaps the keep caption for the replace one
    /// instead of leaving its slot empty.
    #[test]
    fn the_masked_key_hides_its_length_and_typing_keeps_a_caption() {
        let stored = entry(1, "Twitch", StreamTargetProtocol::Rtmp);
        let state = fixture(core::slice::from_ref(&stored));
        let draw = |key: &str| {
            let context = egui::Context::default();
            let mut shell = StudioShell::default();
            let mut draft = TargetDraft::edit(&stored);
            draft.key = key.to_owned();
            shell.stream_targets.dialog = Some(draft);
            let mut texts = Vec::new();
            for _ in 0..3 {
                context.begin_pass(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1_920.0, 1_080.0),
                    )),
                    ..egui::RawInput::default()
                });
                let mut ui = Ui::new(
                    context.clone(),
                    egui::Id::new("fm-stream-target-test"),
                    egui::UiBuilder::new()
                        .layer_id(egui::LayerId::background())
                        .max_rect(context.viewport_rect()),
                );
                let _ = shell.draw(&mut ui, &state);
                drop(ui);
                texts = painted_texts(&context.end_pass());
            }
            texts
        };
        for key in ["abcd", SECRET, &"k".repeat(200)] {
            let texts = draw(key);
            assert!(
                texts.iter().any(|text| text == KEY_MASK),
                "{key}: {texts:?}"
            );
            let dots = "\u{2022}".repeat(key.chars().count());
            assert!(
                key.chars().count() == 8 || !texts.contains(&dots),
                "{key}: the mask shows the key's length"
            );
            assert!(!texts.iter().any(|text| text.contains(key)), "{key}");
            assert!(
                texts
                    .iter()
                    .any(|text| text == Msg::StreamKeyReplaceCaption.en()),
                "{texts:?}"
            );
        }
        let texts = draw("");
        assert!(
            texts
                .iter()
                .any(|text| text == Msg::StreamKeyKeepCaption.en())
        );
        assert!(!texts.iter().any(|text| text == KEY_MASK));
    }

    /// With every tooltip shown and the Edit dialog open over a typed key,
    /// no painted label or tooltip, in either language, shows the key.
    #[test]
    fn no_painted_text_or_tooltip_shows_a_stream_key() {
        use fm_ui_model::i18n::Language;
        let stored = entry(1, "Twitch", StreamTargetProtocol::Rtmp);
        let mut state = fixture(&[stored.clone(), entry(2, "Local", StreamTargetProtocol::Hls)]);
        set_running(&mut state, 2, true);
        for language in [Language::English, Language::Spanish] {
            let context = egui::Context::default();
            context.memory_mut(|memory| memory.set_everything_is_visible(true));
            let mut shell = StudioShell::default();
            shell.set_locale(Locale::new(language));
            let mut draft = TargetDraft::edit(&stored);
            draft.key = SECRET.into();
            // An SRT edit shows the passphrase field too, typed over.
            draft.set_protocol(StreamTargetProtocol::Srt);
            draft.endpoint = "ingest.example:9000".into();
            draft.encrypt = true;
            draft.passphrase = PASSPHRASE.into();
            shell.stream_targets.dialog = Some(draft);
            let mut texts = Vec::new();
            for _ in 0..4 {
                context.begin_pass(egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1_920.0, 1_080.0),
                    )),
                    ..egui::RawInput::default()
                });
                let mut ui = Ui::new(
                    context.clone(),
                    egui::Id::new("fm-stream-target-test"),
                    egui::UiBuilder::new()
                        .layer_id(egui::LayerId::background())
                        .max_rect(context.viewport_rect()),
                );
                let _ = shell.draw(&mut ui, &state);
                drop(ui);
                texts = painted_texts(&context.end_pass());
            }
            let all = texts.join("\n");
            assert!(
                all.contains("rtmp://ingest.example/live"),
                "{language:?}: the destination URL is shown"
            );
            assert!(
                !all.contains(PASSPHRASE),
                "{language:?}: a passphrase was painted"
            );
            assert!(!all.contains(SECRET), "{language:?}: a key was painted");
            assert!(
                !all.contains("TOPSECRET"),
                "{language:?}: a key was painted"
            );
        }
    }

    /// An SRT draft sends its latency; Encrypt with a typed passphrase sets
    /// it, an edit keeps a stored one unless one is typed, and unticking
    /// Encrypt clears it. The form refuses what the daemon would.
    #[test]
    fn srt_drafts_send_their_latency_and_a_secret_passphrase() {
        let mut draft = new_draft(StreamTargetProtocol::Srt, "srt.example:9000", "stream-id");
        let (config, _) = check(&draft).unwrap();
        assert_eq!(
            config.srt_latency_ms,
            Some(DEFAULT_STREAM_TARGET_SRT_LATENCY_MS)
        );
        assert_eq!(draft.passphrase_edit(), Ok(SecretEdit::Keep));
        draft.srt_latency_ms = 19;
        assert_eq!(check(&draft).unwrap_err().field, TargetField::Latency);
        draft.srt_latency_ms = 400;
        draft.encrypt = true;
        let missing = check(&draft).unwrap_err();
        assert_eq!(
            (missing.field, missing.prompt),
            (TargetField::Passphrase, true)
        );
        for refused in ["short", "has a space inside", "***"] {
            draft.passphrase = refused.into();
            assert_eq!(check(&draft).unwrap_err().field, TargetField::Passphrase);
        }
        draft.passphrase = PASSPHRASE.into();
        assert_eq!(check(&draft).unwrap().0.srt_latency_ms, Some(400));
        assert_eq!(
            draft.passphrase_edit(),
            Ok(SecretEdit::Set(PASSPHRASE.into()))
        );
        assert!(!format!("{draft:?}").contains(PASSPHRASE));
        // Moving off SRT drops the typed passphrase and the latency.
        draft.set_protocol(StreamTargetProtocol::Rtmp);
        draft.endpoint = "ingest.example/live".into();
        assert_eq!(draft.passphrase, "");
        assert_eq!(check(&draft).unwrap().0.srt_latency_ms, None);

        // Editing an encrypted SRT destination.
        let mut stored = entry(1, "Relay", StreamTargetProtocol::Srt);
        stored.has_passphrase = true;
        let state = fixture(core::slice::from_ref(&stored));
        let mut draft = TargetDraft::edit(&stored);
        assert!(draft.encrypt);
        assert_eq!(submission(&draft, &state), Err(Blocked::Unchanged));
        draft.passphrase = PASSPHRASE.into();
        let Ok(StudioIntent::SetStreamTarget { srt_passphrase, .. }) = submission(&draft, &state)
        else {
            panic!("a new passphrase submits");
        };
        assert_eq!(srt_passphrase, SecretEdit::Set(PASSPHRASE.into()));
        draft.passphrase.clear();
        draft.encrypt = false;
        let Ok(StudioIntent::SetStreamTarget {
            srt_passphrase,
            stream_key,
            ..
        }) = submission(&draft, &state)
        else {
            panic!("clearing the passphrase submits");
        };
        assert_eq!(
            (srt_passphrase, stream_key),
            (SecretEdit::Clear, SecretEdit::Keep)
        );
    }
}
