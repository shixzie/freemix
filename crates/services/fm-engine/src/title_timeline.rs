//! Title timelines (GX-001): every title input's timed content (tickers,
//! scene clocks, since-take clocks) and its In/Out animation start on the
//! frame the title is taken to Program or Preview, not when the show
//! started.
//!
//! After every realized frame the engine dates each title input it shows:
//!
//! * On air ([`TitleShownOn::Air`]): on Program, either side of a timed or
//!   manual transition, or on an overlay channel. A title newly on air, or
//!   coming to air from Preview, starts its timeline on that frame. A title
//!   take ([`crate::EngineCommand::TakeTitle`]) that starts its In phase
//!   (a fresh take, or a re-take during Out) restarts it there too.
//! * On Preview ([`TitleShownOn::Preview`]) and not on air: the timeline
//!   starts on the frame it reaches Preview, so the operator sees the
//!   entrance and the timed content before taking it.
//! * Shown neither way, the title has no timeline: its timed content rests
//!   at its start.
//!
//! On Program and Preview a title also plays its template's In phase from
//! the frame its timeline started, like a Scene input
//! ([`crate::EngineSceneAir`]): a timed or manual transition that takes it
//! off Program plays its Out phase from the transition's first frame (an
//! Out shorter than the transition holds its last frame), and a cancelled
//! transition that brings it back restarts the timeline. A title shown only
//! on an overlay channel by a plain overlay take holds; one put there by a
//! title take plays its take's phases ([`crate::EngineTitleAir`]).
//!
//! Every pose is a pure function of the frame and the timeline's start, so
//! the timeline is the only state that must survive a restart: a snapshot
//! carries it ([`crate::EngineSnapshot::title_timelines`]) and a restored
//! engine resumes it ([`crate::Engine::restore_title_timelines`]), so a
//! checkpoint followed by journal replay continues every ticker exactly
//! where it was. The Out phase of a transition in flight is not saved; no
//! snapshot is taken while a transition plays.

use std::collections::BTreeMap;

use fm_model::fm_animation::{ClipPosition, ClipState};
use fm_model::{TitleShownOn, TitleTimeline};
use fm_switcher::{OverlayChannelId, ProgramFrame, SwitcherState};
use fm_titles::{TitleAir, TitleAirFrame, TitleAirPhase, TitleAirTiming};
use fm_types::{FrameRate, InputId};

use crate::ShowState;
use crate::scene_animation::{SceneRole, role};

/// One shown title input's runtime timeline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TitleShown {
    pub(crate) timeline: TitleTimeline,
    /// The frame a transition began taking the title off Program, while it
    /// still shows it.
    leaving_since: Option<u64>,
    /// Whether it is on Program or Preview (not only on an overlay).
    on_bus: bool,
}

impl TitleShown {
    const fn new(on: TitleShownOn, since_frame: u64) -> Self {
        Self {
            timeline: TitleTimeline { on, since_frame },
            leaving_since: None,
            on_bus: false,
        }
    }
}

/// One shown title input at one realized frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EngineTitleShown {
    pub input: InputId,
    /// Where it is shown and the frame its timeline started.
    pub timeline: TitleTimeline,
    /// Its In/Hold/Out life on Program or Preview, or `None` when it is
    /// shown only on an overlay channel (where a plain overlay take holds
    /// and a title take plays [`crate::EngineTitleAir`]). After an Out phase
    /// ends while a transition still shows the title, it holds the Out
    /// phase's last frame (`phase_frames` zero).
    pub bus: Option<TitleAirFrame>,
}

impl EngineTitleShown {
    /// Whether the title is mid In or Out phase on Program or Preview.
    #[must_use]
    pub fn in_motion(&self) -> bool {
        self.bus.is_some_and(|frame| frame.phase_frames != 0)
    }
}

/// Where `input` is shown on a realized frame, if at all, and its role on
/// Program or Preview.
fn shown_on(
    input: InputId,
    program: &ProgramFrame,
    switcher: &SwitcherState,
) -> Option<(TitleShownOn, Option<SceneRole>)> {
    let bus = role(input, program, switcher.preview());
    match bus {
        Some(SceneRole::Program | SceneRole::Leaving) => Some((TitleShownOn::Air, bus)),
        _ if switcher.overlay_channels_showing(input).next().is_some() => {
            Some((TitleShownOn::Air, None))
        }
        Some(SceneRole::Preview) => Some((TitleShownOn::Preview, bus)),
        None => None,
    }
}

/// Advances every title input's timeline to realized `frame`. Called once
/// per engine frame, after its actions are realized and before its poses
/// are sampled. `takes` holds every title take's animation; one whose In
/// phase starts on `frame` restarts its title's timeline.
pub(crate) fn step_titles_shown(
    shown: &mut BTreeMap<InputId, TitleShown>,
    state: &ShowState,
    program: &ProgramFrame,
    switcher: &SwitcherState,
    takes: &BTreeMap<InputId, (OverlayChannelId, TitleAir)>,
    frame: u64,
) {
    let mut next = BTreeMap::new();
    for &input in state.titles().keys() {
        let Some((on, bus)) = shown_on(input, program, switcher) else {
            continue;
        };
        let taken_now = takes
            .get(&input)
            .is_some_and(|(_, air)| !air.is_leaving() && air.start_frame() == frame);
        let mut updated = match shown.get(&input) {
            Some(current) if current.timeline.on == on && !taken_now => *current,
            _ => TitleShown::new(on, frame),
        };
        match bus {
            Some(SceneRole::Leaving) => {
                updated.leaving_since.get_or_insert(frame);
            }
            // A cancelled transition brings the title back: it enters again.
            Some(SceneRole::Program) if updated.leaving_since.is_some() => {
                updated = TitleShown::new(on, frame);
            }
            _ => updated.leaving_since = None,
        }
        updated.on_bus = bus.is_some();
        next.insert(input, updated);
    }
    *shown = next;
}

/// Every shown title input at `frame`, ascending by input.
pub(crate) fn shown_frames(
    shown: &BTreeMap<InputId, TitleShown>,
    state: &ShowState,
    frame: u64,
    rate: FrameRate,
) -> Vec<EngineTitleShown> {
    shown
        .iter()
        .map(|(&input, title)| {
            let timing = state
                .title(input)
                .map_or_else(TitleAirTiming::default, |title| title.timing);
            EngineTitleShown {
                input,
                timeline: title.timeline,
                bus: title.on_bus.then(|| bus_frame(title, timing, rate, frame)),
            }
        })
        .collect()
}

/// The pose of a title on Program or Preview at `frame`.
fn bus_frame(
    title: &TitleShown,
    timing: TitleAirTiming,
    rate: FrameRate,
    frame: u64,
) -> TitleAirFrame {
    let entered = TitleAir::take(timing, rate, title.timeline.since_frame);
    let air = title
        .leaving_since
        .map_or(entered, |leaving| entered.off_air(leaving));
    air.at(frame).unwrap_or(TitleAirFrame {
        // The Out phase ended while a transition still shows the title.
        phase: TitleAirPhase::Out,
        phase_frame: 0,
        phase_frames: 0,
        sample: ClipState::Out(ClipPosition::frame(timing.out_frames)),
    })
}

/// The runtime state a restored engine whose realized switcher is
/// `switcher` resumes `timelines` with.
pub(crate) fn restored(
    timelines: &BTreeMap<InputId, TitleTimeline>,
    switcher: &SwitcherState,
) -> BTreeMap<InputId, TitleShown> {
    let program = switcher.program_frame();
    timelines
        .iter()
        .map(|(&input, &timeline)| {
            (
                input,
                TitleShown {
                    timeline,
                    leaving_since: None,
                    on_bus: role(input, &program, switcher.preview()).is_some(),
                },
            )
        })
        .collect()
}

/// The saved form of `shown`.
pub(crate) fn saved(shown: &BTreeMap<InputId, TitleShown>) -> BTreeMap<InputId, TitleTimeline> {
    shown
        .iter()
        .map(|(&input, title)| (input, title.timeline))
        .collect()
}
