//! Secondary mixes (IN-023).
//!
//! Main is mix 1 and stays the switcher's own Program/Preview. The project may
//! configure up to fifteen secondary mixes, each with its own Program source
//! and, unless it is output-only, its own Preview source. A mix reads an input
//! or another mix; the bank's one-frame mix-to-mix delay breaks live feedback,
//! while this module still rejects direct and chained self-reference so the
//! routing graph names a finite set of mixes.

use core::fmt;

use fm_types::InputId;

/// Number of secondary mixes beside Main.
pub const MAX_SECONDARY_MIXES: usize = 15;
/// Highest valid mix number (Main is 1).
pub const MAX_MIX_NUMBER: u8 = 16;
/// Main's mix number.
pub const MAIN_MIX_NUMBER: u8 = 1;
/// Maximum bytes of one secondary mix name.
pub const MAX_SECONDARY_MIX_NAME_BYTES: usize = 128;

/// A one-based mix number in `1..=16`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct MixNumber(u8);

impl MixNumber {
    /// The Main mix.
    pub const MAIN: Self = Self(MAIN_MIX_NUMBER);

    /// Creates a mix number from its one-based operator-facing value.
    #[must_use]
    pub const fn new(number: u8) -> Option<Self> {
        if number >= 1 && number <= MAX_MIX_NUMBER {
            Some(Self(number))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn is_main(self) -> bool {
        self.0 == MAIN_MIX_NUMBER
    }
}

impl fmt::Display for MixNumber {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "mix {}", self.0)
    }
}

/// One source a mix can show.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecondaryMixSource {
    Input(InputId),
    Mix(MixNumber),
}

/// Whether a secondary mix keeps a Preview bus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecondaryMixMode {
    /// The mix has its own Program and Preview sources and takes through them.
    PreviewProgram,
    /// Program only; a take names its destination source directly.
    OutputOnly,
}

/// Routing for one secondary mix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecondaryMixConfig {
    pub number: MixNumber,
    pub name: String,
    pub mode: SecondaryMixMode,
    pub program: SecondaryMixSource,
    pub preview: Option<SecondaryMixSource>,
}

impl SecondaryMixConfig {
    /// A mix that keeps a Preview bus.
    #[must_use]
    pub fn preview_program(
        number: MixNumber,
        name: impl Into<String>,
        program: SecondaryMixSource,
        preview: SecondaryMixSource,
    ) -> Self {
        Self {
            number,
            name: name.into(),
            mode: SecondaryMixMode::PreviewProgram,
            program,
            preview: Some(preview),
        }
    }

    /// A mix whose takes name their destination source directly.
    #[must_use]
    pub fn output_only(
        number: MixNumber,
        name: impl Into<String>,
        program: SecondaryMixSource,
    ) -> Self {
        Self {
            number,
            name: name.into(),
            mode: SecondaryMixMode::OutputOnly,
            program,
            preview: None,
        }
    }
}

/// Why secondary-mix routing is invalid.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecondaryMixError {
    TooManyMixes {
        maximum: usize,
    },
    /// Mixes are not strictly ascending by number.
    OutOfOrder,
    /// Main is the switcher's own mix and cannot be configured here.
    MainNotSecondary,
    DuplicateNumber(MixNumber),
    EmptyName,
    NameTooLong {
        actual: usize,
        maximum: usize,
    },
    ControlCharacterInName {
        index: usize,
    },
    PreviewRequired(MixNumber),
    PreviewNotAllowed(MixNumber),
    PreviewEqualsProgram(MixNumber),
    UnknownInput(InputId),
    UnknownMix(MixNumber),
    SelfReference(MixNumber),
    Cycle,
}

impl SecondaryMixError {
    /// The mix number the error belongs to, when it names one.
    #[must_use]
    pub const fn mix(&self) -> Option<MixNumber> {
        match self {
            Self::DuplicateNumber(number) => Some(*number),
            Self::PreviewRequired(number)
            | Self::PreviewNotAllowed(number)
            | Self::PreviewEqualsProgram(number)
            | Self::SelfReference(number) => Some(*number),
            _ => None,
        }
    }
}

impl fmt::Display for SecondaryMixError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyMixes { maximum } => {
                write!(formatter, "at most {maximum} secondary mixes are allowed")
            }
            Self::OutOfOrder => formatter.write_str("secondary mixes must be in number order"),
            Self::MainNotSecondary => formatter.write_str("Main is not a secondary mix"),
            Self::DuplicateNumber(number) => write!(formatter, "{number} is configured twice"),
            Self::EmptyName => formatter.write_str("a secondary mix name must not be empty"),
            Self::NameTooLong { actual, maximum } => write!(
                formatter,
                "secondary mix name has {actual} bytes; maximum is {maximum}"
            ),
            Self::ControlCharacterInName { index } => write!(
                formatter,
                "secondary mix name contains a control character at byte {index}"
            ),
            Self::PreviewRequired(number) => {
                write!(formatter, "{number} needs a Preview source")
            }
            Self::PreviewNotAllowed(number) => {
                write!(formatter, "{number} is output-only and cannot have Preview")
            }
            Self::PreviewEqualsProgram(number) => {
                write!(formatter, "{number} Preview equals its Program source")
            }
            Self::UnknownInput(input) => write!(formatter, "unknown input {input}"),
            Self::UnknownMix(number) => write!(formatter, "unknown {number}"),
            Self::SelfReference(number) => write!(formatter, "{number} displays itself"),
            Self::Cycle => formatter.write_str("secondary mixes form a cycle"),
        }
    }
}

impl std::error::Error for SecondaryMixError {}

/// Checks a complete secondary-mix list against the show's inputs.
///
/// `configs` must be strictly ascending by number, with numbers in
/// `2..=16`. A source naming another secondary mix must resolve to one of
/// `configs`; Main is always available. Direct and chained self-reference are
/// rejected even though the live bank's one-frame delay would break the
/// feedback path.
///
/// # Errors
///
/// Returns the first structural or reference error.
pub fn validate_secondary_mixes(
    configs: &[SecondaryMixConfig],
    inputs: &[InputId],
) -> Result<(), SecondaryMixError> {
    if configs.len() > MAX_SECONDARY_MIXES {
        return Err(SecondaryMixError::TooManyMixes {
            maximum: MAX_SECONDARY_MIXES,
        });
    }
    let mut previous = None;
    for config in configs {
        if config.number.is_main() {
            return Err(SecondaryMixError::MainNotSecondary);
        }
        if let Some(previous) = previous {
            if config.number == previous {
                return Err(SecondaryMixError::DuplicateNumber(config.number));
            }
            if config.number < previous {
                return Err(SecondaryMixError::OutOfOrder);
            }
        }
        previous = Some(config.number);
        validate_name(&config.name)?;
        match config.mode {
            SecondaryMixMode::PreviewProgram => {
                let Some(preview) = config.preview else {
                    return Err(SecondaryMixError::PreviewRequired(config.number));
                };
                if preview == config.program {
                    return Err(SecondaryMixError::PreviewEqualsProgram(config.number));
                }
            }
            SecondaryMixMode::OutputOnly => {
                if config.preview.is_some() {
                    return Err(SecondaryMixError::PreviewNotAllowed(config.number));
                }
            }
        }
        validate_source(config.number, config.program, inputs, configs)?;
        if let Some(preview) = config.preview {
            validate_source(config.number, preview, inputs, configs)?;
        }
    }
    for config in configs {
        let mut visiting = Vec::new();
        let mut done = Vec::new();
        if visit(config.number, configs, &mut visiting, &mut done) {
            return Err(SecondaryMixError::Cycle);
        }
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<(), SecondaryMixError> {
    if name.is_empty() {
        return Err(SecondaryMixError::EmptyName);
    }
    if name.len() > MAX_SECONDARY_MIX_NAME_BYTES {
        return Err(SecondaryMixError::NameTooLong {
            actual: name.len(),
            maximum: MAX_SECONDARY_MIX_NAME_BYTES,
        });
    }
    if let Some((index, _)) = name
        .char_indices()
        .find(|(_, character)| character.is_control())
    {
        return Err(SecondaryMixError::ControlCharacterInName { index });
    }
    Ok(())
}

fn validate_source(
    owner: MixNumber,
    source: SecondaryMixSource,
    inputs: &[InputId],
    configs: &[SecondaryMixConfig],
) -> Result<(), SecondaryMixError> {
    match source {
        SecondaryMixSource::Input(input) if inputs.contains(&input) => Ok(()),
        SecondaryMixSource::Input(input) => Err(SecondaryMixError::UnknownInput(input)),
        SecondaryMixSource::Mix(number) if number == owner => {
            Err(SecondaryMixError::SelfReference(owner))
        }
        SecondaryMixSource::Mix(number) if number.is_main() => Ok(()),
        SecondaryMixSource::Mix(number) if configs.iter().any(|config| config.number == number) => {
            Ok(())
        }
        SecondaryMixSource::Mix(number) => Err(SecondaryMixError::UnknownMix(number)),
    }
}

fn visit(
    start: MixNumber,
    configs: &[SecondaryMixConfig],
    visiting: &mut Vec<MixNumber>,
    done: &mut Vec<MixNumber>,
) -> bool {
    if done.contains(&start) {
        return false;
    }
    if visiting.contains(&start) {
        return true;
    }
    let Some(config) = configs.iter().find(|config| config.number == start) else {
        return false;
    };
    visiting.push(start);
    let mut sources = Vec::with_capacity(2);
    sources.push(config.program);
    sources.extend(config.preview);
    for source in sources {
        let SecondaryMixSource::Mix(number) = source else {
            continue;
        };
        if number.is_main() {
            continue;
        }
        if visit(number, configs, visiting, done) {
            return true;
        }
    }
    visiting.pop();
    done.push(start);
    false
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU128;

    use super::*;

    fn input(value: u128) -> InputId {
        InputId::new(NonZeroU128::new(value).unwrap())
    }

    fn number(value: u8) -> MixNumber {
        MixNumber::new(value).unwrap()
    }

    fn valid() -> Vec<SecondaryMixConfig> {
        vec![
            SecondaryMixConfig::preview_program(
                number(2),
                "Clean",
                SecondaryMixSource::Input(input(1)),
                SecondaryMixSource::Input(input(2)),
            ),
            SecondaryMixConfig::output_only(number(3), "Feed", SecondaryMixSource::Mix(number(2))),
        ]
    }

    #[test]
    fn a_valid_list_checks_every_source() {
        let inputs = [input(1), input(2)];
        assert_eq!(validate_secondary_mixes(&valid(), &inputs), Ok(()));
        assert_eq!(
            validate_secondary_mixes(&valid(), &[input(1)]),
            Err(SecondaryMixError::UnknownInput(input(2)))
        );
    }

    #[test]
    fn numbers_are_secondary_ascending_and_unique() {
        let mut configs = valid();
        configs[0].number = MixNumber::MAIN;
        assert_eq!(
            validate_secondary_mixes(&configs, &[input(1), input(2)]),
            Err(SecondaryMixError::MainNotSecondary)
        );

        let mut configs = valid();
        configs[1].number = number(2);
        assert_eq!(
            validate_secondary_mixes(&configs, &[input(1), input(2)]),
            Err(SecondaryMixError::DuplicateNumber(number(2)))
        );

        let mut configs = valid();
        configs[1].number = number(4);
        configs[0].number = number(5);
        assert_eq!(
            validate_secondary_mixes(&configs, &[input(1), input(2)]),
            Err(SecondaryMixError::OutOfOrder)
        );
    }

    #[test]
    fn preview_must_match_the_mode_and_differ_from_program() {
        let inputs = [input(1), input(2)];

        let mut configs = valid();
        configs[0].preview = None;
        assert_eq!(
            validate_secondary_mixes(&configs, &inputs),
            Err(SecondaryMixError::PreviewRequired(number(2)))
        );

        let mut configs = valid();
        configs[1].preview = Some(SecondaryMixSource::Input(input(1)));
        assert_eq!(
            validate_secondary_mixes(&configs, &inputs),
            Err(SecondaryMixError::PreviewNotAllowed(number(3)))
        );

        let mut configs = valid();
        configs[0].preview = Some(SecondaryMixSource::Input(input(1)));
        assert_eq!(
            validate_secondary_mixes(&configs, &inputs),
            Err(SecondaryMixError::PreviewEqualsProgram(number(2)))
        );
    }

    #[test]
    fn names_are_bounded_and_printable() {
        let inputs = [input(1), input(2)];

        let mut configs = valid();
        configs[0].name.clear();
        assert_eq!(
            validate_secondary_mixes(&configs, &inputs),
            Err(SecondaryMixError::EmptyName)
        );

        let mut configs = valid();
        configs[0].name = "x".repeat(MAX_SECONDARY_MIX_NAME_BYTES + 1);
        assert!(matches!(
            validate_secondary_mixes(&configs, &inputs),
            Err(SecondaryMixError::NameTooLong { .. })
        ));

        let mut configs = valid();
        configs[0].name = "clean\nfeed".into();
        assert!(matches!(
            validate_secondary_mixes(&configs, &inputs),
            Err(SecondaryMixError::ControlCharacterInName { .. })
        ));
    }

    #[test]
    fn a_mix_source_must_exist_and_may_name_main() {
        let inputs = [input(1), input(2)];

        let mut configs = valid();
        configs[1].program = SecondaryMixSource::Mix(number(9));
        assert_eq!(
            validate_secondary_mixes(&configs, &inputs),
            Err(SecondaryMixError::UnknownMix(number(9)))
        );

        let mut configs = valid();
        configs[1].program = SecondaryMixSource::Mix(MixNumber::MAIN);
        assert_eq!(validate_secondary_mixes(&configs, &inputs), Ok(()));
    }

    #[test]
    fn direct_and_chained_self_reference_are_rejected() {
        let inputs = [input(1), input(2)];

        let mut configs = valid();
        configs[1].program = SecondaryMixSource::Mix(number(3));
        assert_eq!(
            validate_secondary_mixes(&configs, &inputs),
            Err(SecondaryMixError::SelfReference(number(3)))
        );

        let mut configs = valid();
        configs[0].program = SecondaryMixSource::Mix(number(3));
        configs[0].preview = Some(SecondaryMixSource::Mix(number(4)));
        configs.push(SecondaryMixConfig::output_only(
            number(4),
            "Fourth",
            SecondaryMixSource::Mix(number(2)),
        ));
        assert_eq!(
            validate_secondary_mixes(&configs, &inputs),
            Err(SecondaryMixError::Cycle)
        );
    }

    #[test]
    fn the_list_is_bounded() {
        let inputs = [input(1)];
        let configs = (0..MAX_SECONDARY_MIXES + 1)
            .map(|_| {
                SecondaryMixConfig::output_only(
                    number(2),
                    "Feed",
                    SecondaryMixSource::Input(input(1)),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            validate_secondary_mixes(&configs, &inputs),
            Err(SecondaryMixError::TooManyMixes {
                maximum: MAX_SECONDARY_MIXES,
            })
        );
    }
}
