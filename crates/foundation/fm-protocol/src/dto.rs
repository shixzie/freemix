use core::{
    fmt,
    num::{NonZeroU32, NonZeroU128},
};

use fm_animation::{AnimationPreset, Clip, Phase, PhaseMotion};
use fm_command::{CommandEnvelope, Deadline, Revision, StateEpoch};
use fm_grade::{CubeKind, Grade};
use fm_types::{FrameRate, InputId, OutputId, SceneId};

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

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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

/// The kind of a manual (T-bar) transition: every timed mix (Protocol
/// 2.48 adds all of them), or a Stinger scrubbing a clip of
/// `duration_frames` frames (SW-004).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualTransitionKind {
    Fade,
    Wipe,
    AlphaFade,
    Slide,
    VerticalWipe,
    VerticalSlide,
    Zoom,
    Fly,
    FlyRotate,
    Cube,
    CubeZoom,
    CrossZoom,
    Merge,
    Stinger {
        slot: WireStingerSlotId,
        duration_frames: u32,
    },
}

impl ManualTransitionKind {
    /// The manual kind of every automatic mix.
    #[must_use]
    pub const fn from_mix(kind: AutoTransitionKind) -> Self {
        match kind {
            AutoTransitionKind::Fade => Self::Fade,
            AutoTransitionKind::AlphaFade => Self::AlphaFade,
            AutoTransitionKind::Wipe => Self::Wipe,
            AutoTransitionKind::VerticalWipe => Self::VerticalWipe,
            AutoTransitionKind::Slide => Self::Slide,
            AutoTransitionKind::VerticalSlide => Self::VerticalSlide,
            AutoTransitionKind::Zoom => Self::Zoom,
            AutoTransitionKind::Fly => Self::Fly,
            AutoTransitionKind::FlyRotate => Self::FlyRotate,
            AutoTransitionKind::Cube => Self::Cube,
            AutoTransitionKind::CubeZoom => Self::CubeZoom,
            AutoTransitionKind::CrossZoom => Self::CrossZoom,
            AutoTransitionKind::Merge => Self::Merge,
        }
    }

    /// The automatic mix this manual kind draws, or `None` for a Stinger.
    #[must_use]
    pub const fn mix(self) -> Option<AutoTransitionKind> {
        Some(match self {
            Self::Fade => AutoTransitionKind::Fade,
            Self::AlphaFade => AutoTransitionKind::AlphaFade,
            Self::Wipe => AutoTransitionKind::Wipe,
            Self::VerticalWipe => AutoTransitionKind::VerticalWipe,
            Self::Slide => AutoTransitionKind::Slide,
            Self::VerticalSlide => AutoTransitionKind::VerticalSlide,
            Self::Zoom => AutoTransitionKind::Zoom,
            Self::Fly => AutoTransitionKind::Fly,
            Self::FlyRotate => AutoTransitionKind::FlyRotate,
            Self::Cube => AutoTransitionKind::Cube,
            Self::CubeZoom => AutoTransitionKind::CubeZoom,
            Self::CrossZoom => AutoTransitionKind::CrossZoom,
            Self::Merge => AutoTransitionKind::Merge,
            Self::Stinger { .. } => return None,
        })
    }

    /// The wire token: an [`AutoTransitionKind`] token, or `stinger_<slot>`.
    #[must_use]
    pub fn token(self) -> String {
        match (self, self.mix()) {
            (_, Some(mix)) => mix.token().to_owned(),
            (Self::Stinger { slot, .. }, None) => format!("stinger_{slot}"),
            (_, None) => unreachable!("only a Stinger has no mix"),
        }
    }

    /// The clip length of a Stinger; `None` for a mix.
    #[must_use]
    pub const fn duration_frames(self) -> Option<u32> {
        match self {
            Self::Stinger {
                duration_frames, ..
            } => Some(duration_frames),
            _ => None,
        }
    }

    /// The kind named by `token`, with the nonzero clip length a Stinger
    /// needs (and a mix must not carry).
    #[must_use]
    pub fn from_token(token: &str, duration_frames: Option<u32>) -> Option<Self> {
        match TransitionFavoriteEffect::from_token(token)? {
            TransitionFavoriteEffect::Cut => None,
            TransitionFavoriteEffect::Mix(kind) => {
                duration_frames.is_none().then_some(Self::from_mix(kind))
            }
            TransitionFavoriteEffect::Stinger(slot) => Some(Self::Stinger {
                slot,
                duration_frames: duration_frames.filter(|frames| *frames > 0)?,
            }),
        }
    }
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

/// How an overlay channel goes on and off air. Cut and Fade change its
/// opacity; every other kind (Protocol 2.36) moves the layer in from, and
/// out to, transparency with the Program transition of the same name.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverlayTransitionKind {
    #[default]
    Cut,
    Fade,
    Wipe,
    VerticalWipe,
    Slide,
    VerticalSlide,
    Zoom,
    Fly,
    FlyRotate,
    Cube,
    CubeZoom,
    CrossZoom,
    Merge,
}

impl OverlayTransitionKind {
    /// Every overlay transition, in operator order.
    pub const ALL: [Self; 13] = [
        Self::Cut,
        Self::Fade,
        Self::Wipe,
        Self::VerticalWipe,
        Self::Slide,
        Self::VerticalSlide,
        Self::Zoom,
        Self::Fly,
        Self::FlyRotate,
        Self::Cube,
        Self::CubeZoom,
        Self::CrossZoom,
        Self::Merge,
    ];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Cut => "cut",
            Self::Fade => "fade",
            Self::Wipe => "wipe",
            Self::VerticalWipe => "vertical_wipe",
            Self::Slide => "slide",
            Self::VerticalSlide => "vertical_slide",
            Self::Zoom => "zoom",
            Self::Fly => "fly",
            Self::FlyRotate => "fly_rotate",
            Self::Cube => "cube",
            Self::CubeZoom => "cube_zoom",
            Self::CrossZoom => "cross_zoom",
            Self::Merge => "merge",
        }
    }

    /// The overlay transition named by `token`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.token() == token)
    }
}

/// Every automatic mix transition other than a Stinger (Protocol 2.36),
/// taken by kind with [`CommandPayload::Transition`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AutoTransitionKind {
    Fade,
    AlphaFade,
    Wipe,
    VerticalWipe,
    Slide,
    VerticalSlide,
    Zoom,
    Fly,
    FlyRotate,
    Cube,
    CubeZoom,
    CrossZoom,
    Merge,
}

impl AutoTransitionKind {
    /// Every kind, in operator order.
    pub const ALL: [Self; 13] = [
        Self::Fade,
        Self::AlphaFade,
        Self::Wipe,
        Self::VerticalWipe,
        Self::Slide,
        Self::VerticalSlide,
        Self::Zoom,
        Self::Fly,
        Self::FlyRotate,
        Self::Cube,
        Self::CubeZoom,
        Self::CrossZoom,
        Self::Merge,
    ];

    /// The wire token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Fade => "fade",
            Self::AlphaFade => "alpha_fade",
            Self::Wipe => "wipe",
            Self::VerticalWipe => "vertical_wipe",
            Self::Slide => "slide",
            Self::VerticalSlide => "vertical_slide",
            Self::Zoom => "zoom",
            Self::Fly => "fly",
            Self::FlyRotate => "fly_rotate",
            Self::Cube => "cube",
            Self::CubeZoom => "cube_zoom",
            Self::CrossZoom => "cross_zoom",
            Self::Merge => "merge",
        }
    }

    /// The kind named by `token`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.token() == token)
    }
}

/// Number of favourite transition buttons (Protocol 2.36).
pub const TRANSITION_FAVORITE_COUNT: usize = 4;
/// Longest favourite transition duration, in frames.
pub const MAX_TRANSITION_FAVORITE_DURATION_FRAMES: u32 = 3_600;

/// What a favourite transition button takes (Protocol 2.36).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionFavoriteEffect {
    Cut,
    Mix(AutoTransitionKind),
    Stinger(WireStingerSlotId),
}

impl TransitionFavoriteEffect {
    /// The wire token: `cut`, an [`AutoTransitionKind`] token, or
    /// `stinger_<slot>`.
    #[must_use]
    pub fn token(self) -> String {
        match self {
            Self::Cut => "cut".to_owned(),
            Self::Mix(kind) => kind.token().to_owned(),
            Self::Stinger(slot) => format!("stinger_{slot}"),
        }
    }

    /// The effect named by `token`.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        if token == "cut" {
            return Some(Self::Cut);
        }
        if let Some(slot) = token.strip_prefix("stinger_") {
            // One canonical digit: slots are 1 through 8.
            if slot.len() != 1 {
                return None;
            }
            return slot
                .parse()
                .ok()
                .and_then(WireStingerSlotId::new)
                .map(Self::Stinger);
        }
        AutoTransitionKind::from_token(token).map(Self::Mix)
    }
}

/// One favourite transition button: its effect and duration (Protocol 2.36).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionFavoriteStatus {
    pub effect: TransitionFavoriteEffect,
    /// `1..=`[`MAX_TRANSITION_FAVORITE_DURATION_FRAMES`].
    pub duration_frames: u32,
}

impl TransitionFavoriteStatus {
    /// A new project's favourites: Fade, Wipe, Fly, and Cube over 30 frames.
    #[must_use]
    pub fn defaults() -> [Self; TRANSITION_FAVORITE_COUNT] {
        let favorite = |kind| Self {
            effect: TransitionFavoriteEffect::Mix(kind),
            duration_frames: 30,
        };
        [
            favorite(AutoTransitionKind::Fade),
            favorite(AutoTransitionKind::Wipe),
            favorite(AutoTransitionKind::Fly),
            favorite(AutoTransitionKind::Cube),
        ]
    }
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
/// These are the generators the simulated daemon renders, network streams
/// (Protocol 2.46), and capture devices (Protocol 2.47); every other input
/// kind (media files, Scenes, aliases, mix feeds) is added offline. Wire
/// encoding: `kind=solid` plus `color=<rrggbbaa>` (eight lowercase hex
/// digits), `kind=bars`, `kind=network_stream` plus the config fields (see
/// [`crate::NetworkStreamConfig`]), `kind=capture` plus `device` and the
/// settings fields (see [`crate::CaptureInputConfig`]), or
/// `kind=system_audio` plus `backend` and `device` (Protocol 2.55, see
/// [`crate::SystemAudioConfig`]), or `kind=screen_capture` plus the
/// capture fields (IN-011, see [`crate::screen_capture`]). Generator audio
/// is always silent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LiveInputSource {
    Solid {
        red: u8,
        green: u8,
        blue: u8,
        alpha: u8,
    },
    Bars,
    /// A network stream (IN-015). `Debug` shows only the redacted URL.
    NetworkStream(Box<crate::NetworkStreamConfig>),
    /// A local capture device and its settings (Protocol 2.47, IN-001).
    Capture(Box<crate::CaptureInputConfig>),
    /// A system audio capture source (Protocol 2.55, AU-011).
    SystemAudio(Box<crate::SystemAudioConfig>),
    /// A picture file (IN-008): `kind=image` plus `uri` and `fit`.
    Image(Box<crate::ImageInputConfig>),
    /// A slideshow (IN-009): `kind=slideshow` plus `slideshow` (see
    /// [`crate::SlideshowConfig`]).
    Slideshow(Box<crate::SlideshowConfig>),
    /// A screen or window capture and its settings (IN-011).
    ScreenCapture(Box<crate::ScreenCaptureConfig>),
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

/// Stable identity of one title template element carried on the wire
/// (Protocol 2.29): the target of every title animation track.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WireTitleElementId(NonZeroU128);

impl WireTitleElementId {
    #[must_use]
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }
}

impl fmt::Display for WireTitleElementId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A title template's keyframe animation as carried on the wire: an
/// [`fm_animation::Clip`] keyed by element. Every clip bound of
/// `fm_animation` (tracks, keys, phase length, values, easings) holds.
pub type WireTitleClip = Clip<WireTitleElementId>;

/// Largest binary encoding of one [`WireTitleClip`], in bytes. The clip
/// travels as unpadded base64url in one field, so this keeps the field
/// within [`crate::MAX_FIELD_VALUE_BYTES`] (48 KiB encoded).
pub const MAX_TITLE_ANIMATION_BYTES: usize = 36 * 1024;

/// One title template's animation at a snapshot's `revision` (Protocol
/// 2.29).
///
/// A session writes exactly one `title_animation_state` per [`TitleSummary`]
/// of a snapshot, in summary order, after the snapshot's `grade_state`
/// records and before any event. Title inputs that share a template report
/// the same clip and revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleAnimationStateMessage {
    pub revision: u64,
    pub input: WireInputId,
    /// The template's animation revision: the `base_revision` the next
    /// animation edit must name.
    pub animation_revision: u64,
    pub clip: Box<WireTitleClip>,
}

/// Most elements one [`WireTitleLayout`] lists (Protocol 2.30). A template
/// with more lists its first elements in template order; the rest cannot
/// be targeted by an editor.
pub const MAX_TITLE_LAYOUT_ELEMENTS: usize = 192;
/// Longest element display name in a [`WireTitleLayout`], in UTF-8 bytes.
/// Longer template names are cut at a character boundary.
pub const MAX_TITLE_ELEMENT_NAME_BYTES: usize = 96;
/// Largest title template canvas side carried in a [`WireTitleLayout`], in
/// pixels (the `fm_titles` output limit).
pub const MAX_TITLE_CANVAS_SIDE: u32 = 8_192;
/// Largest binary encoding of one [`WireTitleLayout`]'s element list, in
/// bytes; with the bounds above every layout fits (worst case about 28
/// KiB), and the base64url field stays within
/// [`crate::MAX_FIELD_VALUE_BYTES`].
pub const MAX_TITLE_LAYOUT_BYTES: usize = 36 * 1024;

/// What one title template element draws (Protocol 2.30), for editor
/// outlines and icons.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WireTitleElementKind {
    /// Text from a field.
    Text,
    /// A filled rectangle.
    Rectangle,
    /// An image from a field.
    Image,
    /// A scene clock.
    Clock,
    /// A shared production clock.
    ProductionClock,
    /// A ticker crawl.
    Ticker,
}

impl WireTitleElementKind {
    /// Every kind in wire tag order.
    pub const ALL: [Self; 6] = [
        Self::Text,
        Self::Rectangle,
        Self::Image,
        Self::Clock,
        Self::ProductionClock,
        Self::Ticker,
    ];
}

/// One title template element's static design (Protocol 2.30): what an
/// animation editor needs to list, outline, and apply presets to it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireTitleElementOutline {
    pub id: WireTitleElementId,
    /// The element's display name, at most
    /// [`MAX_TITLE_ELEMENT_NAME_BYTES`] bytes with no control characters
    /// (may be empty).
    pub name: String,
    pub kind: WireTitleElementKind,
    /// Static placement in template canvas pixels.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Static opacity, `0..=255`.
    pub opacity: u8,
    /// Static straight RGBA fill.
    pub fill: fm_animation::Rgba,
    /// Whether the element is drawn at all.
    pub visible: bool,
}

/// A title template's static layout (Protocol 2.30): its canvas and its
/// elements in template order. A title without a loaded template has the
/// empty layout: a zero canvas and no elements.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WireTitleLayout {
    /// Canvas width and height in pixels: both zero (no template) or both
    /// in `1..=`[`MAX_TITLE_CANVAS_SIDE`].
    pub width: u32,
    pub height: u32,
    /// At most [`MAX_TITLE_LAYOUT_ELEMENTS`], ids unique.
    pub elements: Vec<WireTitleElementOutline>,
}

/// One title template's layout at a snapshot's `revision` (Protocol 2.30).
///
/// A session writes exactly one `title_layout_state` per [`TitleSummary`]
/// of a snapshot, in summary order, after the snapshot's
/// `title_animation_state` records and before any event. No command edits
/// a template's layout, so it changes only with a new snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitleLayoutStateMessage {
    pub revision: u64,
    pub input: WireInputId,
    pub layout: Box<WireTitleLayout>,
}

/// Stable identity of one project scene carried on the wire (Protocol
/// 2.35): what a scene layout and a scene layer grade name.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WireSceneId(NonZeroU128);

impl WireSceneId {
    #[must_use]
    pub const fn new(value: NonZeroU128) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> NonZeroU128 {
        self.0
    }

    #[must_use]
    pub const fn from_domain(value: SceneId) -> Self {
        Self(value.get())
    }

    #[must_use]
    pub const fn to_domain(self) -> SceneId {
        SceneId::new(self.0)
    }
}

impl fmt::Display for WireSceneId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Stable identity of one scene layer carried on the wire (Protocol 2.31):
/// the target of every scene animation track. Unique within its scene.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WireSceneLayerId(core::num::NonZeroU32);

impl WireSceneLayerId {
    #[must_use]
    pub const fn new(value: core::num::NonZeroU32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> core::num::NonZeroU32 {
        self.0
    }
}

impl fmt::Display for WireSceneLayerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// A scene's layer animation as carried on the wire (Protocol 2.31): an
/// [`fm_animation::Clip`] keyed by layer, in the title clip encoding. Scene
/// clips never animate [`fm_animation::Property::Color`].
pub type WireSceneClip = Clip<WireSceneLayerId>;

/// Largest binary encoding of one [`WireSceneClip`], in bytes (the title
/// clip bound, so the field stays within [`crate::MAX_FIELD_VALUE_BYTES`]).
pub const MAX_SCENE_ANIMATION_BYTES: usize = 36 * 1024;

/// One scene's animation at a snapshot's `revision` (Protocol 2.31).
///
/// A session writes exactly one `scene_animation_state` per Scene input of
/// a snapshot's `scenes` list, in list order, after the snapshot's
/// `title_layout_state` records and before any event. Scene inputs that
/// show the same scene report the same clip and revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneAnimationStateMessage {
    pub revision: u64,
    pub input: WireInputId,
    /// The scene the Scene input shows (Protocol 2.35).
    pub scene: WireSceneId,
    /// The scene's animation revision: the `base_revision` the next
    /// animation edit must name.
    pub animation_revision: u64,
    pub clip: Box<WireSceneClip>,
}

/// Most layers one [`WireSceneLayout`] lists (Protocol 2.31; 192 since 2.39,
/// so the full layer design of every listed layer fits). A scene with more
/// lists its first layers in scene order; the rest cannot be targeted by an
/// editor, and a live layer add never grows a scene beyond it.
pub const MAX_SCENE_LAYOUT_LAYERS: usize = 192;
/// Most enabled (drawn) layers one scene holds: the compositor's limit. A
/// live layer add or enable past it is refused (`too_many_enabled_layers`).
pub const MAX_SCENE_ENABLED_LAYERS: usize = 64;
/// Longest layer display name in a [`WireSceneLayout`], in UTF-8 bytes
/// (Protocol 2.35; room for each layer's source). Longer names are cut at
/// a character boundary.
pub const MAX_SCENE_LAYER_NAME_BYTES: usize = 80;
/// Longest scene display name in a [`SceneLayoutStateMessage`], in UTF-8
/// bytes (Protocol 2.35). Longer names are cut at a character boundary.
pub const MAX_SCENE_NAME_BYTES: usize = 128;
/// Largest scene canvas side carried in a [`WireSceneLayout`], in pixels
/// (the project video limit).
pub const MAX_SCENE_CANVAS_SIDE: u32 = 8_192;
/// Largest binary encoding of one [`WireSceneLayout`]'s layer list, in
/// bytes; with the bounds above every layout fits (worst case about 30
/// KiB of design plus, since Protocol 2.43, about 9 KiB of keys and
/// effects, and since 2.70 about 3.5 KiB more of key/fill keys and key
/// refinements: 43,970 bytes in all).
pub const MAX_SCENE_LAYOUT_BYTES: usize = 44 * 1024;

/// What one scene layer draws (Protocol 2.35): an input (a Scene input
/// draws its scene) or a scene directly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireSceneLayerSource {
    Input(WireInputId),
    Scene(WireSceneId),
}

/// One scene layer's static design (Protocol 2.31): what an animation
/// editor needs to list, outline, and apply presets to it, and (Protocol
/// 2.35) what a colour editor needs to preview it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireSceneLayerOutline {
    pub id: WireSceneLayerId,
    /// The layer's display name, at most [`MAX_SCENE_LAYER_NAME_BYTES`]
    /// bytes with no control characters (may be empty).
    pub name: String,
    /// What the layer draws.
    pub source: WireSceneLayerSource,
    /// Static box in project canvas pixels: the top-left of its
    /// quarter-turned (untilted) footprint and its size before
    /// `quarter_turns`.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// Static clockwise quarter turns, `0..=3`.
    pub quarter_turns: u8,
    /// Static further clockwise turn about the anchor (Protocol 2.39), in
    /// `MIN_SCENE_LAYER_TILT_MILLIDEGREES..MAX_SCENE_LAYER_TILT_MILLIDEGREES`:
    /// the layer's free rotation is `quarter_turns * 90_000 +
    /// tilt_millidegrees` millidegrees.
    pub tilt_millidegrees: i32,
    /// The scale and rotation pivot in the box's own frame, in basis points
    /// of its width and height, each `0..=`[`crate::SCENE_LAYER_ANCHOR_MAX`]
    /// (Protocol 2.39).
    pub anchor_x: u16,
    pub anchor_y: u16,
    /// Static opacity, `0..=255`.
    pub opacity: u8,
    /// Whether the layer is drawn at all.
    pub enabled: bool,
    /// Compositor z-order (Protocol 2.39).
    pub z_order: i32,
    /// Source crop in canvas pixels (Protocol 2.39), inside the canvas.
    pub crop: Option<crate::WireLayerCrop>,
    /// Rectangular mask in post-crop source pixels (Protocol 2.39), inside
    /// the cropped source.
    pub mask: Option<crate::WireLayerMask>,
    /// Chroma or luma key (Protocol 2.43).
    pub key: Option<crate::WireLayerKey>,
    /// Ordered effects, at most [`crate::MAX_SCENE_LAYER_EFFECTS`]
    /// (Protocol 2.43).
    pub effects: Vec<crate::WireLayerEffect>,
}

/// A scene's static layout (Protocol 2.31): the project canvas and the
/// scene's layers in scene (stacking list) order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireSceneLayout {
    /// Canvas width and height in pixels, each in
    /// `1..=`[`MAX_SCENE_CANVAS_SIDE`].
    pub width: u32,
    pub height: u32,
    /// At most [`MAX_SCENE_LAYOUT_LAYERS`], ids unique.
    pub layers: Vec<WireSceneLayerOutline>,
}

/// One project scene's layout at a snapshot's `revision` (Protocol 2.31;
/// keyed by scene since 2.35).
///
/// A session writes exactly one `scene_layout_state` per scene of a
/// snapshot's `layout_scenes` list (every project scene, whether or not a
/// Scene input shows it), in list order, after the snapshot's
/// `scene_animation_state` records and before any event. Since Protocol
/// 2.39 a live scene layer edit (`scene_layer_*`) announces the scene's
/// whole new layout in `event=scene_layout_changed`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SceneLayoutStateMessage {
    pub revision: u64,
    pub scene: WireSceneId,
    /// The scene's display name, at most [`MAX_SCENE_NAME_BYTES`] bytes
    /// with no control characters (may be empty).
    pub name: String,
    pub layout: Box<WireSceneLayout>,
}

/// Where an exact title preview samples the animation. Wire encoding:
/// `phase=static|in|hold|out` with `frame` (zero unless `in` or `out`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitlePreviewSample {
    /// Static values only.
    Static,
    /// The In phase at a whole clip frame.
    In { frame: u32 },
    /// The resting on-air pose.
    Hold,
    /// The Out phase at a whole clip frame.
    Out { frame: u32 },
}

/// Largest exact title preview, in pixels; the monitor frame cap.
pub const MAX_TITLE_PREVIEW_WIDTH: u16 = 640;
pub const MAX_TITLE_PREVIEW_HEIGHT: u16 = 360;
/// Largest raw (pre-base64url) payload of one `title_preview_frame` part.
pub const MAX_TITLE_PREVIEW_PART_BYTES: usize = 32 * 1024;
/// Most parts of one preview: a worst-case 640x360 four-channel QOI stream
/// (`640 * 360 * 5 + 22` bytes) fits.
pub const MAX_TITLE_PREVIEW_PARTS: u16 = 36;

/// A read-only request to render one title exactly as the authority would
/// (Protocol 2.29): its persisted field values in `sample`, scaled to
/// `width` x `height`. `clip`, when present, is an unsaved draft rendered
/// instead of the template's animation. Not a command: it has no
/// idempotency, revision, or durability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitlePreviewRequest {
    pub protocol: crate::ProtocolVersion,
    pub request_id: String,
    pub input: WireInputId,
    pub sample: TitlePreviewSample,
    pub width: u16,
    pub height: u16,
    pub clip: Option<Box<WireTitleClip>>,
}

/// One part of the answer to a [`TitlePreviewRequest`].
///
/// The image is one standard QOI stream with four channels (straight sRGB
/// RGBA8, the template's transparency kept) split into `parts` contiguous
/// chunks of at most [`MAX_TITLE_PREVIEW_PART_BYTES`]. Every part repeats
/// the header fields. Lossy and latest-wins like a monitor frame.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitlePreviewFrameMessage {
    pub server: ServerIdentity,
    pub request_id: String,
    pub input: WireInputId,
    /// The template's animation revision when the frame was rendered.
    pub animation_revision: u64,
    /// Whether the request's draft clip was rendered.
    pub draft: bool,
    pub width: u16,
    pub height: u16,
    pub part: u16,
    pub parts: u16,
    /// Raw QOI bytes for this part; base64url (no padding) on the wire.
    pub data: Vec<u8>,
}

/// A read-only request to render one Scene input exactly as the authority
/// composes it (Protocol 2.32): its layers posed at `sample` of the scene's
/// animation, composed at `width` x `height`. `clip`, when present, is an
/// unsaved draft rendered instead of the scene's animation. Like a title
/// preview it is not a command, needs `EditProject`, shares the session's
/// preview rate, and is served to raw TCP sessions only.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenePreviewRequest {
    pub protocol: crate::ProtocolVersion,
    pub request_id: String,
    pub input: WireInputId,
    pub sample: TitlePreviewSample,
    pub width: u16,
    pub height: u16,
    pub clip: Option<Box<WireSceneClip>>,
}

/// One part of the answer to a [`ScenePreviewRequest`] (Protocol 2.32):
/// exactly a [`TitlePreviewFrameMessage`]'s fields under the record kind
/// `scene_preview_frame`, `animation_revision` being the scene's.
pub type ScenePreviewFrameMessage = TitlePreviewFrameMessage;

/// One fully assembled and decoded scene preview: a [`TitlePreviewImage`]
/// whose `input` is the Scene input.
pub type ScenePreviewImage = TitlePreviewImage;

/// One fully assembled and decoded title preview.
///
/// `rgba` holds exactly `width * height` straight RGBA8 pixels, row-major
/// and tightly packed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TitlePreviewImage {
    pub request_id: String,
    pub input: WireInputId,
    pub animation_revision: u64,
    pub draft: bool,
    pub width: u16,
    pub height: u16,
    pub rgba: std::sync::Arc<[u8]>,
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
        /// Mono source position, `-10_000..=10_000` (Protocol 2.40).
        pan_basis_points: i32,
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
    /// Takes any automatic mix transition by kind (Protocol 2.36). Wire
    /// payload `transition`.
    Transition {
        kind: AutoTransitionKind,
        duration_frames: u32,
    },
    /// Assigns favourite transition button `slot` (1 through
    /// [`TRANSITION_FAVORITE_COUNT`]), an undoable project edit (Protocol
    /// 2.36). Wire payload `transition_favorite_set`.
    SetTransitionFavorite {
        slot: u8,
        favorite: TransitionFavoriteStatus,
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
    /// Reconnects a desired-running destination now (Protocol 2.52,
    /// OR-006): clears a latched failure or a pending backoff and restarts
    /// its reconnect budget. A live operation like `stream_start`.
    StreamRetry {
        target: WireStreamTargetId,
    },
    /// Starts every configured destination (Protocol 2.52, OR-006). Never
    /// affects recording.
    StreamStartAll,
    /// Stops every configured destination (Protocol 2.52, OR-006). Never
    /// affects recording.
    StreamStopAll,
    /// Adds or reconfigures destination `target` (Protocol 2.52, OR-006).
    /// The stream key is kept, cleared, or replaced by `stream_key`, and an
    /// SRT destination's passphrase by `srt_passphrase` (2.54); a client
    /// cannot read either back. `position` (2.54) places the destination at
    /// that index of the authored list (an undone removal returns to its old
    /// place); `None` keeps an existing one where it is and appends a new
    /// one. A padlocked, undoable project edit, refused while the destination
    /// is desired running.
    SetStreamTarget {
        target: WireStreamTargetId,
        config: Box<crate::StreamTargetConfig>,
        stream_key: crate::SecretEdit,
        srt_passphrase: crate::SecretEdit,
        position: Option<u8>,
    },
    /// Removes destination `target` (Protocol 2.52, OR-006). A padlocked,
    /// undoable project edit, refused while it is desired running or a cue
    /// list starts or stops it.
    RemoveStreamTarget {
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
    /// Creates or replaces cue list `list` (Protocol 2.50, RC-017): an
    /// undoable project edit. Wire payload `cue_list_set`.
    SetCueList {
        list: u64,
        spec: Box<crate::CueListSpec>,
        /// Protocol 2.77: places the cue list at this index of the project's
        /// list (an undone removal returns to its old place); `None` keeps
        /// an existing one where it is and appends a new one.
        position: Option<u8>,
    },
    /// Removes cue list `list`. Wire payload `cue_list_remove`.
    RemoveCueList {
        list: u64,
    },
    /// A live cue list operation (GO, standby, back, skip, stop, reset).
    CueControl {
        list: u64,
        operation: crate::CueOperation,
    },
    /// The authority's cue clock: runs list `list`'s waiting actions due on
    /// `frame`. Accepted only for the frame they are due on (or the next
    /// frame once that has passed). List `0` resumes the waiting script run
    /// (Protocol 2.62).
    CueAdvance {
        list: u64,
        frame: u64,
    },
    /// Creates or replaces script `script` (Protocol 2.58, RC-011): an
    /// undoable project edit. Wire payload `script_set`.
    SetScript {
        script: u64,
        spec: Box<crate::ScriptSpec>,
        /// Protocol 2.77: places the script at this index of the project's
        /// list (an undone removal returns to its old place); `None` keeps
        /// an existing one where it is and appends a new one.
        position: Option<u8>,
    },
    /// Removes script `script`. Wire payload `script_remove`.
    RemoveScript {
        script: u64,
    },
    /// Runs script `script` on the authority: its actions apply on the
    /// command's target frame, all or nothing. Wire payload `script_run`.
    RunScript {
        script: u64,
    },
    /// Ends waiting script run `run`, or clears that run's fault
    /// (Protocol 2.62); with no `run`, ends every waiting run and clears
    /// every fault (Protocol 2.69). Wire payload `script_stop`, optional
    /// nonzero `run`.
    StopScript {
        run: Option<u64>,
    },
    /// Creates or replaces user shortcut `shortcut` (Protocol 2.72,
    /// RC-004): an undoable project edit. Wire payload `shortcut_set`.
    SetShortcut {
        shortcut: u64,
        spec: Box<crate::ShortcutSpec>,
        /// Protocol 2.77: places the shortcut at this index of the project's
        /// list (an undone removal returns to its old place); `None` keeps
        /// an existing one where it is and appends a new one.
        position: Option<u8>,
    },
    /// Removes user shortcut `shortcut`. Wire payload `shortcut_remove`.
    RemoveShortcut {
        shortcut: u64,
    },
    /// Fires user shortcut `shortcut` on the authority, as the command it
    /// stands for. Wire payload `shortcut_fire`.
    FireShortcut {
        shortcut: u64,
    },
    /// Sets (`Some`) or clears (`None`) one input's grade (Protocol 2.28).
    /// Wire payloads `grade_input_set` and `grade_input_clear`.
    SetInputGrade {
        input: WireInputId,
        source: Option<WireGradeSource>,
        /// The live drag this edit belongs to, if any (Protocol 2.56).
        gesture: Option<GradeGesture>,
    },
    /// Sets (`Some`) or clears (`None`) the grade of layer `layer` of scene
    /// `scene` (Protocol 2.33; addressed by scene since 2.35), whether or
    /// not a Scene input shows it. Wire payloads `grade_layer_set` and
    /// `grade_layer_clear`. Every Scene input showing the scene shows the
    /// change, and each one's padlock covers it.
    SetLayerGrade {
        scene: WireSceneId,
        layer: WireSceneLayerId,
        source: Option<WireGradeSource>,
        /// The live drag this edit belongs to, if any (Protocol 2.56).
        gesture: Option<GradeGesture>,
    },
    /// Sets (`Some`) or clears (`None`) the program output grade. Wire
    /// payloads `grade_program_set` and `grade_program_clear`.
    SetProgramGrade {
        source: Option<WireGradeSource>,
        /// The live drag this edit belongs to, if any (Protocol 2.56).
        gesture: Option<GradeGesture>,
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
    /// Replaces a title template's whole animation clip (Protocol 2.29).
    /// Wire payload `title_animation_set`. Refused as a conflict unless the
    /// template's animation revision is `base_revision`.
    SetTitleAnimation {
        input: WireInputId,
        base_revision: u64,
        clip: Box<WireTitleClip>,
    },
    /// Applies a built-in preset to one element of a title template's
    /// animation over `frames` frames of `phase`. Wire payload
    /// `title_animation_preset`.
    ApplyTitleAnimationPreset {
        input: WireInputId,
        base_revision: u64,
        element: WireTitleElementId,
        phase: Phase,
        preset: AnimationPreset,
        frames: u32,
    },
    /// Scales every key of one phase of a title template's animation to a
    /// new phase length. Wire payload `title_animation_stretch`.
    StretchTitleAnimation {
        input: WireInputId,
        base_revision: u64,
        phase: Phase,
        frames: u32,
    },
    /// Replaces a scene's whole layer animation clip through one of its
    /// Scene inputs (Protocol 2.31). Wire payload `scene_animation_set`.
    /// Refused as a conflict unless the scene's animation revision is
    /// `base_revision`.
    SetSceneAnimation {
        input: WireInputId,
        base_revision: u64,
        clip: Box<WireSceneClip>,
    },
    /// Applies a built-in preset to one layer of a scene's animation over
    /// `frames` frames of `phase`. Wire payload `scene_animation_preset`.
    ApplySceneAnimationPreset {
        input: WireInputId,
        base_revision: u64,
        layer: WireSceneLayerId,
        phase: Phase,
        preset: AnimationPreset,
        frames: u32,
    },
    /// Scales every key of one phase of a scene's animation to a new phase
    /// length. Wire payload `scene_animation_stretch`.
    StretchSceneAnimation {
        input: WireInputId,
        base_revision: u64,
        phase: Phase,
        frames: u32,
    },
    /// Saves a motion to the animation preset library at `index` (the end
    /// when absent) (Protocol 2.34). Wire payload `animation_preset_add`.
    AddAnimationPreset {
        preset: NonZeroU32,
        name: String,
        phase: Phase,
        motion: Box<PhaseMotion>,
        index: Option<u16>,
    },
    /// Renames a library preset. Wire payload `animation_preset_rename`.
    RenameAnimationPreset {
        preset: NonZeroU32,
        name: String,
    },
    /// Deletes a library preset. Wire payload `animation_preset_remove`.
    RemoveAnimationPreset {
        preset: NonZeroU32,
    },
    /// Applies library preset `preset` to one element of a title template's
    /// animation over `frames` frames of `phase` (Protocol 2.34). Wire
    /// payload `title_animation_user_preset`.
    ApplyTitleAnimationUserPreset {
        input: WireInputId,
        base_revision: u64,
        element: WireTitleElementId,
        phase: Phase,
        preset: NonZeroU32,
        frames: u32,
    },
    /// Applies library preset `preset` to one layer of a scene's animation
    /// over `frames` frames of `phase` (Protocol 2.34). Wire payload
    /// `scene_animation_user_preset`.
    ApplySceneAnimationUserPreset {
        input: WireInputId,
        base_revision: u64,
        layer: WireSceneLayerId,
        phase: Phase,
        preset: NonZeroU32,
        frames: u32,
    },
    /// Controls a media-file input (Protocol 2.37, IN-003). Wire payloads
    /// `media_<operation token>`. Transport operations are operator
    /// controls; the others edit the input's persisted playback settings.
    Media {
        input: WireInputId,
        operation: crate::MediaOperation,
    },
    /// Steps a paged input: payload `page_navigate` with `input`, `action`,
    /// and for `goto` `page`. A live operation: a slideshow (IN-009) steps
    /// on its target frame and is answered by `page_navigated`; a document
    /// (IN-014) moves its durable page at once and is answered by
    /// `paged_changed`.
    PageNavigate {
        input: WireInputId,
        navigation: crate::PagedNavigation,
    },
    /// Creates, edits, or steps a media list input (IN-006; see
    /// [`crate::MediaListOperation`]). Wire payload `media_list`.
    MediaList {
        input: WireInputId,
        operation: crate::MediaListOperation,
    },
    /// An input lifecycle edit (Protocol 2.38, IN-026; see
    /// [`crate::InputLifecycleOperation`]). A project edit.
    InputLifecycle(crate::InputLifecycleOperation),
    /// A live scene layer edit (Protocol 2.39, SW-006; see
    /// [`crate::SceneLayerEdit`]). A project edit, undoable, covered by the
    /// project padlock and by the padlock of every Scene input showing the
    /// scene.
    SceneLayer(crate::SceneLayerEdit),
    /// A mixer label, group, or group-level operation (Protocol 2.40,
    /// AU-001; see [`crate::AudioMixerOperation`]).
    AudioMixer(crate::AudioMixerOperation),
    /// Chooses whether Fade to Black fades `output` (Protocol 2.40, SW-004).
    /// A project edit, undoable.
    SetFadeToBlackOutput {
        output: WireOutputId,
        affected: bool,
    },
    /// Gives `output` its own Fade-to-Black duration, 1 through 3600 frames,
    /// or with `None` returns it to the one each request carries (Protocol
    /// 2.48, SW-004). A padlocked, undoable project edit.
    SetFadeToBlackOutputDuration {
        output: WireOutputId,
        duration_frames: Option<u32>,
    },
    /// Chooses how automatic Fade-to-Black moves ease (Protocol 2.48,
    /// SW-004). A padlocked, undoable project edit.
    SetFadeToBlackCurve {
        curve: crate::FadeToBlackCurve,
    },
    /// Configures one fullscreen display-output slot (Protocol 2.42,
    /// OR-011; see [`crate::DisplayOutputConfig`]). A padlocked, undoable
    /// project edit applied live.
    SetDisplayOutput {
        slot: crate::DisplayOutputSlot,
        config: Box<crate::DisplayOutputConfig>,
    },
    /// Clears one fullscreen display-output slot (Protocol 2.42). A
    /// padlocked, undoable project edit applied live.
    ClearDisplayOutput {
        slot: crate::DisplayOutputSlot,
    },
    /// Reconfigures network stream input `input` (Protocol 2.46, IN-015;
    /// 2.49, IN-016/IN-017): a new URL (`None` keeps the current one),
    /// settings, the SRT stream ID, and edits of the RTMP stream key and SRT
    /// passphrase (which a client cannot read back, so it may keep them). A
    /// padlocked, undoable project edit applied live.
    SetNetworkStream {
        input: WireInputId,
        url: Option<crate::StreamUrl>,
        settings: crate::NetworkStreamSettings,
        stream_id: Option<String>,
        stream_key: crate::SecretEdit,
        passphrase: crate::SecretEdit,
    },
    /// Replaces capture input `input`'s settings (Protocol 2.47, IN-001).
    /// A padlocked, undoable project edit applied live.
    SetCaptureSettings {
        input: WireInputId,
        settings: Box<crate::CaptureSettings>,
    },
    /// Configures the `LiveLAN` viewer stream (Protocol 2.57, OR-017): whether
    /// it runs, what it shows, how many viewers it admits, and an edit of
    /// its access PIN (which a client cannot read back, so it may keep it).
    /// `config.has_pin` is not sent. A padlocked, undoable project edit
    /// applied live.
    SetLiveLan {
        config: crate::LiveLanEntry,
        pin: crate::SecretEdit,
    },
}

/// Most presets one animation preset library holds (Protocol 2.34).
pub const MAX_ANIMATION_PRESETS: u16 = 64;
/// Longest animation preset name, in UTF-8 bytes.
pub const MAX_ANIMATION_PRESET_NAME_BYTES: usize = 128;
/// Largest binary encoding of one preset motion, in bytes (the clip bound,
/// so the field stays within [`crate::MAX_FIELD_VALUE_BYTES`]).
pub const MAX_ANIMATION_PRESET_BYTES: usize = 36 * 1024;

/// One library animation preset on the wire (Protocol 2.34).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimationPresetEntry {
    pub preset: NonZeroU32,
    pub name: String,
    /// The phase it was saved from.
    pub phase: Phase,
    pub motion: Box<PhaseMotion>,
}

/// One library preset at a snapshot's `revision` (Protocol 2.34).
///
/// A session writes exactly [`SnapshotMessage::animation_preset_states`] of
/// these, in library order, after every other follow-on record of the
/// snapshot (its last `scene_layout_state`) and before any event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnimationPresetStateMessage {
    pub revision: u64,
    pub entry: AnimationPresetEntry,
}

/// What one accepted animation preset library edit changed
/// (`animation_preset_changed`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AnimationPresetChange {
    /// A preset was added at `index`, or (keeping its index) renamed: the
    /// whole entry afterwards and its position in the library.
    Set {
        index: u16,
        entry: AnimationPresetEntry,
    },
    Removed {
        preset: NonZeroU32,
    },
}

/// Largest `.cube` text one `grade_lut_import` carries, in bytes. The text
/// travels base64url-encoded (unpadded) in one field, so the bound keeps the
/// encoded field within [`crate::MAX_FIELD_VALUE_BYTES`]. Larger LUTs are
/// imported into the project bundle offline (for example with the CLI).
pub const MAX_GRADE_LUT_IMPORT_BYTES: usize = 32 * 1024;
/// Longest grade or LUT name, in UTF-8 bytes.
pub const MAX_GRADE_NAME_BYTES: usize = 128;
/// Most graded scene layers one project holds (Protocol 2.33).
pub const MAX_GRADE_LAYERS: u16 = 256;
/// Most `grade_state` records one snapshot announces: every LUT, library
/// grade, input grade, scene layer grade, and the program grade of a
/// maximal project.
pub const MAX_GRADE_STATES: u16 = 64 + 256 + 256 + MAX_GRADE_LAYERS + 1;

/// The live drag a grade set or clear belongs to (Protocol 2.56): wire
/// fields `gesture` (a canonical nonzero decimal token the client picks once
/// per drag, unique across sessions) and `continues` (`true` or `false`),
/// both absent outside a drag.
///
/// The server folds a continuing edit into the latest undo step when that
/// step grades the same target and was opened by the same token, keeping
/// the step's inverse (the grade before the drag), so a whole drag undoes
/// at once; a drag that ends where it started leaves no step. The first
/// edit of a drag sends `continues=false`, and a continuation whose drag
/// opened no step (its first edit was refused or lost) opens one.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct GradeGesture {
    pub token: core::num::NonZeroU64,
    pub continues: bool,
}

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
    /// The grade one scene layer carries (Protocol 2.33; by scene since
    /// 2.35).
    Layer {
        scene: WireSceneId,
        layer: WireSceneLayerId,
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
/// then every graded input in ascending input order, then (Protocol 2.33)
/// every graded scene layer (2.35: of any project scene), in scene then
/// layer order, then the program grade when there is one.
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
    /// A scene layer's grade afterwards (Protocol 2.33; by scene since
    /// 2.35); `None` when it is ungraded.
    Layer {
        scene: WireSceneId,
        layer: WireSceneLayerId,
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
        /// The typed reason (Protocol 2.45); `message` is its English
        /// fallback, and a refusal without one is worded from `message`.
        reason: Option<fm_command::RefusalReason>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputAudioStripStatus {
    pub input: WireInputId,
    pub gain_millidb: i32,
    pub balance_basis_points: i32,
    /// Mono source position (Protocol 2.40); balance trims stereo.
    pub pan_basis_points: i32,
    /// The strip's group, `1..=4` (Protocol 2.40).
    pub group: Option<u8>,
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
    /// Category, colour label, pause, close, and offline state (Protocol
    /// 2.38).
    pub lifecycle: crate::InputLifecycleStatus,
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
    /// Every Scene input in ascending input order (Protocol 2.31): one
    /// `scene_animation_state` follows for each.
    pub scenes: Vec<WireInputId>,
    /// Every project scene in ascending scene order, at most
    /// [`crate::MAX_LIST_ITEMS`] (Protocol 2.35): one `scene_layout_state`
    /// follows for each, after the `scene_animation_state` records.
    pub layout_scenes: Vec<WireSceneId>,
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
    /// How many [`AnimationPresetStateMessage`] records follow this
    /// snapshot (after its scene layout states), at most
    /// [`MAX_ANIMATION_PRESETS`] (Protocol 2.34).
    pub animation_preset_states: u16,
    /// The favourite transition buttons, slot 1 first (Protocol 2.36).
    pub transition_favorites: [TransitionFavoriteStatus; TRANSITION_FAVORITE_COUNT],
    /// How many [`crate::MediaStateMessage`] records follow this snapshot
    /// (after its animation preset states), one per media input in
    /// ascending input order, at most [`crate::MAX_MEDIA_STATES`] (Protocol
    /// 2.37).
    pub media_states: u16,
    /// How many [`crate::PagedStateMessage`] records follow this snapshot
    /// (IN-014): one per paged input, ascending by input, at most
    /// [`crate::MAX_PAGED_STATES`], last of its follow-on records (after
    /// the `script_state` records).
    pub paged_states: u16,
    /// How many [`crate::CueListStateMessage`] records follow this snapshot
    /// (after its media states), one per cue list in project order, at
    /// most [`crate::MAX_CUE_LISTS`] (Protocol 2.50).
    pub cue_list_states: u16,
    /// How many [`crate::ScriptStateMessage`] records follow this snapshot
    /// (after its cue list states), one per script in project order, at
    /// most [`crate::MAX_SCRIPTS`] (Protocol 2.58).
    pub script_states: u16,
    /// How many [`crate::ShortcutStateMessage`] records follow this
    /// snapshot (after its script states), one per shortcut in project
    /// order, at most [`crate::MAX_SHORTCUTS`] (Protocol 2.72).
    pub shortcut_states: u16,
    /// How many [`crate::StillStateMessage`] records follow this snapshot
    /// (after its shortcut states), one per image or slideshow input in
    /// ascending input order, at most [`crate::MAX_STILL_STATES`].
    pub still_states: u16,
    /// How many [`crate::MediaListStateMessage`] records follow this
    /// snapshot (after its still states), one per media list input in
    /// ascending input order, at most [`crate::MAX_MEDIA_LIST_STATES`]
    /// (IN-006).
    pub media_list_states: u16,
    /// The project's input category list in display order (Protocol 2.38).
    pub input_categories: Vec<crate::InputCategoryStatus>,
    /// Strip labels and groups (Protocol 2.40).
    pub audio_mixer: crate::AudioMixerStatus,
    /// The outputs Fade to Black leaves live, ascending (Protocol 2.40);
    /// empty when it fades every output.
    pub fade_to_black_excluded_outputs: Vec<WireOutputId>,
    /// The outputs that fade over their own duration in frames, ascending
    /// by output (Protocol 2.48).
    pub fade_to_black_output_durations: Vec<(WireOutputId, u32)>,
    /// How automatic Fade-to-Black moves ease (Protocol 2.48).
    pub fade_to_black_curve: crate::FadeToBlackCurve,
    /// Every configured display-output slot, ascending (Protocol 2.42).
    pub display_outputs: Vec<crate::DisplayOutputEntry>,
    /// Every configured slot's runtime state, ascending (Protocol 2.42).
    pub display_output_states: Vec<crate::DisplayOutputStatus>,
    /// Every network stream input, ascending, with its redacted config and
    /// latest status (Protocol 2.46).
    pub network_streams: Vec<crate::NetworkStreamEntry>,
    /// Every capture input, ascending, with its stable key, settings, and
    /// latest status (Protocol 2.47).
    pub capture_inputs: Vec<crate::CaptureInputEntry>,
    /// Every system audio input, ascending, with its device and latest
    /// status (Protocol 2.55, AU-011).
    pub system_audio_inputs: Vec<crate::SystemAudioEntry>,
    /// Every screen capture input, ascending, with its configuration and
    /// latest status (IN-011).
    pub screen_capture_inputs: Vec<crate::ScreenCaptureEntry>,
    /// Every configured stream destination in authored order, without its
    /// stream key (Protocol 2.52, OR-006).
    pub stream_targets: Vec<crate::StreamTargetEntry>,
    /// The `LiveLAN` configuration, without its PIN (Protocol 2.57, OR-017).
    pub live_lan: crate::LiveLanEntry,
    /// The `LiveLAN` stream's latest runtime status (Protocol 2.57).
    pub live_lan_status: crate::LiveLanStatus,
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
        /// The redacted configuration of an added network stream (Protocol
        /// 2.46); `None` for a generator.
        network_stream: Option<Box<crate::NetworkStreamConfig>>,
        /// The device and settings of an added capture input (Protocol
        /// 2.47); `None` for any other source.
        capture: Option<Box<crate::CaptureInputConfig>>,
        /// The device of an added system audio input (Protocol 2.55);
        /// `None` for any other source.
        system_audio: Option<Box<crate::SystemAudioConfig>>,
        /// The picture or slideshow of an added image or slideshow input
        /// (IN-008, IN-009); `None` for any other source.
        still: Option<Box<crate::WireStillSource>>,
        /// The configuration of an added screen capture input (IN-011);
        /// `None` for any other source.
        screen_capture: Option<Box<crate::ScreenCaptureConfig>>,
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
    /// Destination `target` was added, reconfigured (`entry` is `Some`), or
    /// removed (`None`), with the resulting roster (Protocol 2.52, OR-006).
    StreamTargetChanged {
        target: WireStreamTargetId,
        entry: Option<Box<crate::StreamTargetEntry>>,
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
    /// A cue list was created or edited (or the edit undone or redone):
    /// the whole list afterwards (Protocol 2.50).
    CueListChanged(Box<crate::CueListStatus>),
    /// A cue list was removed.
    CueListRemoved {
        list: u64,
    },
    /// A live cue list operation or cue clock advance was accepted.
    CueRunChanged(Box<crate::CueRunChange>),
    /// A script was created or edited (or the edit undone or redone): the
    /// whole script afterwards (Protocol 2.58).
    ScriptChanged(Box<crate::ScriptStatus>),
    /// A script was removed.
    ScriptRemoved {
        script: u64,
    },
    /// A script run, or one part of a run that waited, was accepted
    /// (Protocol 2.58).
    ScriptRan(Box<crate::ScriptRunChange>),
    /// A script's run changed on `frame` without a part running: it was
    /// stopped, its fault cleared, or the part due on `frame` failed
    /// (Protocol 2.62). `run` is the run afterwards.
    ScriptRunChanged {
        script: u64,
        frame: u64,
        run: crate::ScriptRunStatus,
    },
    /// Every script run was stopped and every fault cleared on `frame`
    /// (Protocol 2.69): `scripts` (ascending, nonzero, at most
    /// [`crate::MAX_SCRIPTS`]) were waiting or faulted and are idle now.
    ScriptRunsStopped {
        frame: u64,
        scripts: Vec<u64>,
    },
    /// A user shortcut was created or edited (or the edit undone or
    /// redone): the whole shortcut afterwards (Protocol 2.72).
    ShortcutChanged(Box<crate::ShortcutStatus>),
    /// A user shortcut was removed.
    ShortcutRemoved {
        shortcut: u64,
    },
    /// A fired shortcut's cue actions were accepted (Protocol 2.72).
    ShortcutFired(Box<crate::ShortcutFireChange>),
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
    /// A title animation edit (or its undo or redo) was accepted (Protocol
    /// 2.29): the template's complete clip and animation revision
    /// afterwards, and every title input (ascending) whose template it is.
    TitleAnimationChanged {
        inputs: Vec<WireInputId>,
        animation_revision: u64,
        clip: Box<WireTitleClip>,
    },
    /// A scene animation edit (or its undo or redo) was accepted (Protocol
    /// 2.31): the scene's complete clip and animation revision afterwards,
    /// and every Scene input (ascending) that shows the scene.
    SceneAnimationChanged {
        inputs: Vec<WireInputId>,
        animation_revision: u64,
        clip: Box<WireSceneClip>,
    },
    /// A live scene layer edit (or its undo or redo) was accepted (Protocol
    /// 2.39): the edit applied and the scene's whole layout afterwards.
    /// Every Scene input showing the scene shows the change.
    SceneLayoutChanged {
        edit: crate::SceneLayerEdit,
        layout: Box<WireSceneLayout>,
    },
    /// A mixer label, group, or group-level operation (or its undo or
    /// redo) was accepted (Protocol 2.40).
    AudioMixerChanged(crate::AudioMixerOperation),
    /// Fade to Black now fades `output` or leaves it live (Protocol 2.40).
    FadeToBlackOutputChanged {
        output: WireOutputId,
        affected: bool,
    },
    /// `output` now fades over its own duration, or with `None` over the
    /// requested one (Protocol 2.48).
    FadeToBlackOutputDurationChanged {
        output: WireOutputId,
        duration_frames: Option<u32>,
    },
    /// Automatic Fade-to-Black moves now ease along `curve` (Protocol 2.48).
    FadeToBlackCurveChanged {
        curve: crate::FadeToBlackCurve,
    },
    /// A display-output slot was configured (`Some`) or cleared (`None`),
    /// or that edit was undone or redone (Protocol 2.42).
    DisplayOutputChanged {
        slot: crate::DisplayOutputSlot,
        config: Option<Box<crate::DisplayOutputConfig>>,
    },
    /// The `LiveLAN` configuration changed, or that edit was undone or redone
    /// (Protocol 2.57). It carries whether a PIN is stored, never the PIN.
    LiveLanChanged(crate::LiveLanEntry),
    /// A network stream input was reconfigured, or that edit was undone or
    /// redone (Protocol 2.46). The URL is redacted.
    NetworkStreamChanged {
        input: WireInputId,
        config: Box<crate::NetworkStreamConfig>,
    },
    /// A capture input's settings changed, or that edit was undone or
    /// redone (Protocol 2.47).
    CaptureSettingsChanged {
        input: WireInputId,
        settings: Box<crate::CaptureSettings>,
    },
    /// An animation preset library edit (or its undo or redo) was accepted
    /// (Protocol 2.34).
    AnimationPresetChanged(AnimationPresetChange),
    /// A favourite transition button was assigned (or the assignment undone
    /// or redone) (Protocol 2.36): the slot and its favourite afterwards.
    TransitionFavoriteChanged {
        slot: u8,
        favorite: TransitionFavoriteStatus,
    },
    /// A media command was accepted for `frame` (Protocol 2.37): the
    /// operation and the input's complete status projected onto `frame`.
    MediaChanged {
        frame: u64,
        operation: crate::MediaOperation,
        status: Box<crate::MediaStatus>,
    },
    /// A page navigation was accepted for `frame` (IN-009); the slide it
    /// lands on follows as a `slideshow_page` runtime event.
    PageNavigated {
        input: WireInputId,
        frame: u64,
        navigation: crate::PagedNavigation,
    },
    /// A media list command was accepted for `frame` (IN-006): the
    /// operation's token, the name of the input a `create` appended, and
    /// the whole list afterwards (its active item is the one the input
    /// plays from `frame`).
    MediaListChanged {
        frame: u64,
        operation: &'static str,
        created: Option<String>,
        status: Box<crate::MediaListStatus>,
    },
    /// A `page_navigate` step of a document input was accepted (IN-014):
    /// the step and the input's complete status afterwards.
    PagedChanged {
        navigation: crate::PagedNavigation,
        status: Box<crate::PagedStatus>,
    },
    /// An input lifecycle edit was accepted (Protocol 2.38): the operation
    /// as executed (an undo or redo reports the edit it applied), the
    /// inputs it appended with their names, and the batch import items it
    /// refused, by item index.
    InputLifecycleChanged {
        operation: crate::InputLifecycleOperation,
        added: Vec<crate::AddedInput>,
        refused: Vec<(u16, crate::ImportRefusal)>,
    },
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
    /// A media input's transport changed on `frame` (Protocol 2.37): what
    /// happened (possibly nothing, for a periodic position sample) and the
    /// input's complete status afterwards.
    Media {
        frame: u64,
        events: Vec<crate::MediaEvent>,
        status: Box<crate::MediaStatus>,
    },
    /// A paged input's source changed (IN-014): its page count, state,
    /// title, or message, with the input's complete status.
    Paged {
        status: Box<crate::PagedStatus>,
    },
    /// An input's source became unavailable or available again (Protocol
    /// 2.38): `offline` is the reason, or `None` once it is back.
    InputAvailability {
        input: WireInputId,
        offline: Option<crate::InputOfflineReason>,
    },
    /// A display-output slot's runtime state changed (Protocol 2.42).
    DisplayOutputStatus(crate::DisplayOutputStatus),
    /// The connected displays changed (Protocol 2.42).
    DisplayInventory(crate::DisplayInventory),
    /// A network stream input's receive status changed (Protocol 2.46).
    NetworkStreamStatus {
        input: WireInputId,
        status: Box<crate::NetworkStreamStatus>,
    },
    /// A capture input's acquisition status changed (Protocol 2.47).
    CaptureStatus {
        input: WireInputId,
        status: Box<crate::CaptureStatus>,
    },
    /// A system audio input's capture status changed (Protocol 2.55).
    SystemAudioStatus {
        input: WireInputId,
        status: Box<crate::SystemAudioStatus>,
    },
    /// A slideshow shows another slide, holds another count of slides, or
    /// listed its folder anew (IN-009).
    SlideshowPage {
        input: WireInputId,
        status: Box<crate::SlideshowPageStatus>,
    },
    /// A screen capture input's status changed (IN-011).
    ScreenCaptureStatus {
        input: WireInputId,
        status: Box<crate::ScreenCaptureStatus>,
    },
    /// The capture devices the daemon can open changed (Protocol 2.47).
    CaptureDevices(crate::CaptureInventory),
    /// The `LiveLAN` stream's runtime status changed (Protocol 2.57).
    LiveLanStatus(crate::LiveLanStatus),
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

/// Whether the monitor bus is reaching its output device (2.53, AU-001).
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum AudioMonitorOutputState {
    /// `off`: no device is chosen; the bus is only metered.
    #[default]
    Off,
    /// `playing`: the bus is playing on the chosen device.
    Playing,
    /// `failed`: the chosen device could not be opened or stopped playing.
    /// Choosing the device again retries.
    Failed,
}

impl AudioMonitorOutputState {
    /// Every state in display order.
    pub const ALL: [Self; 3] = [Self::Off, Self::Playing, Self::Failed];

    /// The wire token: `off`, `playing`, or `failed`.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Playing => "playing",
            Self::Failed => "failed",
        }
    }

    /// Parses [`Self::token`].
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|state| state.token() == token)
    }
}

/// The monitor (headphone) bus meters of one interval (2.44, AU-001).
///
/// `solo` is set while the bus carries a PFL/AFL solo; otherwise it follows
/// Master. `output` (2.53) says whether the bus is reaching its output
/// device. On the wire this is
/// `monitor=<master|solo>~<off|playing|failed>~<channels>`, or `-` when the
/// sender has no monitor bus.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioMonitorMeters {
    pub solo: bool,
    /// Whether the bus is reaching its output device (2.53).
    pub output: AudioMonitorOutputState,
    /// One reading per Master channel, in Master channel order. The meters
    /// read the bus before the monitor output level and mute.
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
    /// The monitor bus, when the sender renders one (2.44).
    pub monitor: Option<AudioMonitorMeters>,
    /// Gain reduction of every chain with a running dynamics stage, Master
    /// first and then ascending by input (2.75).
    pub dynamics: Vec<crate::AudioDynamicsMeter>,
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
/// Counters are cumulative since the target's last start. `failure` is a
/// machine-readable [`crate::OutputFailure`] (Protocol 2.67), never free text,
/// URL text, or key material.
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
    /// Reconnects that delivered media again since the last start
    /// (Protocol 2.52).
    pub reconnects: u64,
    /// Milliseconds the current session has been delivering media, `0` when
    /// it is not (Protocol 2.52).
    pub uptime_ms: u64,
    /// Measured outgoing bitrate in kilobits per second over the last
    /// sampling window, `0` when nothing is flowing (Protocol 2.52).
    pub bitrate_kbps: u32,
    pub failure: Option<crate::OutputFailure>,
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
    /// Every running live source's drift loop (Protocol 2.76), in strictly
    /// increasing input order; empty without live sources.
    pub source_sync: Vec<crate::SourceSyncSample>,
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
    /// Lossy realized recorder status (Protocol 2.66).
    RecorderStatus(crate::RecorderStatusMessage),
    /// Lossy live source drift telemetry (Protocol 2.76).
    SourceSyncStatus(crate::SourceSyncStatusMessage),
    MonitorFrame(MonitorFrameMessage),
    CapabilityReport(CapabilityReportMessage),
    DiagnosticsRequest(DiagnosticsRequest),
    DiagnosticsResponse(DiagnosticsResponse),
    Error(ErrorMessage),
    TitleState(TitleStateMessage),
    PlaylistState(PlaylistStateMessage),
    GradeState(GradeStateMessage),
    TitleAnimationState(TitleAnimationStateMessage),
    TitleLayoutState(TitleLayoutStateMessage),
    SceneAnimationState(SceneAnimationStateMessage),
    SceneLayoutState(SceneLayoutStateMessage),
    TitlePreviewRequest(TitlePreviewRequest),
    TitlePreviewFrame(TitlePreviewFrameMessage),
    ScenePreviewRequest(ScenePreviewRequest),
    ScenePreviewFrame(ScenePreviewFrameMessage),
    AnimationPresetState(AnimationPresetStateMessage),
    /// One input strip's DSP chain after a snapshot (Protocol 2.75).
    AudioStripDspState(crate::AudioStripDspStateMessage),
    MediaState(crate::MediaStateMessage),
    /// One paged input after a snapshot (IN-014).
    PagedState(crate::PagedStateMessage),
    /// Asks for one page's notes or thumbnail (IN-014).
    PagedPageRequest(crate::PagedPageRequest),
    /// Answers a [`crate::PagedPageRequest`] (IN-014).
    PagedPage(crate::PagedPageMessage),
    /// One cue list after a snapshot (Protocol 2.50).
    CueListState(crate::CueListStateMessage),
    /// One script after a snapshot (Protocol 2.58).
    ScriptState(crate::ScriptStateMessage),
    /// One user shortcut after a snapshot (Protocol 2.72).
    ShortcutState(crate::ShortcutStateMessage),
    /// One media list after a snapshot (IN-006).
    MediaListState(crate::MediaListStateMessage),

    /// Asks for the current display inventory (Protocol 2.42).
    DisplayInventoryRequest(crate::DisplayInventoryRequest),
    /// Answers a [`crate::DisplayInventoryRequest`] (Protocol 2.42).
    DisplayInventory(crate::DisplayInventoryMessage),
    /// Asks for the daemon's capture devices (Protocol 2.47).
    CaptureDevicesRequest(crate::CaptureDevicesRequest),
    /// Answers a [`crate::CaptureDevicesRequest`] (Protocol 2.47).
    CaptureDevices(crate::CaptureDevicesMessage),
    /// Turns this session's input thumbnail feed on or off (Protocol 2.59).
    InputThumbnailSubscription(crate::InputThumbnailSubscription),
    /// One lossy live input bank thumbnail (Protocol 2.59).
    InputThumbnail(crate::InputThumbnailMessage),
    /// Asks for the server's system audio capture devices (Protocol 2.68).
    SystemAudioDevicesRequest(crate::SystemAudioDevicesRequest),
    /// Answers a [`crate::SystemAudioDevicesRequest`] (Protocol 2.68).
    SystemAudioDevices(crate::SystemAudioDevicesMessage),
    /// Manages the web gateway's paired devices (Protocol 2.71).
    WebAccessRequest(crate::WebAccessRequest),
    /// Answers a [`crate::WebAccessRequest`] (Protocol 2.71).
    WebAccess(crate::WebAccessMessage),
    /// One image or slideshow input after a snapshot (IN-008, IN-009).
    StillState(crate::StillStateMessage),
    /// Asks for a window of a slideshow's listing (IN-009).
    SlideshowSlidesRequest(crate::SlideshowSlidesRequest),
    /// Answers a [`crate::SlideshowSlidesRequest`] (IN-009).
    SlideshowSlides(crate::SlideshowSlidesMessage),
    /// Asks what the server can capture as screen capture inputs (IN-011).
    ScreenCaptureTargetsRequest(crate::ScreenCaptureTargetsRequest),
    /// Answers a [`crate::ScreenCaptureTargetsRequest`] (IN-011).
    ScreenCaptureTargets(crate::ScreenCaptureTargetsMessage),
}
