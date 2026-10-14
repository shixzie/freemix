use std::{net::SocketAddr, path::PathBuf};

use fm_model::{DEFAULT_VIDEO_BITRATE_KBPS, StartupPolicy, StreamLadder, StreamLadderRung};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ManualTransitionKind {
    Fade,
    Wipe,
    AlphaFade,
    Slide,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayTransition {
    Cut,
    Fade,
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
        rotation: u16,
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
    RemoteStatus {
        address: SocketAddr,
    },
    RemoteDiagnostics {
        address: SocketAddr,
    },
    RemoteAudioStrip {
        address: SocketAddr,
        input: u128,
        gain_millidb: i32,
        balance_basis_points: i32,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArgsError {
    MissingCommand,
    MissingValue(&'static str),
    BlankValue(&'static str),
    /// HLS writes into one local directory and has no failover endpoint.
    BackupNotAllowed,
    /// RTMP/RTMPS/SRT publish one rendition, so a multi-bitrate ladder has no
    /// meaning there.
    LadderNotAllowed,
    /// The authored rungs are individually shaped but invalid as a ladder.
    InvalidLadder(fm_model::StreamLadderError),
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
}

impl core::fmt::Display for ArgsError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MissingCommand => formatter.write_str("a command is required"),
            Self::MissingValue(field) => write!(formatter, "missing value for {field}"),
            Self::BlankValue(field) => write!(formatter, "{field} must not be blank"),
            Self::BackupNotAllowed => {
                formatter.write_str("hls:// stream destinations do not support --backup")
            }
            Self::LadderNotAllowed => {
                formatter.write_str("only hls:// stream destinations support --rung")
            }
            Self::InvalidLadder(error) => write!(formatter, "{error}"),
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
            Self::UnknownOption(option) => write!(formatter, "unknown option `{option}`"),
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
        "remote-audio-strip" => parse_remote_audio_strip(arguments),
        "remote-rename" => parse_remote_rename(arguments),
        "remote-input-reorder" => parse_reorder(arguments, true),
        "remote-input-add" => parse_remote_input_add(arguments),
        "remote-input-remove" => parse_remote_input_remove(arguments),
        "remote-preview" => parse_remote_preview(arguments),
        "remote-cut" => parse_remote_cut(arguments),
        "remote-playlist" => parse_remote_playlist(arguments),
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
        "help" | "--help" | "-h" => Ok(Command::Help),
        _ => Err(ArgsError::UnknownCommand(command)),
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
        Some(nonblank(&mut arguments, "stream key")?)
    };
    let name = nonblank(&mut arguments, "stream name")?;
    let mut backup_url = None;
    let mut startup = StartupPolicy::Stopped;
    let mut video_bitrate_kbps = DEFAULT_VIDEO_BITRATE_KBPS;
    let mut rungs = Vec::new();
    while let Some(option) = arguments.next() {
        match option.as_str() {
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
        },
    ))
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
    reject_extra(&mut arguments)?;
    Ok(Command::AudioStrip {
        path,
        input,
        gain_millidb,
        balance_basis_points,
        muted,
        soloed,
        follow_video,
        delay_samples,
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
    let rotation = number::<u16>(&required(&mut arguments, "rotation")?, "rotation")?;
    if !matches!(rotation, 0 | 90 | 180 | 270) {
        return Err(ArgsError::InvalidChoice {
            field: "rotation",
            value: rotation.to_string(),
        });
    }
    reject_extra(&mut arguments)?;
    Ok(Command::SceneLayerGeometry {
        path,
        scene,
        index,
        x,
        y,
        width,
        height,
        rotation,
    })
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
        let kind = manual_kind(&required(arguments, "transition kind")?)?;
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
    let (key, expected_revision) = command_options(arguments)?;
    Ok(Command::RemoteAudioStrip {
        address,
        input,
        gain_millidb,
        balance_basis_points,
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

fn manual_kind(value: &str) -> Result<ManualTransitionKind, ArgsError> {
    match value {
        "fade" => Ok(ManualTransitionKind::Fade),
        "wipe" => Ok(ManualTransitionKind::Wipe),
        "alpha-fade" => Ok(ManualTransitionKind::AlphaFade),
        "slide" => Ok(ManualTransitionKind::Slide),
        _ => Err(ArgsError::InvalidChoice {
            field: "transition kind",
            value: value.to_owned(),
        }),
    }
}

fn overlay_transition(value: &str) -> Result<OverlayTransition, ArgsError> {
    match value {
        "cut" => Ok(OverlayTransition::Cut),
        "fade" => Ok(OverlayTransition::Fade),
        _ => Err(ArgsError::InvalidChoice {
            field: "overlay transition",
            value: value.to_owned(),
        }),
    }
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
                muted: true,
                soloed: true,
                follow_video: false,
                delay_samples: 48_000,
            })
        );
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
}
