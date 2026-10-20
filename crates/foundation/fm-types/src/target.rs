//! Dynamic input targets (RC-004): an action names a fixed input or the
//! input that holds a role when the action is fired.
//!
//! A shortcut or cue action that acts on an input names it with an
//! [`InputTarget`]: a fixed input id, or "the input on Preview", "the input
//! on Program", or "the selected input" (the input selected on the console
//! that fires it). A dynamic target is resolved once, when the action is
//! fired, against [`TargetBindings`]: Preview and Program are read from the
//! show on the authority, and the selected input is resolved by the client
//! into a fixed id before it sends the fire. The authority journals the ids
//! it resolved, so a replay of the journal acts on the same inputs.
//!
//! The type is generic over the id so the protocol can use its own wire id.

use core::fmt;

use crate::InputId;

/// The input an action acts on.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum InputTarget<I = InputId> {
    /// That input, always.
    Fixed(I),
    /// The input on Preview when the action is fired.
    Preview,
    /// The input on Program when the action is fired.
    Program,
    /// The input selected on the console that fires the action.
    Selected,
}

impl<I: Copy> InputTarget<I> {
    /// The fixed input, `None` for a dynamic target.
    #[must_use]
    pub const fn fixed(self) -> Option<I> {
        match self {
            Self::Fixed(input) => Some(input),
            Self::Preview | Self::Program | Self::Selected => None,
        }
    }

    /// Whether the target is resolved when fired rather than fixed.
    #[must_use]
    pub const fn is_dynamic(self) -> bool {
        !matches!(self, Self::Fixed(_))
    }

    /// The input the target names under `bindings`, `None` when the role
    /// is not bound (no input is selected on the firing console).
    #[must_use]
    pub const fn resolve(self, bindings: &TargetBindings<I>) -> Option<I> {
        match self {
            Self::Fixed(input) => Some(input),
            Self::Preview => bindings.preview,
            Self::Program => bindings.program,
            Self::Selected => bindings.selected,
        }
    }

    /// The same target over another id type.
    #[must_use]
    pub fn map<J>(self, map: impl FnOnce(I) -> J) -> InputTarget<J> {
        match self {
            Self::Fixed(input) => InputTarget::Fixed(map(input)),
            Self::Preview => InputTarget::Preview,
            Self::Program => InputTarget::Program,
            Self::Selected => InputTarget::Selected,
        }
    }

    /// The token of a dynamic target (`preview`, `program`, `selected`),
    /// `None` for a fixed one.
    #[must_use]
    pub const fn role_token(self) -> Option<&'static str> {
        match self {
            Self::Fixed(_) => None,
            Self::Preview => Some("preview"),
            Self::Program => Some("program"),
            Self::Selected => Some("selected"),
        }
    }

    /// The dynamic target a role token names.
    #[must_use]
    pub fn from_role_token(token: &str) -> Option<Self> {
        match token {
            "preview" => Some(Self::Preview),
            "program" => Some(Self::Program),
            "selected" => Some(Self::Selected),
            _ => None,
        }
    }
}

impl<I> From<I> for InputTarget<I> {
    fn from(input: I) -> Self {
        Self::Fixed(input)
    }
}

impl<I: fmt::Display> fmt::Display for InputTarget<I> {
    /// A fixed target shows its id, a dynamic one its role token.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed(input) => input.fmt(formatter),
            Self::Preview => formatter.write_str("preview"),
            Self::Program => formatter.write_str("program"),
            Self::Selected => formatter.write_str("selected"),
        }
    }
}

/// The inputs the dynamic targets of one fire resolve to. A role left
/// `None` on a command is read by the authority (Preview and Program) or is
/// unbound (no input selected on the firing console).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TargetBindings<I = InputId> {
    pub preview: Option<I>,
    pub program: Option<I>,
    pub selected: Option<I>,
}

impl<I> Default for TargetBindings<I> {
    /// No role bound ([`Self::NONE`]).
    fn default() -> Self {
        Self::NONE
    }
}

impl<I> TargetBindings<I> {
    /// No role bound: the authority binds Preview and Program, and no
    /// input is selected.
    pub const NONE: Self = Self {
        preview: None,
        program: None,
        selected: None,
    };
}

impl<I: Copy> TargetBindings<I> {
    /// Whether no role is bound.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.preview.is_none() && self.program.is_none() && self.selected.is_none()
    }

    /// The same bindings over another id type.
    #[must_use]
    pub fn map<J>(self, mut map: impl FnMut(I) -> J) -> TargetBindings<J> {
        TargetBindings {
            preview: self.preview.map(&mut map),
            program: self.program.map(&mut map),
            selected: self.selected.map(&mut map),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: u128) -> InputId {
        InputId::new(core::num::NonZeroU128::new(value).expect("nonzero"))
    }

    #[test]
    fn targets_resolve_against_their_bindings() {
        let bindings = TargetBindings {
            preview: Some(id(2)),
            program: Some(id(3)),
            selected: None,
        };
        assert_eq!(InputTarget::Fixed(id(9)).resolve(&bindings), Some(id(9)));
        assert_eq!(InputTarget::Preview.resolve(&bindings), Some(id(2)));
        assert_eq!(InputTarget::Program.resolve(&bindings), Some(id(3)));
        assert_eq!(InputTarget::<InputId>::Selected.resolve(&bindings), None);
        assert!(InputTarget::<InputId>::Selected.is_dynamic());
        assert!(!InputTarget::Fixed(id(1)).is_dynamic());
    }

    #[test]
    fn role_tokens_round_trip() {
        for target in [
            InputTarget::<InputId>::Preview,
            InputTarget::Program,
            InputTarget::Selected,
        ] {
            let token = target.role_token().expect("dynamic targets have tokens");
            assert_eq!(InputTarget::from_role_token(token), Some(target));
            assert_eq!(target.to_string(), token);
        }
        assert_eq!(InputTarget::<InputId>::from_role_token("pvw"), None);
        assert_eq!(InputTarget::Fixed(id(4)).role_token(), None);
        assert_eq!(InputTarget::Fixed(id(4)).to_string(), "4");
    }
}
