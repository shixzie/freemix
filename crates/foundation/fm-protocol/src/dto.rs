use core::{
    fmt,
    num::{NonZeroU32, NonZeroU128},
};

use fm_command::{CommandEnvelope, Deadline, Revision, StateEpoch};
use fm_grade::{CubeKind, Grade};
use fm_types::{FrameRate, InputId, OutputId};

use crate::ProtocolVersion;

/// Stable identity of one project's durable state on one server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerIdentity {
    pub engine_id: String,
    pub project_id: String,
    pub state_epoch: u64,
    pub log_id: String,
}

impl ServerIdentity {
    #[must_use]
    pub const fn domain_state_epoch(&self) -> StateEpoch {
        StateEpoch::new(self.state_epoch)
    }
}

/// The last durable revision completely applied by a client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResumeCursor {
    pub server: ServerIdentity,
    pub revision: u64,
}

impl ResumeCursor {
    #[must_use]
    pub const fn domain_revision(&self) -> Revision {
        Revision::new(self.revision)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineIdentity {
    pub engine_id: String,
    pub state_epoch: u64,
    pub log_id: String,
}

impl EngineIdentity {
    #[must_use]
    pub const fn domain_state_epoch(&self) -> StateEpoch {
        StateEpoch::new(self.state_epoch)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventCursor {
    pub engine: EngineIdentity,
    pub revision: u64,
}

impl EventCursor {
    #[must_use]
    pub const fn domain_revision(&self) -> Revision {
        Revision::new(self.revision)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientType {
    Studio,
    Web,
    Cli,
    Integration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Viewer,
    Graphics,
    Audio,
    Replay,
    Operator,
    Admin,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerHello {
    pub protocol: ProtocolVersion,
    pub granted_role: Role,
    pub permissions: Vec<String>,
    pub capabilities_digest: String,
    pub engine: EngineIdentity,
    pub current_revision: u64,
    pub resume: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandshakeRequest {
    pub protocol: ProtocolVersion,
    pub build: String,
    pub client_type: ClientType,
    pub desired_role: Role,
    pub resume_cursor: Option<ResumeCursor>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotReason {
    NoCursor,
    IdentityChanged,
    CursorAhead,
    HistoryUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HandshakeOutcome {
    Snapshot { reason: SnapshotReason },
    Resume { cursor: ResumeCursor },
    Rejected { error: StructuredError },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandshakeResponse {
    pub protocol: ProtocolVersion,
    pub granted_role: Role,
    pub permissions: Vec<String>,
    pub capabilities: CapabilityReportSummary,
    pub server: ServerIdentity,
    pub current_revision: u64,
    pub outcome: HandshakeOutcome,
}

/// Chooses snapshot or resume from the retained durable log range.
///
/// `available_from_revision` is the first revision still available for replay.
#[must_use]
pub fn choose_handshake_outcome(
    server: &ServerIdentity,
    current_revision: u64,
    available_from_revision: u64,
    cursor: Option<&ResumeCursor>,
) -> HandshakeOutcome {
    let Some(cursor) = cursor else {
        return HandshakeOutcome::Snapshot {
            reason: SnapshotReason::NoCursor,
        };
    };
    if cursor.server != *server {
        return HandshakeOutcome::Snapshot {
            reason: SnapshotReason::IdentityChanged,
        };
    }
    if cursor.revision > current_revision {
        return HandshakeOutcome::Snapshot {
            reason: SnapshotReason::CursorAhead,
        };
    }
    if cursor.revision.saturating_add(1) < available_from_revision {
        return HandshakeOutcome::Snapshot {
            reason: SnapshotReason::HistoryUnavailable,
        };
    }
    HandshakeOutcome::Resume {
        cursor: cursor.clone(),
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireInputId(NonZeroU128);

impl WireInputId {
    #[must_use]
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }

    #[must_use]
    pub const fn from_domain(value: InputId) -> Self {
        Self(value.get())
    }

    #[must_use]
    pub const fn to_domain(self) -> InputId {
        InputId::new(self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireOutputId(NonZeroU128);

impl WireOutputId {
    #[must_use]
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }

    #[must_use]
    pub const fn from_domain(value: OutputId) -> Self {
        Self(value.get())
    }

    #[must_use]
    pub const fn to_domain(self) -> OutputId {
        OutputId::new(self.0)
    }
}

impl fmt::Display for WireOutputId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Stable identity of one configurable stream target carried on the wire.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WireStreamTargetId(NonZeroU128);

impl WireStreamTargetId {
    #[must_use]
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }
}

impl fmt::Display for WireStreamTargetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Stable one-based downstream overlay channel number carried on the wire.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireOverlayChannelId(u8);

impl WireOverlayChannelId {
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number <= 8 {
            Some(Self(number))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }
}

impl fmt::Display for WireOverlayChannelId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl fmt::Display for WireInputId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Stable one-based Stinger slot number carried on the wire.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireStingerSlotId(u8);

impl WireStingerSlotId {
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number <= 8 {
            Some(Self(number))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }
}

impl fmt::Display for WireStingerSlotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One independently controllable program recorder, numbered 1..=2.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WireRecorderSlot(u8);

impl WireRecorderSlot {
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value >= 1 && value <= 2 {
            Some(Self(value))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl fmt::Display for WireRecorderSlot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualTransitionKind {
    Fade,
    Wipe,
    AlphaFade,
    Slide,
}

/// Exact normalized manual-transition position expressed in basis points.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ManualTransitionPosition(u16);

impl ManualTransitionPosition {
    pub const MAX: u16 = 10_000;
    pub const START: Self = Self(0);
    pub const END: Self = Self(Self::MAX);

    #[must_use]
    pub const fn new(basis_points: u16) -> Option<Self> {
        if basis_points <= Self::MAX {
            Some(Self(basis_points))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn basis_points(self) -> u16 {
        self.0
    }
}

/// Exact active manual-transition state at one frame boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManualTransitionState {
    pub kind: ManualTransitionKind,
    pub from: WireInputId,
    pub to: WireInputId,
    pub interval_start: ManualTransitionPosition,
    pub position: ManualTransitionPosition,
}

/// Versioned additive projection of manual-transition state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualTransitionStatus {
    Inactive,
    Active(ManualTransitionState),
}

/// Exact fixed-rational FTB position; `0` is live and `u16::MAX` is black.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct FadeToBlackPosition(u16);

impl FadeToBlackPosition {
    pub const LIVE: Self = Self(0);
    pub const BLACK: Self = Self(u16::MAX);
    pub const DENOMINATOR: u32 = u16::MAX as u32;

    #[must_use]
    pub const fn new(numerator: u16) -> Self {
        Self(numerator)
    }

    #[must_use]
    pub const fn numerator(self) -> u16 {
        self.0
    }
}

/// Desired or realized Fade-to-Black state at one frame boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FadeToBlackState {
    pub target_active: bool,
    pub position: FadeToBlackPosition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StingerAudioPolicy {
    Muted,
    StingerOnly,
    MixWithProgram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StingerMissingMediaFallback {
    Cut,
    Fade,
    KeepProgram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StingerReadiness {
    NotRequested,
    Ready,
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StingerStatus {
    pub slot: WireStingerSlotId,
    pub media_input: WireInputId,
    pub preload: bool,
    pub cut_point_frames: u32,
    pub audio_policy: StingerAudioPolicy,
    pub missing_media_fallback: StingerMissingMediaFallback,
    pub readiness: StingerReadiness,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlayStatus {
    pub channel: WireOverlayChannelId,
    pub source: Option<WireInputId>,
    pub active: bool,
    pub opacity: u8,
    pub transition: OverlayTransitionKind,
    pub duration_frames: u32,
    pub position: OverlayPositionPreset,
    pub border: OverlayBorderPreset,
    pub queued_sources: Vec<WireInputId>,
    pub included_outputs: Vec<WireOutputId>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverlayTransitionKind {
    #[default]
    Cut,
    Fade,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverlayPositionPreset {
    #[default]
    FullFrame,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverlayBorderPreset {
    #[default]
    None,
    ThinWhite,
    ThickWhite,
}

impl OverlayStatus {
    /// Builds the complete empty state required by the current eight-channel contract.
    #[must_use]
    pub fn empty_channels() -> Vec<Self> {
        (1..=8)
            .filter_map(|channel| {
                WireOverlayChannelId::new(channel).map(|channel| Self {
                    channel,
                    source: None,
                    active: false,
                    opacity: 0,
                    transition: OverlayTransitionKind::Cut,
                    duration_frames: 1,
                    position: OverlayPositionPreset::FullFrame,
                    border: OverlayBorderPreset::None,
                    queued_sources: Vec::new(),
                    included_outputs: Vec::new(),
                })
            })
            .collect()
    }
}

/// Realized runtime state of one stream target at one frame boundary.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StreamRealizedState {
    #[default]
    Stopped,
    Starting,
    Live,
    WaitingToReconnect,
    Congested,
    Failed,
    Unavailable,
}

/// One canonical stream target exposed to clients with its desired and
/// realized runtime state. `detail` is a short redacted human string.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamStatus {
    pub target: WireStreamTargetId,
    pub name: String,
    pub desired_running: bool,
    pub realized: StreamRealizedState,
    pub detail: Option<String>,
}

/// Desired recording state of one program recorder at one snapshot revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecordingStatus {
    pub recorder: WireRecorderSlot,
    pub desired_active: bool,
}

/// Largest undo or redo depth carried on the wire; equals the engine's
/// retained edit-history bound.
pub const MAX_EDIT_HISTORY_DEPTH: u16 = 100;

/// The padlock target of an edit lock.
///
/// Wire encoding: `scope=project`, or `scope=input` plus `input=<id>`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum EditLockScope {
    /// Locks every project edit, including undo and redo.
    Project,
    /// Locks edits that change one input's name or position.
    Input(WireInputId),
}

/// Engaged edit padlocks at one revision.
///
/// `inputs` is in strictly ascending identifier order and names only inputs
/// of the same snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EditLockStatus {
    pub project: bool,
    pub inputs: Vec<WireInputId>,
}

impl EditLockStatus {
    /// Returns whether `input` has its own padlock engaged.
    #[must_use]
    pub fn is_input_locked(&self, input: WireInputId) -> bool {
        self.inputs.contains(&input)
    }
}

/// The server's shared undo/redo history after one revision.
///
/// History is session state on the server: it is never persisted, and a
/// daemon restart starts it empty. `next_undo`/`next_redo` carry the
/// revision at which the top entry's edit was first accepted, which is the
/// value a client passes as `expected_edit` to refuse reverting someone
/// else's later edit. Each is present exactly when its depth is nonzero.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EditHistoryStatus {
    pub undo_depth: u16,
    pub next_undo: Option<u64>,
    pub redo_depth: u16,
    pub next_redo: Option<u64>,
}

impl EditHistoryStatus {
    /// No undoable and no redoable edit.
    pub const EMPTY: Self = Self {
        undo_depth: 0,
        next_undo: None,
        redo_depth: 0,
        next_redo: None,
    };

    #[must_use]
    pub const fn can_undo(&self) -> bool {
        self.undo_depth > 0
    }

    #[must_use]
    pub const fn can_redo(&self) -> bool {
        self.redo_depth > 0
    }
}

/// The source an input may be added with while the show is live.
///
/// These are exactly the generators the simulated daemon renders; every other
/// input kind (media files, capture devices, network sources, Scenes,
/// aliases, mix feeds) is added offline. Wire encoding: `kind=solid` plus
/// `color=<rrggbbaa>` (eight lowercase hex digits), or `kind=bars`. Audio is
/// always silent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LiveInputSource {
    Solid {
        red: u8,
        green: u8,
        blue: u8,
        alpha: u8,
    },
    Bars,
}

/// Most fields one title carries; equals the title domain bound.
pub const MAX_TITLE_FIELDS: usize = 128;
/// Longest text field value, in Unicode scalar values.
pub const MAX_TITLE_TEXT_CHARS: usize = 4_096;
/// Largest total of text values plus image asset URIs in one field list, in
/// UTF-8 bytes.
pub const MAX_TITLE_CONTENT_BYTES: usize = 12_288;
/// Largest title (template) display name carried by `title_state`, in UTF-8
/// bytes.
pub const MAX_TITLE_NAME_BYTES: usize = 256;
/// Largest total of every field name one `title_state` carries, in UTF-8
/// bytes.
pub const MAX_TITLE_FIELD_NAMES_BYTES: usize = 4_096;

/// Stable identity of one title template field carried on the wire.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WireTitleFieldId(NonZeroU128);

impl WireTitleFieldId {
    #[must_use]
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }
}

impl fmt::Display for WireTitleFieldId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One typed title field value.
///
/// Inside a field list a value is `t:<len>:<text>`, `n:<i64>`,
/// `c:<rrggbbaa>`, or `i:<len>:<asset uri>`, where `<len>` is the canonical
/// decimal UTF-8 byte length of the raw text that follows (see
/// `docs/spec/server-client-protocol.md`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TitleFieldValue {
    Text(String),
    Number(i64),
    /// A straight (not premultiplied) RGBA color.
    Color {
        red: u8,
        green: u8,
        blue: u8,
        alpha: u8,
    },
    /// A nonblank image asset URI.
    Image {
        asset_uri: String,
    },
}

impl TitleFieldValue {
    /// UTF-8 bytes this value contributes to [`MAX_TITLE_CONTENT_BYTES`].
    #[must_use]
    pub fn content_bytes(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Image { asset_uri } => asset_uri.len(),
            Self::Number(_) | Self::Color { .. } => 0,
        }
    }
}

/// One title field and its value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleFieldUpdate {
    pub field: WireTitleFieldId,
    pub value: TitleFieldValue,
}

/// Whether a `title_fields` edit requires its title to be off air (prepare)
/// or on air (live update). Wire encoding: `require=off_air|on_air`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitleAirRequirement {
    OffAir,
    OnAir,
}

/// One title input's field revision at a snapshot's revision. The values
/// follow in that title's `title_state` record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TitleSummary {
    pub input: WireInputId,
    pub field_revision: u64,
}

/// One title field: its template name and current value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleFieldStatus {
    pub field: WireTitleFieldId,
    pub name: String,
    pub value: TitleFieldValue,
}

/// Where a title is in its on-air animation (Protocol 2.27). Wire encoding:
/// `phase=idle|in|hold|out`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitlePhase {
    /// Not taken to air with a title take (it may still be on air another
    /// way, for example on Program, where it shows its Hold pose).
    Idle,
    /// Playing its In phase.
    In,
    /// On air, holding the In phase's last frame.
    Hold,
    /// Playing its Out phase; its overlay channel goes off when it ends.
    Out,
}

/// The complete field state of one title input at durable `revision`, and
/// its animation phase at the snapshot's frame cursor.
///
/// A session writes exactly one `title_state` per [`TitleSummary`] of a
/// snapshot, in summary order, immediately after that snapshot and before any
/// event. `fields` is complete and strictly ascending by field id.
///
/// `phase_frame` and `phase_frames` give In/Out progress in project frames:
/// `phase_frame < phase_frames` while In or Out plays, and both are zero
/// while idle or holding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleStateMessage {
    pub revision: u64,
    pub input: WireInputId,
    pub name: String,
    pub field_revision: u64,
    pub phase: TitlePhase,
    pub phase_frame: u64,
    pub phase_frames: u64,
    pub fields: Vec<TitleFieldStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandPayload {
    ReorderInputs {
        inputs: Vec<WireInputId>,
    },
    RenameInput {
        input: WireInputId,
        name: String,
    },
    /// Appends a new input at the end of the input order with a default
    /// Master strip. The client chooses the new, unused identifier.
    AddInput {
        input: WireInputId,
        name: String,
        source: LiveInputSource,
    },
    /// Removes an input nothing on air or configured references.
    RemoveInput {
        input: WireInputId,
    },
    SetInputAudioStrip {
        input: WireInputId,
        gain_millidb: i32,
        balance_basis_points: i32,
        muted: bool,
        soloed: bool,
        follow_video: bool,
        delay_samples: u32,
    },
    SelectPreview {
        input: WireInputId,
    },
    Cut,
    Fade {
        duration_frames: u32,
    },
    AlphaFade {
        duration_frames: u32,
    },
    Slide {
        duration_frames: u32,
    },
    Zoom {
        duration_frames: u32,
    },
    Stinger {
        slot: WireStingerSlotId,
        duration_frames: u32,
    },
    ConfigureStinger {
        slot: WireStingerSlotId,
        media_input: WireInputId,
        preload: bool,
        cut_point_frames: u32,
        audio_policy: StingerAudioPolicy,
        missing_media_fallback: StingerMissingMediaFallback,
    },
    RemoveStinger {
        slot: WireStingerSlotId,
    },
    TakeOverlay {
        channel: WireOverlayChannelId,
        source: WireInputId,
    },
    UpdateOverlay {
        channel: WireOverlayChannelId,
        source: WireInputId,
    },
    OverlayOff {
        channel: WireOverlayChannelId,
    },
    SetOverlayOutputInclusion {
        channel: WireOverlayChannelId,
        output: WireOutputId,
        included: bool,
    },
    ConfigureOverlayTransition {
        channel: WireOverlayChannelId,
        transition: OverlayTransitionKind,
        duration_frames: u32,
    },
    ConfigureOverlayAppearance {
        channel: WireOverlayChannelId,
        position: OverlayPositionPreset,
        border: OverlayBorderPreset,
    },
    QueueOverlay {
        channel: WireOverlayChannelId,
        source: WireInputId,
    },
    TakeNextOverlay {
        channel: WireOverlayChannelId,
    },
    Wipe {
        duration_frames: u32,
    },
    FadeToBlack {
        active: bool,
        duration_frames: u32,
    },
    StartManualTransition {
        kind: ManualTransitionKind,
    },
    SetManualTransitionPosition {
        position: ManualTransitionPosition,
    },
    CommitManualTransition,
    CancelManualTransition,
    StreamStart {
        target: WireStreamTargetId,
    },
    StreamStop {
        target: WireStreamTargetId,
    },
    RecordStart {
        recorder: WireRecorderSlot,
    },
    RecordStop {
        recorder: WireRecorderSlot,
    },
    /// Reverts the server's most recent undoable project edit.
    ///
    /// `expected_edit` names the edit revision the caller means to revert
    /// ([`EditHistoryStatus::next_undo`]); another top entry is refused.
    Undo {
        expected_edit: Option<u64>,
    },
    /// Reapplies the server's most recently undone project edit.
    Redo {
        expected_edit: Option<u64>,
    },
    /// Engages or releases one edit padlock. Live switching is never locked.
    SetEditLock {
        scope: EditLockScope,
        locked: bool,
    },
    /// Atomically sets title field values when `base_revision` is the
    /// title's current field revision and its air state meets `require`.
    SetTitleFields {
        input: WireInputId,
        base_revision: u64,
        require: TitleAirRequirement,
        fields: Vec<TitleFieldUpdate>,
    },
    /// Applies field values to an off-air title and takes it onto `channel`
    /// in the same revision.
    TakeTitle {
        input: WireInputId,
        channel: WireOverlayChannelId,
        base_revision: u64,
        fields: Vec<TitleFieldUpdate>,
    },
    /// Takes a title off the single overlay channel that shows it.
    TitleOffAir {
        input: WireInputId,
    },
    /// Replaces the playlist's entries and loop mode while it is stopped or
    /// finished. A project edit.
    SetPlaylist {
        loop_mode: PlaylistLoopMode,
        entries: Vec<PlaylistEntrySpec>,
    },
    /// Controls the playlist on the command's target frame.
    PlaylistControl {
        operation: PlaylistOperation,
    },
    /// The authority's playlist clock: takes the entry due on `frame` to
    /// Program on exactly that frame. Accepted only for the frame the
    /// playlist is due on (or the next frame when that has passed).
    PlaylistAdvance {
        frame: u64,
    },
    /// Sets (`Some`) or clears (`None`) one input's grade (Protocol 2.28).
    /// Wire payloads `grade_input_set` and `grade_input_clear`.
    SetInputGrade {
        input: WireInputId,
        source: Option<WireGradeSource>,
    },
    /// Sets (`Some`) or clears (`None`) the program output grade. Wire
    /// payloads `grade_program_set` and `grade_program_clear`.
    SetProgramGrade {
        source: Option<WireGradeSource>,
    },
    /// Adds a named grade to the library at `index` (the end when absent).
    AddLibraryGrade {
        grade: NonZeroU32,
        name: String,
        value: Box<Grade>,
        index: Option<u16>,
    },
    /// Renames a library grade and replaces its values.
    UpdateLibraryGrade {
        grade: NonZeroU32,
        name: String,
        value: Box<Grade>,
    },
    /// Removes a library grade no input or the program uses.
    RemoveLibraryGrade {
        grade: NonZeroU32,
    },
    /// Imports `.cube` text, at most [`MAX_GRADE_LUT_IMPORT_BYTES`], as LUT
    /// `lut`. The authority parses it; kind and size come from the text.
    ImportGradeLut {
        lut: NonZeroU32,
        name: String,
        data: String,
    },
    /// Unregisters a LUT no grade uses. Not undoable.
    RemoveGradeLut {
        lut: NonZeroU32,
    },
}

/// Largest `.cube` text one `grade_lut_import` carries, in bytes. The text
/// travels base64url-encoded (unpadded) in one field, so the bound keeps the
/// encoded field within [`crate::MAX_FIELD_VALUE_BYTES`]. Larger LUTs are
/// imported into the project bundle offline (for example with the CLI).
pub const MAX_GRADE_LUT_IMPORT_BYTES: usize = 32 * 1024;
/// Longest grade or LUT name, in UTF-8 bytes.
pub const MAX_GRADE_NAME_BYTES: usize = 128;
/// Most `grade_state` records one snapshot announces: every LUT, library
/// grade, input grade, and the program grade of a maximal project.
pub const MAX_GRADE_STATES: u16 = 64 + 256 + 256 + 1;

/// The grade one input or the program carries on the wire.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireGradeSource {
    /// A grade from the project library (`library=<grade id>`).
    Library(NonZeroU32),
    /// A grade owned by its target alone (`grade=<grade>`).
    Inline(Box<Grade>),
}

/// One item of the project grading, as carried by a `grade_state` record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GradeEntry {
    /// An imported LUT: its id, name, kind, and size (edge length for 3D,
    /// entry count for 1D).
    Lut {
        lut: NonZeroU32,
        name: String,
        kind: CubeKind,
        size: u32,
    },
    /// A named library grade.
    Library {
        grade: NonZeroU32,
        name: String,
        value: Box<Grade>,
    },
    /// The grade one input carries.
    Input {
        input: WireInputId,
        source: WireGradeSource,
    },
    /// The program output grade.
    Program { source: WireGradeSource },
}

/// One grading item at a snapshot's `revision` (Protocol 2.28).
///
/// A session writes exactly [`SnapshotMessage::grade_states`] of these after
/// the snapshot's title states and playlist state and before any event:
/// every LUT in import order, then every library grade in library order,
/// then every graded input in ascending input order, then the program grade
/// when there is one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GradeStateMessage {
    pub revision: u64,
    pub entry: GradeEntry,
}

/// What one accepted grade edit changed (`grade_changed`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GradeChange {
    /// A LUT was imported; it is the last LUT.
    LutImported {
        lut: NonZeroU32,
        name: String,
        kind: CubeKind,
        size: u32,
    },
    LutRemoved {
        lut: NonZeroU32,
    },
    /// A library grade was added at `index`, or (keeping its index) renamed
    /// and updated. `index` is its position in the library afterwards.
    Library {
        grade: NonZeroU32,
        index: u16,
        name: String,
        value: Box<Grade>,
    },
    LibraryRemoved {
        grade: NonZeroU32,
    },
    /// An input's grade afterwards; `None` when it is ungraded.
    Input {
        input: WireInputId,
        source: Option<WireGradeSource>,
    },
    /// The program grade afterwards; `None` when it is ungraded.
    Program {
        source: Option<WireGradeSource>,
    },
}

/// Most entries one playlist may hold; one `entries` list fits one record.
pub const MAX_PLAYLIST_ENTRIES: usize = 256;
/// Longest playlist entry in frames.
pub const MAX_PLAYLIST_ENTRY_FRAMES: u64 = 7 * 24 * 60 * 60 * 1000;
/// Longest playlist transition in frames.
pub const MAX_PLAYLIST_TRANSITION_FRAMES: u32 = 3_600;

/// What happens when the on-air playlist entry ends.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistLoopMode {
    Off,
    All,
    One,
}

/// The Program transition that brings a playlist entry on air.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistTransitionKind {
    Cut,
    Fade,
    AlphaFade,
    Wipe,
    Slide,
    Zoom,
}

/// One playlist row. Durations are in frames; a cut has zero transition
/// frames and every other kind between one and
/// [`MAX_PLAYLIST_TRANSITION_FRAMES`], no longer than the entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PlaylistEntrySpec {
    pub id: u64,
    pub input: WireInputId,
    pub duration_frames: u64,
    pub transition: PlaylistTransitionKind,
    pub transition_frames: u32,
    pub enabled: bool,
}

/// When a started playlist puts its first entry on air.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistStart {
    /// On the command's target frame.
    Now,
    /// This many frames after the command's target frame.
    AfterFrames(u64),
    /// On this absolute frame.
    AtFrame(u64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistOperation {
    Start {
        at: PlaylistStart,
        from: Option<u64>,
    },
    Stop,
    Pause,
    Resume,
    Next,
    Previous,
    Jump {
        entry: u64,
    },
}

/// The playlist's automation state. Frames are engine frame numbers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaylistRunState {
    Stopped,
    Armed {
        start_frame: u64,
        from: Option<u64>,
    },
    Playing {
        entry: u64,
        take: u64,
        cycle: u64,
        started_frame: u64,
    },
    Paused {
        entry: u64,
        take: u64,
        cycle: u64,
        elapsed_frames: u64,
    },
    Finished {
        last: Option<u64>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandMessage {
    pub protocol: ProtocolVersion,
    pub id: String,
    pub idempotency_key: String,
    pub expected_revision: Option<u64>,
    pub deadline_ms: Option<u64>,
    pub payload: CommandPayload,
}

impl CommandMessage {
    /// Builds the transport-neutral domain envelope after the adapter converts
    /// the wire payload to its domain command.
    #[must_use]
    pub fn domain_envelope<C>(&self, command: C) -> CommandEnvelope<C> {
        let mut envelope =
            CommandEnvelope::new(self.id.clone(), self.idempotency_key.clone(), command);
        if let Some(revision) = self.expected_revision {
            envelope = envelope.expecting(Revision::new(revision));
        }
        if let Some(deadline) = self.deadline_ms {
            envelope = envelope.with_deadline(Deadline::from_millis(deadline));
        }
        envelope
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldIssue {
    pub field: String,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuredError {
    pub code: String,
    pub message: String,
    pub fields: Vec<FieldIssue>,
    pub retryable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ErrorMessage {
    pub request_id: Option<String>,
    pub current_revision: Option<u64>,
    pub error: StructuredError,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandResult {
    Accepted {
        id: String,
        revision: u64,
        scheduled_frame: Option<u64>,
    },
    Rejected {
        id: String,
        code: String,
        message: String,
        fields: Vec<FieldIssue>,
        current_revision: u64,
        retryable: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputAudioStripStatus {
    pub input: WireInputId,
    pub gain_millidb: i32,
    pub balance_basis_points: i32,
    pub muted: bool,
    pub soloed: bool,
    pub follow_video: bool,
    pub delay_samples: u32,
}

/// One canonical show input exposed to clients in project order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputStatus {
    pub input: WireInputId,
    pub name: String,
}

/// One canonical show output exposed to clients in project order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputStatus {
    pub output: WireOutputId,
    pub name: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SnapshotMessage {
    pub engine: EngineIdentity,
    pub revision: u64,
    pub show_name: String,
    pub inputs: Vec<InputStatus>,
    pub outputs: Vec<OutputStatus>,
    pub input_audio_strips: Vec<InputAudioStripStatus>,
    pub desired_program: WireInputId,
    pub desired_preview: WireInputId,
    pub realized_program: WireInputId,
    pub realized_preview: WireInputId,
    pub desired_manual_transition: ManualTransitionStatus,
    pub realized_manual_transition: ManualTransitionStatus,
    pub desired_fade_to_black: FadeToBlackState,
    pub realized_fade_to_black: FadeToBlackState,
    pub stingers: Vec<StingerStatus>,
    pub streams: Vec<StreamStatus>,
    pub recordings: Vec<RecordingStatus>,
    pub desired_overlays: Vec<OverlayStatus>,
    pub realized_overlays: Vec<OverlayStatus>,
    pub edit_locks: EditLockStatus,
    pub edit_history: EditHistoryStatus,
    /// Every title input in ascending input order with its field revision.
    pub titles: Vec<TitleSummary>,
    /// The engine's video frame rate: every playlist duration and frame
    /// counts these frames.
    pub frame_rate: FrameRate,
    /// Whether exactly one [`PlaylistStateMessage`] follows this snapshot
    /// (after its title states). `false` means the playlist is empty,
    /// stopped, and does not loop.
    pub playlist_follows: bool,
    /// How many [`GradeStateMessage`] records follow this snapshot (after
    /// its title states and playlist state), at most [`MAX_GRADE_STATES`].
    pub grade_states: u16,
}

/// The complete playlist at a snapshot's revision.
///
/// A session writes exactly one `playlist_state` after a snapshot whose
/// [`SnapshotMessage::playlist_follows`] is set, after that snapshot's title
/// states and before any event. Each entry is encoded at most once, so a
/// maximal playlist (see [`MAX_PLAYLIST_ENTRIES`]) always fits one record
/// independently of the size of the snapshot itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaylistStateMessage {
    /// The snapshot's revision.
    pub revision: u64,
    /// The engine's frame cursor (its next unrendered frame) at the
    /// snapshot, so a client can place `state`'s frames in time.
    pub frame: u64,
    pub loop_mode: PlaylistLoopMode,
    pub entries: Vec<PlaylistEntrySpec>,
    pub state: PlaylistRunState,
}

/// A durable state change. Runtime progress uses [`RuntimeEventMessage`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EventPayload {
    InputOrderChanged {
        inputs: Vec<WireInputId>,
    },
    InputRenamed {
        input: WireInputId,
        name: String,
    },
    /// An input was appended to the end of the input order, with a default
    /// Master strip and no padlock.
    InputAdded {
        input: WireInputId,
        name: String,
    },
    /// An input left the show together with its Master strip and padlock.
    /// It was neither Program nor Preview nor otherwise on air; a settled-off
    /// overlay that retained it as its source no longer does.
    InputRemoved {
        input: WireInputId,
    },
    DesiredSwitcher {
        program: WireInputId,
        preview: WireInputId,
        manual_transition: ManualTransitionStatus,
        fade_to_black: FadeToBlackState,
        overlays: Vec<OverlayStatus>,
        input_audio_strips: Vec<InputAudioStripStatus>,
    },
    StingerSlotsChanged {
        program: WireInputId,
        preview: WireInputId,
        manual_transition: ManualTransitionStatus,
        fade_to_black: FadeToBlackState,
        stingers: Vec<StingerStatus>,
        overlays: Vec<OverlayStatus>,
        input_audio_strips: Vec<InputAudioStripStatus>,
    },
    StreamsChanged {
        streams: Vec<StreamStatus>,
    },
    RecordingChanged {
        recorder: WireRecorderSlot,
        active: bool,
    },
    EditLockChanged {
        scope: EditLockScope,
        locked: bool,
    },
    /// A title's fields were saved, or it was taken on air. `fields` is the
    /// complete value set after this revision, strictly ascending by id, and
    /// `overlays` is the complete desired overlay state.
    TitleChanged {
        input: WireInputId,
        field_revision: u64,
        fields: Vec<TitleFieldUpdate>,
        overlays: Vec<OverlayStatus>,
    },
    /// A playlist command was accepted for `frame`: the complete playlist
    /// afterwards and the desired Program and Preview. When the command put
    /// an entry on air, its input reaches Program on exactly `frame`.
    PlaylistChanged {
        frame: u64,
        loop_mode: PlaylistLoopMode,
        entries: Vec<PlaylistEntrySpec>,
        state: PlaylistRunState,
        program: WireInputId,
        preview: WireInputId,
    },
    /// A grade edit was accepted (Protocol 2.28).
    GradeChanged(GradeChange),
}

/// One durable revision.
///
/// `edit_history` is the server's undo/redo summary *after* this revision.
/// It rides every event because any accepted edit, undo, or redo changes it
/// while live operations leave it unchanged, so the newest applied event
/// always carries the current history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventMessage {
    pub cursor: EventCursor,
    pub payload: EventPayload,
    pub edit_history: EditHistoryStatus,
}

/// One durable event within a single-revision transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableEvent {
    pub sequence: u16,
    pub event_type: String,
    pub payload: String,
}

/// All durable events committed atomically at `cursor.revision`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableEventBatch {
    pub cursor: ResumeCursor,
    pub events: Vec<DurableEvent>,
}

/// A replay discontinuity that requires the receiver to request a snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DurableGap {
    pub server: ServerIdentity,
    pub requested_after_revision: u64,
    pub available_from_revision: u64,
    pub current_revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeDomainBoundary {
    pub domain: String,
    pub boundary: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeFailureDisposition {
    RolledBack,
    RetainedForRetry,
    FallbackRealized,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RuntimeLifecycleEvent {
    Accepted,
    Preparing,
    Scheduled {
        domains: Vec<RuntimeDomainBoundary>,
    },
    Realized {
        domain: String,
        manual_transition: ManualTransitionStatus,
        fade_to_black: FadeToBlackState,
    },
    Failed {
        error: StructuredError,
        disposition: RuntimeFailureDisposition,
    },
    Superseded {
        by_revision: u64,
    },
}

/// Runtime progress references durable state but has an independent sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeEventMessage {
    pub server: ServerIdentity,
    pub revision: u64,
    pub generation: u64,
    pub sequence: u64,
    pub event: RuntimeLifecycleEvent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeartbeatMessage {
    pub server: ServerIdentity,
    pub sequence: u64,
    pub sent_at_ms: u64,
    pub last_applied: Option<ResumeCursor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeartbeatAcknowledgementMessage {
    pub server: ServerIdentity,
    pub heartbeat_sequence: u64,
    pub received_at_ms: u64,
}

/// Fixed-point scale for linear audio meter levels.
pub const AUDIO_METER_LEVEL_SCALE: u32 = 1_000_000;
/// Largest channel count in one audio meter layout.
pub const MAX_AUDIO_METER_CHANNELS: usize = 32;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AudioMeterChannel {
    pub peak_millionths: u32,
    pub rms_millionths: u32,
}

impl AudioMeterChannel {
    pub(crate) const fn is_valid(self) -> bool {
        self.rms_millionths <= self.peak_millionths
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputAudioMeters {
    pub input: WireInputId,
    pub channels: Vec<AudioMeterChannel>,
}

/// Lossy, non-resumable audio meter sample for one native Program interval.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioMetersMessage {
    pub server: ServerIdentity,
    pub sequence: u64,
    pub frame: u64,
    pub start_sample: u64,
    pub end_sample: u64,
    pub master: Vec<AudioMeterChannel>,
    pub inputs: Vec<InputAudioMeters>,
}

/// Latest health of one local HLS output: playlist depth and free space.
///
/// This is lossy, latest-wins runtime telemetry like the rest of
/// [`StreamStatusSample`]: it is never durable, never resumable, and never
/// projected into snapshots or durable events. `segments` is the number of
/// media segments currently published in the output's playlist, and
/// `target_duration_ms` is that playlist's target duration in whole
/// milliseconds. Both are bounded on the wire; `free_bytes` is the free space
/// on the output directory's filesystem and may be any `u64` (`0` when the
/// query was unavailable).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamLocalStatus {
    pub segments: u32,
    pub target_duration_ms: u32,
    pub free_bytes: u64,
}

/// One lossy realized stream status sample for one configured target.
///
/// Counters are cumulative since the target's last start. `failure` carries a
/// sanitized short code and never URL text or key material.
///
/// `local` carries the latest health of this target's local HLS output when it
/// has one. Wire encoding: a sample without local health ends at the `failure`
/// field exactly as protocol 2.18 did; a sample with it appends one trailing
/// `local=<segments>:<target_duration_ms>:<free_bytes>` field. `None` is the
/// only shape for network targets, and `local=none` is never encoded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamStatusSample {
    pub target: WireStreamTargetId,
    pub realized: StreamRealizedState,
    pub connected: bool,
    pub muxed_bytes: u64,
    pub enqueued_pairs: u64,
    pub dropped_pairs: u64,
    pub failure: Option<String>,
    pub local: Option<StreamLocalStatus>,
}

/// Lossy, non-resumable realized streaming status for every active target.
///
/// Like [`AudioMetersMessage`] this is latest-wins transport state: it is not
/// durable, not resumable, and never part of snapshots or durable events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamStatusMessage {
    pub server: ServerIdentity,
    pub sequence: u64,
    pub samples: Vec<StreamStatusSample>,
}

/// Largest encoded monitor frame width in pixels.
pub const MAX_MONITOR_FRAME_WIDTH: u16 = 640;
/// Largest encoded monitor frame height in pixels.
pub const MAX_MONITOR_FRAME_HEIGHT: u16 = 360;
/// Largest raw (pre-base64url) payload carried by one `monitor_frame` part.
pub const MAX_MONITOR_FRAME_PART_BYTES: usize = 32 * 1024;
/// Largest number of `monitor_frame` parts in one frame group.
pub const MAX_MONITOR_FRAME_PARTS: u16 = 32;

/// Which monitor an encoded monitor frame belongs to.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MonitorTap {
    Preview,
    Program,
}

impl MonitorTap {
    /// Both taps in stable index order.
    pub const ALL: [Self; 2] = [Self::Preview, Self::Program];

    /// Stable array index: `Preview` is `0`, `Program` is `1`.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Preview => 0,
            Self::Program => 1,
        }
    }

    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Program => "program",
        }
    }
}

/// How the server produced an encoded monitor frame.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MonitorFrameOrigin {
    /// Rendered from the simulated pipeline's realized state.
    Simulated,
    /// Read back from the native compositor.
    Native,
}

impl MonitorFrameOrigin {
    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Simulated => "simulated",
            Self::Native => "native",
        }
    }
}

/// One lossy, non-resumable part of an encoded monitor frame.
///
/// A frame is one QOI stream (3 channels, sRGB colorspace) split into
/// `parts` contiguous chunks of at most [`MAX_MONITOR_FRAME_PART_BYTES`] raw
/// bytes. Every part repeats the frame header fields; all parts of one frame
/// share `sequence`, which strictly increases per tap. Like
/// [`AudioMetersMessage`] this is latest-wins transport state: never durable,
/// never resumable, and never projected into snapshots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorFrameMessage {
    pub server: ServerIdentity,
    pub tap: MonitorTap,
    pub sequence: u64,
    pub frame: u64,
    pub input: WireInputId,
    pub origin: MonitorFrameOrigin,
    pub width: u16,
    pub height: u16,
    pub part: u16,
    pub parts: u16,
    /// Raw QOI bytes for this part; base64url (no padding) on the wire.
    pub data: Vec<u8>,
}

/// One fully assembled and decoded monitor frame, ready for display.
///
/// `rgba` holds exactly `width * height` straight RGBA8 pixels with alpha
/// `255`, row-major and tightly packed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorImage {
    pub tap: MonitorTap,
    pub sequence: u64,
    pub frame: u64,
    pub input: WireInputId,
    pub origin: MonitorFrameOrigin,
    pub width: u16,
    pub height: u16,
    pub rgba: std::sync::Arc<[u8]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityReportSummary {
    pub digest: String,
    pub total: u32,
    pub available: u32,
    pub degraded: u32,
    pub unavailable: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityReportMessage {
    pub server: ServerIdentity,
    pub revision: u64,
    pub summary: CapabilityReportSummary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsRequest {
    pub protocol: ProtocolVersion,
    pub request_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticsResponse {
    pub protocol: ProtocolVersion,
    pub request_id: String,
    pub engine: EngineIdentity,
    pub current_revision: u64,
    pub oldest_retained_revision: Option<u64>,
    pub newest_retained_revision: Option<u64>,
    pub subscriber_count: u32,
    pub retained_events_limit: u32,
    pub subscriber_limit: u32,
    pub subscriber_queue_limit: u32,
}

// A snapshot is sent once per (re)connection, so its inline size is not worth
// boxing every exact-contract construction site for.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WireMessage {
    Command(CommandMessage),
    CommandResult(CommandResult),
    Snapshot(SnapshotMessage),
    Event(EventMessage),
    HandshakeRequest(HandshakeRequest),
    HandshakeResponse(HandshakeResponse),
    DurableEventBatch(DurableEventBatch),
    DurableGap(DurableGap),
    RuntimeEvent(RuntimeEventMessage),
    Heartbeat(HeartbeatMessage),
    HeartbeatAcknowledgement(HeartbeatAcknowledgementMessage),
    AudioMeters(AudioMetersMessage),
    StreamStatus(StreamStatusMessage),
    MonitorFrame(MonitorFrameMessage),
    CapabilityReport(CapabilityReportMessage),
    DiagnosticsRequest(DiagnosticsRequest),
    DiagnosticsResponse(DiagnosticsResponse),
    Error(ErrorMessage),
    TitleState(TitleStateMessage),
    PlaylistState(PlaylistStateMessage),
    GradeState(GradeStateMessage),
}
