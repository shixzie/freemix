use std::{net::SocketAddr, path::PathBuf};

use fm_model::{
    DEFAULT_VIDEO_BITRATE_KBPS, StartupPolicy, StreamLadder, StreamLadderRung,
    StreamReconnectError, StreamReconnectPolicy,
};
use fm_types::capture::{
    CaptureAudio, CaptureError, CaptureFormat, CaptureInputConfig, CaptureLossSlate,
    CaptureSettings,
};
use fm_types::network_stream::{
    AudioChannels, GiveUp, NetworkStreamConfig, NetworkStreamError, NetworkStreamSettings,
    RtspTransport, SecretEdit, SrtKeyLength, StallSlate, StreamMode, StreamUrl,
};
use fm_types::paged::PagedNavigation;
use fm_types::screen_capture::{
    ScreenCaptureConfig, ScreenCaptureCrop, ScreenCaptureError, ScreenCaptureTarget, TitleMatch,
    WindowRule,
};
use fm_types::still::{
    ImageInputConfig, SlideOrder, SlideTransitionStyle, SlideshowConfig, SlideshowItem,
    SlideshowSource, StillInputError,
};
use fm_types::system_audio::{SystemAudioBackend, SystemAudioConfig, SystemAudioError};

/// How automatic Fade-to-Black moves ease (SW-004).
pub use fm_protocol::FadeToBlackCurve;
/// A manual (T-bar) transition kind: any timed mix, or a Stinger with its
/// clip length (SW-004).
pub use fm_protocol::ManualTransitionKind;

/// An overlay channel transition: Cut, Fade, or a motion kind (SW-005).
pub use fm_protocol::OverlayTransitionKind as OverlayTransition;
/// An automatic Program mix taken by kind (SW-002).
pub use fm_protocol::{AutoTransitionKind as TransitionKind, TransitionFavoriteEffect};

/// The CLI spelling of a protocol token: words joined by `-`, as every
/// other CLI choice is (`vertical-wipe`, `stinger-3`).
#[must_use]
pub fn cli_token(token: &str) -> String {
    token.replace('_', "-")
}

/// A media-file input's transport or playback setting (IN-003).
pub use fm_protocol::{MediaAudioPolicy, MediaEndAction, MediaOperation, MediaSeekTarget};

/// Where an `input-media-*` command goes: a stored project or a server.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MediaTarget {
    Local(PathBuf),
    Remote(SocketAddr),
}

/// The automatic mix a CLI token names.
pub fn transition_kind(value: &str) -> Result<TransitionKind, ArgsError> {
    TransitionKind::from_token(&value.replace('-', "_")).ok_or_else(|| ArgsError::InvalidChoice {
        field: "transition kind",
        value: value.to_owned(),
    })
}

/// The favourite effect a CLI token names: `cut`, a mix, or `stinger-N`.
pub fn transition_favorite_effect(value: &str) -> Result<TransitionFavoriteEffect, ArgsError> {
    TransitionFavoriteEffect::from_token(&value.replace('-', "_")).ok_or_else(|| {
        ArgsError::InvalidChoice {
            field: "favourite transition",
            value: value.to_owned(),
        }
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayPosition {
    FullFrame,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayBorder {
    None,
    ThinWhite,
    ThickWhite,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TBarAction {
    Start(ManualTransitionKind),
    SetPosition(u16),
    Commit,
    Cancel,
}

/// One parsed mixer label, group, or group-level command (AU-001).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AudioMixerArgs {
    /// `audio-label <input> <label>`; an empty label clears it.
    Label { input: u128, label: String },
    /// `audio-group <input> <1..4|none>`.
    Group { input: u128, group: Option<u8> },
    /// `audio-group-name <group> <name>`.
    GroupName { group: u8, name: String },
    /// `audio-group-level <group> <gain millidB> <muted on|off>`.
    GroupLevel {
        group: u8,
        gain_millidb: i32,
        muted: bool,
    },
    /// `audio-group-solo <group> <on|off>`.
    GroupSolo { group: u8, soloed: bool },
    /// `audio-group-parent <group> <1..4|none>`.
    GroupParent { group: u8, parent: Option<u8> },
    /// `audio-solo-mode <sip|pfl|afl>`.
    SoloMode { mode: fm_model::AudioSoloMode },
    /// `audio-monitor-device <device|none>`: the headphone device the
    /// monitor bus plays on.
    MonitorDevice { device: Option<String> },
    /// `audio-monitor-level <gain millidB> <muted on|off>`: the headphone
    /// feed's own level.
    MonitorLevel { gain_millidb: i32, muted: bool },
    /// `audio-strip-dsp <input|master> <preset|chain code>`: a strip's or
    /// the Master's DSP chain (AU-005); `input` is `None` for the Master.
    StripDsp {
        input: Option<u128>,
        chain: fm_types::strip_dsp::StripDsp,
    },
    /// `audio-loudness-reset <input|master>`: restarts a strip's or the
    /// Master's integrated loudness, loudness range, and maximum true peak
    /// (AU-005); `input` is `None` for the Master.
    LoudnessReset { input: Option<u128> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FadeToBlackTarget {
    Live,
    Black,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StingerAudioPolicy {
    Muted,
    StingerOnly,
    MixWithProgram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StingerFallback {
    Cut,
    Fade,
    KeepProgram,
}

impl FadeToBlackTarget {
    #[must_use]
    pub const fn active(self) -> bool {
        matches!(self, Self::Black)
    }
}

/// Everything `stream-add` and `stream-update` author about one destination.
///
/// The stream key arrives as a positional argument for the network protocols,
/// so it is visible in this machine's process list while the command runs.
/// That matches the bundle's trust model, which stores the key in plaintext in
/// `project.json`; it is still worth knowing before pasting a production key
/// on a shared host. An `hls:///absolute/dir` destination writes local
/// segments, carries no key at all, and therefore omits the key positional;
/// the repeated `--rung <video_bitrate_kbps>:<width>x<height>` option authors
/// that destination's multi-bitrate ladder, validated here as a whole. Once
/// parsed the key stops being printable: `Debug` here redacts it, and the
/// model type it becomes never renders it at all.
#[derive(Clone, Eq, PartialEq)]
pub struct StreamSpec {
    pub stream: u128,
    pub output: u128,
    pub url: String,
    pub key: Option<String>,
    pub name: String,
    pub backup_url: Option<String>,
    pub startup: StartupPolicy,
    pub video_bitrate_kbps: u32,
    pub ladder: Option<fm_model::StreamLadder>,
    /// The authored reconnect policy (OR-006); `--reconnect` or the default.
    pub reconnect: StreamReconnectPolicy,
    /// `--srt-latency-ms` (srt:// only); `None` is the 120 ms default.
    pub srt_latency_ms: Option<u32>,
    /// `--srt-passphrase` (srt:// only), a secret `Debug` redacts.
    pub srt_passphrase: Option<String>,
}

/// The options of `remote-stream-set` (OR-006): every field a destination
/// carries on the wire, each optional. For an existing destination an
/// omitted field keeps the daemon's current value; a new destination needs
/// `--name`, `--protocol` (or an endpoint URL with a scheme), `--endpoint`,
/// and `--output`. `--endpoint`/`--backup-endpoint` accept either the
/// scheme-relative endpoint (`host/app`, `/abs/dir`) or a full URL whose
/// scheme names the protocol. `stream_key` keeps, clears, or replaces the
/// stored key; `Debug` redacts a new key.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteStreamTargetOptions {
    pub name: Option<String>,
    pub protocol: Option<fm_protocol::StreamTargetProtocol>,
    pub endpoint: Option<String>,
    /// A new backup endpoint (`--backup-endpoint`).
    pub backup_endpoint: Option<String>,
    /// `--clear-backup-endpoint`: remove the backup endpoint.
    pub clear_backup_endpoint: bool,
    pub output: Option<u128>,
    pub video_bitrate_kbps: Option<u32>,
    pub reconcile_on_startup: Option<bool>,
    pub reconnect: Option<StreamReconnectPolicy>,
    pub stream_key: SecretEdit,
    /// `--srt-latency-ms` (an SRT destination only).
    pub srt_latency_ms: Option<u32>,
    /// `--srt-passphrase` / `--clear-srt-passphrase`; omitted keeps it.
    pub srt_passphrase: SecretEdit,
}

impl std::fmt::Debug for StreamSpec {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamSpec")
            .field("stream", &self.stream)
            .field("output", &self.output)
            .field("url", &self.url)
            .field("key", &self.key.as_ref().map(|_| "****"))
            .field("name", &self.name)
            .field("backup_url", &self.backup_url)
            .field("startup", &self.startup)
            .field("video_bitrate_kbps", &self.video_bitrate_kbps)
            .field("ladder", &self.ladder)
            .field("reconnect", &self.reconnect)
            .field("srt_latency_ms", &self.srt_latency_ms)
            .field(
                "srt_passphrase",
                &self.srt_passphrase.as_ref().map(|_| "****"),
            )
            .finish()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    New {
        path: PathBuf,
        name: String,
    },
    ProjectRename {
        path: PathBuf,
        name: String,
    },
    InputAdd {
        path: PathBuf,
        input: u128,
        name: String,
    },
    SimulatedSolidInputAdd {
        path: PathBuf,
        input: u128,
        name: String,
        red: u8,
        green: u8,
        blue: u8,
        alpha: u8,
    },
    MediaInputAdd {
        path: PathBuf,
        input: u128,
        name: String,
        asset_uri: String,
    },
    MediaInputRelink {
        path: PathBuf,
        input: u128,
        asset_uri: String,
    },
    TitleInputAdd {
        path: PathBuf,
        input: u128,
        name: String,
        template: PathBuf,
        /// `(family, font file)` pairs in command-line order.
        fonts: Vec<(String, PathBuf)>,
    },
    /// Applies a built-in animation preset to one element of a stored
    /// title template.
    TitleAnimationPreset {
        path: PathBuf,
        template_uri: String,
        element: u128,
        phase: TitlePhase,
        /// A stable `fm_animation` preset id, checked when applied.
        preset: String,
        frames: u32,
    },
    /// Scales every key of one phase of a stored title template.
    TitleAnimationStretch {
        path: PathBuf,
        template_uri: String,
        phase: TitlePhase,
        frames: u32,
    },
    /// Grades one input or the program output, inline or from the library.
    GradeSet {
        path: PathBuf,
        target: GradeTargetArg,
        /// Assigns this existing library grade instead of building one.
        library: Option<u32>,
        spec: GradeSpec,
        /// Saves the built grade to the library under this name and
        /// assigns it by reference.
        save_as: Option<String>,
    },
    /// Removes the grade of one input or the program output.
    GradeClear {
        path: PathBuf,
        target: GradeTargetArg,
    },
    /// Copies a `.cube` file into the bundle and registers it.
    GradeImportLut {
        path: PathBuf,
        file: PathBuf,
        name: Option<String>,
    },
    /// Lists imported LUTs, library grades, and grade assignments.
    GradeList {
        path: PathBuf,
    },
    /// Writes one library grade (and its LUT) to a `.fmgrade` file.
    GradeExport {
        path: PathBuf,
        grade: u32,
        file: PathBuf,
        /// Replaces an existing sibling `.cube` (`--replace-lut`).
        replace_lut: bool,
    },
    /// Adds the grade (and its LUT) in a `.fmgrade` file to the library.
    GradeImport {
        path: PathBuf,
        file: PathBuf,
        name: Option<String>,
    },
    /// Writes one user animation preset to a `.fmpreset` file.
    PresetExport {
        path: PathBuf,
        preset: u32,
        file: PathBuf,
    },
    /// Adds one title element's keys in one phase to the preset library.
    PresetSave {
        path: PathBuf,
        template_uri: String,
        element: u128,
        phase: TitlePhase,
        name: String,
    },
    /// Adds the animation preset in a `.fmpreset` file to the library.
    PresetImport {
        path: PathBuf,
        file: PathBuf,
        name: Option<String>,
    },
    /// Applies a built-in animation preset to one layer of a scene.
    SceneAnimationPreset {
        path: PathBuf,
        scene: u128,
        layer: u32,
        phase: TitlePhase,
        /// A stable `fm_animation` preset id, checked when applied.
        preset: String,
        frames: u32,
    },
    /// Scales every key of one phase of a scene's animation.
    SceneAnimationStretch {
        path: PathBuf,
        scene: u128,
        phase: TitlePhase,
        frames: u32,
    },
    /// Renders one scene posed at one frame of its animation.
    SceneRenderFrame {
        path: PathBuf,
        scene: u128,
        phase: ScenePhase,
        /// The frame of an In or Out phase; zero for the hold.
        frame: u32,
        output: PathBuf,
        width: u32,
        height: u32,
    },
    /// Renders one title input at one frame of one animation phase.
    TitleRenderFrame {
        path: PathBuf,
        input: u128,
        phase: TitlePhase,
        frame: u32,
        /// `--at`: the UTC instant production clocks read, as
        /// `YYYY-MM-DDTHH:MM:SSZ`; the Unix epoch when absent.
        at: Option<String>,
        output: PathBuf,
    },
    AudioBusAdd {
        path: PathBuf,
        bus: u128,
        name: String,
    },
    AudioBusRename {
        path: PathBuf,
        bus: u128,
        name: String,
    },
    AudioBusRemove {
        path: PathBuf,
        bus: u128,
    },
    AudioBusSend {
        path: PathBuf,
        source: u128,
        destination: u128,
        add: bool,
    },
    OutputAdd {
        path: PathBuf,
        output: u128,
        scene: u128,
        bus: u128,
        name: String,
    },
    OutputRoute {
        path: PathBuf,
        output: u128,
        scene: u128,
        bus: u128,
    },
    OutputRename {
        path: PathBuf,
        output: u128,
        name: String,
    },
    OutputStartup {
        path: PathBuf,
        output: u128,
        startup: StartupPolicy,
    },
    OutputRemove {
        path: PathBuf,
        output: u128,
    },
    StreamAdd {
        path: PathBuf,
        spec: StreamSpec,
    },
    StreamUpdate {
        path: PathBuf,
        spec: StreamSpec,
    },
    StreamRemove {
        path: PathBuf,
        stream: u128,
    },
    StreamStart {
        path: PathBuf,
        stream: u128,
    },
    StreamStop {
        path: PathBuf,
        stream: u128,
    },
    /// Starts every stream destination at once (OR-006); never recording.
    StreamStartAll {
        path: PathBuf,
    },
    /// Stops every stream destination at once (OR-006); never recording.
    StreamStopAll {
        path: PathBuf,
    },
    RecordStart {
        path: PathBuf,
        /// One-based recorder slot, validated to `1..=2` while parsing.
        recorder: u8,
    },
    RecordStop {
        path: PathBuf,
        /// One-based recorder slot, validated to `1..=2` while parsing.
        recorder: u8,
    },
    Streams {
        path: PathBuf,
    },
    SceneInputAdd {
        path: PathBuf,
        input: u128,
        scene: u128,
        name: String,
    },
    SceneInputDuplicate {
        path: PathBuf,
        source_scene: u128,
        new_scene: u128,
        new_input: u128,
        scene_name: String,
        input_name: String,
    },
    SceneInputAudioSource {
        path: PathBuf,
        scene_input: u128,
        audio_source: u128,
    },
    SceneInputAudioSourceClear {
        path: PathBuf,
        scene_input: u128,
    },
    SceneInputRemove {
        path: PathBuf,
        scene_input: u128,
    },
    SceneBackground {
        path: PathBuf,
        scene: u128,
        red: u8,
        green: u8,
        blue: u8,
        alpha: u8,
    },
    SceneRemove {
        path: PathBuf,
        scene: u128,
    },
    SceneRename {
        path: PathBuf,
        scene: u128,
        name: String,
    },
    SceneLayerAdd {
        path: PathBuf,
        scene: u128,
        source: u128,
        z_order: i32,
        name: String,
    },
    SceneLayerAddScene {
        path: PathBuf,
        scene: u128,
        source_scene: u128,
        z_order: i32,
        name: String,
    },
    SceneLayerRemove {
        path: PathBuf,
        scene: u128,
        index: usize,
    },
    SceneLayerMove {
        path: PathBuf,
        scene: u128,
        source_index: usize,
        destination_index: usize,
    },
    SceneLayerDuplicate {
        path: PathBuf,
        scene: u128,
        index: usize,
        name: String,
    },
    SceneLayerCopy {
        path: PathBuf,
        source_scene: u128,
        source_index: usize,
        target_scene: u128,
        name: String,
    },
    SceneLayerRename {
        path: PathBuf,
        scene: u128,
        index: usize,
        name: String,
    },
    SceneLayerSourceInput {
        path: PathBuf,
        scene: u128,
        index: usize,
        input: u128,
    },
    SceneLayerSourceScene {
        path: PathBuf,
        scene: u128,
        index: usize,
        source_scene: u128,
    },
    SceneLayerZOrder {
        path: PathBuf,
        scene: u128,
        index: usize,
        z_order: i32,
    },
    SceneLayerAppearance {
        path: PathBuf,
        scene: u128,
        index: usize,
        enabled: bool,
        opacity: u8,
    },
    SceneLayerGeometry {
        path: PathBuf,
        scene: u128,
        index: usize,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        /// Clockwise turn, any angle, in millidegrees.
        rotation_millidegrees: i32,
        /// Scale and rotation pivot in basis points of the box (centre when
        /// absent).
        anchor: Option<(u16, u16)>,
    },
    SceneLayerCrop {
        path: PathBuf,
        scene: u128,
        index: usize,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    SceneLayerCropClear {
        path: PathBuf,
        scene: u128,
        index: usize,
    },
    SceneLayerMask {
        path: PathBuf,
        scene: u128,
        index: usize,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        inverted: bool,
    },
    SceneLayerMaskClear {
        path: PathBuf,
        scene: u128,
        index: usize,
    },
    SceneLayerEffectAdd {
        path: PathBuf,
        scene: u128,
        index: usize,
        name: String,
        parameters: Vec<i32>,
    },
    SceneLayerEffectSet {
        path: PathBuf,
        scene: u128,
        index: usize,
        effect_index: usize,
        name: String,
        parameters: Vec<i32>,
    },
    SceneLayerEffectRemove {
        path: PathBuf,
        scene: u128,
        index: usize,
        effect_index: usize,
    },
    SceneLayerEffectClear {
        path: PathBuf,
        scene: u128,
        index: usize,
    },
    /// Sets or clears one layer's chroma or luma key (IN-020).
    SceneLayerKey {
        path: PathBuf,
        scene: u128,
        index: usize,
        layer_key: Option<fm_model::LayerKey>,
    },
    /// Live-edits one layer's key on a daemon (IN-020, Protocol 2.43).
    RemoteSceneLayerKey {
        address: SocketAddr,
        scene: u128,
        layer: u32,
        layer_key: Option<fm_model::LayerKey>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Sets one layer's whole chroma key from its input's picture (SW-008):
    /// the dominant green or blue backing, or the colour at `point`.
    SceneLayerAutoKey {
        path: PathBuf,
        scene: u128,
        index: usize,
        /// A binary PPM to sample instead of the layer's simulated input.
        picture: Option<PathBuf>,
        point: Option<(u32, u32)>,
    },
    /// Live-sets one layer's chroma key on a daemon from a saved picture
    /// of its input (SW-008), as one `scene_layer_key_set` edit.
    RemoteSceneLayerAutoKey {
        address: SocketAddr,
        scene: u128,
        layer: u32,
        picture: PathBuf,
        point: Option<(u32, u32)>,
        /// The key's clip, choke, and edge softness (the despill is the
        /// auto key's).
        refinement: fm_model::KeyRefinement,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Live-replaces one layer's effect list on a daemon (IN-020, Protocol
    /// 2.43).
    RemoteSceneLayerEffects {
        address: SocketAddr,
        scene: u128,
        layer: u32,
        effects: Vec<fm_model::LayerEffect>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    InputRemove {
        path: PathBuf,
        input: u128,
    },
    InputDuplicate {
        path: PathBuf,
        source: u128,
        input: u128,
        name: String,
    },
    InputReplaceSimulated {
        path: PathBuf,
        input: u128,
    },
    InputReplaceSolid {
        path: PathBuf,
        input: u128,
        red: u8,
        green: u8,
        blue: u8,
        alpha: u8,
    },
    InputReplaceMedia {
        path: PathBuf,
        input: u128,
        asset_uri: String,
    },
    InputReplaceScene {
        path: PathBuf,
        input: u128,
        scene: u128,
    },
    Status {
        path: PathBuf,
    },
    JournalRecover {
        path: PathBuf,
    },
    Inputs {
        path: PathBuf,
    },
    /// Lists the automatic mixes and overlay transitions by CLI token.
    TransitionKinds,
    /// Lists the project's favourite transition buttons.
    TransitionFavorites {
        path: PathBuf,
    },
    /// Takes an automatic mix by kind and settles it.
    Transition {
        path: PathBuf,
        kind: TransitionKind,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Lists the media-file inputs' playback settings.
    InputMediaList {
        path: PathBuf,
    },
    /// One input lifecycle edit (IN-026), locally (category list,
    /// category, colour label, pause, close) or on a server (also
    /// duplicate, source replacement, and batch import).
    InputLifecycle {
        target: MediaTarget,
        operation: fm_protocol::InputLifecycleOperation,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Imports media files into the bundle as new media inputs, numbered
    /// from `first_input` (IN-026). Refused files are reported per file.
    InputImportBatch {
        path: PathBuf,
        first_input: u128,
        files: Vec<PathBuf>,
    },
    /// Drives one media-file input's transport or edits its playback
    /// settings, locally or on a server (IN-003).
    InputMedia {
        target: MediaTarget,
        input: u128,
        operation: MediaOperation,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Turns a document input's page in a saved project (IN-014); a
    /// running server takes `remote-page-navigate`.
    InputPage {
        path: PathBuf,
        input: u128,
        navigation: fm_types::paged::PagedNavigation,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Lists the document inputs' pages and page counts (IN-014).
    InputPages {
        path: PathBuf,
    },
    /// Assigns one favourite transition button.
    TransitionFavoriteSet {
        path: PathBuf,
        slot: u8,
        effect: TransitionFavoriteEffect,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Renders Program (from) mixing to Preview (to) at an exact progress.
    TransitionRenderFrame {
        path: PathBuf,
        kind: TransitionKind,
        numerator: u32,
        denominator: u32,
        output: PathBuf,
        width: u32,
        height: u32,
    },
    RemoteTransition {
        address: SocketAddr,
        kind: TransitionKind,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteTransitionFavoriteSet {
        address: SocketAddr,
        slot: u8,
        effect: TransitionFavoriteEffect,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    Outputs {
        path: PathBuf,
    },
    AudioBuses {
        path: PathBuf,
    },
    Scenes {
        path: PathBuf,
    },
    AssetAudit {
        path: PathBuf,
    },
    AudioStrip {
        path: PathBuf,
        input: u128,
        gain_millidb: i32,
        balance_basis_points: i32,
        pan_basis_points: i32,
        muted: bool,
        soloed: bool,
        follow_video: bool,
        delay_samples: u32,
    },
    Rename {
        path: PathBuf,
        input: u128,
        name: String,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    Reorder {
        path: PathBuf,
        inputs: Vec<u128>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    Preview {
        path: PathBuf,
        input: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    Cut {
        path: PathBuf,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    Fade {
        path: PathBuf,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    AlphaFade {
        path: PathBuf,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    Slide {
        path: PathBuf,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    Zoom {
        path: PathBuf,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    Stinger {
        path: PathBuf,
        slot: u8,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    StingerConfigure {
        path: PathBuf,
        slot: u8,
        media_input: u128,
        preload: bool,
        cut_point_frames: u32,
        audio_policy: StingerAudioPolicy,
        fallback: StingerFallback,
    },
    StingerRemove {
        path: PathBuf,
        slot: u8,
    },
    OverlayTake {
        path: PathBuf,
        channel: u8,
        source: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    OverlayUpdate {
        path: PathBuf,
        channel: u8,
        source: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    OverlayOff {
        path: PathBuf,
        channel: u8,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    OverlayOutput {
        path: PathBuf,
        channel: u8,
        output: u128,
        included: bool,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    OverlayTransition {
        path: PathBuf,
        channel: u8,
        transition: OverlayTransition,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    OverlayAppearance {
        path: PathBuf,
        channel: u8,
        position: OverlayPosition,
        border: OverlayBorder,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    OverlayQueue {
        path: PathBuf,
        channel: u8,
        source: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    OverlayNext {
        path: PathBuf,
        channel: u8,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    Wipe {
        path: PathBuf,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    TBar {
        path: PathBuf,
        action: TBarAction,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    FadeToBlack {
        path: PathBuf,
        target: FadeToBlackTarget,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Chooses whether Fade to Black fades an output (SW-004).
    FadeToBlackOutput {
        path: PathBuf,
        output: u128,
        affected: bool,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteFadeToBlackOutput {
        address: SocketAddr,
        output: u128,
        affected: bool,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Gives an output its own Fade-to-Black duration, or (`None`) returns
    /// it to the shared one (SW-004).
    FadeToBlackOutputDuration {
        path: PathBuf,
        output: u128,
        duration_frames: Option<u32>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteFadeToBlackOutputDuration {
        address: SocketAddr,
        output: u128,
        duration_frames: Option<u32>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Chooses the Fade-to-Black curve (SW-004).
    FadeToBlackCurve {
        path: PathBuf,
        curve: FadeToBlackCurve,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteFadeToBlackCurve {
        address: SocketAddr,
        curve: FadeToBlackCurve,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Labels a strip, assigns it to a group, renames a group, or moves a
    /// group fader (AU-001).
    AudioMixer {
        path: PathBuf,
        edit: AudioMixerArgs,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteAudioMixer {
        address: SocketAddr,
        edit: AudioMixerArgs,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteStatus {
        address: SocketAddr,
    },
    RemoteDiagnostics {
        address: SocketAddr,
    },
    /// Configures one fullscreen display-output slot (OR-011).
    DisplayOutputSet {
        path: PathBuf,
        slot: fm_types::display::DisplayOutputSlot,
        config: Box<fm_types::display::DisplayOutputConfig>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteDisplayOutputSet {
        address: SocketAddr,
        slot: fm_types::display::DisplayOutputSlot,
        config: Box<fm_types::display::DisplayOutputConfig>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Clears one fullscreen display-output slot (OR-011).
    DisplayOutputClear {
        path: PathBuf,
        slot: fm_types::display::DisplayOutputSlot,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteDisplayOutputClear {
        address: SocketAddr,
        slot: fm_types::display::DisplayOutputSlot,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Lists the configured display-output slots and their state.
    DisplayOutputList {
        path: PathBuf,
    },
    RemoteDisplayOutputList {
        address: SocketAddr,
    },
    /// Lists the displays a daemon's presenter sees (Protocol 2.42).
    RemoteDisplayInventory {
        address: SocketAddr,
    },
    /// Prints what a project records for instant replay (OR-012, Protocol 2.106).
    Replay {
        path: PathBuf,
    },
    /// Prints a daemon's replay recording, events, and Replay inputs
    /// (Protocol 2.106).
    RemoteReplay {
        address: SocketAddr,
    },
    /// Edits what is recorded for replay; absent options keep their value
    /// (Protocol 2.106).
    ReplaySet {
        path: PathBuf,
        edit: ReplayEdit,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Edits what a daemon records for replay (Protocol 2.106).
    RemoteReplaySet {
        address: SocketAddr,
        edit: ReplayEdit,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Sends one live replay command: a mark, a capture, an event edit, or
    /// a Replay input's transport (Protocol 2.106).
    RemoteReplayDo {
        address: SocketAddr,
        operation: fm_protocol::ReplayOperation,
        key: Option<String>,
    },
    /// Prints a project's stored highlight reels (OR-013, Protocol 2.107).
    Reel {
        path: PathBuf,
    },
    /// Prints a daemon's highlight reels and export jobs (Protocol 2.107).
    RemoteReel {
        address: SocketAddr,
    },
    /// Replaces or creates a highlight reel (Protocol 2.107).
    ReelSet {
        path: PathBuf,
        spec: Box<fm_protocol::ReelSpec>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Replaces or creates a daemon's highlight reel (Protocol 2.107).
    RemoteReelSet {
        address: SocketAddr,
        spec: Box<fm_protocol::ReelSpec>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Deletes a highlight reel (Protocol 2.107).
    ReelDelete {
        path: PathBuf,
        id: u64,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Deletes a daemon's highlight reel (Protocol 2.107).
    RemoteReelDelete {
        address: SocketAddr,
        id: u64,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Starts a daemon's reel export (Protocol 2.107).
    RemoteReelExport {
        address: SocketAddr,
        job: u64,
        reel: u64,
        destination: String,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Cancels a daemon's reel export (Protocol 2.107).
    RemoteReelCancel {
        address: SocketAddr,
        job: u64,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Forgets a daemon's terminal reel export (Protocol 2.107).
    RemoteReelForget {
        address: SocketAddr,
        job: u64,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Prints the `LiveLAN` viewer stream's settings (OR-017).
    LiveLan {
        path: PathBuf,
    },
    /// Prints a daemon's `LiveLAN` settings and status (Protocol 2.57).
    RemoteLiveLan {
        address: SocketAddr,
    },
    /// Edits the `LiveLAN` viewer stream; absent options keep their value.
    LiveLanSet {
        path: PathBuf,
        edit: LiveLanEdit,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteLiveLanSet {
        address: SocketAddr,
        edit: LiveLanEdit,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteAudioStrip {
        address: SocketAddr,
        input: u128,
        gain_millidb: i32,
        balance_basis_points: i32,
        pan_basis_points: i32,
        muted: bool,
        soloed: bool,
        follow_video: bool,
        delay_samples: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteRename {
        address: SocketAddr,
        input: u128,
        name: String,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteInputAdd {
        address: SocketAddr,
        input: u128,
        name: String,
        source: fm_protocol::LiveInputSource,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Adds a network stream input to a project (IN-015).
    NetworkStreamInputAdd {
        path: PathBuf,
        input: u128,
        name: String,
        config: Box<NetworkStreamConfig>,
    },
    /// Adds a network stream input to a running daemon (IN-015).
    RemoteNetworkStreamInputAdd {
        address: SocketAddr,
        input: u128,
        name: String,
        config: Box<NetworkStreamConfig>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Edits a project's network stream input; omitted options keep the
    /// input's current values.
    NetworkStreamSet {
        path: PathBuf,
        input: u128,
        options: NetworkStreamOptions,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Edits a running daemon's network stream input; omitted options keep
    /// the input's current values.
    RemoteNetworkStreamSet {
        address: SocketAddr,
        input: u128,
        options: NetworkStreamOptions,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Lists a project's network stream inputs (idle: nothing receives).
    NetworkStreamStatus {
        path: PathBuf,
    },
    /// Lists a running daemon's network stream inputs with their status.
    RemoteNetworkStreamStatus {
        address: SocketAddr,
    },
    /// Adds a capture device input to a project (IN-001).
    CaptureInputAdd {
        path: PathBuf,
        input: u128,
        name: String,
        config: Box<CaptureInputConfig>,
    },
    /// Adds a capture device input to a running daemon (IN-001).
    RemoteCaptureInputAdd {
        address: SocketAddr,
        input: u128,
        name: String,
        config: Box<CaptureInputConfig>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Edits a project's capture input settings; omitted options keep the
    /// input's current values (IN-001).
    CaptureSet {
        path: PathBuf,
        input: u128,
        options: CaptureOptions,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Edits a running daemon's capture input settings; omitted options
    /// keep the input's current values (IN-001).
    RemoteCaptureSet {
        address: SocketAddr,
        input: u128,
        options: CaptureOptions,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Lists a project's capture inputs (idle: nothing acquires them).
    CaptureStatus {
        path: PathBuf,
    },
    /// Lists a running daemon's capture inputs with their status.
    RemoteCaptureStatus {
        address: SocketAddr,
    },
    /// Lists the capture devices a running daemon can open, with their
    /// formats (IN-001).
    RemoteCaptureDevices {
        address: SocketAddr,
    },
    /// Lists the system audio capture devices of a running daemon's host
    /// (AU-011, Protocol 2.68).
    RemoteSystemAudioDevices {
        address: SocketAddr,
    },
    /// Lists the headphone output devices of a running daemon's host
    /// (AU-001, Protocol 2.102).
    RemoteAudioOutputDevices {
        address: SocketAddr,
    },
    /// Adds a screen or window capture input to a project (IN-011).
    ScreenCaptureInputAdd {
        path: PathBuf,
        input: u128,
        name: String,
        config: Box<ScreenCaptureConfig>,
    },
    /// Adds a screen or window capture input to a running daemon (IN-011).
    RemoteScreenCaptureInputAdd {
        address: SocketAddr,
        input: u128,
        name: String,
        config: Box<ScreenCaptureConfig>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Lists a project's screen capture inputs (idle: nothing captures).
    ScreenCaptureStatus {
        path: PathBuf,
    },
    /// Lists a running daemon's screen capture inputs with their status.
    RemoteScreenCaptureStatus {
        address: SocketAddr,
    },
    /// Lists the screens and windows a running daemon can capture
    /// (IN-011).
    RemoteScreenCaptureTargets {
        address: SocketAddr,
    },
    /// Asks a running daemon's desktop to pick a screen capture input's
    /// screen or window again (IN-011, Protocol 2.85).
    RemoteScreenCapturePick {
        address: SocketAddr,
        input: u128,
    },
    /// Adds a system audio input to a project (AU-011).
    SystemAudioInputAdd {
        path: PathBuf,
        input: u128,
        name: String,
        config: Box<SystemAudioConfig>,
    },
    /// Adds a system audio input to a running daemon (AU-011).
    RemoteSystemAudioInputAdd {
        address: SocketAddr,
        input: u128,
        name: String,
        config: Box<SystemAudioConfig>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Lists a project's system audio inputs (idle: nothing captures).
    SystemAudioStatus {
        path: PathBuf,
    },
    /// Lists a running daemon's system audio inputs with their status.
    RemoteSystemAudioStatus {
        address: SocketAddr,
    },
    /// Adds a still image input to a project (IN-008); a host path is
    /// copied into the project's assets first.
    ImageInputAdd {
        path: PathBuf,
        input: u128,
        name: String,
        config: Box<ImageInputConfig>,
    },
    /// Adds a still image input to a running daemon (IN-008); the daemon
    /// copies a host path into its project.
    RemoteImageInputAdd {
        address: SocketAddr,
        input: u128,
        name: String,
        config: Box<ImageInputConfig>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Adds a slideshow input to a project (IN-009); host paths and a host
    /// folder are copied into the project's assets first.
    SlideshowInputAdd {
        path: PathBuf,
        input: u128,
        name: String,
        config: Box<SlideshowConfig>,
    },
    /// Adds a slideshow input to a running daemon (IN-009).
    RemoteSlideshowInputAdd {
        address: SocketAddr,
        input: u128,
        name: String,
        config: Box<SlideshowConfig>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Steps a running daemon's paged input (a slideshow's slides).
    RemotePageNavigate {
        address: SocketAddr,
        input: u128,
        navigation: PagedNavigation,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteInputRemove {
        address: SocketAddr,
        input: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteReorder {
        address: SocketAddr,
        inputs: Vec<u128>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemotePreview {
        address: SocketAddr,
        input: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteCut {
        address: SocketAddr,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// A cue list edit or cue control applied to a local project (RC-017),
    /// or a project shortcut fire (RC-004).
    Cue {
        path: PathBuf,
        payload: fm_protocol::CommandPayload,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// A cue list edit or cue control sent to a running daemon (RC-017), or
    /// a project shortcut fire (RC-004).
    RemoteCue {
        address: SocketAddr,
        payload: fm_protocol::CommandPayload,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Prints a local project's cue lists (RC-017).
    CueLists {
        path: PathBuf,
    },
    /// Prints a running daemon's cue lists with their run state (RC-017).
    RemoteCueLists {
        address: SocketAddr,
    },
    /// Prints a local project's scripts (RC-011).
    Scripts {
        path: PathBuf,
    },
    /// Prints a running daemon's scripts (RC-011).
    RemoteScripts {
        address: SocketAddr,
    },
    /// A script edit or run on a local project (RC-011).
    Script {
        path: PathBuf,
        command: ScriptCommand,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Lists, exports, or imports a local project's shortcut set (RC-004).
    Shortcuts {
        path: PathBuf,
        command: ShortcutSetCommand,
    },
    /// Lists, exports, or imports a running daemon's shortcut set (RC-004).
    RemoteShortcuts {
        address: SocketAddr,
        command: ShortcutSetCommand,
    },
    /// A script edit or run sent to a running daemon (RC-011).
    RemoteScript {
        address: SocketAddr,
        command: ScriptCommand,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// A playlist edit or control sent to a running daemon.
    RemotePlaylist {
        address: SocketAddr,
        payload: fm_protocol::CommandPayload,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteUndo {
        address: SocketAddr,
        redo: bool,
        expected_edit: Option<u64>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteLock {
        address: SocketAddr,
        locked: bool,
        input: Option<u128>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteFade {
        address: SocketAddr,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteAlphaFade {
        address: SocketAddr,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteSlide {
        address: SocketAddr,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteZoom {
        address: SocketAddr,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteStinger {
        address: SocketAddr,
        slot: u8,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteStingerConfigure {
        address: SocketAddr,
        slot: u8,
        media_input: u128,
        preload: bool,
        cut_point_frames: u32,
        audio_policy: StingerAudioPolicy,
        fallback: StingerFallback,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteStingerRemove {
        address: SocketAddr,
        slot: u8,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteOverlayTake {
        address: SocketAddr,
        channel: u8,
        source: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteOverlayUpdate {
        address: SocketAddr,
        channel: u8,
        source: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteOverlayOff {
        address: SocketAddr,
        channel: u8,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteOverlayOutput {
        address: SocketAddr,
        channel: u8,
        output: u128,
        included: bool,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteOverlayTransition {
        address: SocketAddr,
        channel: u8,
        transition: OverlayTransition,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteOverlayAppearance {
        address: SocketAddr,
        channel: u8,
        position: OverlayPosition,
        border: OverlayBorder,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteOverlayQueue {
        address: SocketAddr,
        channel: u8,
        source: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteOverlayNext {
        address: SocketAddr,
        channel: u8,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteWipe {
        address: SocketAddr,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteTBar {
        address: SocketAddr,
        action: TBarAction,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteFadeToBlack {
        address: SocketAddr,
        target: FadeToBlackTarget,
        frames: u32,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteStreamStart {
        address: SocketAddr,
        stream: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteStreamStop {
        address: SocketAddr,
        stream: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Reconnects one desired-running destination now (OR-006).
    RemoteStreamRetry {
        address: SocketAddr,
        stream: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Starts every destination of a running daemon (OR-006).
    RemoteStreamStartAll {
        address: SocketAddr,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Stops every destination of a running daemon (OR-006).
    RemoteStreamStopAll {
        address: SocketAddr,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Adds or reconfigures one destination of a running daemon (OR-006).
    RemoteStreamSet {
        address: SocketAddr,
        stream: u128,
        options: Box<RemoteStreamTargetOptions>,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Removes one destination of a running daemon (OR-006).
    RemoteStreamRemove {
        address: SocketAddr,
        stream: u128,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    /// Lists a running daemon's destinations with their latest status.
    RemoteStreams {
        address: SocketAddr,
    },
    RemoteRecordStart {
        address: SocketAddr,
        /// One-based recorder slot, validated to `1..=2` while parsing.
        recorder: u8,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    RemoteRecordStop {
        address: SocketAddr,
        /// One-based recorder slot, validated to `1..=2` while parsing.
        recorder: u8,
        key: Option<String>,
        expected_revision: Option<u64>,
    },
    Render {
        path: PathBuf,
        output: PathBuf,
        width: u32,
        height: u32,
    },
    Demo {
        path: PathBuf,
        output: Option<PathBuf>,
    },
    ProjectExportBundle {
        source: PathBuf,
        destination: PathBuf,
    },
    ProjectFromTemplate {
        template: PathBuf,
        destination: PathBuf,
        name: String,
    },
    ProjectAppend {
        target: PathBuf,
        source: PathBuf,
    },
    ProjectImportMedia {
        path: PathBuf,
        file: PathBuf,
        /// Also add a media input with this id referencing the imported asset.
        input: Option<u128>,
        /// Name for that input; defaults to the imported file's stem.
        name: Option<String>,
    },
    ProjectRelink {
        path: PathBuf,
        search: Vec<PathBuf>,
        apply: bool,
    },
    ProjectAutosave {
        path: PathBuf,
        keep: usize,
    },
    ProjectRecover {
        path: PathBuf,
        restore: bool,
    },
    RecentProjects {
        file: PathBuf,
    },
    RecentProjectsAdd {
        file: PathBuf,
        project: PathBuf,
        capacity: usize,
    },
    RecentProjectsRemove {
        file: PathBuf,
        project: PathBuf,
    },
    RecentProjectsPrune {
        file: PathBuf,
    },
    Help,
}

/// One project script command (RC-011).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptCommand {
    /// Creates or replaces script `script` with `name` and the text of the
    /// file `source` (`-` reads standard input).
    Set {
        script: u64,
        name: String,
        source: PathBuf,
    },
    Remove {
        script: u64,
    },
    Run {
        script: ScriptRef,
    },
    /// Stops run `run` paused at a wait, or clears its failed run's
    /// report; with no `run`, stops every run and clears every report
    /// (`script_stop`, Protocol 2.62; `--run` since 2.69).
    Stop {
        run: Option<u64>,
    },
}

/// One shortcut set command (RC-004).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ShortcutSetCommand {
    /// Prints every shortcut.
    List,
    /// Writes every shortcut to the `.fmshortcuts` file `file`, under the
    /// set name `name` (the show name by default).
    Export { file: PathBuf, name: Option<String> },
    /// Merges the set in the `.fmshortcuts` file `file` into the project.
    Import { file: PathBuf },
}

/// A script named on the command line: its id, or else its exact name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScriptRef {
    Id(u64),
    Name(String),
}

impl ScriptRef {
    fn parse(value: String) -> Self {
        match value.parse::<u64>() {
            Ok(id) if id != 0 => Self::Id(id),
            _ => Self::Name(value),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArgsError {
    MissingCommand,
    MissingValue(&'static str),
    /// A secret option was followed by something that looks like another
    /// option (`--...`); the value is never echoed.
    SecretLooksLikeOption(&'static str),
    BlankValue(&'static str),
    /// HLS writes into one local directory and has no failover endpoint.
    BackupNotAllowed,
    /// RTMP/RTMPS/SRT publish one rendition, so a multi-bitrate ladder has no
    /// meaning there.
    LadderNotAllowed,
    /// `--srt-latency-ms` or `--srt-passphrase` on a destination other than
    /// `srt://`.
    SrtOptionNotAllowed(&'static str),
    /// The authored rungs are individually shaped but invalid as a ladder.
    InvalidLadder(fm_model::StreamLadderError),
    /// A well-shaped `--reconnect` policy is out of bounds (OR-006).
    InvalidReconnect(StreamReconnectError),
    InvalidNumber {
        field: &'static str,
        value: String,
    },
    InvalidChoice {
        field: &'static str,
        value: String,
    },
    /// The `--recorder` value is not one of the two program recorder slots.
    InvalidRecorder(String),
    OutOfRange {
        field: &'static str,
        minimum: u16,
        maximum: u16,
        value: u16,
    },
    UnexpectedArgument(String),
    UnknownOption(String),
    UnknownCommand(String),
    /// The input kind cannot be added to a running daemon.
    NotLiveInputKind(String),
    /// An option is only meaningful together with another one.
    RequiresOption {
        option: &'static str,
        requires: &'static str,
    },
    /// An option cannot be combined with others.
    ConflictingOptions {
        option: &'static str,
        conflicts: &'static str,
    },
    /// A display-output spec does not parse (OR-011).
    InvalidDisplayOutput(String),
    /// A `LiveLAN` PIN is not 4 to 12 digits; the value is never echoed.
    InvalidLiveLanPin,
    /// Replay settings are unusable (OR-012).
    InvalidReplay(fm_protocol::ReplaySettingsError),
    /// A highlight reel is unusable (OR-013).
    InvalidReel(fm_protocol::ReelSpecError),
    /// A `--clips` record is not `<event>/<angle>/<in>/<out>/<speed>`.
    InvalidReelClip(String),
    /// A reel export destination is not an absolute, bounded path.
    InvalidReelDestination,
    /// One option is given more than once.
    DuplicateOption(&'static str),
    /// A network stream URL or setting is unusable (IN-015). Never names
    /// the URL, which may carry credentials.
    InvalidNetworkStream(NetworkStreamError),
    /// A capture device key or setting is unusable (IN-001).
    InvalidCapture(CaptureError),
    /// A system audio device name is unusable (AU-011).
    InvalidSystemAudio(SystemAudioError),
    /// An image or slideshow setting is unusable (IN-008, IN-009).
    InvalidStill(StillInputError),
    /// A screen capture target or setting is unusable (IN-011).
    InvalidScreenCapture(ScreenCaptureError),
}

impl core::fmt::Display for ArgsError {
    #[allow(clippy::too_many_lines)] // one arm per argument error
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingCommand => formatter.write_str("a command is required"),
            Self::MissingValue(field) => write!(formatter, "missing value for {field}"),
            Self::SecretLooksLikeOption(option) => write!(
                formatter,
                "missing value for {option}: it is followed by another option, and a secret \
                 value cannot start with `--`"
            ),
            Self::BlankValue(field) => write!(formatter, "{field} must not be blank"),
            Self::BackupNotAllowed => {
                formatter.write_str("hls:// stream destinations do not support --backup")
            }
            Self::LadderNotAllowed => {
                formatter.write_str("only hls:// stream destinations support --rung")
            }
            Self::SrtOptionNotAllowed(option) => {
                write!(
                    formatter,
                    "only srt:// stream destinations support {option}"
                )
            }
            Self::InvalidLadder(error) => write!(formatter, "{error}"),
            Self::InvalidReconnect(error) => write!(formatter, "{error}"),
            Self::InvalidLiveLanPin => write!(
                formatter,
                "--pin must be {}-{} digits",
                fm_types::live_lan::MIN_LIVE_LAN_PIN_DIGITS,
                fm_types::live_lan::MAX_LIVE_LAN_PIN_DIGITS
            ),
            Self::InvalidReplay(error) => write!(formatter, "invalid replay settings: {error}"),
            Self::InvalidReel(error) => write!(formatter, "invalid reel: {error}"),
            Self::InvalidReelClip(value) => write!(
                formatter,
                "invalid reel clip `{value}`; expected \
                 <event>/<angle>/<trim-in>/<trim-out>/<speed>"
            ),
            Self::InvalidReelDestination => write!(
                formatter,
                "a reel export destination must be an absolute path of at most {} bytes",
                fm_protocol::MAX_REEL_PATH_BYTES
            ),
            Self::DuplicateOption(option) => {
                write!(formatter, "{option} is given more than once")
            }
            Self::InvalidDisplayOutput(error) => {
                write!(formatter, "invalid display-output spec: {error}")
            }
            Self::InvalidNetworkStream(error) => {
                write!(formatter, "invalid network stream: {error}")
            }
            Self::InvalidCapture(error) => write!(formatter, "invalid capture input: {error}"),
            Self::InvalidSystemAudio(error) => {
                write!(formatter, "invalid system audio input: {error}")
            }
            Self::InvalidStill(error) => write!(formatter, "invalid picture input: {error}"),
            Self::InvalidScreenCapture(error) => {
                write!(formatter, "invalid screen capture input: {error}")
            }
            Self::InvalidNumber { field, value } | Self::InvalidChoice { field, value } => {
                write!(formatter, "invalid {field} value `{value}`")
            }
            Self::InvalidRecorder(value) => write!(
                formatter,
                "invalid value for --recorder `{value}`; expected 1 or 2"
            ),
            Self::OutOfRange {
                field,
                minimum,
                maximum,
                value,
            } => write!(
                formatter,
                "{field} must be in {minimum}..={maximum}, got {value}"
            ),
            Self::UnexpectedArgument(argument) => {
                write!(formatter, "unexpected argument `{argument}`")
            }
            // No option takes an attached `=value`, and the value may be a
            // secret (`--stream-key=...`), so it is never echoed.
            Self::UnknownOption(option) => match option.split_once('=') {
                Some((name, _)) if name.starts_with('-') => write!(
                    formatter,
                    "unknown option `{name}=<value>`; give the value as the next argument"
                ),
                _ => write!(formatter, "unknown option `{option}`"),
            },
            Self::UnknownCommand(command) => write!(formatter, "unknown command `{command}`"),
            Self::NotLiveInputKind(kind) => write!(
                formatter,
                "input kind `{kind}` cannot be added to a running daemon: only solid and bars \
                 inputs are realized live; add other kinds offline with input-add, \
                 media-input-add or the other offline add commands"
            ),
            Self::RequiresOption { option, requires } => {
                write!(formatter, "{option} requires {requires}")
            }
            Self::ConflictingOptions { option, conflicts } => {
                write!(formatter, "{option} cannot be combined with {conflicts}")
            }
        }
    }
}

impl std::error::Error for ArgsError {}

#[allow(clippy::too_many_lines)]
pub fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Command, ArgsError> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Err(ArgsError::MissingCommand);
    };
    match command.as_str() {
        "new" => parse_new(arguments),
        "project-rename" => parse_project_rename(arguments),
        "input-add" => parse_input_add(arguments),
        "simulated-solid-input-add" => parse_simulated_solid_input_add(arguments),
        "media-input-add" => parse_media_input_add(arguments),
        "media-input-relink" => parse_media_input_relink(arguments),
        "title-input-add" => parse_title_input_add(arguments),
        "title-animation-preset" => parse_title_animation_preset(arguments),
        "title-animation-stretch" => parse_title_animation_stretch(arguments),
        "title-render-frame" => parse_title_render_frame(arguments),
        "scene-animation-preset" => parse_scene_animation_preset(arguments),
        "scene-animation-stretch" => parse_scene_animation_stretch(arguments),
        "scene-render-frame" => parse_scene_render_frame(arguments),
        "grade-set" => parse_grade_set(arguments),
        "grade-clear" => parse_grade_clear(arguments),
        "grade-import-lut" => parse_grade_import_lut(arguments),
        "grade-list" => parse_grade_list(arguments),
        "grade-export" => parse_grade_export(arguments),
        "grade-import" => parse_shared_import(arguments, "grade", |path, file, name| {
            Command::GradeImport { path, file, name }
        }),
        "preset-export" => parse_shared_export(arguments, "preset", |path, preset, file| {
            Command::PresetExport { path, preset, file }
        }),
        "preset-save" => parse_preset_save(arguments),
        "preset-import" => parse_shared_import(arguments, "preset", |path, file, name| {
            Command::PresetImport { path, file, name }
        }),
        "audio-bus-add" => parse_audio_bus_add(arguments),
        "audio-bus-rename" => parse_audio_bus_rename(arguments),
        "audio-bus-remove" => parse_audio_bus_remove(arguments),
        "audio-bus-send-add" => parse_audio_bus_send(arguments, true),
        "audio-bus-send-remove" => parse_audio_bus_send(arguments, false),
        "output-add" => parse_output_add(arguments),
        "output-remove" => parse_output_remove(arguments),
        "output-route" => parse_output_route(arguments),
        "output-rename" => parse_output_rename(arguments),
        "output-startup" => parse_output_startup(arguments),
        "stream-add" => parse_stream(arguments, |path, spec| Command::StreamAdd { path, spec }),
        "stream-update" => {
            parse_stream(arguments, |path, spec| Command::StreamUpdate { path, spec })
        }
        "stream-remove" => parse_stream_remove(arguments),
        "stream-start" => parse_stream_running(arguments, true),
        "stream-stop" => parse_stream_running(arguments, false),
        "stream-start-all" | "stream-stop-all" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(if command == "stream-start-all" {
                Command::StreamStartAll { path }
            } else {
                Command::StreamStopAll { path }
            })
        }
        "record-start" => parse_record_desired(arguments, true),
        "record-stop" => parse_record_desired(arguments, false),
        "scene-input-add" => parse_scene_input_add(arguments),
        "scene-input-duplicate" => parse_scene_input_duplicate(arguments),
        "scene-input-audio-source" => parse_scene_input_audio_source(arguments),
        "scene-input-audio-source-clear" => parse_scene_input_audio_source_clear(arguments),
        "scene-input-remove" => parse_scene_input_remove(arguments),
        "scene-background" => parse_scene_background(arguments),
        "scene-remove" => parse_scene_remove(arguments),
        "scene-rename" => parse_scene_rename(arguments),
        "scene-layer-add" => parse_scene_layer_add(arguments),
        "scene-layer-add-scene" => parse_scene_layer_add_scene(arguments),
        "scene-layer-remove" => parse_scene_layer_remove(arguments),
        "scene-layer-move" => parse_scene_layer_move(arguments),
        "scene-layer-duplicate" => parse_scene_layer_duplicate(arguments),
        "scene-layer-copy" => parse_scene_layer_copy(arguments),
        "scene-layer-rename" => parse_scene_layer_rename(arguments),
        "scene-layer-source-input" => parse_scene_layer_source_input(arguments),
        "scene-layer-source-scene" => parse_scene_layer_source_scene(arguments),
        "scene-layer-z-order" => parse_scene_layer_z_order(arguments),
        "scene-layer-appearance" => parse_scene_layer_appearance(arguments),
        "scene-layer-geometry" => parse_scene_layer_geometry(arguments),
        "scene-layer-crop" => parse_scene_layer_crop(arguments),
        "scene-layer-crop-clear" => parse_scene_layer_crop_clear(arguments),
        "scene-layer-mask" => parse_scene_layer_mask(arguments),
        "scene-layer-mask-clear" => parse_scene_layer_mask_clear(arguments),
        "scene-layer-effect-add" => parse_scene_layer_effect(arguments, EffectEdit::Add),
        "scene-layer-effect-set" => parse_scene_layer_effect(arguments, EffectEdit::Set),
        "scene-layer-effect-remove" => parse_scene_layer_effect_remove(arguments),
        "scene-layer-effect-clear" => parse_scene_layer_effect_clear(arguments),
        "scene-layer-key" => parse_scene_layer_key(arguments),
        "remote-scene-layer-key" => parse_remote_scene_layer_key(arguments),
        "scene-layer-auto-key" => parse_scene_layer_auto_key(arguments),
        "remote-scene-layer-auto-key" => parse_remote_scene_layer_auto_key(arguments),
        "remote-scene-layer-effects" => parse_remote_scene_layer_effects(arguments),
        "input-remove" => parse_input_remove(arguments),
        "input-duplicate" => parse_input_duplicate(arguments),
        "input-replace-simulated" => parse_input_replace_simulated(arguments),
        "input-replace-solid" => parse_input_replace_solid(arguments),
        "input-replace-media" => parse_input_replace_media(arguments),
        "input-replace-scene" => parse_input_replace_scene(arguments),
        "status" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::Status { path })
        }
        "journal-recover" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::JournalRecover { path })
        }
        "inputs" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::Inputs { path })
        }
        "transition-kinds" => {
            reject_extra(&mut arguments)?;
            Ok(Command::TransitionKinds)
        }
        "transition-favorites" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::TransitionFavorites { path })
        }
        "transition" => {
            let path = required_path(&mut arguments, "project path")?;
            let kind = transition_kind(&required(&mut arguments, "transition kind")?)?;
            let frames = number(&required(&mut arguments, "frames")?, "frames")?;
            let (key, expected_revision) = command_options(arguments)?;
            Ok(Command::Transition {
                path,
                kind,
                frames,
                key,
                expected_revision,
            })
        }
        "remote-transition" => {
            let address = socket_address(&required(&mut arguments, "address")?)?;
            let kind = transition_kind(&required(&mut arguments, "transition kind")?)?;
            let frames = number(&required(&mut arguments, "frames")?, "frames")?;
            let (key, expected_revision) = command_options(arguments)?;
            Ok(Command::RemoteTransition {
                address,
                kind,
                frames,
                key,
                expected_revision,
            })
        }
        "transition-favorite-set" => {
            let path = required_path(&mut arguments, "project path")?;
            let (slot, effect, frames) = favorite_fields(&mut arguments)?;
            let (key, expected_revision) = command_options(arguments)?;
            Ok(Command::TransitionFavoriteSet {
                path,
                slot,
                effect,
                frames,
                key,
                expected_revision,
            })
        }
        "remote-transition-favorite-set" => {
            let address = socket_address(&required(&mut arguments, "address")?)?;
            let (slot, effect, frames) = favorite_fields(&mut arguments)?;
            let (key, expected_revision) = command_options(arguments)?;
            Ok(Command::RemoteTransitionFavoriteSet {
                address,
                slot,
                effect,
                frames,
                key,
                expected_revision,
            })
        }
        "transition-render-frame" => parse_transition_render_frame(arguments),
        "outputs" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::Outputs { path })
        }
        "streams" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::Streams { path })
        }
        "audio-buses" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::AudioBuses { path })
        }
        "scenes" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::Scenes { path })
        }
        "asset-audit" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::AssetAudit { path })
        }
        "audio-strip" => parse_audio_strip(arguments),
        mixer @ ("audio-label"
        | "audio-group"
        | "audio-group-name"
        | "audio-group-level"
        | "audio-group-solo"
        | "audio-group-parent"
        | "audio-solo-mode"
        | "audio-monitor-device"
        | "audio-monitor-level"
        | "audio-strip-dsp"
        | "audio-loudness-reset"
        | "remote-audio-label"
        | "remote-audio-group"
        | "remote-audio-group-name"
        | "remote-audio-group-level"
        | "remote-audio-group-solo"
        | "remote-audio-group-parent"
        | "remote-audio-solo-mode"
        | "remote-audio-monitor-device"
        | "remote-audio-monitor-level"
        | "remote-audio-strip-dsp"
        | "remote-audio-loudness-reset") => parse_audio_mixer(mixer, arguments),
        "remote-audio-output-devices" => {
            let address = socket_address(&required(&mut arguments, "address")?)?;
            reject_extra(&mut arguments)?;
            Ok(Command::RemoteAudioOutputDevices { address })
        }
        output @ ("ftb-output" | "remote-ftb-output") => {
            parse_fade_to_black_output(output, arguments)
        }
        timing @ ("ftb-output-duration"
        | "remote-ftb-output-duration"
        | "ftb-curve"
        | "remote-ftb-curve") => parse_fade_to_black_timing(timing, arguments),
        "rename" => parse_rename(arguments),
        "input-reorder" => parse_reorder(arguments, false),
        "preview" => {
            let path = required_path(&mut arguments, "project path")?;
            let input = number(&required(&mut arguments, "input")?, "input")?;
            let (key, expected_revision) = command_options(arguments)?;
            Ok(Command::Preview {
                path,
                input,
                key,
                expected_revision,
            })
        }
        "cut" => {
            let path = required_path(&mut arguments, "project path")?;
            let (key, expected_revision) = command_options(arguments)?;
            Ok(Command::Cut {
                path,
                key,
                expected_revision,
            })
        }
        "fade" | "alpha-fade" | "slide" | "zoom" | "wipe" => {
            parse_local_timed_transition(&command, arguments)
        }
        "stinger" => parse_local_stinger(arguments),
        "stinger-configure" => parse_stinger_configuration(arguments),
        "stinger-remove" => parse_stinger_removal(arguments),
        "overlay-take" | "overlay-update" | "overlay-off" | "overlay-output"
        | "overlay-transition" | "overlay-appearance" | "overlay-queue" | "overlay-next" => {
            parse_local_overlay(&command, arguments)
        }
        "tbar-start" | "tbar-position" | "tbar-commit" | "tbar-cancel" => {
            parse_local_t_bar(&command, arguments)
        }
        "ftb" => parse_local_fade_to_black(arguments),
        "remote-status" => parse_remote_status(arguments),
        "remote-diagnostics" => parse_remote_diagnostics(arguments),
        display @ ("display-output-set"
        | "display-output-clear"
        | "display-output-list"
        | "remote-display-output-set"
        | "remote-display-output-clear"
        | "remote-display-output-list"
        | "remote-display-inventory") => parse_display_output(display, arguments),
        live @ ("live-lan" | "remote-live-lan" | "live-lan-set" | "remote-live-lan-set") => {
            parse_live_lan(live, arguments)
        }
        replay @ ("replay" | "remote-replay" | "replay-set" | "remote-replay-set") => {
            parse_replay(replay, arguments)
        }
        "remote-replay-do" => parse_remote_replay_do(arguments),
        reel @ ("reel" | "reel-set" | "reel-delete" | "remote-reel" | "remote-reel-set"
        | "remote-reel-delete" | "remote-reel-export" | "remote-reel-cancel"
        | "remote-reel-forget") => parse_reel(reel, arguments),
        "remote-audio-strip" => parse_remote_audio_strip(arguments),
        "remote-rename" => parse_remote_rename(arguments),
        "remote-input-reorder" => parse_reorder(arguments, true),
        "remote-input-add" => parse_remote_input_add(arguments),
        stream @ ("input-add-stream"
        | "remote-input-add-stream"
        | "input-stream-set"
        | "remote-input-stream-set"
        | "input-stream-status"
        | "remote-input-stream-status") => parse_network_stream(stream, arguments),
        capture @ ("input-add-capture"
        | "remote-input-add-capture"
        | "input-capture-set"
        | "remote-input-capture-set"
        | "input-capture-status"
        | "remote-input-capture-status"
        | "remote-capture-devices") => parse_capture(capture, arguments),
        system_audio @ ("input-add-system-audio"
        | "remote-input-add-system-audio"
        | "input-system-audio-status"
        | "remote-input-system-audio-status"
        | "remote-system-audio-devices") => parse_system_audio(system_audio, arguments),
        still @ ("input-add-image"
        | "remote-input-add-image"
        | "input-add-slideshow"
        | "remote-input-add-slideshow") => parse_still_input_add(still, arguments),
        "remote-page-navigate" => parse_remote_page_navigate(arguments),
        screen_capture @ ("input-add-screen-capture"
        | "remote-input-add-screen-capture"
        | "input-screen-capture-status"
        | "remote-input-screen-capture-status"
        | "remote-screen-capture-targets"
        | "remote-screen-capture-pick") => parse_screen_capture(screen_capture, arguments),
        "remote-input-remove" => parse_remote_input_remove(arguments),
        "remote-preview" => parse_remote_preview(arguments),
        "remote-cut" => parse_remote_cut(arguments),
        "remote-playlist" => parse_remote_playlist(arguments),
        cue @ ("cue-lists"
        | "remote-cue-lists"
        | "cue-list-set"
        | "remote-cue-list-set"
        | "cue-list-remove"
        | "remote-cue-list-remove"
        | "go"
        | "remote-go"
        | "cue-standby"
        | "remote-cue-standby"
        | "cue-back"
        | "remote-cue-back"
        | "cue-skip"
        | "remote-cue-skip"
        | "cue-stop"
        | "remote-cue-stop"
        | "cue-reset"
        | "remote-cue-reset"
        | "cue-arm"
        | "remote-cue-arm"
        | "cue-disarm"
        | "remote-cue-disarm") => parse_cue(cue, arguments),
        shortcut @ ("shortcuts"
        | "remote-shortcuts"
        | "shortcut-export"
        | "remote-shortcut-export"
        | "shortcut-import"
        | "remote-shortcut-import") => parse_shortcut_set(shortcut, arguments),
        fire @ ("shortcut-fire" | "remote-shortcut-fire") => parse_shortcut_fire(fire, arguments),
        script @ ("scripts"
        | "remote-scripts"
        | "script-set"
        | "remote-script-set"
        | "script-remove"
        | "remote-script-remove"
        | "script-run"
        | "remote-script-run"
        | "script-stop"
        | "remote-script-stop") => parse_script(script, arguments),
        "remote-undo" => parse_remote_history(false, arguments),
        "remote-redo" => parse_remote_history(true, arguments),
        "remote-lock" => parse_remote_lock(arguments),
        "remote-fade" | "remote-alpha-fade" | "remote-slide" | "remote-zoom" | "remote-wipe" => {
            parse_remote_timed_transition(&command, arguments)
        }
        "remote-stinger" => parse_remote_stinger(arguments),
        "remote-stinger-configure" => parse_remote_stinger_configuration(arguments),
        "remote-stinger-remove" => parse_remote_stinger_removal(arguments),
        "remote-overlay-take"
        | "remote-overlay-update"
        | "remote-overlay-off"
        | "remote-overlay-output"
        | "remote-overlay-transition"
        | "remote-overlay-appearance"
        | "remote-overlay-queue"
        | "remote-overlay-next" => parse_remote_overlay(&command, arguments),
        "remote-tbar-start"
        | "remote-tbar-position"
        | "remote-tbar-commit"
        | "remote-tbar-cancel" => parse_remote_t_bar(&command, arguments),
        "remote-ftb" => parse_remote_fade_to_black(arguments),
        "remote-stream-start" => parse_remote_stream_running(arguments, true),
        "remote-stream-stop" => parse_remote_stream_running(arguments, false),
        "remote-stream-retry" | "remote-stream-remove" => {
            let address = socket_address(&required(&mut arguments, "address")?)?;
            let stream = number(&required(&mut arguments, "stream")?, "stream")?;
            let (key, expected_revision) = command_options(arguments)?;
            Ok(if command == "remote-stream-retry" {
                Command::RemoteStreamRetry {
                    address,
                    stream,
                    key,
                    expected_revision,
                }
            } else {
                Command::RemoteStreamRemove {
                    address,
                    stream,
                    key,
                    expected_revision,
                }
            })
        }
        "remote-stream-start-all" | "remote-stream-stop-all" => {
            let address = socket_address(&required(&mut arguments, "address")?)?;
            let (key, expected_revision) = command_options(arguments)?;
            Ok(if command == "remote-stream-start-all" {
                Command::RemoteStreamStartAll {
                    address,
                    key,
                    expected_revision,
                }
            } else {
                Command::RemoteStreamStopAll {
                    address,
                    key,
                    expected_revision,
                }
            })
        }
        "remote-stream-set" => parse_remote_stream_set(arguments),
        "remote-streams" => {
            let address = socket_address(&required(&mut arguments, "address")?)?;
            reject_extra(&mut arguments)?;
            Ok(Command::RemoteStreams { address })
        }
        "remote-record-start" => parse_remote_record_desired(arguments, true),
        "remote-record-stop" => parse_remote_record_desired(arguments, false),
        "render" => {
            let path = required_path(&mut arguments, "project path")?;
            let output = required_path(&mut arguments, "output path")?;
            let mut width = 640;
            let mut height = 360;
            while let Some(option) = arguments.next() {
                match option.as_str() {
                    "--width" => {
                        width = number(&required(&mut arguments, "width")?, "width")?;
                    }
                    "--height" => {
                        height = number(&required(&mut arguments, "height")?, "height")?;
                    }
                    _ => return Err(ArgsError::UnknownOption(option)),
                }
            }
            Ok(Command::Render {
                path,
                output,
                width,
                height,
            })
        }
        "demo" => {
            let path = required_path(&mut arguments, "project path")?;
            let output = arguments.next().map(PathBuf::from);
            reject_extra(&mut arguments)?;
            Ok(Command::Demo { path, output })
        }
        "project-export-bundle" => {
            let source = required_path(&mut arguments, "source project path")?;
            let destination = required_path(&mut arguments, "destination project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::ProjectExportBundle {
                source,
                destination,
            })
        }
        "project-from-template" => parse_project_from_template(arguments),
        "project-append" => {
            let target = required_path(&mut arguments, "target project path")?;
            let source = required_path(&mut arguments, "source project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::ProjectAppend { target, source })
        }
        "project-import-media" => parse_project_import_media(arguments),
        "project-relink" => parse_project_relink(arguments),
        "project-autosave" => parse_project_autosave(arguments),
        "project-recover" => {
            let path = required_path(&mut arguments, "project path")?;
            let mut restore = false;
            for option in arguments {
                match option.as_str() {
                    "--restore" => restore = true,
                    _ => return Err(ArgsError::UnknownOption(option)),
                }
            }
            Ok(Command::ProjectRecover { path, restore })
        }
        "recent-projects" => {
            let file = required_path(&mut arguments, "recent-projects file")?;
            reject_extra(&mut arguments)?;
            Ok(Command::RecentProjects { file })
        }
        "recent-projects-add" => parse_recent_projects_add(arguments),
        "recent-projects-remove" => {
            let file = required_path(&mut arguments, "recent-projects file")?;
            let project = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::RecentProjectsRemove { file, project })
        }
        "recent-projects-prune" => {
            let file = required_path(&mut arguments, "recent-projects file")?;
            reject_extra(&mut arguments)?;
            Ok(Command::RecentProjectsPrune { file })
        }
        "input-media-list" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::InputMediaList { path })
        }
        "input-import-batch" => {
            let path = required_path(&mut arguments, "project path")?;
            let first_input = number(&required(&mut arguments, "first input")?, "first input")?;
            let files = arguments.map(PathBuf::from).collect::<Vec<_>>();
            if files.is_empty() {
                return Err(ArgsError::MissingValue("media file"));
            }
            Ok(Command::InputImportBatch {
                path,
                first_input,
                files,
            })
        }
        "input-categories" | "input-category" | "input-color" | "input-pause" | "input-close" => {
            let path = required_path(&mut arguments, "project path")?;
            let operation = command.strip_prefix("input-").unwrap_or_default();
            parse_input_lifecycle(MediaTarget::Local(path), operation, arguments)
        }
        "remote-input-categories"
        | "remote-input-category"
        | "remote-input-color"
        | "remote-input-pause"
        | "remote-input-close"
        | "remote-input-duplicate"
        | "remote-input-replace"
        | "remote-input-import-batch" => {
            let address = socket_address(&required(&mut arguments, "address")?)?;
            let operation = command.strip_prefix("remote-input-").unwrap_or_default();
            parse_input_lifecycle(MediaTarget::Remote(address), operation, arguments)
        }
        "input-page" => {
            let path = required_path(&mut arguments, "project path")?;
            parse_input_page(path, arguments)
        }
        "input-pages" => {
            let path = required_path(&mut arguments, "project path")?;
            reject_extra(&mut arguments)?;
            Ok(Command::InputPages { path })
        }
        "help" | "--help" | "-h" => Ok(Command::Help),
        _ => {
            if let Some(operation) = command.strip_prefix("remote-input-media-") {
                let address = socket_address(&required(&mut arguments, "address")?)?;
                parse_input_media(MediaTarget::Remote(address), operation, arguments)
            } else if let Some(operation) = command.strip_prefix("input-media-") {
                let path = required_path(&mut arguments, "project path")?;
                parse_input_media(MediaTarget::Local(path), operation, arguments)
            } else {
                Err(ArgsError::UnknownCommand(command))
            }
        }
    }
}

fn parse_output_route(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let output = number(&required(&mut arguments, "output")?, "output")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let bus = number(&required(&mut arguments, "bus")?, "bus")?;
    reject_extra(&mut arguments)?;
    Ok(Command::OutputRoute {
        path,
        output,
        scene,
        bus,
    })
}

fn parse_output_startup(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let output = number(&required(&mut arguments, "output")?, "output")?;
    let startup = startup_policy(&required(&mut arguments, "startup policy")?)?;
    reject_extra(&mut arguments)?;
    Ok(Command::OutputStartup {
        path,
        output,
        startup,
    })
}

/// The options `stream-add` and `stream-update` accept after their
/// positionals.
const STREAM_OPTIONS: [&str; 7] = [
    "--reconnect",
    "--backup",
    "--startup",
    "--video-bitrate-kbps",
    "--rung",
    "--srt-latency-ms",
    "--srt-passphrase",
];

/// One non-blank `stream-add`/`stream-update` positional. One of the
/// command's own options there means the positional was left out:
/// `stream-add ... <url> --reconnect 1:2:3` must not store `--reconnect` as
/// the key and `1:2:3` as the name.
fn stream_positional(
    arguments: &mut impl Iterator<Item = String>,
    field: &'static str,
) -> Result<String, ArgsError> {
    let value = nonblank(arguments, field)?;
    if STREAM_OPTIONS.contains(&value.as_str()) {
        return Err(ArgsError::MissingValue(field));
    }
    Ok(value)
}

fn parse_stream(
    mut arguments: impl Iterator<Item = String>,
    build: fn(PathBuf, StreamSpec) -> Command,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let stream = number(&required(&mut arguments, "stream")?, "stream")?;
    let output = number(&required(&mut arguments, "output")?, "output")?;
    let url = nonblank(&mut arguments, "stream url")?;
    // HLS writes to a local directory and carries no stream key, so its
    // positional shape is `<url> <name>`; every network protocol keeps the
    // existing `<url> <key> <name>`.
    let hls = url.starts_with("hls://");
    let key = if hls {
        None
    } else {
        Some(stream_positional(&mut arguments, "stream key")?)
    };
    let name = stream_positional(&mut arguments, "stream name")?;
    let mut backup_url = None;
    let mut startup = StartupPolicy::Stopped;
    let mut video_bitrate_kbps = DEFAULT_VIDEO_BITRATE_KBPS;
    let mut rungs = Vec::new();
    let mut reconnect = StreamReconnectPolicy::DEFAULT;
    let srt = url.starts_with("srt://");
    let mut srt_latency_ms = None;
    let mut srt_passphrase = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--reconnect" => {
                reconnect = reconnect_policy(&required(&mut arguments, "stream reconnect")?)?;
            }
            // SRT caller options (Protocol 2.54). The passphrase is a secret
            // like the key: no error below repeats it.
            "--srt-latency-ms" => {
                if !srt {
                    return Err(ArgsError::SrtOptionNotAllowed("--srt-latency-ms"));
                }
                srt_latency_ms = Some(number(
                    &required(&mut arguments, "SRT latency ms")?,
                    "SRT latency",
                )?);
            }
            "--srt-passphrase" => {
                if !srt {
                    return Err(ArgsError::SrtOptionNotAllowed("--srt-passphrase"));
                }
                srt_passphrase = Some(secret(
                    "--srt-passphrase",
                    nonblank(&mut arguments, "SRT passphrase")?,
                )?);
            }
            "--backup" => {
                if hls {
                    return Err(ArgsError::BackupNotAllowed);
                }
                backup_url = Some(nonblank(&mut arguments, "backup stream url")?);
            }
            "--startup" => {
                startup = startup_policy(&required(&mut arguments, "startup policy")?)?;
            }
            "--video-bitrate-kbps" => {
                video_bitrate_kbps = number(
                    &required(&mut arguments, "video bitrate kbps")?,
                    "video bitrate",
                )?;
            }
            // Each `--rung` is one authored rendition. Only HLS publishes more
            // than one, so a ladder on any other protocol is refused before
            // the value is even read.
            "--rung" => {
                if !hls {
                    return Err(ArgsError::LadderNotAllowed);
                }
                let value = required(&mut arguments, "stream ladder rung")?;
                rungs.push(ladder_rung(&value)?);
            }
            // An HLS destination has no key positional: a leftover positional
            // is most likely one typed from the network shape, so refuse it
            // instead of silently folding it into the name.
            _ if hls && !option.starts_with("--") => {
                return Err(ArgsError::UnexpectedArgument(option));
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    let ladder = if rungs.is_empty() {
        None
    } else {
        Some(StreamLadder::new(rungs).map_err(ArgsError::InvalidLadder)?)
    };
    Ok(build(
        path,
        StreamSpec {
            stream,
            output,
            url,
            key,
            name,
            backup_url,
            startup,
            video_bitrate_kbps,
            ladder,
            reconnect,
            srt_latency_ms,
            srt_passphrase,
        },
    ))
}

/// Parses one `--reconnect <initial_ms>:<max_ms>:<attempts|none>` value
/// (OR-006), for example `500:10000:20` or `250:2000:none` (retry forever).
///
/// A value of any other shape is an [`ArgsError::InvalidNumber`]; a
/// well-shaped policy is validated by [`StreamReconnectPolicy::new`].
fn reconnect_policy(value: &str) -> Result<StreamReconnectPolicy, ArgsError> {
    let invalid = || ArgsError::InvalidNumber {
        field: "stream reconnect (<initial_ms>:<max_ms>:<attempts|none>)",
        value: value.to_owned(),
    };
    let mut parts = value.split(':');
    let (Some(initial), Some(max), Some(attempts), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(invalid());
    };
    // Plain decimal digits only, like the wire: `str::parse` would also
    // take a leading `+`.
    let digits = |text: &str| {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid());
        }
        text.parse::<u32>().map_err(|_| invalid())
    };
    let initial = digits(initial)?;
    let max = digits(max)?;
    let attempts = if attempts == "none" {
        None
    } else {
        Some(digits(attempts)?)
    };
    StreamReconnectPolicy::new(initial, max, attempts).map_err(ArgsError::InvalidReconnect)
}

/// Parses `remote-stream-set <address> <stream> [options] [--key <key>]
/// [--expect <revision>]` (OR-006). See [`RemoteStreamTargetOptions`].
#[allow(clippy::too_many_lines)] // one arm per option
fn parse_remote_stream_set(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let stream = number(&required(&mut arguments, "stream")?, "stream")?;
    let mut options = RemoteStreamTargetOptions::default();
    let mut key = None;
    let mut expected_revision = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--name" => options.name = Some(nonblank(&mut arguments, "stream name")?),
            "--protocol" => {
                let value = required(&mut arguments, "stream protocol")?;
                options.protocol =
                    Some(fm_protocol::StreamTargetProtocol::from_token(&value).ok_or(
                        ArgsError::InvalidChoice {
                            field: "stream protocol",
                            value,
                        },
                    )?);
            }
            "--endpoint" => options.endpoint = Some(nonblank(&mut arguments, "stream endpoint")?),
            "--backup-endpoint" => {
                if options.clear_backup_endpoint {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--backup-endpoint",
                        conflicts: "--clear-backup-endpoint",
                    });
                }
                options.backup_endpoint = Some(nonblank(&mut arguments, "backup stream endpoint")?);
            }
            "--clear-backup-endpoint" => {
                if options.backup_endpoint.is_some() {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--clear-backup-endpoint",
                        conflicts: "--backup-endpoint",
                    });
                }
                options.clear_backup_endpoint = true;
            }
            "--output" => {
                options.output = Some(number(&required(&mut arguments, "output")?, "output")?);
            }
            "--bitrate-kbps" | "--video-bitrate-kbps" => {
                options.video_bitrate_kbps = Some(number(
                    &required(&mut arguments, "video bitrate kbps")?,
                    "video bitrate",
                )?);
            }
            "--startup" => {
                let value = required(&mut arguments, "startup policy")?;
                options.reconcile_on_startup = Some(match value.as_str() {
                    "stopped" => false,
                    "reconcile" | "reconcile-desired-state" => true,
                    _ => {
                        return Err(ArgsError::InvalidChoice {
                            field: "startup policy",
                            value,
                        });
                    }
                });
            }
            "--reconnect" => {
                options.reconnect = Some(reconnect_policy(&required(
                    &mut arguments,
                    "stream reconnect",
                )?)?);
            }
            "--stream-key" => {
                if options.stream_key == SecretEdit::Clear {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--stream-key",
                        conflicts: "--clear-stream-key",
                    });
                }
                options.stream_key = SecretEdit::Set(secret(
                    "--stream-key",
                    nonblank(&mut arguments, "stream key")?,
                )?);
            }
            "--clear-stream-key" => {
                if matches!(options.stream_key, SecretEdit::Set(_)) {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--clear-stream-key",
                        conflicts: "--stream-key",
                    });
                }
                options.stream_key = SecretEdit::Clear;
            }
            "--srt-latency-ms" => {
                options.srt_latency_ms = Some(number(
                    &required(&mut arguments, "SRT latency ms")?,
                    "SRT latency",
                )?);
            }
            "--srt-passphrase" => {
                if options.srt_passphrase == SecretEdit::Clear {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--srt-passphrase",
                        conflicts: "--clear-srt-passphrase",
                    });
                }
                options.srt_passphrase = SecretEdit::Set(secret(
                    "--srt-passphrase",
                    nonblank(&mut arguments, "SRT passphrase")?,
                )?);
            }
            "--clear-srt-passphrase" => {
                if matches!(options.srt_passphrase, SecretEdit::Set(_)) {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--clear-srt-passphrase",
                        conflicts: "--srt-passphrase",
                    });
                }
                options.srt_passphrase = SecretEdit::Clear;
            }
            "--key" => {
                let value = required(&mut arguments, "idempotency key")?;
                if value.trim().is_empty() {
                    return Err(ArgsError::BlankValue("idempotency key"));
                }
                key = Some(value);
            }
            "--expect" => {
                expected_revision = Some(number(
                    &required(&mut arguments, "expected revision")?,
                    "expected revision",
                )?);
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::RemoteStreamSet {
        address,
        stream,
        options: Box::new(options),
        key,
        expected_revision,
    })
}

/// Parses one repeated `--rung` value:
/// `<video_bitrate_kbps>:<width>x<height>`, for example `1000:640x360`.
///
/// A value of any other shape is an [`ArgsError::InvalidNumber`]. A well-shaped
/// rung is validated by [`StreamLadderRung::new`], whose refusal carries the
/// model's own message.
fn ladder_rung(value: &str) -> Result<StreamLadderRung, ArgsError> {
    let (video_bitrate_kbps, dimensions) = value
        .split_once(':')
        .ok_or_else(|| invalid_ladder_rung(value))?;
    let (width, height) = dimensions
        .split_once('x')
        .ok_or_else(|| invalid_ladder_rung(value))?;
    let video_bitrate_kbps = video_bitrate_kbps
        .parse::<u32>()
        .map_err(|_| invalid_ladder_rung(value))?;
    let width = width
        .parse::<u32>()
        .map_err(|_| invalid_ladder_rung(value))?;
    let height = height
        .parse::<u32>()
        .map_err(|_| invalid_ladder_rung(value))?;
    StreamLadderRung::new(video_bitrate_kbps, width, height).map_err(ArgsError::InvalidLadder)
}

fn invalid_ladder_rung(value: &str) -> ArgsError {
    ArgsError::InvalidNumber {
        field: "stream ladder rung",
        value: value.to_owned(),
    }
}

fn parse_stream_remove(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let stream = number(&required(&mut arguments, "stream")?, "stream")?;
    reject_extra(&mut arguments)?;
    Ok(Command::StreamRemove { path, stream })
}

fn parse_stream_running(
    mut arguments: impl Iterator<Item = String>,
    running: bool,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let stream = number(&required(&mut arguments, "stream")?, "stream")?;
    reject_extra(&mut arguments)?;
    Ok(if running {
        Command::StreamStart { path, stream }
    } else {
        Command::StreamStop { path, stream }
    })
}

/// Parses one `--recorder <1|2>` value into its one-based slot.
///
/// The value must parse to a nonzero slot `1..=2`; zero, a slot past the
/// second recorder, and any non-numeric string are refused with the same
/// message, because the flag addresses exactly the two program recorder
/// slots.
fn recorder_slot(value: &str) -> Result<u8, ArgsError> {
    value
        .parse::<u8>()
        .ok()
        .filter(|slot| (1..=2).contains(slot))
        .ok_or_else(|| ArgsError::InvalidRecorder(value.to_owned()))
}

fn parse_record_desired(
    mut arguments: impl Iterator<Item = String>,
    active: bool,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let mut recorder = 1;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--recorder" => {
                recorder = recorder_slot(&required(&mut arguments, "--recorder")?)?;
            }
            _ => return Err(ArgsError::UnexpectedArgument(option)),
        }
    }
    Ok(if active {
        Command::RecordStart { path, recorder }
    } else {
        Command::RecordStop { path, recorder }
    })
}

fn parse_remote_stream_running(
    mut arguments: impl Iterator<Item = String>,
    running: bool,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let stream = number(&required(&mut arguments, "stream")?, "stream")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(if running {
        Command::RemoteStreamStart {
            address,
            stream,
            key,
            expected_revision,
        }
    } else {
        Command::RemoteStreamStop {
            address,
            stream,
            key,
            expected_revision,
        }
    })
}

fn parse_remote_record_desired(
    mut arguments: impl Iterator<Item = String>,
    active: bool,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let mut recorder = 1;
    let mut key = None;
    let mut expected_revision = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--recorder" => {
                recorder = recorder_slot(&required(&mut arguments, "--recorder")?)?;
            }
            "--key" => {
                let value = required(&mut arguments, "idempotency key")?;
                if value.trim().is_empty() {
                    return Err(ArgsError::BlankValue("idempotency key"));
                }
                key = Some(value);
            }
            "--expect" => {
                expected_revision = Some(number(
                    &required(&mut arguments, "expected revision")?,
                    "expected revision",
                )?);
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(if active {
        Command::RemoteRecordStart {
            address,
            recorder,
            key,
            expected_revision,
        }
    } else {
        Command::RemoteRecordStop {
            address,
            recorder,
            key,
            expected_revision,
        }
    })
}

fn startup_policy(value: &str) -> Result<StartupPolicy, ArgsError> {
    match value {
        "stopped" => Ok(StartupPolicy::Stopped),
        "reconcile-desired-state" => Ok(StartupPolicy::ReconcileDesiredState),
        _ => Err(ArgsError::InvalidChoice {
            field: "startup policy",
            value: value.to_owned(),
        }),
    }
}

fn parse_output_rename(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let output = number(&required(&mut arguments, "output")?, "output")?;
    let name = nonblank(&mut arguments, "output name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::OutputRename { path, output, name })
}

fn parse_audio_bus_add(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let bus = number(&required(&mut arguments, "bus")?, "bus")?;
    let name = nonblank(&mut arguments, "bus name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::AudioBusAdd { path, bus, name })
}

fn parse_audio_bus_rename(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let bus = number(&required(&mut arguments, "bus")?, "bus")?;
    let name = nonblank(&mut arguments, "bus name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::AudioBusRename { path, bus, name })
}

fn parse_audio_bus_remove(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let bus = number(&required(&mut arguments, "bus")?, "bus")?;
    reject_extra(&mut arguments)?;
    Ok(Command::AudioBusRemove { path, bus })
}

fn parse_audio_bus_send(
    mut arguments: impl Iterator<Item = String>,
    add: bool,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let source = number(&required(&mut arguments, "source bus")?, "source bus")?;
    let destination = number(
        &required(&mut arguments, "destination bus")?,
        "destination bus",
    )?;
    reject_extra(&mut arguments)?;
    Ok(Command::AudioBusSend {
        path,
        source,
        destination,
        add,
    })
}

fn parse_output_add(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let output = number(&required(&mut arguments, "output")?, "output")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let bus = number(&required(&mut arguments, "bus")?, "bus")?;
    let name = nonblank(&mut arguments, "output name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::OutputAdd {
        path,
        output,
        scene,
        bus,
        name,
    })
}

fn parse_output_remove(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let output = number(&required(&mut arguments, "output")?, "output")?;
    reject_extra(&mut arguments)?;
    Ok(Command::OutputRemove { path, output })
}

fn parse_audio_strip(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let gain_millidb = number(&required(&mut arguments, "gain millidB")?, "gain millidB")?;
    let balance_basis_points = number(
        &required(&mut arguments, "balance basis points")?,
        "balance basis points",
    )?;
    let muted = boolean_choice(&required(&mut arguments, "muted")?, "muted")?;
    let soloed = boolean_choice(&required(&mut arguments, "soloed")?, "soloed")?;
    let follow_video = boolean_choice(&required(&mut arguments, "follow video")?, "follow video")?;
    let delay_samples = number(&required(&mut arguments, "delay samples")?, "delay samples")?;
    let mut arguments = arguments.peekable();
    let pan_basis_points = pan_option(&mut arguments)?;
    reject_extra(&mut arguments)?;
    Ok(Command::AudioStrip {
        path,
        input,
        gain_millidb,
        balance_basis_points,
        pan_basis_points,
        muted,
        soloed,
        follow_video,
        delay_samples,
    })
}

/// An optional trailing `--pan <basis points>` (AU-001); centre otherwise.
fn pan_option(
    arguments: &mut std::iter::Peekable<impl Iterator<Item = String>>,
) -> Result<i32, ArgsError> {
    if arguments.peek().is_some_and(|argument| argument == "--pan") {
        arguments.next();
        number(
            &required(arguments, "pan basis points")?,
            "pan basis points",
        )
    } else {
        Ok(0)
    }
}

fn audio_group_number(value: &str) -> Result<u8, ArgsError> {
    match value.parse::<u8>() {
        Ok(group @ 1..=4) if group.to_string() == value => Ok(group),
        _ => Err(ArgsError::InvalidChoice {
            field: "group",
            value: value.to_owned(),
        }),
    }
}

/// `<input|master>` of `audio-strip-dsp` and `audio-loudness-reset`
/// (AU-005): `None` for the Master.
fn parse_dsp_target(
    arguments: &mut impl Iterator<Item = String>,
) -> Result<Option<u128>, ArgsError> {
    let target = required(arguments, "input or master")?;
    Ok(if target == "master" {
        None
    } else {
        Some(number(&target, "input")?)
    })
}

/// `<input|master> <preset|chain code>` of `audio-strip-dsp` (AU-005).
fn parse_strip_dsp(
    arguments: &mut impl Iterator<Item = String>,
) -> Result<AudioMixerArgs, ArgsError> {
    let input = parse_dsp_target(arguments)?;
    let chain = required(arguments, "DSP preset or chain code")?;
    Ok(AudioMixerArgs::StripDsp {
        input,
        chain: fm_types::strip_dsp::StripDspPreset::from_token(&chain)
            .map(fm_types::strip_dsp::StripDspPreset::chain)
            .or_else(|| fm_types::strip_dsp::StripDsp::from_code(&chain).ok())
            .ok_or(ArgsError::InvalidChoice {
                field: "DSP preset or chain code",
                value: chain,
            })?,
    })
}

fn parse_audio_mixer(
    command: &str,
    arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let mut arguments = arguments.peekable();
    let remote = command.starts_with("remote-");
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    let edit = match command.trim_start_matches("remote-") {
        "audio-label" => AudioMixerArgs::Label {
            input: number(&required(&mut arguments, "input")?, "input")?,
            label: required(&mut arguments, "label")?,
        },
        "audio-group" => {
            let input = number(&required(&mut arguments, "input")?, "input")?;
            let group = required(&mut arguments, "group")?;
            AudioMixerArgs::Group {
                input,
                group: if group == "none" {
                    None
                } else {
                    Some(audio_group_number(&group)?)
                },
            }
        }
        "audio-group-name" => AudioMixerArgs::GroupName {
            group: audio_group_number(&required(&mut arguments, "group")?)?,
            name: nonblank(&mut arguments, "group name")?,
        },
        "audio-group-solo" => AudioMixerArgs::GroupSolo {
            group: audio_group_number(&required(&mut arguments, "group")?)?,
            soloed: boolean_choice(&required(&mut arguments, "soloed")?, "soloed")?,
        },
        "audio-group-parent" => {
            let group = audio_group_number(&required(&mut arguments, "group")?)?;
            let parent = required(&mut arguments, "parent")?;
            AudioMixerArgs::GroupParent {
                group,
                parent: if parent == "none" {
                    None
                } else {
                    Some(audio_group_number(&parent)?)
                },
            }
        }
        "audio-solo-mode" => {
            let mode = required(&mut arguments, "solo mode")?;
            AudioMixerArgs::SoloMode {
                mode: fm_model::AudioSoloMode::from_token(&mode).ok_or(
                    ArgsError::InvalidChoice {
                        field: "solo mode",
                        value: mode,
                    },
                )?,
            }
        }
        "audio-monitor-device" => {
            let device = required(&mut arguments, "monitor device")?;
            AudioMixerArgs::MonitorDevice {
                device: (device != "none").then_some(device),
            }
        }
        "audio-monitor-level" => AudioMixerArgs::MonitorLevel {
            gain_millidb: number(&required(&mut arguments, "gain millidB")?, "gain millidB")?,
            muted: boolean_choice(&required(&mut arguments, "muted")?, "muted")?,
        },
        "audio-strip-dsp" => parse_strip_dsp(&mut arguments)?,
        "audio-loudness-reset" => AudioMixerArgs::LoudnessReset {
            input: parse_dsp_target(&mut arguments)?,
        },
        _ => AudioMixerArgs::GroupLevel {
            group: audio_group_number(&required(&mut arguments, "group")?)?,
            gain_millidb: number(&required(&mut arguments, "gain millidB")?, "gain millidB")?,
            muted: boolean_choice(&required(&mut arguments, "muted")?, "muted")?,
        },
    };
    let (key, expected_revision) = command_options(arguments)?;
    Ok(match (path, address) {
        (Some(path), _) => Command::AudioMixer {
            path,
            edit,
            key,
            expected_revision,
        },
        (None, Some(address)) => Command::RemoteAudioMixer {
            address,
            edit,
            key,
            expected_revision,
        },
        (None, None) => unreachable!("a mixer command names a project or an address"),
    })
}

fn parse_fade_to_black_output(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let address = if remote {
        Some(socket_address(&required(&mut arguments, "address")?)?)
    } else {
        None
    };
    let path = if remote {
        None
    } else {
        Some(required_path(&mut arguments, "project path")?)
    };
    let output = number(&required(&mut arguments, "output")?, "output")?;
    let affected = boolean_choice(&required(&mut arguments, "affected")?, "affected")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(match (path, address) {
        (Some(path), _) => Command::FadeToBlackOutput {
            path,
            output,
            affected,
            key,
            expected_revision,
        },
        (None, Some(address)) => Command::RemoteFadeToBlackOutput {
            address,
            output,
            affected,
            key,
            expected_revision,
        },
        (None, None) => unreachable!("an FTB output command names a project or an address"),
    })
}

/// `ftb-output-duration <output> <frames|none>` and `ftb-curve
/// <linear|ease>`, with their `remote-` forms (SW-004).
fn parse_fade_to_black_timing(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let address = if remote {
        Some(socket_address(&required(&mut arguments, "address")?)?)
    } else {
        None
    };
    let path = if remote {
        None
    } else {
        Some(required_path(&mut arguments, "project path")?)
    };
    if command.ends_with("-curve") {
        let value = required(&mut arguments, "curve")?;
        let curve = FadeToBlackCurve::from_token(&value).ok_or(ArgsError::InvalidChoice {
            field: "curve",
            value,
        })?;
        let (key, expected_revision) = command_options(arguments)?;
        return Ok(match (path, address) {
            (Some(path), _) => Command::FadeToBlackCurve {
                path,
                curve,
                key,
                expected_revision,
            },
            (None, Some(address)) => Command::RemoteFadeToBlackCurve {
                address,
                curve,
                key,
                expected_revision,
            },
            (None, None) => unreachable!("an FTB curve command names a project or an address"),
        });
    }
    let output = number(&required(&mut arguments, "output")?, "output")?;
    let value = required(&mut arguments, "frames")?;
    let duration_frames = if value == "none" {
        None
    } else {
        let frames = number::<u32>(&value, "frames")?;
        if !(1..=fm_protocol::MAX_FADE_TO_BLACK_DURATION_FRAMES).contains(&frames) {
            return Err(ArgsError::InvalidChoice {
                field: "frames",
                value,
            });
        }
        Some(frames)
    };
    let (key, expected_revision) = command_options(arguments)?;
    Ok(match (path, address) {
        (Some(path), _) => Command::FadeToBlackOutputDuration {
            path,
            output,
            duration_frames,
            key,
            expected_revision,
        },
        (None, Some(address)) => Command::RemoteFadeToBlackOutputDuration {
            address,
            output,
            duration_frames,
            key,
            expected_revision,
        },
        (None, None) => unreachable!("an FTB duration command names a project or an address"),
    })
}

fn parse_rename(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let name = nonblank(&mut arguments, "input name")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::Rename {
        path,
        input,
        name,
        key,
        expected_revision,
    })
}

fn parse_reorder(
    mut arguments: impl Iterator<Item = String>,
    remote: bool,
) -> Result<Command, ArgsError> {
    let path_or_address = required(
        &mut arguments,
        if remote { "address" } else { "project path" },
    )?;
    let mut inputs = Vec::new();
    let mut options = Vec::new();
    let mut positional = true;
    while let Some(argument) = arguments.next() {
        if argument.starts_with("--") {
            positional = false;
            options.push(argument);
            if matches!(
                options.last().map(String::as_str),
                Some("--key" | "--expect")
            ) {
                options.push(required(&mut arguments, "option value")?);
            }
        } else if positional {
            inputs.push(number(&argument, "input")?);
        } else {
            options.push(argument);
        }
    }
    if inputs.is_empty() {
        return Err(ArgsError::MissingValue("input"));
    }
    let (key, expected_revision) = command_options(options.into_iter())?;
    if remote {
        Ok(Command::RemoteReorder {
            address: socket_address(&path_or_address)?,
            inputs,
            key,
            expected_revision,
        })
    } else {
        Ok(Command::Reorder {
            path: PathBuf::from(path_or_address),
            inputs,
            key,
            expected_revision,
        })
    }
}

fn parse_local_overlay(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let channel = overlay_channel(&required(&mut arguments, "overlay channel")?)?;
    let result = match command {
        "overlay-take" | "overlay-update" | "overlay-queue" => {
            let source = number(&required(&mut arguments, "source input")?, "source input")?;
            let (key, expected_revision) = command_options(arguments)?;
            if command == "overlay-take" {
                Command::OverlayTake {
                    path,
                    channel,
                    source,
                    key,
                    expected_revision,
                }
            } else if command == "overlay-update" {
                Command::OverlayUpdate {
                    path,
                    channel,
                    source,
                    key,
                    expected_revision,
                }
            } else {
                Command::OverlayQueue {
                    path,
                    channel,
                    source,
                    key,
                    expected_revision,
                }
            }
        }
        "overlay-off" => {
            let (key, expected_revision) = command_options(arguments)?;
            Command::OverlayOff {
                path,
                channel,
                key,
                expected_revision,
            }
        }
        "overlay-output" => {
            let output = number(&required(&mut arguments, "output")?, "output")?;
            let included = boolean(&required(&mut arguments, "included")?, "included")?;
            let (key, expected_revision) = command_options(arguments)?;
            Command::OverlayOutput {
                path,
                channel,
                output,
                included,
                key,
                expected_revision,
            }
        }
        "overlay-transition" => {
            let transition = overlay_transition(&required(&mut arguments, "overlay transition")?)?;
            let frames = number(&required(&mut arguments, "frames")?, "frames")?;
            let (key, expected_revision) = command_options(arguments)?;
            Command::OverlayTransition {
                path,
                channel,
                transition,
                frames,
                key,
                expected_revision,
            }
        }
        "overlay-appearance" => {
            let position = overlay_position(&required(&mut arguments, "overlay position")?)?;
            let border = overlay_border(&required(&mut arguments, "overlay border")?)?;
            let (key, expected_revision) = command_options(arguments)?;
            Command::OverlayAppearance {
                path,
                channel,
                position,
                border,
                key,
                expected_revision,
            }
        }
        "overlay-next" => {
            let (key, expected_revision) = command_options(arguments)?;
            Command::OverlayNext {
                path,
                channel,
                key,
                expected_revision,
            }
        }
        _ => unreachable!("caller dispatches only overlay commands"),
    };
    Ok(result)
}

fn parse_remote_overlay(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let channel = overlay_channel(&required(&mut arguments, "overlay channel")?)?;
    let result = match command {
        "remote-overlay-take" | "remote-overlay-update" | "remote-overlay-queue" => {
            let source = number(&required(&mut arguments, "source input")?, "source input")?;
            let (key, expected_revision) = command_options(arguments)?;
            if command == "remote-overlay-take" {
                Command::RemoteOverlayTake {
                    address,
                    channel,
                    source,
                    key,
                    expected_revision,
                }
            } else if command == "remote-overlay-update" {
                Command::RemoteOverlayUpdate {
                    address,
                    channel,
                    source,
                    key,
                    expected_revision,
                }
            } else {
                Command::RemoteOverlayQueue {
                    address,
                    channel,
                    source,
                    key,
                    expected_revision,
                }
            }
        }
        "remote-overlay-off" => {
            let (key, expected_revision) = command_options(arguments)?;
            Command::RemoteOverlayOff {
                address,
                channel,
                key,
                expected_revision,
            }
        }
        "remote-overlay-output" => {
            let output = number(&required(&mut arguments, "output")?, "output")?;
            let included = boolean(&required(&mut arguments, "included")?, "included")?;
            let (key, expected_revision) = command_options(arguments)?;
            Command::RemoteOverlayOutput {
                address,
                channel,
                output,
                included,
                key,
                expected_revision,
            }
        }
        "remote-overlay-transition" => {
            let transition = overlay_transition(&required(&mut arguments, "overlay transition")?)?;
            let frames = number(&required(&mut arguments, "frames")?, "frames")?;
            let (key, expected_revision) = command_options(arguments)?;
            Command::RemoteOverlayTransition {
                address,
                channel,
                transition,
                frames,
                key,
                expected_revision,
            }
        }
        "remote-overlay-appearance" => {
            let position = overlay_position(&required(&mut arguments, "overlay position")?)?;
            let border = overlay_border(&required(&mut arguments, "overlay border")?)?;
            let (key, expected_revision) = command_options(arguments)?;
            Command::RemoteOverlayAppearance {
                address,
                channel,
                position,
                border,
                key,
                expected_revision,
            }
        }
        "remote-overlay-next" => {
            let (key, expected_revision) = command_options(arguments)?;
            Command::RemoteOverlayNext {
                address,
                channel,
                key,
                expected_revision,
            }
        }
        _ => unreachable!("caller dispatches only remote overlay commands"),
    };
    Ok(result)
}

fn parse_project_from_template(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let template = required_path(&mut arguments, "template project path")?;
    let destination = required_path(&mut arguments, "destination project path")?;
    let mut name = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--name" => name = Some(nonblank(&mut arguments, "project name")?),
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::ProjectFromTemplate {
        template,
        destination,
        name: name.ok_or(ArgsError::MissingValue("--name <project name>"))?,
    })
}

fn parse_project_import_media(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let file = required_path(&mut arguments, "media file")?;
    let mut input = None;
    let mut name = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--input" => input = Some(number(&required(&mut arguments, "input")?, "input")?),
            "--name" => name = Some(nonblank(&mut arguments, "input name")?),
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    if name.is_some() && input.is_none() {
        return Err(ArgsError::RequiresOption {
            option: "--name",
            requires: "--input",
        });
    }
    Ok(Command::ProjectImportMedia {
        path,
        file,
        input,
        name,
    })
}

fn parse_project_relink(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let mut search = Vec::new();
    let mut apply = false;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--search" => search.push(required_path(&mut arguments, "search directory")?),
            "--apply" => apply = true,
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::ProjectRelink {
        path,
        search,
        apply,
    })
}

fn parse_project_autosave(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let mut keep = fm_persistence::DEFAULT_AUTOSAVE_GENERATIONS;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--keep" => keep = number(&required(&mut arguments, "keep")?, "keep")?,
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::ProjectAutosave { path, keep })
}

fn parse_recent_projects_add(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let file = required_path(&mut arguments, "recent-projects file")?;
    let project = required_path(&mut arguments, "project path")?;
    let mut capacity = fm_persistence::DEFAULT_RECENT_PROJECTS;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--capacity" => {
                capacity = number(&required(&mut arguments, "capacity")?, "capacity")?;
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::RecentProjectsAdd {
        file,
        project,
        capacity,
    })
}

fn parse_new(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let mut name = "FreeMix Show".to_owned();
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--name" => name = required(&mut arguments, "name")?,
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::New { path, name })
}

fn parse_project_rename(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let name = nonblank(&mut arguments, "project name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::ProjectRename { path, name })
}

fn parse_input_add(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let name = required(&mut arguments, "input name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::InputAdd { path, input, name })
}

fn parse_simulated_solid_input_add(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let name = nonblank(&mut arguments, "input name")?;
    let red = bounded_u8(&required(&mut arguments, "red")?, "red", u8::MAX)?;
    let green = bounded_u8(&required(&mut arguments, "green")?, "green", u8::MAX)?;
    let blue = bounded_u8(&required(&mut arguments, "blue")?, "blue", u8::MAX)?;
    let alpha = bounded_u8(&required(&mut arguments, "alpha")?, "alpha", u8::MAX)?;
    reject_extra(&mut arguments)?;
    Ok(Command::SimulatedSolidInputAdd {
        path,
        input,
        name,
        red,
        green,
        blue,
        alpha,
    })
}

fn parse_media_input_add(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let name = required(&mut arguments, "input name")?;
    let asset_uri = required(&mut arguments, "asset URI")?;
    reject_extra(&mut arguments)?;
    Ok(Command::MediaInputAdd {
        path,
        input,
        name,
        asset_uri,
    })
}

fn parse_media_input_relink(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let asset_uri = required(&mut arguments, "asset URI")?;
    reject_extra(&mut arguments)?;
    Ok(Command::MediaInputRelink {
        path,
        input,
        asset_uri,
    })
}

fn parse_title_input_add(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let mut input = None;
    let mut name = None;
    let mut template = None;
    let mut fonts = Vec::new();
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--id" => input = Some(number(&required(&mut arguments, "input")?, "input")?),
            "--name" => name = Some(nonblank(&mut arguments, "input name")?),
            "--template" => template = Some(required_path(&mut arguments, "template path")?),
            "--font" => {
                let value = required(&mut arguments, "font")?;
                let Some((family, file)) = value.split_once('=') else {
                    return Err(ArgsError::InvalidChoice {
                        field: "font (expected Family=path)",
                        value,
                    });
                };
                if family.trim().is_empty() {
                    return Err(ArgsError::BlankValue("font family"));
                }
                if file.is_empty() {
                    return Err(ArgsError::BlankValue("font path"));
                }
                fonts.push((family.to_owned(), PathBuf::from(file)));
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::TitleInputAdd {
        path,
        input: input.ok_or(ArgsError::MissingValue("--id"))?,
        name: name.ok_or(ArgsError::MissingValue("--name"))?,
        template: template.ok_or(ArgsError::MissingValue("--template"))?,
        fonts,
    })
}

/// The title animation phase a command addresses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TitlePhase {
    In,
    Out,
}

fn title_phase(arguments: &mut impl Iterator<Item = String>) -> Result<TitlePhase, ArgsError> {
    let value = required(arguments, "phase")?;
    match value.as_str() {
        "in" => Ok(TitlePhase::In),
        "out" => Ok(TitlePhase::Out),
        _ => Err(ArgsError::InvalidChoice {
            field: "phase (expected in or out)",
            value,
        }),
    }
}

fn parse_title_animation_preset(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let template_uri = nonblank(&mut arguments, "template URI")?;
    let element = number(&required(&mut arguments, "element")?, "element")?;
    let mut phase = None;
    let mut preset = None;
    let mut frames = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--phase" => phase = Some(title_phase(&mut arguments)?),
            "--preset" => preset = Some(nonblank(&mut arguments, "preset")?),
            "--frames" => frames = Some(number(&required(&mut arguments, "frames")?, "frames")?),
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::TitleAnimationPreset {
        path,
        template_uri,
        element,
        phase: phase.ok_or(ArgsError::MissingValue("--phase"))?,
        preset: preset.ok_or(ArgsError::MissingValue("--preset"))?,
        frames: frames.ok_or(ArgsError::MissingValue("--frames"))?,
    })
}

fn parse_title_animation_stretch(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let template_uri = nonblank(&mut arguments, "template URI")?;
    let mut phase = None;
    let mut frames = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--phase" => phase = Some(title_phase(&mut arguments)?),
            "--frames" => frames = Some(number(&required(&mut arguments, "frames")?, "frames")?),
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::TitleAnimationStretch {
        path,
        template_uri,
        phase: phase.ok_or(ArgsError::MissingValue("--phase"))?,
        frames: frames.ok_or(ArgsError::MissingValue("--frames"))?,
    })
}

/// What a grade command colours.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GradeTargetArg {
    Input(u128),
    /// One layer (by stable layer id) of one scene (by scene id).
    Layer {
        scene: u128,
        layer: u32,
    },
    Program,
}

/// Grade values given on the command line, applied over a preset (neutral
/// when none is named). Bounds are checked when the grade is assigned.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GradeSpec {
    pub preset: Option<String>,
    pub exposure_millistops: Option<i16>,
    pub temperature: Option<i16>,
    pub tint: Option<i16>,
    pub contrast: Option<i16>,
    pub saturation_permille: Option<u16>,
    pub lut: Option<u32>,
    pub lut_strength_permille: Option<u16>,
}

impl GradeSpec {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn grade_target(arguments: &mut impl Iterator<Item = String>) -> Result<GradeTargetArg, ArgsError> {
    let value = required(arguments, "grade target")?;
    if value == "program" {
        Ok(GradeTargetArg::Program)
    } else if value == "layer" {
        let scene = required(arguments, "scene id")?;
        let layer = required(arguments, "layer id")?;
        Ok(GradeTargetArg::Layer {
            scene: number(&scene, "scene id")?,
            layer: number(&layer, "layer id")?,
        })
    } else {
        number(&value, "grade target (an input id or `program`)").map(GradeTargetArg::Input)
    }
}

fn parse_grade_set(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let target = grade_target(&mut arguments)?;
    let mut library = None;
    let mut save_as = None;
    let mut spec = GradeSpec::default();
    while let Some(option) = arguments.next() {
        let field = match option.as_str() {
            "--grade" => "grade",
            "--save-as" => "grade name",
            "--preset" => "preset",
            "--exposure" => "exposure",
            "--temperature" => "temperature",
            "--tint" => "tint",
            "--contrast" => "contrast",
            "--saturation" => "saturation",
            "--lut" => "lut",
            "--lut-strength" => "lut strength",
            _ => return Err(ArgsError::UnknownOption(option)),
        };
        let value = required(&mut arguments, field)?;
        match option.as_str() {
            "--grade" => library = Some(number(&value, field)?),
            "--save-as" => {
                if value.trim().is_empty() {
                    return Err(ArgsError::BlankValue(field));
                }
                save_as = Some(value);
            }
            "--preset" => spec.preset = Some(value),
            "--exposure" => spec.exposure_millistops = Some(number(&value, field)?),
            "--temperature" => spec.temperature = Some(number(&value, field)?),
            "--tint" => spec.tint = Some(number(&value, field)?),
            "--contrast" => spec.contrast = Some(number(&value, field)?),
            "--saturation" => spec.saturation_permille = Some(number(&value, field)?),
            "--lut" => spec.lut = Some(number(&value, field)?),
            _ => spec.lut_strength_permille = Some(number(&value, field)?),
        }
    }
    if library.is_some() && (!spec.is_empty() || save_as.is_some()) {
        return Err(ArgsError::ConflictingOptions {
            option: "--grade",
            conflicts: "grade values and --save-as",
        });
    }
    if spec.lut_strength_permille.is_some() && spec.lut.is_none() {
        return Err(ArgsError::RequiresOption {
            option: "--lut-strength",
            requires: "--lut",
        });
    }
    Ok(Command::GradeSet {
        path,
        target,
        library,
        spec,
        save_as,
    })
}

fn parse_grade_clear(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let target = grade_target(&mut arguments)?;
    reject_extra(&mut arguments)?;
    Ok(Command::GradeClear { path, target })
}

fn parse_grade_import_lut(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let file = required_path(&mut arguments, "LUT file")?;
    let mut name = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--name" => name = Some(nonblank(&mut arguments, "LUT name")?),
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::GradeImportLut { path, file, name })
}

/// `<show.freemix> <id> <file>`: an export of one library entry.
fn parse_shared_export(
    mut arguments: impl Iterator<Item = String>,
    what: &'static str,
    command: impl FnOnce(PathBuf, u32, PathBuf) -> Command,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let id = required(&mut arguments, what)?;
    let id = number(&id, what)?;
    let file = required_path(&mut arguments, "file")?;
    reject_extra(&mut arguments)?;
    Ok(command(path, id, file))
}

/// `<show.freemix> <grade-id> <file.fmgrade> [--replace-lut]`.
fn parse_grade_export(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let grade = number(&required(&mut arguments, "grade")?, "grade")?;
    let file = required_path(&mut arguments, "file")?;
    let mut replace_lut = false;
    for option in arguments {
        match option.as_str() {
            "--replace-lut" if !replace_lut => replace_lut = true,
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::GradeExport {
        path,
        grade,
        file,
        replace_lut,
    })
}

/// `<show.freemix> <template-uri> <element> --phase in|out --name <name>`.
fn parse_preset_save(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let template_uri = nonblank(&mut arguments, "template URI")?;
    let element = number(&required(&mut arguments, "element")?, "element")?;
    let mut phase = None;
    let mut name = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--phase" if phase.is_none() => phase = Some(title_phase(&mut arguments)?),
            "--name" if name.is_none() => name = Some(nonblank(&mut arguments, "preset name")?),
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::PresetSave {
        path,
        template_uri,
        element,
        phase: phase.ok_or(ArgsError::MissingValue("--phase"))?,
        name: name.ok_or(ArgsError::MissingValue("--name"))?,
    })
}

/// `<show.freemix> <file> [--name <name>]`: an import into the library.
fn parse_shared_import(
    mut arguments: impl Iterator<Item = String>,
    what: &'static str,
    command: impl FnOnce(PathBuf, PathBuf, Option<String>) -> Command,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let file = required_path(&mut arguments, "file")?;
    let mut name = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--name" if name.is_none() => name = Some(nonblank(&mut arguments, what)?),
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(command(path, file, name))
}

fn parse_grade_list(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    reject_extra(&mut arguments)?;
    Ok(Command::GradeList { path })
}

fn parse_title_render_frame(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let mut phase = None;
    let mut frame = None;
    let mut at = None;
    let mut output = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--phase" => phase = Some(title_phase(&mut arguments)?),
            "--frame" => frame = Some(number(&required(&mut arguments, "frame")?, "frame")?),
            "--at" => at = Some(nonblank(&mut arguments, "at")?),
            option if option.starts_with("--") => {
                return Err(ArgsError::UnknownOption(argument));
            }
            _ if output.is_none() => output = Some(PathBuf::from(argument)),
            _ => return Err(ArgsError::UnexpectedArgument(argument)),
        }
    }
    Ok(Command::TitleRenderFrame {
        path,
        input,
        phase: phase.ok_or(ArgsError::MissingValue("--phase"))?,
        frame: frame.ok_or(ArgsError::MissingValue("--frame"))?,
        at,
        output: output.ok_or(ArgsError::MissingValue("output path"))?,
    })
}

/// The point of a scene's animation `scene-render-frame` poses: a frame of
/// the In or Out phase, or the In phase held at its end (on air).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScenePhase {
    In,
    Hold,
    Out,
}

fn parse_scene_animation_preset(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let layer = number(&required(&mut arguments, "layer")?, "layer")?;
    let mut phase = None;
    let mut preset = None;
    let mut frames = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--phase" => phase = Some(title_phase(&mut arguments)?),
            "--preset" => preset = Some(nonblank(&mut arguments, "preset")?),
            "--frames" => frames = Some(number(&required(&mut arguments, "frames")?, "frames")?),
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::SceneAnimationPreset {
        path,
        scene,
        layer,
        phase: phase.ok_or(ArgsError::MissingValue("--phase"))?,
        preset: preset.ok_or(ArgsError::MissingValue("--preset"))?,
        frames: frames.ok_or(ArgsError::MissingValue("--frames"))?,
    })
}

fn parse_scene_animation_stretch(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let mut phase = None;
    let mut frames = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--phase" => phase = Some(title_phase(&mut arguments)?),
            "--frames" => frames = Some(number(&required(&mut arguments, "frames")?, "frames")?),
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::SceneAnimationStretch {
        path,
        scene,
        phase: phase.ok_or(ArgsError::MissingValue("--phase"))?,
        frames: frames.ok_or(ArgsError::MissingValue("--frames"))?,
    })
}

fn parse_scene_render_frame(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let mut phase = None;
    let mut frame = None;
    let mut output = None;
    let mut width = 640;
    let mut height = 360;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--phase" => {
                let value = required(&mut arguments, "phase")?;
                phase = Some(match value.as_str() {
                    "in" => ScenePhase::In,
                    "hold" => ScenePhase::Hold,
                    "out" => ScenePhase::Out,
                    _ => {
                        return Err(ArgsError::InvalidChoice {
                            field: "phase (expected in, hold, or out)",
                            value,
                        });
                    }
                });
            }
            "--frame" => frame = Some(number(&required(&mut arguments, "frame")?, "frame")?),
            "--width" => width = number(&required(&mut arguments, "width")?, "width")?,
            "--height" => height = number(&required(&mut arguments, "height")?, "height")?,
            option if option.starts_with("--") => {
                return Err(ArgsError::UnknownOption(argument));
            }
            _ if output.is_none() => output = Some(PathBuf::from(argument)),
            _ => return Err(ArgsError::UnexpectedArgument(argument)),
        }
    }
    let phase = phase.ok_or(ArgsError::MissingValue("--phase"))?;
    let frame = match (phase, frame) {
        (ScenePhase::Hold, None) => 0,
        (ScenePhase::Hold, Some(_)) => {
            return Err(ArgsError::UnknownOption(
                "--frame (the hold has no frames)".into(),
            ));
        }
        (_, frame) => frame.ok_or(ArgsError::MissingValue("--frame"))?,
    };
    Ok(Command::SceneRenderFrame {
        path,
        scene,
        phase,
        frame,
        output: output.ok_or(ArgsError::MissingValue("output path"))?,
        width,
        height,
    })
}

fn parse_scene_input_add(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let name = nonblank(&mut arguments, "input name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneInputAdd {
        path,
        input,
        scene,
        name,
    })
}

fn parse_scene_input_duplicate(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let source_scene = number(&required(&mut arguments, "source scene")?, "source scene")?;
    let new_scene = number(&required(&mut arguments, "new scene")?, "new scene")?;
    let new_input = number(&required(&mut arguments, "new input")?, "new input")?;
    let scene_name = nonblank(&mut arguments, "scene name")?;
    let input_name = nonblank(&mut arguments, "input name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneInputDuplicate {
        path,
        source_scene,
        new_scene,
        new_input,
        scene_name,
        input_name,
    })
}

fn parse_scene_input_audio_source(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene_input = number(&required(&mut arguments, "scene input")?, "scene input")?;
    let audio_source = number(&required(&mut arguments, "source input")?, "source input")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneInputAudioSource {
        path,
        scene_input,
        audio_source,
    })
}

fn parse_scene_input_audio_source_clear(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene_input = number(&required(&mut arguments, "scene input")?, "scene input")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneInputAudioSourceClear { path, scene_input })
}

fn parse_scene_input_remove(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene_input = number(&required(&mut arguments, "scene input")?, "scene input")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneInputRemove { path, scene_input })
}

fn parse_scene_background(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let red = bounded_u8(
        &required(&mut arguments, "premultiplied red")?,
        "premultiplied red",
        255,
    )?;
    let green = bounded_u8(
        &required(&mut arguments, "premultiplied green")?,
        "premultiplied green",
        255,
    )?;
    let blue = bounded_u8(
        &required(&mut arguments, "premultiplied blue")?,
        "premultiplied blue",
        255,
    )?;
    let alpha = bounded_u8(&required(&mut arguments, "alpha")?, "alpha", 255)?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneBackground {
        path,
        scene,
        red,
        green,
        blue,
        alpha,
    })
}

fn parse_scene_remove(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneRemove { path, scene })
}

fn parse_scene_rename(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let name = nonblank(&mut arguments, "scene name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneRename { path, scene, name })
}

fn parse_scene_layer_add(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let source = number(&required(&mut arguments, "source input")?, "source input")?;
    let z_order = number(&required(&mut arguments, "z-order")?, "z-order")?;
    let name = nonblank(&mut arguments, "layer name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerAdd {
        path,
        scene,
        source,
        z_order,
        name,
    })
}

fn parse_scene_layer_add_scene(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let source_scene = number(&required(&mut arguments, "source scene")?, "source scene")?;
    let z_order = number(&required(&mut arguments, "z-order")?, "z-order")?;
    let name = nonblank(&mut arguments, "layer name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerAddScene {
        path,
        scene,
        source_scene,
        z_order,
        name,
    })
}

fn parse_scene_layer_remove(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerRemove { path, scene, index })
}

fn parse_scene_layer_move(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let source_index = number(
        &required(&mut arguments, "source layer index")?,
        "source layer index",
    )?;
    let destination_index = number(
        &required(&mut arguments, "destination layer index")?,
        "destination layer index",
    )?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerMove {
        path,
        scene,
        source_index,
        destination_index,
    })
}

fn parse_scene_layer_duplicate(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let name = nonblank(&mut arguments, "layer name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerDuplicate {
        path,
        scene,
        index,
        name,
    })
}

fn parse_scene_layer_copy(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let source_scene = number(&required(&mut arguments, "source scene")?, "source scene")?;
    let source_index = number(
        &required(&mut arguments, "source layer index")?,
        "source layer index",
    )?;
    let target_scene = number(&required(&mut arguments, "target scene")?, "target scene")?;
    let name = nonblank(&mut arguments, "layer name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerCopy {
        path,
        source_scene,
        source_index,
        target_scene,
        name,
    })
}

fn parse_scene_layer_rename(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let name = nonblank(&mut arguments, "layer name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerRename {
        path,
        scene,
        index,
        name,
    })
}

fn parse_scene_layer_source_input(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerSourceInput {
        path,
        scene,
        index,
        input,
    })
}

fn parse_scene_layer_source_scene(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let source_scene = number(&required(&mut arguments, "source scene")?, "source scene")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerSourceScene {
        path,
        scene,
        index,
        source_scene,
    })
}

fn parse_scene_layer_z_order(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let z_order = number(&required(&mut arguments, "z-order")?, "z-order")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerZOrder {
        path,
        scene,
        index,
        z_order,
    })
}

fn parse_scene_layer_appearance(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let enabled = boolean_choice(&required(&mut arguments, "enabled")?, "enabled")?;
    let opacity = bounded_u8(&required(&mut arguments, "opacity")?, "opacity", 255)?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerAppearance {
        path,
        scene,
        index,
        enabled,
        opacity,
    })
}

fn parse_scene_layer_geometry(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let x = number(&required(&mut arguments, "x")?, "x")?;
    let y = number(&required(&mut arguments, "y")?, "y")?;
    let width = number(&required(&mut arguments, "width")?, "width")?;
    let height = number(&required(&mut arguments, "height")?, "height")?;
    let rotation_millidegrees = degrees_millidegrees(&required(&mut arguments, "rotation")?)?;
    let anchor = match arguments.next() {
        None => None,
        Some(flag) if flag == "--anchor" => {
            let anchor_x = anchor_basis_points(&required(&mut arguments, "anchor x")?, "anchor x")?;
            let anchor_y = anchor_basis_points(&required(&mut arguments, "anchor y")?, "anchor y")?;
            Some((anchor_x, anchor_y))
        }
        Some(other) => return Err(ArgsError::UnexpectedArgument(other)),
    };
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerGeometry {
        path,
        scene,
        index,
        x,
        y,
        width,
        height,
        rotation_millidegrees,
        anchor,
    })
}

/// A clockwise angle in degrees with at most three decimals (`-12.5`,
/// `90`) as millidegrees, within one full turn either way.
fn degrees_millidegrees(value: &str) -> Result<i32, ArgsError> {
    let invalid = || ArgsError::InvalidNumber {
        field: "rotation",
        value: value.to_owned(),
    };
    let (negative, digits) = value
        .strip_prefix('-')
        .map_or((false, value), |rest| (true, rest));
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    if whole.is_empty()
        || fraction.len() > 3
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || (digits.contains('.') && fraction.is_empty())
    {
        return Err(invalid());
    }
    let whole: i64 = whole.parse().map_err(|_| invalid())?;
    let fraction: i64 = format!("{fraction:0<3}").parse().map_err(|_| invalid())?;
    let magnitude = whole
        .checked_mul(1_000)
        .and_then(|value| value.checked_add(fraction))
        .filter(|value| *value <= 360_000)
        .ok_or_else(invalid)?;
    i32::try_from(if negative { -magnitude } else { magnitude }).map_err(|_| invalid())
}

/// An anchor coordinate in basis points, `0..=10000` (5000 is the centre).
fn anchor_basis_points(value: &str, field: &'static str) -> Result<u16, ArgsError> {
    let basis_points = number::<u16>(value, field)?;
    if basis_points > fm_model::LayerAnchor::MAX {
        return Err(ArgsError::InvalidNumber {
            field,
            value: value.to_owned(),
        });
    }
    Ok(basis_points)
}

fn parse_scene_layer_crop(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let x = number(&required(&mut arguments, "x")?, "x")?;
    let y = number(&required(&mut arguments, "y")?, "y")?;
    let width = number(&required(&mut arguments, "width")?, "width")?;
    let height = number(&required(&mut arguments, "height")?, "height")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerCrop {
        path,
        scene,
        index,
        x,
        y,
        width,
        height,
    })
}

fn parse_scene_layer_crop_clear(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerCropClear { path, scene, index })
}

fn parse_scene_layer_mask(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let x = number(&required(&mut arguments, "x")?, "x")?;
    let y = number(&required(&mut arguments, "y")?, "y")?;
    let width = number(&required(&mut arguments, "width")?, "width")?;
    let height = number(&required(&mut arguments, "height")?, "height")?;
    let inverted = match required(&mut arguments, "mask mode")?.as_str() {
        "normal" => false,
        "inverted" => true,
        value => {
            return Err(ArgsError::InvalidChoice {
                field: "mask mode",
                value: value.to_owned(),
            });
        }
    };
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerMask {
        path,
        scene,
        index,
        x,
        y,
        width,
        height,
        inverted,
    })
}

fn parse_scene_layer_mask_clear(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerMaskClear { path, scene, index })
}

/// Which of the two effect authoring shapes `parse_scene_layer_effect` reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EffectEdit {
    /// `scene-layer-effect-add`: appends an effect after the layer index.
    Add,
    /// `scene-layer-effect-set`: replaces the effect at an additional index.
    Set,
}

/// Parses `scene-layer-effect-add` and `scene-layer-effect-set`.
///
/// Every trailing argument becomes one exact `i32` parameter, so the parameter
/// count is bounded by the model rather than by this parser. The name is also
/// bounded by the model, which refuses an empty, oversized, or control-bearing
/// name the same way masks refuse invalid bounds.
fn parse_scene_layer_effect(
    mut arguments: impl Iterator<Item = String>,
    edit: EffectEdit,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let effect_index = match edit {
        EffectEdit::Add => None,
        EffectEdit::Set => Some(number(
            &required(&mut arguments, "effect index")?,
            "effect index",
        )?),
    };
    let name = required(&mut arguments, "effect name")?;
    let parameters = arguments
        .map(|value| number::<i32>(&value, "effect parameter"))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(match effect_index {
        None => Command::SceneLayerEffectAdd {
            path,
            scene,
            index,
            name,
            parameters,
        },
        Some(effect_index) => Command::SceneLayerEffectSet {
            path,
            scene,
            index,
            effect_index,
            name,
            parameters,
        },
    })
}

fn parse_scene_layer_effect_remove(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let effect_index = number(&required(&mut arguments, "effect index")?, "effect index")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerEffectRemove {
        path,
        scene,
        index,
        effect_index,
    })
}

fn parse_scene_layer_effect_clear(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerEffectClear { path, scene, index })
}

/// Reads a key spec: `none`, `chroma <red> <green> <blue> <similarity>
/// <smoothness> <spill>`, `luma <low> <high> <softness> <invert:on|off>`
/// (IN-020), or `fill <key-input> luma|alpha <low> <high> <softness>
/// <invert:on|off>` (SW-008), then any of the refinement options
/// `--clip <black> <white>`, `--choke <-8..=8>`, `--edge-softness <0..=8>`,
/// `--despill green|blue <strength>`, `--light-wrap <amount> <1..=8>`
/// (`0 0` off), and `--matte inside|outside <x:y,...>` (a polygon garbage
/// matte normalized to `0..=1000`). Returns the key and the arguments
/// after it. The model checks the band and refinement bounds.
fn layer_key_spec(
    arguments: &mut impl Iterator<Item = String>,
) -> Result<(Option<fm_model::LayerKey>, Vec<String>), ArgsError> {
    let kind = required(arguments, "key kind")?;
    let mut byte = |field: &'static str| -> Result<u8, ArgsError> {
        number(&required(arguments, field)?, field)
    };
    let key = match kind.as_str() {
        "none" => None,
        "chroma" => Some(fm_model::LayerKey::Chroma(fm_model::ChromaLayerKey {
            red: byte("red")?,
            green: byte("green")?,
            blue: byte("blue")?,
            similarity: byte("similarity")?,
            smoothness: byte("smoothness")?,
            spill: byte("spill")?,
            refinement: fm_model::KeyRefinement::NONE,
        })),
        "luma" => {
            let low = byte("low")?;
            let high = byte("high")?;
            let softness = byte("softness")?;
            let invert = boolean_choice(&required(arguments, "invert")?, "invert")?;
            Some(fm_model::LayerKey::Luma(fm_model::LumaLayerKey {
                low,
                high,
                softness,
                invert,
                refinement: fm_model::KeyRefinement::NONE,
            }))
        }
        "fill" => {
            let key_input =
                fm_types::InputId::new(number(&required(arguments, "key input")?, "key input")?);
            let channel = match required(arguments, "key channel")?.as_str() {
                "luma" => fm_model::KeyChannel::Luma,
                "alpha" => fm_model::KeyChannel::Alpha,
                other => {
                    return Err(ArgsError::InvalidChoice {
                        field: "key channel",
                        value: other.to_owned(),
                    });
                }
            };
            let mut byte = |field: &'static str| -> Result<u8, ArgsError> {
                number(&required(arguments, field)?, field)
            };
            let low = byte("low")?;
            let high = byte("high")?;
            let softness = byte("softness")?;
            let invert = boolean_choice(&required(arguments, "invert")?, "invert")?;
            Some(fm_model::LayerKey::Fill(fm_model::FillLayerKey {
                key_input,
                channel,
                low,
                high,
                softness,
                invert,
                refinement: fm_model::KeyRefinement::NONE,
            }))
        }
        _ => {
            return Err(ArgsError::InvalidChoice {
                field: "key kind",
                value: kind,
            });
        }
    };
    let Some(key) = key else {
        return Ok((None, arguments.collect()));
    };
    let (refinement, rest) = key_refinement_options(arguments)?;
    Ok((Some(key.with_refinement(refinement)), rest))
}

fn read_byte(
    arguments: &mut impl Iterator<Item = String>,
    field: &'static str,
) -> Result<u8, ArgsError> {
    number(&required(arguments, field)?, field)
}

/// A polygon garbage matte's `x:y,x:y,...` points (each `0..=1000`, three
/// to eight of them).
fn garbage_matte(value: &str, invert: bool) -> Result<fm_types::GarbageMatte, ArgsError> {
    let mut points = Vec::new();
    for pair in value.split(',') {
        let (x, y) = pair
            .split_once(':')
            .ok_or_else(|| ArgsError::InvalidChoice {
                field: "matte point",
                value: pair.to_owned(),
            })?;
        points.push(fm_types::MattePoint::new(
            number(x, "matte x")?,
            number(y, "matte y")?,
        ));
    }
    fm_types::GarbageMatte::new(&points, invert).map_err(|_| ArgsError::InvalidChoice {
        field: "matte points",
        value: value.to_owned(),
    })
}

/// A key's refinement options (SW-008): `--clip <black> <white>`,
/// `--choke <n>`, `--edge-softness <n>`, `--despill green|blue
/// <strength>`, `--light-wrap <amount> <width>`, and `--matte
/// inside|outside <x:y,x:y,...>`; every other argument is returned in
/// order.
fn key_refinement_options(
    arguments: &mut impl Iterator<Item = String>,
) -> Result<(fm_model::KeyRefinement, Vec<String>), ArgsError> {
    let mut refinement = fm_model::KeyRefinement::NONE;
    let mut rest = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--clip" => {
                refinement.clip_black = read_byte(arguments, "clip black")?;
                refinement.clip_white = read_byte(arguments, "clip white")?;
            }
            "--choke" => {
                refinement.choke = number(&required(arguments, "choke")?, "choke")?;
            }
            "--edge-softness" => {
                refinement.edge_softness = read_byte(arguments, "edge softness")?;
            }
            "--light-wrap" => {
                refinement.light_wrap = read_byte(arguments, "light wrap")?;
                refinement.light_wrap_width = read_byte(arguments, "light wrap width")?;
            }
            "--matte" => {
                let side = required(arguments, "matte side")?;
                let invert = match side.as_str() {
                    "inside" => false,
                    "outside" => true,
                    _ => {
                        return Err(ArgsError::InvalidChoice {
                            field: "matte side",
                            value: side,
                        });
                    }
                };
                let points = required(arguments, "matte points")?;
                refinement.garbage_matte = Some(garbage_matte(&points, invert)?);
            }
            "--despill" => {
                let backing = required(arguments, "despill backing")?;
                let strength = read_byte(arguments, "despill strength")?;
                refinement.despill = match backing.as_str() {
                    "green" => fm_model::KeyDespill::Green { strength },
                    "blue" => fm_model::KeyDespill::Blue { strength },
                    _ => {
                        return Err(ArgsError::InvalidChoice {
                            field: "despill backing",
                            value: backing,
                        });
                    }
                };
            }
            _ => rest.push(argument),
        }
    }
    Ok((refinement, rest))
}

/// `scene-layer-key <show> <scene> <layer-index> <key spec>` (IN-020).
fn parse_scene_layer_key(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let (layer_key, rest) = layer_key_spec(&mut arguments)?;
    reject_extra(&mut rest.into_iter())?;
    Ok(Command::SceneLayerKey {
        path,
        scene,
        index,
        layer_key,
    })
}

/// `remote-scene-layer-key <address> <scene> <layer-id> <key spec>
/// [--key <key>] [--expect <revision>]` (IN-020, Protocol 2.43; key/fill
/// and refinements 2.70).
fn parse_remote_scene_layer_key(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let layer = number(&required(&mut arguments, "layer id")?, "layer id")?;
    let (layer_key, rest) = layer_key_spec(&mut arguments)?;
    let (key, expected_revision) = command_options(rest.into_iter())?;
    Ok(Command::RemoteSceneLayerKey {
        address,
        scene,
        layer,
        layer_key,
        key,
        expected_revision,
    })
}

/// An auto key's `--picture` and `--point`, then the other arguments.
type AutoKeyOptions = (Option<PathBuf>, Option<(u32, u32)>, Vec<String>);

/// The auto key options `--picture <file.ppm>` and `--point <x> <y>`;
/// every other argument is returned in order.
fn auto_key_options(arguments: impl Iterator<Item = String>) -> Result<AutoKeyOptions, ArgsError> {
    let mut arguments = arguments;
    let mut picture = None;
    let mut point = None;
    let mut rest = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--picture" => picture = Some(required_path(&mut arguments, "picture path")?),
            "--point" => {
                let x = number(&required(&mut arguments, "point x")?, "point x")?;
                let y = number(&required(&mut arguments, "point y")?, "point y")?;
                point = Some((x, y));
            }
            _ => rest.push(argument),
        }
    }
    Ok((picture, point, rest))
}

/// `scene-layer-auto-key <show> <scene> <layer-index> [--picture
/// <file.ppm>] [--point <x> <y>]` (SW-008).
fn parse_scene_layer_auto_key(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let index = number(&required(&mut arguments, "layer index")?, "layer index")?;
    let (picture, point, rest) = auto_key_options(arguments)?;
    reject_extra(&mut rest.into_iter())?;
    Ok(Command::SceneLayerAutoKey {
        path,
        scene,
        index,
        picture,
        point,
    })
}

/// `remote-scene-layer-auto-key <address> <scene> <layer-id> --picture
/// <file.ppm> [--point <x> <y>] [--clip <black> <white>] [--choke <n>]
/// [--edge-softness <n>] [--light-wrap <amount> <width>] [--key <key>]
/// [--expect <revision>]` (SW-008).
fn parse_remote_scene_layer_auto_key(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let layer = number(&required(&mut arguments, "layer id")?, "layer id")?;
    let (picture, point, rest) = auto_key_options(arguments)?;
    let picture = picture.ok_or(ArgsError::MissingValue("--picture <file.ppm>"))?;
    let (refinement, rest) = key_refinement_options(&mut rest.into_iter())?;
    if refinement.despill != fm_model::KeyDespill::Off {
        return Err(ArgsError::UnknownOption("--despill".to_owned()));
    }
    let (key, expected_revision) = command_options(rest.into_iter())?;
    Ok(Command::RemoteSceneLayerAutoKey {
        address,
        scene,
        layer,
        picture,
        point,
        refinement,
        key,
        expected_revision,
    })
}

/// One `<name>[:<i32>,<i32>...]` effect token of
/// `remote-scene-layer-effects`.
fn effect_token(value: &str) -> Result<fm_model::LayerEffect, ArgsError> {
    let (name, parameters) = value.split_once(':').unwrap_or((value, ""));
    let parameters = if parameters.is_empty() {
        Vec::new()
    } else {
        parameters
            .split(',')
            .map(|parameter| number::<i32>(parameter, "effect parameter"))
            .collect::<Result<Vec<_>, _>>()?
    };
    Ok(fm_model::LayerEffect::new(name, parameters))
}

/// `remote-scene-layer-effects <address> <scene> <layer-id> none|<name>[:<p>,...]...
/// [--key <key>] [--expect <revision>]` (IN-020, Protocol 2.43): replaces
/// the whole ordered list.
fn parse_remote_scene_layer_effects(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    let layer = number(&required(&mut arguments, "layer id")?, "layer id")?;
    let first = required(&mut arguments, "effects")?;
    let mut effects = Vec::new();
    let mut options = Vec::new();
    if first != "none" {
        effects.push(effect_token(&first)?);
    }
    let mut rest = arguments.peekable();
    while let Some(value) = rest.next_if(|value| !value.starts_with("--")) {
        if first == "none" {
            return Err(ArgsError::UnexpectedArgument(value));
        }
        effects.push(effect_token(&value)?);
    }
    options.extend(rest);
    let (key, expected_revision) = command_options(options.into_iter())?;
    Ok(Command::RemoteSceneLayerEffects {
        address,
        scene,
        layer,
        effects,
        key,
        expected_revision,
    })
}

fn parse_input_remove(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    reject_extra(&mut arguments)?;
    Ok(Command::InputRemove { path, input })
}

fn parse_input_duplicate(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let source = number(&required(&mut arguments, "source input")?, "source input")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let name = required(&mut arguments, "input name")?;
    reject_extra(&mut arguments)?;
    Ok(Command::InputDuplicate {
        path,
        source,
        input,
        name,
    })
}

fn parse_input_replace_simulated(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    reject_extra(&mut arguments)?;
    Ok(Command::InputReplaceSimulated { path, input })
}

fn parse_input_replace_solid(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let red = bounded_u8(&required(&mut arguments, "red")?, "red", u8::MAX)?;
    let green = bounded_u8(&required(&mut arguments, "green")?, "green", u8::MAX)?;
    let blue = bounded_u8(&required(&mut arguments, "blue")?, "blue", u8::MAX)?;
    let alpha = bounded_u8(&required(&mut arguments, "alpha")?, "alpha", u8::MAX)?;
    reject_extra(&mut arguments)?;
    Ok(Command::InputReplaceSolid {
        path,
        input,
        red,
        green,
        blue,
        alpha,
    })
}

fn parse_input_replace_media(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let asset_uri = required(&mut arguments, "asset URI")?;
    reject_extra(&mut arguments)?;
    Ok(Command::InputReplaceMedia {
        path,
        input,
        asset_uri,
    })
}

fn parse_input_replace_scene(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let scene = number(&required(&mut arguments, "scene")?, "scene")?;
    reject_extra(&mut arguments)?;
    Ok(Command::InputReplaceScene { path, input, scene })
}

fn parse_local_timed_transition(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let frames = number(&required(&mut arguments, "frames")?, "frames")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(match command {
        "fade" => Command::Fade {
            path,
            frames,
            key,
            expected_revision,
        },
        "alpha-fade" => Command::AlphaFade {
            path,
            frames,
            key,
            expected_revision,
        },
        "slide" => Command::Slide {
            path,
            frames,
            key,
            expected_revision,
        },
        "zoom" => Command::Zoom {
            path,
            frames,
            key,
            expected_revision,
        },
        "wipe" => Command::Wipe {
            path,
            frames,
            key,
            expected_revision,
        },
        _ => unreachable!("caller only dispatches timed local transitions"),
    })
}

fn parse_local_stinger(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let slot = stinger_slot(&required(&mut arguments, "Stinger slot")?)?;
    let frames = number(&required(&mut arguments, "frames")?, "frames")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::Stinger {
        path,
        slot,
        frames,
        key,
        expected_revision,
    })
}

fn parse_stinger_configuration(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let (slot, media_input, preload, cut_point_frames, audio_policy, fallback) =
        parse_stinger_configuration_fields(&mut arguments)?;
    reject_extra(&mut arguments)?;
    Ok(Command::StingerConfigure {
        path,
        slot,
        media_input,
        preload,
        cut_point_frames,
        audio_policy,
        fallback,
    })
}

fn parse_stinger_configuration_fields(
    arguments: &mut impl Iterator<Item = String>,
) -> Result<(u8, u128, bool, u32, StingerAudioPolicy, StingerFallback), ArgsError> {
    let slot = stinger_slot(&required(arguments, "Stinger slot")?)?;
    let media_input = number(&required(arguments, "media input")?, "media input")?;
    let preload = match required(arguments, "preload")?.as_str() {
        "true" => true,
        "false" => false,
        value => {
            return Err(ArgsError::InvalidChoice {
                field: "preload",
                value: value.to_owned(),
            });
        }
    };
    let cut_point_frames = number(
        &required(arguments, "cut point frames")?,
        "cut point frames",
    )?;
    let audio_policy = match required(arguments, "Stinger audio policy")?.as_str() {
        "muted" => StingerAudioPolicy::Muted,
        "stinger-only" => StingerAudioPolicy::StingerOnly,
        "mix-with-program" => StingerAudioPolicy::MixWithProgram,
        value => {
            return Err(ArgsError::InvalidChoice {
                field: "Stinger audio policy",
                value: value.to_owned(),
            });
        }
    };
    let fallback = match required(arguments, "Stinger fallback")?.as_str() {
        "cut" => StingerFallback::Cut,
        "fade" => StingerFallback::Fade,
        "keep-program" => StingerFallback::KeepProgram,
        value => {
            return Err(ArgsError::InvalidChoice {
                field: "Stinger fallback",
                value: value.to_owned(),
            });
        }
    };
    Ok((
        slot,
        media_input,
        preload,
        cut_point_frames,
        audio_policy,
        fallback,
    ))
}

fn parse_stinger_removal(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let slot = stinger_slot(&required(&mut arguments, "Stinger slot")?)?;
    reject_extra(&mut arguments)?;
    Ok(Command::StingerRemove { path, slot })
}

fn parse_local_t_bar(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let action = parse_t_bar_action(command, &mut arguments)?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::TBar {
        path,
        action,
        key,
        expected_revision,
    })
}

fn parse_remote_t_bar(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let action = parse_t_bar_action(command, &mut arguments)?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteTBar {
        address,
        action,
        key,
        expected_revision,
    })
}

fn parse_local_fade_to_black(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let target = fade_to_black_target(&required(&mut arguments, "FTB target")?)?;
    let frames = number(&required(&mut arguments, "frames")?, "frames")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::FadeToBlack {
        path,
        target,
        frames,
        key,
        expected_revision,
    })
}

fn parse_remote_fade_to_black(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let target = fade_to_black_target(&required(&mut arguments, "FTB target")?)?;
    let frames = number(&required(&mut arguments, "frames")?, "frames")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteFadeToBlack {
        address,
        target,
        frames,
        key,
        expected_revision,
    })
}

fn parse_t_bar_action(
    command: &str,
    arguments: &mut impl Iterator<Item = String>,
) -> Result<TBarAction, ArgsError> {
    if command.ends_with("-start") {
        let kind = manual_kind(&required(arguments, "transition kind")?, arguments)?;
        Ok(TBarAction::Start(kind))
    } else if command.ends_with("-position") {
        let position = basis_points(&required(arguments, "basis points")?)?;
        Ok(TBarAction::SetPosition(position))
    } else if command.ends_with("-commit") {
        Ok(TBarAction::Commit)
    } else {
        Ok(TBarAction::Cancel)
    }
}

fn parse_remote_status(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    reject_extra(&mut arguments)?;
    Ok(Command::RemoteStatus { address })
}

fn display_output_slot(value: &str) -> Result<fm_types::display::DisplayOutputSlot, ArgsError> {
    let slot = number::<u8>(value, "slot")?;
    fm_types::display::DisplayOutputSlot::new(slot).ok_or(ArgsError::OutOfRange {
        field: "slot",
        minimum: 1,
        maximum: 2,
        value: u16::from(slot),
    })
}

/// `display-output-set|clear|list` and their `remote-` forms, plus
/// `remote-display-inventory` (OR-011, Protocol 2.42).
fn parse_display_output(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    let action = command
        .strip_prefix("remote-")
        .unwrap_or(command)
        .strip_prefix("display-")
        .unwrap_or(command);
    Ok(match (action, path, address) {
        ("output-list", Some(path), _) => {
            reject_extra(&mut arguments)?;
            Command::DisplayOutputList { path }
        }
        ("output-list", None, Some(address)) => {
            reject_extra(&mut arguments)?;
            Command::RemoteDisplayOutputList { address }
        }
        ("inventory", None, Some(address)) => {
            reject_extra(&mut arguments)?;
            Command::RemoteDisplayInventory { address }
        }
        ("output-set", path, address) => {
            let slot = display_output_slot(&required(&mut arguments, "slot")?)?;
            let spec = required(&mut arguments, "display-output spec")?;
            let config = Box::new(
                spec.parse::<fm_types::display::DisplayOutputConfig>()
                    .map_err(|error| ArgsError::InvalidDisplayOutput(error.to_string()))?,
            );
            let (key, expected_revision) = command_options(arguments)?;
            match (path, address) {
                (Some(path), _) => Command::DisplayOutputSet {
                    path,
                    slot,
                    config,
                    key,
                    expected_revision,
                },
                (None, Some(address)) => Command::RemoteDisplayOutputSet {
                    address,
                    slot,
                    config,
                    key,
                    expected_revision,
                },
                (None, None) => unreachable!("a display-output command names a target"),
            }
        }
        ("output-clear", path, address) => {
            let slot = display_output_slot(&required(&mut arguments, "slot")?)?;
            let (key, expected_revision) = command_options(arguments)?;
            match (path, address) {
                (Some(path), _) => Command::DisplayOutputClear {
                    path,
                    slot,
                    key,
                    expected_revision,
                },
                (None, Some(address)) => Command::RemoteDisplayOutputClear {
                    address,
                    slot,
                    key,
                    expected_revision,
                },
                (None, None) => unreachable!("a display-output command names a target"),
            }
        }
        _ => return Err(ArgsError::UnknownCommand(command.to_owned())),
    })
}

/// A `replay-set` edit (OR-012): each `None` keeps the current value.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReplayEdit {
    /// The recorded inputs, in angle order (`--sources 3,1`, or `--sources
    /// none`).
    pub sources: Option<Vec<u128>>,
    pub retention_seconds: Option<u32>,
    /// `Some(None)` lets the server choose (`--default-root`).
    #[allow(clippy::option_option)] // keep, the server's default, or a root
    pub storage_root: Option<Option<String>>,
}

/// The inputs `--sources` names, in angle order (OR-012): distinct, nonzero,
/// and at most [`fm_protocol::MAX_REPLAY_RECORD_SOURCES`].
fn replay_sources(value: &str) -> Result<Vec<u128>, ArgsError> {
    if value == "none" {
        return Ok(Vec::new());
    }
    let mut sources: Vec<u128> = Vec::new();
    for input in value.split(',') {
        let source: u128 = number(input, "replay source")?;
        if source == 0 {
            return Err(ArgsError::InvalidChoice {
                field: "replay source",
                value: source.to_string(),
            });
        }
        if sources.contains(&source) {
            let input = fm_types::InputId::new(
                core::num::NonZeroU128::new(source).expect("a replay source is nonzero"),
            );
            return Err(ArgsError::InvalidReplay(
                fm_protocol::ReplaySettingsError::DuplicateSource(input),
            ));
        }
        sources.push(source);
    }
    if sources.len() > fm_protocol::MAX_REPLAY_RECORD_SOURCES {
        return Err(ArgsError::InvalidReplay(
            fm_protocol::ReplaySettingsError::TooManySources,
        ));
    }
    Ok(sources)
}

/// The `--retention` window in seconds (OR-012), within the documented
/// [`fm_protocol::MIN_REPLAY_RETENTION_SECONDS`] through
/// [`fm_protocol::MAX_REPLAY_RETENTION_SECONDS`].
fn replay_retention(value: &str) -> Result<u32, ArgsError> {
    let seconds = number::<u16>(value, "retention seconds")?;
    let minimum = u16::try_from(fm_protocol::MIN_REPLAY_RETENTION_SECONDS)
        .expect("the replay retention minimum fits u16");
    let maximum = u16::try_from(fm_protocol::MAX_REPLAY_RETENTION_SECONDS)
        .expect("the replay retention maximum fits u16");
    if !(minimum..=maximum).contains(&seconds) {
        return Err(ArgsError::OutOfRange {
            field: "replay retention seconds",
            minimum,
            maximum,
            value: seconds,
        });
    }
    Ok(u32::from(seconds))
}

/// The `--root` storage directory (OR-012): absolute and within the
/// documented length.
fn replay_root(value: String) -> Result<String, ArgsError> {
    if !fm_protocol::replay_storage_root_is_valid(&value) {
        return Err(ArgsError::InvalidReplay(
            fm_protocol::ReplaySettingsError::StorageRoot,
        ));
    }
    Ok(value)
}

/// `replay`, `replay-set`, and their `remote-` forms (OR-012).
fn parse_replay(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let target = required(
        &mut arguments,
        if remote { "address" } else { "project path" },
    )?;
    if !command.ends_with("-set") {
        reject_extra(&mut arguments)?;
        return Ok(if remote {
            Command::RemoteReplay {
                address: socket_address(&target)?,
            }
        } else {
            Command::Replay {
                path: PathBuf::from(target),
            }
        });
    }
    let mut edit = ReplayEdit::default();
    let mut key = None;
    let mut expected_revision = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--sources" => {
                let value = required(&mut arguments, "replay sources")?;
                edit.sources = Some(replay_sources(&value)?);
            }
            "--retention" => {
                edit.retention_seconds = Some(replay_retention(&required(
                    &mut arguments,
                    "retention seconds",
                )?)?);
            }
            "--root" => {
                if edit.storage_root == Some(None) {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--root",
                        conflicts: "--default-root",
                    });
                }
                edit.storage_root = Some(Some(replay_root(required(
                    &mut arguments,
                    "storage root",
                )?)?));
            }
            "--default-root" => {
                if matches!(edit.storage_root, Some(Some(_))) {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--default-root",
                        conflicts: "--root",
                    });
                }
                edit.storage_root = Some(None);
            }
            "--key" => {
                let value = required(&mut arguments, "idempotency key")?;
                if value.trim().is_empty() {
                    return Err(ArgsError::BlankValue("idempotency key"));
                }
                key = Some(value);
            }
            "--expect" => {
                expected_revision = Some(number(
                    &required(&mut arguments, "expected revision")?,
                    "expected revision",
                )?);
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(if remote {
        Command::RemoteReplaySet {
            address: socket_address(&target)?,
            edit,
            key,
            expected_revision,
        }
    } else {
        Command::ReplaySet {
            path: PathBuf::from(target),
            edit,
            key,
            expected_revision,
        }
    })
}

fn replay_input(value: &str) -> Result<fm_protocol::WireInputId, ArgsError> {
    let value: u128 = number(value, "input")?;
    core::num::NonZeroU128::new(value)
        .map(fm_protocol::WireInputId::new)
        .ok_or_else(|| ArgsError::InvalidChoice {
            field: "input",
            value: value.to_string(),
        })
}

/// `remote-replay-do <address> <operation> ...` (OR-012):
/// `mark-in`, `mark-out`, `capture [--name NAME]`, `last 5|10|20`,
/// `rename EVENT NAME`, `delete EVENT`, `cue INPUT EVENT [--angle INPUT]`,
/// `angle INPUT ANGLE`, `play INPUT [25|50|75|100]`, `pause INPUT`, and
/// `cue-in INPUT`, each with an optional `--key`.
fn parse_remote_replay_do(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    use fm_protocol::{ReplayOperation, ReplaySpeed};
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let verb = required(&mut arguments, "replay operation")?;
    let mut positional = Vec::new();
    let mut name = None;
    let mut angle = None;
    let mut key = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--name" => name = Some(required(&mut arguments, "event name")?),
            "--angle" => angle = Some(replay_input(&required(&mut arguments, "angle")?)?),
            "--key" => {
                let value = required(&mut arguments, "idempotency key")?;
                if value.trim().is_empty() {
                    return Err(ArgsError::BlankValue("idempotency key"));
                }
                key = Some(value);
            }
            option if option.starts_with("--") => {
                return Err(ArgsError::UnknownOption(argument));
            }
            _ => positional.push(argument),
        }
    }
    let wrong = || ArgsError::InvalidChoice {
        field: "replay operation",
        value: verb.clone(),
    };
    let event = |value: &String| number::<u32>(value, "event");
    let operation = match (verb.as_str(), positional.as_slice()) {
        ("mark-in", []) => ReplayOperation::MarkIn,
        ("mark-out", []) => ReplayOperation::MarkOut,
        ("capture", []) => ReplayOperation::CaptureMarks { name: name.take() },
        ("last", [seconds]) => {
            let seconds: u8 = number(seconds, "seconds")?;
            if !fm_protocol::REPLAY_LAST_SECONDS.contains(&seconds) {
                return Err(ArgsError::InvalidChoice {
                    field: "seconds",
                    value: seconds.to_string(),
                });
            }
            ReplayOperation::CaptureLast { seconds }
        }
        ("rename", [id, new_name]) => ReplayOperation::RenameEvent {
            event: event(id)?,
            name: new_name.clone(),
        },
        ("delete", [id]) => ReplayOperation::DeleteEvent { event: event(id)? },
        ("cue", [input, id]) => ReplayOperation::Cue {
            input: replay_input(input)?,
            event: event(id)?,
            angle: angle.take(),
        },
        ("angle", [input, shown]) => ReplayOperation::Angle {
            input: replay_input(input)?,
            angle: replay_input(shown)?,
        },
        ("play", [input]) => ReplayOperation::Play {
            input: replay_input(input)?,
            speed: ReplaySpeed::Full,
        },
        ("play", [input, percent]) => ReplayOperation::Play {
            input: replay_input(input)?,
            speed: ReplaySpeed::from_percent(number(percent, "speed")?).ok_or_else(|| {
                ArgsError::InvalidChoice {
                    field: "speed",
                    value: percent.clone(),
                }
            })?,
        },
        ("pause", [input]) => ReplayOperation::Pause {
            input: replay_input(input)?,
        },
        ("cue-in", [input]) => ReplayOperation::CueIn {
            input: replay_input(input)?,
        },
        _ => return Err(wrong()),
    };
    if name.is_some() || angle.is_some() {
        return Err(wrong());
    }
    Ok(Command::RemoteReplayDo {
        address,
        operation,
        key,
    })
}

/// A nonzero reel or export-job id (OR-013).
fn reel_id(value: &str) -> Result<u64, ArgsError> {
    let id: u64 = number(value, "reel id")?;
    if id == 0 {
        Err(ArgsError::InvalidChoice {
            field: "reel id",
            value: value.to_owned(),
        })
    } else {
        Ok(id)
    }
}

/// A nonzero reel export job id (OR-013).
fn reel_job(value: &str) -> Result<u64, ArgsError> {
    let job: u64 = number(value, "reel export job")?;
    if job == 0 {
        Err(ArgsError::InvalidChoice {
            field: "reel export job",
            value: value.to_owned(),
        })
    } else {
        Ok(job)
    }
}

/// A reel name (OR-013): non-blank, at most
/// [`fm_protocol::MAX_REEL_NAME_BYTES`] bytes, and free of control
/// characters.
fn reel_name(value: String) -> Result<String, ArgsError> {
    if fm_protocol::reel_name_is_valid(&value) {
        Ok(value)
    } else {
        Err(ArgsError::InvalidReel(fm_protocol::ReelSpecError::Name))
    }
}

/// A reel export destination (OR-013): an absolute path of at most
/// [`fm_protocol::MAX_REEL_PATH_BYTES`] bytes.
fn reel_destination(value: String) -> Result<String, ArgsError> {
    if fm_protocol::reel_export_path_is_valid(&value) {
        Ok(value)
    } else {
        Err(ArgsError::InvalidReelDestination)
    }
}

/// The `--clips` list (OR-013): 1 to [`fm_protocol::MAX_REEL_CLIPS`]
/// `<event>/<angle>/<trim-in>/<trim-out>/<speed>` records, comma separated.
/// Each event and angle is nonzero, each trim is at most
/// [`fm_protocol::MAX_REEL_TRIM_FRAMES`], and each speed is 25, 50, 75, or
/// 100 percent.
fn reel_clips(value: &str) -> Result<Vec<fm_protocol::ReelClip>, ArgsError> {
    let mut clips = Vec::new();
    for record in value.split(',') {
        let fields: Vec<&str> = record.split('/').collect();
        let [event, angle, trim_in, trim_out, speed] = fields.as_slice() else {
            return Err(ArgsError::InvalidReelClip(record.to_owned()));
        };
        let event: u32 = number(event, "reel clip event")?;
        if event == 0 {
            return Err(ArgsError::InvalidChoice {
                field: "reel clip event",
                value: event.to_string(),
            });
        }
        let angle_value: u128 = number(angle, "reel clip angle")?;
        let angle = core::num::NonZeroU128::new(angle_value)
            .map(fm_types::InputId::new)
            .ok_or_else(|| ArgsError::InvalidChoice {
                field: "reel clip angle",
                value: angle_value.to_string(),
            })?;
        let trim_in: u32 = number(trim_in, "reel clip trim in")?;
        let trim_out: u32 = number(trim_out, "reel clip trim out")?;
        if trim_in > fm_protocol::MAX_REEL_TRIM_FRAMES
            || trim_out > fm_protocol::MAX_REEL_TRIM_FRAMES
        {
            return Err(ArgsError::InvalidReel(fm_protocol::ReelSpecError::Trim));
        }
        let speed: u8 = number(speed, "reel clip speed")?;
        if fm_protocol::ReplaySpeed::from_percent(speed).is_none() {
            return Err(ArgsError::InvalidReel(fm_protocol::ReelSpecError::Speed(
                speed,
            )));
        }
        clips.push(fm_protocol::ReelClip {
            event,
            angle,
            trim_in_frames: trim_in,
            trim_out_frames: trim_out,
            speed_percent: speed,
        });
    }
    if clips.len() > fm_protocol::MAX_REEL_CLIPS {
        return Err(ArgsError::InvalidReel(
            fm_protocol::ReelSpecError::TooManyClips,
        ));
    }
    Ok(clips)
}

/// `reel`, `reel-set`, `reel-delete`, and their `remote-` forms, plus the
/// live export controls (OR-013, Protocol 2.107).
#[allow(clippy::too_many_lines)] // one arm per command shape
fn parse_reel(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let target = required(
        &mut arguments,
        if remote { "address" } else { "project path" },
    )?;
    let address = || socket_address(&target);
    let path = || PathBuf::from(&target);
    match command {
        "reel" => {
            reject_extra(&mut arguments)?;
            Ok(Command::Reel { path: path() })
        }
        "remote-reel" => {
            reject_extra(&mut arguments)?;
            Ok(Command::RemoteReel {
                address: address()?,
            })
        }
        "reel-set" | "remote-reel-set" => {
            let mut id = None;
            let mut name = None;
            let mut clips = None;
            let mut key = None;
            let mut expected_revision = None;
            while let Some(option) = arguments.next() {
                match option.as_str() {
                    "--id" => {
                        if id.is_some() {
                            return Err(ArgsError::DuplicateOption("--id"));
                        }
                        id = Some(reel_id(&required(&mut arguments, "reel id")?)?);
                    }
                    "--name" => {
                        if name.is_some() {
                            return Err(ArgsError::DuplicateOption("--name"));
                        }
                        name = Some(reel_name(required(&mut arguments, "reel name")?)?);
                    }
                    "--clips" => {
                        if clips.is_some() {
                            return Err(ArgsError::DuplicateOption("--clips"));
                        }
                        clips = Some(reel_clips(&required(&mut arguments, "reel clips")?)?);
                    }
                    "--key" => {
                        let value = required(&mut arguments, "idempotency key")?;
                        if value.trim().is_empty() {
                            return Err(ArgsError::BlankValue("idempotency key"));
                        }
                        key = Some(value);
                    }
                    "--expect" => {
                        expected_revision = Some(number(
                            &required(&mut arguments, "expected revision")?,
                            "expected revision",
                        )?);
                    }
                    _ => return Err(ArgsError::UnknownOption(option)),
                }
            }
            let spec = Box::new(fm_protocol::ReelSpec {
                id: id.ok_or(ArgsError::MissingValue("reel id"))?,
                name: name.ok_or(ArgsError::MissingValue("reel name"))?,
                clips: clips.ok_or(ArgsError::MissingValue("reel clips"))?,
            });
            Ok(if remote {
                Command::RemoteReelSet {
                    address: address()?,
                    spec,
                    key,
                    expected_revision,
                }
            } else {
                Command::ReelSet {
                    path: path(),
                    spec,
                    key,
                    expected_revision,
                }
            })
        }
        "reel-delete" | "remote-reel-delete" => {
            let mut id = None;
            let mut key = None;
            let mut expected_revision = None;
            while let Some(option) = arguments.next() {
                match option.as_str() {
                    "--id" => {
                        if id.is_some() {
                            return Err(ArgsError::DuplicateOption("--id"));
                        }
                        id = Some(reel_id(&required(&mut arguments, "reel id")?)?);
                    }
                    "--key" => {
                        let value = required(&mut arguments, "idempotency key")?;
                        if value.trim().is_empty() {
                            return Err(ArgsError::BlankValue("idempotency key"));
                        }
                        key = Some(value);
                    }
                    "--expect" => {
                        expected_revision = Some(number(
                            &required(&mut arguments, "expected revision")?,
                            "expected revision",
                        )?);
                    }
                    _ => return Err(ArgsError::UnknownOption(option)),
                }
            }
            let id = id.ok_or(ArgsError::MissingValue("reel id"))?;
            Ok(if remote {
                Command::RemoteReelDelete {
                    address: address()?,
                    id,
                    key,
                    expected_revision,
                }
            } else {
                Command::ReelDelete {
                    path: path(),
                    id,
                    key,
                    expected_revision,
                }
            })
        }
        "remote-reel-export" => {
            let mut job = None;
            let mut reel = None;
            let mut destination = None;
            let mut key = None;
            let mut expected_revision = None;
            while let Some(option) = arguments.next() {
                match option.as_str() {
                    "--job" => {
                        if job.is_some() {
                            return Err(ArgsError::DuplicateOption("--job"));
                        }
                        job = Some(reel_job(&required(&mut arguments, "reel export job")?)?);
                    }
                    "--reel" => {
                        if reel.is_some() {
                            return Err(ArgsError::DuplicateOption("--reel"));
                        }
                        reel = Some(reel_id(&required(&mut arguments, "reel id")?)?);
                    }
                    "--destination" => {
                        if destination.is_some() {
                            return Err(ArgsError::DuplicateOption("--destination"));
                        }
                        destination = Some(reel_destination(required(
                            &mut arguments,
                            "reel export destination",
                        )?)?);
                    }
                    "--key" => {
                        let value = required(&mut arguments, "idempotency key")?;
                        if value.trim().is_empty() {
                            return Err(ArgsError::BlankValue("idempotency key"));
                        }
                        key = Some(value);
                    }
                    "--expect" => {
                        expected_revision = Some(number(
                            &required(&mut arguments, "expected revision")?,
                            "expected revision",
                        )?);
                    }
                    _ => return Err(ArgsError::UnknownOption(option)),
                }
            }
            Ok(Command::RemoteReelExport {
                address: address()?,
                job: job.ok_or(ArgsError::MissingValue("reel export job"))?,
                reel: reel.ok_or(ArgsError::MissingValue("reel id"))?,
                destination: destination
                    .ok_or(ArgsError::MissingValue("reel export destination"))?,
                key,
                expected_revision,
            })
        }
        "remote-reel-cancel" | "remote-reel-forget" => {
            let mut job = None;
            let mut key = None;
            let mut expected_revision = None;
            while let Some(option) = arguments.next() {
                match option.as_str() {
                    "--job" => {
                        if job.is_some() {
                            return Err(ArgsError::DuplicateOption("--job"));
                        }
                        job = Some(reel_job(&required(&mut arguments, "reel export job")?)?);
                    }
                    "--key" => {
                        let value = required(&mut arguments, "idempotency key")?;
                        if value.trim().is_empty() {
                            return Err(ArgsError::BlankValue("idempotency key"));
                        }
                        key = Some(value);
                    }
                    "--expect" => {
                        expected_revision = Some(number(
                            &required(&mut arguments, "expected revision")?,
                            "expected revision",
                        )?);
                    }
                    _ => return Err(ArgsError::UnknownOption(option)),
                }
            }
            let job = job.ok_or(ArgsError::MissingValue("reel export job"))?;
            Ok(if command.ends_with("-cancel") {
                Command::RemoteReelCancel {
                    address: address()?,
                    job,
                    key,
                    expected_revision,
                }
            } else {
                Command::RemoteReelForget {
                    address: address()?,
                    job,
                    key,
                    expected_revision,
                }
            })
        }
        _ => Err(ArgsError::UnknownCommand(command.to_owned())),
    }
}

/// A `LiveLAN` edit from the command line (OR-017): every absent field keeps
/// the current value. The PIN is validated here and never echoed.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LiveLanEdit {
    pub enabled: Option<bool>,
    pub source: Option<fm_types::live_lan::LiveLanSource>,
    pub max_viewers: Option<u8>,
    pub pin: SecretEdit,
}

/// `live-lan`, `live-lan-set`, and their `remote-` forms (OR-017).
fn parse_live_lan(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let target = required(
        &mut arguments,
        if remote { "address" } else { "project path" },
    )?;
    if !command.ends_with("-set") {
        reject_extra(&mut arguments)?;
        return Ok(if remote {
            Command::RemoteLiveLan {
                address: socket_address(&target)?,
            }
        } else {
            Command::LiveLan {
                path: PathBuf::from(target),
            }
        });
    }
    let mut edit = LiveLanEdit::default();
    let mut key = None;
    let mut expected_revision = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--enable" | "--disable" => {
                let enabled = option == "--enable";
                if edit.enabled == Some(!enabled) {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--enable",
                        conflicts: "--disable",
                    });
                }
                edit.enabled = Some(enabled);
            }
            "--source" => {
                let value = required(&mut arguments, "LiveLAN source")?;
                edit.source = Some(value.parse().map_err(|_| ArgsError::InvalidChoice {
                    field: "LiveLAN source",
                    value,
                })?);
            }
            "--max-viewers" => {
                edit.max_viewers = Some(number(
                    &required(&mut arguments, "max viewers")?,
                    "max viewers",
                )?);
            }
            "--pin" => {
                if edit.pin == SecretEdit::Clear {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--pin",
                        conflicts: "--clear-pin",
                    });
                }
                let pin = secret("--pin", required(&mut arguments, "LiveLAN PIN")?)?;
                if !fm_types::live_lan::live_lan_pin_is_valid(&pin) {
                    return Err(ArgsError::InvalidLiveLanPin);
                }
                edit.pin = SecretEdit::Set(pin);
            }
            "--clear-pin" => {
                if matches!(edit.pin, SecretEdit::Set(_)) {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--clear-pin",
                        conflicts: "--pin",
                    });
                }
                edit.pin = SecretEdit::Clear;
            }
            "--key" => {
                let value = required(&mut arguments, "idempotency key")?;
                if value.trim().is_empty() {
                    return Err(ArgsError::BlankValue("idempotency key"));
                }
                key = Some(value);
            }
            "--expect" => {
                expected_revision = Some(number(
                    &required(&mut arguments, "expected revision")?,
                    "expected revision",
                )?);
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(if remote {
        Command::RemoteLiveLanSet {
            address: socket_address(&target)?,
            edit,
            key,
            expected_revision,
        }
    } else {
        Command::LiveLanSet {
            path: PathBuf::from(target),
            edit,
            key,
            expected_revision,
        }
    })
}

fn parse_remote_diagnostics(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    reject_extra(&mut arguments)?;
    Ok(Command::RemoteDiagnostics { address })
}

fn parse_remote_audio_strip(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let gain_millidb = number(&required(&mut arguments, "gain millidB")?, "gain millidB")?;
    let balance_basis_points = number(
        &required(&mut arguments, "balance basis points")?,
        "balance basis points",
    )?;
    let muted = boolean_choice(&required(&mut arguments, "muted")?, "muted")?;
    let soloed = boolean_choice(&required(&mut arguments, "soloed")?, "soloed")?;
    let follow_video = boolean_choice(&required(&mut arguments, "follow video")?, "follow video")?;
    let delay_samples = number(&required(&mut arguments, "delay samples")?, "delay samples")?;
    let mut arguments = arguments.peekable();
    let pan_basis_points = pan_option(&mut arguments)?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteAudioStrip {
        address,
        input,
        gain_millidb,
        balance_basis_points,
        pan_basis_points,
        muted,
        soloed,
        follow_video,
        delay_samples,
        key,
        expected_revision,
    })
}

fn parse_remote_rename(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let name = nonblank(&mut arguments, "input name")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteRename {
        address,
        input,
        name,
        key,
        expected_revision,
    })
}

/// Parses `remote-input-add <address> <solid|bars> <input> <name>
/// [<red> <green> <blue> <alpha>] [--key <key>] [--expect <revision>]`.
///
/// `solid` takes the four colour components, `bars` none. Every other kind is
/// refused here with the reason, before connecting: the daemon only realizes
/// simulated solid and bars inputs live.
fn parse_remote_input_add(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let kind = required(&mut arguments, "input kind")?;
    if !matches!(kind.as_str(), "solid" | "bars") {
        return Err(ArgsError::NotLiveInputKind(kind));
    }
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let name = nonblank(&mut arguments, "input name")?;
    let source = if kind == "solid" {
        let red = bounded_u8(&required(&mut arguments, "red")?, "red", u8::MAX)?;
        let green = bounded_u8(&required(&mut arguments, "green")?, "green", u8::MAX)?;
        let blue = bounded_u8(&required(&mut arguments, "blue")?, "blue", u8::MAX)?;
        let alpha = bounded_u8(&required(&mut arguments, "alpha")?, "alpha", u8::MAX)?;
        fm_protocol::LiveInputSource::Solid {
            red,
            green,
            blue,
            alpha,
        }
    } else {
        fm_protocol::LiveInputSource::Bars
    };
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteInputAdd {
        address,
        input,
        name,
        source,
        key,
        expected_revision,
    })
}

/// Changes to a network stream input's configuration; `None` keeps the
/// current value (IN-015).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct NetworkStreamOptions {
    /// A new URL; its `Debug` output is redacted.
    pub url: Option<StreamUrl>,
    pub transport: Option<RtspTransport>,
    pub jitter_buffer_ms: Option<u32>,
    pub backoff_min_ms: Option<u32>,
    pub backoff_max_ms: Option<u32>,
    pub give_up: Option<GiveUp>,
    pub timeout_ms: Option<u32>,
    pub stall: Option<StallSlate>,
    /// RTMP/SRT connection mode (IN-016, IN-017).
    pub mode: Option<StreamMode>,
    /// SRT receive latency.
    pub latency_ms: Option<u32>,
    /// SRT AES key length.
    pub key_length: Option<SrtKeyLength>,
    /// Which source channels feed the input's stereo audio.
    pub audio: Option<AudioChannels>,
    /// The SRT stream ID edit (keep, clear, or set). The ID is not secret,
    /// but shares the keep/clear/set shape; its `Debug` output is masked.
    pub stream_id: SecretEdit,
    /// The RTMP stream key edit; its `Debug` output is redacted.
    pub stream_key: SecretEdit,
    /// The SRT passphrase edit; its `Debug` output is redacted.
    pub passphrase: SecretEdit,
}

impl NetworkStreamOptions {
    /// `settings` with every given option applied.
    #[must_use]
    pub fn apply(&self, mut settings: NetworkStreamSettings) -> NetworkStreamSettings {
        if let Some(transport) = self.transport {
            settings.transport = transport;
        }
        if let Some(jitter) = self.jitter_buffer_ms {
            settings.jitter_buffer_ms = jitter;
        }
        if let Some(minimum) = self.backoff_min_ms {
            settings.reconnect.backoff_min_ms = minimum;
        }
        if let Some(maximum) = self.backoff_max_ms {
            settings.reconnect.backoff_max_ms = maximum;
        }
        if let Some(give_up) = self.give_up {
            settings.reconnect.give_up = give_up;
        }
        if let Some(timeout) = self.timeout_ms {
            settings.timeout_ms = timeout;
        }
        if let Some(stall) = self.stall {
            settings.stall = stall;
        }
        if let Some(mode) = self.mode {
            settings.mode = mode;
        }
        if let Some(latency) = self.latency_ms {
            settings.latency_ms = latency;
        }
        if let Some(key_length) = self.key_length {
            settings.key_length = key_length;
        }
        if let Some(audio) = self.audio {
            settings.audio = audio;
        }
        settings
    }

    /// The stream ID after this edit, given the current one.
    #[must_use]
    pub fn stream_id(&self, current: Option<&str>) -> Option<String> {
        self.stream_id.apply(current)
    }

    /// `config` with every given option applied, validated.
    ///
    /// # Errors
    ///
    /// Returns the first problem with the result.
    pub fn configure(
        &self,
        config: &NetworkStreamConfig,
    ) -> Result<NetworkStreamConfig, NetworkStreamError> {
        let configured = NetworkStreamConfig {
            url: self
                .url
                .as_ref()
                .map_or_else(|| config.url.clone(), |url| url.as_str().to_owned()),
            settings: self.apply(config.settings),
            stream_id: self.stream_id(config.stream_id.as_deref()),
            stream_key: self.stream_key.apply(config.stream_key.as_deref()),
            passphrase: self.passphrase.apply(config.passphrase.as_deref()),
        };
        configured.validate()?;
        Ok(configured)
    }
}

/// Parses the network stream options and `--key`/`--expect`:
/// `[--url <url>] [--transport auto|tcp|udp] [--jitter-ms <ms>]
/// [--backoff-min-ms <ms>] [--backoff-max-ms <ms>] [--give-up never|<n>]
/// [--timeout-ms <ms>] [--stall freeze|black]
/// [--mode caller|listener|rendezvous] [--latency-ms <ms>]
/// [--key-length 16|24|32] [--audio mix|1-2|3-4|5-6|7-8]
/// [--stream-id <id>] [--stream-key <key>] [--passphrase <passphrase>]`,
/// and for an edit (`allow_url`) `--clear-stream-id`, `--clear-stream-key`,
/// and `--clear-passphrase`. A secret option is never echoed.
#[allow(clippy::too_many_lines)]
fn network_stream_options(
    mut arguments: impl Iterator<Item = String>,
    allow_url: bool,
) -> Result<(NetworkStreamOptions, Option<String>, Option<u64>), ArgsError> {
    let mut options = NetworkStreamOptions::default();
    let mut key = None;
    let mut expected_revision = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--url" if allow_url => {
                options.url = Some(StreamUrl::new(nonblank(&mut arguments, "stream URL")?));
            }
            "--transport" => {
                let value = required(&mut arguments, "transport")?;
                options.transport = Some(RtspTransport::from_token(&value).ok_or(
                    ArgsError::InvalidChoice {
                        field: "transport",
                        value,
                    },
                )?);
            }
            "--jitter-ms" => {
                options.jitter_buffer_ms =
                    Some(number(&required(&mut arguments, "jitter")?, "jitter")?);
            }
            "--backoff-min-ms" => {
                options.backoff_min_ms = Some(number(
                    &required(&mut arguments, "minimum backoff")?,
                    "minimum backoff",
                )?);
            }
            "--backoff-max-ms" => {
                options.backoff_max_ms = Some(number(
                    &required(&mut arguments, "maximum backoff")?,
                    "maximum backoff",
                )?);
            }
            "--give-up" => {
                let value = required(&mut arguments, "give-up policy")?;
                options.give_up =
                    Some(GiveUp::from_token(&value).ok_or(ArgsError::InvalidChoice {
                        field: "give-up policy",
                        value,
                    })?);
            }
            "--timeout-ms" => {
                options.timeout_ms =
                    Some(number(&required(&mut arguments, "timeout")?, "timeout")?);
            }
            "--stall" => {
                let value = required(&mut arguments, "stall slate")?;
                options.stall = Some(StallSlate::from_token(&value).ok_or(
                    ArgsError::InvalidChoice {
                        field: "stall slate",
                        value,
                    },
                )?);
            }
            "--mode" => {
                let value = required(&mut arguments, "stream mode")?;
                options.mode = Some(StreamMode::from_token(&value).ok_or(
                    ArgsError::InvalidChoice {
                        field: "stream mode",
                        value,
                    },
                )?);
            }
            "--latency-ms" => {
                options.latency_ms =
                    Some(number(&required(&mut arguments, "latency")?, "latency")?);
            }
            "--key-length" => {
                let value = required(&mut arguments, "key length")?;
                options.key_length = Some(SrtKeyLength::from_token(&value).ok_or(
                    ArgsError::InvalidChoice {
                        field: "key length",
                        value,
                    },
                )?);
            }
            "--audio" => {
                let value = required(&mut arguments, "audio channels")?;
                options.audio = Some(AudioChannels::from_token(&value).ok_or(
                    ArgsError::InvalidChoice {
                        field: "audio channels",
                        value,
                    },
                )?);
            }
            "--stream-id" => {
                if options.stream_id == SecretEdit::Clear {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--stream-id",
                        conflicts: "--clear-stream-id",
                    });
                }
                options.stream_id = SecretEdit::Set(secret(
                    "--stream-id",
                    required(&mut arguments, "stream ID")?,
                )?);
            }
            "--clear-stream-id" if allow_url => {
                if matches!(options.stream_id, SecretEdit::Set(_)) {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--clear-stream-id",
                        conflicts: "--stream-id",
                    });
                }
                options.stream_id = SecretEdit::Clear;
            }
            "--stream-key" => {
                if options.stream_key == SecretEdit::Clear {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--stream-key",
                        conflicts: "--clear-stream-key",
                    });
                }
                options.stream_key = SecretEdit::Set(secret(
                    "--stream-key",
                    required(&mut arguments, "stream key")?,
                )?);
            }
            "--clear-stream-key" if allow_url => {
                if matches!(options.stream_key, SecretEdit::Set(_)) {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--clear-stream-key",
                        conflicts: "--stream-key",
                    });
                }
                options.stream_key = SecretEdit::Clear;
            }
            "--passphrase" => {
                if options.passphrase == SecretEdit::Clear {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--passphrase",
                        conflicts: "--clear-passphrase",
                    });
                }
                options.passphrase = SecretEdit::Set(secret(
                    "--passphrase",
                    required(&mut arguments, "passphrase")?,
                )?);
            }
            "--clear-passphrase" if allow_url => {
                if matches!(options.passphrase, SecretEdit::Set(_)) {
                    return Err(ArgsError::ConflictingOptions {
                        option: "--clear-passphrase",
                        conflicts: "--passphrase",
                    });
                }
                options.passphrase = SecretEdit::Clear;
            }
            "--key" => {
                let value = required(&mut arguments, "idempotency key")?;
                if value.trim().is_empty() {
                    return Err(ArgsError::BlankValue("idempotency key"));
                }
                key = Some(value);
            }
            "--expect" => {
                expected_revision = Some(number(
                    &required(&mut arguments, "expected revision")?,
                    "expected revision",
                )?);
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok((options, key, expected_revision))
}

/// Parses the network stream input commands (IN-015, IN-016, IN-017):
/// `input-add-stream <project> <input> <name> <url> [options]`,
/// `remote-input-add-stream <address> <input> <name> <url> [options]
/// [--key <key>] [--expect <revision>]`,
/// `input-stream-set <project> <input> [--url <url>] [options]
/// [--key <key>] [--expect <revision>]`, its `remote-` form, and
/// `input-stream-status <project>` / `remote-input-stream-status <address>`.
fn parse_network_stream(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    let action = command.strip_prefix("remote-").unwrap_or(command);
    if action == "input-stream-status" {
        reject_extra(&mut arguments)?;
        return Ok(match (path, address) {
            (Some(path), _) => Command::NetworkStreamStatus { path },
            (None, Some(address)) => Command::RemoteNetworkStreamStatus { address },
            (None, None) => unreachable!("a network stream command names a target"),
        });
    }
    let input = number(&required(&mut arguments, "input")?, "input")?;
    if action == "input-add-stream" {
        let name = nonblank(&mut arguments, "input name")?;
        let url = nonblank(&mut arguments, "stream URL")?;
        let (options, key, expected_revision) = network_stream_options(arguments, false)?;
        let config = Box::new(
            options
                .configure(&NetworkStreamConfig::new(url))
                .map_err(ArgsError::InvalidNetworkStream)?,
        );
        return match (path, address) {
            (Some(path), _) => {
                if key.is_some() || expected_revision.is_some() {
                    return Err(ArgsError::UnknownOption(
                        if key.is_some() { "--key" } else { "--expect" }.into(),
                    ));
                }
                Ok(Command::NetworkStreamInputAdd {
                    path,
                    input,
                    name,
                    config,
                })
            }
            (None, Some(address)) => Ok(Command::RemoteNetworkStreamInputAdd {
                address,
                input,
                name,
                config,
                key,
                expected_revision,
            }),
            (None, None) => unreachable!("a network stream command names a target"),
        };
    }
    let (options, key, expected_revision) = network_stream_options(arguments, true)?;
    if let Some(url) = &options.url {
        fm_types::network_stream::validate_stream_url(url.as_str())
            .map_err(ArgsError::InvalidNetworkStream)?;
    }
    Ok(match (path, address) {
        (Some(path), _) => Command::NetworkStreamSet {
            path,
            input,
            options,
            key,
            expected_revision,
        },
        (None, Some(address)) => Command::RemoteNetworkStreamSet {
            address,
            input,
            options,
            key,
            expected_revision,
        },
        (None, None) => unreachable!("a network stream command names a target"),
    })
}

/// Changes to a capture input's settings; `None` keeps the current value
/// (IN-001).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CaptureOptions {
    pub format: Option<CaptureFormatChoice>,
    pub audio: Option<CaptureAudio>,
    pub loss: Option<CaptureLossSlate>,
}

/// A capture input's requested format: `auto` picks the device format
/// nearest the program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureFormatChoice {
    Auto,
    Format(CaptureFormat),
}

impl CaptureOptions {
    /// `settings` with every given option applied.
    #[must_use]
    pub fn apply(&self, mut settings: CaptureSettings) -> CaptureSettings {
        if let Some(format) = self.format {
            settings.format = match format {
                CaptureFormatChoice::Auto => None,
                CaptureFormatChoice::Format(format) => Some(format),
            };
        }
        if let Some(audio) = &self.audio {
            settings.audio = audio.clone();
        }
        if let Some(loss) = self.loss {
            settings.loss = loss;
        }
        settings
    }
}

/// Parses the capture options and `--key`/`--expect`:
/// `[--format <WxH@num/den:pixel:scan>|auto] [--audio none|paired|alsa:<dev>]
/// [--loss hold|black]`.
fn capture_options(
    mut arguments: impl Iterator<Item = String>,
) -> Result<(CaptureOptions, Option<String>, Option<u64>), ArgsError> {
    let mut options = CaptureOptions::default();
    let mut key = None;
    let mut expected_revision = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--format" => {
                let value = required(&mut arguments, "capture format")?;
                options.format = Some(if value == "auto" {
                    CaptureFormatChoice::Auto
                } else {
                    CaptureFormatChoice::Format(
                        CaptureFormat::from_token(&value).map_err(ArgsError::InvalidCapture)?,
                    )
                });
            }
            "--audio" => {
                let value = required(&mut arguments, "capture audio")?;
                options.audio =
                    Some(CaptureAudio::from_token(&value).map_err(ArgsError::InvalidCapture)?);
            }
            "--loss" => {
                let value = required(&mut arguments, "loss slate")?;
                options.loss = Some(CaptureLossSlate::from_token(&value).ok_or(
                    ArgsError::InvalidChoice {
                        field: "loss slate",
                        value,
                    },
                )?);
            }
            "--key" => {
                let value = required(&mut arguments, "idempotency key")?;
                if value.trim().is_empty() {
                    return Err(ArgsError::BlankValue("idempotency key"));
                }
                key = Some(value);
            }
            "--expect" => {
                expected_revision = Some(number(
                    &required(&mut arguments, "expected revision")?,
                    "expected revision",
                )?);
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok((options, key, expected_revision))
}

/// Parses the capture input commands (IN-001):
/// `input-add-capture <project> <input> <name> <stable-key> [options]`,
/// `remote-input-add-capture <address> <input> <name> <stable-key> [options]
/// [--key <key>] [--expect <revision>]`, `input-capture-set <project>
/// <input> [options] [--key <key>] [--expect <revision>]`, its `remote-`
/// form, `input-capture-status <project>` /
/// `remote-input-capture-status <address>`, and
/// `remote-capture-devices <address>`.
fn parse_capture(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    let action = command.strip_prefix("remote-").unwrap_or(command);
    if matches!(action, "input-capture-status" | "capture-devices") {
        reject_extra(&mut arguments)?;
        return Ok(match (action, path, address) {
            ("input-capture-status", Some(path), _) => Command::CaptureStatus { path },
            ("input-capture-status", None, Some(address)) => {
                Command::RemoteCaptureStatus { address }
            }
            (_, None, Some(address)) => Command::RemoteCaptureDevices { address },
            _ => unreachable!("a capture command names a target"),
        });
    }
    let input = number(&required(&mut arguments, "input")?, "input")?;
    if action == "input-add-capture" {
        let name = nonblank(&mut arguments, "input name")?;
        let stable_key = nonblank(&mut arguments, "capture device key")?;
        let (options, key, expected_revision) = capture_options(arguments)?;
        let config = Box::new(CaptureInputConfig {
            stable_key,
            settings: options.apply(CaptureSettings::default()),
        });
        config.validate().map_err(ArgsError::InvalidCapture)?;
        return match (path, address) {
            (Some(path), _) => {
                if key.is_some() || expected_revision.is_some() {
                    return Err(ArgsError::UnknownOption(
                        if key.is_some() { "--key" } else { "--expect" }.into(),
                    ));
                }
                Ok(Command::CaptureInputAdd {
                    path,
                    input,
                    name,
                    config,
                })
            }
            (None, Some(address)) => Ok(Command::RemoteCaptureInputAdd {
                address,
                input,
                name,
                config,
                key,
                expected_revision,
            }),
            (None, None) => unreachable!("a capture command names a target"),
        };
    }
    let (options, key, expected_revision) = capture_options(arguments)?;
    Ok(match (path, address) {
        (Some(path), _) => Command::CaptureSet {
            path,
            input,
            options,
            key,
            expected_revision,
        },
        (None, Some(address)) => Command::RemoteCaptureSet {
            address,
            input,
            options,
            key,
            expected_revision,
        },
        (None, None) => unreachable!("a capture command names a target"),
    })
}

/// Parses the system audio input commands (AU-011):
/// `input-add-system-audio <project> <input> <name> <pulse|alsa> <device>`,
/// `remote-input-add-system-audio <address> <input> <name> <pulse|alsa>
/// <device> [--key <key>] [--expect <revision>]`, and
/// `input-system-audio-status <project>` /
/// `remote-input-system-audio-status <address>`, and
/// `remote-system-audio-devices <address>`.
fn parse_system_audio(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    let action = command.strip_prefix("remote-").unwrap_or(command);
    if action == "system-audio-devices" {
        reject_extra(&mut arguments)?;
        let address = address.expect("only the remote form lists devices");
        return Ok(Command::RemoteSystemAudioDevices { address });
    }
    if action == "input-system-audio-status" {
        reject_extra(&mut arguments)?;
        return Ok(match (path, address) {
            (Some(path), _) => Command::SystemAudioStatus { path },
            (None, Some(address)) => Command::RemoteSystemAudioStatus { address },
            (None, None) => unreachable!("a system audio command names a target"),
        });
    }
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let name = nonblank(&mut arguments, "input name")?;
    let backend_token = required(&mut arguments, "system audio backend")?;
    let backend =
        SystemAudioBackend::from_token(&backend_token).ok_or(ArgsError::InvalidChoice {
            field: "system audio backend (pulse or alsa)",
            value: backend_token,
        })?;
    let device = required(&mut arguments, "system audio device")?;
    let config = Box::new(SystemAudioConfig::new(backend, device));
    config.validate().map_err(ArgsError::InvalidSystemAudio)?;
    match (path, address) {
        (Some(path), _) => {
            reject_extra(&mut arguments)?;
            Ok(Command::SystemAudioInputAdd {
                path,
                input,
                name,
                config,
            })
        }
        (None, Some(address)) => {
            let (key, expected_revision) = command_options(arguments)?;
            Ok(Command::RemoteSystemAudioInputAdd {
                address,
                input,
                name,
                config,
                key,
                expected_revision,
            })
        }
        (None, None) => unreachable!("a system audio command names a target"),
    }
}

/// Parses the picture input commands (IN-008, IN-009):
/// `input-add-image <project> <input> <name> <picture> [--fit <fit>]`,
/// `input-add-slideshow <project> <input> <name> (--folder <folder> |
/// --picture <picture>[|<caption>]...) [slideshow options]`, and their
/// `remote-` forms naming an address and taking `[--key <key>]
/// [--expect <revision>]`. A picture or folder is an `asset://` URI or an
/// absolute host path.
#[allow(clippy::too_many_lines)]
fn parse_still_input_add(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    let image = command.ends_with("-image");
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let name = nonblank(&mut arguments, "input name")?;
    let picture = if image {
        Some(nonblank(&mut arguments, "picture")?)
    } else {
        None
    };
    let mut fit = fm_types::OutputScaling::Fit;
    let mut folder = None;
    let mut items = Vec::new();
    let mut slideshow = SlideshowConfig::new(SlideshowSource::List(Vec::new()));
    let mut key = None;
    let mut expected_revision = None;
    while let Some(option) = arguments.next() {
        match (option.as_str(), image, remote) {
            ("--fit", _, _) => {
                let value = required(&mut arguments, "fit")?;
                fit = value.parse().map_err(|_| ArgsError::InvalidChoice {
                    field: "fit (fit, fill, stretch, or 1:1)",
                    value,
                })?;
            }
            ("--folder", false, _) => folder = Some(nonblank(&mut arguments, "folder")?),
            ("--picture", false, _) => {
                let value = nonblank(&mut arguments, "picture")?;
                items.push(match value.split_once('|') {
                    Some((uri, caption)) => {
                        SlideshowItem::new(uri.trim()).with_caption(caption.trim())
                    }
                    None => SlideshowItem::new(value),
                });
            }
            ("--slide-frames", false, _) => {
                slideshow.slide_frames =
                    number(&required(&mut arguments, "slide frames")?, "slide frames")?;
            }
            ("--fade-frames", false, _) => {
                slideshow.transition = SlideTransitionStyle::Fade;
                slideshow.transition_frames =
                    number(&required(&mut arguments, "fade frames")?, "fade frames")?;
            }
            ("--cut", false, _) => slideshow.transition = SlideTransitionStyle::Cut,
            ("--ken-burns", false, _) => slideshow.ken_burns = true,
            ("--random", false, _) => {
                let seed = number(&required(&mut arguments, "random seed")?, "random seed")?;
                slideshow.order = SlideOrder::Random { seed };
            }
            ("--no-loop", false, _) => slideshow.loop_show = false,
            ("--manual", false, _) => slideshow.auto_advance = false,
            ("--key", _, true) => key = Some(nonblank(&mut arguments, "idempotency key")?),
            ("--expect", _, true) => {
                expected_revision = Some(number(
                    &required(&mut arguments, "expected revision")?,
                    "expected revision",
                )?);
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    if let Some(picture) = picture {
        let config = Box::new(ImageInputConfig::new(picture, fit));
        config.validate_request().map_err(ArgsError::InvalidStill)?;
        return Ok(match (path, address) {
            (Some(path), _) => Command::ImageInputAdd {
                path,
                input,
                name,
                config,
            },
            (None, Some(address)) => Command::RemoteImageInputAdd {
                address,
                input,
                name,
                config,
                key,
                expected_revision,
            },
            (None, None) => unreachable!("an image command names a target"),
        });
    }
    slideshow.fit = fit;
    slideshow.source = match (folder, items.is_empty()) {
        (Some(folder), true) => SlideshowSource::Folder { asset_uri: folder },
        (None, false) => SlideshowSource::List(items),
        (Some(_), false) => return Err(ArgsError::UnknownOption("--picture with --folder".into())),
        (None, true) => return Err(ArgsError::MissingValue("--folder or --picture")),
    };
    let config = Box::new(slideshow);
    config.validate_request().map_err(ArgsError::InvalidStill)?;
    Ok(match (path, address) {
        (Some(path), _) => Command::SlideshowInputAdd {
            path,
            input,
            name,
            config,
        },
        (None, Some(address)) => Command::RemoteSlideshowInputAdd {
            address,
            input,
            name,
            config,
            key,
            expected_revision,
        },
        (None, None) => unreachable!("a slideshow command names a target"),
    })
}

/// Parses `remote-page-navigate <address> <input>
/// <next|previous|first|last|goto <page>> [--key <key>] [--expect <revision>]`;
/// `goto` counts pages from 1.
fn parse_remote_page_navigate(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let action = required(&mut arguments, "page action")?;
    let page = if action == "goto" {
        let page: u32 = number(&required(&mut arguments, "page")?, "page")?;
        Some(page.checked_sub(1).ok_or(ArgsError::InvalidNumber {
            field: "page (counted from 1)",
            value: page.to_string(),
        })?)
    } else {
        None
    };
    let navigation =
        PagedNavigation::from_token(&action, page).ok_or(ArgsError::InvalidChoice {
            field: "page action (next, previous, first, last, or goto <page>)",
            value: action,
        })?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemotePageNavigate {
        address,
        input,
        navigation,
        key,
        expected_revision,
    })
}

/// Parses the screen capture input commands (IN-011):
/// `input-add-screen-capture <project> <input> <name> <target> [options]`,
/// `remote-input-add-screen-capture <address> <input> <name> <target>
/// [options] [--key <key>] [--expect <revision>]`,
/// `input-screen-capture-status <project>` /
/// `remote-input-screen-capture-status <address>`,
/// `remote-screen-capture-targets <address>`, and
/// `remote-screen-capture-pick <address> <input>`.
///
/// The target is `screen <index>`, `window <app> <title>`, or
/// `picked-window` (the window a Wayland server's sharing dialog picks); the
/// options
/// are `--match exact|contains|any`, `--window-id <id>`,
/// `--crop <left>,<top>,<right>,<bottom>`, `--cursor on|off`,
/// `--fps <1-60>`, `--stall freeze|black`, and `--audio none|system|app`
/// (the sound captured with the picture into the input's own mixer strip:
/// none, the whole system output, or only the window's application, which
/// needs a window target).
fn parse_screen_capture(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    let action = command.strip_prefix("remote-").unwrap_or(command);
    if action == "screen-capture-targets" {
        reject_extra(&mut arguments)?;
        let address = address.expect("only the remote form lists targets");
        return Ok(Command::RemoteScreenCaptureTargets { address });
    }
    if action == "screen-capture-pick" {
        let address = address.expect("only the remote form picks");
        let input = number(&required(&mut arguments, "input")?, "input")?;
        reject_extra(&mut arguments)?;
        return Ok(Command::RemoteScreenCapturePick { address, input });
    }
    if action == "input-screen-capture-status" {
        reject_extra(&mut arguments)?;
        return Ok(match (path, address) {
            (Some(path), _) => Command::ScreenCaptureStatus { path },
            (None, Some(address)) => Command::RemoteScreenCaptureStatus { address },
            (None, None) => unreachable!("a screen capture command names a target"),
        });
    }
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let name = nonblank(&mut arguments, "input name")?;
    let mut config = ScreenCaptureConfig::new(screen_capture_target(&mut arguments)?);
    let mut rest = Vec::new();
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--match" => {
                let value = required(&mut arguments, "title match")?;
                let title_match =
                    TitleMatch::from_token(&value).ok_or(ArgsError::InvalidChoice {
                        field: "title match (exact, contains, or any)",
                        value,
                    })?;
                window_rule(&mut config)?.title_match = title_match;
            }
            "--window-id" => {
                let id = number(&required(&mut arguments, "window id")?, "window id")?;
                window_rule(&mut config)?.window_id = Some(id);
            }
            "--crop" => config.crop = screen_capture_crop(required(&mut arguments, "crop")?)?,
            "--cursor" => {
                let value = required(&mut arguments, "cursor")?;
                config.cursor = on_off(value, "cursor (on or off)")?;
            }
            "--fps" => config.fps = number(&required(&mut arguments, "fps")?, "fps")?,
            "--stall" => {
                let value = required(&mut arguments, "stall slate")?;
                config.slate = fm_types::network_stream::StallSlate::from_token(&value).ok_or(
                    ArgsError::InvalidChoice {
                        field: "stall slate (freeze or black)",
                        value,
                    },
                )?;
            }
            "--audio" => {
                let value = required(&mut arguments, "audio")?;
                config.audio = fm_types::screen_capture::ScreenCaptureAudio::from_token(&value)
                    .ok_or(ArgsError::InvalidChoice {
                        field: "audio (none, system, or app)",
                        value,
                    })?;
            }
            _ => rest.push(option),
        }
    }
    config.validate().map_err(ArgsError::InvalidScreenCapture)?;
    let config = Box::new(config);
    match (path, address) {
        (Some(path), _) => {
            reject_extra(&mut rest.into_iter())?;
            Ok(Command::ScreenCaptureInputAdd {
                path,
                input,
                name,
                config,
            })
        }
        (None, Some(address)) => {
            let (key, expected_revision) = command_options(rest.into_iter())?;
            Ok(Command::RemoteScreenCaptureInputAdd {
                address,
                input,
                name,
                config,
                key,
                expected_revision,
            })
        }
        (None, None) => unreachable!("a screen capture command names a target"),
    }
}

/// The window rule of a window capture, which the window-only options
/// change.
/// Parses `screen <index>` or `window <app> <title>`.
fn screen_capture_target(
    arguments: &mut impl Iterator<Item = String>,
) -> Result<ScreenCaptureTarget, ArgsError> {
    let kind = required(
        arguments,
        "capture target (screen, window, or picked-window)",
    )?;
    match kind.as_str() {
        "picked-window" => Ok(ScreenCaptureTarget::Window(WindowRule::picked())),
        "screen" => Ok(ScreenCaptureTarget::Screen {
            index: number(&required(arguments, "screen index")?, "screen index")?,
        }),
        "window" => Ok(ScreenCaptureTarget::Window(WindowRule::exact(
            required(arguments, "window application")?,
            required(arguments, "window title")?,
            None,
        ))),
        _ => Err(ArgsError::InvalidChoice {
            field: "capture target (screen, window, or picked-window)",
            value: kind,
        }),
    }
}

/// Parses `--crop <left>,<top>,<right>,<bottom>`.
fn screen_capture_crop(value: String) -> Result<ScreenCaptureCrop, ArgsError> {
    let insets: Vec<&str> = value.split(',').collect();
    let [left, top, right, bottom] = insets.as_slice() else {
        return Err(ArgsError::InvalidNumber {
            field: "crop (left,top,right,bottom)",
            value,
        });
    };
    Ok(ScreenCaptureCrop {
        left: number(left, "crop left")?,
        top: number(top, "crop top")?,
        right: number(right, "crop right")?,
        bottom: number(bottom, "crop bottom")?,
    })
}

/// Parses an `on` or `off` option value.
fn on_off(value: String, field: &'static str) -> Result<bool, ArgsError> {
    match value.as_str() {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(ArgsError::InvalidChoice { field, value }),
    }
}

fn window_rule(config: &mut ScreenCaptureConfig) -> Result<&mut WindowRule, ArgsError> {
    match &mut config.target {
        ScreenCaptureTarget::Window(rule) => Ok(rule),
        ScreenCaptureTarget::Screen { .. } => Err(ArgsError::InvalidScreenCapture(
            ScreenCaptureError::InvalidWindowRule,
        )),
    }
}

/// Parses `remote-input-remove <address> <input> [--key <key>]
/// [--expect <revision>]`.
fn parse_remote_input_remove(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteInputRemove {
        address,
        input,
        key,
        expected_revision,
    })
}

fn parse_remote_preview(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemotePreview {
        address,
        input,
        key,
        expected_revision,
    })
}

fn parse_remote_cut(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteCut {
        address,
        key,
        expected_revision,
    })
}

/// Parses `remote-playlist <address> <operation> ...`:
///
/// * `set <off|all|one> <entries>` where `<entries>` is a comma-separated
///   list of `<id>:<input>:<frames>:<cut|fade|alpha_fade|wipe|slide|zoom>:<transition frames>:<0|1>`
///   (empty with `-`);
/// * `start [now|after:<frames>|at:<frame>] [--from <entry>]`;
/// * `stop`, `pause`, `resume`, `next`, `previous`, or `jump <entry>`.
fn parse_remote_playlist(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    use fm_protocol::{CommandPayload, PlaylistLoopMode, PlaylistOperation, PlaylistStart};

    let address = socket_address(&required(&mut arguments, "address")?)?;
    let operation = required(&mut arguments, "playlist operation")?;
    let control = |operation| CommandPayload::PlaylistControl { operation };
    let mut rest = Vec::new();
    let payload = match operation.as_str() {
        "set" => {
            let loop_mode = match required(&mut arguments, "loop mode")?.as_str() {
                "off" => PlaylistLoopMode::Off,
                "all" => PlaylistLoopMode::All,
                "one" => PlaylistLoopMode::One,
                other => {
                    return Err(ArgsError::InvalidChoice {
                        field: "loop mode",
                        value: other.to_owned(),
                    });
                }
            };
            let entries = required(&mut arguments, "playlist entries")?;
            CommandPayload::SetPlaylist {
                loop_mode,
                entries: playlist_entries(&entries)?,
            }
        }
        "start" => {
            let mut at = PlaylistStart::Now;
            let mut from = None;
            while let Some(argument) = arguments.next() {
                if argument == "--from" {
                    from = Some(number(&required(&mut arguments, "entry")?, "entry")?);
                } else if argument == "now" {
                    at = PlaylistStart::Now;
                } else if let Some(frames) = argument.strip_prefix("after:") {
                    at = PlaylistStart::AfterFrames(number(frames, "start frames")?);
                } else if let Some(frame) = argument.strip_prefix("at:") {
                    at = PlaylistStart::AtFrame(number(frame, "start frame")?);
                } else {
                    rest.push(argument);
                    rest.extend(arguments.by_ref());
                }
            }
            control(PlaylistOperation::Start { at, from })
        }
        "stop" => control(PlaylistOperation::Stop),
        "pause" => control(PlaylistOperation::Pause),
        "resume" => control(PlaylistOperation::Resume),
        "next" => control(PlaylistOperation::Next),
        "previous" => control(PlaylistOperation::Previous),
        "jump" => control(PlaylistOperation::Jump {
            entry: number(&required(&mut arguments, "entry")?, "entry")?,
        }),
        other => {
            return Err(ArgsError::InvalidChoice {
                field: "playlist operation",
                value: other.to_owned(),
            });
        }
    };
    rest.extend(arguments);
    let (key, expected_revision) = command_options(rest.into_iter())?;
    Ok(Command::RemotePlaylist {
        address,
        payload,
        key,
        expected_revision,
    })
}

/// Parses the cue list commands (RC-017), each taking a project path or,
/// in its `remote-` form, a daemon address first:
///
/// * `cue-lists`;
/// * `cue-list-set <list> <name> <cues>` where `<cues>` is a `;`-separated
///   list (`-` for none) of `<id>:<name>:<follow 0|1>:<actions>` and
///   `<actions>` a `,`-separated list (empty for none) of wire action
///   tokens, e.g. `take.3.fade.25`, `overlay_on.1.4`, `wait.50`,
///   `delay.1500`, `media_restart.4`, `media_seek.4.250`, `title_number.5.2.add.1`
///   (adds one to number field 2 of title 5, Protocol 2.65; `set` sets it),
///   `run_script.2`
///   (runs project script 2 as one step, Protocol 2.60) — except that
///   `title_text.<input>.<field>.<text>` takes plain text. A backslash
///   escapes the next character, so names and title text may hold the
///   separators: `\;`, `\:`, `\,`, and `\\` stand for `;`, `:`, `,`, and
///   `\` (see [`split_escaped`]);
/// * `cue-list-remove <list>`;
/// * `go <list> [<cue>]`, `cue-standby <list> <cue>`, and `cue-back`,
///   `cue-skip`, `cue-stop`, or `cue-reset <list>`;
/// * `cue-arm <list> <milliseconds> [<cue>]` and `cue-disarm <list>`
///   (Protocol 2.64).
///
/// `go` and `cue-arm` take `--selected <input>`, the input a cue action on
/// the selected input acts on (RC-004); without it such a GO or arm is
/// refused.
fn parse_cue(command: &str, arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    use fm_protocol::{CommandPayload, CueListSpec, CueOperation};

    let mut arguments = arguments.peekable();
    let (base, remote) = command
        .strip_prefix("remote-")
        .map_or((command, false), |base| (base, true));
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    if base == "cue-lists" {
        if let Some(argument) = arguments.next() {
            return Err(ArgsError::UnexpectedArgument(argument));
        }
        return Ok(match (path, address) {
            (Some(path), _) => Command::CueLists { path },
            (None, Some(address)) => Command::RemoteCueLists { address },
            (None, None) => unreachable!("a cue command names a project or an address"),
        });
    }
    let list = number(&required(&mut arguments, "cue list")?, "cue list")?;
    let control = |operation| CommandPayload::CueControl { list, operation };
    let mut payload = match base {
        "cue-list-set" => {
            let name = required(&mut arguments, "cue list name")?;
            let cues = cue_specs(&required(&mut arguments, "cues")?)?;
            CommandPayload::SetCueList {
                list,
                spec: Box::new(CueListSpec { name, cues }),
                position: None,
            }
        }
        "cue-list-remove" => CommandPayload::RemoveCueList { list },
        "go" => {
            let cue = match arguments.next_if(|argument| !argument.starts_with("--")) {
                Some(cue) => Some(number(&cue, "cue")?),
                None => None,
            };
            control(CueOperation::Go {
                cue,
                targets: fm_protocol::WireTargetBindings::default(),
            })
        }
        "cue-standby" => control(CueOperation::Standby {
            cue: number(&required(&mut arguments, "cue")?, "cue")?,
        }),
        "cue-back" => control(CueOperation::Back),
        "cue-skip" => control(CueOperation::Skip),
        "cue-stop" => control(CueOperation::Stop),
        "cue-reset" => control(CueOperation::Reset),
        "cue-arm" => {
            let in_ms = required(&mut arguments, "milliseconds")?;
            let in_ms = in_ms
                .parse::<u32>()
                .ok()
                .filter(|millis| (1..=fm_protocol::MAX_CUE_ARM_MS).contains(millis))
                .ok_or(ArgsError::InvalidNumber {
                    field: "milliseconds",
                    value: in_ms,
                })?;
            let cue = match arguments.next_if(|argument| !argument.starts_with("--")) {
                Some(cue) => Some(number(&cue, "cue")?),
                None => None,
            };
            control(CueOperation::Arm {
                cue,
                in_ms,
                targets: fm_protocol::WireTargetBindings::default(),
            })
        }
        "cue-disarm" => control(CueOperation::Disarm),
        other => return Err(ArgsError::UnknownCommand(other.to_owned())),
    };
    let (selected, key, expected_revision) =
        fire_options(arguments, matches!(base, "go" | "cue-arm"))?;
    if let CommandPayload::CueControl {
        operation: CueOperation::Go { targets, .. } | CueOperation::Arm { targets, .. },
        ..
    } = &mut payload
    {
        targets.selected = selected;
    }
    Ok(match (path, address) {
        (Some(path), _) => Command::Cue {
            path,
            payload,
            key,
            expected_revision,
        },
        (None, Some(address)) => Command::RemoteCue {
            address,
            payload,
            key,
            expected_revision,
        },
        (None, None) => unreachable!("a cue command names a project or an address"),
    })
}

/// Splits `value` at every `separator` not escaped by a backslash, at most
/// `limit` pieces (the last keeps the rest). Each piece keeps its escapes so
/// a nested list can be split again; [`unescape`] resolves them. `None` when
/// `value` ends in a lone backslash.
fn split_escaped(value: &str, separator: char, limit: usize) -> Option<Vec<&str>> {
    let mut pieces = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == separator && pieces.len() + 1 < limit {
            pieces.push(&value[start..index]);
            start = index + character.len_utf8();
        }
    }
    if escaped {
        return None;
    }
    pieces.push(&value[start..]);
    Some(pieces)
}

/// Resolves the backslash escapes [`split_escaped`] kept: `\x` stands for
/// `x`.
fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            out.extend(characters.next());
        } else {
            out.push(character);
        }
    }
    out
}

/// Parses the shortcut set commands (RC-004), each taking a project path
/// or, in its `remote-` form, a daemon address first: `shortcuts`,
/// `shortcut-export <file> [--name <name>]`, and `shortcut-import <file>`.
fn parse_shortcut_set(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let (base, remote) = command
        .strip_prefix("remote-")
        .map_or((command, false), |base| (base, true));
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    let command = match base {
        "shortcuts" => ShortcutSetCommand::List,
        "shortcut-export" => {
            let file = required_path(&mut arguments, "shortcut set file")?;
            let mut name = None;
            while let Some(option) = arguments.next() {
                match option.as_str() {
                    "--name" if name.is_none() => {
                        name = Some(nonblank(&mut arguments, "shortcut set name")?);
                    }
                    _ => return Err(ArgsError::UnknownOption(option)),
                }
            }
            ShortcutSetCommand::Export { file, name }
        }
        _ => ShortcutSetCommand::Import {
            file: required_path(&mut arguments, "shortcut set file")?,
        },
    };
    reject_extra(&mut arguments)?;
    Ok(match (path, address) {
        (Some(path), _) => Command::Shortcuts { path, command },
        (None, Some(address)) => Command::RemoteShortcuts { address, command },
        (None, None) => unreachable!("a shortcut command names a project or an address"),
    })
}

/// Parses `shortcut-fire <project> <shortcut>` and
/// `remote-shortcut-fire <address> <shortcut>` (RC-004): fires project shortcut `<shortcut>` as
/// the command it stands for, with `--selected <input>` the input its
/// actions on the selected input act on, and `--value <0..1000>` the
/// value an action taking its gain from the firing control reads (per
/// mille of the control's travel). The fire travels like a cue
/// command: a local fire restores, runs, and saves the project.
fn parse_shortcut_fire(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let remote = command.starts_with("remote-");
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    let shortcut = number(&required(&mut arguments, "shortcut")?, "shortcut")?;
    let mut value = None;
    let mut rest = Vec::new();
    while let Some(argument) = arguments.next() {
        if argument == "--value" {
            if value.is_some() {
                return Err(ArgsError::UnexpectedArgument(argument));
            }
            let text = required(&mut arguments, "fire value")?;
            value = Some(
                text.parse::<u16>()
                    .ok()
                    .filter(|_| !text.starts_with('+'))
                    .and_then(fm_protocol::FireValue::new)
                    .ok_or(ArgsError::InvalidNumber {
                        field: "fire value",
                        value: text,
                    })?,
            );
        } else {
            rest.push(argument);
        }
    }
    let (selected, key, expected_revision) = fire_options(rest.into_iter(), true)?;
    let payload = fm_protocol::CommandPayload::FireShortcut {
        shortcut,
        targets: fm_protocol::WireTargetBindings {
            selected,
            ..fm_protocol::WireTargetBindings::NONE
        },
        value,
    };
    Ok(match (path, address) {
        (Some(path), _) => Command::Cue {
            path,
            payload,
            key,
            expected_revision,
        },
        (None, Some(address)) => Command::RemoteCue {
            address,
            payload,
            key,
            expected_revision,
        },
        (None, None) => unreachable!("a shortcut fire names a project or an address"),
    })
}

/// Parses the project script commands (RC-011), each taking a project path
/// or, in its `remote-` form, a daemon address first:
///
/// * `scripts`;
/// * `script-set <script> <name> <source-file>` (`-` reads the source from
///   standard input);
/// * `script-remove <script>`;
/// * `script-run <script>`, where the script is its id or, when that is not
///   a nonzero number, its exact name;
/// * `script-stop [--run <run>]`: stops run `<run>` paused at a wait, or
///   clears its failed run's report; without `--run`, stops every run and
///   clears every report (Protocol 2.62; `--run` since 2.69).
fn parse_script(
    command: &str,
    arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let mut arguments = arguments.peekable();
    let (base, remote) = command
        .strip_prefix("remote-")
        .map_or((command, false), |base| (base, true));
    let (path, address) = if remote {
        (
            None,
            Some(socket_address(&required(&mut arguments, "address")?)?),
        )
    } else {
        (Some(required_path(&mut arguments, "project path")?), None)
    };
    if base == "scripts" {
        if let Some(argument) = arguments.next() {
            return Err(ArgsError::UnexpectedArgument(argument));
        }
        return Ok(match (path, address) {
            (Some(path), _) => Command::Scripts { path },
            (None, Some(address)) => Command::RemoteScripts { address },
            (None, None) => unreachable!("a script command names a project or an address"),
        });
    }
    let command = match base {
        "script-set" => ScriptCommand::Set {
            script: number(&required(&mut arguments, "script")?, "script")?,
            name: required(&mut arguments, "script name")?,
            source: PathBuf::from(required(&mut arguments, "script source")?),
        },
        "script-remove" => ScriptCommand::Remove {
            script: number(&required(&mut arguments, "script")?, "script")?,
        },
        "script-run" => ScriptCommand::Run {
            script: ScriptRef::parse(required(&mut arguments, "script")?),
        },
        "script-stop" => ScriptCommand::Stop {
            run: if arguments.next_if(|argument| argument == "--run").is_some() {
                let run = number(&required(&mut arguments, "run")?, "run")?;
                if run == 0 {
                    return Err(ArgsError::InvalidChoice {
                        field: "run",
                        value: "0".to_owned(),
                    });
                }
                Some(run)
            } else {
                None
            },
        },
        other => return Err(ArgsError::UnknownCommand(other.to_owned())),
    };
    let (key, expected_revision) = command_options(arguments)?;
    Ok(match (path, address) {
        (Some(path), _) => Command::Script {
            path,
            command,
            key,
            expected_revision,
        },
        (None, Some(address)) => Command::RemoteScript {
            address,
            command,
            key,
            expected_revision,
        },
        (None, None) => unreachable!("a script command names a project or an address"),
    })
}

/// Parses `cue-list-set` cues (see [`parse_cue`]).
fn cue_specs(value: &str) -> Result<Vec<fm_protocol::WireCue>, ArgsError> {
    if value == "-" {
        return Ok(Vec::new());
    }
    let invalid = |field, item: &str| ArgsError::InvalidChoice {
        field,
        value: item.to_owned(),
    };
    split_escaped(value, ';', usize::MAX)
        .ok_or_else(|| invalid("cues", value))?
        .into_iter()
        .map(|item| {
            let parts = split_escaped(item, ':', 4).ok_or_else(|| invalid("cue", item))?;
            let [id, name, follow, actions] = parts[..] else {
                return Err(invalid("cue", item));
            };
            Ok(fm_protocol::WireCue {
                id: id.parse().map_err(|_| invalid("cue", item))?,
                name: unescape(name),
                auto_follow: match follow {
                    "0" => false,
                    "1" => true,
                    _ => return Err(invalid("cue", item)),
                },
                actions: if actions.is_empty() {
                    Vec::new()
                } else {
                    split_escaped(actions, ',', usize::MAX)
                        .ok_or_else(|| invalid("cue action", actions))?
                        .into_iter()
                        .map(|token| cue_action(token).ok_or_else(|| invalid("cue action", token)))
                        .collect::<Result<_, _>>()?
                },
            })
        })
        .collect()
}

/// One cue action token; `title_text` takes plain (escaped) text, which is
/// hex-encoded into the wire token here. Other tokens hold no escapes.
fn cue_action(token: &str) -> Option<fm_protocol::WireCueAction> {
    use core::fmt::Write as _;

    if let Some(rest) = token.strip_prefix("title_text.") {
        let mut parts = rest.splitn(3, '.');
        let (input, field, text) = (parts.next()?, parts.next()?, parts.next()?);
        let mut hex = String::with_capacity(text.len() * 2);
        for byte in unescape(text).bytes() {
            write!(hex, "{byte:02x}").ok()?;
        }
        return fm_protocol::parse_cue_action_token(&format!("title_text.{input}.{field}.{hex}"));
    }
    if token.contains('\\') {
        return None;
    }
    fm_protocol::parse_cue_action_token(token)
}

fn playlist_entries(value: &str) -> Result<Vec<fm_protocol::PlaylistEntrySpec>, ArgsError> {
    use fm_protocol::PlaylistTransitionKind;

    if value == "-" {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|item| {
            let invalid = || ArgsError::InvalidChoice {
                field: "playlist entry",
                value: item.to_owned(),
            };
            let parts: Vec<_> = item.split(':').collect();
            let [id, input, frames, transition, transition_frames, enabled] = parts[..] else {
                return Err(invalid());
            };
            Ok(fm_protocol::PlaylistEntrySpec {
                id: id.parse().map_err(|_| invalid())?,
                input: input
                    .parse::<u128>()
                    .ok()
                    .and_then(core::num::NonZeroU128::new)
                    .map(fm_protocol::WireInputId::new)
                    .ok_or_else(invalid)?,
                duration_frames: frames.parse().map_err(|_| invalid())?,
                transition: match transition {
                    "cut" => PlaylistTransitionKind::Cut,
                    "fade" => PlaylistTransitionKind::Fade,
                    "alpha_fade" => PlaylistTransitionKind::AlphaFade,
                    "wipe" => PlaylistTransitionKind::Wipe,
                    "slide" => PlaylistTransitionKind::Slide,
                    "zoom" => PlaylistTransitionKind::Zoom,
                    _ => return Err(invalid()),
                },
                transition_frames: transition_frames.parse().map_err(|_| invalid())?,
                enabled: match enabled {
                    "0" => false,
                    "1" => true,
                    _ => return Err(invalid()),
                },
            })
        })
        .collect()
}

/// Parses `remote-undo|remote-redo <address> [--edit <revision>] [--key <key>]
/// [--expect <revision>]`. `--edit` names the edit the operator expects to
/// revert so a concurrent editor's newer edit is refused, not reverted.
fn parse_remote_history(
    redo: bool,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let (expected_edit, rest) = take_numeric_option(arguments, "--edit", "expected edit")?;
    if expected_edit == Some(0) {
        return Err(ArgsError::InvalidNumber {
            field: "expected edit",
            value: "0".to_owned(),
        });
    }
    let (key, expected_revision) = command_options(rest.into_iter())?;
    Ok(Command::RemoteUndo {
        address,
        redo,
        expected_edit,
        key,
        expected_revision,
    })
}

/// Parses `remote-lock <address> on|off [--input <id>] [--key <key>]
/// [--expect <revision>]`.
fn parse_remote_lock(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let locked = boolean_choice(&required(&mut arguments, "lock state")?, "lock state")?;
    let (input, rest) = take_numeric_option(arguments, "--input", "input")?;
    let (key, expected_revision) = command_options(rest.into_iter())?;
    Ok(Command::RemoteLock {
        address,
        locked,
        input,
        key,
        expected_revision,
    })
}

/// Removes one optional `<flag> <number>` pair from the remaining options,
/// returning the value and the untouched arguments.
fn take_numeric_option<T: core::str::FromStr>(
    mut arguments: impl Iterator<Item = String>,
    flag: &str,
    field: &'static str,
) -> Result<(Option<T>, Vec<String>), ArgsError> {
    let mut value = None;
    let mut rest = Vec::new();
    while let Some(argument) = arguments.next() {
        if argument == flag {
            if value.is_some() {
                return Err(ArgsError::UnexpectedArgument(argument));
            }
            value = Some(number(&required(&mut arguments, field)?, field)?);
        } else {
            rest.push(argument);
            if let Some(next) = arguments.next() {
                rest.push(next);
            }
        }
    }
    Ok((value, rest))
}

fn parse_remote_timed_transition(
    command: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let frames = number(&required(&mut arguments, "frames")?, "frames")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(match command {
        "remote-fade" => Command::RemoteFade {
            address,
            frames,
            key,
            expected_revision,
        },
        "remote-alpha-fade" => Command::RemoteAlphaFade {
            address,
            frames,
            key,
            expected_revision,
        },
        "remote-slide" => Command::RemoteSlide {
            address,
            frames,
            key,
            expected_revision,
        },
        "remote-zoom" => Command::RemoteZoom {
            address,
            frames,
            key,
            expected_revision,
        },
        "remote-wipe" => Command::RemoteWipe {
            address,
            frames,
            key,
            expected_revision,
        },
        _ => unreachable!("caller only dispatches timed remote transitions"),
    })
}

fn parse_remote_stinger(mut arguments: impl Iterator<Item = String>) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let slot = stinger_slot(&required(&mut arguments, "Stinger slot")?)?;
    let frames = number(&required(&mut arguments, "frames")?, "frames")?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteStinger {
        address,
        slot,
        frames,
        key,
        expected_revision,
    })
}

fn parse_remote_stinger_configuration(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let (slot, media_input, preload, cut_point_frames, audio_policy, fallback) =
        parse_stinger_configuration_fields(&mut arguments)?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteStingerConfigure {
        address,
        slot,
        media_input,
        preload,
        cut_point_frames,
        audio_policy,
        fallback,
        key,
        expected_revision,
    })
}

fn parse_remote_stinger_removal(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let address = socket_address(&required(&mut arguments, "address")?)?;
    let slot = stinger_slot(&required(&mut arguments, "Stinger slot")?)?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteStingerRemove {
        address,
        slot,
        key,
        expected_revision,
    })
}

/// `input-media-<operation> <target> <input> <operands...> [--key K]
/// [--expect N]`.
fn parse_input_media(
    target: MediaTarget,
    operation: &str,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let operation = match operation {
        "play" => MediaOperation::Play,
        "pause" => MediaOperation::Pause,
        "stop" => MediaOperation::Stop,
        "complete" => MediaOperation::CompleteSource,
        "seek" => MediaOperation::Seek(media_seek_target(&required(
            &mut arguments,
            "seek position",
        )?)?),
        "trim" => {
            let trim_in = number(&required(&mut arguments, "trim in")?, "trim in")?;
            let trim_out = required(&mut arguments, "trim out")?;
            let trim_out = if trim_out == "end" {
                None
            } else {
                Some(number(&trim_out, "trim out")?)
            };
            MediaOperation::Trim { trim_in, trim_out }
        }
        "rate" => MediaOperation::Rate {
            speed_basis_points: number(
                &required(&mut arguments, "speed basis points")?,
                "speed basis points",
            )?,
        },
        "end-action" => {
            let value = required(&mut arguments, "end action")?;
            MediaOperation::EndAction(MediaEndAction::from_token(&value).ok_or(
                ArgsError::InvalidChoice {
                    field: "end action",
                    value,
                },
            )?)
        }
        "end-trigger" => {
            let value = required(&mut arguments, "end-trigger lead")?;
            MediaOperation::EndTriggerLead(if value == "off" {
                None
            } else {
                Some(number(&value, "end-trigger lead")?)
            })
        }
        "audio-track" => {
            let value = required(&mut arguments, "audio track")?;
            MediaOperation::AudioTrack(if value == "none" {
                None
            } else {
                Some(number(&value, "audio track")?)
            })
        }
        "audio-policy" => {
            let value = required(&mut arguments, "audio policy")?;
            MediaOperation::AudioPolicy(MediaAudioPolicy::from_token(&value).ok_or(
                ArgsError::InvalidChoice {
                    field: "audio policy",
                    value,
                },
            )?)
        }
        other => {
            return Err(ArgsError::UnknownCommand(format!("input-media-{other}")));
        }
    };
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::InputMedia {
        target,
        input,
        operation,
        key,
        expected_revision,
    })
}

/// `<input> next|previous|first|last|goto <page>`, the page one-based.
fn parse_input_page(
    path: PathBuf,
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let input = number(&required(&mut arguments, "input")?, "input")?;
    let value = required(&mut arguments, "page action")?;
    let page = if value == "goto" {
        let page: u32 = number(&required(&mut arguments, "page")?, "page")?;
        Some(page.checked_sub(1).ok_or(ArgsError::InvalidNumber {
            field: "page",
            value: page.to_string(),
        })?)
    } else {
        None
    };
    let navigation = fm_types::paged::PagedNavigation::from_token(&value, page).ok_or(
        ArgsError::InvalidChoice {
            field: "page action",
            value,
        },
    )?;
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::InputPage {
        path,
        input,
        navigation,
        key,
        expected_revision,
    })
}

/// A source frame (`120`) or a rational time in seconds (`6/5s`, `3s`).
fn media_seek_target(value: &str) -> Result<MediaSeekTarget, ArgsError> {
    let invalid = || ArgsError::InvalidNumber {
        field: "seek position",
        value: value.to_owned(),
    };
    let Some(seconds) = value.strip_suffix('s') else {
        return number(value, "seek position").map(MediaSeekTarget::Frame);
    };
    let (numerator, denominator) = seconds.split_once('/').unwrap_or((seconds, "1"));
    let numerator: u64 = numerator.parse().map_err(|_| invalid())?;
    let denominator: u64 = denominator.parse().map_err(|_| invalid())?;
    if denominator == 0 {
        return Err(invalid());
    }
    Ok(MediaSeekTarget::Time {
        numerator,
        denominator,
    })
}

/// Parses one input lifecycle command's operands (IN-026) and its
/// `[--key <key>] [--expect <revision>]` options.
#[allow(clippy::too_many_lines)] // one arm per lifecycle operation
fn parse_input_lifecycle(
    target: MediaTarget,
    operation: &str,
    arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    use fm_protocol::InputLifecycleOperation as Operation;
    let mut arguments = arguments.peekable();
    let wire = |value: u128| -> Result<fm_protocol::WireInputId, ArgsError> {
        let id = core::num::NonZeroU128::new(value).ok_or(ArgsError::InvalidNumber {
            field: "input",
            value: value.to_string(),
        })?;
        Ok(fm_protocol::WireInputId::from_domain(
            fm_types::InputId::new(id),
        ))
    };
    let input = |arguments: &mut core::iter::Peekable<_>| -> Result<_, ArgsError> {
        wire(number(&required(arguments, "input")?, "input")?)
    };
    let positional = |arguments: &mut core::iter::Peekable<_>| -> Vec<String> {
        let mut values = Vec::new();
        while let Some(value) = arguments.next_if(|value: &String| !value.starts_with("--")) {
            values.push(value);
        }
        values
    };
    let operation = match operation {
        "categories" => {
            let mut categories = Vec::new();
            for entry in positional(&mut arguments) {
                let (id, name) = entry
                    .split_once('=')
                    .ok_or_else(|| ArgsError::InvalidChoice {
                        field: "category (<id>=<name>)",
                        value: entry.clone(),
                    })?;
                categories.push(fm_protocol::InputCategoryStatus {
                    id: number(id, "category id")?,
                    name: name.to_owned(),
                });
            }
            Operation::SetCategories { categories }
        }
        "category" => {
            let input = input(&mut arguments)?;
            let category = required(&mut arguments, "category")?;
            Operation::SetCategory {
                input,
                category: if category == "none" {
                    None
                } else {
                    Some(number(&category, "category")?)
                },
            }
        }
        "color" => {
            let input = input(&mut arguments)?;
            let color = required(&mut arguments, "colour")?;
            Operation::SetColor {
                input,
                color: if color == "none" {
                    None
                } else {
                    Some(fm_protocol::InputColorLabel::from_token(&color).ok_or(
                        ArgsError::InvalidChoice {
                            field: "colour",
                            value: color,
                        },
                    )?)
                },
            }
        }
        "pause" => Operation::SetPaused {
            input: input(&mut arguments)?,
            paused: boolean_choice(&required(&mut arguments, "paused")?, "paused")?,
        },
        "close" => Operation::SetClosed {
            input: input(&mut arguments)?,
            closed: boolean_choice(&required(&mut arguments, "closed")?, "closed")?,
        },
        "duplicate" => {
            let source = input(&mut arguments)?;
            let input = input(&mut arguments)?;
            let name = positional(&mut arguments).into_iter().next();
            Operation::Duplicate {
                source,
                input,
                name,
            }
        }
        "replace" => {
            let input = input(&mut arguments)?;
            let source = input_source(&required(&mut arguments, "source")?)?;
            Operation::ReplaceSource { input, source }
        }
        "import-batch" => {
            let first: u128 = number(&required(&mut arguments, "first input")?, "first input")?;
            let files = positional(&mut arguments);
            if files.is_empty() {
                return Err(ArgsError::MissingValue("media file"));
            }
            let mut items = Vec::with_capacity(files.len());
            for (offset, file) in (0_u128..).zip(files) {
                items.push(fm_protocol::ImportItemSpec {
                    input: wire(first.saturating_add(offset))?,
                    name: None,
                    uri: host_media_uri(&file),
                });
            }
            Operation::Import { items }
        }
        _ => return Err(ArgsError::UnknownCommand(format!("input-{operation}"))),
    };
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::InputLifecycle {
        target,
        operation,
        key,
        expected_revision,
    })
}

/// An `asset://` URI as given, otherwise the absolute form of a host path,
/// which the daemon imports into its bundle.
fn host_media_uri(value: &str) -> String {
    if value.starts_with("asset://") {
        return value.to_owned();
    }
    std::path::absolute(value).map_or_else(
        |_| value.to_owned(),
        |path| path.to_string_lossy().into_owned(),
    )
}

/// Parses a replacement source: `solid:RRGGBBAA`, `bars`, `media:<file or
/// asset://key>`, `device:<stable key>`, `network:<endpoint>`, or
/// `system-audio:<pulse|alsa>:<device>`, each generator optionally followed
/// by `:<tone hz>`.
fn input_source(value: &str) -> Result<fm_protocol::WireInputSource, ArgsError> {
    use fm_protocol::{WireGeneratorVideo, WireInputSource};
    let invalid = || ArgsError::InvalidChoice {
        field: "source (solid:RRGGBBAA, bars, media:<file>, device:<key>, network:<endpoint>, \
                system-audio:<pulse|alsa>:<device>)",
        value: value.to_owned(),
    };
    let tone = |rest: Option<&str>| -> Result<Option<u32>, ArgsError> {
        rest.map(|hz| number(hz, "tone hz")).transpose()
    };
    let (kind, rest) = value
        .split_once(':')
        .map_or((value, None), |(kind, rest)| (kind, Some(rest)));
    match kind {
        "bars" => Ok(WireInputSource::Generator {
            video: WireGeneratorVideo::Bars,
            tone_hz: tone(rest)?,
        }),
        "solid" => {
            let rest = rest.ok_or_else(invalid)?;
            let (hex, hz) = rest
                .split_once(':')
                .map_or((rest, None), |(hex, hz)| (hex, Some(hz)));
            if hex.len() != 8 {
                return Err(invalid());
            }
            let mut rgba = [0_u8; 4];
            for (index, channel) in rgba.iter_mut().enumerate() {
                *channel = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)
                    .map_err(|_| invalid())?;
            }
            Ok(WireInputSource::Generator {
                video: WireGeneratorVideo::Solid { rgba },
                tone_hz: tone(hz)?,
            })
        }
        "media" => Ok(WireInputSource::Media {
            uri: host_media_uri(rest.filter(|rest| !rest.is_empty()).ok_or_else(invalid)?),
        }),
        "device" => Ok(WireInputSource::Device {
            stable_key: rest
                .filter(|rest| !rest.is_empty())
                .ok_or_else(invalid)?
                .to_owned(),
        }),
        "network" => Ok(WireInputSource::Network {
            endpoint: rest
                .filter(|rest| !rest.is_empty())
                .ok_or_else(invalid)?
                .to_owned(),
        }),
        "system-audio" => {
            let (backend, device) = rest
                .and_then(|rest| rest.split_once(':'))
                .ok_or_else(invalid)?;
            let config = SystemAudioConfig::new(
                SystemAudioBackend::from_token(backend).ok_or_else(invalid)?,
                device,
            );
            config.validate().map_err(ArgsError::InvalidSystemAudio)?;
            Ok(WireInputSource::SystemAudio(config))
        }
        _ => Err(invalid()),
    }
}

/// A fire's selected input, idempotency key, and expected revision.
type FireOptions = (
    Option<fm_protocol::WireInputId>,
    Option<String>,
    Option<u64>,
);

/// Reads a fire's options: `--selected <input>` (RC-004) when `selectable`,
/// then [`command_options`]. The selected input is the one a shortcut or
/// cue action on the selected input acts on; the CLI has no console
/// selection of its own, so without the option nothing is selected and
/// such an action is refused.
fn fire_options(
    mut arguments: impl Iterator<Item = String>,
    selectable: bool,
) -> Result<FireOptions, ArgsError> {
    let mut selected = None;
    let mut rest = Vec::new();
    while let Some(argument) = arguments.next() {
        if selectable && argument == "--selected" {
            if selected.is_some() {
                return Err(ArgsError::UnexpectedArgument(argument));
            }
            let value = required(&mut arguments, "selected input")?;
            selected = Some(
                value
                    .parse::<core::num::NonZeroU128>()
                    .map(fm_protocol::WireInputId::new)
                    .map_err(|_| ArgsError::InvalidNumber {
                        field: "selected input",
                        value,
                    })?,
            );
        } else {
            rest.push(argument);
        }
    }
    let (key, expected_revision) = command_options(rest.into_iter())?;
    Ok((selected, key, expected_revision))
}

fn command_options(
    mut arguments: impl Iterator<Item = String>,
) -> Result<(Option<String>, Option<u64>), ArgsError> {
    let mut key = None;
    let mut expected_revision = None;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--key" => {
                let value = required(&mut arguments, "idempotency key")?;
                if value.trim().is_empty() {
                    return Err(ArgsError::BlankValue("idempotency key"));
                }
                key = Some(value);
            }
            "--expect" => {
                expected_revision = Some(number(
                    &required(&mut arguments, "expected revision")?,
                    "expected revision",
                )?);
            }
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok((key, expected_revision))
}

fn reject_extra(arguments: &mut impl Iterator<Item = String>) -> Result<(), ArgsError> {
    match arguments.next() {
        Some(argument) => Err(ArgsError::UnexpectedArgument(argument)),
        None => Ok(()),
    }
}

fn required(
    arguments: &mut impl Iterator<Item = String>,
    field: &'static str,
) -> Result<String, ArgsError> {
    arguments.next().ok_or(ArgsError::MissingValue(field))
}

/// The value of secret `option` (a stream key, passphrase, or stream ID).
/// A value that looks like an option (`--...`) means the secret was left
/// out: `--srt-passphrase --srt-latency-ms 200` must not store
/// `--srt-latency-ms` as the passphrase and then misread `200`. The error
/// names only the option, never the value.
fn secret(option: &'static str, value: String) -> Result<String, ArgsError> {
    if value.starts_with("--") {
        Err(ArgsError::SecretLooksLikeOption(option))
    } else {
        Ok(value)
    }
}

fn nonblank(
    arguments: &mut impl Iterator<Item = String>,
    field: &'static str,
) -> Result<String, ArgsError> {
    let value = required(arguments, field)?;
    if value.trim().is_empty() {
        Err(ArgsError::BlankValue(field))
    } else {
        Ok(value)
    }
}

fn required_path(
    arguments: &mut impl Iterator<Item = String>,
    field: &'static str,
) -> Result<PathBuf, ArgsError> {
    required(arguments, field).map(PathBuf::from)
}

fn number<T>(value: &str, field: &'static str) -> Result<T, ArgsError>
where
    T: core::str::FromStr,
{
    value.parse().map_err(|_| ArgsError::InvalidNumber {
        field,
        value: value.to_owned(),
    })
}

fn boolean_choice(value: &str, field: &'static str) -> Result<bool, ArgsError> {
    match value {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(ArgsError::InvalidChoice {
            field,
            value: value.to_owned(),
        }),
    }
}

fn bounded_u8(value: &str, field: &'static str, maximum: u8) -> Result<u8, ArgsError> {
    let parsed = number::<u16>(value, field)?;
    if parsed > u16::from(maximum) {
        return Err(ArgsError::OutOfRange {
            field,
            minimum: 0,
            maximum: u16::from(maximum),
            value: parsed,
        });
    }
    Ok(u8::try_from(parsed).expect("validated bounded u8"))
}

fn socket_address(value: &str) -> Result<SocketAddr, ArgsError> {
    number(value, "address")
}

/// A manual kind: any timed mix (`cube-zoom` or `cube_zoom`), or
/// `stinger-<slot>` followed by its clip length in frames.
fn manual_kind(
    value: &str,
    arguments: &mut impl Iterator<Item = String>,
) -> Result<ManualTransitionKind, ArgsError> {
    let token = value.replace('-', "_");
    let invalid = || ArgsError::InvalidChoice {
        field: "transition kind",
        value: value.to_owned(),
    };
    match fm_protocol::TransitionFavoriteEffect::from_token(&token).ok_or_else(invalid)? {
        fm_protocol::TransitionFavoriteEffect::Cut => Err(invalid()),
        fm_protocol::TransitionFavoriteEffect::Mix(kind) => {
            Ok(ManualTransitionKind::from_mix(kind))
        }
        fm_protocol::TransitionFavoriteEffect::Stinger(slot) => {
            let duration_frames =
                number::<u32>(&required(arguments, "clip frames")?, "clip frames")?;
            if duration_frames == 0 {
                return Err(ArgsError::InvalidChoice {
                    field: "clip frames",
                    value: duration_frames.to_string(),
                });
            }
            Ok(ManualTransitionKind::Stinger {
                slot,
                duration_frames,
            })
        }
    }
}

fn overlay_transition(value: &str) -> Result<OverlayTransition, ArgsError> {
    OverlayTransition::from_token(&value.replace('-', "_")).ok_or_else(|| {
        ArgsError::InvalidChoice {
            field: "overlay transition",
            value: value.to_owned(),
        }
    })
}

/// `<slot:1..=4> <effect> <frames>` of a favourite assignment.
fn favorite_fields(
    arguments: &mut impl Iterator<Item = String>,
) -> Result<(u8, TransitionFavoriteEffect, u32), ArgsError> {
    let slot: u8 = number(&required(arguments, "favourite slot")?, "favourite slot")?;
    if !(1..=4).contains(&slot) {
        return Err(ArgsError::InvalidChoice {
            field: "favourite slot",
            value: slot.to_string(),
        });
    }
    let effect = transition_favorite_effect(&required(arguments, "favourite transition")?)?;
    let frames = number(&required(arguments, "frames")?, "frames")?;
    Ok((slot, effect, frames))
}

fn parse_transition_render_frame(
    mut arguments: impl Iterator<Item = String>,
) -> Result<Command, ArgsError> {
    let path = required_path(&mut arguments, "project path")?;
    let kind = transition_kind(&required(&mut arguments, "transition kind")?)?;
    let numerator = number(&required(&mut arguments, "numerator")?, "numerator")?;
    let denominator = number(&required(&mut arguments, "denominator")?, "denominator")?;
    let output = required_path(&mut arguments, "output path")?;
    let mut width = 640;
    let mut height = 360;
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--width" => width = number(&required(&mut arguments, "width")?, "width")?,
            "--height" => height = number(&required(&mut arguments, "height")?, "height")?,
            _ => return Err(ArgsError::UnknownOption(option)),
        }
    }
    Ok(Command::TransitionRenderFrame {
        path,
        kind,
        numerator,
        denominator,
        output,
        width,
        height,
    })
}

fn overlay_position(value: &str) -> Result<OverlayPosition, ArgsError> {
    match value {
        "full-frame" => Ok(OverlayPosition::FullFrame),
        "top-left" => Ok(OverlayPosition::TopLeft),
        "top-right" => Ok(OverlayPosition::TopRight),
        "bottom-left" => Ok(OverlayPosition::BottomLeft),
        "bottom-right" => Ok(OverlayPosition::BottomRight),
        _ => Err(ArgsError::InvalidChoice {
            field: "overlay position",
            value: value.to_owned(),
        }),
    }
}

fn overlay_border(value: &str) -> Result<OverlayBorder, ArgsError> {
    match value {
        "none" => Ok(OverlayBorder::None),
        "thin-white" => Ok(OverlayBorder::ThinWhite),
        "thick-white" => Ok(OverlayBorder::ThickWhite),
        _ => Err(ArgsError::InvalidChoice {
            field: "overlay border",
            value: value.to_owned(),
        }),
    }
}

fn fade_to_black_target(value: &str) -> Result<FadeToBlackTarget, ArgsError> {
    match value {
        "live" => Ok(FadeToBlackTarget::Live),
        "black" => Ok(FadeToBlackTarget::Black),
        _ => Err(ArgsError::InvalidChoice {
            field: "FTB target",
            value: value.to_owned(),
        }),
    }
}

fn basis_points(value: &str) -> Result<u16, ArgsError> {
    let position = number(value, "basis points")?;
    if position <= 10_000 {
        Ok(position)
    } else {
        Err(ArgsError::OutOfRange {
            field: "basis points",
            minimum: 0,
            maximum: 10_000,
            value: position,
        })
    }
}

fn stinger_slot(value: &str) -> Result<u8, ArgsError> {
    let slot = number::<u16>(value, "Stinger slot")?;
    if !(1..=8).contains(&slot) {
        return Err(ArgsError::OutOfRange {
            field: "Stinger slot",
            minimum: 1,
            maximum: 8,
            value: slot,
        });
    }
    Ok(u8::try_from(slot).expect("validated Stinger slot fits u8"))
}

fn overlay_channel(value: &str) -> Result<u8, ArgsError> {
    let channel = number::<u16>(value, "overlay channel")?;
    if !(1..=8).contains(&channel) {
        return Err(ArgsError::OutOfRange {
            field: "overlay channel",
            minimum: 1,
            maximum: 8,
            value: channel,
        });
    }
    Ok(u8::try_from(channel).expect("validated overlay channel fits u8"))
}

fn boolean(value: &str, field: &'static str) -> Result<bool, ArgsError> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ArgsError::InvalidChoice {
            field,
            value: value.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn cue_specs_honour_backslash_escapes() {
        assert_eq!(
            split_escaped(r"a\;b;c\\;d", ';', usize::MAX),
            Some(vec![r"a\;b", r"c\\", "d"])
        );
        assert_eq!(
            split_escaped("a:b:c:d:e", ':', 4),
            Some(vec!["a", "b", "c", "d:e"])
        );
        assert_eq!(split_escaped(r"dangling\", ',', usize::MAX), None);
        assert_eq!(unescape(r"a\,b\;c\:d\\e"), r"a,b;c:d\e");

        let cues = cue_specs(
            r"1:Open\: cold:0:title_text.2.1.a\, b\; c:d \\ e,delay.40,run_script.3;2:B:1:",
        )
        .unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].name, "Open: cold");
        assert_eq!(
            cues[0].actions,
            vec![
                fm_protocol::WireCueAction::TitleText {
                    input: fm_types::InputTarget::Fixed(fm_protocol::WireInputId::new(
                        core::num::NonZeroU128::new(2).unwrap()
                    )),
                    field: fm_protocol::WireTitleFieldId::new(
                        core::num::NonZeroU128::new(1).unwrap()
                    ),
                    text: r"a, b; c:d \ e".into(),
                },
                fm_protocol::WireCueAction::Delay { millis: 40 },
                fm_protocol::WireCueAction::RunScript { script: 3 },
            ]
        );
        assert!(cue_specs("1:A:0:run_script.x").is_err());
        assert!(cues[1].auto_follow);
        // Escapes belong in names and title text only.
        assert!(cue_specs(r"1:A:0:wait.\5").is_err());
        assert!(cue_specs(r"1:A:0:wait.5\").is_err());
    }

    #[test]
    fn parses_revisioned_fade() {
        assert_eq!(
            parse(strings(&[
                "fade",
                "show.freemix",
                "30",
                "--key",
                "take-1",
                "--expect",
                "4"
            ])),
            Ok(Command::Fade {
                path: "show.freemix".into(),
                frames: 30,
                key: Some("take-1".into()),
                expected_revision: Some(4),
            })
        );
    }

    #[test]
    fn parses_local_and_remote_wipe() {
        assert_eq!(
            parse(strings(&[
                "wipe",
                "show.freemix",
                "45",
                "--key",
                "wipe-1",
                "--expect",
                "4",
            ])),
            Ok(Command::Wipe {
                path: "show.freemix".into(),
                frames: 45,
                key: Some("wipe-1".into()),
                expected_revision: Some(4),
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-wipe",
                "127.0.0.1:9123",
                "12",
                "--key",
                "remote-wipe",
            ])),
            Ok(Command::RemoteWipe {
                address: "127.0.0.1:9123".parse().unwrap(),
                frames: 12,
                key: Some("remote-wipe".into()),
                expected_revision: None,
            })
        );
    }

    #[test]
    fn parses_local_and_remote_alpha_fade() {
        assert_eq!(
            parse(strings(&[
                "alpha-fade",
                "show.freemix",
                "45",
                "--key",
                "alpha-fade-1",
                "--expect",
                "4",
            ])),
            Ok(Command::AlphaFade {
                path: "show.freemix".into(),
                frames: 45,
                key: Some("alpha-fade-1".into()),
                expected_revision: Some(4),
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-alpha-fade",
                "127.0.0.1:9123",
                "12",
                "--key",
                "remote-alpha-fade",
            ])),
            Ok(Command::RemoteAlphaFade {
                address: "127.0.0.1:9123".parse().unwrap(),
                frames: 12,
                key: Some("remote-alpha-fade".into()),
                expected_revision: None,
            })
        );
    }

    #[test]
    fn parses_local_and_remote_slide() {
        assert_eq!(
            parse(strings(&[
                "slide",
                "show.freemix",
                "45",
                "--key",
                "slide-1",
                "--expect",
                "4",
            ])),
            Ok(Command::Slide {
                path: "show.freemix".into(),
                frames: 45,
                key: Some("slide-1".into()),
                expected_revision: Some(4),
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-slide",
                "127.0.0.1:9123",
                "12",
                "--key",
                "remote-slide",
            ])),
            Ok(Command::RemoteSlide {
                address: "127.0.0.1:9123".parse().unwrap(),
                frames: 12,
                key: Some("remote-slide".into()),
                expected_revision: None,
            })
        );
    }

    #[test]
    fn parses_local_and_remote_zoom() {
        assert_eq!(
            parse(strings(&[
                "zoom",
                "show.freemix",
                "45",
                "--key",
                "zoom-1",
                "--expect",
                "4",
            ])),
            Ok(Command::Zoom {
                path: "show.freemix".into(),
                frames: 45,
                key: Some("zoom-1".into()),
                expected_revision: Some(4),
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-zoom",
                "127.0.0.1:9123",
                "12",
                "--key",
                "remote-zoom",
            ])),
            Ok(Command::RemoteZoom {
                address: "127.0.0.1:9123".parse().unwrap(),
                frames: 12,
                key: Some("remote-zoom".into()),
                expected_revision: None,
            })
        );
    }

    #[test]
    fn parses_local_and_remote_stinger_and_rejects_invalid_slots() {
        assert_eq!(
            parse(strings(&[
                "stinger",
                "show.freemix",
                "8",
                "45",
                "--key",
                "stinger-8",
                "--expect",
                "4",
            ])),
            Ok(Command::Stinger {
                path: "show.freemix".into(),
                slot: 8,
                frames: 45,
                key: Some("stinger-8".into()),
                expected_revision: Some(4),
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-stinger",
                "127.0.0.1:9123",
                "1",
                "12",
                "--key",
                "remote-stinger",
            ])),
            Ok(Command::RemoteStinger {
                address: "127.0.0.1:9123".parse().unwrap(),
                slot: 1,
                frames: 12,
                key: Some("remote-stinger".into()),
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&["stinger", "show.freemix", "0", "12"])),
            Err(ArgsError::OutOfRange {
                field: "Stinger slot",
                minimum: 1,
                maximum: 8,
                value: 0,
            })
        );
        assert_eq!(
            parse(strings(&["remote-stinger", "127.0.0.1:9123", "9", "12"])),
            Err(ArgsError::OutOfRange {
                field: "Stinger slot",
                minimum: 1,
                maximum: 8,
                value: 9,
            })
        );
    }

    #[test]
    fn parses_local_and_remote_overlay_appearance() {
        assert_eq!(
            parse(strings(&[
                "overlay-appearance",
                "show.freemix",
                "4",
                "bottom-right",
                "thick-white",
                "--expect",
                "7",
            ])),
            Ok(Command::OverlayAppearance {
                path: "show.freemix".into(),
                channel: 4,
                position: OverlayPosition::BottomRight,
                border: OverlayBorder::ThickWhite,
                key: None,
                expected_revision: Some(7),
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-overlay-appearance",
                "127.0.0.1:9123",
                "1",
                "top-left",
                "thin-white",
            ])),
            Ok(Command::RemoteOverlayAppearance {
                address: "127.0.0.1:9123".parse().unwrap(),
                channel: 1,
                position: OverlayPosition::TopLeft,
                border: OverlayBorder::ThinWhite,
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&["overlay-queue", "show.freemix", "4", "2",])),
            Ok(Command::OverlayQueue {
                path: "show.freemix".into(),
                channel: 4,
                source: 2,
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&["remote-overlay-next", "127.0.0.1:9123", "4"])),
            Ok(Command::RemoteOverlayNext {
                address: "127.0.0.1:9123".parse().unwrap(),
                channel: 4,
                key: None,
                expected_revision: None,
            })
        );
    }

    #[test]
    fn parses_complete_stinger_configuration_and_removal() {
        assert_eq!(
            parse(strings(&[
                "stinger-configure",
                "show.freemix",
                "8",
                "340282366920938463463374607431768211455",
                "false",
                "45",
                "mix-with-program",
                "keep-program",
            ])),
            Ok(Command::StingerConfigure {
                path: "show.freemix".into(),
                slot: 8,
                media_input: u128::MAX,
                preload: false,
                cut_point_frames: 45,
                audio_policy: StingerAudioPolicy::MixWithProgram,
                fallback: StingerFallback::KeepProgram,
            })
        );
        assert_eq!(
            parse(strings(&["stinger-remove", "show.freemix", "1"])),
            Ok(Command::StingerRemove {
                path: "show.freemix".into(),
                slot: 1,
            })
        );
        assert!(matches!(
            parse(strings(&[
                "stinger-configure",
                "show.freemix",
                "1",
                "2",
                "yes",
                "12",
                "muted",
                "cut",
            ])),
            Err(ArgsError::InvalidChoice {
                field: "preload",
                ..
            })
        ));
        assert!(matches!(
            parse(strings(&[
                "stinger-configure",
                "show.freemix",
                "1",
                "2",
                "true",
                "12",
                "program-only",
                "cut",
            ])),
            Err(ArgsError::InvalidChoice {
                field: "Stinger audio policy",
                ..
            })
        ));
    }

    #[test]
    fn parses_local_and_remote_t_bar_commands_with_exact_endpoints() {
        assert_eq!(
            parse(strings(&[
                "tbar-start",
                "show.freemix",
                "wipe",
                "--key",
                "manual-start",
                "--expect",
                "4",
            ])),
            Ok(Command::TBar {
                path: "show.freemix".into(),
                action: TBarAction::Start(ManualTransitionKind::Wipe),
                key: Some("manual-start".into()),
                expected_revision: Some(4),
            })
        );
        assert_eq!(
            parse(strings(&["tbar-position", "show.freemix", "0"])),
            Ok(Command::TBar {
                path: "show.freemix".into(),
                action: TBarAction::SetPosition(0),
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-tbar-position",
                "127.0.0.1:9123",
                "10000",
                "--expect",
                "9",
            ])),
            Ok(Command::RemoteTBar {
                address: "127.0.0.1:9123".parse().unwrap(),
                action: TBarAction::SetPosition(10_000),
                key: None,
                expected_revision: Some(9),
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-tbar-start",
                "127.0.0.1:9123",
                "alpha-fade",
                "--key",
                "manual-alpha-start",
            ])),
            Ok(Command::RemoteTBar {
                address: "127.0.0.1:9123".parse().unwrap(),
                action: TBarAction::Start(ManualTransitionKind::AlphaFade),
                key: Some("manual-alpha-start".into()),
                expected_revision: None,
            })
        );
        assert!(matches!(
            parse(strings(&["tbar-commit", "show.freemix"])),
            Ok(Command::TBar {
                action: TBarAction::Commit,
                ..
            })
        ));
        assert!(matches!(
            parse(strings(&["remote-tbar-cancel", "127.0.0.1:9123"])),
            Ok(Command::RemoteTBar {
                action: TBarAction::Cancel,
                ..
            })
        ));
    }

    #[test]
    fn parses_every_manual_kind_and_fade_to_black_timing() {
        for (token, kind) in [
            ("cube-zoom", ManualTransitionKind::CubeZoom),
            ("vertical_slide", ManualTransitionKind::VerticalSlide),
            ("merge", ManualTransitionKind::Merge),
        ] {
            assert_eq!(
                parse(strings(&["tbar-start", "show.freemix", token])),
                Ok(Command::TBar {
                    path: "show.freemix".into(),
                    action: TBarAction::Start(kind),
                    key: None,
                    expected_revision: None,
                })
            );
        }
        assert_eq!(
            parse(strings(&[
                "remote-tbar-start",
                "127.0.0.1:9123",
                "stinger-2",
                "24",
                "--expect",
                "3",
            ])),
            Ok(Command::RemoteTBar {
                address: "127.0.0.1:9123".parse().unwrap(),
                action: TBarAction::Start(ManualTransitionKind::Stinger {
                    slot: fm_protocol::WireStingerSlotId::new(2).unwrap(),
                    duration_frames: 24,
                }),
                key: None,
                expected_revision: Some(3),
            })
        );
        assert_eq!(
            parse(strings(&["ftb-output-duration", "show.freemix", "8", "45"])),
            Ok(Command::FadeToBlackOutputDuration {
                path: "show.freemix".into(),
                output: 8,
                duration_frames: Some(45),
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-ftb-output-duration",
                "127.0.0.1:9123",
                "8",
                "none",
            ])),
            Ok(Command::RemoteFadeToBlackOutputDuration {
                address: "127.0.0.1:9123".parse().unwrap(),
                output: 8,
                duration_frames: None,
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&["ftb-curve", "show.freemix", "ease"])),
            Ok(Command::FadeToBlackCurve {
                path: "show.freemix".into(),
                curve: FadeToBlackCurve::Ease,
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&["remote-ftb-curve", "127.0.0.1:9123", "linear"])),
            Ok(Command::RemoteFadeToBlackCurve {
                address: "127.0.0.1:9123".parse().unwrap(),
                curve: FadeToBlackCurve::Linear,
                key: None,
                expected_revision: None,
            })
        );
        for bad in [
            &["tbar-start", "show.freemix", "cut"][..],
            &["tbar-start", "show.freemix", "stinger-2"],
            &["tbar-start", "show.freemix", "stinger-2", "0"],
            &["tbar-start", "show.freemix", "stinger-9", "24"],
            &["ftb-output-duration", "show.freemix", "8", "0"],
            &["ftb-output-duration", "show.freemix", "8", "3601"],
            &["ftb-output-duration", "show.freemix", "8"],
            &["ftb-curve", "show.freemix", "Ease"],
        ] {
            assert!(parse(strings(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn parses_local_and_remote_fade_to_black_targets() {
        assert_eq!(
            parse(strings(&[
                "ftb",
                "show.freemix",
                "black",
                "45",
                "--key",
                "blackout",
                "--expect",
                "7",
            ])),
            Ok(Command::FadeToBlack {
                path: "show.freemix".into(),
                target: FadeToBlackTarget::Black,
                frames: 45,
                key: Some("blackout".into()),
                expected_revision: Some(7),
            })
        );
        assert_eq!(
            parse(strings(&["remote-ftb", "127.0.0.1:9123", "live", "30",])),
            Ok(Command::RemoteFadeToBlack {
                address: "127.0.0.1:9123".parse().unwrap(),
                target: FadeToBlackTarget::Live,
                frames: 30,
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&["ftb", "show.freemix", "toggle", "10"])),
            Err(ArgsError::InvalidChoice {
                field: "FTB target",
                value: "toggle".into(),
            })
        );
    }

    #[test]
    fn rejects_non_integer_unknown_and_out_of_range_t_bar_values() {
        assert!(matches!(
            parse(strings(&["tbar-position", "show.freemix", "62.50"])),
            Err(ArgsError::InvalidNumber {
                field: "basis points",
                ..
            })
        ));
        assert_eq!(
            parse(strings(&["tbar-position", "show.freemix", "10001"])),
            Err(ArgsError::OutOfRange {
                field: "basis points",
                minimum: 0,
                maximum: 10_000,
                value: 10_001,
            })
        );
        assert!(matches!(
            parse(strings(&["tbar-start", "show.freemix", "unknown"])),
            Err(ArgsError::InvalidChoice {
                field: "transition kind",
                ..
            })
        ));
    }

    #[test]
    fn parses_project_bundle_commands_and_their_options() {
        assert_eq!(
            parse(strings(&[
                "project-relink",
                "show.freemix",
                "--search",
                "a",
                "--apply",
                "--search",
                "b",
            ])),
            Ok(Command::ProjectRelink {
                path: "show.freemix".into(),
                search: vec!["a".into(), "b".into()],
                apply: true,
            })
        );
        assert_eq!(
            parse(strings(&["project-autosave", "show.freemix"])),
            Ok(Command::ProjectAutosave {
                path: "show.freemix".into(),
                keep: fm_persistence::DEFAULT_AUTOSAVE_GENERATIONS,
            })
        );
        assert_eq!(
            parse(strings(&[
                "project-autosave",
                "show.freemix",
                "--keep",
                "x"
            ])),
            Err(ArgsError::InvalidNumber {
                field: "keep",
                value: "x".into(),
            })
        );
        assert_eq!(
            parse(strings(&["project-recover", "show.freemix", "--restore"])),
            Ok(Command::ProjectRecover {
                path: "show.freemix".into(),
                restore: true,
            })
        );
        assert_eq!(
            parse(strings(&[
                "project-from-template",
                "t.freemix",
                "d.freemix"
            ])),
            Err(ArgsError::MissingValue("--name <project name>"))
        );
        assert_eq!(
            parse(strings(&[
                "project-import-media",
                "show.freemix",
                "clip.ppm",
                "--name",
                "Clip",
            ])),
            Err(ArgsError::RequiresOption {
                option: "--name",
                requires: "--input",
            })
        );
        assert_eq!(
            parse(strings(&["project-append", "a.freemix", "b.freemix", "c"])),
            Err(ArgsError::UnexpectedArgument("c".into()))
        );
        assert_eq!(
            parse(strings(&[
                "recent-projects-add",
                "recent.json",
                "show.freemix"
            ])),
            Ok(Command::RecentProjectsAdd {
                file: "recent.json".into(),
                project: "show.freemix".into(),
                capacity: fm_persistence::DEFAULT_RECENT_PROJECTS,
            })
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn parses_grade_commands() {
        assert_eq!(
            parse(strings(&[
                "grade-set",
                "show.freemix",
                "7",
                "--preset",
                "warm",
                "--exposure",
                "-500",
                "--saturation",
                "1200",
                "--lut",
                "2",
                "--lut-strength",
                "400",
                "--save-as",
                "Look",
            ])),
            Ok(Command::GradeSet {
                path: "show.freemix".into(),
                target: GradeTargetArg::Input(7),
                library: None,
                spec: GradeSpec {
                    preset: Some("warm".into()),
                    exposure_millistops: Some(-500),
                    saturation_permille: Some(1200),
                    lut: Some(2),
                    lut_strength_permille: Some(400),
                    ..GradeSpec::default()
                },
                save_as: Some("Look".into()),
            })
        );
        assert_eq!(
            parse(strings(&["grade-set", "s", "program", "--grade", "3"])),
            Ok(Command::GradeSet {
                path: "s".into(),
                target: GradeTargetArg::Program,
                library: Some(3),
                spec: GradeSpec::default(),
                save_as: None,
            })
        );
        assert_eq!(
            parse(strings(&["grade-set", "s", "1", "--exposure", "40000"])),
            Err(ArgsError::InvalidNumber {
                field: "exposure",
                value: "40000".into(),
            })
        );
        assert_eq!(
            parse(strings(&[
                "grade-set",
                "s",
                "layer",
                "4",
                "2",
                "--preset",
                "warm"
            ])),
            Ok(Command::GradeSet {
                path: PathBuf::from("s"),
                target: GradeTargetArg::Layer { scene: 4, layer: 2 },
                library: None,
                spec: GradeSpec {
                    preset: Some("warm".into()),
                    ..GradeSpec::default()
                },
                save_as: None,
            })
        );
        assert!(matches!(
            parse(strings(&["grade-clear", "s", "layer", "4"])),
            Err(ArgsError::MissingValue("layer id"))
        ));
        assert_eq!(
            parse(strings(&["grade-clear", "s", "layer", "4", "2"])),
            Ok(Command::GradeClear {
                path: PathBuf::from("s"),
                target: GradeTargetArg::Layer { scene: 4, layer: 2 },
            })
        );
        assert_eq!(
            parse(strings(&["grade-clear", "s", "program"])),
            Ok(Command::GradeClear {
                path: "s".into(),
                target: GradeTargetArg::Program,
            })
        );
        assert_eq!(
            parse(strings(&["grade-import-lut", "s", "a.cube", "--name", "A"])),
            Ok(Command::GradeImportLut {
                path: "s".into(),
                file: "a.cube".into(),
                name: Some("A".into()),
            })
        );
        assert_eq!(
            parse(strings(&["grade-list", "s", "x"])),
            Err(ArgsError::UnexpectedArgument("x".into()))
        );
    }

    #[test]
    fn parses_preset_sharing_commands() {
        assert_eq!(
            parse(strings(&["grade-export", "s", "2", "w.fmgrade"])),
            Ok(Command::GradeExport {
                path: "s".into(),
                grade: 2,
                file: "w.fmgrade".into(),
                replace_lut: false,
            })
        );
        assert_eq!(
            parse(strings(&[
                "grade-export",
                "s",
                "2",
                "w.fmgrade",
                "--replace-lut"
            ])),
            Ok(Command::GradeExport {
                path: "s".into(),
                grade: 2,
                file: "w.fmgrade".into(),
                replace_lut: true,
            })
        );
        assert!(
            parse(strings(&[
                "grade-export",
                "s",
                "2",
                "w.fmgrade",
                "--replace-lut",
                "--replace-lut"
            ]))
            .is_err()
        );
        assert_eq!(
            parse(strings(&["grade-import", "s", "w.fmgrade", "--name", "W"])),
            Ok(Command::GradeImport {
                path: "s".into(),
                file: "w.fmgrade".into(),
                name: Some("W".into()),
            })
        );
        assert_eq!(
            parse(strings(&["preset-export", "s", "3", "p.fmpreset"])),
            Ok(Command::PresetExport {
                path: "s".into(),
                preset: 3,
                file: "p.fmpreset".into(),
            })
        );
        assert_eq!(
            parse(strings(&["preset-import", "s", "p.fmpreset"])),
            Ok(Command::PresetImport {
                path: "s".into(),
                file: "p.fmpreset".into(),
                name: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "preset-save",
                "s",
                "asset://titles/a.fmtitle",
                "5",
                "--name",
                "Rise",
                "--phase",
                "out",
            ])),
            Ok(Command::PresetSave {
                path: "s".into(),
                template_uri: "asset://titles/a.fmtitle".into(),
                element: 5,
                phase: TitlePhase::Out,
                name: "Rise".into(),
            })
        );
        assert_eq!(
            parse(strings(&["preset-export", "s", "3", "p.fmpreset", "x"])),
            Err(ArgsError::UnexpectedArgument("x".into()))
        );
        assert_eq!(
            parse(strings(&["preset-import", "s", "p.fmpreset", "--wat"])),
            Err(ArgsError::UnknownOption("--wat".into()))
        );
        assert_eq!(
            parse(strings(&["preset-save", "s", "u", "5", "--phase", "in"])),
            Err(ArgsError::MissingValue("--name"))
        );
        assert!(parse(strings(&["grade-export", "s", "two", "w.fmgrade"])).is_err());
    }

    #[test]
    fn parses_local_and_remote_shortcut_set_commands() {
        assert_eq!(
            parse(strings(&["shortcuts", "s"])),
            Ok(Command::Shortcuts {
                path: "s".into(),
                command: ShortcutSetCommand::List,
            })
        );
        assert_eq!(
            parse(strings(&[
                "shortcut-export",
                "s",
                "a.fmshortcuts",
                "--name",
                "A"
            ])),
            Ok(Command::Shortcuts {
                path: "s".into(),
                command: ShortcutSetCommand::Export {
                    file: "a.fmshortcuts".into(),
                    name: Some("A".into()),
                },
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-shortcut-import",
                "127.0.0.1:9123",
                "a.fmshortcuts"
            ])),
            Ok(Command::RemoteShortcuts {
                address: "127.0.0.1:9123".parse().unwrap(),
                command: ShortcutSetCommand::Import {
                    file: "a.fmshortcuts".into(),
                },
            })
        );
        assert_eq!(
            parse(strings(&["remote-shortcuts", "127.0.0.1:9123"])),
            Ok(Command::RemoteShortcuts {
                address: "127.0.0.1:9123".parse().unwrap(),
                command: ShortcutSetCommand::List,
            })
        );
        assert!(parse(strings(&["shortcut-import", "s"])).is_err());
        assert!(parse(strings(&["shortcut-import", "s", "a", "b"])).is_err());
        assert!(parse(strings(&["shortcut-export", "s", "a", "--wat"])).is_err());
        assert!(parse(strings(&["shortcuts", "s", "extra"])).is_err());
    }

    #[test]
    fn rejects_unknown_option() {
        assert_eq!(
            parse(strings(&["cut", "show.freemix", "--wat"])),
            Err(ArgsError::UnknownOption("--wat".into()))
        );
    }

    #[test]
    fn status_rejects_extra_positional_argument() {
        assert_eq!(
            parse(strings(&["status", "show.freemix", "extra"])),
            Err(ArgsError::UnexpectedArgument("extra".into()))
        );
    }

    #[test]
    fn audio_strip_parses_exact_project_input_and_controls() {
        assert_eq!(
            parse(strings(&[
                "audio-strip",
                "show.freemix",
                "7",
                "-6000",
                "2500",
                "on",
                "on",
                "off",
                "48000",
            ])),
            Ok(Command::AudioStrip {
                path: PathBuf::from("show.freemix"),
                input: 7,
                gain_millidb: -6_000,
                balance_basis_points: 2_500,
                pan_basis_points: 0,
                muted: true,
                soloed: true,
                follow_video: false,
                delay_samples: 48_000,
            })
        );
    }

    #[test]
    fn monitor_output_commands_parse() {
        assert_eq!(
            parse(strings(&[
                "audio-monitor-device",
                "show.freemix",
                "hw:CARD=Headset,DEV=0"
            ])),
            Ok(Command::AudioMixer {
                path: PathBuf::from("show.freemix"),
                edit: AudioMixerArgs::MonitorDevice {
                    device: Some("hw:CARD=Headset,DEV=0".into()),
                },
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&["audio-monitor-device", "show.freemix", "none"])),
            Ok(Command::AudioMixer {
                path: PathBuf::from("show.freemix"),
                edit: AudioMixerArgs::MonitorDevice { device: None },
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-audio-monitor-level",
                "127.0.0.1:9123",
                "-6000",
                "on",
            ])),
            Ok(Command::RemoteAudioMixer {
                address: "127.0.0.1:9123".parse().unwrap(),
                edit: AudioMixerArgs::MonitorLevel {
                    gain_millidb: -6_000,
                    muted: true,
                },
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&["audio-strip-dsp", "show.freemix", "3", "voice"])),
            Ok(Command::AudioMixer {
                path: PathBuf::from("show.freemix"),
                edit: AudioMixerArgs::StripDsp {
                    input: Some(3),
                    chain: fm_types::strip_dsp::StripDspPreset::Voice.chain(),
                },
                key: None,
                expected_revision: None,
            })
        );
        let code = fm_types::strip_dsp::StripDspPreset::Master.chain().code();
        assert_eq!(
            parse(strings(&[
                "remote-audio-strip-dsp",
                "127.0.0.1:9123",
                "master",
                &code,
            ])),
            Ok(Command::RemoteAudioMixer {
                address: "127.0.0.1:9123".parse().unwrap(),
                edit: AudioMixerArgs::StripDsp {
                    input: None,
                    chain: fm_types::strip_dsp::StripDspPreset::Master.chain(),
                },
                key: None,
                expected_revision: None,
            })
        );
        for bad in [
            &["audio-strip-dsp", "show.freemix", "3"][..],
            &["audio-strip-dsp", "show.freemix", "Master", "voice"],
            &["audio-strip-dsp", "show.freemix", "3", "loud"],
            &["audio-strip-dsp", "show.freemix", "3", &code[1..]],
            &["audio-monitor-device", "show.freemix"],
            &["audio-monitor-level", "show.freemix", "-6000"],
            &["audio-monitor-level", "show.freemix", "loud", "on"],
            &["audio-monitor-level", "show.freemix", "0", "yes"],
        ] {
            assert!(parse(strings(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn loudness_reset_commands_parse() {
        assert_eq!(
            parse(strings(&[
                "remote-audio-loudness-reset",
                "127.0.0.1:9123",
                "master",
            ])),
            Ok(Command::RemoteAudioMixer {
                address: "127.0.0.1:9123".parse().unwrap(),
                edit: AudioMixerArgs::LoudnessReset { input: None },
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&["audio-loudness-reset", "show.freemix", "3"])),
            Ok(Command::AudioMixer {
                path: PathBuf::from("show.freemix"),
                edit: AudioMixerArgs::LoudnessReset { input: Some(3) },
                key: None,
                expected_revision: None,
            })
        );
        for bad in [
            &["audio-loudness-reset", "show.freemix"][..],
            &["audio-loudness-reset", "show.freemix", "Master"],
            &["audio-loudness-reset", "show.freemix", "3", "4"],
        ] {
            assert!(parse(strings(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn mixer_group_solo_parent_and_solo_mode_commands_parse() {
        assert_eq!(
            parse(strings(&["audio-group-solo", "show.freemix", "3", "on"])),
            Ok(Command::AudioMixer {
                path: PathBuf::from("show.freemix"),
                edit: AudioMixerArgs::GroupSolo {
                    group: 3,
                    soloed: true,
                },
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "audio-group-parent",
                "show.freemix",
                "1",
                "none"
            ])),
            Ok(Command::AudioMixer {
                path: PathBuf::from("show.freemix"),
                edit: AudioMixerArgs::GroupParent {
                    group: 1,
                    parent: None,
                },
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-audio-solo-mode",
                "127.0.0.1:9123",
                "afl",
                "--expect",
                "4",
            ])),
            Ok(Command::RemoteAudioMixer {
                address: "127.0.0.1:9123".parse().unwrap(),
                edit: AudioMixerArgs::SoloMode {
                    mode: fm_model::AudioSoloMode::Afl,
                },
                key: None,
                expected_revision: Some(4),
            })
        );
        for bad in [
            &["audio-group-solo", "show.freemix", "3", "yes"][..],
            &["audio-group-parent", "show.freemix", "1", "5"],
            &["audio-group-parent", "show.freemix", "0", "none"],
            &["audio-solo-mode", "show.freemix", "PFL"],
            &["remote-audio-solo-mode", "127.0.0.1:9123"],
        ] {
            assert!(parse(strings(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn mixer_label_group_and_ftb_output_commands_parse() {
        assert_eq!(
            parse(strings(&["audio-label", "show.freemix", "7", "Host mic"])),
            Ok(Command::AudioMixer {
                path: PathBuf::from("show.freemix"),
                edit: AudioMixerArgs::Label {
                    input: 7,
                    label: "Host mic".into(),
                },
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&["audio-group", "show.freemix", "7", "none"])),
            Ok(Command::AudioMixer {
                path: PathBuf::from("show.freemix"),
                edit: AudioMixerArgs::Group {
                    input: 7,
                    group: None,
                },
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-audio-group-level",
                "127.0.0.1:9123",
                "2",
                "-6000",
                "on",
                "--key",
                "vca",
            ])),
            Ok(Command::RemoteAudioMixer {
                address: "127.0.0.1:9123".parse().unwrap(),
                edit: AudioMixerArgs::GroupLevel {
                    group: 2,
                    gain_millidb: -6_000,
                    muted: true,
                },
                key: Some("vca".into()),
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-ftb-output",
                "127.0.0.1:9123",
                "8",
                "off"
            ])),
            Ok(Command::RemoteFadeToBlackOutput {
                address: "127.0.0.1:9123".parse().unwrap(),
                output: 8,
                affected: false,
                key: None,
                expected_revision: None,
            })
        );
        for bad in [
            &["audio-group", "show.freemix", "7", "5"][..],
            &["audio-group", "show.freemix", "7", "01"],
            &["audio-group-name", "show.freemix", "1", " "],
            &["ftb-output", "show.freemix", "8", "maybe"],
            &[
                "audio-strip",
                "show.freemix",
                "7",
                "0",
                "0",
                "off",
                "off",
                "on",
                "0",
                "--pan",
            ],
        ] {
            assert!(parse(strings(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn remote_audio_strip_parses_command_options() {
        assert_eq!(
            parse(strings(&[
                "remote-audio-strip",
                "127.0.0.1:9123",
                "7",
                "-6000",
                "-2500",
                "off",
                "off",
                "on",
                "2400",
                "--pan",
                "-7500",
                "--key",
                "delay",
                "--expect",
                "3",
            ])),
            Ok(Command::RemoteAudioStrip {
                address: "127.0.0.1:9123".parse().unwrap(),
                input: 7,
                gain_millidb: -6_000,
                balance_basis_points: -2_500,
                pan_basis_points: -7_500,
                muted: false,
                soloed: false,
                follow_video: true,
                delay_samples: 2_400,
                key: Some("delay".into()),
                expected_revision: Some(3),
            })
        );
    }

    #[test]
    fn demo_rejects_more_than_one_output_path() {
        assert_eq!(
            parse(strings(&[
                "demo",
                "show.freemix",
                "output.ppm",
                "extra.ppm"
            ])),
            Err(ArgsError::UnexpectedArgument("extra.ppm".into()))
        );
    }

    #[test]
    fn rejects_blank_explicit_idempotency_key() {
        assert_eq!(
            parse(strings(&["cut", "show.freemix", "--key", "  \t"])),
            Err(ArgsError::BlankValue("idempotency key"))
        );
    }

    #[test]
    fn parses_local_and_remote_record_desired_commands() {
        assert_eq!(
            parse(strings(&["record-start", "show.freemix"])),
            Ok(Command::RecordStart {
                path: "show.freemix".into(),
                recorder: 1,
            })
        );
        assert_eq!(
            parse(strings(&[
                "record-start",
                "show.freemix",
                "--recorder",
                "2"
            ])),
            Ok(Command::RecordStart {
                path: "show.freemix".into(),
                recorder: 2,
            })
        );
        assert_eq!(
            parse(strings(&["record-stop", "show.freemix", "--recorder", "2"])),
            Ok(Command::RecordStop {
                path: "show.freemix".into(),
                recorder: 2,
            })
        );
        // Any value that parses to slot 1 or 2 is accepted, like every other
        // numeric CLI value.
        assert_eq!(
            parse(strings(&[
                "record-start",
                "show.freemix",
                "--recorder",
                "02"
            ])),
            Ok(Command::RecordStart {
                path: "show.freemix".into(),
                recorder: 2,
            })
        );
        assert_eq!(
            parse(strings(&["record-stop", "show.freemix", "extra"])),
            Err(ArgsError::UnexpectedArgument("extra".into()))
        );
        assert_eq!(
            parse(strings(&[
                "remote-record-stop",
                "127.0.0.1:9123",
                "--recorder",
                "2",
                "--key",
                "remote-record-stop",
                "--expect",
                "4",
            ])),
            Ok(Command::RemoteRecordStop {
                address: "127.0.0.1:9123".parse().unwrap(),
                recorder: 2,
                key: Some("remote-record-stop".into()),
                expected_revision: Some(4),
            })
        );
        assert_eq!(
            parse(strings(&["remote-record-start", "127.0.0.1:9123"])),
            Ok(Command::RemoteRecordStart {
                address: "127.0.0.1:9123".parse().unwrap(),
                recorder: 1,
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-record-start",
                "127.0.0.1:9123",
                "--recorder",
                "2",
            ])),
            Ok(Command::RemoteRecordStart {
                address: "127.0.0.1:9123".parse().unwrap(),
                recorder: 2,
                key: None,
                expected_revision: None,
            })
        );
    }

    #[test]
    fn rejects_recorder_values_outside_the_two_program_slots() {
        for value in ["0", "3", "-1", "one", "1.0", ""] {
            for command in ["record-start", "record-stop"] {
                assert_eq!(
                    parse(strings(&[command, "show.freemix", "--recorder", value])),
                    Err(ArgsError::InvalidRecorder(value.into())),
                    "{command} accepted --recorder {value:?}"
                );
            }
            for command in ["remote-record-start", "remote-record-stop"] {
                assert_eq!(
                    parse(strings(&[command, "127.0.0.1:9123", "--recorder", value])),
                    Err(ArgsError::InvalidRecorder(value.into())),
                    "{command} accepted --recorder {value:?}"
                );
            }
        }
        // The exact operator-facing message names the flag and the two slots.
        assert_eq!(
            ArgsError::InvalidRecorder("3".into()).to_string(),
            "invalid value for --recorder `3`; expected 1 or 2"
        );
        assert_eq!(
            ArgsError::InvalidRecorder("nope".into()).to_string(),
            "invalid value for --recorder `nope`; expected 1 or 2"
        );
    }

    /// Protocol 2.64: arm a GO of the standby cue, or of a named cue, a
    /// delay of 1 ms to 24 hours ahead.
    #[test]
    fn parses_cue_arm_local_and_remote() {
        use fm_protocol::{CommandPayload, CueOperation};

        let address: SocketAddr = "127.0.0.1:9123".parse().unwrap();
        let control = |operation| CommandPayload::CueControl { list: 3, operation };
        assert_eq!(
            parse(strings(&["remote-cue-arm", "127.0.0.1:9123", "3", "1500"])),
            Ok(Command::RemoteCue {
                address,
                payload: control(CueOperation::Arm {
                    cue: None,
                    in_ms: 1_500,
                    targets: fm_protocol::WireTargetBindings::default()
                }),
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "cue-arm",
                "show.freemix",
                "3",
                "86400000",
                "20",
                "--expect",
                "4"
            ])),
            Ok(Command::Cue {
                path: PathBuf::from("show.freemix"),
                payload: control(CueOperation::Arm {
                    cue: Some(20),
                    in_ms: 86_400_000,
                    targets: fm_protocol::WireTargetBindings::default()
                }),
                key: None,
                expected_revision: Some(4),
            })
        );
        for bad in ["0", "86400001", "soon"] {
            assert!(matches!(
                parse(strings(&["remote-cue-arm", "127.0.0.1:9123", "3", bad])),
                Err(ArgsError::InvalidNumber {
                    field: "milliseconds",
                    ..
                })
            ));
        }
        assert_eq!(
            parse(strings(&["remote-cue-disarm", "127.0.0.1:9123", "3"])),
            Ok(Command::RemoteCue {
                address,
                payload: control(CueOperation::Disarm),
                key: None,
                expected_revision: None,
            })
        );
    }

    /// RC-004: `--selected <input>` names the input a fire's actions on
    /// the selected input act on, for `go`, `cue-arm`, and `shortcut-fire`,
    /// before or after `--key` and `--expect`; no other cue command takes
    /// it.
    #[allow(clippy::too_many_lines)] // one case per fire command
    #[test]
    fn parses_a_fires_selected_input() {
        use fm_protocol::{CommandPayload, CueOperation, WireInputId, WireTargetBindings};

        let address: SocketAddr = "127.0.0.1:9123".parse().unwrap();
        let selected = |input: u128| WireTargetBindings {
            selected: Some(WireInputId::new(
                core::num::NonZeroU128::new(input).unwrap(),
            )),
            ..WireTargetBindings::NONE
        };
        let control = |operation| CommandPayload::CueControl { list: 3, operation };
        assert_eq!(
            parse(strings(&[
                "remote-go",
                "127.0.0.1:9123",
                "3",
                "--selected",
                "7",
                "--expect",
                "4"
            ])),
            Ok(Command::RemoteCue {
                address,
                payload: control(CueOperation::Go {
                    cue: None,
                    targets: selected(7),
                }),
                key: None,
                expected_revision: Some(4),
            })
        );
        assert_eq!(
            parse(strings(&[
                "go",
                "show.freemix",
                "3",
                "20",
                "--key",
                "k",
                "--selected",
                "2"
            ])),
            Ok(Command::Cue {
                path: PathBuf::from("show.freemix"),
                payload: control(CueOperation::Go {
                    cue: Some(20),
                    targets: selected(2),
                }),
                key: Some("k".into()),
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "cue-arm",
                "show.freemix",
                "3",
                "1500",
                "20",
                "--selected",
                "5"
            ])),
            Ok(Command::Cue {
                path: PathBuf::from("show.freemix"),
                payload: control(CueOperation::Arm {
                    cue: Some(20),
                    in_ms: 1_500,
                    targets: selected(5),
                }),
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "shortcut-fire",
                "show.freemix",
                "9",
                "--selected",
                "7"
            ])),
            Ok(Command::Cue {
                path: PathBuf::from("show.freemix"),
                payload: CommandPayload::FireShortcut {
                    shortcut: 9,
                    targets: selected(7),
                    value: None,
                },
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-shortcut-fire",
                "127.0.0.1:9123",
                "9",
                "--key",
                "fire-1"
            ])),
            Ok(Command::RemoteCue {
                address,
                payload: CommandPayload::FireShortcut {
                    shortcut: 9,
                    targets: WireTargetBindings::NONE,
                    value: None,
                },
                key: Some("fire-1".into()),
                expected_revision: None,
            })
        );
        // RC-004 dynamic values: the firing control's value, per mille.
        assert_eq!(
            parse(strings(&[
                "remote-shortcut-fire",
                "127.0.0.1:9123",
                "9",
                "--value",
                "750",
                "--selected",
                "7"
            ])),
            Ok(Command::RemoteCue {
                address,
                payload: CommandPayload::FireShortcut {
                    shortcut: 9,
                    targets: selected(7),
                    value: fm_protocol::FireValue::new(750),
                },
                key: None,
                expected_revision: None,
            })
        );
        for bad in ["1001", "-1", "0.5", "+5", "x"] {
            assert!(
                matches!(
                    parse(strings(&[
                        "shortcut-fire",
                        "show.freemix",
                        "9",
                        "--value",
                        bad
                    ])),
                    Err(ArgsError::InvalidNumber {
                        field: "fire value",
                        ..
                    })
                ),
                "{bad}"
            );
        }
        assert!(matches!(
            parse(strings(&[
                "shortcut-fire",
                "show.freemix",
                "9",
                "--value",
                "1",
                "--value",
                "2"
            ])),
            Err(ArgsError::UnexpectedArgument(_))
        ));
        assert!(matches!(
            parse(strings(&["go", "show.freemix", "3", "--value", "5"])),
            Err(ArgsError::UnknownOption(_))
        ));
        for bad in ["0", "camera", "-1"] {
            assert!(
                matches!(
                    parse(strings(&["go", "show.freemix", "3", "--selected", bad])),
                    Err(ArgsError::InvalidNumber {
                        field: "selected input",
                        ..
                    })
                ),
                "{bad}"
            );
        }
        assert!(matches!(
            parse(strings(&["go", "show.freemix", "3", "--selected"])),
            Err(ArgsError::MissingValue("selected input"))
        ));
        assert!(matches!(
            parse(strings(&[
                "shortcut-fire",
                "show.freemix",
                "9",
                "--selected",
                "1",
                "--selected",
                "2"
            ])),
            Err(ArgsError::UnexpectedArgument(_))
        ));
        assert!(matches!(
            parse(strings(&[
                "cue-stop",
                "show.freemix",
                "3",
                "--selected",
                "1"
            ])),
            Err(ArgsError::UnknownOption(_))
        ));
        assert!(matches!(
            parse(strings(&["shortcut-fire", "show.freemix"])),
            Err(ArgsError::MissingValue("shortcut"))
        ));
    }

    #[allow(clippy::too_many_lines)] // one case per cue command
    #[test]
    fn parses_cue_commands_local_and_remote() {
        use fm_protocol::{CommandPayload, CueOperation};

        let address: SocketAddr = "127.0.0.1:9123".parse().unwrap();
        let remote = |payload, expected_revision| {
            Ok(Command::RemoteCue {
                address,
                payload,
                key: None,
                expected_revision,
            })
        };
        let control = |operation| CommandPayload::CueControl { list: 3, operation };
        assert_eq!(
            parse(strings(&["remote-go", "127.0.0.1:9123", "3"])),
            remote(
                control(CueOperation::Go {
                    cue: None,
                    targets: fm_protocol::WireTargetBindings::default()
                }),
                None
            )
        );
        assert_eq!(
            parse(strings(&[
                "remote-go",
                "127.0.0.1:9123",
                "3",
                "20",
                "--expect",
                "4"
            ])),
            remote(
                control(CueOperation::Go {
                    cue: Some(20),
                    targets: fm_protocol::WireTargetBindings::default()
                }),
                Some(4)
            )
        );
        assert_eq!(
            parse(strings(&[
                "remote-go",
                "127.0.0.1:9123",
                "3",
                "--expect",
                "4"
            ])),
            remote(
                control(CueOperation::Go {
                    cue: None,
                    targets: fm_protocol::WireTargetBindings::default()
                }),
                Some(4)
            )
        );
        assert_eq!(
            parse(strings(&[
                "remote-cue-standby",
                "127.0.0.1:9123",
                "3",
                "20"
            ])),
            remote(control(CueOperation::Standby { cue: 20 }), None)
        );
        for (command, operation) in [
            ("remote-cue-back", CueOperation::Back),
            ("remote-cue-skip", CueOperation::Skip),
            ("remote-cue-stop", CueOperation::Stop),
            ("remote-cue-reset", CueOperation::Reset),
        ] {
            assert_eq!(
                parse(strings(&[command, "127.0.0.1:9123", "3"])),
                remote(control(operation), None)
            );
        }
        assert_eq!(
            parse(strings(&["remote-cue-list-remove", "127.0.0.1:9123", "3"])),
            remote(CommandPayload::RemoveCueList { list: 3 }, None)
        );
        assert_eq!(
            parse(strings(&["remote-cue-lists", "127.0.0.1:9123"])),
            Ok(Command::RemoteCueLists { address })
        );
        assert_eq!(
            parse(strings(&["cue-list-set", "show.freemix", "3", "Show", "-"])),
            Ok(Command::Cue {
                path: PathBuf::from("show.freemix"),
                payload: CommandPayload::SetCueList {
                    list: 3,
                    spec: Box::new(fm_protocol::CueListSpec {
                        name: "Show".into(),
                        cues: Vec::new(),
                    }),
                    position: None,
                },
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "cue-list-set",
                "show.freemix",
                "3",
                "Show",
                "1:A:2:"
            ])),
            Err(ArgsError::InvalidChoice {
                field: "cue",
                value: "1:A:2:".into(),
            })
        );
        assert_eq!(
            parse(strings(&["go", "show.freemix"])),
            Err(ArgsError::MissingValue("cue list"))
        );
        assert_eq!(
            parse(strings(&["cue-lists", "show.freemix", "extra"])),
            Err(ArgsError::UnexpectedArgument("extra".into()))
        );
    }

    #[test]
    fn parses_remote_playlist_operations() {
        use fm_protocol::{
            CommandPayload, PlaylistEntrySpec, PlaylistLoopMode, PlaylistOperation, PlaylistStart,
            PlaylistTransitionKind, WireInputId,
        };

        let address: SocketAddr = "127.0.0.1:9123".parse().unwrap();
        let playlist = |payload, key: Option<&str>| {
            Ok(Command::RemotePlaylist {
                address,
                payload,
                key: key.map(Into::into),
                expected_revision: None,
            })
        };
        let wire = |value| WireInputId::new(core::num::NonZeroU128::new(value).unwrap());
        assert_eq!(
            parse(strings(&[
                "remote-playlist",
                "127.0.0.1:9123",
                "set",
                "all",
                "1:2:250:cut:0:1,2:3:100:fade:12:0",
            ])),
            playlist(
                CommandPayload::SetPlaylist {
                    loop_mode: PlaylistLoopMode::All,
                    entries: vec![
                        PlaylistEntrySpec {
                            id: 1,
                            input: wire(2),
                            duration_frames: 250,
                            transition: PlaylistTransitionKind::Cut,
                            transition_frames: 0,
                            enabled: true,
                        },
                        PlaylistEntrySpec {
                            id: 2,
                            input: wire(3),
                            duration_frames: 100,
                            transition: PlaylistTransitionKind::Fade,
                            transition_frames: 12,
                            enabled: false,
                        },
                    ],
                },
                None
            )
        );
        assert_eq!(
            parse(strings(&[
                "remote-playlist",
                "127.0.0.1:9123",
                "start",
                "after:50",
                "--from",
                "2",
                "--key",
                "go",
            ])),
            playlist(
                CommandPayload::PlaylistControl {
                    operation: PlaylistOperation::Start {
                        at: PlaylistStart::AfterFrames(50),
                        from: Some(2),
                    },
                },
                Some("go")
            )
        );
        assert_eq!(
            parse(strings(&["remote-playlist", "127.0.0.1:9123", "jump", "7"])),
            playlist(
                CommandPayload::PlaylistControl {
                    operation: PlaylistOperation::Jump { entry: 7 },
                },
                None
            )
        );
        assert!(
            parse(strings(&[
                "remote-playlist",
                "127.0.0.1:9123",
                "set",
                "off",
                "1:0:250:cut:0:1",
            ]))
            .is_err()
        );
        assert!(parse(strings(&["remote-playlist", "127.0.0.1:9123", "rewind"])).is_err());
    }

    #[test]
    fn parses_remote_preview_options() {
        assert_eq!(
            parse(strings(&[
                "remote-preview",
                "127.0.0.1:9123",
                "2",
                "--key",
                "remote-preview",
                "--expect",
                "7",
            ])),
            Ok(Command::RemotePreview {
                address: "127.0.0.1:9123".parse().unwrap(),
                input: 2,
                key: Some("remote-preview".into()),
                expected_revision: Some(7),
            })
        );
    }

    #[test]
    fn parses_remote_history_and_padlock_commands() {
        let address: SocketAddr = "127.0.0.1:9123".parse().unwrap();
        assert_eq!(
            parse(strings(&["remote-undo", "127.0.0.1:9123"])),
            Ok(Command::RemoteUndo {
                address,
                redo: false,
                expected_edit: None,
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-redo",
                "127.0.0.1:9123",
                "--key",
                "redo-1",
                "--edit",
                "12",
                "--expect",
                "14",
            ])),
            Ok(Command::RemoteUndo {
                address,
                redo: true,
                expected_edit: Some(12),
                key: Some("redo-1".into()),
                expected_revision: Some(14),
            })
        );
        assert_eq!(
            parse(strings(&["remote-lock", "127.0.0.1:9123", "on"])),
            Ok(Command::RemoteLock {
                address,
                locked: true,
                input: None,
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-lock",
                "127.0.0.1:9123",
                "off",
                "--input",
                "3"
            ])),
            Ok(Command::RemoteLock {
                address,
                locked: false,
                input: Some(3),
                key: None,
                expected_revision: None,
            })
        );
        assert!(parse(strings(&["remote-undo", "127.0.0.1:9123", "--edit", "0"])).is_err());
        assert!(
            parse(strings(&[
                "remote-undo",
                "127.0.0.1:9123",
                "--edit",
                "1",
                "--edit",
                "2"
            ]))
            .is_err()
        );
        assert!(parse(strings(&["remote-lock", "127.0.0.1:9123", "maybe"])).is_err());
        assert!(parse(strings(&["remote-lock", "127.0.0.1:9123"])).is_err());
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn parses_remote_input_add_and_remove() {
        let address = "127.0.0.1:9123".parse().unwrap();
        assert_eq!(
            parse(strings(&[
                "remote-input-add",
                "127.0.0.1:9123",
                "solid",
                "7",
                "Guest",
                "0",
                "128",
                "255",
                "255",
                "--key",
                "add-7",
            ])),
            Ok(Command::RemoteInputAdd {
                address,
                input: 7,
                name: "Guest".into(),
                source: fm_protocol::LiveInputSource::Solid {
                    red: 0,
                    green: 128,
                    blue: 255,
                    alpha: 255,
                },
                key: Some("add-7".into()),
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-input-add",
                "127.0.0.1:9123",
                "bars",
                "8",
                "Bars",
                "--expect",
                "4",
            ])),
            Ok(Command::RemoteInputAdd {
                address,
                input: 8,
                name: "Bars".into(),
                source: fm_protocol::LiveInputSource::Bars,
                key: None,
                expected_revision: Some(4),
            })
        );
        assert_eq!(
            parse(strings(&["remote-input-remove", "127.0.0.1:9123", "7"])),
            Ok(Command::RemoteInputRemove {
                address,
                input: 7,
                key: None,
                expected_revision: None,
            })
        );
        let refused = parse(strings(&[
            "remote-input-add",
            "127.0.0.1:9123",
            "media",
            "9",
            "Clip",
        ]));
        assert_eq!(refused, Err(ArgsError::NotLiveInputKind("media".into())));
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("only solid and bars inputs are realized live")
        );
        // A solid input needs all four components; bars takes none.
        assert!(
            parse(strings(&[
                "remote-input-add",
                "127.0.0.1:9123",
                "solid",
                "7",
                "Guest",
                "0",
                "128",
            ]))
            .is_err()
        );
        assert!(
            parse(strings(&[
                "remote-input-add",
                "127.0.0.1:9123",
                "bars",
                "8",
                "Bars",
                "1",
            ]))
            .is_err()
        );
        assert!(
            parse(strings(&[
                "remote-input-add",
                "127.0.0.1:9123",
                "solid",
                "7",
                " "
            ]))
            .is_err()
        );
        assert!(parse(strings(&["remote-input-remove", "127.0.0.1:9123"])).is_err());
    }

    /// OR-012: replay settings edits and live replay operations parse
    /// into their typed commands; zero, repeated, and excess sources, an
    /// out-of-range retention, a relative root, bad speeds, last-N lengths,
    /// and conflicting roots are refused.
    #[test]
    #[allow(clippy::too_many_lines)] // one case per command shape
    fn replay_commands_parse_into_typed_operations() {
        use fm_protocol::{ReplayOperation, ReplaySpeed};
        let input = |value: u128| {
            fm_protocol::WireInputId::new(core::num::NonZeroU128::new(value).unwrap())
        };
        assert_eq!(
            parse(strings(&[
                "replay-set",
                "show.freemix",
                "--sources",
                "3,1",
                "--retention",
                "30",
                "--default-root",
            ]))
            .unwrap(),
            Command::ReplaySet {
                path: PathBuf::from("show.freemix"),
                edit: ReplayEdit {
                    sources: Some(vec![3, 1]),
                    retention_seconds: Some(30),
                    storage_root: Some(None),
                },
                key: None,
                expected_revision: None,
            }
        );
        assert!(
            parse(strings(&[
                "replay-set",
                "show.freemix",
                "--root",
                "/r",
                "--default-root"
            ]))
            .is_err()
        );
        let set = |words: &[&str]| {
            let mut all = vec!["replay-set", "show.freemix"];
            all.extend_from_slice(words);
            parse(strings(&all))
        };
        assert_eq!(
            set(&["--sources", "0"]),
            Err(ArgsError::InvalidChoice {
                field: "replay source",
                value: "0".to_owned(),
            })
        );
        let two = fm_types::InputId::new(core::num::NonZeroU128::new(2).unwrap());
        assert_eq!(
            set(&["--sources", "2,2"]),
            Err(ArgsError::InvalidReplay(
                fm_protocol::ReplaySettingsError::DuplicateSource(two)
            ))
        );
        assert_eq!(
            set(&["--sources", "1,2,3,4,5"]),
            Err(ArgsError::InvalidReplay(
                fm_protocol::ReplaySettingsError::TooManySources
            ))
        );
        assert_eq!(
            set(&["--retention", "5"]),
            Err(ArgsError::OutOfRange {
                field: "replay retention seconds",
                minimum: 10,
                maximum: 3_600,
                value: 5,
            })
        );
        assert_eq!(
            set(&["--retention", "3601"]),
            Err(ArgsError::OutOfRange {
                field: "replay retention seconds",
                minimum: 10,
                maximum: 3_600,
                value: 3_601,
            })
        );
        assert!(set(&["--retention", "10"]).is_ok());
        assert!(set(&["--retention", "3600"]).is_ok());
        assert_eq!(
            set(&["--root", "relative/dir"]),
            Err(ArgsError::InvalidReplay(
                fm_protocol::ReplaySettingsError::StorageRoot
            ))
        );
        assert!(set(&["--root", "/absolute/dir"]).is_ok());
        assert!(set(&["--sources", "none"]).is_ok());
        let remote = |words: &[&str]| {
            let mut all = vec!["remote-replay-do", "127.0.0.1:9123"];
            all.extend_from_slice(words);
            parse(strings(&all)).map(|command| match command {
                Command::RemoteReplayDo { operation, .. } => operation,
                other => panic!("{other:?}"),
            })
        };
        assert_eq!(
            remote(&["last", "10"]),
            Ok(ReplayOperation::CaptureLast { seconds: 10 })
        );
        assert_eq!(
            remote(&["play", "7", "50"]),
            Ok(ReplayOperation::Play {
                input: input(7),
                speed: ReplaySpeed::Half,
            })
        );
        assert_eq!(
            remote(&["cue", "7", "2", "--angle", "3"]),
            Ok(ReplayOperation::Cue {
                input: input(7),
                event: 2,
                angle: Some(input(3)),
            })
        );
        assert!(remote(&["last", "7"]).is_err());
        assert!(remote(&["play", "7", "60"]).is_err());
        assert!(remote(&["mark-in", "--name", "x"]).is_err());
    }

    /// OR-013 / Protocol 2.107: reel commands parse into typed specs and
    /// export controls; bad ids, names, clips, speeds, trims, and clip
    /// counts are refused before a project or daemon is touched.
    #[allow(clippy::too_many_lines)]
    #[test]
    fn reel_commands_parse_into_typed_specs_and_refuse_bad_input() {
        use core::num::NonZeroU128;

        let address: SocketAddr = "127.0.0.1:9123".parse().unwrap();
        let angle = |value: u128| fm_types::InputId::new(NonZeroU128::new(value).unwrap());
        let clip = |event, angle, trim_in, trim_out, speed| fm_protocol::ReelClip {
            event,
            angle,
            trim_in_frames: trim_in,
            trim_out_frames: trim_out,
            speed_percent: speed,
        };
        let spec = Box::new(fm_protocol::ReelSpec {
            id: 3,
            name: "Highlights".into(),
            clips: vec![clip(7, angle(1), 0, 0, 100), clip(8, angle(2), 5, 6, 50)],
        });
        assert_eq!(
            parse(strings(&["reel", "show.freemix"])),
            Ok(Command::Reel {
                path: PathBuf::from("show.freemix"),
            })
        );
        assert_eq!(
            parse(strings(&["remote-reel", "127.0.0.1:9123"])),
            Ok(Command::RemoteReel { address })
        );
        assert_eq!(
            parse(strings(&[
                "reel-set",
                "show.freemix",
                "--id",
                "3",
                "--name",
                "Highlights",
                "--clips",
                "7/1/0/0/100,8/2/5/6/50",
                "--key",
                "reel-key",
                "--expect",
                "4",
            ])),
            Ok(Command::ReelSet {
                path: PathBuf::from("show.freemix"),
                spec: spec.clone(),
                key: Some("reel-key".into()),
                expected_revision: Some(4),
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-reel-set",
                "127.0.0.1:9123",
                "--id",
                "3",
                "--name",
                "Highlights",
                "--clips",
                "7/1/0/0/100,8/2/5/6/50",
            ])),
            Ok(Command::RemoteReelSet {
                address,
                spec: spec.clone(),
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "reel-delete",
                "show.freemix",
                "--id",
                "3",
                "--key",
                "reel-delete",
            ])),
            Ok(Command::ReelDelete {
                path: PathBuf::from("show.freemix"),
                id: 3,
                key: Some("reel-delete".into()),
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-reel-delete",
                "127.0.0.1:9123",
                "--id",
                "3",
                "--expect",
                "7",
            ])),
            Ok(Command::RemoteReelDelete {
                address,
                id: 3,
                key: None,
                expected_revision: Some(7),
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-reel-export",
                "127.0.0.1:9123",
                "--job",
                "4",
                "--reel",
                "3",
                "--destination",
                "/srv/reels/highlights.mp4",
                "--key",
                "reel-export",
            ])),
            Ok(Command::RemoteReelExport {
                address,
                job: 4,
                reel: 3,
                destination: "/srv/reels/highlights.mp4".into(),
                key: Some("reel-export".into()),
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-reel-cancel",
                "127.0.0.1:9123",
                "--job",
                "4",
            ])),
            Ok(Command::RemoteReelCancel {
                address,
                job: 4,
                key: None,
                expected_revision: None,
            })
        );
        assert_eq!(
            parse(strings(&[
                "remote-reel-forget",
                "127.0.0.1:9123",
                "--job",
                "4",
            ])),
            Ok(Command::RemoteReelForget {
                address,
                job: 4,
                key: None,
                expected_revision: None,
            })
        );

        let set = |words: &[&str]| {
            let mut all = vec!["reel-set", "show.freemix", "--id", "1", "--name", "Reel"];
            all.extend_from_slice(words);
            parse(strings(&all))
        };
        let bare = |words: &[&str]| {
            let mut all = vec!["reel-set", "show.freemix"];
            all.extend_from_slice(words);
            parse(strings(&all))
        };
        assert_eq!(
            set(&["--clips", "1/1/0/0/60"]),
            Err(ArgsError::InvalidReel(fm_protocol::ReelSpecError::Speed(
                60
            )))
        );
        assert_eq!(
            set(&["--clips", "1/0/0/0/100"]),
            Err(ArgsError::InvalidChoice {
                field: "reel clip angle",
                value: "0".into(),
            })
        );
        assert_eq!(
            set(&["--clips", "1/1/216001/0/100"]),
            Err(ArgsError::InvalidReel(fm_protocol::ReelSpecError::Trim))
        );
        assert_eq!(
            set(&["--clips", "1/1/0/0"]),
            Err(ArgsError::InvalidReelClip("1/1/0/0".into()))
        );
        assert_eq!(
            set(&["--clips", "0/1/0/0/100"]),
            Err(ArgsError::InvalidChoice {
                field: "reel clip event",
                value: "0".into(),
            })
        );
        let many = (1..=65)
            .map(|_| "1/1/0/0/100")
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            set(&["--clips", many.as_str()]),
            Err(ArgsError::InvalidReel(
                fm_protocol::ReelSpecError::TooManyClips
            ))
        );
        assert_eq!(
            bare(&["--id", "1", "--name", " ", "--clips", "1/1/0/0/100"]),
            Err(ArgsError::InvalidReel(fm_protocol::ReelSpecError::Name))
        );
        assert_eq!(
            bare(&["--id", "0", "--name", "Reel", "--clips", "1/1/0/0/100"]),
            Err(ArgsError::InvalidChoice {
                field: "reel id",
                value: "0".into(),
            })
        );
        assert_eq!(
            bare(&[
                "--id",
                "1",
                "--id",
                "2",
                "--name",
                "Reel",
                "--clips",
                "1/1/0/0/100",
            ]),
            Err(ArgsError::DuplicateOption("--id"))
        );
        assert_eq!(
            bare(&["--name", "Reel", "--clips", "1/1/0/0/100"]),
            Err(ArgsError::MissingValue("reel id"))
        );
        assert_eq!(
            bare(&["--id", "1", "--clips", "1/1/0/0/100"]),
            Err(ArgsError::MissingValue("reel name"))
        );
        assert_eq!(
            bare(&["--id", "1", "--name", "Reel"]),
            Err(ArgsError::MissingValue("reel clips"))
        );
        assert_eq!(
            parse(strings(&[
                "remote-reel-export",
                "127.0.0.1:9123",
                "--job",
                "4",
                "--reel",
                "3",
                "--destination",
                "relative/reel.mp4",
            ])),
            Err(ArgsError::InvalidReelDestination)
        );
        assert_eq!(
            parse(strings(&[
                "remote-reel-export",
                "127.0.0.1:9123",
                "--job",
                "4",
                "--reel",
                "3",
            ])),
            Err(ArgsError::MissingValue("reel export destination"))
        );
        assert_eq!(
            parse(strings(&["reel-delete", "show.freemix"])),
            Err(ArgsError::MissingValue("reel id"))
        );
        assert_eq!(
            parse(strings(&[
                "remote-reel-cancel",
                "127.0.0.1:9123",
                "--job",
                "0",
            ])),
            Err(ArgsError::InvalidChoice {
                field: "reel export job",
                value: "0".into(),
            })
        );
    }
}
