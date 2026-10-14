//! Strict, bounded SDP offer/answer subset for encrypted browser guests.
//!
//! The accepted dialect is deliberately narrow: `UDP/TLS/RTP/SAVPF` audio and
//! video sections only, Opus/H.264/VP8 codecs, mandatory DTLS `sha-256`
//! fingerprints and ICE credentials, mandatory `rtcp-mux`, a single BUNDLE
//! group covering every section, and no simulcast. Browser attributes that do
//! not affect this subset (`extmap`, `msid`, `ssrc`, ...) are recognized and
//! dropped; anything else is rejected with an explicit [`SdpError`].
//!
//! Rendering is canonical: CRLF line endings, media-level transport
//! attributes, and a fixed attribute order, so `parse(render(x)) == x`.

use core::fmt;
use std::fmt::Write as _;
use std::net::IpAddr;

/// Maximum accepted SDP text size in bytes.
pub const MAX_SDP_BYTES: usize = 16 * 1024;
/// Maximum number of SDP lines.
pub const MAX_SDP_LINES: usize = 512;
/// Maximum bytes per SDP line, excluding the CRLF terminator.
pub const MAX_LINE_BYTES: usize = 1024;
/// Maximum number of media sections per description.
pub const MAX_MEDIA_SECTIONS: usize = 4;
/// Maximum payload types listed on one `m=` line.
pub const MAX_PAYLOAD_TYPES: usize = 64;
/// Maximum supported codecs retained per media section.
pub const MAX_CODECS_PER_SECTION: usize = 16;
/// Maximum `fmtp` parameters per codec.
pub const MAX_FMTP_PARAMS: usize = 12;
/// Maximum RTCP feedback entries per codec.
pub const MAX_FEEDBACK_PER_CODEC: usize = 8;
/// Maximum candidates embedded in one media section.
pub const MAX_CANDIDATES_PER_SECTION: usize = 16;
/// Return/guest video bitrate ceiling in bits per second (RC-001: 4 Mbps).
pub const MAX_VIDEO_BITRATE_BPS: u64 = 4_000_000;

const MAX_MID_BYTES: usize = 64;
const MAX_FOUNDATION_BYTES: usize = 32;
const MAX_HOSTNAME_BYTES: usize = 253;
const MAX_CANDIDATE_TOKENS: usize = 24;
const FIRST_DYNAMIC_PAYLOAD_TYPE: u8 = 96;
const LAST_PAYLOAD_TYPE: u8 = 127;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SdpError {
    TooLarge { actual: usize, maximum: usize },
    TooManyLines { maximum: usize },
    LineTooLong { line: usize, maximum: usize },
    MissingFinalCrlf,
    BareLineEnding { line: usize },
    MalformedLine { line: usize },
    UnexpectedLine { line: usize, kind: char },
    UnsupportedLine { line: usize, kind: char },
    UnsupportedVersion,
    InvalidOrigin,
    InvalidSessionName,
    InvalidTiming,
    InvalidConnection,
    InvalidBandwidth,
    InvalidMediaLine,
    UnsupportedMediaKind(String),
    InsecureProfile(String),
    RejectedMediaSection { mid: Option<String> },
    TooManyMediaSections { maximum: usize },
    NoMediaSections,
    TooManyPayloadTypes { maximum: usize },
    InvalidPayloadType,
    DuplicatePayloadType(u8),
    UnlistedPayloadType(u8),
    DuplicateAttribute(&'static str),
    UnsupportedAttribute(String),
    UnsupportedGroup(String),
    SimulcastUnsupported,
    InvalidMid,
    MissingMid,
    DuplicateMid(String),
    MissingIceCredentials { mid: String },
    InvalidIceUfrag,
    InvalidIcePwd,
    MissingFingerprint { mid: String },
    UnsupportedFingerprintAlgorithm(String),
    InvalidFingerprint,
    MissingSetup { mid: String },
    InvalidSetup,
    RtcpMuxRequired { mid: String },
    InvalidRtpmap,
    CodecKindMismatch { payload_type: u8 },
    PreferenceKindMismatch(CodecName),
    InvalidFmtp,
    UnsupportedFmtpParameter(String),
    TooManyFmtpParameters { maximum: usize },
    InvalidRtcpFeedback,
    UnsupportedRtcpFeedback(String),
    TooManyFeedback { maximum: usize },
    TooManyCodecs { maximum: usize },
    NoSupportedCodec { mid: String },
    InvalidCandidate,
    UnsupportedCandidateComponent,
    UnsupportedCandidateTransport(String),
    UnsupportedCandidateType(String),
    UnsupportedCandidateExtension(String),
    TooManyCandidates { maximum: usize },
    UnbundledMedia { mid: String },
    UnknownBundleMid(String),
    InconsistentBundleTransport,
    NoCommonCodec { mid: String },
    EmptyCodecPreferences,
    InvalidAnswerSetup,
    BitrateCapExceeded { requested: u64, maximum: u64 },
    PayloadTypesExhausted,
    AnswerMismatch(&'static str),
}

impl fmt::Display for SdpError {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { actual, maximum } => {
                write!(formatter, "SDP is {actual} bytes; maximum is {maximum}")
            }
            Self::TooManyLines { maximum } => write!(formatter, "SDP exceeds {maximum} lines"),
            Self::LineTooLong { line, maximum } => {
                write!(formatter, "SDP line {line} exceeds {maximum} bytes")
            }
            Self::MissingFinalCrlf => formatter.write_str("SDP must end with CRLF"),
            Self::BareLineEnding { line } => {
                write!(formatter, "SDP line {line} uses a bare CR or LF")
            }
            Self::MalformedLine { line } => write!(formatter, "SDP line {line} is malformed"),
            Self::UnexpectedLine { line, kind } => {
                write!(formatter, "SDP line {line} ({kind}=) is out of order")
            }
            Self::UnsupportedLine { line, kind } => {
                write!(formatter, "SDP line {line} ({kind}=) is not supported")
            }
            Self::UnsupportedVersion => formatter.write_str("only SDP version 0 is supported"),
            Self::InvalidOrigin => formatter.write_str("invalid o= line"),
            Self::InvalidSessionName => formatter.write_str("invalid s= line"),
            Self::InvalidTiming => formatter.write_str("t= must be `0 0`"),
            Self::InvalidConnection => formatter.write_str("invalid c= line"),
            Self::InvalidBandwidth => formatter.write_str("invalid b= line"),
            Self::InvalidMediaLine => formatter.write_str("invalid m= line"),
            Self::UnsupportedMediaKind(kind) => write!(formatter, "media kind {kind} unsupported"),
            Self::InsecureProfile(profile) => {
                write!(formatter, "transport profile {profile} is not DTLS-SRTP")
            }
            Self::RejectedMediaSection { mid } => {
                write!(formatter, "media section {mid:?} has port 0")
            }
            Self::TooManyMediaSections { maximum } => {
                write!(formatter, "more than {maximum} media sections")
            }
            Self::NoMediaSections => formatter.write_str("SDP has no media sections"),
            Self::TooManyPayloadTypes { maximum } => {
                write!(
                    formatter,
                    "more than {maximum} payload types on one m= line"
                )
            }
            Self::InvalidPayloadType => formatter.write_str("invalid RTP payload type"),
            Self::DuplicatePayloadType(pt) => write!(formatter, "payload type {pt} repeated"),
            Self::UnlistedPayloadType(pt) => {
                write!(formatter, "payload type {pt} is not listed on the m= line")
            }
            Self::DuplicateAttribute(name) => write!(formatter, "attribute {name} repeated"),
            Self::UnsupportedAttribute(name) => write!(formatter, "attribute {name} unsupported"),
            Self::UnsupportedGroup(semantics) => {
                write!(formatter, "group semantics {semantics} unsupported")
            }
            Self::SimulcastUnsupported => formatter.write_str("simulcast is not supported"),
            Self::InvalidMid => formatter.write_str("invalid mid"),
            Self::MissingMid => formatter.write_str("media section has no mid"),
            Self::DuplicateMid(mid) => write!(formatter, "mid {mid} repeated"),
            Self::MissingIceCredentials { mid } => {
                write!(formatter, "media section {mid} has no ICE credentials")
            }
            Self::InvalidIceUfrag => formatter.write_str("invalid ice-ufrag"),
            Self::InvalidIcePwd => formatter.write_str("invalid ice-pwd"),
            Self::MissingFingerprint { mid } => write!(
                formatter,
                "media section {mid} has no DTLS fingerprint; encryption is mandatory"
            ),
            Self::UnsupportedFingerprintAlgorithm(algorithm) => {
                write!(formatter, "fingerprint algorithm {algorithm} unsupported")
            }
            Self::InvalidFingerprint => formatter.write_str("invalid sha-256 fingerprint"),
            Self::MissingSetup { mid } => write!(formatter, "media section {mid} has no setup"),
            Self::InvalidSetup => formatter.write_str("invalid setup role"),
            Self::RtcpMuxRequired { mid } => {
                write!(formatter, "media section {mid} lacks rtcp-mux")
            }
            Self::InvalidRtpmap => formatter.write_str("invalid rtpmap"),
            Self::CodecKindMismatch { payload_type } => {
                write!(formatter, "codec {payload_type} does not match media kind")
            }
            Self::PreferenceKindMismatch(codec) => {
                write!(
                    formatter,
                    "codec preference {codec:?} is in the wrong media list"
                )
            }
            Self::InvalidFmtp => formatter.write_str("invalid fmtp"),
            Self::UnsupportedFmtpParameter(key) => write!(formatter, "fmtp {key} unsupported"),
            Self::TooManyFmtpParameters { maximum } => {
                write!(formatter, "more than {maximum} fmtp parameters")
            }
            Self::InvalidRtcpFeedback => formatter.write_str("invalid rtcp-fb"),
            Self::UnsupportedRtcpFeedback(value) => {
                write!(formatter, "rtcp-fb {value} unsupported")
            }
            Self::TooManyFeedback { maximum } => {
                write!(formatter, "more than {maximum} rtcp-fb entries per codec")
            }
            Self::TooManyCodecs { maximum } => {
                write!(
                    formatter,
                    "more than {maximum} supported codecs in a section"
                )
            }
            Self::NoSupportedCodec { mid } => {
                write!(formatter, "media section {mid} has no Opus/H.264/VP8 codec")
            }
            Self::InvalidCandidate => formatter.write_str("invalid ICE candidate"),
            Self::UnsupportedCandidateComponent => {
                formatter.write_str("only ICE component 1 (rtcp-mux) is supported")
            }
            Self::UnsupportedCandidateTransport(transport) => {
                write!(formatter, "candidate transport {transport} unsupported")
            }
            Self::UnsupportedCandidateType(kind) => {
                write!(formatter, "candidate type {kind} unsupported")
            }
            Self::UnsupportedCandidateExtension(key) => {
                write!(formatter, "candidate extension {key} unsupported")
            }
            Self::TooManyCandidates { maximum } => {
                write!(formatter, "more than {maximum} ICE candidates")
            }
            Self::UnbundledMedia { mid } => {
                write!(formatter, "media section {mid} is not in the BUNDLE group")
            }
            Self::UnknownBundleMid(mid) => write!(formatter, "BUNDLE names unknown mid {mid}"),
            Self::InconsistentBundleTransport => {
                formatter.write_str("bundled sections disagree on ICE/DTLS parameters")
            }
            Self::NoCommonCodec { mid } => {
                write!(
                    formatter,
                    "media section {mid} shares no codec with local preferences"
                )
            }
            Self::EmptyCodecPreferences => formatter.write_str("no local codec preferences"),
            Self::InvalidAnswerSetup => {
                formatter.write_str("answer setup must be active or passive")
            }
            Self::BitrateCapExceeded { requested, maximum } => {
                write!(formatter, "bitrate {requested} exceeds {maximum} bps")
            }
            Self::PayloadTypesExhausted => formatter.write_str("dynamic payload types exhausted"),
            Self::AnswerMismatch(reason) => {
                write!(formatter, "answer does not match offer: {reason}")
            }
        }
    }
}

impl std::error::Error for SdpError {}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SdpMediaKind {
    Audio,
    Video,
}

impl SdpMediaKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SdpDirection {
    SendRecv,
    SendOnly,
    RecvOnly,
    Inactive,
}

impl SdpDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SendRecv => "sendrecv",
            Self::SendOnly => "sendonly",
            Self::RecvOnly => "recvonly",
            Self::Inactive => "inactive",
        }
    }

    #[must_use]
    pub const fn sends(self) -> bool {
        matches!(self, Self::SendRecv | Self::SendOnly)
    }

    #[must_use]
    pub const fn receives(self) -> bool {
        matches!(self, Self::SendRecv | Self::RecvOnly)
    }

    #[must_use]
    pub const fn from_flags(sends: bool, receives: bool) -> Self {
        match (sends, receives) {
            (true, true) => Self::SendRecv,
            (true, false) => Self::SendOnly,
            (false, true) => Self::RecvOnly,
            (false, false) => Self::Inactive,
        }
    }

    /// The direction as seen from the other endpoint.
    #[must_use]
    pub const fn reversed(self) -> Self {
        Self::from_flags(self.receives(), self.sends())
    }

    /// The directions permitted by both `self` and `other`.
    #[must_use]
    pub const fn intersect(self, other: Self) -> Self {
        Self::from_flags(
            self.sends() && other.sends(),
            self.receives() && other.receives(),
        )
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "sendrecv" => Some(Self::SendRecv),
            "sendonly" => Some(Self::SendOnly),
            "recvonly" => Some(Self::RecvOnly),
            "inactive" => Some(Self::Inactive),
            _ => None,
        }
    }
}

/// DTLS role (`a=setup`).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SetupRole {
    ActPass,
    Active,
    Passive,
}

impl SetupRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ActPass => "actpass",
            Self::Active => "active",
            Self::Passive => "passive",
        }
    }

    fn parse(value: &str) -> Result<Self, SdpError> {
        match value {
            "actpass" => Ok(Self::ActPass),
            "active" => Ok(Self::Active),
            "passive" => Ok(Self::Passive),
            _ => Err(SdpError::InvalidSetup),
        }
    }
}

/// ICE username fragment and password. Debug output redacts the password.
#[derive(Clone, Eq, Hash, PartialEq)]
pub struct IceCredentials {
    ufrag: String,
    pwd: String,
}

impl IceCredentials {
    /// # Errors
    /// Returns [`SdpError::InvalidIceUfrag`] or [`SdpError::InvalidIcePwd`]
    /// unless the ufrag is 4–256 and the password 22–256 ICE characters.
    pub fn new(ufrag: impl Into<String>, pwd: impl Into<String>) -> Result<Self, SdpError> {
        let ufrag = ufrag.into();
        let pwd = pwd.into();
        if !is_ice_chars(&ufrag, 4, 256) {
            return Err(SdpError::InvalidIceUfrag);
        }
        if !is_ice_chars(&pwd, 22, 256) {
            return Err(SdpError::InvalidIcePwd);
        }
        Ok(Self { ufrag, pwd })
    }

    #[must_use]
    pub fn ufrag(&self) -> &str {
        &self.ufrag
    }

    #[must_use]
    pub fn pwd(&self) -> &str {
        &self.pwd
    }
}

impl fmt::Debug for IceCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IceCredentials")
            .field("ufrag", &self.ufrag)
            .field("pwd", &"[REDACTED]")
            .finish()
    }
}

/// A DTLS certificate `sha-256` fingerprint.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    #[must_use]
    pub const fn new(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    #[must_use]
    pub const fn digest(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parses the value of `a=fingerprint:` (`sha-256 AB:CD:...`).
    ///
    /// # Errors
    /// Returns an unsupported-algorithm error for anything but `sha-256`, or
    /// [`SdpError::InvalidFingerprint`] unless exactly 32 colon-separated hex
    /// octets follow.
    pub fn parse(value: &str) -> Result<Self, SdpError> {
        let (algorithm, hex) = value.split_once(' ').ok_or(SdpError::InvalidFingerprint)?;
        if !algorithm.eq_ignore_ascii_case("sha-256") {
            return Err(SdpError::UnsupportedFingerprintAlgorithm(
                algorithm.to_ascii_lowercase(),
            ));
        }
        let mut digest = [0_u8; 32];
        let mut count = 0_usize;
        for octet in hex.split(':') {
            let slot = digest.get_mut(count).ok_or(SdpError::InvalidFingerprint)?;
            if octet.len() != 2 {
                return Err(SdpError::InvalidFingerprint);
            }
            *slot = u8::from_str_radix(octet, 16).map_err(|_| SdpError::InvalidFingerprint)?;
            count += 1;
        }
        if count != digest.len() {
            return Err(SdpError::InvalidFingerprint);
        }
        Ok(Self(digest))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("sha-256 ")?;
        for (index, octet) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str(":")?;
            }
            write!(formatter, "{octet:02X}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CodecName {
    Opus,
    H264,
    Vp8,
}

impl CodecName {
    #[must_use]
    pub const fn kind(self) -> SdpMediaKind {
        match self {
            Self::Opus => SdpMediaKind::Audio,
            Self::H264 | Self::Vp8 => SdpMediaKind::Video,
        }
    }

    #[must_use]
    pub const fn rtpmap(self) -> &'static str {
        match self {
            Self::Opus => "opus/48000/2",
            Self::H264 => "H264/90000",
            Self::Vp8 => "VP8/90000",
        }
    }

    #[must_use]
    pub const fn clock_rate(self) -> u32 {
        match self {
            Self::Opus => 48_000,
            Self::H264 | Self::Vp8 => 90_000,
        }
    }

    /// Returns `Ok(None)` for well-formed encodings outside the subset.
    fn parse_rtpmap(encoding: &str) -> Result<Option<Self>, SdpError> {
        let mut parts = encoding.split('/');
        let name = parts.next().ok_or(SdpError::InvalidRtpmap)?;
        let clock = parts.next().ok_or(SdpError::InvalidRtpmap)?;
        let channels = parts.next();
        if parts.next().is_some() || name.is_empty() || clock.parse::<u32>().is_err() {
            return Err(SdpError::InvalidRtpmap);
        }
        let codec = if name.eq_ignore_ascii_case("opus") {
            Self::Opus
        } else if name.eq_ignore_ascii_case("h264") {
            Self::H264
        } else if name.eq_ignore_ascii_case("vp8") {
            Self::Vp8
        } else {
            return Ok(None);
        };
        let expected_channels = match codec {
            Self::Opus => Some("2"),
            Self::H264 | Self::Vp8 => None,
        };
        if clock != codec.clock_rate().to_string() || channels != expected_channels {
            return Err(SdpError::InvalidRtpmap);
        }
        Ok(Some(codec))
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct FmtpParam {
    pub key: String,
    pub value: String,
}

impl FmtpParam {
    #[must_use]
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RtcpFeedback {
    Nack,
    NackPli,
    CcmFir,
    GoogRemb,
    TransportCc,
}

impl RtcpFeedback {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Nack => "nack",
            Self::NackPli => "nack pli",
            Self::CcmFir => "ccm fir",
            Self::GoogRemb => "goog-remb",
            Self::TransportCc => "transport-cc",
        }
    }

    fn parse(value: &str) -> Result<Self, SdpError> {
        match value {
            "nack" => Ok(Self::Nack),
            "nack pli" => Ok(Self::NackPli),
            "ccm fir" => Ok(Self::CcmFir),
            "goog-remb" => Ok(Self::GoogRemb),
            "transport-cc" => Ok(Self::TransportCc),
            other => Err(SdpError::UnsupportedRtcpFeedback(other.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SdpCodec {
    pub payload_type: u8,
    pub name: CodecName,
    pub fmtp: Vec<FmtpParam>,
    pub feedback: Vec<RtcpFeedback>,
}

impl SdpCodec {
    #[must_use]
    pub fn fmtp_value(&self, key: &str) -> Option<&str> {
        fmtp_value(&self.fmtp, key)
    }

    /// H.264 `profile_idc` (RFC 6184 default `0x42`); `None` for other codecs.
    #[must_use]
    pub fn h264_profile_idc(&self) -> Option<u8> {
        (self.name == CodecName::H264).then(|| h264_profile_idc(&self.fmtp))
    }

    /// H.264 `packetization-mode` (default 0); `None` for other codecs.
    #[must_use]
    pub fn h264_packetization_mode(&self) -> Option<u8> {
        (self.name == CodecName::H264).then(|| h264_packetization_mode(&self.fmtp))
    }
}

/// A local codec preference for offers and answers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodecPreference {
    pub name: CodecName,
    pub fmtp: Vec<FmtpParam>,
    pub feedback: Vec<RtcpFeedback>,
}

impl CodecPreference {
    /// Opus with in-band FEC and 10 ms minimum packetization.
    #[must_use]
    pub fn opus() -> Self {
        Self {
            name: CodecName::Opus,
            fmtp: vec![
                FmtpParam::new("minptime", "10"),
                FmtpParam::new("useinbandfec", "1"),
            ],
            feedback: vec![RtcpFeedback::TransportCc],
        }
    }

    /// VP8 with NACK/PLI/FIR, REMB, and transport-wide congestion control.
    #[must_use]
    pub fn vp8() -> Self {
        Self {
            name: CodecName::Vp8,
            fmtp: Vec::new(),
            feedback: video_feedback(),
        }
    }

    /// H.264 constrained baseline level 3.1, packetization mode 1.
    #[must_use]
    pub fn h264_constrained_baseline() -> Self {
        Self {
            name: CodecName::H264,
            fmtp: vec![
                FmtpParam::new("level-asymmetry-allowed", "1"),
                FmtpParam::new("packetization-mode", "1"),
                FmtpParam::new("profile-level-id", "42e01f"),
            ],
            feedback: video_feedback(),
        }
    }

    fn accepts(&self, offered: &SdpCodec) -> bool {
        if self.name != offered.name {
            return false;
        }
        match self.name {
            CodecName::H264 => {
                h264_profile_idc(&self.fmtp) == h264_profile_idc(&offered.fmtp)
                    && h264_packetization_mode(&self.fmtp) == h264_packetization_mode(&offered.fmtp)
            }
            CodecName::Opus | CodecName::Vp8 => true,
        }
    }
}

fn video_feedback() -> Vec<RtcpFeedback> {
    vec![
        RtcpFeedback::GoogRemb,
        RtcpFeedback::TransportCc,
        RtcpFeedback::CcmFir,
        RtcpFeedback::Nack,
        RtcpFeedback::NackPli,
    ]
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CandidateType {
    Host,
    ServerReflexive,
    Relay,
}

impl CandidateType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::ServerReflexive => "srflx",
            Self::Relay => "relay",
        }
    }
}

/// A UDP, component-1 ICE candidate (`candidate:` attribute value).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SdpCandidate {
    pub foundation: String,
    pub priority: u32,
    /// An IP literal or an mDNS `.local` hostname.
    pub address: String,
    pub port: u16,
    pub kind: CandidateType,
    pub related: Option<(String, u16)>,
    pub ufrag: Option<String>,
}

impl SdpCandidate {
    /// Parses a trickled `candidate:...` string (as carried in browser
    /// `RTCIceCandidate.candidate`).
    ///
    /// # Errors
    /// Returns a candidate error for malformed fields, non-UDP transports,
    /// components other than 1, `prflx` types, or unknown extensions.
    pub fn parse(text: &str) -> Result<Self, SdpError> {
        let value = text
            .strip_prefix("candidate:")
            .ok_or(SdpError::InvalidCandidate)?;
        Self::parse_value(value)
    }

    fn parse_value(value: &str) -> Result<Self, SdpError> {
        if value.len() > MAX_LINE_BYTES {
            return Err(SdpError::InvalidCandidate);
        }
        let tokens: Vec<&str> = value.split(' ').collect();
        if tokens.len() > MAX_CANDIDATE_TOKENS || tokens.iter().any(|token| token.is_empty()) {
            return Err(SdpError::InvalidCandidate);
        }
        let [
            foundation,
            component,
            transport,
            priority,
            address,
            port,
            typ,
            kind,
            rest @ ..,
        ] = tokens.as_slice()
        else {
            return Err(SdpError::InvalidCandidate);
        };
        if !is_ice_chars(foundation, 1, MAX_FOUNDATION_BYTES) || *typ != "typ" {
            return Err(SdpError::InvalidCandidate);
        }
        if *component != "1" {
            return Err(SdpError::UnsupportedCandidateComponent);
        }
        if !transport.eq_ignore_ascii_case("udp") {
            return Err(SdpError::UnsupportedCandidateTransport(
                transport.to_ascii_lowercase(),
            ));
        }
        let priority = parse_nonzero_u32(priority).ok_or(SdpError::InvalidCandidate)?;
        validate_address(address)?;
        let port = parse_nonzero_u16(port).ok_or(SdpError::InvalidCandidate)?;
        let kind = match *kind {
            "host" => CandidateType::Host,
            "srflx" => CandidateType::ServerReflexive,
            "relay" => CandidateType::Relay,
            other => return Err(SdpError::UnsupportedCandidateType(other.to_owned())),
        };
        if rest.len() % 2 != 0 {
            return Err(SdpError::InvalidCandidate);
        }
        let mut raddr = None;
        let mut rport = None;
        let mut ufrag = None;
        let mut seen: Vec<&str> = Vec::new();
        for pair in rest.as_chunks::<2>().0 {
            let (key, value) = (pair[0], pair[1]);
            if seen.contains(&key) {
                return Err(SdpError::InvalidCandidate);
            }
            seen.push(key);
            match key {
                "raddr" => {
                    validate_address(value)?;
                    raddr = Some(value.to_owned());
                }
                "rport" => {
                    rport = Some(
                        value
                            .parse::<u16>()
                            .map_err(|_| SdpError::InvalidCandidate)?,
                    );
                }
                "ufrag" => {
                    if !is_ice_chars(value, 4, 256) {
                        return Err(SdpError::InvalidCandidate);
                    }
                    ufrag = Some(value.to_owned());
                }
                "generation" | "network-id" | "network-cost" => {
                    value
                        .parse::<u32>()
                        .map_err(|_| SdpError::InvalidCandidate)?;
                }
                other => return Err(SdpError::UnsupportedCandidateExtension(other.to_owned())),
            }
        }
        let related = match (raddr, rport) {
            (None, None) => None,
            (Some(address), Some(port)) if kind != CandidateType::Host => Some((address, port)),
            _ => return Err(SdpError::InvalidCandidate),
        };
        Ok(Self {
            foundation: (*foundation).to_owned(),
            priority,
            address: (*address).to_owned(),
            port,
            kind,
            related,
            ufrag,
        })
    }
}

impl fmt::Display for SdpCandidate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "candidate:{} 1 udp {} {} {} typ {}",
            self.foundation,
            self.priority,
            self.address,
            self.port,
            self.kind.as_str()
        )?;
        if let Some((address, port)) = &self.related {
            write!(formatter, " raddr {address} rport {port}")?;
        }
        if let Some(ufrag) = &self.ufrag {
            write!(formatter, " ufrag {ufrag}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Origin {
    pub username: String,
    pub session_id: u64,
    pub session_version: u64,
    pub address: IpAddr,
}

impl Origin {
    fn parse(value: &str) -> Result<Self, SdpError> {
        let tokens: Vec<&str> = value.split(' ').collect();
        let [username, session_id, session_version, "IN", family, address] = tokens.as_slice()
        else {
            return Err(SdpError::InvalidOrigin);
        };
        if username.is_empty() || !username.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(SdpError::InvalidOrigin);
        }
        let address: IpAddr = address.parse().map_err(|_| SdpError::InvalidOrigin)?;
        if address_family(address) != *family {
            return Err(SdpError::InvalidOrigin);
        }
        Ok(Self {
            username: (*username).to_owned(),
            session_id: session_id.parse().map_err(|_| SdpError::InvalidOrigin)?,
            session_version: session_version
                .parse()
                .map_err(|_| SdpError::InvalidOrigin)?,
            address,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaSection {
    pub kind: SdpMediaKind,
    pub port: u16,
    pub mid: String,
    pub direction: SdpDirection,
    pub ice: IceCredentials,
    pub fingerprint: Fingerprint,
    pub setup: SetupRole,
    /// Supported codecs in `m=` line order (unsupported payloads are dropped).
    pub codecs: Vec<SdpCodec>,
    /// `b=TIAS` (or `b=AS` × 1000) receive ceiling.
    pub max_bitrate_bps: Option<u64>,
    pub candidates: Vec<SdpCandidate>,
    pub end_of_candidates: bool,
}

/// A parsed description in the supported subset.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Sdp {
    pub origin: Origin,
    /// Mids in the single BUNDLE group (empty only for one-section SDP).
    pub bundle: Vec<String>,
    pub media: Vec<MediaSection>,
}

/// Local endpoint parameters used to build offers and answers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalEndpoint {
    pub ice: IceCredentials,
    pub fingerprint: Fingerprint,
    /// Role taken when the remote offer says `actpass` (`Active` or `Passive`).
    pub setup_when_actpass: SetupRole,
    pub audio: Vec<CodecPreference>,
    pub video: Vec<CodecPreference>,
    pub audio_direction: SdpDirection,
    pub video_direction: SdpDirection,
    /// Advertised video receive ceiling (at most [`MAX_VIDEO_BITRATE_BPS`]).
    pub max_video_bitrate_bps: u64,
}

impl LocalEndpoint {
    /// # Errors
    /// Rejects an `actpass` answer role, an empty or oversize codec list,
    /// codecs of the wrong kind, invalid fmtp, or a bitrate cap above 4 Mbps.
    pub fn validate(&self) -> Result<(), SdpError> {
        if self.setup_when_actpass == SetupRole::ActPass {
            return Err(SdpError::InvalidAnswerSetup);
        }
        if self.audio.is_empty() && self.video.is_empty() {
            return Err(SdpError::EmptyCodecPreferences);
        }
        if self.max_video_bitrate_bps == 0 || self.max_video_bitrate_bps > MAX_VIDEO_BITRATE_BPS {
            return Err(SdpError::BitrateCapExceeded {
                requested: self.max_video_bitrate_bps,
                maximum: MAX_VIDEO_BITRATE_BPS,
            });
        }
        for (kind, preferences) in [
            (SdpMediaKind::Audio, &self.audio),
            (SdpMediaKind::Video, &self.video),
        ] {
            if preferences.len() > MAX_CODECS_PER_SECTION {
                return Err(SdpError::TooManyCodecs {
                    maximum: MAX_CODECS_PER_SECTION,
                });
            }
            for preference in preferences {
                if preference.name.kind() != kind {
                    return Err(SdpError::PreferenceKindMismatch(preference.name));
                }
                validate_fmtp(preference.name, &preference.fmtp)?;
                validate_feedback_list(&preference.feedback)?;
            }
        }
        Ok(())
    }

    fn preferences(&self, kind: SdpMediaKind) -> (&[CodecPreference], SdpDirection) {
        match kind {
            SdpMediaKind::Audio => (&self.audio, self.audio_direction),
            SdpMediaKind::Video => (&self.video, self.video_direction),
        }
    }
}

impl Sdp {
    /// Parses SDP text in the supported subset.
    ///
    /// # Errors
    /// Returns the first size, syntax, ordering, security, or subset
    /// violation encountered.
    pub fn parse(text: &str) -> Result<Self, SdpError> {
        Parser::default().parse(text)
    }

    /// Checks cross-section invariants (mids, BUNDLE, shared transport).
    ///
    /// # Errors
    /// Returns the first violated invariant.
    pub fn validate(&self) -> Result<(), SdpError> {
        if self.media.is_empty() {
            return Err(SdpError::NoMediaSections);
        }
        if self.media.len() > MAX_MEDIA_SECTIONS {
            return Err(SdpError::TooManyMediaSections {
                maximum: MAX_MEDIA_SECTIONS,
            });
        }
        for (index, section) in self.media.iter().enumerate() {
            if !is_mid(&section.mid) {
                return Err(SdpError::InvalidMid);
            }
            if self.media[..index]
                .iter()
                .any(|other| other.mid == section.mid)
            {
                return Err(SdpError::DuplicateMid(section.mid.clone()));
            }
            if section.port == 0 {
                return Err(SdpError::RejectedMediaSection {
                    mid: Some(section.mid.clone()),
                });
            }
            if section.codecs.is_empty() {
                return Err(SdpError::NoSupportedCodec {
                    mid: section.mid.clone(),
                });
            }
            if section.candidates.len() > MAX_CANDIDATES_PER_SECTION {
                return Err(SdpError::TooManyCandidates {
                    maximum: MAX_CANDIDATES_PER_SECTION,
                });
            }
        }
        for (index, mid) in self.bundle.iter().enumerate() {
            if self.bundle[..index].contains(mid) {
                return Err(SdpError::DuplicateMid(mid.clone()));
            }
            if !self.media.iter().any(|section| &section.mid == mid) {
                return Err(SdpError::UnknownBundleMid(mid.clone()));
            }
        }
        if (self.media.len() > 1 || !self.bundle.is_empty())
            && let Some(section) = self
                .media
                .iter()
                .find(|section| !self.bundle.contains(&section.mid))
        {
            return Err(SdpError::UnbundledMedia {
                mid: section.mid.clone(),
            });
        }
        let first = &self.media[0];
        if self.media.iter().any(|section| {
            section.ice != first.ice
                || section.fingerprint != first.fingerprint
                || section.setup != first.setup
        }) {
            return Err(SdpError::InconsistentBundleTransport);
        }
        Ok(())
    }

    /// The shared (bundled) ICE credentials.
    #[must_use]
    pub fn ice(&self) -> &IceCredentials {
        &self.media[0].ice
    }

    /// The shared (bundled) DTLS fingerprint.
    #[must_use]
    pub fn fingerprint(&self) -> Fingerprint {
        self.media[0].fingerprint
    }

    #[must_use]
    pub fn section(&self, mid: &str) -> Option<&MediaSection> {
        self.media.iter().find(|section| section.mid == mid)
    }

    /// Renders canonical CRLF-terminated SDP.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        push_line(&mut out, format_args!("v=0"));
        push_line(
            &mut out,
            format_args!(
                "o={} {} {} IN {} {}",
                self.origin.username,
                self.origin.session_id,
                self.origin.session_version,
                address_family(self.origin.address),
                self.origin.address
            ),
        );
        push_line(&mut out, format_args!("s=-"));
        push_line(&mut out, format_args!("t=0 0"));
        if !self.bundle.is_empty() {
            push_line(
                &mut out,
                format_args!("a=group:BUNDLE {}", self.bundle.join(" ")),
            );
        }
        push_line(&mut out, format_args!("a=ice-options:trickle"));
        for section in &self.media {
            render_section(&mut out, section);
        }
        out
    }

    /// Builds a deterministic BUNDLE offer: audio as mid `0`, video as mid
    /// `1` (sections without preferences are omitted), `actpass` setup, and
    /// dynamic payload types assigned from 96 in preference order.
    ///
    /// # Errors
    /// Returns a [`LocalEndpoint::validate`] error.
    pub fn offer(
        local: &LocalEndpoint,
        session_id: u64,
        session_version: u64,
    ) -> Result<Self, SdpError> {
        local.validate()?;
        let mut next_payload_type = FIRST_DYNAMIC_PAYLOAD_TYPE;
        let mut media = Vec::new();
        for (mid, kind) in [("0", SdpMediaKind::Audio), ("1", SdpMediaKind::Video)] {
            let (preferences, direction) = local.preferences(kind);
            if preferences.is_empty() {
                continue;
            }
            let mut codecs = Vec::with_capacity(preferences.len());
            for preference in preferences {
                if next_payload_type > LAST_PAYLOAD_TYPE {
                    return Err(SdpError::PayloadTypesExhausted);
                }
                codecs.push(SdpCodec {
                    payload_type: next_payload_type,
                    name: preference.name,
                    fmtp: preference.fmtp.clone(),
                    feedback: preference.feedback.clone(),
                });
                next_payload_type += 1;
            }
            media.push(local_section(
                local,
                kind,
                mid.to_owned(),
                direction,
                SetupRole::ActPass,
                codecs,
            ));
        }
        let sdp = Self {
            origin: local_origin(session_id, session_version),
            bundle: media.iter().map(|section| section.mid.clone()).collect(),
            media,
        };
        sdp.validate()?;
        Ok(sdp)
    }

    /// Builds the answer to `offer`.
    ///
    /// For each offered section, every local preference (in preference order)
    /// selects the first compatible offered codec (in offer order) and keeps
    /// the offerer's payload type; RTCP feedback is the ordered intersection.
    /// H.264 compatibility requires equal `profile_idc` and packetization mode.
    ///
    /// # Errors
    /// Returns a [`LocalEndpoint::validate`] error or
    /// [`SdpError::NoCommonCodec`] when a section cannot be satisfied.
    pub fn answer(
        offer: &Self,
        local: &LocalEndpoint,
        session_id: u64,
        session_version: u64,
    ) -> Result<Self, SdpError> {
        local.validate()?;
        offer.validate()?;
        let mut media = Vec::with_capacity(offer.media.len());
        for offered in &offer.media {
            let (preferences, local_direction) = local.preferences(offered.kind);
            let mut codecs: Vec<SdpCodec> = Vec::new();
            for preference in preferences {
                let Some(matched) = offered.codecs.iter().find(|codec| {
                    preference.accepts(codec)
                        && !codecs
                            .iter()
                            .any(|chosen| chosen.payload_type == codec.payload_type)
                }) else {
                    continue;
                };
                codecs.push(SdpCodec {
                    payload_type: matched.payload_type,
                    name: preference.name,
                    fmtp: preference.fmtp.clone(),
                    feedback: preference
                        .feedback
                        .iter()
                        .copied()
                        .filter(|feedback| matched.feedback.contains(feedback))
                        .collect(),
                });
            }
            if codecs.is_empty() {
                return Err(SdpError::NoCommonCodec {
                    mid: offered.mid.clone(),
                });
            }
            let setup = match offered.setup {
                SetupRole::ActPass => local.setup_when_actpass,
                SetupRole::Active => SetupRole::Passive,
                SetupRole::Passive => SetupRole::Active,
            };
            media.push(local_section(
                local,
                offered.kind,
                offered.mid.clone(),
                offered.direction.reversed().intersect(local_direction),
                setup,
                codecs,
            ));
        }
        let answer = Self {
            origin: local_origin(session_id, session_version),
            bundle: offer.bundle.clone(),
            media,
        };
        answer.validate()?;
        answer.validate_answer_for(offer)?;
        Ok(answer)
    }

    /// Checks that `self`, received as an answer, is consistent with `offer`.
    ///
    /// # Errors
    /// Returns [`SdpError::AnswerMismatch`] or [`SdpError::InvalidAnswerSetup`].
    pub fn validate_answer_for(&self, offer: &Self) -> Result<(), SdpError> {
        if self.media.len() != offer.media.len() {
            return Err(SdpError::AnswerMismatch("media section count differs"));
        }
        if self.bundle.iter().any(|mid| !offer.bundle.contains(mid)) {
            return Err(SdpError::AnswerMismatch("BUNDLE names a mid not offered"));
        }
        for (answered, offered) in self.media.iter().zip(&offer.media) {
            if answered.kind != offered.kind || answered.mid != offered.mid {
                return Err(SdpError::AnswerMismatch("media sections are reordered"));
            }
            let setup_ok = match (offered.setup, answered.setup) {
                (_, SetupRole::ActPass) => false,
                (SetupRole::ActPass, _) => true,
                (offered_role, answered_role) => offered_role != answered_role,
            };
            if !setup_ok {
                return Err(SdpError::InvalidAnswerSetup);
            }
            if offered.direction.reversed().intersect(answered.direction) != answered.direction {
                return Err(SdpError::AnswerMismatch("direction exceeds the offer"));
            }
            for codec in &answered.codecs {
                if !offered.codecs.iter().any(|candidate| {
                    candidate.payload_type == codec.payload_type && candidate.name == codec.name
                }) {
                    return Err(SdpError::AnswerMismatch("codec was not offered"));
                }
            }
        }
        Ok(())
    }
}

fn local_origin(session_id: u64, session_version: u64) -> Origin {
    Origin {
        username: "-".to_owned(),
        session_id,
        session_version,
        address: IpAddr::from([127, 0, 0, 1]),
    }
}

fn local_section(
    local: &LocalEndpoint,
    kind: SdpMediaKind,
    mid: String,
    direction: SdpDirection,
    setup: SetupRole,
    codecs: Vec<SdpCodec>,
) -> MediaSection {
    MediaSection {
        kind,
        port: 9,
        mid,
        direction,
        ice: local.ice.clone(),
        fingerprint: local.fingerprint,
        setup,
        codecs,
        max_bitrate_bps: (kind == SdpMediaKind::Video).then_some(local.max_video_bitrate_bps),
        candidates: Vec::new(),
        end_of_candidates: false,
    }
}

fn push_line(out: &mut String, args: fmt::Arguments<'_>) {
    let _ = out.write_fmt(args);
    out.push_str("\r\n");
}

fn render_section(out: &mut String, section: &MediaSection) {
    let payload_types: Vec<String> = section
        .codecs
        .iter()
        .map(|codec| codec.payload_type.to_string())
        .collect();
    push_line(
        out,
        format_args!(
            "m={} {} UDP/TLS/RTP/SAVPF {}",
            section.kind.as_str(),
            section.port,
            payload_types.join(" ")
        ),
    );
    push_line(out, format_args!("c=IN IP4 0.0.0.0"));
    if let Some(bitrate) = section.max_bitrate_bps {
        push_line(out, format_args!("b=TIAS:{bitrate}"));
    }
    push_line(out, format_args!("a=mid:{}", section.mid));
    push_line(out, format_args!("a=ice-ufrag:{}", section.ice.ufrag));
    push_line(out, format_args!("a=ice-pwd:{}", section.ice.pwd));
    push_line(out, format_args!("a=fingerprint:{}", section.fingerprint));
    push_line(out, format_args!("a=setup:{}", section.setup.as_str()));
    push_line(out, format_args!("a={}", section.direction.as_str()));
    push_line(out, format_args!("a=rtcp-mux"));
    for codec in &section.codecs {
        let pt = codec.payload_type;
        push_line(out, format_args!("a=rtpmap:{pt} {}", codec.name.rtpmap()));
        for feedback in &codec.feedback {
            push_line(out, format_args!("a=rtcp-fb:{pt} {}", feedback.as_str()));
        }
        if !codec.fmtp.is_empty() {
            let params: Vec<String> = codec
                .fmtp
                .iter()
                .map(|param| format!("{}={}", param.key, param.value))
                .collect();
            push_line(out, format_args!("a=fmtp:{pt} {}", params.join(";")));
        }
    }
    for candidate in &section.candidates {
        push_line(out, format_args!("a={candidate}"));
    }
    if section.end_of_candidates {
        push_line(out, format_args!("a=end-of-candidates"));
    }
}

// ---------------------------------------------------------------------------
// Parsing

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    Version,
    Origin,
    SessionName,
    SessionConnection,
    Timing,
    SessionAttributes,
    MediaConnection,
    MediaBandwidth,
    MediaAttributes,
}

#[derive(Default)]
struct SessionDraft {
    origin: Option<Origin>,
    bundle: Option<Vec<String>>,
    ufrag: Option<String>,
    pwd: Option<String>,
    fingerprint: Option<Fingerprint>,
    setup: Option<SetupRole>,
}

struct MediaDraft {
    kind: SdpMediaKind,
    port: u16,
    payload_types: Vec<u8>,
    mid: Option<String>,
    direction: Option<SdpDirection>,
    ufrag: Option<String>,
    pwd: Option<String>,
    fingerprint: Option<Fingerprint>,
    setup: Option<SetupRole>,
    rtcp_mux: bool,
    rtpmaps: Vec<(u8, Option<CodecName>)>,
    fmtps: Vec<(u8, String)>,
    feedback: Vec<(u8, String)>,
    max_bitrate_bps: Option<u64>,
    candidates: Vec<SdpCandidate>,
    end_of_candidates: bool,
}

#[derive(Default)]
struct Parser {
    session: SessionDraft,
    media: Vec<MediaDraft>,
}

/// Browser attributes that are recognized but do not affect the subset.
const IGNORED_ATTRIBUTES: &[&str] = &[
    "extmap",
    "extmap-allow-mixed",
    "msid",
    "msid-semantic",
    "ssrc",
    "rtcp",
    "rtcp-rsize",
    "rtcp-mux-only",
    "ice-options",
    "ptime",
    "maxptime",
];

impl Parser {
    fn parse(mut self, text: &str) -> Result<Sdp, SdpError> {
        if text.len() > MAX_SDP_BYTES {
            return Err(SdpError::TooLarge {
                actual: text.len(),
                maximum: MAX_SDP_BYTES,
            });
        }
        let body = text
            .strip_suffix("\r\n")
            .ok_or(SdpError::MissingFinalCrlf)?;
        let mut stage = Stage::Version;
        for (index, line) in body.split("\r\n").enumerate() {
            let number = index + 1;
            if number > MAX_SDP_LINES {
                return Err(SdpError::TooManyLines {
                    maximum: MAX_SDP_LINES,
                });
            }
            if line.len() > MAX_LINE_BYTES {
                return Err(SdpError::LineTooLong {
                    line: number,
                    maximum: MAX_LINE_BYTES,
                });
            }
            if line.contains(['\r', '\n']) {
                return Err(SdpError::BareLineEnding { line: number });
            }
            let bytes = line.as_bytes();
            if bytes.len() < 2 || bytes[1] != b'=' || !bytes[0].is_ascii_lowercase() {
                return Err(SdpError::MalformedLine { line: number });
            }
            if !line.is_ascii() || line.bytes().any(|byte| byte.is_ascii_control()) {
                return Err(SdpError::MalformedLine { line: number });
            }
            let kind = char::from(bytes[0]);
            let value = &line[2..];
            stage = self.line(stage, number, kind, value)?;
        }
        if !matches!(
            stage,
            Stage::MediaConnection | Stage::MediaBandwidth | Stage::MediaAttributes
        ) {
            return Err(SdpError::NoMediaSections);
        }
        self.finish()
    }

    fn line(
        &mut self,
        stage: Stage,
        number: usize,
        kind: char,
        value: &str,
    ) -> Result<Stage, SdpError> {
        let unexpected = SdpError::UnexpectedLine { line: number, kind };
        match (stage, kind) {
            (Stage::Version, 'v') => {
                if value != "0" {
                    return Err(SdpError::UnsupportedVersion);
                }
                Ok(Stage::Origin)
            }
            (Stage::Origin, 'o') => {
                self.session.origin = Some(Origin::parse(value)?);
                Ok(Stage::SessionName)
            }
            (Stage::SessionName, 's') => {
                if value.is_empty() {
                    return Err(SdpError::InvalidSessionName);
                }
                Ok(Stage::SessionConnection)
            }
            (Stage::SessionConnection, 'c') => {
                parse_connection(value)?;
                Ok(Stage::Timing)
            }
            (Stage::SessionConnection | Stage::Timing, 't') => {
                if value != "0 0" {
                    return Err(SdpError::InvalidTiming);
                }
                Ok(Stage::SessionAttributes)
            }
            (Stage::SessionAttributes, 'a') => {
                self.session_attribute(value)?;
                Ok(Stage::SessionAttributes)
            }
            (
                Stage::SessionAttributes
                | Stage::MediaConnection
                | Stage::MediaBandwidth
                | Stage::MediaAttributes,
                'm',
            ) => {
                if self.media.len() == MAX_MEDIA_SECTIONS {
                    return Err(SdpError::TooManyMediaSections {
                        maximum: MAX_MEDIA_SECTIONS,
                    });
                }
                self.media.push(parse_media_line(value)?);
                Ok(Stage::MediaConnection)
            }
            (Stage::MediaConnection, 'c') => {
                parse_connection(value)?;
                Ok(Stage::MediaBandwidth)
            }
            (Stage::MediaConnection | Stage::MediaBandwidth, 'b') => {
                let bitrate = parse_bandwidth(value)?;
                let section = self.current_media()?;
                if section.max_bitrate_bps.is_some() {
                    return Err(SdpError::DuplicateAttribute("b"));
                }
                section.max_bitrate_bps = Some(bitrate);
                Ok(Stage::MediaBandwidth)
            }
            (Stage::MediaConnection | Stage::MediaBandwidth | Stage::MediaAttributes, 'a') => {
                self.media_attribute(value)?;
                Ok(Stage::MediaAttributes)
            }
            (_, 'v' | 'o' | 's' | 'c' | 't' | 'm' | 'a' | 'b') => Err(unexpected),
            _ => Err(SdpError::UnsupportedLine { line: number, kind }),
        }
    }

    fn current_media(&mut self) -> Result<&mut MediaDraft, SdpError> {
        self.media.last_mut().ok_or(SdpError::NoMediaSections)
    }

    fn session_attribute(&mut self, attribute: &str) -> Result<(), SdpError> {
        let (name, value) = split_attribute(attribute);
        match (name, value) {
            ("group", Some(value)) => {
                let mut tokens = value.split(' ');
                let semantics = tokens.next().unwrap_or_default();
                if semantics != "BUNDLE" {
                    return Err(SdpError::UnsupportedGroup(semantics.to_owned()));
                }
                if self.session.bundle.is_some() {
                    return Err(SdpError::DuplicateAttribute("group"));
                }
                let mids: Vec<String> = tokens.map(str::to_owned).collect();
                if mids.is_empty()
                    || mids.len() > MAX_MEDIA_SECTIONS
                    || !mids.iter().all(|mid| is_mid(mid))
                {
                    return Err(SdpError::InvalidMid);
                }
                self.session.bundle = Some(mids);
            }
            ("ice-ufrag", Some(value)) => {
                set_once(&mut self.session.ufrag, value.to_owned(), "ice-ufrag")?;
            }
            ("ice-pwd", Some(value)) => {
                set_once(&mut self.session.pwd, value.to_owned(), "ice-pwd")?;
            }
            ("fingerprint", Some(value)) => {
                set_once(
                    &mut self.session.fingerprint,
                    Fingerprint::parse(value)?,
                    "fingerprint",
                )?;
            }
            ("setup", Some(value)) => {
                set_once(&mut self.session.setup, SetupRole::parse(value)?, "setup")?;
            }
            ("simulcast" | "rid", _) => return Err(SdpError::SimulcastUnsupported),
            (name, _) if IGNORED_ATTRIBUTES.contains(&name) => {}
            (name, _) => return Err(SdpError::UnsupportedAttribute(name.to_owned())),
        }
        Ok(())
    }

    fn media_attribute(&mut self, attribute: &str) -> Result<(), SdpError> {
        let (name, value) = split_attribute(attribute);
        let section = self.current_media()?;
        if let Some(direction) = SdpDirection::parse(name) {
            if value.is_some() {
                return Err(SdpError::UnsupportedAttribute(name.to_owned()));
            }
            return set_once(&mut section.direction, direction, "direction");
        }
        match (name, value) {
            ("mid", Some(value)) => {
                if !is_mid(value) {
                    return Err(SdpError::InvalidMid);
                }
                set_once(&mut section.mid, value.to_owned(), "mid")?;
            }
            ("ice-ufrag", Some(value)) => {
                set_once(&mut section.ufrag, value.to_owned(), "ice-ufrag")?;
            }
            ("ice-pwd", Some(value)) => set_once(&mut section.pwd, value.to_owned(), "ice-pwd")?,
            ("fingerprint", Some(value)) => {
                set_once(
                    &mut section.fingerprint,
                    Fingerprint::parse(value)?,
                    "fingerprint",
                )?;
            }
            ("setup", Some(value)) => {
                set_once(&mut section.setup, SetupRole::parse(value)?, "setup")?;
            }
            ("rtcp-mux", None) => {
                if section.rtcp_mux {
                    return Err(SdpError::DuplicateAttribute("rtcp-mux"));
                }
                section.rtcp_mux = true;
            }
            ("rtpmap", Some(value)) => {
                let (pt, encoding) = split_payload(value).ok_or(SdpError::InvalidRtpmap)?;
                section.require_listed(pt)?;
                if section.rtpmaps.iter().any(|(existing, _)| *existing == pt) {
                    return Err(SdpError::DuplicateAttribute("rtpmap"));
                }
                let codec = CodecName::parse_rtpmap(encoding)?;
                if codec.is_some_and(|codec| codec.kind() != section.kind) {
                    return Err(SdpError::CodecKindMismatch { payload_type: pt });
                }
                section.rtpmaps.push((pt, codec));
            }
            ("fmtp", Some(value)) => {
                let (pt, params) = split_payload(value).ok_or(SdpError::InvalidFmtp)?;
                section.require_listed(pt)?;
                if section.fmtps.iter().any(|(existing, _)| *existing == pt) {
                    return Err(SdpError::DuplicateAttribute("fmtp"));
                }
                section.fmtps.push((pt, params.to_owned()));
            }
            ("rtcp-fb", Some(value)) => {
                let (pt, feedback) = split_payload(value).ok_or(SdpError::InvalidRtcpFeedback)?;
                section.require_listed(pt)?;
                if section.feedback.len() >= MAX_PAYLOAD_TYPES * MAX_FEEDBACK_PER_CODEC {
                    return Err(SdpError::TooManyFeedback {
                        maximum: MAX_FEEDBACK_PER_CODEC,
                    });
                }
                section.feedback.push((pt, feedback.to_owned()));
            }
            ("candidate", Some(value)) => {
                if section.end_of_candidates {
                    return Err(SdpError::InvalidCandidate);
                }
                if section.candidates.len() == MAX_CANDIDATES_PER_SECTION {
                    return Err(SdpError::TooManyCandidates {
                        maximum: MAX_CANDIDATES_PER_SECTION,
                    });
                }
                section.candidates.push(SdpCandidate::parse_value(value)?);
            }
            ("end-of-candidates", None) => {
                if section.end_of_candidates {
                    return Err(SdpError::DuplicateAttribute("end-of-candidates"));
                }
                section.end_of_candidates = true;
            }
            ("simulcast" | "rid", _) => return Err(SdpError::SimulcastUnsupported),
            ("ssrc-group", Some(value)) if value.starts_with("SIM ") => {
                return Err(SdpError::SimulcastUnsupported);
            }
            ("ssrc-group", _) => {}
            (name, _) if IGNORED_ATTRIBUTES.contains(&name) => {}
            (name, _) => return Err(SdpError::UnsupportedAttribute(name.to_owned())),
        }
        Ok(())
    }

    fn finish(self) -> Result<Sdp, SdpError> {
        let mut session = self.session;
        let origin = session.origin.take().ok_or(SdpError::InvalidOrigin)?;
        let mut media = Vec::with_capacity(self.media.len());
        for draft in self.media {
            media.push(draft.finish(&session)?);
        }
        let sdp = Sdp {
            origin,
            bundle: session.bundle.unwrap_or_default(),
            media,
        };
        sdp.validate()?;
        Ok(sdp)
    }
}

impl MediaDraft {
    fn require_listed(&self, payload_type: u8) -> Result<(), SdpError> {
        if self.payload_types.contains(&payload_type) {
            Ok(())
        } else {
            Err(SdpError::UnlistedPayloadType(payload_type))
        }
    }

    fn finish(self, session: &SessionDraft) -> Result<MediaSection, SdpError> {
        let mid = self.mid.ok_or(SdpError::MissingMid)?;
        if self.port == 0 {
            return Err(SdpError::RejectedMediaSection { mid: Some(mid) });
        }
        let ufrag = self.ufrag.or_else(|| session.ufrag.clone());
        let pwd = self.pwd.or_else(|| session.pwd.clone());
        let (Some(ufrag), Some(pwd)) = (ufrag, pwd) else {
            return Err(SdpError::MissingIceCredentials { mid });
        };
        let ice = IceCredentials::new(ufrag, pwd)?;
        let Some(fingerprint) = self.fingerprint.or(session.fingerprint) else {
            return Err(SdpError::MissingFingerprint { mid });
        };
        let Some(setup) = self.setup.or(session.setup) else {
            return Err(SdpError::MissingSetup { mid });
        };
        if !self.rtcp_mux {
            return Err(SdpError::RtcpMuxRequired { mid });
        }
        let mut codecs = Vec::new();
        for payload_type in &self.payload_types {
            let Some(name) = self
                .rtpmaps
                .iter()
                .find(|(pt, _)| pt == payload_type)
                .and_then(|(_, codec)| *codec)
            else {
                continue;
            };
            if codecs.len() == MAX_CODECS_PER_SECTION {
                return Err(SdpError::TooManyCodecs {
                    maximum: MAX_CODECS_PER_SECTION,
                });
            }
            let fmtp = match self.fmtps.iter().find(|(pt, _)| pt == payload_type) {
                Some((_, params)) => parse_fmtp(params)?,
                None => Vec::new(),
            };
            validate_fmtp(name, &fmtp)?;
            let mut feedback = Vec::new();
            for (_, value) in self.feedback.iter().filter(|(pt, _)| pt == payload_type) {
                let entry = RtcpFeedback::parse(value)?;
                if feedback.contains(&entry) {
                    return Err(SdpError::DuplicateAttribute("rtcp-fb"));
                }
                feedback.push(entry);
            }
            validate_feedback_list(&feedback)?;
            codecs.push(SdpCodec {
                payload_type: *payload_type,
                name,
                fmtp,
                feedback,
            });
        }
        if codecs.is_empty() {
            return Err(SdpError::NoSupportedCodec { mid });
        }
        Ok(MediaSection {
            kind: self.kind,
            port: self.port,
            mid,
            direction: self.direction.unwrap_or(SdpDirection::SendRecv),
            ice,
            fingerprint,
            setup,
            codecs,
            max_bitrate_bps: self.max_bitrate_bps,
            candidates: self.candidates,
            end_of_candidates: self.end_of_candidates,
        })
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T, name: &'static str) -> Result<(), SdpError> {
    if slot.is_some() {
        return Err(SdpError::DuplicateAttribute(name));
    }
    *slot = Some(value);
    Ok(())
}

fn split_attribute(attribute: &str) -> (&str, Option<&str>) {
    match attribute.split_once(':') {
        Some((name, value)) => (name, Some(value)),
        None => (attribute, None),
    }
}

fn split_payload(value: &str) -> Option<(u8, &str)> {
    let (pt, rest) = value.split_once(' ')?;
    let pt = parse_payload_type(pt)?;
    (!rest.is_empty()).then_some((pt, rest))
}

fn parse_payload_type(text: &str) -> Option<u8> {
    if text.is_empty() || text.len() > 3 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u8>()
        .ok()
        .filter(|pt| *pt <= LAST_PAYLOAD_TYPE)
}

fn parse_media_line(value: &str) -> Result<MediaDraft, SdpError> {
    let tokens: Vec<&str> = value.split(' ').collect();
    let [kind, port, profile, formats @ ..] = tokens.as_slice() else {
        return Err(SdpError::InvalidMediaLine);
    };
    let kind = match *kind {
        "audio" => SdpMediaKind::Audio,
        "video" => SdpMediaKind::Video,
        other => return Err(SdpError::UnsupportedMediaKind(other.to_owned())),
    };
    if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SdpError::InvalidMediaLine);
    }
    let port: u16 = port.parse().map_err(|_| SdpError::InvalidMediaLine)?;
    if *profile != "UDP/TLS/RTP/SAVPF" {
        return Err(SdpError::InsecureProfile((*profile).to_owned()));
    }
    if formats.is_empty() {
        return Err(SdpError::InvalidMediaLine);
    }
    if formats.len() > MAX_PAYLOAD_TYPES {
        return Err(SdpError::TooManyPayloadTypes {
            maximum: MAX_PAYLOAD_TYPES,
        });
    }
    let mut payload_types = Vec::with_capacity(formats.len());
    for format in formats {
        let pt = parse_payload_type(format).ok_or(SdpError::InvalidPayloadType)?;
        if payload_types.contains(&pt) {
            return Err(SdpError::DuplicatePayloadType(pt));
        }
        payload_types.push(pt);
    }
    Ok(MediaDraft {
        kind,
        port,
        payload_types,
        mid: None,
        direction: None,
        ufrag: None,
        pwd: None,
        fingerprint: None,
        setup: None,
        rtcp_mux: false,
        rtpmaps: Vec::new(),
        fmtps: Vec::new(),
        feedback: Vec::new(),
        max_bitrate_bps: None,
        candidates: Vec::new(),
        end_of_candidates: false,
    })
}

fn parse_connection(value: &str) -> Result<(), SdpError> {
    let tokens: Vec<&str> = value.split(' ').collect();
    let ["IN", family, address] = tokens.as_slice() else {
        return Err(SdpError::InvalidConnection);
    };
    let address: IpAddr = address.parse().map_err(|_| SdpError::InvalidConnection)?;
    if address_family(address) != *family {
        return Err(SdpError::InvalidConnection);
    }
    Ok(())
}

fn parse_bandwidth(value: &str) -> Result<u64, SdpError> {
    let (kind, amount) = value.split_once(':').ok_or(SdpError::InvalidBandwidth)?;
    if amount.is_empty() || amount.len() > 10 || !amount.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(SdpError::InvalidBandwidth);
    }
    let amount: u64 = amount.parse().map_err(|_| SdpError::InvalidBandwidth)?;
    let bps = match kind {
        "TIAS" => amount,
        "AS" => amount * 1000,
        _ => return Err(SdpError::InvalidBandwidth),
    };
    if bps == 0 {
        return Err(SdpError::InvalidBandwidth);
    }
    Ok(bps)
}

fn parse_fmtp(params: &str) -> Result<Vec<FmtpParam>, SdpError> {
    let mut parsed: Vec<FmtpParam> = Vec::new();
    for param in params.split(';') {
        let param = param.trim_start_matches(' ');
        let (key, value) = param.split_once('=').ok_or(SdpError::InvalidFmtp)?;
        let key = key.to_ascii_lowercase();
        if parsed.len() == MAX_FMTP_PARAMS {
            return Err(SdpError::TooManyFmtpParameters {
                maximum: MAX_FMTP_PARAMS,
            });
        }
        if parsed.iter().any(|existing| existing.key == key) {
            return Err(SdpError::DuplicateAttribute("fmtp parameter"));
        }
        parsed.push(FmtpParam::new(key, value));
    }
    Ok(parsed)
}

fn validate_fmtp(codec: CodecName, params: &[FmtpParam]) -> Result<(), SdpError> {
    if params.len() > MAX_FMTP_PARAMS {
        return Err(SdpError::TooManyFmtpParameters {
            maximum: MAX_FMTP_PARAMS,
        });
    }
    for (index, param) in params.iter().enumerate() {
        let key_ok = !param.key.is_empty()
            && param.key.len() <= 32
            && param
                .key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        if !key_ok || params[..index].iter().any(|other| other.key == param.key) {
            return Err(SdpError::InvalidFmtp);
        }
        let flag = || matches!(param.value.as_str(), "0" | "1");
        let ranged = |low: u32, high: u32| {
            param.value.len() <= 10
                && param.value.bytes().all(|byte| byte.is_ascii_digit())
                && param
                    .value
                    .parse::<u32>()
                    .is_ok_and(|value| (low..=high).contains(&value))
        };
        let valid = match (codec, param.key.as_str()) {
            (CodecName::Opus, "minptime" | "ptime" | "maxptime") => ranged(3, 120),
            (CodecName::Opus, "useinbandfec" | "stereo" | "sprop-stereo" | "usedtx" | "cbr") => {
                flag()
            }
            (CodecName::Opus, "maxplaybackrate" | "sprop-maxcapturerate") => ranged(8_000, 48_000),
            (CodecName::Opus, "maxaveragebitrate") => ranged(6_000, 510_000),
            (CodecName::H264, "profile-level-id") => {
                param.value.len() == 6 && param.value.bytes().all(|byte| byte.is_ascii_hexdigit())
            }
            (CodecName::H264, "packetization-mode" | "level-asymmetry-allowed") => flag(),
            (CodecName::H264, "max-mbps" | "max-fs" | "max-cpb" | "max-dpb" | "max-br")
            | (CodecName::Vp8, "max-fs" | "max-fr") => ranged(1, u32::MAX),
            (_, key) => return Err(SdpError::UnsupportedFmtpParameter(key.to_owned())),
        };
        if !valid {
            return Err(SdpError::InvalidFmtp);
        }
    }
    Ok(())
}

fn validate_feedback_list(feedback: &[RtcpFeedback]) -> Result<(), SdpError> {
    if feedback.len() > MAX_FEEDBACK_PER_CODEC {
        return Err(SdpError::TooManyFeedback {
            maximum: MAX_FEEDBACK_PER_CODEC,
        });
    }
    for (index, entry) in feedback.iter().enumerate() {
        if feedback[..index].contains(entry) {
            return Err(SdpError::DuplicateAttribute("rtcp-fb"));
        }
    }
    Ok(())
}

fn fmtp_value<'a>(params: &'a [FmtpParam], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|param| param.key == key)
        .map(|param| param.value.as_str())
}

fn h264_profile_idc(params: &[FmtpParam]) -> u8 {
    fmtp_value(params, "profile-level-id")
        .and_then(|value| value.get(..2))
        .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        .unwrap_or(0x42)
}

fn h264_packetization_mode(params: &[FmtpParam]) -> u8 {
    match fmtp_value(params, "packetization-mode") {
        Some("1") => 1,
        _ => 0,
    }
}

fn address_family(address: IpAddr) -> &'static str {
    match address {
        IpAddr::V4(_) => "IP4",
        IpAddr::V6(_) => "IP6",
    }
}

fn validate_address(address: &str) -> Result<(), SdpError> {
    if address.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    let mdns = address.len() <= MAX_HOSTNAME_BYTES
        && address.strip_suffix(".local").is_some_and(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
        });
    if mdns {
        Ok(())
    } else {
        Err(SdpError::InvalidCandidate)
    }
}

fn parse_nonzero_u32(text: &str) -> Option<u32> {
    if text.len() > 10 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u32>().ok().filter(|value| *value != 0)
}

fn parse_nonzero_u16(text: &str) -> Option<u16> {
    if text.len() > 5 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|value| *value != 0)
}

/// RFC 8445 `ice-char`: ALPHA / DIGIT / "+" / "/".
fn is_ice_chars(text: &str, minimum: usize, maximum: usize) -> bool {
    (minimum..=maximum).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/')
}

fn is_mid(text: &str) -> bool {
    (1..=MAX_MID_BYTES).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}
