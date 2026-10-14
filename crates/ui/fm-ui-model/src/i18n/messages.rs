//! Message keys with their compiled-in English text.
//!
//! Each line declares one [`Msg`] key and its English entry: `t("…")` for
//! a plain template, `plural("…one…", "…other…")` for a message whose form
//! follows its `{count}` argument. English is complete by construction;
//! other catalogs are checked against this list by unit test.

use super::{Entry, plural, t};

macro_rules! messages {
    ($($key:ident => $entry:expr,)*) => {
        /// A typed message key. The English text lives beside each key; see
        /// [`super::Locale::text`] and [`super::Locale::format`].
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub enum Msg {
            $($key,)*
        }

        impl Msg {
            /// Every key, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$key,)*];

            /// The key's name, for diagnostics and catalog reports.
            #[must_use]
            pub const fn id(self) -> &'static str {
                match self {
                    $(Self::$key => stringify!($key),)*
                }
            }

            /// The compiled-in English entry.
            #[must_use]
            pub const fn source(self) -> Entry {
                match self {
                    $(Self::$key => $entry,)*
                }
            }
        }
    };
}

impl Msg {
    /// The English text (the `other` form of a plural). Code that threads
    /// operator text as `&'static str` produces it with this, and drawing
    /// code words it in the console language with
    /// [`super::Locale::tr`].
    #[must_use]
    pub const fn en(self) -> &'static str {
        self.source().other()
    }
}

messages! {
    // Shared words and templates.
    Unavailable => t("Unavailable: {reason}"),
    None => t("none"),
    Locked => t("LOCKED"),
    Unlocked => t("UNLOCKED"),
    Pending => t("PENDING"),
    InUse => t("IN USE"),
    Live => t("LIVE"),
    Recording => t("RECORDING"),
    Stopped => t("STOPPED"),
    EditsLocked => t("EDITS LOCKED"),
    Undo => t("Undo"),
    Redo => t("Redo"),
    InputBank => t("Input bank"),
    SelectOnPreview => t("Select on Preview"),
    RenameEllipsis => t("Rename…"),
    AddInput => t("Add input"),
    AddInputEllipsis => t("Add input…"),
    FadeToBlack => t("Fade to black"),
    NoSynchronizedShow => t("No synchronized show"),
    KeyboardShortcuts => t("Keyboard shortcuts"),

    // Connection lifecycle.
    ConnLaunching => t("LAUNCHING"),
    ConnConnecting => t("CONNECTING"),
    ConnSynchronizing => t("SYNCHRONIZING"),
    ConnReady => t("CONTROL READY"),
    ConnBackoff => t("RETRY BACKOFF"),
    ConnDisconnected => t("DISCONNECTED"),
    ConnFailed => t("FAILED"),
    ConnProtocolMismatch => t("PROTOCOL MISMATCH"),

    // Why a control is disabled.
    ReasonProtocol => t("the daemon did not select the current protocol"),
    ReasonConnectionFailed => t("the control connection failed"),
    ReasonNoConnection => t("there is no control connection"),
    ReasonNotReady => t("the control connection is not ready yet"),
    ReasonNotSynchronized => t("project state is not synchronized yet"),
    ReasonNoSelectPreview => t("this session lacks the select_preview permission"),
    ReasonNoTransition => t("this session lacks the transition permission"),
    ReasonNoControlAudio => t("this session lacks the control_audio permission"),
    ReasonNoEditProject => t("this session lacks the edit_project permission"),
    ReasonManualInFlight => t("a manual T-bar transition is in progress"),
    ReasonStateDisallows => t("the current state does not allow it"),
    ReasonProjectLocked => t("the project is locked against edits"),
    ReasonInputLocked => t("this input is locked against edits"),
    ReasonNothingToUndo => t("there is no project edit to undo"),
    ReasonNothingToRedo => t("there is no project edit to redo"),
    ReasonNeighbourLocked => t("the neighbouring input is locked against moves"),
    ReasonNoPreviewInput => t("no input is selected on Preview"),
    ReasonPreviewFirst => t("the Preview input is already first"),
    ReasonPreviewLast => t("the Preview input is already last"),
    ReasonInputFirst => t("this input is already first"),
    ReasonInputLast => t("this input is already last"),
    ReasonShowHasNoInputs => t("the show has no inputs"),
    ReasonNoManualActive => t("no manual transition is active"),
    ReasonRecordingPending => t("an earlier recording command is still pending"),
    ReasonNoStreamTargets => t("the show has no configured stream targets"),
    ReasonNoStingerConfigured => t("no Stinger slot is configured"),
    ReasonStingerSlotNotConfigured => t("this Stinger slot is not configured"),
    ReasonSidePanelHidden => t("the side panel is hidden"),
    ReasonInputLimit => t("the show already holds the maximum of 256 inputs"),
    ReasonUnknownInput => t("the input no longer exists"),
    ReasonRemovalPending => t("a removal of this input is already pending"),
    ReasonOnProgram => t("this input is on Program"),
    ReasonOnPreview => t("this input is on Preview"),
    ReasonInTransition => t("this input is in the active manual transition"),
    ReasonStingerMedia => t("this input is a Stinger slot's media"),
    ReasonOverlayOnAir => t("this input is on air in an overlay"),
    ReasonOverlayQueued => t("this input is queued on an overlay"),
    ReasonNameEmpty => t("the name is empty"),
    ReasonNameTooLong => t("the name is too long"),
    ReasonNameTooLongBytes => t("the name is longer than 128 bytes"),
    ReasonNameTaken => t("another input already uses this name"),
    ReasonNameUnchanged => t("the name is unchanged"),

    // Top bar.
    UntitledShow => t("Untitled show"),
    ShowNameHover => t("The show name appears once project state is synchronized"),
    ConnectionPillHover => t("Control connection lifecycle"),
    RevisionShort => t("REV {revision}"),
    RevisionHover => t("Replicated project revision"),
    PendingPill => t("{count} PENDING"),
    PadlockClickUnlock => t("Click to unlock project edits."),
    PadlockClickLock => t("Click to lock project edits; live switching stays available."),
    ClockHover => t("Wall clock (UTC) · {date}"),
    BadgeRec => t("REC"),
    LiveHoverLive => t("A stream target reports realized Live"),
    LiveHoverDesired => t("A stream target is desired running but not reported live"),
    LiveHoverIdle => t("No stream target is desired running"),
    RecHoverRecording => t("A recorder slot is desired recording"),
    RecHoverIdle => t("No recorder slot is desired recording"),

    // Edit history and padlocks.
    UndoEditAt => t("Undo edit at REV {revision}"),
    RedoEditAt => t("Redo edit at REV {revision}"),
    UndoHoverAt => t("Undo the project edit at REV {revision}"),
    RedoHoverAt => t("Redo the project edit at REV {revision}"),
    HistoryEmpty => t("history empty"),
    HistorySteps => plural("{count} step in the shared history", "{count} steps in the shared history"),
    PadlockHoverLocked => t("Project edits are locked; live switching still works. Click to unlock."),
    PadlockHoverUnlocked => t("Project edits are unlocked. Click to lock edits; live switching stays available."),
    PadlockMenuUnlock => t("Unlock project edits"),
    PadlockMenuLock => t("Lock project edits"),
    InputsLockedChip => plural("{count} LOCKED", "{count} LOCKED"),
    InputsLockedDetail => plural(
        "{count} input is locked against rename, move, and removal.",
        "{count} inputs are locked against rename, move, and removal."
    ),
    ProjectEditsLockedDetail => t("Project edits are locked: rename, move, adding and removing inputs, Stinger slot setup, undo, and redo are blocked. Switching, transitions, audio, overlays, and outputs still work."),
    ProjectEditsUnlocked => t("Project edits are unlocked."),

    // Status bar.
    PendingCount => plural("{count} pending", "{count} pending"),
    StatusError => t("ERROR | {message}"),
    StatusNotice => t("NOTICE | {message}"),
    StatusUncertain => plural(
        "{count} command with uncertain outcome; last {command}",
        "{count} commands with uncertain outcome; last {command}"
    ),
    StatusIdleReady => t("Ready"),
    StatusIdleProtocol => t("Peer did not select the current protocol"),
    StatusIdleFailed => t("Connection failed"),
    StatusIdleDisconnected => t("No control connection"),
    StatusIdleWaiting => t("Waiting for synchronized state"),
    StatusBarKeyHints => t("1–8 PVW   C CUT   F FADE   B FTB   ESC CANCEL T-BAR   ·   HELP › SHORTCUTS"),
    StatusBarKeys => t("KEYS"),

    // Toasts.
    ToastNotice => t("NOTICE"),
    ToastError => t("ERROR"),
    ToastUncertain => t("OUTCOME UNCERTAIN"),
    ToastDone => t("DONE"),
    ToastDismissHover => t("Dismiss; the status bar keeps the latest message"),
    ToastUncertainCommand => t("Command {command} may or may not have applied (server receipt {receipt}). Check Program before retrying."),
    QueuedCommands => plural(
        "{count} command queued in the operator FIFO",
        "{count} commands queued in the operator FIFO"
    ),
    ToastAdding => t("Adding \"{name}\" as input {input}…"),
    ToastRemoving => t("Removing \"{name}\" (input {input})…"),
    ToastAdded => t("Added \"{name}\" as input {input} at position {position}. Undo removes it again."),
    ToastRemoved => t("Removed \"{name}\" (input {input}) with its mixer strip."),
    ToastAddNotApplied => t("Add of \"{name}\" was not applied: {detail}"),
    ToastRemovalNotApplied => t("Removal of \"{name}\" was not applied: {detail}"),
    ToastAddNotConfirmed => t("Add of \"{name}\" was not confirmed after {seconds} s: {detail}. Check the input bank."),
    ToastRemovalNotConfirmed => t("Removal of \"{name}\" was not confirmed after {seconds} s: {detail}. Check the input bank."),
    NoDaemonResult => t("no result from the daemon"),

    // Menu bar.
    MenuFile => t("File"),
    MenuEdit => t("Edit"),
    MenuView => t("View"),
    MenuInputs => t("Inputs"),
    MenuOutput => t("Output"),
    MenuHelp => t("Help"),
    FileShowHeading => t("Show"),
    FileRevisionInputs => plural("Revision {revision} · {count} input", "Revision {revision} · {count} inputs"),
    FileSavedByDaemon => t("Shows are saved by the daemon after every accepted edit."),
    FileQuit => t("Quit FreeMix Studio"),
    FileQuitHover => t("Close this window; a supervised daemon is shut down with it"),
    EditPreviewInputHeading => t("Preview input"),
    MoveEarlier => t("Move earlier"),
    MoveLater => t("Move later"),
    HistoryHeading => t("History"),
    ViewPanels => t("Panels"),
    ViewInputBankHover => t("Show or hide the input bank"),
    ViewAudioMixer => t("Audio mixer"),
    ViewAudioMixerHover => t("Show or hide the audio mixer"),
    ViewSidePanel => t("Side panel"),
    ViewSidePanelHover => t("Show or hide recording, streams, overlays, and stingers"),
    ViewStatusBar => t("Status bar"),
    ViewStatusBarHover => t("Show or hide the bottom status bar"),
    ViewSidePanelTab => t("Side panel tab"),
    TabOverlays => t("Overlays"),
    TabStingers => t("Stingers"),
    ViewResetLayout => t("Reset layout"),
    ViewResetLayoutDisabled => t("Every panel is already shown"),
    ViewLanguage => t("Language"),
    ViewLanguageHover => t("Console language; local to this console and never sent to the daemon"),
    ViewClock12h => t("12-hour clock"),
    ViewClock12hHover => t("Show the top-bar clock with AM/PM instead of 24 hours"),
    NoInputs => t("No inputs"),
    CutDirectlyToProgram => t("Cut directly to Program"),
    CatalogueHeading => t("Catalogue"),
    AddInputMenuHover => t("Append a solid colour or colour bars input to the end of the bank"),
    RemoveInput => t("Remove input"),
    RemoveInputHover => t("Remove {name} (asks for confirmation)"),
    RemoveInputMenuHover => t("Remove an input that is not on air or in use"),
    TransitionHeading => t("Transition · {frames} fr"),
    TransitionCut => t("Cut"),
    TransitionFade => t("Fade"),
    TransitionWipe => t("Wipe"),
    TransitionAlphaFade => t("AlphaFade"),
    TransitionSlide => t("Slide"),
    TransitionZoom => t("Zoom"),
    ReturnToLive => t("Return to live"),
    ManualTBarHeading => t("Manual T-bar"),
    CommitTransition => t("Commit transition"),
    CancelTransition => t("Cancel transition"),
    RecordingHeading => t("Recording"),
    StartRecorder => t("Start recorder {slot}"),
    StopRecorder => t("Stop recorder {slot}"),
    RecorderHover => t("{label}: Program recorder slot {slot}"),
    StreamsHeading => t("Streams"),
    NoStreamTargets => t("No stream targets"),
    StartStream => t("Start {name}"),
    StopStream => t("Stop {name}"),
    AssignStingerMedia => t("Assign Preview as stinger media"),
    AssignStingerMediaHover => t("Use the Preview input as a slot's media; other slot settings are kept"),
    AssignStingerSlotHover => t("Assign the Preview input as Stinger S{slot} media"),
    StingerSlot => t("Slot S{slot}"),
    ClearStingerSlot => t("Clear stinger slot"),
    ClearStingerSlotHover => t("Clear Stinger S{slot}"),
    StingerSlotsEllipsis => t("Stinger slots…"),
    StingerSlotsHover => t("Open the Stingers tab to pick any input as slot media"),
    AboutStudio => t("About FreeMix Studio"),
    About => t("About"),
    AboutVersion => t("Version {version}"),
    AboutDescription => t("Native operator console for the freemixd live production engine. The console renders replicated control state and sends commands; the daemon owns media, persistence, and outputs."),
    AboutLicense => t("License: {license}"),
    CloseEsc => t("Close  [Esc]"),
    KeysActive => t("Keys active"),
    KeysInactive => t("Keys inactive: {reason}"),
    ShortcutsIntro => t("Keys act only while the console is ready, no text field has focus, and the session holds the matching permission."),
    ShortcutsFooter => t("Esc or a click outside closes this reference"),
    ShortcutGroupSwitching => t("Switching"),
    ShortcutGroupTransitions => t("Transitions"),
    ShortcutGroupEditing => t("Editing"),
    ShortcutGroupInputBank => t("Input bank (pointer and focus)"),
    ShortcutSelectPreview => t("Select input 1–8 on Preview"),
    ShortcutCut => t("Cut Preview to Program"),
    ShortcutFade => t("Fade Preview to Program (shared duration)"),
    ShortcutFadeToBlack => t("Toggle Fade-to-Black"),
    ShortcutCancelManual => t("Cancel the active manual T-bar transition"),
    ShortcutUndo => t("Undo the last project edit (shared history)"),
    ShortcutRedo => t("Redo the last undone project edit"),
    ShortcutTileClick => t("Select the input on Preview"),
    ShortcutTileDoubleClick => t("Select on Preview, then Cut to Program"),
    ShortcutTileMenu => t("Input menu: Preview, Cut, Rename, Move, Remove"),
    ShortcutTileFocus => t("Focus a tile, then select it on Preview"),
    KeyClick => t("Click"),
    KeyDoubleClick => t("Double-click"),
    KeyDoubleClickShort => t("Dbl-click"),
    KeyRightClick => t("Right-click"),
    KeyTabEnter => t("Tab · Enter"),

    // Output rows.
    StatusNoState => t("NO STATE"),
    StreamStarting => t("STARTING"),
    StreamReconnecting => t("RECONNECTING"),
    StreamCongested => t("CONGESTED"),
    StreamNotLive => t("NOT LIVE"),
    StreamOff => t("OFF"),
    StreamUnavailable => t("UNAVAILABLE"),
    RowNotSynchronized => t("Not synchronized"),
    RowCommandPending => t("Command pending"),
    RowRecordingProgram => t("Recording Program"),
    RowProgramIdle => t("Program · idle"),
    RowStreamCounters => plural("{bytes} sent · {count} dropped", "{bytes} sent · {count} dropped"),
    RowSegments => plural("{count} seg", "{count} seg"),
    RowRequestedNoSample => t("Requested · no transport sample"),
    RowDesiredOff => t("Desired off"),
    StingerStateUnavailable => t("STATE UNAVAILABLE"),
    StingerUnconfigured => t("UNCONFIGURED"),
    StingerNotRequested => t("NOT REQUESTED"),
    StingerReady => t("READY"),
    StingerMissing => t("MISSING"),

    // Input bank.
    SourcesUnknown => t("— SOURCES"),
    SourcesCount => plural("{count} SOURCE", "{count} SOURCES"),
    BankGestureHint => t("click PVW · double-click CUT · right-click more"),
    NoInputState => t("No input state"),
    NoInputStateDetail => t("Sources appear once project state is synchronized."),
    ThumbnailNone => t("Thumbnail: none (only Preview/Program tap frames are delivered)"),
    ThumbnailProgram => t("Thumbnail: current Program tap frame"),
    ThumbnailPreview => t("Thumbnail: current Preview tap frame"),
    TileGestures => t("Click: select Preview{shortcut} · Double-click: cut to Program · Right-click: more"),
    TileMoreHover => t("More: Preview, Cut, Rename, Move, Remove"),
    CutKey => t("CUT"),
    TileCutHover => t("Select this input on Preview, then cut it to Program  [double-click]"),
    CutToProgram => t("Cut to Program"),
    MoveLeft => t("Move left"),
    MoveRight => t("Move right"),
    RemoveEllipsis => t("Remove…"),
    NotePreviewCutUnavailable => t("Preview and Cut unavailable: {reason}"),
    NoteCutUnavailable => t("Cut unavailable: {reason}"),
    NoteEditUnavailable => t("Rename, Move, and Remove unavailable: {reason}"),
    NoteRemoveUnavailable => t("Remove unavailable: {reason}"),
    ApplyKey => t("APPLY"),
    CancelKey => t("CANCEL"),
    RenameApplyHover => t("Rename this input  [Enter]"),
    RenameCancelHover => t("Discard this rename"),
    AddTileHover => t("Add input…\nA solid colour or colour bars, appended to the end of the bank"),
    AddingName => t("ADDING {name}…"),
    InputCountHover => t("Inputs in the show / the most a show may hold"),
    AddTileKinds => t("solid colour · colour bars"),
    NoThumbnail => t("no thumbnail"),

    // Tally.
    TallyProgram => t("PROGRAM"),
    TallyPreview => t("PREVIEW"),
    TallyProgramDesired => t("PROGRAM DESIRED"),
    TallyPreviewDesired => t("PREVIEW DESIRED"),
    TallyNone => t("NO TALLY"),

    // Dates and times.
    TimeAm => t("AM"),
    TimePm => t("PM"),
    DateLong => t("{month} {day}, {year}"),
    MonthJan => t("Jan"),
    MonthFeb => t("Feb"),
    MonthMar => t("Mar"),
    MonthApr => t("Apr"),
    MonthMay => t("May"),
    MonthJun => t("Jun"),
    MonthJul => t("Jul"),
    MonthAug => t("Aug"),
    MonthSep => t("Sep"),
    MonthOct => t("Oct"),
    MonthNov => t("Nov"),
    MonthDec => t("Dec"),
}
