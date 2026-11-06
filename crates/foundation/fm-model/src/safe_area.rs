//! Operator safe-area guide selection (SW-014).
//!
//! The project stores which operator guides the console draws over Program
//! and Preview. Guides never reach Program, recordings, streams, or outputs;
//! these flags are show configuration, so every console showing the project
//! sees the same selection.

/// One safe-area guide selection.
#[allow(clippy::struct_excessive_bools)] // one independent guide switch each
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SafeAreaGuideSelection {
    /// The centered 93% action-safe rectangle.
    pub action: bool,
    /// The centered 90% title-safe rectangle.
    pub title: bool,
    /// The largest centered 4:3 cut.
    pub center_4_3: bool,
    /// The largest centered 16:9 cut.
    pub center_16_9: bool,
    /// The largest centered 9:16 cut.
    pub center_9_16: bool,
}

impl Default for SafeAreaGuideSelection {
    /// Title safe on, matching the guide the console always drew.
    fn default() -> Self {
        Self {
            action: false,
            title: true,
            center_4_3: false,
            center_16_9: false,
            center_9_16: false,
        }
    }
}

impl SafeAreaGuideSelection {
    /// No guides.
    pub const NONE: Self = Self {
        action: false,
        title: false,
        center_4_3: false,
        center_16_9: false,
        center_9_16: false,
    };

    /// Whether no guide is selected.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        !(self.action || self.title || self.center_4_3 || self.center_16_9 || self.center_9_16)
    }

    /// The canonical token list: `-` for none, else comma-separated
    /// `action`, `title`, `4:3`, `16:9`, and `9:16` in that order.
    #[must_use]
    pub fn tokens(self) -> String {
        if self.is_empty() {
            return "-".to_owned();
        }
        let mut tokens = Vec::new();
        if self.action {
            tokens.push("action");
        }
        if self.title {
            tokens.push("title");
        }
        if self.center_4_3 {
            tokens.push("4:3");
        }
        if self.center_16_9 {
            tokens.push("16:9");
        }
        if self.center_9_16 {
            tokens.push("9:16");
        }
        tokens.join(",")
    }

    /// Parses the canonical token list; `None` for an unknown or repeated
    /// token, or for an empty string (use `-` for none).
    #[must_use]
    pub fn from_tokens(value: &str) -> Option<Self> {
        if value == "-" {
            return Some(Self::NONE);
        }
        let mut guides = Self::NONE;
        let mut any = false;
        for token in value.split(',') {
            let slot = match token {
                "action" => &mut guides.action,
                "title" => &mut guides.title,
                "4:3" => &mut guides.center_4_3,
                "16:9" => &mut guides.center_16_9,
                "9:16" => &mut guides.center_9_16,
                _ => return None,
            };
            if *slot {
                return None;
            }
            *slot = true;
            any = true;
        }
        any.then_some(guides)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_list_is_canonical_and_strict() {
        assert_eq!(SafeAreaGuideSelection::default().tokens(), "title");
        assert_eq!(SafeAreaGuideSelection::NONE.tokens(), "-");
        let all = SafeAreaGuideSelection {
            action: true,
            title: true,
            center_4_3: true,
            center_16_9: true,
            center_9_16: true,
        };
        assert_eq!(all.tokens(), "action,title,4:3,16:9,9:16");
        assert_eq!(
            SafeAreaGuideSelection::from_tokens(&all.tokens()),
            Some(all)
        );
        assert_eq!(
            SafeAreaGuideSelection::from_tokens("title,action"),
            Some(SafeAreaGuideSelection {
                action: true,
                title: true,
                ..SafeAreaGuideSelection::NONE
            })
        );
        assert_eq!(
            SafeAreaGuideSelection::from_tokens("-"),
            Some(SafeAreaGuideSelection::NONE)
        );
        for invalid in ["", "action,action", "safe", "4:2", "title,"] {
            assert_eq!(
                SafeAreaGuideSelection::from_tokens(invalid),
                None,
                "{invalid}"
            );
        }
    }
}
