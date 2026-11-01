//! Per-recorder track-map documents in a project bundle (AU-010).
//!
//! A track map is stored as `recorders/<slot>.trackmap` beside the manifest.
//! The file is the same canonical `freemix-record-track-map 1` document the
//! `fm-record` model validates, so a rewrite is byte-identical and an
//! unsupported map is refused before it reaches a recorder.

use std::{fmt, fs, io, path::PathBuf};

use fm_model::RecorderSlot;
use fm_record::{RecorderTrackMap, TrackMapDocumentError};

use crate::fsutil;
use crate::store::ProjectStore;

/// Directory inside a bundle that holds recorder track maps.
pub const RECORDER_TRACK_MAP_DIRECTORY: &str = "recorders";
/// Extension of one recorder track map.
pub const RECORDER_TRACK_MAP_EXTENSION: &str = "trackmap";

/// Why a recorder track map could not be read or written.
#[derive(Debug)]
pub enum TrackMapStoreError {
    Io(io::Error),
    Document(TrackMapDocumentError),
}

impl fmt::Display for TrackMapStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "recorder track map: {error}"),
            Self::Document(error) => write!(formatter, "recorder track map: {error}"),
        }
    }
}

impl std::error::Error for TrackMapStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Document(error) => Some(error),
        }
    }
}

impl From<io::Error> for TrackMapStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<TrackMapDocumentError> for TrackMapStoreError {
    fn from(error: TrackMapDocumentError) -> Self {
        Self::Document(error)
    }
}

impl ProjectStore {
    /// The path of `slot`'s track map inside this bundle.
    #[must_use]
    pub fn recorder_track_map_path(&self, slot: RecorderSlot) -> PathBuf {
        self.root()
            .join(RECORDER_TRACK_MAP_DIRECTORY)
            .join(format!("{}.{RECORDER_TRACK_MAP_EXTENSION}", slot.get()))
    }

    /// Atomically stores `map` as `slot`'s track map.
    ///
    /// # Errors
    ///
    /// Returns the directory creation, temporary-file, sync, or rename error.
    pub fn save_recorder_track_map(
        &self,
        slot: RecorderSlot,
        map: &RecorderTrackMap,
    ) -> Result<(), TrackMapStoreError> {
        let path = self.recorder_track_map_path(slot);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fsutil::write_atomically(&path, map.to_document().as_bytes())?;
        Ok(())
    }

    /// Loads `slot`'s track map, or `None` when the bundle has none.
    ///
    /// # Errors
    ///
    /// Returns the read error or the strict document error.
    pub fn load_recorder_track_map(
        &self,
        slot: RecorderSlot,
    ) -> Result<Option<RecorderTrackMap>, TrackMapStoreError> {
        match fs::read_to_string(self.recorder_track_map_path(slot)) {
            Ok(text) => Ok(Some(RecorderTrackMap::from_document(&text)?)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(TrackMapStoreError::Io(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use fm_record::{BusInventory, RecordBus, RecordTrack, TrackSource};
    use fm_types::audio_bus::AuxBusNumber;

    use super::*;

    fn map() -> RecorderTrackMap {
        let sub = AuxBusNumber::new(7).unwrap();
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
                    vec![TrackSource {
                        bus: RecordBus::Aux(sub),
                        channel: 0,
                    }],
                ),
            ],
            BusInventory::new(vec![(RecordBus::Master, 2), (RecordBus::Aux(sub), 2)]).unwrap(),
        )
        .unwrap()
    }

    fn store() -> (tempfile::TempDir, ProjectStore) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("show.freemix");
        std::fs::create_dir(&root).unwrap();
        let store = ProjectStore::new(&root).unwrap();
        (directory, store)
    }

    #[test]
    fn recorder_track_maps_round_trip_per_slot() {
        let (_directory, store) = store();
        assert_eq!(
            store
                .load_recorder_track_map(RecorderSlot::new(1).unwrap())
                .unwrap(),
            None
        );
        let map = map();
        store
            .save_recorder_track_map(RecorderSlot::new(2).unwrap(), &map)
            .unwrap();
        assert_eq!(
            store
                .load_recorder_track_map(RecorderSlot::new(2).unwrap())
                .unwrap(),
            Some(map.clone())
        );
        assert_eq!(
            std::fs::read_to_string(store.recorder_track_map_path(RecorderSlot::new(2).unwrap()))
                .unwrap(),
            map.to_document()
        );
    }

    #[test]
    fn recorder_track_maps_are_strict() {
        let (_directory, store) = store();
        let path = store.recorder_track_map_path(RecorderSlot::new(1).unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "freemix-record-track-map 2\n").unwrap();
        assert!(matches!(
            store.load_recorder_track_map(RecorderSlot::new(1).unwrap()),
            Err(TrackMapStoreError::Document(_))
        ));
    }
}
