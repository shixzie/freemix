use core::fmt::{self, Write as _};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use fm_frame::{NormalizedDuration, NormalizedTimestamp, VideoDimensions};

use crate::RenditionId;

/// Maximum byte length accepted by bounded M3U8 playlist parsers.
pub const MAX_M3U8_BYTES: usize = 64 * 1024;

/// Maximum number of media segments accepted in a single M3U8 media playlist.
pub const MAX_M3U8_SEGMENTS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HlsPlaylistType {
    Live,
    Event,
    VideoOnDemand,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsSegmentMetadata {
    rendition: RenditionId,
    media_sequence: u64,
    discontinuity_sequence: u64,
    start: NormalizedTimestamp,
    duration: NormalizedDuration,
    byte_length: u64,
    independent: bool,
    uri: String,
}

impl HlsSegmentMetadata {
    /// Creates metadata for one completed media segment.
    ///
    /// # Errors
    ///
    /// Rejects empty/control-containing URIs and zero byte lengths.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rendition: RenditionId,
        media_sequence: u64,
        discontinuity_sequence: u64,
        start: NormalizedTimestamp,
        duration: NormalizedDuration,
        byte_length: u64,
        independent: bool,
        uri: impl Into<String>,
    ) -> Result<Self, HlsError> {
        let uri = uri.into();
        if uri.is_empty() || uri.len() > 2_048 || uri.chars().any(char::is_control) {
            return Err(HlsError::InvalidUri);
        }
        if byte_length == 0 {
            return Err(HlsError::EmptySegment);
        }
        Ok(Self {
            rendition,
            media_sequence,
            discontinuity_sequence,
            start,
            duration,
            byte_length,
            independent,
            uri,
        })
    }

    #[must_use]
    pub const fn rendition(&self) -> RenditionId {
        self.rendition
    }

    #[must_use]
    pub const fn media_sequence(&self) -> u64 {
        self.media_sequence
    }

    #[must_use]
    pub const fn discontinuity_sequence(&self) -> u64 {
        self.discontinuity_sequence
    }

    #[must_use]
    pub const fn start(&self) -> NormalizedTimestamp {
        self.start
    }

    #[must_use]
    pub const fn duration(&self) -> NormalizedDuration {
        self.duration
    }

    #[must_use]
    pub const fn byte_length(&self) -> u64 {
        self.byte_length
    }

    #[must_use]
    pub const fn is_independent(&self) -> bool {
        self.independent
    }

    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsPlaylistMetadata {
    version: u8,
    rendition: RenditionId,
    target_duration: NormalizedDuration,
    media_sequence: u64,
    discontinuity_sequence: u64,
    playlist_type: HlsPlaylistType,
    end_list: bool,
    segments: Vec<HlsSegmentMetadata>,
}

impl HlsPlaylistMetadata {
    #[must_use]
    pub const fn version(&self) -> u8 {
        self.version
    }

    #[must_use]
    pub const fn rendition(&self) -> RenditionId {
        self.rendition
    }

    #[must_use]
    pub const fn target_duration(&self) -> NormalizedDuration {
        self.target_duration
    }

    #[must_use]
    pub const fn media_sequence(&self) -> u64 {
        self.media_sequence
    }

    #[must_use]
    pub const fn discontinuity_sequence(&self) -> u64 {
        self.discontinuity_sequence
    }

    #[must_use]
    pub const fn playlist_type(&self) -> HlsPlaylistType {
        self.playlist_type
    }

    #[must_use]
    pub const fn is_end_list(&self) -> bool {
        self.end_list
    }

    #[must_use]
    pub fn segments(&self) -> &[HlsSegmentMetadata] {
        &self.segments
    }

    /// Renders this media playlist metadata deterministically as M3U8 text.
    #[must_use]
    pub fn render_m3u8(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "#EXTM3U");
        let _ = writeln!(out, "#EXT-X-VERSION:{}", self.version);
        let target_seconds = self
            .target_duration
            .as_nanos()
            .div_ceil(1_000_000_000)
            .max(1);
        let _ = writeln!(out, "#EXT-X-TARGETDURATION:{target_seconds}");
        let _ = writeln!(out, "#EXT-X-MEDIA-SEQUENCE:{}", self.media_sequence);
        if self.discontinuity_sequence > 0 {
            let _ = writeln!(
                out,
                "#EXT-X-DISCONTINUITY-SEQUENCE:{}",
                self.discontinuity_sequence
            );
        }
        match self.playlist_type {
            HlsPlaylistType::Live => {}
            HlsPlaylistType::Event => {
                let _ = writeln!(out, "#EXT-X-PLAYLIST-TYPE:EVENT");
            }
            HlsPlaylistType::VideoOnDemand => {
                let _ = writeln!(out, "#EXT-X-PLAYLIST-TYPE:VOD");
            }
        }
        if self.segments.iter().all(HlsSegmentMetadata::is_independent) {
            let _ = writeln!(out, "#EXT-X-INDEPENDENT-SEGMENTS");
        }
        let mut current_discontinuity = self.discontinuity_sequence;
        for segment in &self.segments {
            if segment.discontinuity_sequence > current_discontinuity {
                let _ = writeln!(out, "#EXT-X-DISCONTINUITY");
                current_discontinuity = segment.discontinuity_sequence;
            }
            let total_micros = segment.duration.as_nanos().saturating_add(500) / 1_000;
            let whole_seconds = total_micros / 1_000_000;
            let frac_micros = total_micros % 1_000_000;
            let _ = writeln!(out, "#EXTINF:{whole_seconds}.{frac_micros:06},");
            let _ = writeln!(out, "{}", segment.uri);
        }
        if self.end_list {
            let _ = writeln!(out, "#EXT-X-ENDLIST");
        }
        out
    }

    /// Parses a bounded M3U8 media playlist and validates its sequence invariants.
    ///
    /// # Errors
    ///
    /// Rejects oversized inputs (`> MAX_M3U8_BYTES`), missing `#EXTM3U` or
    /// `#EXT-X-TARGETDURATION` headers, zero `default_byte_length`, malformed
    /// tags or durations, more than `MAX_M3U8_SEGMENTS` segments, or segment
    /// sequences that fail [`HlsPlaylist`] validation.
    pub fn parse_m3u8(
        text: &str,
        rendition: RenditionId,
        default_byte_length: u64,
    ) -> Result<Self, HlsError> {
        if text.len() > MAX_M3U8_BYTES {
            return Err(HlsError::MalformedPlaylist(
                "playlist exceeds maximum byte length",
            ));
        }
        if default_byte_length == 0 {
            return Err(HlsError::EmptySegment);
        }
        let mut lines = text.lines().map(str::trim);
        if lines.next() != Some("#EXTM3U") {
            return Err(HlsError::MalformedPlaylist("missing EXTM3U header"));
        }

        let mut parser = MediaM3u8Parser::new(rendition, default_byte_length);
        for line in lines {
            parser.push_line(line)?;
        }
        parser.finish()
    }
}

struct MediaM3u8Parser {
    rendition: RenditionId,
    default_byte_length: u64,
    version: u8,
    target_duration: Option<NormalizedDuration>,
    media_sequence: u64,
    initial_discontinuity_sequence: u64,
    current_discontinuity: u64,
    playlist_type: HlsPlaylistType,
    independent: bool,
    end_list: bool,
    pending_discontinuity: bool,
    pending_extinf: Option<NormalizedDuration>,
    cumulative_nanos: i64,
    segments: Vec<HlsSegmentMetadata>,
}

impl MediaM3u8Parser {
    const fn new(rendition: RenditionId, default_byte_length: u64) -> Self {
        Self {
            rendition,
            default_byte_length,
            version: 3,
            target_duration: None,
            media_sequence: 0,
            initial_discontinuity_sequence: 0,
            current_discontinuity: 0,
            playlist_type: HlsPlaylistType::Live,
            independent: false,
            end_list: false,
            pending_discontinuity: false,
            pending_extinf: None,
            cumulative_nanos: 0,
            segments: Vec::new(),
        }
    }

    fn push_line(&mut self, line: &str) -> Result<(), HlsError> {
        if line.is_empty() {
            return Ok(());
        }
        if self.end_list {
            return Err(HlsError::MalformedPlaylist("content after EXT-X-ENDLIST"));
        }
        if line.starts_with("#EXT") {
            self.push_tag(line)
        } else if line.starts_with('#') {
            Ok(())
        } else {
            self.push_uri(line)
        }
    }

    fn push_tag(&mut self, line: &str) -> Result<(), HlsError> {
        if let Some(rest) = line.strip_prefix("#EXT-X-VERSION:") {
            let parsed: u8 = rest
                .parse()
                .map_err(|_| HlsError::MalformedPlaylist("invalid EXT-X-VERSION"))?;
            if parsed == 0 {
                return Err(HlsError::InvalidVersion);
            }
            self.version = parsed;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            let seconds: u64 = rest
                .parse()
                .map_err(|_| HlsError::MalformedPlaylist("invalid EXT-X-TARGETDURATION"))?;
            if seconds == 0 {
                return Err(HlsError::MalformedPlaylist(
                    "EXT-X-TARGETDURATION must be positive",
                ));
            }
            let nanos = seconds
                .checked_mul(1_000_000_000)
                .ok_or(HlsError::MalformedPlaylist("EXT-X-TARGETDURATION overflow"))?;
            self.target_duration = Some(
                NormalizedDuration::from_nanos(nanos)
                    .map_err(|_| HlsError::MalformedPlaylist("invalid EXT-X-TARGETDURATION"))?,
            );
        } else if let Some(rest) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            if !self.segments.is_empty() || self.pending_extinf.is_some() {
                return Err(HlsError::MalformedPlaylist(
                    "EXT-X-MEDIA-SEQUENCE after segments",
                ));
            }
            self.media_sequence = rest
                .parse()
                .map_err(|_| HlsError::MalformedPlaylist("invalid EXT-X-MEDIA-SEQUENCE"))?;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-DISCONTINUITY-SEQUENCE:") {
            if !self.segments.is_empty()
                || self.pending_extinf.is_some()
                || self.pending_discontinuity
            {
                return Err(HlsError::MalformedPlaylist(
                    "EXT-X-DISCONTINUITY-SEQUENCE after segments",
                ));
            }
            self.initial_discontinuity_sequence = rest
                .parse()
                .map_err(|_| HlsError::MalformedPlaylist("invalid EXT-X-DISCONTINUITY-SEQUENCE"))?;
            self.current_discontinuity = self.initial_discontinuity_sequence;
        } else if let Some(rest) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            self.playlist_type = match rest {
                "EVENT" => HlsPlaylistType::Event,
                "VOD" => HlsPlaylistType::VideoOnDemand,
                "LIVE" => HlsPlaylistType::Live,
                _ => return Err(HlsError::MalformedPlaylist("invalid EXT-X-PLAYLIST-TYPE")),
            };
        } else if line == "#EXT-X-INDEPENDENT-SEGMENTS" {
            self.independent = true;
        } else if line == "#EXT-X-DISCONTINUITY" {
            if self.pending_discontinuity || self.pending_extinf.is_some() {
                return Err(HlsError::DiscontinuitySequence);
            }
            self.current_discontinuity = self
                .current_discontinuity
                .checked_add(1)
                .ok_or(HlsError::DiscontinuitySequence)?;
            self.pending_discontinuity = true;
        } else if let Some(rest) = line.strip_prefix("#EXTINF:") {
            if self.pending_extinf.is_some() {
                return Err(HlsError::MalformedPlaylist(
                    "consecutive EXTINF without segment URI",
                ));
            }
            self.pending_extinf = Some(parse_extinf_duration(rest)?);
        } else if line == "#EXT-X-ENDLIST" {
            if self.pending_extinf.is_some() || self.pending_discontinuity {
                return Err(HlsError::MalformedPlaylist(
                    "dangling segment tag before EXT-X-ENDLIST",
                ));
            }
            self.end_list = true;
        } else if line.starts_with("#EXT-X-ALLOW-CACHE:") {
            // Allowed legacy FFmpeg tag.
        } else {
            return Err(HlsError::MalformedPlaylist(
                "unsupported or malformed HLS tag",
            ));
        }
        Ok(())
    }

    fn push_uri(&mut self, line: &str) -> Result<(), HlsError> {
        let Some(duration) = self.pending_extinf.take() else {
            return Err(HlsError::MalformedPlaylist("segment URI without EXTINF"));
        };
        if self.segments.len() >= MAX_M3U8_SEGMENTS {
            return Err(HlsError::MalformedPlaylist(
                "playlist exceeds maximum segment count",
            ));
        }
        let seq_offset = u64::try_from(self.segments.len())
            .map_err(|_| HlsError::MalformedPlaylist("segment count overflow"))?;
        let segment_sequence = self
            .media_sequence
            .checked_add(seq_offset)
            .ok_or(HlsError::MediaSequence)?;
        let start = NormalizedTimestamp::from_nanos(self.cumulative_nanos);
        let duration_i64 = i64::try_from(duration.as_nanos())
            .map_err(|_| HlsError::MalformedPlaylist("segment duration overflow"))?;
        self.cumulative_nanos = self
            .cumulative_nanos
            .checked_add(duration_i64)
            .ok_or(HlsError::MalformedPlaylist("cumulative timestamp overflow"))?;
        let segment = HlsSegmentMetadata::new(
            self.rendition,
            segment_sequence,
            self.current_discontinuity,
            start,
            duration,
            self.default_byte_length,
            self.independent,
            line,
        )?;
        self.segments.push(segment);
        self.pending_discontinuity = false;
        Ok(())
    }

    fn finish(self) -> Result<HlsPlaylistMetadata, HlsError> {
        if self.pending_extinf.is_some() || self.pending_discontinuity {
            return Err(HlsError::MalformedPlaylist("dangling segment tag at EOF"));
        }
        let Some(target_duration) = self.target_duration else {
            return Err(HlsError::MalformedPlaylist("missing EXT-X-TARGETDURATION"));
        };
        let max_window = self.segments.len().max(1);
        let mut playlist = HlsPlaylist::new(
            self.version,
            self.rendition,
            target_duration,
            self.media_sequence,
            max_window,
            self.playlist_type,
        )?;
        for segment in self.segments {
            playlist.append(segment)?;
        }
        if self.end_list {
            playlist.finish();
        }
        let mut metadata = playlist.metadata();
        if metadata.segments.is_empty() {
            metadata.discontinuity_sequence = self.initial_discontinuity_sequence;
        }
        Ok(metadata)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsPlaylist {
    version: u8,
    rendition: RenditionId,
    target_duration: NormalizedDuration,
    initial_media_sequence: u64,
    maximum_segments: usize,
    playlist_type: HlsPlaylistType,
    end_list: bool,
    segments: VecDeque<HlsSegmentMetadata>,
}

impl HlsPlaylist {
    /// Creates a bounded media playlist metadata accumulator.
    ///
    /// # Errors
    ///
    /// Rejects version zero or a zero segment window.
    pub fn new(
        version: u8,
        rendition: RenditionId,
        target_duration: NormalizedDuration,
        initial_media_sequence: u64,
        maximum_segments: usize,
        playlist_type: HlsPlaylistType,
    ) -> Result<Self, HlsError> {
        if version == 0 {
            return Err(HlsError::InvalidVersion);
        }
        if maximum_segments == 0 {
            return Err(HlsError::InvalidWindow);
        }
        Ok(Self {
            version,
            rendition,
            target_duration,
            initial_media_sequence,
            maximum_segments,
            playlist_type,
            end_list: false,
            segments: VecDeque::with_capacity(maximum_segments),
        })
    }

    /// Appends one contiguous segment and rolls the bounded live window.
    ///
    /// # Errors
    ///
    /// Rejects wrong renditions, gaps/regressions, invalid discontinuities,
    /// overlapping timestamps, oversized durations, or writes after end-list.
    pub fn append(&mut self, segment: HlsSegmentMetadata) -> Result<(), HlsError> {
        if self.end_list {
            return Err(HlsError::PlaylistEnded);
        }
        if segment.rendition != self.rendition {
            return Err(HlsError::WrongRendition);
        }
        if segment.duration.as_nanos() > self.target_duration.as_nanos() {
            return Err(HlsError::DurationExceedsTarget);
        }
        let expected_sequence = self
            .segments
            .back()
            .map_or(self.initial_media_sequence, |last| {
                last.media_sequence.saturating_add(1)
            });
        if segment.media_sequence != expected_sequence {
            return Err(HlsError::MediaSequence);
        }
        if let Some(previous) = self.segments.back() {
            if segment.discontinuity_sequence < previous.discontinuity_sequence
                || segment.discontinuity_sequence
                    > previous.discontinuity_sequence.saturating_add(1)
            {
                return Err(HlsError::DiscontinuitySequence);
            }
            let previous_end = previous
                .start
                .as_nanos()
                .saturating_add(i64::try_from(previous.duration.as_nanos()).unwrap_or(i64::MAX));
            if segment.start.as_nanos() < previous_end {
                return Err(HlsError::TimestampOverlap);
            }
        }
        if self.segments.len() == self.maximum_segments {
            self.segments.pop_front();
        }
        self.segments.push_back(segment);
        Ok(())
    }

    pub const fn finish(&mut self) {
        self.end_list = true;
    }

    #[must_use]
    pub fn metadata(&self) -> HlsPlaylistMetadata {
        let media_sequence = self.segments.front().map_or(
            self.initial_media_sequence,
            HlsSegmentMetadata::media_sequence,
        );
        let discontinuity_sequence = self
            .segments
            .front()
            .map_or(0, HlsSegmentMetadata::discontinuity_sequence);
        HlsPlaylistMetadata {
            version: self.version,
            rendition: self.rendition,
            target_duration: self.target_duration,
            media_sequence,
            discontinuity_sequence,
            playlist_type: self.playlist_type,
            end_list: self.end_list,
            segments: self.segments.iter().cloned().collect(),
        }
    }

    fn segment(&self, sequence: u64) -> Option<&HlsSegmentMetadata> {
        self.segments
            .iter()
            .find(|segment| segment.media_sequence == sequence)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsVariantMetadata {
    rendition: RenditionId,
    name: String,
    bandwidth_bps: u64,
    dimensions: VideoDimensions,
    codecs: String,
    playlist_uri: String,
}

impl HlsVariantMetadata {
    /// Creates one master-playlist variant descriptor.
    ///
    /// # Errors
    ///
    /// Rejects zero bandwidth or empty/control-containing text fields.
    pub fn new(
        rendition: RenditionId,
        name: impl Into<String>,
        bandwidth_bps: u64,
        dimensions: VideoDimensions,
        codecs: impl Into<String>,
        playlist_uri: impl Into<String>,
    ) -> Result<Self, HlsError> {
        let name = name.into();
        let codecs = codecs.into();
        let playlist_uri = playlist_uri.into();
        if bandwidth_bps == 0 {
            return Err(HlsError::ZeroBandwidth);
        }
        if [name.as_str(), codecs.as_str(), playlist_uri.as_str()]
            .into_iter()
            .any(|value| {
                value.is_empty() || value.len() > 2_048 || value.chars().any(char::is_control)
            })
        {
            return Err(HlsError::InvalidUri);
        }
        Ok(Self {
            rendition,
            name,
            bandwidth_bps,
            dimensions,
            codecs,
            playlist_uri,
        })
    }

    #[must_use]
    pub const fn rendition(&self) -> RenditionId {
        self.rendition
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn bandwidth_bps(&self) -> u64 {
        self.bandwidth_bps
    }

    #[must_use]
    pub const fn dimensions(&self) -> VideoDimensions {
        self.dimensions
    }

    #[must_use]
    pub fn codecs(&self) -> &str {
        &self.codecs
    }

    #[must_use]
    pub fn playlist_uri(&self) -> &str {
        &self.playlist_uri
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsAbrCoordinator {
    variants: Vec<HlsVariantMetadata>,
    playlists: BTreeMap<RenditionId, HlsPlaylist>,
}

impl HlsAbrCoordinator {
    /// Creates aligned, bounded playlists for a validated set of ABR variants.
    ///
    /// # Errors
    ///
    /// Requires at least two variants with unique rendition IDs and names.
    pub fn new(
        variants: Vec<HlsVariantMetadata>,
        version: u8,
        target_duration: NormalizedDuration,
        initial_media_sequence: u64,
        maximum_segments: usize,
        playlist_type: HlsPlaylistType,
    ) -> Result<Self, HlsError> {
        if variants.len() < 2 {
            return Err(HlsError::InsufficientVariants);
        }
        let mut renditions = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut playlists = BTreeMap::new();
        for variant in &variants {
            if !renditions.insert(variant.rendition) || !names.insert(variant.name.clone()) {
                return Err(HlsError::DuplicateVariant);
            }
            playlists.insert(
                variant.rendition,
                HlsPlaylist::new(
                    version,
                    variant.rendition,
                    target_duration,
                    initial_media_sequence,
                    maximum_segments,
                    playlist_type,
                )?,
            );
        }
        Ok(Self {
            variants,
            playlists,
        })
    }

    /// Appends a segment after checking boundary alignment with every available
    /// segment of the same media sequence in the other variants.
    ///
    /// # Errors
    ///
    /// Returns a playlist sequencing error or [`HlsError::AbrSequenceMismatch`].
    pub fn append(&mut self, segment: HlsSegmentMetadata) -> Result<(), HlsError> {
        if !segment.independent {
            return Err(HlsError::AbrSegmentNotIndependent);
        }
        let rendition = segment.rendition;
        if !self.playlists.contains_key(&rendition) {
            return Err(HlsError::WrongRendition);
        }
        for (other_rendition, playlist) in &self.playlists {
            if *other_rendition == rendition {
                continue;
            }
            if let Some(other) = playlist.segment(segment.media_sequence)
                && (other.start != segment.start
                    || other.duration != segment.duration
                    || other.discontinuity_sequence != segment.discontinuity_sequence
                    || !other.independent)
            {
                return Err(HlsError::AbrSequenceMismatch);
            }
        }
        let Some(playlist) = self.playlists.get_mut(&rendition) else {
            return Err(HlsError::WrongRendition);
        };
        playlist.append(segment)
    }

    #[must_use]
    pub fn variants(&self) -> &[HlsVariantMetadata] {
        &self.variants
    }

    #[must_use]
    pub fn playlist(&self, rendition: RenditionId) -> Option<HlsPlaylistMetadata> {
        self.playlists.get(&rendition).map(HlsPlaylist::metadata)
    }

    pub fn finish(&mut self) {
        for playlist in self.playlists.values_mut() {
            playlist.finish();
        }
    }

    /// Renders a deterministic M3U8 master playlist for the configured ABR variants.
    #[must_use]
    pub fn render_master_m3u8(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "#EXTM3U");
        let _ = writeln!(out, "#EXT-X-VERSION:6");
        let _ = writeln!(out, "#EXT-X-INDEPENDENT-SEGMENTS");
        for variant in &self.variants {
            let bandwidth = variant.bandwidth_bps;
            let width = variant.dimensions.width();
            let height = variant.dimensions.height();
            let codecs = &variant.codecs;
            let _ = writeln!(
                out,
                "#EXT-X-STREAM-INF:BANDWIDTH={bandwidth},AVERAGE-BANDWIDTH={bandwidth},RESOLUTION={width}x{height},CODECS=\"{codecs}\""
            );
            let _ = writeln!(out, "{}", variant.playlist_uri);
        }
        out
    }
}

/// One parsed `#EXT-X-STREAM-INF` variant entry from an M3U8 master playlist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HlsMasterVariant {
    bandwidth_bps: u64,
    resolution: VideoDimensions,
    codecs: Option<String>,
    playlist_uri: String,
}

impl HlsMasterVariant {
    #[must_use]
    pub const fn bandwidth_bps(&self) -> u64 {
        self.bandwidth_bps
    }

    #[must_use]
    pub const fn resolution(&self) -> VideoDimensions {
        self.resolution
    }

    #[must_use]
    pub fn codecs(&self) -> Option<&str> {
        self.codecs.as_deref()
    }

    #[must_use]
    pub fn playlist_uri(&self) -> &str {
        &self.playlist_uri
    }
}

/// Parses a bounded M3U8 media playlist and validates its sequence invariants.
///
/// # Errors
///
/// Forwards [`HlsPlaylistMetadata::parse_m3u8`] validation errors.
pub fn parse_media_m3u8(
    text: &str,
    rendition: RenditionId,
    default_byte_length: u64,
) -> Result<HlsPlaylistMetadata, HlsError> {
    HlsPlaylistMetadata::parse_m3u8(text, rendition, default_byte_length)
}

/// Parses a bounded M3U8 master playlist into its variant stream entries.
///
/// # Errors
///
/// Rejects inputs exceeding [`MAX_M3U8_BYTES`], missing `#EXTM3U`, malformed
/// `#EXT-X-STREAM-INF:` attributes, missing or invalid variant URIs, or variant
/// counts outside `1..=8` ([`HlsError::InvalidVariantCount`]).
pub fn parse_master_m3u8(text: &str) -> Result<Vec<HlsMasterVariant>, HlsError> {
    if text.len() > MAX_M3U8_BYTES {
        return Err(HlsError::MalformedPlaylist(
            "playlist exceeds maximum byte length",
        ));
    }
    let mut lines = text.lines().map(str::trim);
    if lines.next() != Some("#EXTM3U") {
        return Err(HlsError::MalformedPlaylist("missing EXTM3U header"));
    }

    let mut variants: Vec<HlsMasterVariant> = Vec::new();
    let mut pending_stream_inf: Option<(u64, VideoDimensions, Option<String>)> = None;

    for line in lines {
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXT-X-VERSION:") {
            let version: u8 = rest
                .parse()
                .map_err(|_| HlsError::MalformedPlaylist("invalid EXT-X-VERSION"))?;
            if version == 0 {
                return Err(HlsError::InvalidVersion);
            }
        } else if line == "#EXT-X-INDEPENDENT-SEGMENTS" {
            // Valid master playlist header tag.
        } else if let Some(rest) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            if pending_stream_inf.is_some() {
                return Err(HlsError::MalformedPlaylist(
                    "missing variant URI after EXT-X-STREAM-INF",
                ));
            }
            pending_stream_inf = Some(parse_stream_inf_attributes(rest)?);
        } else if line.starts_with("#EXT") {
            return Err(HlsError::MalformedPlaylist(
                "unsupported or unexpected tag in master playlist",
            ));
        } else if line.starts_with('#') {
            // Comment line.
        } else {
            let Some((bandwidth_bps, resolution, codecs)) = pending_stream_inf.take() else {
                return Err(HlsError::MalformedPlaylist(
                    "unexpected URI line without EXT-X-STREAM-INF",
                ));
            };
            if line.is_empty() || line.len() > 2_048 || line.chars().any(char::is_control) {
                return Err(HlsError::InvalidUri);
            }
            if variants.len() >= 8 {
                return Err(HlsError::InvalidVariantCount);
            }
            variants.push(HlsMasterVariant {
                bandwidth_bps,
                resolution,
                codecs,
                playlist_uri: line.to_owned(),
            });
        }
    }

    if pending_stream_inf.is_some() {
        return Err(HlsError::MalformedPlaylist(
            "missing variant URI after EXT-X-STREAM-INF",
        ));
    }
    if variants.is_empty() {
        return Err(HlsError::InvalidVariantCount);
    }
    Ok(variants)
}

fn parse_extinf_duration(text: &str) -> Result<NormalizedDuration, HlsError> {
    let Some((duration_str, _title)) = text.split_once(',') else {
        return Err(HlsError::MalformedPlaylist("EXTINF missing comma"));
    };
    if duration_str.is_empty() {
        return Err(HlsError::MalformedPlaylist("empty EXTINF duration"));
    }
    let (whole_secs, frac_nanos) = if let Some((whole, frac)) = duration_str.split_once('.') {
        if whole.is_empty()
            || frac.is_empty()
            || frac.len() > 9
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || !frac.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(HlsError::MalformedPlaylist("invalid EXTINF duration"));
        }
        let whole_secs: u64 = whole
            .parse()
            .map_err(|_| HlsError::MalformedPlaylist("invalid EXTINF duration"))?;
        let mut frac_nanos: u64 = frac
            .parse()
            .map_err(|_| HlsError::MalformedPlaylist("invalid EXTINF duration"))?;
        for _ in frac.len()..9 {
            frac_nanos = frac_nanos.saturating_mul(10);
        }
        (whole_secs, frac_nanos)
    } else {
        if !duration_str.bytes().all(|b| b.is_ascii_digit()) {
            return Err(HlsError::MalformedPlaylist("invalid EXTINF duration"));
        }
        let whole_secs: u64 = duration_str
            .parse()
            .map_err(|_| HlsError::MalformedPlaylist("invalid EXTINF duration"))?;
        (whole_secs, 0)
    };
    let total_nanos = whole_secs
        .checked_mul(1_000_000_000)
        .and_then(|nanos| nanos.checked_add(frac_nanos))
        .ok_or(HlsError::MalformedPlaylist("EXTINF duration overflow"))?;
    NormalizedDuration::from_nanos(total_nanos)
        .map_err(|_| HlsError::MalformedPlaylist("EXTINF duration must be positive"))
}

fn parse_stream_inf_attributes(
    text: &str,
) -> Result<(u64, VideoDimensions, Option<String>), HlsError> {
    if text.is_empty() {
        return Err(HlsError::MalformedPlaylist(
            "empty EXT-X-STREAM-INF attributes",
        ));
    }
    let mut bandwidth_bps: Option<u64> = None;
    let mut resolution: Option<VideoDimensions> = None;
    let mut codecs: Option<String> = None;

    for attr in split_quoted_attributes(text)? {
        let Some((key, value)) = attr.split_once('=') else {
            return Err(HlsError::MalformedPlaylist(
                "invalid EXT-X-STREAM-INF attribute",
            ));
        };
        if key.is_empty() || value.is_empty() {
            return Err(HlsError::MalformedPlaylist(
                "empty EXT-X-STREAM-INF attribute key or value",
            ));
        }
        match key {
            "BANDWIDTH" => {
                if bandwidth_bps.is_some() {
                    return Err(HlsError::MalformedPlaylist("duplicate BANDWIDTH attribute"));
                }
                let parsed: u64 = value
                    .parse()
                    .map_err(|_| HlsError::MalformedPlaylist("invalid BANDWIDTH attribute"))?;
                if parsed == 0 {
                    return Err(HlsError::ZeroBandwidth);
                }
                bandwidth_bps = Some(parsed);
            }
            "RESOLUTION" => {
                if resolution.is_some() {
                    return Err(HlsError::MalformedPlaylist(
                        "duplicate RESOLUTION attribute",
                    ));
                }
                let Some((width_str, height_str)) =
                    value.split_once('x').or_else(|| value.split_once('X'))
                else {
                    return Err(HlsError::MalformedPlaylist("invalid RESOLUTION attribute"));
                };
                let width: u32 = width_str
                    .parse()
                    .map_err(|_| HlsError::MalformedPlaylist("invalid RESOLUTION width"))?;
                let height: u32 = height_str
                    .parse()
                    .map_err(|_| HlsError::MalformedPlaylist("invalid RESOLUTION height"))?;
                let dims = VideoDimensions::new(width, height)
                    .ok_or(HlsError::MalformedPlaylist("invalid RESOLUTION dimensions"))?;
                resolution = Some(dims);
            }
            "CODECS" => {
                if codecs.is_some() {
                    return Err(HlsError::MalformedPlaylist("duplicate CODECS attribute"));
                }
                let Some(unquoted) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
                else {
                    return Err(HlsError::MalformedPlaylist(
                        "CODECS attribute must be quoted",
                    ));
                };
                if unquoted.is_empty()
                    || unquoted.len() > 2_048
                    || unquoted.chars().any(char::is_control)
                {
                    return Err(HlsError::MalformedPlaylist("invalid CODECS value"));
                }
                codecs = Some(unquoted.to_owned());
            }
            _ => {}
        }
    }

    let Some(bandwidth_bps) = bandwidth_bps else {
        return Err(HlsError::MalformedPlaylist(
            "missing BANDWIDTH in EXT-X-STREAM-INF",
        ));
    };
    let Some(resolution) = resolution else {
        return Err(HlsError::MalformedPlaylist(
            "missing RESOLUTION in EXT-X-STREAM-INF",
        ));
    };
    Ok((bandwidth_bps, resolution, codecs))
}

fn split_quoted_attributes(text: &str) -> Result<Vec<&str>, HlsError> {
    let mut attributes = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (index, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                attributes.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    if in_quotes {
        return Err(HlsError::MalformedPlaylist(
            "unterminated quote in EXT-X-STREAM-INF",
        ));
    }
    attributes.push(&text[start..]);
    Ok(attributes)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HlsError {
    InvalidUri,
    EmptySegment,
    InvalidVersion,
    InvalidWindow,
    PlaylistEnded,
    WrongRendition,
    DurationExceedsTarget,
    MediaSequence,
    DiscontinuitySequence,
    TimestampOverlap,
    ZeroBandwidth,
    InsufficientVariants,
    InvalidVariantCount,
    DuplicateVariant,
    AbrSegmentNotIndependent,
    AbrSequenceMismatch,
    MalformedPlaylist(&'static str),
}

impl fmt::Display for HlsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUri => formatter.write_str("HLS URI or descriptor text is invalid"),
            Self::EmptySegment => formatter.write_str("HLS segment byte length must be nonzero"),
            Self::InvalidVersion => formatter.write_str("HLS playlist version must be nonzero"),
            Self::InvalidWindow => formatter.write_str("HLS playlist window must be nonzero"),
            Self::PlaylistEnded => formatter.write_str("HLS playlist is already ended"),
            Self::WrongRendition => formatter.write_str("HLS segment belongs to another rendition"),
            Self::DurationExceedsTarget => {
                formatter.write_str("HLS segment exceeds target duration")
            }
            Self::MediaSequence => formatter.write_str("HLS media sequence is not contiguous"),
            Self::DiscontinuitySequence => {
                formatter.write_str("HLS discontinuity sequence is invalid")
            }
            Self::TimestampOverlap => formatter.write_str("HLS segment timestamps overlap"),
            Self::ZeroBandwidth => formatter.write_str("HLS variant bandwidth must be nonzero"),
            Self::InsufficientVariants => {
                formatter.write_str("HLS ABR output requires at least two variants")
            }
            Self::InvalidVariantCount => {
                formatter.write_str("HLS master playlist must contain between 1 and 8 variants")
            }
            Self::DuplicateVariant => {
                formatter.write_str("HLS ABR variant identifiers and names must be unique")
            }
            Self::AbrSegmentNotIndependent => {
                formatter.write_str("HLS ABR segments must begin independently")
            }
            Self::AbrSequenceMismatch => {
                formatter.write_str("HLS ABR segment boundaries are not aligned")
            }
            Self::MalformedPlaylist(reason) => {
                write!(formatter, "malformed HLS playlist: {reason}")
            }
        }
    }
}

impl std::error::Error for HlsError {}
