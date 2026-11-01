//! A cross-input bank of media-list players (`IN-006`).
//!
//! A bank owns at most [`MAX_GO_BANK_INPUTS`] players keyed by input, so a GO
//! can be addressed to a named input instead of whichever list happens to be
//! focused. The bank is a model: callers tick each player and own the
//! transport/source wiring.

use core::fmt;
use std::collections::BTreeMap;

use fm_types::InputId;

use crate::{GoScheduleOutcome, ListGo, ListGoError, ListPlayer};

/// Most inputs one bank holds.
pub const MAX_GO_BANK_INPUTS: usize = 64;

/// Why a bank edit or GO was refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ListGoBankError {
    /// The input already has a player.
    DuplicateInput(InputId),
    /// The bank already holds [`MAX_GO_BANK_INPUTS`] players.
    TooManyInputs { maximum: usize },
    /// No player is registered for the input.
    UnknownInput(InputId),
    /// The player refused the programmed GO.
    Go(ListGoError),
}

impl fmt::Display for ListGoBankError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateInput(input) => {
                write!(formatter, "input {input} already has a list player")
            }
            Self::TooManyInputs { maximum } => {
                write!(formatter, "a GO bank holds at most {maximum} inputs")
            }
            Self::UnknownInput(input) => write!(formatter, "the GO bank has no input {input}"),
            Self::Go(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ListGoBankError {}

impl From<ListGoError> for ListGoBankError {
    fn from(error: ListGoError) -> Self {
        Self::Go(error)
    }
}

/// One media-list player per input.
#[derive(Clone, Debug)]
pub struct ListGoBank<F> {
    players: BTreeMap<InputId, ListPlayer<F>>,
}

impl<F: Clone> Default for ListGoBank<F> {
    fn default() -> Self {
        Self::new()
    }
}

impl<F: Clone> ListGoBank<F> {
    /// An empty bank.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            players: BTreeMap::new(),
        }
    }

    /// Adds `player` for `input`.
    ///
    /// # Errors
    ///
    /// Returns [`ListGoBankError::DuplicateInput`] when the input already has
    /// a player, or [`ListGoBankError::TooManyInputs`] at the bound.
    pub fn insert(&mut self, input: InputId, player: ListPlayer<F>) -> Result<(), ListGoBankError> {
        if self.players.contains_key(&input) {
            return Err(ListGoBankError::DuplicateInput(input));
        }
        if self.players.len() >= MAX_GO_BANK_INPUTS {
            return Err(ListGoBankError::TooManyInputs {
                maximum: MAX_GO_BANK_INPUTS,
            });
        }
        self.players.insert(input, player);
        Ok(())
    }

    /// Removes and returns the player for `input`.
    pub fn remove(&mut self, input: InputId) -> Option<ListPlayer<F>> {
        self.players.remove(&input)
    }

    /// The player for `input`.
    #[must_use]
    pub fn player(&self, input: InputId) -> Option<&ListPlayer<F>> {
        self.players.get(&input)
    }

    /// The player for `input`, mutably.
    #[must_use]
    pub fn player_mut(&mut self, input: InputId) -> Option<&mut ListPlayer<F>> {
        self.players.get_mut(&input)
    }

    /// Inputs in ascending order.
    pub fn inputs(&self) -> impl Iterator<Item = InputId> + '_ {
        self.players.keys().copied()
    }

    /// How many inputs the bank holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.players.len()
    }

    /// Whether the bank holds no input.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    /// Schedules `command` on the player registered for `input`.
    ///
    /// # Errors
    ///
    /// Returns [`ListGoBankError::UnknownInput`] or the player's
    /// [`ListGoError`].
    pub fn schedule_go(
        &mut self,
        input: InputId,
        command: ListGo,
    ) -> Result<GoScheduleOutcome, ListGoBankError> {
        self.players
            .get_mut(&input)
            .ok_or(ListGoBankError::UnknownInput(input))?
            .schedule_go(command)
            .map_err(Into::into)
    }
}
