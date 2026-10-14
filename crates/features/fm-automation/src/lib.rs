//! Deterministic shortcuts, triggers, macros, schedules, and controllers.
//!
//! This crate describes automation intent only. It performs no device I/O and
//! never reads a clock; every time-sensitive operation receives a caller-owned
//! timestamp.

mod catalog;
mod condition;
mod controller;
mod go;
mod intent;
mod macros;
mod schedule;
mod script;
mod shortcut;
mod template;
mod trigger;

pub use catalog::{
    InputObservation, InputObserveReport, InputTrigger, InputTriggerBinding, InputTriggerError,
    InputTriggerFire, InputTriggerMatch, InputTriggerSet, MAX_INPUT_TRIGGER_ACTIONS,
    MAX_INPUT_TRIGGER_BINDINGS, TimelineObservation, TriggerAction,
};
pub use condition::{Condition, ConditionContext, Predicate, Value};
pub use controller::{
    ActivatorEngine, ActivatorMapping, ActivatorRule, ControlAddress, ControlMode, ControllerError,
    ControllerFeedback, ControllerInput, ControllerManager, DeviceState, LearnRequest,
    MappedControllerIntent, Mapping, TallySnapshot, ValueRange,
};
pub use go::{
    GoAction, GoActionFire, GoEngine, GoError, GoPreview, GoStart, MAX_GO_START_RECEIPTS,
    PlannedGoAction, ProgrammedGo,
};
pub use intent::{CommandIntent, IntentBuffer};
pub use macros::{
    AttemptOutcome, CancelPolicy, MacroDecision, MacroDefinition, MacroDispatch, MacroError,
    MacroRun, MacroStatus, RetryPolicy,
};
pub use schedule::{ScheduleError, ScheduleFire, ScheduleId, ScheduleKind, ScheduleSet};
pub use script::{
    MAX_SCRIPT_MACROS, MAX_SCRIPT_STEPS, RunId, RunOutcome, RunState, ScriptDispatch, ScriptEngine,
    ScriptError, ScriptFailure, ScriptLimits, ScriptLogEntry, ScriptLogEvent, ScriptMacro,
    ScriptPoll, ScriptStep, TimeoutAction,
};
pub use shortcut::{
    Chord, ChordError, ConflictKind, KeyStroke, Modifiers, Shortcut, ShortcutConflict,
    ShortcutError, ShortcutRegistry, ShortcutScope,
};
pub use template::{
    InputSelector, MAX_PROFILE_LINE_BYTES, MAX_STREAM_NAME_BYTES, MAX_TEMPLATE_NAME_BYTES,
    MAX_TEMPLATES, SHORTCUT_PROFILE_MAGIC, SHORTCUT_PROFILE_VERSION, ShortcutProfileCodec,
    ShortcutProfileError, ShortcutTemplate, ShortcutTemplateRegistry, TemplateError, TemplatePress,
    TemplateRegistryError, TemplateTarget, parse_shortcut_profile, render_shortcut_profile,
};
pub use trigger::{
    AutomationEvent, EventFilter, Trigger, TriggerEngine, TriggerError, TriggerFire,
};
