//! The transport bar and the timeline's keyboard shortcuts. Buttons and keys
//! share one action table, so a button's hover text always names the key
//! that does the same thing and its disabled reason matches what the key
//! would report.

use egui::{Event, Key, Modifiers, Rect, RichText, Stroke, Ui, Vec2};
use fm_ui_model::animation_editor::{AnimDocument, AnimEditorError, EditorMode};

use super::AnimationTimelineState;
use crate::gating::Hint;
use crate::l10n;
use crate::theme::{
    ACCENT_FILL, accent_color, border_strong_color, legible, line_width, text_color, widget_color,
};

/// One transport-bar control.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TimelineAction {
    PlayPause,
    ToggleLoop,
    StepBack,
    StepForward,
    KeyAtPlayhead,
    Delete,
    Copy,
    Paste,
    Undo,
    Redo,
    ToggleMode,
}

impl TimelineAction {
    /// Every transport control, in bar order.
    pub const ALL: [Self; 11] = [
        Self::PlayPause,
        Self::ToggleLoop,
        Self::StepBack,
        Self::StepForward,
        Self::KeyAtPlayhead,
        Self::Delete,
        Self::Copy,
        Self::Paste,
        Self::Undo,
        Self::Redo,
        Self::ToggleMode,
    ];

    /// The keyboard shortcut, as shown in hover text.
    #[must_use]
    pub const fn shortcut(self) -> &'static str {
        match self {
            Self::PlayPause => "Space",
            Self::ToggleLoop => "L",
            Self::StepBack => "Page Up, or ← with no keys selected",
            Self::StepForward => "Page Down, or → with no keys selected",
            Self::KeyAtPlayhead => "K",
            Self::Delete => "Delete",
            Self::Copy => "Ctrl+C",
            Self::Paste => "Ctrl+V",
            Self::Undo => "Ctrl+Z",
            Self::Redo => "Ctrl+Shift+Z",
            Self::ToggleMode => "M",
        }
    }
}

/// How one transport control looks and reads right now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActionSpec {
    /// Button caption.
    pub label: &'static str,
    /// What the control does, without the shortcut.
    pub action: &'static str,
    /// Why the control is disabled; `None` while enabled.
    pub disabled_reason: Option<&'static str>,
    /// Whether the control is a toggle that is currently on.
    pub active: bool,
}

impl ActionSpec {
    /// The hover text: the action with its shortcut.
    #[must_use]
    pub fn hover(&self, action: TimelineAction) -> String {
        l10n::with_key(l10n::Locale::ENGLISH, self.action, action.shortcut())
    }
}

const REASON_DRAG: &str = "finish the current drag first";
const REASON_NO_LENGTH: &str =
    "this phase has no length yet; apply a preset or drag the phase end on the ruler";
const REASON_NO_SELECTION: &str = "no keys are selected; click a key or drag a box around keys";
const REASON_SIMPLE_KEY: &str =
    "keys are set per property in Advanced mode; in Simple mode pick a preset instead";
const REASON_NO_ROW: &str = "click a property row (or one of its keys) to choose what to key";

/// The current look and availability of `action`.
#[must_use]
#[allow(clippy::too_many_lines)] // One arm per control keeps the table readable.
pub fn action_spec<T: Copy + Ord>(
    action: TimelineAction,
    document: &AnimDocument<T>,
    state: &AnimationTimelineState<T>,
) -> ActionSpec {
    let dragging = document.drag().is_some();
    let busy = dragging.then_some(REASON_DRAG);
    let no_selection = document
        .selection()
        .is_empty()
        .then_some(REASON_NO_SELECTION);
    let (label, text, reason, active) = match action {
        TimelineAction::PlayPause => {
            let playing = document.transport().is_playing();
            (
                if playing { "⏸" } else { "▶" },
                if playing {
                    "Pause playback"
                } else {
                    "Play this phase from the playhead"
                },
                busy.or((document.phase_frames() == 0).then_some(REASON_NO_LENGTH)),
                playing,
            )
        }
        TimelineAction::ToggleLoop => (
            "Loop",
            if document.transport().is_looping() {
                "Looping is on: playback wraps to the start of the phase. Click to stop at the end"
            } else {
                "Looping is off: playback stops at the end of the phase. Click to loop"
            },
            None,
            document.transport().is_looping(),
        ),
        TimelineAction::StepBack => (
            "◀ 1",
            "Step back one frame (Shift: ten frames)",
            busy.or((document.playhead() == 0).then_some("the playhead is on the first frame")),
            false,
        ),
        TimelineAction::StepForward => (
            "1 ▶",
            "Step forward one frame (Shift: ten frames)",
            busy.or((document.playhead() >= document.phase_frames())
                .then_some("the playhead is on the last frame")),
            false,
        ),
        TimelineAction::KeyAtPlayhead => (
            "◆ Key",
            "Add a key at the playhead on the active property row, holding its current value",
            busy.or(match document.mode() {
                EditorMode::Simple => Some(REASON_SIMPLE_KEY),
                EditorMode::Advanced => state.active_row.is_none().then_some(REASON_NO_ROW),
            }),
            false,
        ),
        TimelineAction::Delete => (
            "Delete",
            "Delete the selected keys",
            busy.or(no_selection),
            false,
        ),
        TimelineAction::Copy => (
            "Copy",
            "Copy the selected keys (timing, values, and easing)",
            no_selection,
            false,
        ),
        TimelineAction::Paste => (
            "Paste",
            "Paste copied keys with the earliest one at the playhead",
            busy.or(document
                .clipboard()
                .is_none_or(|clipboard| clipboard.keys.is_empty())
                .then_some("nothing has been copied yet; select keys and copy them first")),
            false,
        ),
        TimelineAction::Undo => (
            "Undo",
            "Undo the last animation edit (a whole drag is one step)",
            busy.or((!document.can_undo()).then_some("there is nothing to undo")),
            false,
        ),
        TimelineAction::Redo => (
            "Redo",
            "Redo the last undone animation edit",
            busy.or((!document.can_redo()).then_some("there is nothing to redo")),
            false,
        ),
        TimelineAction::ToggleMode => match document.mode() {
            EditorMode::Simple => (
                "Advanced",
                "Switch to Advanced mode: one row per property with individual keys, easing, \
                 and a value graph",
                busy,
                false,
            ),
            EditorMode::Advanced => (
                "Simple",
                "Switch to Simple mode: one bar per element, stretch it to retime and pick \
                 ready-made presets",
                busy,
                true,
            ),
        },
    };
    ActionSpec {
        label,
        action: text,
        disabled_reason: reason,
        active,
    }
}

/// Runs `action` (`shift` widens steps to ten frames).
///
/// # Errors
///
/// The core's refusal, which leaves the document unchanged.
pub fn perform<T: Copy + Ord>(
    action: TimelineAction,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
    shift: bool,
) -> Result<(), AnimEditorError<T>> {
    let step = if shift { 10 } else { 1 };
    match action {
        TimelineAction::PlayPause => document.toggle_play(),
        TimelineAction::ToggleLoop => {
            let looping = document.transport().is_looping();
            document.set_looping(!looping);
        }
        TimelineAction::StepBack => document.step(-step),
        TimelineAction::StepForward => document.step(step),
        TimelineAction::KeyAtPlayhead => {
            if document.mode() == EditorMode::Simple {
                return Err(AnimEditorError::EmptySelection);
            }
            let row = state.active_row.ok_or(AnimEditorError::EmptySelection)?;
            document.key_at_playhead(row.target, row.property)?;
        }
        TimelineAction::Delete => document.delete_selection()?,
        TimelineAction::Copy => {
            document.copy_selection()?;
        }
        TimelineAction::Paste => document.paste_at_playhead()?,
        TimelineAction::Undo => document.undo()?,
        TimelineAction::Redo => document.redo()?,
        TimelineAction::ToggleMode => {
            document.ensure_not_dragging()?;
            document.set_mode(match document.mode() {
                EditorMode::Simple => EditorMode::Advanced,
                EditorMode::Advanced => EditorMode::Simple,
            });
        }
    }
    Ok(())
}

/// `AnimDocument` has no public "is a drag running" error helper; this
/// mirrors its `DragActive` refusal for UI-only actions.
trait DragGuard<T> {
    fn ensure_not_dragging(&self) -> Result<(), AnimEditorError<T>>;
}

impl<T: Copy + Ord> DragGuard<T> for AnimDocument<T> {
    fn ensure_not_dragging(&self) -> Result<(), AnimEditorError<T>> {
        if self.drag().is_some() {
            Err(AnimEditorError::DragActive)
        } else {
            Ok(())
        }
    }
}

/// Draws the transport bar; returns each control's rect and the actions
/// clicked.
pub(super) fn draw_transport<T: Copy + Ord>(
    ui: &mut Ui,
    document: &AnimDocument<T>,
    state: &AnimationTimelineState<T>,
    rects: &mut Vec<(TimelineAction, Rect)>,
) -> Vec<(TimelineAction, bool)> {
    let mut clicked = Vec::new();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        for action in TimelineAction::ALL {
            if matches!(
                action,
                TimelineAction::KeyAtPlayhead | TimelineAction::Undo | TimelineAction::ToggleMode
            ) {
                ui.separator();
            }
            if action == TimelineAction::ToggleMode {
                timecode_readout(ui, document);
                ui.separator();
            }
            let spec = action_spec(action, document, state);
            let button = egui::Button::new(
                RichText::new(spec.label)
                    .size(legible(11.0))
                    .color(text_color()),
            )
            .fill(if spec.active {
                ACCENT_FILL
            } else {
                widget_color()
            })
            .stroke(Stroke::new(
                line_width(),
                if spec.active {
                    accent_color().gamma_multiply(0.7)
                } else {
                    border_strong_color()
                },
            ))
            .min_size(Vec2::new(30.0, 22.0));
            let response = ui
                .add_enabled(spec.disabled_reason.is_none(), button)
                .hint(&spec.hover(action), spec.disabled_reason.unwrap_or(""));
            rects.push((action, response.rect));
            if response.clicked() {
                clicked.push((action, ui.input(|input| input.modifiers.shift)));
            }
        }
    });
    clicked
}

fn timecode_readout<T: Copy + Ord>(ui: &mut Ui, document: &AnimDocument<T>) {
    let rate = document.clip().timebase().rate();
    let text = format!(
        "{} / {}",
        crate::timecode(u64::from(document.playhead()), rate),
        crate::timecode(u64::from(document.phase_frames()), rate)
    );
    ui.label(
        crate::theme::mono(text)
            .size(legible(11.0))
            .color(text_color()),
    )
    .on_hover_text(format!(
        "Playhead / phase length as HH:MM:SS:FF (frame {} of {}). \
             Drag on the ruler to scrub.",
        document.playhead(),
        document.phase_frames()
    ));
}

/// Keys handled by the timeline while it owns the keyboard. Consumed keys
/// do not reach the rest of the console.
#[allow(clippy::too_many_lines)] // One shortcut table.
pub(super) fn handle_keys<T: Copy + Ord>(
    ui: &mut Ui,
    document: &mut AnimDocument<T>,
    state: &mut AnimationTimelineState<T>,
) -> Vec<Result<TimelineAction, AnimEditorError<T>>> {
    let mut results = Vec::new();
    if !state.keyboard_active || ui.ctx().egui_wants_keyboard_input() {
        return results;
    }
    let run = |action: TimelineAction,
               shift: bool,
               document: &mut AnimDocument<T>,
               state: &mut AnimationTimelineState<T>|
     -> Option<Result<TimelineAction, AnimEditorError<T>>> {
        // A key for a disabled control explains itself with the same reason
        // the button's hover gives, instead of failing silently.
        if let Some(reason) = action_spec(action, document, state).disabled_reason {
            state.notice = Some(super::Notice::hint(reason));
            return None;
        }
        Some(perform(action, document, state, shift).map(|()| action))
    };
    let (shift, events) = ui.input_mut(|input| {
        let shift = input.modifiers.shift;
        let mut events = Vec::new();
        // Most specific first: Ctrl+Shift+Z before Ctrl+Z.
        if input.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)
            || input.consume_key(Modifiers::COMMAND, Key::Y)
        {
            events.push(Shortcut::Action(TimelineAction::Redo));
        }
        if input.consume_key(Modifiers::COMMAND, Key::Z) {
            events.push(Shortcut::Action(TimelineAction::Undo));
        }
        if input.consume_key(Modifiers::COMMAND, Key::A) {
            events.push(Shortcut::SelectAll);
        }
        if input.consume_key(Modifiers::COMMAND, Key::C) {
            events.push(Shortcut::Action(TimelineAction::Copy));
        }
        if input.consume_key(Modifiers::COMMAND, Key::V) {
            events.push(Shortcut::Action(TimelineAction::Paste));
        }
        if input.consume_key(Modifiers::COMMAND, Key::X) {
            events.push(Shortcut::Cut);
        }
        // Platform clipboard events (egui-winit turns Ctrl+C/V/X into these).
        input.events.retain(|event| match event {
            Event::Copy => {
                events.push(Shortcut::Action(TimelineAction::Copy));
                false
            }
            Event::Paste(_) => {
                events.push(Shortcut::Action(TimelineAction::Paste));
                false
            }
            Event::Cut => {
                events.push(Shortcut::Cut);
                false
            }
            _ => true,
        });
        let plain = [
            (Key::Space, Shortcut::Action(TimelineAction::PlayPause)),
            (Key::K, Shortcut::Action(TimelineAction::KeyAtPlayhead)),
            (Key::L, Shortcut::Action(TimelineAction::ToggleLoop)),
            (Key::M, Shortcut::Action(TimelineAction::ToggleMode)),
            (Key::Delete, Shortcut::Action(TimelineAction::Delete)),
            (Key::Backspace, Shortcut::Action(TimelineAction::Delete)),
            (Key::PageUp, Shortcut::Action(TimelineAction::StepBack)),
            (Key::PageDown, Shortcut::Action(TimelineAction::StepForward)),
            (Key::ArrowLeft, Shortcut::Arrow(-1)),
            (Key::ArrowRight, Shortcut::Arrow(1)),
            (Key::Home, Shortcut::Home),
            (Key::End, Shortcut::End),
            (Key::Escape, Shortcut::Escape),
        ];
        for (key, shortcut) in plain {
            // `consume_key` ignores extra Shift, so Shift+arrow still lands
            // here (and widens the step).
            while input.consume_key(Modifiers::NONE, key) {
                events.push(shortcut);
            }
        }
        (shift, events)
    });
    for event in events {
        match event {
            Shortcut::Action(action) => results.extend(run(action, shift, document, state)),
            Shortcut::SelectAll => {
                if document.drag().is_none() {
                    document.select_all();
                }
            }
            Shortcut::Cut => {
                results.extend(run(TimelineAction::Copy, shift, document, state));
                results.extend(run(TimelineAction::Delete, shift, document, state));
            }
            Shortcut::Arrow(direction) => {
                let delta = if shift { 10 * direction } else { direction };
                if document.selection().is_empty() {
                    if document.drag().is_none() {
                        document.step(delta);
                    }
                } else if let Err(error) = document.nudge_selection(delta) {
                    // A nudge is not a transport action; only its refusal
                    // is reported.
                    results.push(Err(error));
                }
            }
            Shortcut::Home => {
                if document.drag().is_none() {
                    document.set_playhead(0);
                }
            }
            Shortcut::End => {
                if document.drag().is_none() {
                    document.set_playhead(document.phase_frames());
                }
            }
            Shortcut::Escape => {
                if document.drag().is_some() {
                    let _ = document.cancel_drag();
                } else {
                    document.clear_selection();
                }
            }
        }
    }
    results
}

#[derive(Clone, Copy, Debug)]
enum Shortcut {
    Action(TimelineAction),
    SelectAll,
    Cut,
    Arrow(i64),
    Home,
    End,
    Escape,
}
