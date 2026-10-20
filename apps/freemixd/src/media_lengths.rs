//! Media list item lengths (IN-006, Protocol 2.91).
//!
//! The transport reports only the length of the file a media list input has
//! open, so Studio could not show where any other item's unmarked out point
//! falls. The authority probes every item's file instead, off the session
//! loop: a [`MediaLengthWorker`] thread holds the set of item assets the
//! loop last published (after each revision: an item added, a list
//! restored, or the program rate changed) and probes each asset once,
//! caching its length by asset. Every [`REFRESH`] it checks each cached
//! file's size and modification time again and probes a file that changed
//! (a clip replaced or still being copied in). A file that does not exist,
//! cannot be read, or does not probe is reported unknown, as is an asset
//! still waiting for its probe.
//!
//! The loop collects a changed report ([`MediaLengthWorker::take_changed`])
//! and hands it to the control service
//! ([`fm_control::ControlService::set_media_list_lengths`]), which
//! republishes each list whose lengths changed. Lengths are runtime
//! knowledge only: never journaled or saved, so a replay never depends on
//! them.
//!
//! What probes a file is a [`LengthProbe`]: a native daemon opens the file
//! through the same decoder path playback uses (see
//! `native_files::item_lengths`), so a probed length is exactly the frame
//! count the transport reports once the item plays. A simulated daemon,
//! which decodes no media, uses [`SimulatedLengths`]: every item has the
//! length of the synthetic source the simulated engine plays
//! ([`fm_engine::SYNTHETIC_MEDIA_SECONDS`] at the program rate), so it
//! agrees with the transport of whichever item is current.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use fm_control::MediaListLengths;
use fm_protocol::MediaListItemLength;
use fm_types::FrameRate;

/// How often each cached file is checked for a change.
pub(crate) const REFRESH: Duration = Duration::from_secs(2);

/// Probes one media list item's file.
pub(crate) trait LengthProbe: Send + Sync {
    /// The length of the file at `path` (`None` when the asset has no
    /// bundle path) played at program `rate`, or `None` when it cannot be
    /// read. Blocking: called only on the worker thread.
    fn length(&self, path: Option<&Path>, rate: FrameRate) -> Option<MediaListItemLength>;
}

/// A simulated daemon's deterministic stand-in: every item is as long as
/// the synthetic source the simulated engine plays.
pub(crate) struct SimulatedLengths;

impl LengthProbe for SimulatedLengths {
    fn length(&self, _: Option<&Path>, rate: FrameRate) -> Option<MediaListItemLength> {
        let frames = fm_engine::SYNTHETIC_MEDIA_SECONDS * u64::from(rate.numerator())
            / u64::from(rate.denominator());
        Some(MediaListItemLength { frames, rate }).filter(|length| length.is_valid())
    }
}

/// The probe of a native daemon without a bundle: nothing is read, so
/// every item is unknown.
pub(crate) struct NoLengths;

impl LengthProbe for NoLengths {
    fn length(&self, _: Option<&Path>, _: FrameRate) -> Option<MediaListItemLength> {
        None
    }
}

/// What a file looked like when it was probed: its size and modification
/// time, or `None` when it could not be read.
type Fingerprint = Option<(u64, Option<SystemTime>)>;

fn fingerprint(path: Option<&Path>) -> Fingerprint {
    let metadata = fs::metadata(path?).ok()?;
    Some((metadata.len(), metadata.modified().ok()))
}

/// One asset's cached probe.
struct Entry {
    rate: FrameRate,
    fingerprint: Fingerprint,
    length: Option<MediaListItemLength>,
    checked: Instant,
}

#[derive(Default)]
struct Shared {
    /// The newest assets and program rate the loop published, until the
    /// worker takes them.
    wanted: Option<(BTreeSet<String>, FrameRate)>,
    /// Every known length, by asset.
    lengths: MediaListLengths,
    /// Whether `lengths` changed since the loop last took it.
    changed: bool,
    stop: bool,
}

type SharedState = Arc<(Mutex<Shared>, Condvar)>;

fn lock(shared: &SharedState) -> MutexGuard<'_, Shared> {
    shared.0.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Probes media list item lengths on its own thread; see the module
/// documentation.
pub(crate) struct MediaLengthWorker {
    shared: SharedState,
    /// What the loop last published, so an unchanged set is not sent again.
    published: Option<(BTreeSet<String>, FrameRate)>,
}

impl MediaLengthWorker {
    /// Starts the worker for the assets under `assets_root` (`None`
    /// without a bundle: every file is then unreadable to a native probe).
    ///
    /// # Errors
    ///
    /// The thread could not start.
    pub(crate) fn spawn(
        assets_root: Option<PathBuf>,
        probe: Arc<dyn LengthProbe>,
    ) -> std::io::Result<Self> {
        let shared = SharedState::default();
        let worker = Arc::clone(&shared);
        thread::Builder::new()
            .name("freemixd-media-lengths".into())
            .spawn(move || run(&worker, assets_root.as_deref(), probe.as_ref()))?;
        Ok(Self {
            shared,
            published: None,
        })
    }

    /// Publishes every media list item asset and the program rate their
    /// lengths are counted at. Cheap: an unchanged set does nothing.
    pub(crate) fn want(&mut self, assets: BTreeSet<String>, rate: FrameRate) {
        if self
            .published
            .as_ref()
            .is_some_and(|(published, published_rate)| {
                *published == assets && *published_rate == rate
            })
        {
            return;
        }
        self.published = Some((assets.clone(), rate));
        lock(&self.shared).wanted = Some((assets, rate));
        self.shared.1.notify_all();
    }

    /// The lengths, when they changed since the last call.
    pub(crate) fn take_changed(&self) -> Option<MediaListLengths> {
        let mut shared = lock(&self.shared);
        if !shared.changed {
            return None;
        }
        shared.changed = false;
        Some(shared.lengths.clone())
    }
}

impl Drop for MediaLengthWorker {
    /// Stops the worker. It is not joined: a probe in flight is bounded by
    /// the decoder's own timeouts and its result is dropped.
    fn drop(&mut self) {
        lock(&self.shared).stop = true;
        self.shared.1.notify_all();
    }
}

/// The bundle path of item asset `uri`.
fn asset_path(assets_root: Option<&Path>, uri: &str) -> Option<PathBuf> {
    fm_model::validate_media_asset_uri(uri).ok()?;
    let key = uri.strip_prefix(fm_model::MEDIA_ASSET_URI_PREFIX)?;
    Some(assets_root?.join(key))
}

/// Publishes the cache's lengths when they changed.
fn publish(shared: &SharedState, cache: &BTreeMap<String, Entry>) {
    let lengths: MediaListLengths = cache
        .iter()
        .filter_map(|(uri, entry)| Some((uri.clone(), entry.length?)))
        .collect();
    let mut shared = lock(shared);
    if shared.lengths != lengths {
        shared.lengths = lengths;
        shared.changed = true;
    }
}

fn run(shared: &SharedState, assets_root: Option<&Path>, probe: &dyn LengthProbe) {
    let mut cache: BTreeMap<String, Entry> = BTreeMap::new();
    let mut wanted = BTreeSet::new();
    let mut rate = None;
    // Whether a pass was cut short and must resume without waiting.
    let mut pending = false;
    loop {
        {
            let mut state = lock(shared);
            if !pending {
                state = shared
                    .1
                    .wait_timeout_while(state, REFRESH, |state| {
                        !state.stop && state.wanted.is_none()
                    })
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            if state.stop {
                return;
            }
            if let Some((assets, new_rate)) = state.wanted.take() {
                wanted = assets;
                rate = Some(new_rate);
            }
        }
        let Some(rate) = rate else {
            continue;
        };
        cache.retain(|uri, _| wanted.contains(uri));
        pending = false;
        for uri in &wanted {
            {
                let state = lock(shared);
                if state.stop || state.wanted.is_some() {
                    pending = true;
                    break;
                }
            }
            let due = cache
                .get(uri)
                .is_none_or(|entry| entry.rate != rate || entry.checked.elapsed() >= REFRESH);
            if !due {
                continue;
            }
            let path = asset_path(assets_root, uri);
            let fingerprint = fingerprint(path.as_deref());
            if let Some(entry) = cache.get_mut(uri)
                && entry.rate == rate
                && entry.fingerprint == fingerprint
            {
                entry.checked = Instant::now();
                continue;
            }
            let length = probe
                .length(path.as_deref(), rate)
                .filter(|length| length.is_valid());
            cache.insert(
                uri.clone(),
                Entry {
                    rate,
                    fingerprint,
                    length,
                    checked: Instant::now(),
                },
            );
            // Each probe is reported as it lands, so a long list fills in.
            publish(shared, &cache);
        }
        publish(shared, &cache);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A probe that reads each file's length from its first byte (a
    /// stand-in decoder), counting its runs.
    #[derive(Default)]
    struct ByteProbe {
        runs: AtomicUsize,
    }

    impl LengthProbe for ByteProbe {
        fn length(&self, path: Option<&Path>, rate: FrameRate) -> Option<MediaListItemLength> {
            self.runs.fetch_add(1, Ordering::SeqCst);
            let frames = u64::from(*fs::read(path?).ok()?.first()?);
            Some(MediaListItemLength { frames, rate })
        }
    }

    fn rate() -> FrameRate {
        FrameRate::new(25, 1).unwrap()
    }

    fn wait_for(
        worker: &MediaLengthWorker,
        accept: impl Fn(&MediaListLengths) -> bool,
    ) -> MediaListLengths {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(lengths) = worker.take_changed()
                && accept(&lengths)
            {
                return lengths;
            }
            assert!(Instant::now() < deadline, "the lengths never arrived");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn assets(uris: &[&str]) -> BTreeSet<String> {
        uris.iter().map(|uri| (*uri).to_owned()).collect()
    }

    #[test]
    fn the_simulated_stand_in_is_the_synthetic_source() {
        for (numerator, denominator, frames) in [(25, 1, 250), (30_000, 1_001, 299), (60, 1, 600)] {
            let rate = FrameRate::new(numerator, denominator).unwrap();
            assert_eq!(
                SimulatedLengths.length(None, rate),
                Some(MediaListItemLength { frames, rate })
            );
        }
    }

    /// Items are probed off the caller's thread once each, a missing or
    /// unreadable file is unknown, a changed file is probed again, and an
    /// asset no list names any more is forgotten.
    #[test]
    fn items_are_probed_once_refreshed_when_they_change_and_forgotten() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("media")).unwrap();
        fs::write(root.path().join("media/a.mov"), [40]).unwrap();
        fs::write(root.path().join("media/empty.mov"), []).unwrap();
        let probe = Arc::new(ByteProbe::default());
        let mut worker = MediaLengthWorker::spawn(
            Some(root.path().to_path_buf()),
            Arc::clone(&probe) as Arc<dyn LengthProbe>,
        )
        .unwrap();
        worker.want(
            assets(&[
                "asset://media/a.mov",
                "asset://media/empty.mov",
                "asset://media/missing.mov",
            ]),
            rate(),
        );
        let lengths = wait_for(&worker, |lengths| !lengths.is_empty());
        assert_eq!(
            lengths,
            BTreeMap::from([(
                "asset://media/a.mov".to_owned(),
                MediaListItemLength {
                    frames: 40,
                    rate: rate()
                }
            )])
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while probe.runs.load(Ordering::SeqCst) < 3 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        // Wanting the same set again probes nothing.
        worker.want(
            assets(&[
                "asset://media/a.mov",
                "asset://media/empty.mov",
                "asset://media/missing.mov",
            ]),
            rate(),
        );
        // A file that appears or changes is probed again on the next check.
        fs::write(root.path().join("media/missing.mov"), [7, 7]).unwrap();
        fs::write(root.path().join("media/a.mov"), [90, 0]).unwrap();
        let lengths = wait_for(&worker, |lengths| {
            lengths.len() == 2 && lengths["asset://media/a.mov"].frames == 90
        });
        assert_eq!(lengths["asset://media/missing.mov"].frames, 7);
        // An unchanged file is never probed again.
        assert_eq!(probe.runs.load(Ordering::SeqCst), 5);
        // Dropping an item forgets its asset.
        worker.want(assets(&["asset://media/missing.mov"]), rate());
        let lengths = wait_for(&worker, |lengths| lengths.len() == 1);
        assert!(lengths.contains_key("asset://media/missing.mov"));
        // A new program rate probes again.
        let fifty = FrameRate::new(50, 1).unwrap();
        worker.want(assets(&["asset://media/missing.mov"]), fifty);
        let lengths = wait_for(&worker, |lengths| {
            lengths["asset://media/missing.mov"].rate == fifty
        });
        assert_eq!(lengths["asset://media/missing.mov"].frames, 7);
    }

    #[test]
    fn a_daemon_without_a_bundle_reports_every_native_item_unknown() {
        let probe = Arc::new(ByteProbe::default());
        let mut worker =
            MediaLengthWorker::spawn(None, Arc::clone(&probe) as Arc<dyn LengthProbe>).unwrap();
        worker.want(assets(&["asset://media/a.mov"]), rate());
        let deadline = Instant::now() + Duration::from_secs(20);
        while probe.runs.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(10));
        }
        thread::sleep(Duration::from_millis(50));
        assert_eq!(worker.take_changed(), None);
    }
}
