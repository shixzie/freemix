//! Canonical text for a recorder track map (AU-010).
//!
//! The document is the offline, strictly decoded form of a
//! [`RecorderTrackMap`]: the bus inventory and every track's ordered channel
//! sources. It is canonical so a rewrite is byte-identical, and it carries no
//! daemon or mixer state beyond what the map itself validates.

use core::fmt;

use fm_types::audio_bus::AuxBusNumber;

use crate::track_map::{
    BusInventory, MAX_INVENTORY_BUSES, MAX_RECORDER_TRACKS, RecordBus, RecordTrack,
    RecorderTrackMap, TrackMapError, TrackMapLayout, TrackSource,
};

/// Why a track-map document could not be restored.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TrackMapDocumentError {
    /// The first line is not the current document header.
    Header,
    /// The layout line is malformed or appears after other lines.
    Layout,
    /// An inventory line is missing, malformed, duplicated, or out of order.
    Inventory,
    /// A track line is missing, malformed, or over the track limit.
    Track,
    /// Inventory lines do not ascend by bus.
    Order,
    /// The inventory or track set failed model validation.
    Map(TrackMapError),
}

impl fmt::Display for TrackMapDocumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Header => formatter.write_str("record track-map document header is unknown"),
            Self::Layout => formatter.write_str("record track-map layout line is malformed"),
            Self::Inventory => formatter.write_str("record track-map inventory line is malformed"),
            Self::Track => formatter.write_str("record track-map track line is malformed"),
            Self::Order => formatter.write_str("record track-map inventory is not in bus order"),
            Self::Map(error) => write!(formatter, "record track map is invalid: {error}"),
        }
    }
}

impl std::error::Error for TrackMapDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Map(error) => Some(error),
            _ => None,
        }
    }
}

impl RecorderTrackMap {
    /// Encodes the map as canonical text:
    ///
    /// ```text
    /// freemix-record-track-map 1
    /// inventory <bus> <channels>
    /// track <name hex> <bus> <channel> ...
    /// ```
    ///
    /// Inventory buses ascend, and tracks keep their map order.
    #[must_use]
    pub fn to_document(&self) -> String {
        use fmt::Write as _;
        let mut output = String::from("freemix-record-track-map 1\n");
        if self.layout() == TrackMapLayout::SplitPerTrack {
            output.push_str("layout split\n");
        }
        let mut buses = self.inventory().buses().to_vec();
        buses.sort_unstable();
        for (bus, channels) in buses {
            writeln!(output, "inventory {} {channels}", bus_document(bus))
                .expect("writing to a string cannot fail");
        }
        for track in self.tracks() {
            let mut parts = vec![hex_text(track.name())];
            for source in track.sources() {
                parts.push(bus_document(source.bus));
                parts.push(source.channel.to_string());
            }
            writeln!(output, "track {}", parts.join(" ")).expect("writing to a string cannot fail");
        }
        output
    }

    /// Restores a map from `document`.
    ///
    /// # Errors
    ///
    /// Returns [`TrackMapDocumentError`] for anything but the exact contract.
    pub fn from_document(document: &str) -> Result<Self, TrackMapDocumentError> {
        let mut lines = document.lines();
        if lines.next() != Some("freemix-record-track-map 1") {
            return Err(TrackMapDocumentError::Header);
        }
        let mut layout = TrackMapLayout::Interleaved;
        let mut inventory: Vec<(RecordBus, u16)> = Vec::new();
        let mut tracks: Vec<RecordTrack> = Vec::new();
        for line in lines {
            if let Some(rest) = line.strip_prefix("layout ") {
                if layout != TrackMapLayout::Interleaved
                    || !inventory.is_empty()
                    || !tracks.is_empty()
                {
                    return Err(TrackMapDocumentError::Layout);
                }
                layout = match rest {
                    "split" => TrackMapLayout::SplitPerTrack,
                    _ => return Err(TrackMapDocumentError::Layout),
                };
            } else if let Some(rest) = line.strip_prefix("inventory ") {
                if !tracks.is_empty() {
                    return Err(TrackMapDocumentError::Order);
                }
                let parts: Vec<&str> = rest.split(' ').collect();
                let [bus, channels] = parts.as_slice() else {
                    return Err(TrackMapDocumentError::Inventory);
                };
                let bus = bus_from_token(bus).ok_or(TrackMapDocumentError::Inventory)?;
                let channels = channels
                    .parse::<u16>()
                    .map_err(|_| TrackMapDocumentError::Inventory)?;
                if inventory.len() >= MAX_INVENTORY_BUSES {
                    return Err(TrackMapDocumentError::Inventory);
                }
                if let Some((previous, _)) = inventory.last() {
                    if bus == *previous {
                        return Err(TrackMapDocumentError::Inventory);
                    }
                    if bus < *previous {
                        return Err(TrackMapDocumentError::Order);
                    }
                }
                inventory.push((bus, channels));
            } else if let Some(rest) = line.strip_prefix("track ") {
                let parts: Vec<&str> = rest.split(' ').collect();
                let Some((name, sources)) = parts.split_first() else {
                    return Err(TrackMapDocumentError::Track);
                };
                if sources.is_empty() || sources.len() % 2 != 0 {
                    return Err(TrackMapDocumentError::Track);
                }
                let name = unhex_text(name).ok_or(TrackMapDocumentError::Track)?;
                let mut track_sources = Vec::with_capacity(sources.len() / 2);
                for pair in sources.chunks(2) {
                    let bus = bus_from_token(pair[0]).ok_or(TrackMapDocumentError::Track)?;
                    let channel = pair[1]
                        .parse::<u16>()
                        .map_err(|_| TrackMapDocumentError::Track)?;
                    track_sources.push(TrackSource { bus, channel });
                }
                if tracks.len() >= MAX_RECORDER_TRACKS {
                    return Err(TrackMapDocumentError::Track);
                }
                tracks.push(RecordTrack::new(name, track_sources));
            } else {
                return Err(if tracks.is_empty() {
                    TrackMapDocumentError::Inventory
                } else {
                    TrackMapDocumentError::Track
                });
            }
        }
        if inventory.is_empty() {
            return Err(TrackMapDocumentError::Inventory);
        }
        if tracks.is_empty() {
            return Err(TrackMapDocumentError::Track);
        }
        let inventory = BusInventory::new(inventory).map_err(TrackMapDocumentError::Map)?;
        Self::new(tracks, inventory)
            .map(|map| map.with_layout(layout))
            .map_err(TrackMapDocumentError::Map)
    }
}

fn bus_document(bus: RecordBus) -> String {
    match bus {
        RecordBus::Master => "master".to_owned(),
        RecordBus::Aux(bus) => format!("aux:{}", bus.get()),
    }
}

fn bus_from_token(token: &str) -> Option<RecordBus> {
    if token == "master" {
        return Some(RecordBus::Master);
    }
    let id = token.strip_prefix("aux:")?;
    let value = id.parse::<u8>().ok()?;
    Some(RecordBus::Aux(AuxBusNumber::new(value)?))
}

fn hex_text(text: &str) -> String {
    use fmt::Write as _;
    let mut output = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        write!(output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

fn unhex_text(text: &str) -> Option<String> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return None;
    }
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let bytes = text.as_bytes();
    let mut output = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks(2) {
        output.push((digit(pair[0])? << 4) | digit(pair[1])?);
    }
    String::from_utf8(output).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> RecorderTrackMap {
        let sub = AuxBusNumber::new(7).unwrap();
        let inventory =
            BusInventory::new(vec![(RecordBus::Master, 2), (RecordBus::Aux(sub), 4)]).unwrap();
        RecorderTrackMap::new(
            vec![
                RecordTrack::new(
                    "Master",
                    vec![
                        TrackSource {
                            bus: RecordBus::Master,
                            channel: 0,
                        },
                        TrackSource {
                            bus: RecordBus::Master,
                            channel: 1,
                        },
                    ],
                ),
                RecordTrack::new(
                    "Sub",
                    vec![
                        TrackSource {
                            bus: RecordBus::Aux(sub),
                            channel: 0,
                        },
                        TrackSource {
                            bus: RecordBus::Aux(sub),
                            channel: 1,
                        },
                    ],
                ),
            ],
            inventory,
        )
        .unwrap()
    }

    #[test]
    fn track_map_documents_round_trip() {
        let map = map();
        let document = map.to_document();
        assert_eq!(
            document,
            "freemix-record-track-map 1\n\
             inventory master 2\n\
             inventory aux:7 4\n\
             track 4d6173746572 master 0 master 1\n\
             track 537562 aux:7 0 aux:7 1\n"
        );
        let restored = RecorderTrackMap::from_document(&document).unwrap();
        assert_eq!(restored, map);
        assert_eq!(restored.to_document(), document);
    }

    #[test]
    fn split_track_map_documents_round_trip() {
        let map = map().with_layout(TrackMapLayout::SplitPerTrack);
        let document = map.to_document();
        assert!(document.starts_with("freemix-record-track-map 1\nlayout split\n"));
        let restored = RecorderTrackMap::from_document(&document).unwrap();
        assert_eq!(restored, map);
        assert_eq!(restored.to_document(), document);
    }

    #[test]
    fn track_map_documents_are_strict() {
        for document in [
            "",
            "freemix-record-track-map 2\ninventory master 2\ntrack 4d master 0\n",
            "freemix-record-track-map 1\n",
            "freemix-record-track-map 1\ninventory master 2\n",
            "freemix-record-track-map 1\ntrack 4d master 0\n",
            "freemix-record-track-map 1\nbogus master 2\ntrack 4d master 0\n",
            "freemix-record-track-map 1\ninventory master 0\ntrack 4d master 0\n",
            "freemix-record-track-map 1\ninventory master 2\ninventory master 2\ntrack 4d master 0\n",
            "freemix-record-track-map 1\ninventory aux:7 2\ninventory master 2\ntrack 4d master 0\n",
            "freemix-record-track-map 1\ninventory master 2\ntrack 4d master 2\n",
            "freemix-record-track-map 1\ninventory master 2\ntrack 4d master\n",
            "freemix-record-track-map 1\ninventory master 2\ntrack zz master 0\n",
            "freemix-record-track-map 1\ninventory master 2\ntrack 4d aux:62 0\n",
            "freemix-record-track-map 1\ninventory master 2\ntrack 4d aux:8 0\n",
            "freemix-record-track-map 1\ninventory master 2\ntrack 4d aux:0 0\n",
            "freemix-record-track-map 1\nlayout stereo\ninventory master 2\ntrack 4d master 0\n",
            "freemix-record-track-map 1\nlayout split\nlayout split\ninventory master 2\ntrack 4d master 0\n",
            "freemix-record-track-map 1\ninventory master 2\nlayout split\ntrack 4d master 0\n",
            "freemix-record-track-map 1\ninventory master 2\ntrack 4d master 0 extra\n",
            "freemix-record-track-map 1\ninventory master 2\ntrack 4d master 0\ninventory master 2\n",
            "freemix-record-track-map 1\ninventory master 2\ntrack 4d master 0\nextra\n",
        ] {
            assert!(
                RecorderTrackMap::from_document(document).is_err(),
                "{document:?}"
            );
        }
    }
}
