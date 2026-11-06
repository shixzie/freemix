//! Operator safe-area guide selection on the wire (SW-014, Protocol 2.172).
//!
//! The snapshot carries `safe_area_guides`; `set_safe_area_guides` replaces
//! it as one padlocked, undoable project edit, and `safe_area_guides_changed`
//! announces the new selection. The canonical token list is `-` for none or
//! a comma-separated subset of `action`, `title`, `4:3`, `16:9`, and `9:16`
//! in that order.

/// One operator safe-area guide selection.
#[allow(clippy::struct_excessive_bools)] // one independent guide switch each
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WireSafeAreaGuides {
    pub action: bool,
    pub title: bool,
    pub center_4_3: bool,
    pub center_16_9: bool,
    pub center_9_16: bool,
}

impl Default for WireSafeAreaGuides {
    /// Title safe on, the console's historical default.
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

impl WireSafeAreaGuides {
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

    /// The canonical token list.
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

    /// Parses a canonical token list; `None` for an unknown, repeated, or
    /// empty token.
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
        assert_eq!(WireSafeAreaGuides::default().tokens(), "title");
        assert_eq!(WireSafeAreaGuides::NONE.tokens(), "-");
        let all = WireSafeAreaGuides {
            action: true,
            title: true,
            center_4_3: true,
            center_16_9: true,
            center_9_16: true,
        };
        assert_eq!(all.tokens(), "action,title,4:3,16:9,9:16");
        assert_eq!(WireSafeAreaGuides::from_tokens(&all.tokens()), Some(all));
        for invalid in ["", "action,action", "safe", "4:2"] {
            assert_eq!(WireSafeAreaGuides::from_tokens(invalid), None, "{invalid}");
        }
    }
}
