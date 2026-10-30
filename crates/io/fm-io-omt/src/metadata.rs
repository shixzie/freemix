//! OMT 1.0 control metadata: the exact commands receivers send.
//!
//! The protocol fixes these strings so a sender can match them without an
//! XML parser, including the `Program=="false"` double equals that old
//! implementations send. [`Command::parse`] matches them exactly and falls
//! back to [`Command::Metadata`] for per-frame XML it does not know.

/// `<OMTSubscribe Video="true" />`
pub const SUBSCRIBE_VIDEO: &str = r#"<OMTSubscribe Video="true" />"#;
/// `<OMTSubscribe Audio="true" />`
pub const SUBSCRIBE_AUDIO: &str = r#"<OMTSubscribe Audio="true" />"#;
/// `<OMTSubscribe Metadata="true" />`
pub const SUBSCRIBE_METADATA: &str = r#"<OMTSubscribe Metadata="true" />"#;
/// `<OMTSettings Preview="true" />`
pub const PREVIEW_ON: &str = r#"<OMTSettings Preview="true" />"#;
/// `<OMTSettings Preview="false" />`
pub const PREVIEW_OFF: &str = r#"<OMTSettings Preview="false" />"#;
/// `<OMTTally Preview="true" Program=="false" />` (the protocol's typo).
pub const TALLY_PREVIEW: &str = r#"<OMTTally Preview="true" Program=="false" />"#;
/// `<OMTTally Preview="false" Program=="true" />`
pub const TALLY_PROGRAM: &str = r#"<OMTTally Preview="false" Program=="true" />"#;
/// `<OMTTally Preview="true" Program=="true" />`
pub const TALLY_BOTH: &str = r#"<OMTTally Preview="true" Program=="true" />"#;
/// `<OMTTally Preview="false" Program=="false" />`
pub const TALLY_NONE: &str = r#"<OMTTally Preview="false" Program=="false" />"#;

use crate::frame::FrameKind;

/// One receiver's tally state.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Tally {
    /// The source is on the receiver's Preview.
    pub preview: bool,
    /// The source is on the receiver's Program.
    pub program: bool,
}

impl Tally {
    /// The exact metadata string a receiver sends for this tally.
    #[must_use]
    pub const fn command(self) -> &'static str {
        match (self.preview, self.program) {
            (false, false) => TALLY_NONE,
            (true, false) => TALLY_PREVIEW,
            (false, true) => TALLY_PROGRAM,
            (true, true) => TALLY_BOTH,
        }
    }

    /// Parses one of the four exact tally strings (or the same with a single
    /// equals, which some newer senders write).
    #[must_use]
    pub fn parse(xml: &str) -> Option<Self> {
        let xml = xml.trim();
        if xml == TALLY_NONE || xml == r#"<OMTTally Preview="false" Program="false" />"# {
            return Some(Self::default());
        }
        if xml == TALLY_PREVIEW || xml == r#"<OMTTally Preview="true" Program="false" />"# {
            return Some(Self {
                preview: true,
                program: false,
            });
        }
        if xml == TALLY_PROGRAM || xml == r#"<OMTTally Preview="false" Program="true" />"# {
            return Some(Self {
                preview: false,
                program: true,
            });
        }
        if xml == TALLY_BOTH || xml == r#"<OMTTally Preview="true" Program="true" />"# {
            return Some(Self {
                preview: true,
                program: true,
            });
        }
        None
    }

    /// The combination of two receivers' tallies: either lights the source.
    #[must_use]
    pub const fn or(self, other: Self) -> Self {
        Self {
            preview: self.preview || other.preview,
            program: self.program || other.program,
        }
    }
}

/// The compression quality a receiver suggests.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Quality {
    /// Defer to the other receivers (the protocol's default).
    #[default]
    Default,
    Low,
    Medium,
    High,
}

impl Quality {
    /// The four labels, in protocol order.
    pub const ALL: [Self; 4] = [Self::Default, Self::Low, Self::Medium, Self::High];

    /// The label the setting uses.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }

    /// Parses a label.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|quality| quality.label() == label)
    }

    /// The exact metadata string.
    #[must_use]
    pub fn command(self) -> String {
        format!(r#"<OMTSettings Quality="{}" />"#, self.label())
    }

    /// The stronger of two suggestions, `Default` giving way.
    #[must_use]
    pub const fn strongest(self, other: Self) -> Self {
        match (self, other) {
            (Self::Default, other) => other,
            (this, Self::Default) => this,
            (Self::High, _) | (_, Self::High) => Self::High,
            (Self::Medium, _) | (_, Self::Medium) => Self::Medium,
            (Self::Low, Self::Low) => Self::Low,
        }
    }
}

/// The sender information a source may announce.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SenderInfo {
    pub product_name: String,
    pub manufacturer: String,
    pub version: String,
}

impl SenderInfo {
    /// The exact `<OMTInfo …/>` metadata string, attributes escaped.
    #[must_use]
    pub fn command(&self) -> String {
        format!(
            r#"<OMTInfo ProductName="{}" Manufacturer="{}" Version="{}" />"#,
            escape(&self.product_name),
            escape(&self.manufacturer),
            escape(&self.version)
        )
    }

    /// Parses an `<OMTInfo …/>` record.
    #[must_use]
    pub fn parse(xml: &str) -> Option<Self> {
        let xml = xml.trim();
        if !xml.starts_with("<OMTInfo") {
            return None;
        }
        Some(Self {
            product_name: attribute(xml, "ProductName").unwrap_or_default(),
            manufacturer: attribute(xml, "Manufacturer").unwrap_or_default(),
            version: attribute(xml, "Version").unwrap_or_default(),
        })
    }
}

/// One command a receiver (or the sender) sends over the metadata channel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// The receiver subscribes to one frame kind.
    Subscribe(FrameKind),
    /// The receiver asks for or stops one-eighth preview frames.
    Preview(bool),
    /// The receiver's tally changed.
    Tally(Tally),
    /// The receiver suggests a compression quality.
    Quality(Quality),
    /// A sender announces itself.
    Info(SenderInfo),
    /// Any other XML: per-frame metadata or a command this version does not
    /// interpret.
    Metadata(String),
}

impl Command {
    /// Parses one metadata string.
    #[must_use]
    pub fn parse(xml: &str) -> Self {
        let trimmed = xml.trim();
        if trimmed == SUBSCRIBE_VIDEO {
            return Self::Subscribe(FrameKind::Video);
        }
        if trimmed == SUBSCRIBE_AUDIO {
            return Self::Subscribe(FrameKind::Audio);
        }
        if trimmed == SUBSCRIBE_METADATA {
            return Self::Subscribe(FrameKind::Metadata);
        }
        if trimmed == PREVIEW_ON {
            return Self::Preview(true);
        }
        if trimmed == PREVIEW_OFF {
            return Self::Preview(false);
        }
        if let Some(tally) = Tally::parse(trimmed) {
            return Self::Tally(tally);
        }
        if let Some(quality) = quality(trimmed) {
            return Self::Quality(quality);
        }
        if let Some(info) = SenderInfo::parse(trimmed) {
            return Self::Info(info);
        }
        Self::Metadata(xml.to_owned())
    }

    /// The exact metadata string for a command this implementation sends.
    #[must_use]
    pub fn command(&self) -> String {
        match self {
            Self::Subscribe(FrameKind::Video) => SUBSCRIBE_VIDEO.to_owned(),
            Self::Subscribe(FrameKind::Audio) => SUBSCRIBE_AUDIO.to_owned(),
            Self::Subscribe(FrameKind::Metadata) => SUBSCRIBE_METADATA.to_owned(),
            Self::Preview(true) => PREVIEW_ON.to_owned(),
            Self::Preview(false) => PREVIEW_OFF.to_owned(),
            Self::Tally(tally) => tally.command().to_owned(),
            Self::Quality(quality) => quality.command(),
            Self::Info(info) => info.command(),
            Self::Metadata(xml) => xml.clone(),
        }
    }
}

fn quality(xml: &str) -> Option<Quality> {
    let rest = xml.strip_prefix(r#"<OMTSettings Quality=""#)?;
    let label = rest.strip_suffix(r#"" />"#)?;
    Quality::from_label(label)
}

fn attribute(xml: &str, name: &str) -> Option<String> {
    let start = xml.find(&format!("{name}=\""))? + name.len() + 2;
    let rest = &xml[start..];
    let end = rest.find('"')?;
    Some(unescape(&rest[..end]))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}
