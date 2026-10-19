//! What it costs to keep a journal, over a whole show rather than one command.
//!
//! The journal was correct and unbounded: compaction deleted rows and left
//! every page they had been written to in the write-ahead log, and every append
//! opened the database again and paid to rebuild that log's index. Both are
//! invisible in a test that records a handful of batches and both decide what a
//! cut costs at command five thousand.

use std::{
    fs,
    num::NonZeroU128,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_model::{Input, InputKind, MainMix, Project, ProjectSettings};
use fm_persistence::{
    JournalError, MutationBatch, ProjectPosition, ProjectStore, RuntimeRouting, StoreError,
    StoredProject,
};
use fm_types::{
    AudioFormat, ChannelLayout, ColorMetadata, FrameRate, InputId, PixelFormat, ProjectId,
    SampleFormat, SampleRate, ScanMode, VideoDimensions, VideoFormat,
};

const WRITE_AHEAD_LOG_NAME: &str = "journal.db-wal";

/// Commands between two manifest checkpoints in the daemon.
const BATCHES_PER_CHECKPOINT: u64 = 64;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestBundle(PathBuf);

impl TestBundle {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "fm-journal-wal-{}-{}-{name}.freemix",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&path);
        Self(path)
    }

    fn store(&self) -> ProjectStore {
        let store = ProjectStore::new(self.0.clone()).unwrap();
        store.save(&manifest(0)).unwrap();
        store
    }
}

impl Drop for TestBundle {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn manifest(revision: u64) -> StoredProject {
    let frame_rate = FrameRate::new(60, 1).unwrap();
    let settings = ProjectSettings {
        frame_rate,
        video: VideoFormat {
            dimensions: VideoDimensions::new(1_920, 1_080).unwrap(),
            frame_rate,
            pixel_format: PixelFormat::Nv12,
            scan: ScanMode::Progressive,
            color: ColorMetadata::default(),
        },
        audio: AudioFormat {
            sample_rate: SampleRate::new(48_000).unwrap(),
            sample_format: SampleFormat::F32,
            channels: ChannelLayout::stereo(),
        },
    };
    let program = InputId::new(NonZeroU128::new(1).unwrap());
    let preview = InputId::new(NonZeroU128::new(2).unwrap());
    let mut domain = Project::new(
        ProjectId::new(NonZeroU128::new(7).unwrap()),
        "Journal",
        settings,
    );
    for (id, name) in [(program, "Program"), (preview, "Preview")] {
        domain.add_input(Input {
            id,
            name: name.to_owned(),
            kind: InputKind::Color,
            required_capabilities: Vec::new(),
        });
    }
    domain.set_main_mix(MainMix::new(program, preview));
    StoredProject::from_project(
        domain,
        RuntimeRouting {
            desired_program_id: Some(program),
            realized_program_id: Some(program),
            desired_preview_id: Some(preview),
            realized_preview_id: Some(preview),
        },
        ProjectPosition {
            revision,
            ..ProjectPosition::default()
        },
        Vec::new(),
    )
    .unwrap()
}

/// A batch whose revision is its sequence, so the manifest that checkpoints
/// through sequence `n` is simply the manifest at revision `n`.
fn batch(sequence: u64) -> MutationBatch {
    MutationBatch::new(sequence, sequence - 1, sequence, vec![0x5a; 400])
}

fn write_ahead_log_bytes(store: &ProjectStore) -> u64 {
    fs::metadata(store.journal_path().join(WRITE_AHEAD_LOG_NAME)).map_or(0, |file| file.len())
}

/// The write-ahead log must not be the one part of a bundle that only grows.
///
/// Compaction removed rows and never truncated the log they were written to, so
/// a bundle reused across shows carried every page of every command it had ever
/// served: the log reached megabytes while the database stayed at one page, it
/// survived a clean stop, and every open of it paid to rebuild its index. This
/// records and checkpoints far more than a show's worth of commands and demands
/// that the log end each cycle exactly where it ended the first one.
#[test]
fn the_write_ahead_log_stays_flat_across_many_checkpoints() {
    let bundle = TestBundle::new("flat-log");
    let store = bundle.store();
    let writer = store.open_journal_writer().unwrap();

    let mut sequence = 0;
    let mut settled = Vec::new();
    let mut peak = 0;
    for _ in 0..12 {
        for _ in 0..BATCHES_PER_CHECKPOINT {
            sequence += 1;
            writer.append_batch(&batch(sequence)).unwrap();
        }
        peak = peak.max(write_ahead_log_bytes(&store));
        writer
            .checkpoint_and_compact(&manifest(sequence), sequence)
            .unwrap();
        settled.push(write_ahead_log_bytes(&store));
    }

    assert_eq!(sequence, 768, "the run is longer than a show's worth");
    assert!(
        settled.iter().all(|bytes| *bytes == settled[0]),
        "the log must end every checkpoint where it ended the first one, got {settled:?}"
    );
    // The log is bounded by what one checkpoint interval writes, not by the
    // history behind it: 768 records of 400 bytes cannot fit in this.
    assert!(
        peak < 1024 * 1024,
        "the log peaked at {peak} bytes between checkpoints"
    );
    // Dropping the writer must not leave the growth behind either: the log
    // survived a clean SIGTERM before, larger than it started.
    drop(writer);
    assert_eq!(write_ahead_log_bytes(&store), settled[0]);
    assert_eq!(store.load().unwrap().position().revision, sequence);
    assert_eq!(store.scan_journal().unwrap().batches(), []);
}

/// An append must go through the handle the writer already holds, not reopen
/// the database by path.
///
/// Every append used to open a fresh database — a stat, a lock, a write-ahead
/// log index rebuild, two pragma round trips and two schema queries — before
/// the transaction that was the actual work. The observable difference is that
/// a writer no longer needs the database to be reachable by name: with the file
/// renamed out from under it, the one-shot path cannot find it and the writer
/// carries on, because it never looks.
#[test]
fn the_held_writer_appends_without_reopening_the_database() {
    let bundle = TestBundle::new("no-reopen");
    let store = bundle.store();
    let writer = store.open_journal_writer().unwrap();
    writer.append_batch(&batch(1)).unwrap();

    let database = store.journal_database_path();
    let renamed = database.with_extension("db.renamed");
    fs::rename(&database, &renamed).unwrap();

    // The one-shot path resolves the database by name every time, so it now
    // finds a surviving write-ahead log with no database beside it.
    assert!(
        matches!(
            store.append_batch(&batch(2)),
            Err(StoreError::Journal(JournalError::MissingDatabase { .. }))
        ),
        "the one-shot path must reopen by path, or this proves nothing"
    );
    // The writer does not, so it is unaffected.
    writer.append_batch(&batch(2)).unwrap();
    writer.append_batch(&batch(3)).unwrap();

    fs::rename(&renamed, &database).unwrap();
    drop(writer);

    let scan = store.scan_journal().unwrap();
    let sequences: Vec<u64> = scan.batches().iter().map(MutationBatch::sequence).collect();
    assert_eq!(sequences, vec![1, 2, 3]);
    assert_eq!(scan.observations(), []);
}

/// A group commit is the rows a batch at a time writes, in one transaction:
/// it reads back exactly like them, it continues the chain like them, and a
/// group whose chain breaks anywhere leaves none of its batches behind.
#[test]
fn a_group_commit_reads_back_as_single_appends_and_is_refused_whole() {
    let bundle = TestBundle::new("group-commit");
    let store = bundle.store();
    let writer = store.open_journal_writer().unwrap();
    writer.append_batch(&batch(1)).unwrap();
    writer
        .append_batches(&[batch(2), batch(3), batch(4)])
        .unwrap();
    writer.append_batches(&[]).unwrap();
    assert_eq!(writer.head_sequence(), 4);

    // The fourth batch of this group skips a sequence: nothing of it lands.
    let refused = writer.append_batches(&[batch(5), batch(6), batch(7), batch(9)]);
    assert!(
        matches!(
            refused,
            Err(StoreError::Journal(JournalError::SequenceGap {
                expected: 8,
                found: 9
            }))
        ),
        "{refused:?}"
    );
    assert_eq!(writer.head_sequence(), 4);
    writer.append_batches(&[batch(5), batch(6)]).unwrap();
    drop(writer);

    let scan = store.scan_journal().unwrap();
    let sequences: Vec<u64> = scan.batches().iter().map(MutationBatch::sequence).collect();
    assert_eq!(sequences, vec![1, 2, 3, 4, 5, 6]);
    assert_eq!(scan.batches()[2], batch(3));
    assert_eq!(scan.observations(), []);
}

/// The page size the write-ahead log at `path` records in its header.
fn log_page_bytes(path: &std::path::Path) -> u64 {
    let header = fs::read(path).unwrap();
    u64::from(u32::from_be_bytes(header[8..12].try_into().unwrap()))
}

/// A crash in the middle of a group commit: the log holds only part of the
/// group's frames. Recovery keeps every batch committed before the group and
/// none of the group, whether the tear falls inside a frame or exactly on a
/// frame boundary before the group's commit frame, and the journal continues
/// from the last whole commit.
#[test]
fn a_group_commit_torn_by_a_crash_is_dropped_whole() {
    const LOG_HEADER_BYTES: u64 = 32;
    const FRAME_HEADER_BYTES: u64 = 24;
    // Large enough that the group spans several logged pages.
    let large =
        |sequence: u64| MutationBatch::new(sequence, sequence - 1, sequence, vec![0xa5; 3_000]);

    for tear_on_frame_boundary in [false, true] {
        let bundle = TestBundle::new("torn-group");
        let store = bundle.store();
        let log = store.journal_path().join(WRITE_AHEAD_LOG_NAME);
        let writer = store.open_journal_writer().unwrap();
        writer.append_batch(&large(1)).unwrap();
        writer.append_batch(&large(2)).unwrap();
        let before = fs::metadata(&log).unwrap().len();
        writer
            .append_batches(&[large(3), large(4), large(5), large(6)])
            .unwrap();
        let after = fs::metadata(&log).unwrap().len();
        drop(writer);

        let frame_bytes = FRAME_HEADER_BYTES + log_page_bytes(&log);
        assert_eq!((before - LOG_HEADER_BYTES) % frame_bytes, 0);
        let group_frames = (after - before) / frame_bytes;
        assert!(
            group_frames >= 3,
            "the group must span several frames to be torn inside, it wrote {group_frames}"
        );
        // Keep the group's first frames and lose the rest, including its
        // commit frame: either a whole number of frames or one cut short.
        let kept = before + frame_bytes * (group_frames / 2);
        let torn = if tear_on_frame_boundary {
            kept
        } else {
            kept + frame_bytes / 2
        };
        fs::OpenOptions::new()
            .write(true)
            .open(&log)
            .unwrap()
            .set_len(torn)
            .unwrap();

        let scan = store.recover_journal().unwrap();
        let sequences: Vec<u64> = scan.batches().iter().map(MutationBatch::sequence).collect();
        assert_eq!(
            sequences,
            vec![1, 2],
            "a torn group must leave none of its batches (tear on a frame boundary: \
             {tear_on_frame_boundary})"
        );
        assert_eq!(
            scan.observations().is_empty(),
            tear_on_frame_boundary,
            "{:?}",
            scan.observations()
        );
        // The journal continues from the last whole commit: the lost group's
        // sequences are free again.
        let writer = store.open_journal_writer().unwrap();
        assert_eq!(writer.head_sequence(), 2);
        writer.append_batches(&[large(3), large(4)]).unwrap();
        drop(writer);
        let sequences: Vec<u64> = store
            .scan_journal()
            .unwrap()
            .batches()
            .iter()
            .map(MutationBatch::sequence)
            .collect();
        assert_eq!(sequences, vec![1, 2, 3, 4]);
    }
}
