//! What a cue does when it fires (`RC-017`), worked out on this console
//! before GO.
//!
//! [`preview`] walks the chain a GO of one cue runs (its actions, across
//! its waits and delays, and every cue it auto-follows into) over a local
//! copy of the show ([`Show`]) and describes each step in the console
//! language: "Cut to Camera 2 (now Camera 1)", "Set Score Home to 3 (now
//! 2)", "Wait 2.0 s, then:". As it goes it keeps the copy current, so a
//! later step reads the values earlier steps leave, as the engine does.
//!
//! Each step is marked when this session lacks a permission it needs (the
//! same classes [`crate::cues::chain_block`] checks) and when the server
//! would refuse it as the show stands: a name the show lacks, a media or
//! title action on another kind of input, a title number update its field
//! cannot take (with copies of `fm-model`'s arithmetic, which Studio does
//! not link; tests hold them to the originals), a script that waits or
//! does not
//! parse, an empty Stinger slot, Preview set to the Program input, Program
//! taken twice on one frame, or a switcher step while a transition the
//! chain started (or the operator's T-bar) is still running. A step that
//! changes nothing is noted too. A refusal on the GO's own frame refuses
//! the GO; one after a wait stops the list with a fault.
//!
//! The preview is local and approximate where the show can move between
//! now and the step: values after a wait assume nothing else changes, and
//! after a script step or a playlist start (which can change anything, or
//! Program) the values it cannot know are left out rather than guessed.

use std::num::NonZeroU128;

use egui::{RichText, Ui};
use fm_protocol::{
    CueGain, CueMute, MAX_CUE_TITLE_NUMBER, ScriptStatus, StingerReadiness, TitleFieldStatus,
    TitleFieldValue, TransitionFavoriteEffect, WireCue, WireCueAction, WireCueMediaOperation,
    WireInputId, WireInputTarget, WireTargetBindings, WireTitleFieldId,
};
use fm_types::{FrameRate, InputId};
use fm_ui_model::{check_script_syntax, script_verbs, script_waits};

use crate::StudioUiState;
use crate::cues::{action_capability, verb_capability};
use crate::gating::Capability;
use crate::l10n::{Arg, Locale, Msg};
use crate::monitors::input_name;
use crate::theme::{AMBER, ERROR, caption, ellipsize, legible, muted_color, text_dim_color};

/// The fixed input of an action the walk resolved (see `Walk::action`).
fn resolved(target: WireInputTarget) -> InputId {
    match target.fixed() {
        Some(input) => input.to_domain(),
        None => unreachable!("the walk resolves a dynamic target before it reads it"),
    }
}

/// One title as the preview needs it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ShowTitle {
    pub(crate) input: InputId,
    pub(crate) name: String,
    pub(crate) fields: Vec<TitleFieldStatus>,
}

/// The show as this console last saw it, and the session's permissions:
/// everything [`preview`] reads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Show {
    pub(crate) inputs: Vec<InputId>,
    pub(crate) input_names: Vec<String>,
    pub(crate) program: InputId,
    pub(crate) preview: InputId,
    pub(crate) fade_to_black: bool,
    /// Each on-air overlay channel with its source.
    pub(crate) overlays: Vec<(u8, InputId)>,
    pub(crate) media: Vec<InputId>,
    /// Every paged input: slideshows (IN-009) and documents (IN-014).
    pub(crate) slideshows: Vec<InputId>,
    pub(crate) titles: Vec<ShowTitle>,
    /// Each audio strip: input, gain in millidecibels, muted.
    pub(crate) strips: Vec<(InputId, i32, bool)>,
    /// REC 1 and REC 2's desired state.
    pub(crate) recording: [bool; 2],
    /// Each stream destination: target, name, desired running.
    pub(crate) streams: Vec<(NonZeroU128, String, bool)>,
    /// Each configured Stinger slot with its readiness.
    pub(crate) stingers: Vec<(u8, StingerReadiness)>,
    pub(crate) scripts: Vec<ScriptStatus>,
    pub(crate) rate: FrameRate,
    /// An operator's T-bar transition is in flight.
    pub(crate) manual_in_flight: bool,
    /// A timed transition (or a scheduled take) the daemon accepted is
    /// still being realized on its frame loop (Protocol 2.86).
    pub(crate) timed_in_flight: bool,
    pub(crate) permissions: Permissions,
    /// The input selected on this console, which an action on the
    /// selected input acts on (RC-004).
    pub(crate) selected: Option<InputId>,
}

/// The session permissions a step may need beyond `transition`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Permissions {
    pub(crate) select_preview: bool,
    pub(crate) control_audio: bool,
    pub(crate) edit_project: bool,
}

impl Show {
    /// The show of `state`, once its view and project state are synced.
    pub(crate) fn of(state: &StudioUiState) -> Option<Self> {
        let (view, project) = (state.view.as_ref()?, state.project_state.as_ref()?);
        let gate = crate::TransitionGate::from_state(state);
        let mut recording = [false; 2];
        for status in &view.recordings {
            if let Some(slot) = recording.get_mut(usize::from(status.recorder.get()) - 1) {
                *slot = status.desired_active;
            }
        }
        Some(Self {
            inputs: view.inputs.clone(),
            input_names: view.input_names.clone(),
            program: view.switcher.desired.program,
            preview: view.switcher.desired.preview,
            fade_to_black: view.switcher.desired_fade_to_black.target_active,
            overlays: view
                .desired_overlays
                .iter()
                .filter(|overlay| overlay.active)
                .filter_map(|overlay| Some((overlay.channel, overlay.source?)))
                .collect(),
            media: project.media().keys().copied().collect(),
            slideshows: project
                .still_inputs()
                .iter()
                .filter(|(_, still)| {
                    matches!(still.source, fm_protocol::WireStillSource::Slideshow(_))
                })
                .map(|(input, _)| *input)
                // A document steps like a slideshow (IN-014).
                .chain(project.paged_statuses().map(|status| status.input.to_domain()))
                .collect(),
            titles: project
                .titles()
                .iter()
                .map(|title| ShowTitle {
                    input: title.input,
                    name: title.name.clone(),
                    fields: title.fields.clone(),
                })
                .collect(),
            strips: view
                .input_audio_strips
                .iter()
                .map(|strip| (strip.input, strip.gain_millidb, strip.muted))
                .collect(),
            recording,
            streams: view
                .streams
                .iter()
                .map(|stream| (stream.target, stream.name.clone(), stream.desired_running))
                .collect(),
            stingers: view
                .stingers
                .iter()
                .map(|stinger| (stinger.slot, stinger.readiness))
                .collect(),
            scripts: project.scripts().to_vec(),
            rate: project.frame_rate(),
            manual_in_flight: gate.manual_transition_in_flight,
            timed_in_flight: gate.transition_in_flight,
            permissions: Permissions {
                select_preview: state.can_select_preview,
                control_audio: state.can_control_audio,
                edit_project: state.can_edit_project,
            },
            selected: None,
        })
    }

    fn input(&self, l: Locale, input: InputId) -> String {
        input_name(l, &self.inputs, &self.input_names, input)
    }

    fn title(&self, input: InputId) -> Option<&ShowTitle> {
        self.titles.iter().find(|title| title.input == input)
    }

    /// The title input's name as the bank and the Titles tab lead with it;
    /// the template's name only when the input is not in the show.
    fn title_name(&self, l: Locale, input: InputId) -> String {
        if self.inputs.contains(&input) {
            return self.input(l, input);
        }
        self.title(input)
            .map(|title| title.name.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| self.input(l, input))
    }

    const fn permitted(&self, capability: Capability) -> bool {
        match capability {
            Capability::SelectPreview => self.permissions.select_preview,
            Capability::Audio => self.permissions.control_audio,
            Capability::EditProject => self.permissions.edit_project,
            Capability::Transition => true,
        }
    }
}

/// What a line describes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LineKind {
    /// One action.
    Action,
    /// A wait or delay: the actions below run on a later frame.
    Wait,
    /// The next cue, which the one above auto-follows into.
    Follow,
}

/// Something worth knowing about a step, worst first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Issue {
    /// The server would refuse it as the show stands.
    Refused(String),
    /// This session lacks a permission it needs.
    Denied(String),
    /// It may not do what it says (a Stinger that is not loaded).
    Caution(String),
    /// It changes nothing.
    NoChange(String),
}

/// One line of a preview.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Line {
    pub(crate) kind: LineKind,
    pub(crate) text: String,
    pub(crate) issues: Vec<Issue>,
}

/// What firing one cue does.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CuePreview {
    pub(crate) lines: Vec<Line>,
    /// A step on the GO's own frame would be refused: nothing runs.
    pub(crate) refuses_go: bool,
    /// A step after a wait would be refused: the list stops with a fault.
    pub(crate) faults: bool,
    /// A step needs a permission this session lacks.
    pub(crate) denied: bool,
    /// The input the chain's first take puts on Program (or, without a
    /// take, the input its first Preview step selects), if it is known.
    pub(crate) bus_input: Option<InputId>,
}

/// The local copy of the show the walk keeps current. `None` is a value
/// the walk cannot know (a script or the playlist may have changed it).
struct Sim {
    program: Option<InputId>,
    preview: Option<InputId>,
    fade_to_black: Option<bool>,
    overlays: Option<Vec<(u8, InputId)>>,
    /// Title field values the walk set, over the show's.
    fields: Vec<((InputId, WireTitleFieldId), Option<TitleFieldValue>)>,
    strips: Vec<(InputId, i32, bool)>,
    recording: [Option<bool>; 2],
    streams: Vec<(NonZeroU128, Option<bool>)>,
    /// A script ran: values it may have changed are unknown.
    scripted: bool,
    /// Frames since the GO's frame.
    frame: u64,
    /// Waits passed (zero on the GO's frame).
    segment: usize,
    /// Takes of Program on the current frame.
    takes: u32,
    /// The frame a transition the chain started ends on.
    transition_until: Option<u64>,
}

impl Sim {
    fn of(show: &Show) -> Self {
        Self {
            program: Some(show.program),
            preview: Some(show.preview),
            fade_to_black: Some(show.fade_to_black),
            overlays: Some(show.overlays.clone()),
            fields: Vec::new(),
            strips: show.strips.clone(),
            recording: show.recording.map(Some),
            streams: show
                .streams
                .iter()
                .map(|(target, _, running)| (*target, Some(*running)))
                .collect(),
            scripted: false,
            frame: 0,
            segment: 0,
            takes: 0,
            transition_until: None,
        }
    }

    /// Field `field` of title `input` as the walk left it.
    fn field(
        &self,
        show: &Show,
        input: InputId,
        field: WireTitleFieldId,
    ) -> Option<TitleFieldValue> {
        if let Some((_, value)) = self.fields.iter().find(|(key, _)| *key == (input, field)) {
            return value.clone();
        }
        if self.scripted {
            return None;
        }
        show.title(input)?
            .fields
            .iter()
            .find(|found| found.field == field)
            .map(|found| found.value.clone())
    }

    fn set_field(
        &mut self,
        input: InputId,
        field: WireTitleFieldId,
        value: Option<TitleFieldValue>,
    ) {
        self.fields.retain(|(key, _)| *key != (input, field));
        self.fields.push(((input, field), value));
    }

    /// A script step ran: anything it can touch is unknown from here.
    fn forget(&mut self) {
        self.scripted = true;
        self.program = None;
        self.preview = None;
        self.fade_to_black = None;
        self.overlays = None;
        for (_, value) in &mut self.fields {
            *value = None;
        }
        self.recording = [None; 2];
        for (_, running) in &mut self.streams {
            *running = None;
        }
    }
}

/// Upper bound on the lines a preview holds; a chain longer than this
/// ends in [`Msg::CuePvMore`].
const MAX_LINES: usize = 200;

/// What firing cue `cue` of `cues` (a saved list, in order) does on
/// `show`, in `l`'s words; `None` when the list lacks the cue.
#[must_use]
pub(crate) fn preview(l: Locale, show: &Show, cues: &[WireCue], cue: u64) -> Option<CuePreview> {
    let start = cues.iter().position(|found| found.id == cue)?;
    let mut walk = Walk {
        l,
        show,
        sim: Sim::of(show),
        out: CuePreview::default(),
        first_preview: None,
        first_take: None,
    };
    for (index, found) in cues[start..].iter().enumerate() {
        if index > 0 {
            let name = quoted(l, &found.name);
            walk.push(
                LineKind::Follow,
                l.format(Msg::CuePvFollow, &[("name", Arg::Str(&name))]),
                Vec::new(),
            );
        }
        for action in &found.actions {
            walk.action(action);
        }
        if !found.auto_follow {
            break;
        }
    }
    // A chain that ends on a wait holds its last cue until the wait
    // passes; say so rather than leave "then:" hanging.
    if walk
        .out
        .lines
        .last()
        .is_some_and(|line| line.kind == LineKind::Wait)
    {
        walk.push(
            LineKind::Action,
            l.text(Msg::CuePvCueEnds).to_owned(),
            Vec::new(),
        );
    }
    let mut out = walk.out;
    out.bus_input = walk.first_take.or(walk.first_preview).flatten();
    Some(out)
}

/// The field's value after setting (`add` false) or adding `value` to
/// `current`, or `None` past the bound: `fm_model::CueNumberUpdate::apply`.
fn number_update(add: bool, value: i64, current: i64) -> Option<i64> {
    let result = if add {
        current.checked_add(value)?
    } else {
        value
    };
    (result.unsigned_abs() <= MAX_CUE_TITLE_NUMBER.unsigned_abs()).then_some(result)
}

/// The integer a text field holds for a number update (blank is zero):
/// `fm_model::title_text_number`.
fn title_text_number(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return Some(0);
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    if digits.is_empty() || digits.len() > 19 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// A delay's whole frames at `rate`, rounded up, at least one:
/// `fm_model::delay_frames`.
fn delay_frames(millis: u32, rate: FrameRate) -> u32 {
    let numerator = u128::from(millis) * u128::from(rate.numerator());
    let denominator = u128::from(rate.denominator()) * 1_000;
    u32::try_from(numerator.div_ceil(denominator))
        .unwrap_or(u32::MAX)
        .max(1)
}

/// `text` in the language's quotation marks.
fn quoted(l: Locale, text: &str) -> String {
    l.format(Msg::CuePvQuoted, &[("text", Arg::Str(text))])
}

/// A title field's value as an operator reads it.
fn value_text(l: Locale, value: &TitleFieldValue) -> String {
    match value {
        TitleFieldValue::Number(number) => l.signed_integer(*number),
        TitleFieldValue::Text(text) => quoted(l, &ellipsize(text, 32)),
        TitleFieldValue::Color { .. } | TitleFieldValue::Image { .. } => String::new(),
    }
}

/// `+n` or `-n`, grouped.
fn step_text(l: Locale, step: i64) -> String {
    let number = l.signed_integer(step);
    if step > 0 {
        format!("+{number}")
    } else {
        number
    }
}

/// A frame count's duration at `rate`, as seconds with one decimal.
fn frames_time(l: Locale, frames: u32, rate: FrameRate) -> String {
    let millis = u64::from(frames) * 1_000 * u64::from(rate.denominator())
        / u64::from(rate.numerator().max(1));
    l.seconds_tenths(millis)
}

/// Milliseconds as an operator reads them: `500 ms`, `2.0 s`.
fn millis_time(l: Locale, millis: u32) -> String {
    if millis < 1_000 {
        format!(
            "{} {}",
            l.integer(u64::from(millis)),
            l.text(Msg::CueMillisUnit)
        )
    } else {
        l.seconds_tenths(u64::from(millis))
    }
}

// `Some(None)` is a step whose input the walk cannot know.
#[allow(clippy::option_option)]
struct Walk<'a> {
    l: Locale,
    show: &'a Show,
    sim: Sim,
    out: CuePreview,
    /// The first Preview step's input (`Some(None)`: not known).
    first_preview: Option<Option<InputId>>,
    /// The first take's input (`Some(None)`: not known).
    first_take: Option<Option<InputId>>,
}

impl Walk<'_> {
    fn push(&mut self, kind: LineKind, text: String, issues: Vec<Issue>) {
        for issue in &issues {
            match issue {
                Issue::Refused(_) if self.sim.segment == 0 => self.out.refuses_go = true,
                Issue::Refused(_) => self.out.faults = true,
                Issue::Denied(_) => self.out.denied = true,
                Issue::Caution(_) | Issue::NoChange(_) => {}
            }
        }
        if self.out.lines.len() < MAX_LINES {
            self.out.lines.push(Line { kind, text, issues });
        }
    }

    /// `text` with the current value `now` in brackets, when it is known.
    fn with_now(&self, text: String, now: Option<String>) -> String {
        match now {
            Some(value) if !value.is_empty() => {
                let now = self.l.format(Msg::CuePvNow, &[("value", Arg::Str(&value))]);
                format!("{text} {now}")
            }
            _ => text,
        }
    }

    fn refused(&self, reason: &str) -> Issue {
        Issue::Refused(self.l.format(
            Msg::CuePvRefused,
            &[("reason", Arg::Str(self.l.tr(reason)))],
        ))
    }

    fn refused_text(&self, reason: &str) -> Issue {
        Issue::Refused(
            self.l
                .format(Msg::CuePvRefused, &[("reason", Arg::Str(reason))]),
        )
    }

    fn no_change(&self, msg: Msg) -> Issue {
        Issue::NoChange(self.l.text(msg).to_owned())
    }

    /// The permission issue of `action`, if this session lacks it.
    fn denied(&self, action: &WireCueAction) -> Option<Issue> {
        let needs: Vec<Capability> = match action {
            WireCueAction::RunScript { script } => self
                .show
                .scripts
                .iter()
                .find(|found| found.id == *script)
                .map(|found| script_verbs(&found.spec.source))
                .unwrap_or_default()
                .into_iter()
                .filter_map(verb_capability)
                .collect(),
            action => action_capability(action).into_iter().collect(),
        };
        let lacking = needs.into_iter().find(|need| !self.show.permitted(*need))?;
        let reason = match lacking {
            Capability::SelectPreview => Msg::ReasonNoSelectPreview,
            Capability::Audio => Msg::ReasonNoControlAudio,
            Capability::EditProject | Capability::Transition => Msg::ReasonNoEditProject,
        };
        Some(Issue::Denied(self.l.format(
            Msg::CuePvDenied,
            &[("reason", Arg::Str(self.l.text(reason)))],
        )))
    }

    /// The refusal of an input the show lacks, or of a media or title
    /// action naming another kind of input (the engine's reference check).
    fn reference(&self, action: &WireCueAction) -> Option<Issue> {
        let input = match action {
            WireCueAction::Preview { input }
            | WireCueAction::Take {
                input: Some(input), ..
            }
            | WireCueAction::OverlayOn { input, .. }
            | WireCueAction::TitleOn { input, .. }
            | WireCueAction::TitleText { input, .. }
            | WireCueAction::TitleNumber { input, .. }
            | WireCueAction::TitleOff { input }
            | WireCueAction::AudioGain { input, .. }
            | WireCueAction::AudioMute { input, .. }
            | WireCueAction::Media { input, .. }
            | WireCueAction::Page { input, .. } => resolved(*input),
            _ => return None,
        };
        if !self.show.inputs.contains(&input) {
            return Some(self.refused(Msg::CuePvRefMissingInput.en()));
        }
        if matches!(action, WireCueAction::Media { .. }) && !self.show.media.contains(&input) {
            return Some(self.refused(Msg::CuePvRefNotMedia.en()));
        }
        if matches!(action, WireCueAction::Page { .. }) && !self.show.slideshows.contains(&input) {
            return Some(self.refused(Msg::CuePvRefNotSlideshow.en()));
        }
        let title = matches!(
            action,
            WireCueAction::TitleOn { .. }
                | WireCueAction::TitleText { .. }
                | WireCueAction::TitleNumber { .. }
                | WireCueAction::TitleOff { .. }
        );
        (title && self.show.title(input).is_none())
            .then(|| self.refused(Msg::CuePvRefNotTitle.en()))
    }

    /// The refusal of a switcher step (Preview, a take, a playlist start)
    /// while a transition is running, if one is.
    fn switcher_busy(&self) -> Option<Issue> {
        if self.sim.segment == 0 && self.show.manual_in_flight {
            return Some(self.refused(Msg::ReasonManualInFlight.en()));
        }
        if self.sim.segment == 0 && self.show.timed_in_flight {
            return Some(self.refused(Msg::CuePvRefTimedInFlight.en()));
        }
        self.sim
            .transition_until
            .is_some_and(|until| self.sim.frame < until)
            .then(|| self.refused(Msg::CuePvRefInFlight.en()))
    }

    /// Counts a take of Program on this frame; the second is refused
    /// (unless the step is refused already: the engine stops at the
    /// first refusal).
    fn take(&mut self, issues: &mut Vec<Issue>) {
        self.sim.takes += 1;
        if self.sim.takes > 1
            && !issues
                .iter()
                .any(|issue| matches!(issue, Issue::Refused(_)))
        {
            issues.push(self.refused(Msg::CuePvRefTwice.en()));
        }
    }

    /// Moves the walk past a wait of `frames`.
    fn wait(&mut self, frames: u32) {
        self.sim.frame += u64::from(frames);
        self.sim.segment += 1;
        self.sim.takes = 0;
    }

    /// Walks one action, its dynamic target (RC-004) bound as a GO binds
    /// it: to the show's Preview and Program when the GO fires, and to the
    /// input selected on this console.
    fn action(&mut self, action: &WireCueAction) {
        let Some(target) = action.target().filter(|target| target.is_dynamic()) else {
            self.fixed_action(action);
            return;
        };
        let bindings = WireTargetBindings {
            preview: Some(WireInputId::from_domain(self.show.preview)),
            program: Some(WireInputId::from_domain(self.show.program)),
            selected: self.show.selected.map(WireInputId::from_domain),
        };
        if let Some(input) = target.resolve(&bindings) {
            let resolved = action.clone().with_target(WireInputTarget::Fixed(input));
            self.fixed_action(&resolved);
        } else {
            let kind = self.l.tr(crate::cues::ActionKind::of(action).label());
            let text = self.l.format(
                Msg::CuePvUnboundTarget,
                &[
                    ("action", Arg::Str(kind)),
                    (
                        "target",
                        Arg::Str(&crate::cues::target_name(self.l, target)),
                    ),
                ],
            );
            let issue = self.refused(Msg::CuePvRefNoSelection.en());
            self.push(LineKind::Action, text, vec![issue]);
        }
    }

    #[allow(clippy::too_many_lines)] // one arm per action kind
    fn fixed_action(&mut self, action: &WireCueAction) {
        let l = self.l;
        let show = self.show;
        let mut issues = Vec::new();
        if let Some(issue) = self.reference(action) {
            issues.push(issue);
        }
        let bad_reference = !issues.is_empty();
        if let Some(issue) = self.denied(action) {
            issues.push(issue);
        }
        let name = |input: &WireInputTarget| show.input(l, resolved(*input));
        let (kind, text) = match action {
            WireCueAction::Preview { input } => {
                let target = resolved(*input);
                let text = l.format(Msg::CuePvPreview, &[("input", Arg::Str(&name(input)))]);
                let now = self.sim.preview.map(|now| show.input(l, now));
                if !bad_reference {
                    if let Some(issue) = self.switcher_busy() {
                        issues.push(issue);
                    } else if self.sim.program == Some(target) {
                        issues.push(self.refused(Msg::CuePvRefPreviewIsProgram.en()));
                    } else if self.sim.preview == Some(target) {
                        issues.push(self.no_change(Msg::CuePvAlreadyPreview));
                    }
                }
                self.first_preview.get_or_insert(Some(target));
                if self.sim.program != Some(target) {
                    self.sim.preview = Some(target);
                }
                (LineKind::Action, self.with_now(text, now))
            }
            WireCueAction::Take {
                input,
                effect,
                duration_frames,
            } => {
                let target = match input {
                    Some(input) => Some(resolved(*input)),
                    None => self.sim.preview,
                };
                let target_name = target.map_or_else(
                    || l.text(Msg::CuePvPreviewInput).to_owned(),
                    |input| show.input(l, input),
                );
                let text = match effect {
                    TransitionFavoriteEffect::Cut => {
                        l.format(Msg::CuePvCut, &[("input", Arg::Str(&target_name))])
                    }
                    _ => l.format(
                        Msg::CuePvMix,
                        &[
                            (
                                "effect",
                                Arg::Str(&crate::transitions::effect_name(l, *effect)),
                            ),
                            ("input", Arg::Str(&target_name)),
                            ("frames", Arg::from(*duration_frames)),
                        ],
                    ),
                };
                let now = self.sim.program.map(|now| show.input(l, now));
                // A take of the Program input does nothing (the engine
                // plans no command for it).
                if input.is_some() && target.is_some() && target == self.sim.program {
                    if !bad_reference {
                        issues.push(self.no_change(Msg::CuePvAlreadyProgram));
                    }
                } else {
                    if !bad_reference {
                        if let Some(issue) = self.switcher_busy() {
                            issues.push(issue);
                        }
                        if let TransitionFavoriteEffect::Stinger(slot) = effect {
                            let slot = slot.number();
                            let slot_arg = [("slot", Arg::from(u32::from(slot)))];
                            match show.stingers.iter().find(|(found, _)| *found == slot) {
                                None => issues.push(
                                    self.refused_text(&l.format(Msg::CuePvRefNoStinger, &slot_arg)),
                                ),
                                Some((_, StingerReadiness::Ready)) => {}
                                Some(_) => issues.push(Issue::Caution(
                                    l.format(Msg::CuePvStingerNotReady, &slot_arg),
                                )),
                            }
                        }
                    }
                    self.take(&mut issues);
                    self.first_take.get_or_insert(target);
                    if *effect != TransitionFavoriteEffect::Cut {
                        self.sim.transition_until =
                            Some(self.sim.frame + u64::from(*duration_frames));
                    }
                    let previous = self.sim.program;
                    self.sim.program = target;
                    self.sim.preview = previous;
                }
                (LineKind::Action, self.with_now(text, now))
            }
            WireCueAction::OverlayOn { channel, input } => {
                let number = channel.number();
                let text = l.format(
                    Msg::CuePvOverlayOn,
                    &[
                        ("channel", Arg::from(u32::from(number))),
                        ("input", Arg::Str(&name(input))),
                    ],
                );
                let now = self.overlay_now(number);
                let target = resolved(*input);
                if !bad_reference && self.overlay_source(number) == Some(Some(target)) {
                    issues.push(self.no_change(Msg::CuePvAlreadyState));
                }
                self.set_overlay(number, Some(target));
                (LineKind::Action, self.with_now(text, now))
            }
            WireCueAction::OverlayOff { channel } => {
                let number = channel.number();
                let text = l.format(
                    Msg::CuePvOverlayOff,
                    &[("channel", Arg::from(u32::from(number)))],
                );
                let now = self.overlay_now(number);
                if self.overlay_source(number) == Some(None) {
                    issues.push(self.no_change(Msg::CuePvAlreadyState));
                }
                self.set_overlay(number, None);
                (LineKind::Action, self.with_now(text, now))
            }
            WireCueAction::TitleOn { input, channel } => {
                let number = channel.number();
                let text = l.format(
                    Msg::CuePvTitleOn,
                    &[
                        ("channel", Arg::from(u32::from(number))),
                        ("title", Arg::Str(&show.title_name(l, resolved(*input)))),
                    ],
                );
                let now = self.overlay_now(number);
                self.set_overlay(number, Some(resolved(*input)));
                (LineKind::Action, self.with_now(text, now))
            }
            WireCueAction::TitleOff { input } => {
                let text = l.format(
                    Msg::CuePvTitleOff,
                    &[("title", Arg::Str(&show.title_name(l, resolved(*input))))],
                );
                let target = resolved(*input);
                if let Some(overlays) = &mut self.sim.overlays {
                    overlays.retain(|(_, source)| *source != target);
                }
                (LineKind::Action, text)
            }
            WireCueAction::TitleText { input, field, text } => {
                let target = resolved(*input);
                let line = self.title_line(
                    target,
                    *field,
                    Msg::CuePvTitleSet,
                    &[("value", quoted(l, &ellipsize(text, 32)))],
                );
                let now = self
                    .sim
                    .field(show, target, *field)
                    .map(|value| value_text(l, &value));
                if !bad_reference
                    && show
                        .title(target)
                        .is_some_and(|title| title.fields.iter().all(|found| found.field != *field))
                {
                    issues.push(self.refused(Msg::TitleRejUnknownField.en()));
                }
                self.sim
                    .set_field(target, *field, Some(TitleFieldValue::Text(text.clone())));
                (LineKind::Action, self.with_now(line, now))
            }
            WireCueAction::TitleNumber {
                input,
                field,
                add,
                value,
            } => {
                let target = resolved(*input);
                let (line, now) =
                    self.title_number(target, *field, *add, *value, bad_reference, &mut issues);
                (LineKind::Action, self.with_now(line, now))
            }
            // Only a Web session is sent the hidden marker (Protocol
            // 2.104); Studio never sees one, so it reads as a value-less set.
            WireCueAction::TitleHidden { input, field, .. } => {
                let target = resolved(*input);
                let line = self.title_line(
                    target,
                    *field,
                    Msg::CuePvTitleSet,
                    &[("value", quoted(l, "…"))],
                );
                (LineKind::Action, line)
            }
            WireCueAction::FadeToBlack {
                active,
                duration_frames,
            } => {
                let msg = if *active {
                    Msg::CuePvFtbOn
                } else {
                    Msg::CuePvFtbOff
                };
                let text = l.format(msg, &[("frames", Arg::from(*duration_frames))]);
                if self.sim.fade_to_black == Some(*active) {
                    issues.push(self.no_change(Msg::CuePvAlreadyState));
                }
                self.sim.fade_to_black = Some(*active);
                (LineKind::Action, text)
            }
            WireCueAction::PlaylistStart => {
                if let Some(issue) = self.switcher_busy() {
                    issues.push(issue);
                }
                self.take(&mut issues);
                self.first_take.get_or_insert(None);
                self.sim.program = None;
                self.sim.preview = None;
                (LineKind::Action, l.text(Msg::CuePvPlaylistStart).to_owned())
            }
            WireCueAction::PlaylistStop => {
                (LineKind::Action, l.text(Msg::CuePvPlaylistStop).to_owned())
            }
            WireCueAction::AudioGain { input, gain: level } => {
                let target = resolved(*input);
                let gain = |millidb: i32| l.decibels(f64::from(millidb) / 1_000.0, -1_000.0);
                let input_name = name(input);
                let text = match *level {
                    CueGain::Set(value) => l.format(
                        Msg::CuePvGain,
                        &[
                            ("input", Arg::Str(&input_name)),
                            ("gain", Arg::Str(&gain(value))),
                        ],
                    ),
                    CueGain::Step(step) => l.format(
                        Msg::CuePvGainStep,
                        &[
                            ("input", Arg::Str(&input_name)),
                            ("gain", Arg::Str(&gain(step))),
                        ],
                    ),
                    CueGain::Fired { min, max } => l.format(
                        Msg::CuePvGainFired,
                        &[
                            ("input", Arg::Str(&input_name)),
                            ("min", Arg::Str(&gain(min))),
                            ("max", Arg::Str(&gain(max))),
                        ],
                    ),
                };
                let strip = self
                    .sim
                    .strips
                    .iter_mut()
                    .find(|(found, ..)| *found == target);
                let now = if let Some((_, current, _)) = strip {
                    let now = gain(*current);
                    // A control's value is known only when it fires.
                    let next = level.apply(*current, None).unwrap_or(*current);
                    let same = *current == next && !level.is_fired();
                    *current = next;
                    if same && !self.sim.scripted {
                        issues.push(self.no_change(Msg::CuePvAlreadyState));
                    }
                    (!self.sim.scripted).then_some(now)
                } else {
                    if !bad_reference {
                        issues.push(self.no_change(Msg::CuePvNoStrip));
                    }
                    None
                };
                (LineKind::Action, self.with_now(text, now))
            }
            WireCueAction::AudioMute { input, mute } => {
                let target = resolved(*input);
                let msg = match mute {
                    CueMute::Set(true) => Msg::CuePvMute,
                    CueMute::Set(false) => Msg::CuePvUnmute,
                    CueMute::Toggle => Msg::CuePvMuteToggle,
                };
                let text = l.format(msg, &[("input", Arg::Str(&name(input)))]);
                let scripted = self.sim.scripted;
                match self
                    .sim
                    .strips
                    .iter_mut()
                    .find(|(found, ..)| *found == target)
                {
                    Some((_, _, current)) => {
                        let next = mute.apply(*current);
                        let same = *current == next;
                        *current = next;
                        if same && !scripted {
                            issues.push(self.no_change(Msg::CuePvAlreadyState));
                        }
                    }
                    None if !bad_reference => issues.push(self.no_change(Msg::CuePvNoStrip)),
                    None => {}
                }
                (LineKind::Action, text)
            }
            WireCueAction::Record { recorder, start } => {
                let slot = recorder.get();
                let msg = if *start {
                    Msg::CuePvRecordStart
                } else {
                    Msg::CuePvRecordStop
                };
                let text = l.format(msg, &[("slot", Arg::from(u32::from(slot)))]);
                if let Some(current) = self.sim.recording.get_mut(usize::from(slot) - 1) {
                    let same = *current == Some(*start);
                    *current = Some(*start);
                    if same {
                        issues.push(self.no_change(Msg::CuePvAlreadyState));
                    }
                }
                (LineKind::Action, text)
            }
            WireCueAction::Stream { target, start } => {
                let found = show
                    .streams
                    .iter()
                    .find(|(candidate, ..)| *candidate == target.get());
                let target_name = found.map_or_else(
                    || target.get().to_string(),
                    |(_, stream_name, _)| stream_name.clone(),
                );
                let msg = if *start {
                    Msg::CuePvStreamStart
                } else {
                    Msg::CuePvStreamStop
                };
                let text = l.format(msg, &[("target", Arg::Str(&target_name))]);
                if found.is_none() {
                    issues.push(self.refused(Msg::CuePvRefNoStream.en()));
                } else if let Some((_, running)) = self
                    .sim
                    .streams
                    .iter_mut()
                    .find(|(candidate, _)| *candidate == target.get())
                {
                    let same = *running == Some(*start);
                    *running = Some(*start);
                    if same {
                        issues.push(self.no_change(Msg::CuePvAlreadyState));
                    }
                }
                (LineKind::Action, text)
            }
            WireCueAction::Media { input, operation } => {
                let input_arg = [("input", Arg::Str(&name(input)))];
                let text = match operation {
                    WireCueMediaOperation::Play => l.format(Msg::CuePvPlay, &input_arg),
                    WireCueMediaOperation::Pause => l.format(Msg::CuePvPause, &input_arg),
                    WireCueMediaOperation::Restart => l.format(Msg::CuePvRestart, &input_arg),
                    WireCueMediaOperation::Seek { frame } => l.format(
                        Msg::CuePvSeek,
                        &[
                            ("input", Arg::Str(&name(input))),
                            ("frame", Arg::Plain(*frame)),
                        ],
                    ),
                };
                (LineKind::Action, text)
            }
            WireCueAction::Page { input, navigation } => {
                let input_arg = [("input", Arg::Str(&name(input)))];
                let text = match navigation {
                    fm_protocol::PagedNavigation::Next => l.format(Msg::CuePvSlideNext, &input_arg),
                    fm_protocol::PagedNavigation::Previous => {
                        l.format(Msg::CuePvSlidePrevious, &input_arg)
                    }
                    fm_protocol::PagedNavigation::First => {
                        l.format(Msg::CuePvSlideFirst, &input_arg)
                    }
                    fm_protocol::PagedNavigation::Last => l.format(Msg::CuePvSlideLast, &input_arg),
                    fm_protocol::PagedNavigation::Goto(page) => l.format(
                        Msg::CuePvSlideGoto,
                        &[
                            ("input", Arg::Str(&name(input))),
                            ("page", Arg::Plain(u64::from(*page) + 1)),
                        ],
                    ),
                };
                (LineKind::Action, text)
            }
            WireCueAction::Wait { frames } => {
                let time = frames_time(l, *frames, show.rate);
                let text = l.format(
                    Msg::CuePvWaitFrames,
                    &[("count", Arg::from(*frames)), ("time", Arg::Str(&time))],
                );
                self.push(LineKind::Wait, text, issues);
                self.wait(*frames);
                return;
            }
            WireCueAction::Delay { millis } => {
                let time = millis_time(l, *millis);
                let text = l.format(Msg::CuePvWaitTime, &[("time", Arg::Str(&time))]);
                self.push(LineKind::Wait, text, issues);
                self.wait(delay_frames(*millis, show.rate));
                return;
            }
            WireCueAction::RunScript { script } => {
                let found = show.scripts.iter().find(|found| found.id == *script);
                let script_name =
                    found.map_or_else(|| script.to_string(), |found| found.spec.name.clone());
                let text = l.format(
                    Msg::CuePvScript,
                    &[("name", Arg::Str(&quoted(l, &script_name)))],
                );
                match found {
                    None => issues.push(self.refused(Msg::CuePvRefNoScript.en())),
                    Some(found) if check_script_syntax(&found.spec.source).is_some() => {
                        issues.push(self.refused(Msg::CuePvRefScriptSyntax.en()));
                    }
                    Some(found) if script_waits(&found.spec.source) => {
                        issues.push(self.refused(Msg::CuePvRefScriptWaits.en()));
                    }
                    Some(_) => {}
                }
                self.sim.forget();
                (LineKind::Action, text)
            }
        };
        self.push(kind, text, issues);
    }

    /// The description of a set or step of a title number field, with its
    /// current value, and its refusal (`fm-model`'s own arithmetic).
    fn title_number(
        &mut self,
        target: InputId,
        field: WireTitleFieldId,
        add: bool,
        value: i64,
        bad_reference: bool,
        issues: &mut Vec<Issue>,
    ) -> (String, Option<String>) {
        let l = self.l;
        let show = self.show;
        let kind = show
            .title(target)
            .and_then(|title| title.fields.iter().find(|found| found.field == field))
            .map(|found| &found.value);
        let current = self.sim.field(show, target, field);
        let now = current.as_ref().map(|value| value_text(l, value));
        // The integer the field holds, `None` when it holds none, and
        // `Some(None)` when the walk cannot know it.
        let held = match &current {
            _ if !add => Some(Some(0)),
            Some(TitleFieldValue::Number(number)) => Some(Some(*number)),
            Some(TitleFieldValue::Text(text)) => title_text_number(text).map(Some),
            Some(_) => None,
            None => Some(None),
        };
        let mut result = None;
        if !bad_reference {
            match kind {
                None => issues.push(self.refused(Msg::TitleRejUnknownField.en())),
                Some(TitleFieldValue::Image { .. } | TitleFieldValue::Color { .. }) => {
                    issues.push(self.refused(Msg::CuePvRefImageField.en()));
                }
                Some(_) => match held {
                    None => issues.push(self.refused(Msg::CuePvRefNotWhole.en())),
                    Some(None) => {}
                    Some(Some(number)) => {
                        result = number_update(add, value, number);
                        if result.is_none() {
                            let max = l.integer(MAX_CUE_TITLE_NUMBER.unsigned_abs());
                            issues.push(self.refused_text(
                                &l.format(Msg::CuePvRefBound, &[("max", Arg::Str(&max))]),
                            ));
                        }
                    }
                },
            }
        }
        let text_field = matches!(kind, Some(TitleFieldValue::Text(_)));
        let next = result.map(|next| {
            if text_field {
                TitleFieldValue::Text(next.to_string())
            } else {
                TitleFieldValue::Number(next)
            }
        });
        let line = if add {
            let step = step_text(l, value);
            match &next {
                Some(next) => self.title_line(
                    target,
                    field,
                    Msg::CuePvTitleStep,
                    &[("step", step), ("value", value_text(l, next))],
                ),
                None => self.title_line(target, field, Msg::CuePvTitleStepBy, &[("step", step)]),
            }
        } else {
            let shown = next
                .as_ref()
                .map_or_else(|| l.signed_integer(value), |next| value_text(l, next));
            self.title_line(target, field, Msg::CuePvTitleSet, &[("value", shown)])
        };
        // A refused update leaves the field as it was.
        if next.is_some() || held == Some(None) {
            self.sim.set_field(target, field, next);
        }
        (line, now)
    }

    /// `msg` naming title `target`'s field `field`, with `extra` arguments.
    fn title_line(
        &self,
        target: InputId,
        field: WireTitleFieldId,
        msg: Msg,
        extra: &[(&str, String)],
    ) -> String {
        let l = self.l;
        let title = self.show.title_name(l, target);
        let field_name = self
            .show
            .title(target)
            .and_then(|title| title.fields.iter().find(|found| found.field == field))
            .map_or_else(|| field.get().to_string(), |found| found.name.clone());
        let mut args = vec![
            ("title", Arg::Str(&title)),
            ("field", Arg::Str(&field_name)),
        ];
        args.extend(extra.iter().map(|(name, value)| (*name, Arg::Str(value))));
        l.format(msg, &args)
    }

    /// The source on overlay `channel` as the walk left it: `Some(None)`
    /// when it is off air, `None` when the walk cannot know.
    #[allow(clippy::option_option)] // unknown, off air, or a source
    fn overlay_source(&self, channel: u8) -> Option<Option<InputId>> {
        let overlays = self.sim.overlays.as_ref()?;
        Some(
            overlays
                .iter()
                .find(|(found, _)| *found == channel)
                .map(|(_, source)| *source),
        )
    }

    /// What overlay `channel` shows now, in words.
    fn overlay_now(&self, channel: u8) -> Option<String> {
        self.overlay_source(channel).map(|source| {
            source.map_or_else(
                || self.l.text(Msg::CuePvOverlayEmpty).to_owned(),
                |input| self.show.input(self.l, input),
            )
        })
    }

    fn set_overlay(&mut self, channel: u8, source: Option<InputId>) {
        if let Some(overlays) = &mut self.sim.overlays {
            overlays.retain(|(found, _)| *found != channel);
            if let Some(source) = source {
                overlays.push((channel, source));
            }
        }
    }
}

/// The note under a preview that says what its marks mean for a GO, if
/// any step is marked.
#[must_use]
pub(crate) fn summary(l: Locale, preview: &CuePreview) -> Option<String> {
    let msg = if preview.denied {
        Msg::CuePvSummaryDenied
    } else if preview.refuses_go {
        Msg::CuePvSummaryRefused
    } else if preview.faults {
        Msg::CuePvSummaryFault
    } else {
        return None;
    };
    Some(l.text(msg).to_owned())
}

/// Draws `preview`'s lines (at most `max_lines`, then how many more) and
/// its summary.
pub(crate) fn draw_lines(ui: &mut Ui, l: Locale, preview: &CuePreview, max_lines: usize) {
    if preview.lines.is_empty() {
        ui.label(caption(l.text(Msg::CuePvEmpty)));
        return;
    }
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let size = legible(11.0);
        for line in preview.lines.iter().take(max_lines) {
            let (indent, text) = match line.kind {
                LineKind::Action => (12, RichText::new(&line.text).size(size)),
                LineKind::Wait | LineKind::Follow => (
                    0,
                    RichText::new(&line.text)
                        .size(size)
                        .strong()
                        .color(text_dim_color()),
                ),
            };
            indented(ui, indent, text);
            for issue in &line.issues {
                let (text, color) = match issue {
                    Issue::Refused(text) => (text, ERROR),
                    Issue::Denied(text) | Issue::Caution(text) => (text, AMBER),
                    Issue::NoChange(text) => (text, muted_color()),
                };
                indented(
                    ui,
                    indent + 14,
                    RichText::new(text).size(legible(10.5)).color(color),
                );
            }
        }
        let hidden = preview.lines.len().saturating_sub(max_lines);
        if hidden > 0 {
            ui.label(caption(
                l.format(Msg::CuePvMore, &[("count", Arg::from(hidden))]),
            ));
        }
        if let Some(note) = summary(l, preview) {
            let color = if preview.denied || preview.refuses_go {
                ERROR
            } else {
                AMBER
            };
            ui.label(
                RichText::new(note)
                    .size(legible(10.5))
                    .strong()
                    .color(color),
            );
        }
    });
}

/// One wrapping label, `indent` points in from the leading edge.
fn indented(ui: &mut Ui, indent: i8, text: RichText) {
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: indent,
            ..egui::Margin::ZERO
        })
        .show(ui, |ui| {
            ui.add(egui::Label::new(text).wrap());
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use fm_protocol::{
        AutoTransitionKind, ScriptSpec, WireOverlayChannelId, WireRecorderSlot, WireStingerSlotId,
        WireStreamTargetId,
    };

    fn id(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn wire(value: u128) -> WireInputId {
        WireInputId::from_domain(id(value))
    }

    fn field(value: u128) -> WireTitleFieldId {
        WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
    }

    const SCORE: u128 = 5;

    /// Cameras 1 to 3 (1 on Program, 2 on Preview), a clip on input 4, and
    /// a "Score" title on input 5 with Home (a number, 2), Away (text
    /// "7"), Team (text "Lions"), and a colour; REC 1 idle; one stream.
    fn show() -> Show {
        let status = |value: u128, name: &str, value_of: TitleFieldValue| TitleFieldStatus {
            field: field(value),
            name: name.into(),
            value: value_of,
            exposed: true,
            revision: 0,
        };
        Show {
            inputs: (1..=5).map(id).collect(),
            input_names: ["Camera 1", "Camera 2", "Camera 3", "Clip", "Score"]
                .map(String::from)
                .to_vec(),
            program: id(1),
            preview: id(2),
            fade_to_black: false,
            overlays: vec![(1, id(3))],
            media: vec![id(4)],
            slideshows: Vec::new(),
            titles: vec![ShowTitle {
                input: id(SCORE),
                name: "Score".into(),
                fields: vec![
                    status(1, "Home", TitleFieldValue::Number(2)),
                    status(2, "Away", TitleFieldValue::Text("7".into())),
                    status(3, "Team", TitleFieldValue::Text("Lions".into())),
                    status(
                        4,
                        "Tint",
                        TitleFieldValue::Color {
                            red: 1,
                            green: 2,
                            blue: 3,
                            alpha: 255,
                        },
                    ),
                ],
            }],
            strips: vec![(id(1), -6_000, false), (id(2), 0, true)],
            recording: [false, true],
            streams: vec![(NonZeroU128::new(9).unwrap(), "Main".into(), false)],
            stingers: vec![
                (1, StingerReadiness::Ready),
                (2, StingerReadiness::NotRequested),
            ],
            scripts: vec![
                script(1, "Intro", "preview 2\n"),
                script(2, "Slow", "wait 5\ncut\n"),
                script(3, "Mute", "mute 1\n"),
            ],
            rate: FrameRate::new(25, 1).unwrap(),
            manual_in_flight: false,
            timed_in_flight: false,
            permissions: Permissions {
                select_preview: true,
                control_audio: true,
                edit_project: true,
            },
            selected: None,
        }
    }

    fn script(id: u64, name: &str, source: &str) -> ScriptStatus {
        ScriptStatus {
            id,
            spec: ScriptSpec {
                name: name.into(),
                source: source.into(),
            },
            source_hidden: false,
        }
    }

    fn cue(id: u64, auto_follow: bool, actions: Vec<WireCueAction>) -> WireCue {
        WireCue {
            id,
            name: format!("Cue {id}"),
            auto_follow,
            actions,
        }
    }

    fn cut(input: u128) -> WireCueAction {
        WireCueAction::Take {
            input: Some(fm_types::InputTarget::Fixed(wire(input))),
            effect: TransitionFavoriteEffect::Cut,
            duration_frames: 0,
        }
    }

    fn number(field_id: u128, add: bool, value: i64) -> WireCueAction {
        WireCueAction::TitleNumber {
            input: fm_types::InputTarget::Fixed(wire(SCORE)),
            field: field(field_id),
            add,
            value,
        }
    }

    fn run(show: &Show, cues: &[WireCue]) -> CuePreview {
        preview(Locale::ENGLISH, show, cues, cues[0].id).unwrap()
    }

    fn texts(preview: &CuePreview) -> Vec<&str> {
        preview
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect()
    }

    fn refused(text: &str) -> Issue {
        Issue::Refused(format!("Would be refused: {text}"))
    }

    /// The copies of the engine's arithmetic agree with `fm-model`.
    #[test]
    fn number_and_delay_arithmetic_match_the_model() {
        let max = MAX_CUE_TITLE_NUMBER;
        for current in [0, 1, -1, 41, max, -max, max - 1, i64::MAX, i64::MIN] {
            for value in [1, -1, 10, max, -max, 2 * max] {
                for add in [false, true] {
                    let update = if add {
                        fm_model::CueNumberUpdate::Add(value)
                    } else {
                        fm_model::CueNumberUpdate::Set(value)
                    };
                    assert_eq!(number_update(add, value, current), update.apply(current));
                }
            }
        }
        for text in [
            "",
            "  ",
            "7",
            " -3 ",
            "-",
            "1.5",
            "Lions",
            "+4",
            "99999999999999999999",
        ] {
            assert_eq!(title_text_number(text), fm_model::title_text_number(text));
        }
        for rate in [(25, 1), (30_000, 1_001), (60, 1)] {
            let rate = FrameRate::new(rate.0, rate.1).unwrap();
            for millis in [1, 39, 40, 41, 1_000, 1_500, fm_protocol::MAX_CUE_DELAY_MS] {
                assert_eq!(
                    delay_frames(millis, rate),
                    fm_model::delay_frames(millis, rate)
                );
            }
        }
        for source in [
            "preview 2\n",
            "wait 5\ncut\n",
            "if program == 1\nwait 2\nend\n",
            "((",
        ] {
            assert_eq!(script_waits(source), fm_model::script_waits(source));
        }
        assert_eq!(MAX_CUE_TITLE_NUMBER, fm_model::MAX_CUE_TITLE_NUMBER);
    }

    #[test]
    fn steps_read_with_their_current_values_and_a_follow_chain() {
        let cues = [
            cue(
                1,
                true,
                vec![
                    cut(2),
                    number(1, false, 3),
                    WireCueAction::Record {
                        recorder: WireRecorderSlot::new(1).unwrap(),
                        start: true,
                    },
                    WireCueAction::Delay { millis: 2_000 },
                    WireCueAction::RunScript { script: 1 },
                ],
            ),
            cue(
                2,
                false,
                vec![WireCueAction::Take {
                    input: Some(fm_types::InputTarget::Fixed(wire(3))),
                    effect: TransitionFavoriteEffect::Mix(AutoTransitionKind::Fade),
                    duration_frames: 25,
                }],
            ),
            cue(3, false, vec![cut(1)]),
        ];
        let preview = run(&show(), &cues);
        assert_eq!(
            texts(&preview),
            [
                "Cut to Camera 2 (now Camera 1)",
                "Set Score Home to 3 (now 2)",
                "Start recording on REC 1",
                "Wait 2.0 s, then:",
                "Run script \"Intro\"",
                "Then cue \"Cue 2\" follows:",
                // A script may change Program, so its value is not shown.
                "Fade to Camera 3 over 25 frames",
            ]
        );
        assert_eq!(
            preview
                .lines
                .iter()
                .map(|line| line.kind)
                .collect::<Vec<_>>(),
            [
                LineKind::Action,
                LineKind::Action,
                LineKind::Action,
                LineKind::Wait,
                LineKind::Action,
                LineKind::Follow,
                LineKind::Action,
            ]
        );
        assert!(preview.lines.iter().all(|line| line.issues.is_empty()));
        assert!(!preview.refuses_go && !preview.faults && !preview.denied);
        assert_eq!(summary(Locale::ENGLISH, &preview), None);
        assert_eq!(preview.bus_input, Some(id(2)));
        // A cue the list lacks has no preview.
        assert_eq!(preview_of(&cues, 99), None);
    }

    fn preview_of(cues: &[WireCue], cue: u64) -> Option<CuePreview> {
        preview(Locale::ENGLISH, &show(), cues, cue)
    }

    #[test]
    fn number_updates_read_and_step_like_the_engine() {
        let cues = [cue(
            1,
            false,
            vec![
                number(1, true, 1),
                // Two steps of one field on one frame add both.
                number(1, true, 1),
                // A text field holding a whole number steps as text.
                number(2, true, -10),
                number(3, true, 1),
                number(4, false, 1),
                number(99, false, 1),
                number(1, false, MAX_CUE_TITLE_NUMBER),
                number(1, true, 1),
            ],
        )];
        let preview = run(&show(), &cues);
        assert_eq!(
            texts(&preview),
            [
                "Step Score Home by +1 to 3 (now 2)",
                "Step Score Home by +1 to 4 (now 3)",
                "Step Score Away by -10 to \"-3\" (now \"7\")",
                "Step Score Team by +1 (now \"Lions\")",
                "Set Score Tint to 1",
                "Set Score 99 to 1",
                "Set Score Home to 999,999,999,999 (now 4)",
                "Step Score Home by +1 (now 999,999,999,999)",
            ]
        );
        let issues: Vec<_> = preview
            .lines
            .iter()
            .map(|line| line.issues.clone())
            .collect();
        assert_eq!(issues[..3], [vec![], vec![], vec![]]);
        assert_eq!(
            issues[3],
            [refused("the field does not hold a whole number")]
        );
        assert_eq!(
            issues[4],
            [refused(
                "that field holds an image or a colour, not a number"
            )]
        );
        assert_eq!(issues[5], [refused("the title has no such field")]);
        assert_eq!(issues[6], []);
        assert_eq!(
            issues[7],
            [refused("the result would pass 999,999,999,999 either way")]
        );
        assert!(preview.refuses_go);
        assert_eq!(
            summary(Locale::ENGLISH, &preview).as_deref(),
            Some(Msg::CuePvSummaryRefused.en())
        );
    }

    #[test]
    fn references_the_engine_refuses_are_marked() {
        let cues = [cue(
            1,
            false,
            vec![
                WireCueAction::Preview {
                    input: fm_types::InputTarget::Fixed(wire(9)),
                },
                WireCueAction::Media {
                    input: fm_types::InputTarget::Fixed(wire(1)),
                    operation: WireCueMediaOperation::Play,
                },
                WireCueAction::TitleOff {
                    input: fm_types::InputTarget::Fixed(wire(2)),
                },
                WireCueAction::Stream {
                    target: WireStreamTargetId::new(NonZeroU128::new(8).unwrap()),
                    start: true,
                },
                WireCueAction::RunScript { script: 7 },
                WireCueAction::RunScript { script: 2 },
                WireCueAction::RunScript { script: 3 },
            ],
        )];
        let mut lacking = show();
        lacking.permissions.control_audio = false;
        let preview = run(&lacking, &cues);
        let issues: Vec<_> = preview
            .lines
            .iter()
            .map(|line| line.issues.clone())
            .collect();
        assert_eq!(issues[0], [refused("the show has no such input")]);
        assert_eq!(issues[1], [refused("it is not a media input")]);
        assert_eq!(issues[2], [refused("it is not a title")]);
        assert_eq!(
            issues[3],
            [refused("the show has no such stream destination")]
        );
        assert_eq!(issues[4], [refused("the project has no such script")]);
        assert_eq!(
            issues[5],
            [refused(
                "the script holds a wait, which a cue step cannot run"
            )]
        );
        // A script whose verbs need a permission the session lacks.
        assert_eq!(
            issues[6],
            [Issue::Denied(
                "Not permitted: this session lacks the control_audio permission".into()
            )]
        );
        assert!(preview.refuses_go && preview.denied);
        assert_eq!(
            summary(Locale::ENGLISH, &preview).as_deref(),
            Some(Msg::CuePvSummaryDenied.en())
        );
    }

    #[test]
    fn switcher_steps_follow_the_buses_and_running_transitions() {
        let fade = |input: u128, frames: u32| WireCueAction::Take {
            input: Some(fm_types::InputTarget::Fixed(wire(input))),
            effect: TransitionFavoriteEffect::Mix(AutoTransitionKind::Fade),
            duration_frames: frames,
        };
        let cues = [cue(
            1,
            false,
            vec![
                // Program is Camera 1: a take of it does nothing, and
                // Preview may not show it.
                cut(1),
                WireCueAction::Preview {
                    input: fm_types::InputTarget::Fixed(wire(1)),
                },
                WireCueAction::Preview {
                    input: fm_types::InputTarget::Fixed(wire(2)),
                },
                fade(3, 25),
                // A second take on the frame.
                WireCueAction::PlaylistStart,
                WireCueAction::Wait { frames: 10 },
                // The fade still runs 15 more frames.
                cut(2),
                WireCueAction::Wait { frames: 15 },
                cut(3),
            ],
        )];
        let preview = run(&show(), &cues);
        assert_eq!(
            texts(&preview),
            [
                "Cut to Camera 1 (now Camera 1)",
                "Put Camera 1 on Preview (now Camera 2)",
                "Put Camera 2 on Preview (now Camera 2)",
                "Fade to Camera 3 over 25 frames (now Camera 1)",
                "Start the playlist (it takes Program)",
                "Wait 10 frames (0.4 s), then:",
                "Cut to Camera 2",
                "Wait 15 frames (0.6 s), then:",
                "Cut to Camera 3 (now Camera 2)",
            ]
        );
        let issues: Vec<_> = preview
            .lines
            .iter()
            .map(|line| line.issues.clone())
            .collect();
        assert_eq!(
            issues[0],
            [Issue::NoChange(Msg::CuePvAlreadyProgram.en().into())]
        );
        assert_eq!(
            issues[1],
            [refused(
                "that input is on Program, and Preview and Program must differ"
            )]
        );
        assert_eq!(
            issues[2],
            [Issue::NoChange(Msg::CuePvAlreadyPreview.en().into())]
        );
        assert_eq!(issues[3], []);
        // Refused once: the fade still runs (the second take is not
        // reported over it).
        assert_eq!(
            issues[4],
            [refused(
                "the transition started earlier is still running then"
            )]
        );
        assert_eq!(
            issues[6],
            [refused(
                "the transition started earlier is still running then"
            )]
        );
        assert_eq!(issues[8], []);
        assert!(preview.refuses_go && preview.faults);
        // The first take that changes Program names the shot.
        assert_eq!(preview.bus_input, Some(id(3)));

        // An operator's T-bar refuses the GO frame's switcher steps.
        let mut busy = show();
        busy.manual_in_flight = true;
        let preview = run(&busy, &[cue(1, false, vec![cut(3)])]);
        assert_eq!(
            preview.lines[0].issues,
            [refused("a manual T-bar transition is in progress")]
        );

        // A cue that follows into another without a wait shares its frame,
        // so two cuts take Program twice on it.
        let cues = [cue(1, true, vec![cut(2)]), cue(2, false, vec![cut(3)])];
        let preview = run(&show(), &cues);
        assert_eq!(
            preview.lines[2].issues,
            [refused("Program would be taken twice on one frame")]
        );
        assert!(preview.refuses_go);
    }

    /// A timed transition the daemon is still realizing refuses the GO
    /// frame's switcher steps, as an operator's T-bar does, in English and
    /// Spanish; the cue's later segments run after it ends.
    #[test]
    fn a_timed_transition_in_flight_refuses_the_go_frame() {
        let mut busy = show();
        busy.timed_in_flight = true;
        let preview = run(
            &busy,
            &[cue(
                1,
                false,
                vec![cut(3), WireCueAction::Wait { frames: 40 }, cut(2)],
            )],
        );
        assert_eq!(
            preview.lines[0].issues,
            [refused(
                "a timed transition is still running on Program; GO now would be refused until it ends"
            )]
        );
        assert!(preview.refuses_go);
        assert_eq!(preview.lines[2].issues, []);
        let spanish = Locale::new(fm_ui_model::i18n::Language::Spanish);
        let es = super::preview(spanish, &busy, &[cue(1, false, vec![cut(3)])], 1).unwrap();
        assert!(
            es.lines[0].issues.iter().any(|issue| matches!(
                issue,
                Issue::Refused(text) if text.contains("una transición temporizada sigue en curso")
            )),
            "{:?}",
            es.lines[0].issues
        );
    }

    #[test]
    fn stingers_overlays_audio_and_outputs_say_when_they_change_nothing() {
        let channel = |number| WireOverlayChannelId::new(number).unwrap();
        let stinger = |slot| WireCueAction::Take {
            input: None,
            effect: TransitionFavoriteEffect::Stinger(WireStingerSlotId::new(slot).unwrap()),
            duration_frames: 30,
        };
        let cues = [cue(
            1,
            false,
            vec![
                stinger(3),
                WireCueAction::Wait { frames: 40 },
                stinger(2),
                WireCueAction::OverlayOn {
                    channel: channel(1),
                    input: fm_types::InputTarget::Fixed(wire(3)),
                },
                WireCueAction::OverlayOff {
                    channel: channel(2),
                },
                WireCueAction::AudioGain {
                    input: fm_types::InputTarget::Fixed(wire(1)),
                    gain: CueGain::Set(-6_000),
                },
                WireCueAction::AudioGain {
                    input: fm_types::InputTarget::Fixed(wire(3)),
                    gain: CueGain::Set(0),
                },
                WireCueAction::AudioMute {
                    input: fm_types::InputTarget::Fixed(wire(2)),
                    mute: CueMute::Set(false),
                },
                WireCueAction::Record {
                    recorder: WireRecorderSlot::new(2).unwrap(),
                    start: true,
                },
                WireCueAction::FadeToBlack {
                    active: true,
                    duration_frames: 25,
                },
            ],
        )];
        let preview = run(&show(), &cues);
        assert_eq!(
            texts(&preview),
            [
                "Stinger S3 to Camera 2 over 30 frames (now Camera 1)",
                "Wait 40 frames (1.6 s), then:",
                "Stinger S2 to Camera 1 over 30 frames (now Camera 2)",
                "Overlay 1: show Camera 3 (now Camera 3)",
                "Overlay 2: take off air (now nothing on it)",
                "Set the Camera 1 fader to -6.0 dB (now -6.0 dB)",
                "Set the Camera 3 fader to +0.0 dB",
                "Unmute Camera 2",
                "Start recording on REC 2",
                "Fade to black over 25 frames",
            ]
        );
        let issues: Vec<_> = preview
            .lines
            .iter()
            .map(|line| line.issues.clone())
            .collect();
        assert_eq!(issues[0], [refused("Stinger slot 3 holds no stinger")]);
        assert_eq!(
            issues[2],
            [Issue::Caution(
                "Stinger slot 2 is not loaded yet, so the take may fall back or be refused".into()
            )]
        );
        let unchanged = Issue::NoChange(Msg::CuePvAlreadyState.en().into());
        for unchanged_step in &issues[3..=5] {
            assert_eq!(*unchanged_step, vec![unchanged.clone()]);
        }
        assert_eq!(issues[6], [Issue::NoChange(Msg::CuePvNoStrip.en().into())]);
        assert_eq!(issues[7], []);
        assert_eq!(issues[8], [unchanged]);
        assert_eq!(issues[9], []);
        assert!(preview.refuses_go && !preview.faults);
    }

    #[test]
    fn permissions_mark_the_steps_this_session_may_not_run() {
        let mut viewer = show();
        viewer.permissions.select_preview = false;
        viewer.permissions.edit_project = false;
        let cues = [cue(
            1,
            false,
            vec![
                WireCueAction::Preview {
                    input: fm_types::InputTarget::Fixed(wire(3)),
                },
                number(1, true, 1),
                cut(3),
            ],
        )];
        let preview = run(&viewer, &cues);
        assert_eq!(
            preview.lines[0].issues,
            [Issue::Denied(
                "Not permitted: this session lacks the select_preview permission".into()
            )]
        );
        assert_eq!(
            preview.lines[1].issues,
            [Issue::Denied(
                "Not permitted: this session lacks the edit_project permission".into()
            )]
        );
        assert_eq!(preview.lines[2].issues, []);
        assert!(preview.denied && !preview.refuses_go);
        // Without a take, the first Preview step names the shot.
        assert_eq!(preview.bus_input, Some(id(3)));
    }

    #[test]
    fn spanish_reads_the_same_steps_in_spanish() {
        let cues = [cue(
            1,
            false,
            vec![
                cut(2),
                number(1, false, 3),
                WireCueAction::Delay { millis: 500 },
            ],
        )];
        let preview = preview(
            Locale::new(fm_ui_model::i18n::Language::Spanish),
            &show(),
            &cues,
            1,
        )
        .unwrap();
        assert_eq!(preview.lines[0].text, "Corte a Camera 2 (ahora Camera 1)");
        assert_eq!(preview.lines[1].text, "Poner Score Home a 3 (ahora 2)");
        assert_eq!(preview.lines[2].text, "Esperar 500 ms y después:");
    }

    /// RC-004: a dynamic target reads as the input a GO from this console
    /// would bind — Preview and Program as they stand at GO, and this
    /// console's selection — and with nothing selected, an action on the
    /// selected input would be refused.
    #[test]
    fn dynamic_targets_read_as_the_inputs_a_go_binds() {
        let cues = [cue(
            1,
            false,
            vec![
                WireCueAction::Preview {
                    input: WireInputTarget::Program,
                },
                WireCueAction::Take {
                    input: Some(WireInputTarget::Selected),
                    effect: TransitionFavoriteEffect::Cut,
                    duration_frames: 0,
                },
                WireCueAction::OverlayOn {
                    channel: WireOverlayChannelId::new(2).unwrap(),
                    input: WireInputTarget::Preview,
                },
            ],
        )];
        let mut show = show();
        let unbound = run(&show, &cues);
        assert_eq!(unbound.lines[1].text, "Take: Selected input");
        assert_eq!(
            unbound.lines[1].issues,
            [refused("no input is selected on this console")]
        );
        show.selected = Some(id(3));
        let bound = run(&show, &cues);
        let texts = texts(&bound);
        assert_eq!(texts[1], "Cut to Camera 3 (now Camera 1)", "{texts:?}");
        assert!(bound.lines[1].issues.is_empty(), "{:?}", bound.lines[1]);
        // Preview binds at GO: Camera 2, not Camera 1 that the first
        // action put there.
        assert!(texts[0].contains("Camera 1"), "{texts:?}");
        assert!(texts[2].contains("Camera 2"), "{texts:?}");
    }
}
