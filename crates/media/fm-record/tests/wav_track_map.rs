use std::{
    cell::RefCell,
    fs,
    io::{self, Cursor, Read, Seek, SeekFrom, Write},
    num::NonZeroU128,
    path::PathBuf,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use fm_record::{
    BextField, BextMetadata, BusInventory, LargeFilePolicy, MAX_RECORDER_TRACKS,
    MAX_TRACK_CHANNELS, MAX_WAV_BUFFER_BYTES, RecordBus, RecordTrack, RecorderTrackMap,
    TrackFileMode, TrackLayout, TrackMapError, TrackSource, TrackWavError, TrackWavOptions,
    TrackWavRecorder, WavError, WavSampleFormat, WavSink, WavSpec, WavWriter, encode_sample,
    encode_wav_header, read_wav_info, recover_wav, recover_wav_file, split_track_file_name,
};
use fm_types::{BusId, FrameRate, Timecode};

// ---------------------------------------------------------------------------
// Helpers

/// A `Write + Seek` sink whose bytes stay observable after the writer that
/// owns the boxed handle is dropped (simulating a crash).
#[derive(Clone, Default)]
struct SharedFile(Rc<RefCell<Cursor<Vec<u8>>>>);

impl SharedFile {
    fn bytes(&self) -> Vec<u8> {
        self.0.borrow().get_ref().clone()
    }

    fn boxed(&self) -> Box<dyn WavSink> {
        Box::new(self.clone())
    }
}

impl Write for SharedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for SharedFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.0.borrow_mut().seek(pos)
    }
}

/// Sparse file: keeps the first `HEAD` bytes, discards the rest, and reads
/// zeros beyond the head. Lets recovery be exercised on multi-GiB lengths.
struct SparseFile {
    head: Vec<u8>,
    len: u64,
    pos: u64,
}

const HEAD: usize = 4096;

impl SparseFile {
    fn new(header: &[u8], len: u64) -> Self {
        let mut head = vec![0; HEAD];
        head[..header.len()].copy_from_slice(header);
        Self { head, len, pos: 0 }
    }

    fn head(&self, len: usize) -> &[u8] {
        &self.head[..len]
    }
}

impl Read for SparseFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let available = self.len.saturating_sub(self.pos);
        let count = usize::try_from(available.min(buf.len() as u64)).unwrap();
        for (index, byte) in buf[..count].iter_mut().enumerate() {
            let at = self.pos + index as u64;
            *byte = usize::try_from(at)
                .ok()
                .and_then(|at| self.head.get(at).copied())
                .unwrap_or(0);
        }
        self.pos += count as u64;
        Ok(count)
    }
}

impl Write for SparseFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for (index, byte) in buf.iter().enumerate() {
            let at = self.pos + index as u64;
            if let Some(slot) = usize::try_from(at)
                .ok()
                .and_then(|at| self.head.get_mut(at))
            {
                *slot = *byte;
            }
        }
        self.pos += buf.len() as u64;
        self.len = self.len.max(self.pos);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for SparseFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.pos = match pos {
            SeekFrom::Start(at) => at,
            SeekFrom::End(delta) => self.len.checked_add_signed(delta).unwrap(),
            SeekFrom::Current(delta) => self.pos.checked_add_signed(delta).unwrap(),
        };
        Ok(self.pos)
    }
}

/// Sink whose writes start failing after `budget` bytes.
struct FailingSink {
    inner: Cursor<Vec<u8>>,
    budget: usize,
}

impl Write for FailingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.budget {
            return Err(io::Error::other("disk full"));
        }
        self.budget -= buf.len();
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Seek for FailingSink {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

fn riff_spec(channels: u16, format: WavSampleFormat) -> WavSpec {
    WavSpec {
        large_file: LargeFilePolicy::RiffOnly,
        ..WavSpec::new(channels, 48_000, format)
    }
}

fn write_all(spec: WavSpec, samples: &[f32]) -> Vec<u8> {
    let file = SharedFile::default();
    let mut writer = WavWriter::create(file.boxed(), spec, 64).unwrap();
    writer.write_f32(samples).unwrap();
    let finalized = writer.finalize().unwrap();
    let bytes = file.bytes();
    assert_eq!(finalized.summary.file_len, bytes.len() as u64);
    bytes
}

fn aux(value: u128) -> RecordBus {
    RecordBus::Aux(BusId::new(NonZeroU128::new(value).unwrap()))
}

fn inventory() -> BusInventory {
    BusInventory::new(vec![(RecordBus::Master, 2), (aux(7), 4)]).unwrap()
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "fm-record-wav-{name}-{}-{counter}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------
// Exact header and sample bytes

#[test]
fn pcm16_stereo_riff_file_has_exact_bytes() {
    let bytes = write_all(riff_spec(2, WavSampleFormat::Pcm16), &[0.0, 1.0, -1.0, 0.5]);
    let expected: Vec<u8> = [
        &b"RIFF"[..],
        &[44, 0, 0, 0],
        b"WAVE",
        b"fmt ",
        &[16, 0, 0, 0],
        &[1, 0, 2, 0],
        &[0x80, 0xBB, 0, 0],
        &[0x00, 0xEE, 0x02, 0x00],
        &[4, 0, 16, 0],
        b"data",
        &[8, 0, 0, 0],
        &[0x00, 0x00, 0xFF, 0x7F, 0x00, 0x80, 0x00, 0x40],
    ]
    .concat();
    assert_eq!(bytes, expected);
}

#[test]
fn pcm24_mono_with_rf64_reservation_pads_odd_data() {
    let spec = WavSpec::new(1, 48_000, WavSampleFormat::Pcm24);
    assert_eq!(spec.header_len(), 80);
    let bytes = write_all(spec, &[0.25]);
    let expected: Vec<u8> = [
        &b"RIFF"[..],
        &[76, 0, 0, 0],
        b"WAVE",
        b"JUNK",
        &[28, 0, 0, 0],
        &[0; 28],
        b"fmt ",
        &[16, 0, 0, 0],
        &[1, 0, 1, 0],
        &[0x80, 0xBB, 0, 0],
        &[0x80, 0x32, 0x02, 0x00],
        &[3, 0, 24, 0],
        b"data",
        &[3, 0, 0, 0],
        &[0x00, 0x00, 0x20],
        &[0],
    ]
    .concat();
    assert_eq!(bytes, expected);
}

#[test]
fn sample_conversion_rounds_clamps_and_sanitizes() {
    let encode = |format, sample| {
        let mut out = Vec::new();
        encode_sample(format, sample, &mut out);
        out
    };
    assert_eq!(encode(WavSampleFormat::Pcm16, 2.0), [0xFF, 0x7F]);
    assert_eq!(encode(WavSampleFormat::Pcm16, -2.0), [0x00, 0x80]);
    assert_eq!(encode(WavSampleFormat::Pcm16, f32::NAN), [0x00, 0x00]);
    // 1.5 / 32768 rounds half away from zero to 2.
    assert_eq!(encode(WavSampleFormat::Pcm16, 1.5 / 32_768.0), [0x02, 0x00]);
    assert_eq!(encode(WavSampleFormat::Pcm24, -1.0), [0x00, 0x00, 0x80]);
    assert_eq!(encode(WavSampleFormat::Pcm24, 1.0), [0xFF, 0xFF, 0x7F]);
    assert_eq!(
        encode(WavSampleFormat::Pcm32, 1.0),
        [0xFF, 0xFF, 0xFF, 0x7F]
    );
    assert_eq!(
        encode(WavSampleFormat::Pcm32, -1.0),
        [0x00, 0x00, 0x00, 0x80]
    );
    assert_eq!(
        encode(WavSampleFormat::Pcm32, 0.5),
        [0x00, 0x00, 0x00, 0x40]
    );
    assert_eq!(encode(WavSampleFormat::Float32, 0.5), [0, 0, 0, 0x3F]);
    assert_eq!(
        encode(WavSampleFormat::Float32, f32::INFINITY),
        [0, 0, 0x80, 0x3F]
    );
    assert_eq!(encode(WavSampleFormat::Float32, f32::NAN), [0, 0, 0, 0]);
}

#[test]
fn float_stereo_with_bext_has_fact_and_metadata_bytes() {
    let rate = FrameRate::new(25, 1).unwrap();
    let timecode = Timecode::new(1, 0, 0, 0, 25).unwrap();
    let time_reference = BextMetadata::time_reference_for_timecode(timecode, rate, 48_000).unwrap();
    assert_eq!(time_reference, 172_800_000);
    let bext = BextMetadata {
        description: "Program".into(),
        originator: "freemix".into(),
        originator_reference: "REC-1".into(),
        origination_date: "2026-10-14".into(),
        origination_time: "12:34:56".into(),
        time_reference,
        coding_history: "A=PCM,F=48000\r\n".into(),
    };
    let spec = WavSpec {
        bext: Some(bext.clone()),
        ..riff_spec(2, WavSampleFormat::Float32)
    };
    // 12 + fmt(8+18) + fact(8+4) + bext(8+617+pad) + data header(8)
    assert_eq!(spec.header_len(), 12 + 26 + 12 + 626 + 8);
    let bytes = write_all(spec.clone(), &[0.5, -0.5, 0.25, 0.0]);
    assert_eq!(bytes.len(), 684 + 16);

    assert_eq!(
        &bytes[0..12],
        &[&b"RIFF"[..], &692_u32.to_le_bytes(), b"WAVE"].concat()[..]
    );
    assert_eq!(
        &bytes[12..38],
        &[
            &b"fmt "[..],
            &[18, 0, 0, 0],
            &[3, 0, 2, 0],
            &[0x80, 0xBB, 0, 0],
            &[0x00, 0xDC, 0x05, 0x00],
            &[8, 0, 32, 0],
            &[0, 0],
        ]
        .concat()[..]
    );
    assert_eq!(
        &bytes[38..50],
        &[&b"fact"[..], &[4, 0, 0, 0], &[2, 0, 0, 0]].concat()[..]
    );
    assert_eq!(
        &bytes[50..58],
        &[&b"bext"[..], &[0x69, 0x02, 0, 0]].concat()[..]
    );
    let bext_payload = &bytes[58..58 + 617];
    assert_eq!(&bext_payload[0..8], b"Program\0");
    assert_eq!(&bext_payload[256..264], b"freemix\0");
    assert_eq!(&bext_payload[288..294], b"REC-1\0");
    assert_eq!(&bext_payload[320..338], b"2026-10-1412:34:56");
    assert_eq!(&bext_payload[338..346], &172_800_000_u64.to_le_bytes());
    assert_eq!(&bext_payload[346..348], &[1, 0]);
    assert!(bext_payload[348..602].iter().all(|byte| *byte == 0));
    assert_eq!(&bext_payload[602..], b"A=PCM,F=48000\r\n");
    assert_eq!(bytes[675], 0, "odd bext payload is padded");
    assert_eq!(
        &bytes[676..684],
        &[&b"data"[..], &[16, 0, 0, 0]].concat()[..]
    );
    assert_eq!(&bytes[684..688], &[0, 0, 0, 0x3F]);

    let info = read_wav_info(&mut Cursor::new(&bytes)).unwrap();
    assert_eq!(info.spec, spec);
    assert_eq!(
        (info.data_offset, info.data_bytes, info.frames()),
        (684, 16, 2)
    );
    assert!(!info.rf64);
}

#[test]
fn bext_time_reference_handles_fractional_rates_and_validates() {
    let ntsc = FrameRate::new(30_000, 1_001).unwrap();
    let one_second = Timecode::new(0, 0, 1, 0, 30).unwrap();
    assert_eq!(
        BextMetadata::time_reference_for_timecode(one_second, ntsc, 48_000),
        Ok(48_048)
    );
    let frame_29 = Timecode::new(0, 0, 0, 29, 30).unwrap();
    assert_eq!(
        BextMetadata::time_reference_for_timecode(frame_29, FrameRate::new(25, 1).unwrap(), 48_000),
        Err(WavError::InvalidTimecode)
    );

    let invalid = [
        (
            BextMetadata {
                description: "x".repeat(257),
                ..BextMetadata::default()
            },
            BextField::Description,
        ),
        (
            BextMetadata {
                originator: "caf\u{e9}".into(),
                ..BextMetadata::default()
            },
            BextField::Originator,
        ),
        (
            BextMetadata {
                origination_date: "2026/10/1".into(),
                ..BextMetadata::default()
            },
            BextField::OriginationDate,
        ),
        (
            BextMetadata {
                origination_time: "12:3x:56".into(),
                ..BextMetadata::default()
            },
            BextField::OriginationTime,
        ),
        (
            BextMetadata {
                coding_history: "A=PCM\t".into(),
                ..BextMetadata::default()
            },
            BextField::CodingHistory,
        ),
    ];
    for (bext, field) in invalid {
        assert_eq!(bext.validate(), Err(WavError::InvalidBext { field }));
    }
}

#[test]
fn multichannel_files_use_wave_format_extensible() {
    let bytes = write_all(riff_spec(4, WavSampleFormat::Pcm24), &[0.0, 0.0, 0.0, 0.0]);
    assert_eq!(bytes.len(), 12 + 48 + 8 + 12);
    assert_eq!(
        &bytes[12..60],
        &[
            &b"fmt "[..],
            &[40, 0, 0, 0],
            &[0xFE, 0xFF, 4, 0],
            &[0x80, 0xBB, 0, 0],
            &[0x00, 0xCA, 0x08, 0x00],
            &[12, 0, 24, 0],
            &[22, 0, 24, 0],
            &[0, 0, 0, 0],
            &[
                1, 0, 0, 0, 0, 0, 0x10, 0, 0x80, 0, 0, 0xAA, 0, 0x38, 0x9B, 0x71
            ],
        ]
        .concat()[..]
    );
    let info = read_wav_info(&mut Cursor::new(&bytes)).unwrap();
    assert_eq!(info.spec, riff_spec(4, WavSampleFormat::Pcm24));
}

#[test]
fn rf64_header_bytes_for_large_data_are_exact() {
    let spec = WavSpec::new(2, 48_000, WavSampleFormat::Pcm16);
    let data: u64 = 5 << 30;
    let header = encode_wav_header(&spec, data).unwrap();
    let riff: u64 = 72 + data;
    let expected: Vec<u8> = [
        &b"RF64"[..],
        &[0xFF; 4],
        b"WAVE",
        b"ds64",
        &[28, 0, 0, 0],
        &riff.to_le_bytes(),
        &data.to_le_bytes(),
        &(data / 4).to_le_bytes(),
        &[0, 0, 0, 0],
        b"fmt ",
        &[16, 0, 0, 0],
        &[1, 0, 2, 0],
        &[0x80, 0xBB, 0, 0],
        &[0x00, 0xEE, 0x02, 0x00],
        &[4, 0, 16, 0],
        b"data",
        &[0xFF; 4],
    ]
    .concat();
    assert_eq!(header, expected);

    let riff_only = riff_spec(2, WavSampleFormat::Pcm16);
    assert_eq!(riff_only.max_data_bytes(), 0xFFFF_FFFE - 36);
    assert_eq!(
        encode_wav_header(&riff_only, data),
        Err(WavError::SizeLimit {
            requested: data,
            limit: 0xFFFF_FFFE - 36
        })
    );
}

// ---------------------------------------------------------------------------
// Buffering, limits, and failure latching

#[test]
fn block_buffer_is_bounded_and_flushes_whole_frames() {
    let file = SharedFile::default();
    let spec = riff_spec(2, WavSampleFormat::Pcm16);
    let mut writer = WavWriter::create(file.boxed(), spec.clone(), 4).unwrap();
    assert_eq!(writer.buffer_capacity_bytes(), 16);
    assert_eq!(
        file.bytes().len(),
        44,
        "zero-size header is written eagerly"
    );

    writer.write_f32(&[0.0; 6]).unwrap();
    assert_eq!((writer.flushed_bytes(), writer.buffered_bytes()), (0, 12));
    writer.write_f32(&[0.0; 14]).unwrap();
    assert_eq!((writer.flushed_bytes(), writer.buffered_bytes()), (32, 8));
    assert_eq!(writer.frames(), 10);
    assert_eq!(file.bytes().len(), 44 + 32);
    assert_eq!(
        &file.bytes()[4..8],
        &[36, 0, 0, 0],
        "sizes untouched until checkpoint"
    );

    writer.checkpoint().unwrap();
    let bytes = file.bytes();
    assert_eq!(bytes.len(), 44 + 40);
    assert_eq!(&bytes[4..8], &[76, 0, 0, 0]);
    assert_eq!(&bytes[40..44], &[40, 0, 0, 0]);
    writer.write_f32(&[0.25, 0.25]).unwrap();
    let summary = writer.finalize().unwrap().summary;
    assert_eq!(
        (summary.data_bytes, summary.frames, summary.file_len),
        (44, 11, 88)
    );

    assert_eq!(
        WavWriter::create(SharedFile::default().boxed(), spec.clone(), 0).unwrap_err(),
        WavError::InvalidBufferFrames { frames: 0 }
    );
    let too_many = u32::try_from(MAX_WAV_BUFFER_BYTES / 4 + 1).unwrap();
    assert_eq!(
        WavWriter::create(SharedFile::default().boxed(), spec, too_many).unwrap_err(),
        WavError::InvalidBufferFrames { frames: too_many }
    );
}

#[test]
fn writer_refuses_partial_frames_and_oversize_and_latches_io_failure() {
    let mut writer = WavWriter::create(
        SharedFile::default().boxed(),
        riff_spec(2, WavSampleFormat::Pcm16),
        4,
    )
    .unwrap();
    assert_eq!(
        writer.write_f32(&[0.0; 3]),
        Err(WavError::PartialFrame { len: 3, frame: 2 })
    );
    assert_eq!(
        writer.write_encoded(&[0; 6]),
        Err(WavError::PartialFrame { len: 6, frame: 4 })
    );
    assert_eq!(writer.data_bytes(), 0);
    assert!(writer.can_accept_frames((0xFFFF_FFFE - 36) / 4));
    assert!(!writer.can_accept_frames((0xFFFF_FFFE - 36) / 4 + 1));

    let sink = FailingSink {
        inner: Cursor::new(Vec::new()),
        budget: 44 + 16,
    };
    let mut writer =
        WavWriter::create(Box::new(sink), riff_spec(2, WavSampleFormat::Pcm16), 4).unwrap();
    writer.write_f32(&[0.0; 8]).unwrap();
    assert_eq!(
        writer.write_f32(&[0.0; 8]),
        Err(WavError::Io(io::ErrorKind::Other))
    );
    assert!(writer.is_poisoned());
    assert_eq!(writer.write_f32(&[0.0; 2]), Err(WavError::Poisoned));
    assert_eq!(writer.finalize().unwrap_err(), WavError::Poisoned);
}

// ---------------------------------------------------------------------------
// Crash recovery

#[test]
fn recovery_of_unfinalized_file_matches_finalized_bytes() {
    let samples: Vec<f32> = (0..9)
        .map(|index| f32::from(u8::try_from(index).unwrap()) / 16.0)
        .collect();
    for spec in [
        WavSpec::new(1, 48_000, WavSampleFormat::Pcm24),
        riff_spec(3, WavSampleFormat::Pcm16),
        WavSpec {
            bext: Some(BextMetadata {
                description: "iso".into(),
                time_reference: 7,
                ..BextMetadata::default()
            }),
            ..WavSpec::new(3, 44_100, WavSampleFormat::Float32)
        },
    ] {
        let finalized = write_all(spec.clone(), &samples);

        let crashed = SharedFile::default();
        let mut writer = WavWriter::create(crashed.boxed(), spec.clone(), 2).unwrap();
        writer.write_f32(&samples).unwrap();
        writer.checkpoint().unwrap();
        drop(writer); // simulated crash: no finalize
        let mut bytes = crashed.bytes();
        // A torn partial frame follows the last whole frame.
        bytes.extend_from_slice(&[0xAB, 0xCD]);

        let mut cursor = Cursor::new(bytes);
        let recovery = recover_wav(&mut cursor).unwrap();
        assert!(!recovery.was_consistent);
        let mut repaired = cursor.into_inner();
        repaired.truncate(usize::try_from(recovery.file_len).unwrap());
        assert_eq!(repaired, finalized, "{spec:?}");
        assert_eq!(recovery.info.frames(), 3 * 3 / u64::from(spec.channels));

        // Recovering an already-finalized file is a no-op.
        let again = recover_wav(&mut Cursor::new(finalized.clone())).unwrap();
        assert!(again.was_consistent);
        assert_eq!(again.file_len, finalized.len() as u64);
    }
}

#[test]
fn recovery_without_checkpoint_rebuilds_sizes_from_data_length() {
    let file = SharedFile::default();
    let spec = riff_spec(2, WavSampleFormat::Pcm16);
    let mut writer = WavWriter::create(file.boxed(), spec.clone(), 1).unwrap();
    writer.write_f32(&[0.5, -0.5, 0.25, -0.25]).unwrap();
    drop(writer);
    let mut bytes = file.bytes();
    assert_eq!(&bytes[4..8], &[36, 0, 0, 0]);
    assert_eq!(&bytes[40..44], &[0, 0, 0, 0]);
    bytes.extend_from_slice(&[1, 2, 3]);

    let mut cursor = Cursor::new(bytes);
    let recovery = recover_wav(&mut cursor).unwrap();
    assert_eq!(recovery.discarded_tail_bytes, 3);
    assert_eq!(recovery.file_len, 52);
    let repaired = cursor.into_inner();
    assert_eq!(&repaired[4..8], &[44, 0, 0, 0]);
    assert_eq!(&repaired[40..44], &[8, 0, 0, 0]);
    assert_eq!(
        &repaired[44..52],
        &[0x00, 0x40, 0x00, 0xC0, 0x00, 0x20, 0x00, 0xE0]
    );
}

#[test]
fn recovery_promotes_oversized_files_to_rf64_in_place() {
    let spec = WavSpec::new(2, 48_000, WavSampleFormat::Float32);
    let header = encode_wav_header(&spec, 0).unwrap();
    let data: u64 = (5 << 30) + 8;
    let mut file = SparseFile::new(&header, header.len() as u64 + data + 3);
    let recovery = recover_wav(&mut file).unwrap();
    assert!(recovery.info.rf64);
    assert_eq!(recovery.info.data_bytes, data);
    assert_eq!(recovery.discarded_tail_bytes, 3);
    assert_eq!(recovery.file_len, header.len() as u64 + data);
    let expected = encode_wav_header(&spec, data).unwrap();
    assert_eq!(file.head(expected.len()), &expected[..]);
    assert_eq!(
        &expected[0..8],
        &[b'R', b'F', b'6', b'4', 0xFF, 0xFF, 0xFF, 0xFF]
    );

    // Shrinking back below the RIFF limit restores RIFF/JUNK.
    file.len = header.len() as u64 + 16;
    let shrunk = recover_wav(&mut file).unwrap();
    assert!(!shrunk.info.rf64);
    assert_eq!(
        file.head(header.len()),
        &encode_wav_header(&spec, 16).unwrap()[..]
    );

    let riff_only = riff_spec(2, WavSampleFormat::Pcm16);
    let header = encode_wav_header(&riff_only, 0).unwrap();
    let mut file = SparseFile::new(&header, header.len() as u64 + data);
    assert_eq!(recover_wav(&mut file), Err(WavError::Rf64Unavailable));
}

#[test]
fn recovery_rejects_non_wave_and_truncated_headers() {
    assert_eq!(
        recover_wav(&mut Cursor::new(b"RIFX\0\0\0\0WAVE".to_vec())),
        Err(WavError::NotWave)
    );
    let header = encode_wav_header(&riff_spec(2, WavSampleFormat::Pcm16), 0).unwrap();
    assert_eq!(
        recover_wav(&mut Cursor::new(header[..30].to_vec())),
        Err(WavError::Truncated)
    );
}

#[test]
fn file_recovery_truncates_torn_tail_on_disk() {
    let dir = TempDir::new("recover");
    let path = dir.0.join("take.wav");
    let spec = WavSpec::new(2, 48_000, WavSampleFormat::Pcm24);
    {
        let file = fs::File::create(&path).unwrap();
        let mut writer = WavWriter::create(Box::new(file), spec.clone(), 8).unwrap();
        writer
            .write_f32(&[0.5, -0.5, 0.25, -0.25, 0.0, 0.0])
            .unwrap();
        writer.checkpoint().unwrap();
        writer.write_f32(&[0.125, 0.125]).unwrap();
        // Writer dropped with two frames still buffered: they are lost.
    }
    let mut torn = fs::OpenOptions::new().append(true).open(&path).unwrap();
    torn.write_all(&[9, 9, 9, 9]).unwrap();
    drop(torn);

    let recovery = recover_wav_file(&path).unwrap();
    assert_eq!(recovery.info.data_bytes, 18);
    assert_eq!(recovery.discarded_tail_bytes, 4);
    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes.len() as u64, recovery.file_len);
    assert_eq!(bytes, write_all(spec, &[0.5, -0.5, 0.25, -0.25, 0.0, 0.0]));
}

// ---------------------------------------------------------------------------
// Track maps

#[test]
fn track_map_validates_buses_channels_and_limits() {
    let master = TrackSource::master;
    let check = |tracks: Vec<RecordTrack>| RecorderTrackMap::new(tracks, inventory()).unwrap_err();

    assert_eq!(check(Vec::new()), TrackMapError::NoTracks);
    assert_eq!(
        check(vec![RecordTrack::mono("A", TrackSource::new(aux(9), 0))]),
        TrackMapError::UnknownBus {
            track: 0,
            bus: aux(9)
        }
    );
    assert_eq!(
        check(vec![
            RecordTrack::mono("A", master(0)),
            RecordTrack::mono("B", TrackSource::new(aux(7), 4)),
        ]),
        TrackMapError::ChannelOutOfRange {
            track: 1,
            bus: aux(7),
            channel: 4,
            bus_channels: 4
        }
    );
    assert_eq!(
        check(vec![RecordTrack::stereo("A", master(1), master(1))]),
        TrackMapError::DuplicateSource {
            track: 0,
            source: master(1)
        }
    );
    assert_eq!(
        check(vec![RecordTrack::new("A", Vec::new())]),
        TrackMapError::EmptyTrack { track: 0 }
    );
    assert_eq!(
        check(vec![
            RecordTrack::mono("A", master(0)),
            RecordTrack::mono("A", master(1)),
        ]),
        TrackMapError::DuplicateTrackName { track: 1 }
    );
    let long = "n".repeat(65);
    for name in ["", " A", "A\n", long.as_str()] {
        assert_eq!(
            check(vec![RecordTrack::mono(name, master(0))]),
            TrackMapError::InvalidTrackName { track: 0 }
        );
    }

    let wide = BusInventory::new(vec![(aux(1), 64)]).unwrap();
    let too_wide = RecordTrack::whole_bus("W", aux(1), 17);
    assert_eq!(
        RecorderTrackMap::new(vec![too_wide], wide.clone()).unwrap_err(),
        TrackMapError::TrackTooWide {
            track: 0,
            channels: MAX_TRACK_CHANNELS + 1
        }
    );
    let many: Vec<RecordTrack> = (0..=MAX_RECORDER_TRACKS)
        .map(|index| RecordTrack::mono(format!("T{index}"), TrackSource::new(aux(1), 0)))
        .collect();
    assert_eq!(
        RecorderTrackMap::new(many, wide.clone()).unwrap_err(),
        TrackMapError::TooManyTracks { count: 33 }
    );
    let channels: Vec<RecordTrack> = (0..5)
        .map(|index| RecordTrack::whole_bus(format!("T{index}"), aux(1), 16))
        .collect();
    assert_eq!(
        RecorderTrackMap::new(channels, wide).unwrap_err(),
        TrackMapError::TooManyChannels { count: 80 }
    );

    assert_eq!(
        BusInventory::new(vec![(RecordBus::Master, 2), (RecordBus::Master, 2)]),
        Err(TrackMapError::DuplicateBus {
            bus: RecordBus::Master
        })
    );
    assert_eq!(
        BusInventory::new(vec![(aux(2), 0)]),
        Err(TrackMapError::InvalidBusChannelCount {
            bus: aux(2),
            channels: 0
        })
    );
}

fn show_map() -> RecorderTrackMap {
    RecorderTrackMap::new(
        vec![
            RecordTrack::mono("Host Mic", TrackSource::new(aux(7), 2)),
            RecordTrack::stereo("Program", TrackSource::master(0), TrackSource::master(1)),
            RecordTrack::new(
                "Stems",
                vec![
                    TrackSource::new(aux(7), 3),
                    TrackSource::new(aux(7), 0),
                    TrackSource::master(0),
                ],
            ),
        ],
        inventory(),
    )
    .unwrap()
}

const MASTER: [f32; 4] = [0.1, 0.2, 0.3, 0.4];
const AUX: [f32; 8] = [1.0, 1.1, 1.2, 1.3, 2.0, 2.1, 2.2, 2.3];

#[test]
fn routing_selects_channels_into_ordered_tracks() {
    let map = show_map();
    let layouts: Vec<TrackLayout> = map.tracks().iter().map(RecordTrack::layout).collect();
    assert_eq!(
        layouts,
        [
            TrackLayout::Mono,
            TrackLayout::Stereo,
            TrackLayout::Multichannel(3)
        ]
    );
    assert_eq!(map.total_channels(), 6);

    let buses: [(RecordBus, &[f32]); 2] = [(aux(7), &AUX), (RecordBus::Master, &MASTER)];
    let routed = map.route(2, &buses).unwrap();
    assert_eq!(
        routed,
        vec![
            vec![1.2, 2.2],
            vec![0.1, 0.2, 0.3, 0.4],
            vec![1.3, 1.0, 0.1, 2.3, 2.0, 0.3],
        ]
    );
    assert_eq!(
        map.route_interleaved(2, &buses).unwrap(),
        vec![1.2, 0.1, 0.2, 1.3, 1.0, 0.1, 2.2, 0.3, 0.4, 2.3, 2.0, 0.3]
    );

    assert_eq!(
        map.route(2, &[(RecordBus::Master, &MASTER)]),
        Err(TrackMapError::MissingBusInput { bus: aux(7) })
    );
    assert_eq!(
        map.route(2, &[(RecordBus::Master, &MASTER[..3]), (aux(7), &AUX)]),
        Err(TrackMapError::BusInputLength {
            bus: RecordBus::Master,
            expected: 4,
            actual: 3
        })
    );
    assert_eq!(
        map.route(2, &[(aux(8), &AUX)]),
        Err(TrackMapError::UnexpectedBusInput { bus: aux(8) })
    );
    assert_eq!(
        map.route(
            2,
            &[(RecordBus::Master, &MASTER), (RecordBus::Master, &MASTER)]
        ),
        Err(TrackMapError::DuplicateBusInput {
            bus: RecordBus::Master
        })
    );
}

fn options() -> TrackWavOptions {
    TrackWavOptions {
        sample_rate: 48_000,
        format: WavSampleFormat::Pcm16,
        large_file: LargeFilePolicy::RiffOnly,
        bext: Some(BextMetadata {
            description: "show".into(),
            time_reference: 480,
            ..BextMetadata::default()
        }),
        buffer_frames: 16,
    }
}

#[test]
fn split_wav_writes_one_file_per_track() {
    let map = show_map();
    let files: Vec<SharedFile> = (0..3).map(|_| SharedFile::default()).collect();
    assert_eq!(
        TrackWavRecorder::create(
            map.clone(),
            TrackFileMode::SplitPerTrack,
            &options(),
            vec![files[0].boxed()]
        )
        .unwrap_err(),
        TrackWavError::SinkCount {
            expected: 3,
            actual: 1
        }
    );
    let mut recorder = TrackWavRecorder::create(
        map,
        TrackFileMode::SplitPerTrack,
        &options(),
        files.iter().map(SharedFile::boxed).collect(),
    )
    .unwrap();
    let buses: [(RecordBus, &[f32]); 2] = [(RecordBus::Master, &MASTER), (aux(7), &AUX)];
    recorder.write(2, &buses).unwrap();
    assert_eq!(recorder.frames(), 2);
    assert!(matches!(
        recorder.write(2, &[(RecordBus::Master, &MASTER)]),
        Err(TrackWavError::Map(TrackMapError::MissingBusInput { .. }))
    ));
    assert_eq!(recorder.frames(), 2, "refused writes leave files unchanged");
    let summaries: Vec<_> = recorder
        .finalize()
        .unwrap()
        .into_iter()
        .map(|file| file.summary)
        .collect();
    assert_eq!(
        summaries.iter().map(|s| s.data_bytes).collect::<Vec<_>>(),
        [4, 8, 12]
    );

    let infos: Vec<_> = files
        .iter()
        .map(|file| read_wav_info(&mut Cursor::new(file.bytes())).unwrap())
        .collect();
    assert_eq!(
        infos
            .iter()
            .map(|info| info.spec.channels)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
    let descriptions: Vec<_> = infos
        .iter()
        .map(|info| info.spec.bext.as_ref().unwrap().description.clone())
        .collect();
    assert_eq!(
        descriptions,
        [
            "show track=Host Mic",
            "show track=Program",
            "show track=Stems"
        ]
    );
    assert_eq!(infos[0].spec.bext.as_ref().unwrap().time_reference, 480);

    // Host Mic: aux channel 2 -> 1.2, 2.2 as PCM16.
    let host = files[0].bytes();
    let offset = usize::try_from(infos[0].data_offset).unwrap();
    let mut expected = Vec::new();
    for sample in [1.2_f32, 2.2] {
        encode_sample(WavSampleFormat::Pcm16, sample, &mut expected);
    }
    assert_eq!(&host[offset..], &expected[..]);
    assert_eq!(&host[offset..], &[0xFF, 0x7F, 0xFF, 0x7F]);

    let program = files[1].bytes();
    let offset = usize::try_from(infos[1].data_offset).unwrap();
    assert_eq!(
        &program[offset..],
        &[0xCD, 0x0C, 0x9A, 0x19, 0x66, 0x26, 0x33, 0x33]
    );

    assert_eq!(
        split_track_file_name("take", 0, "Host Mic"),
        "take_01_Host_Mic.wav"
    );
}

#[test]
fn interleaved_mode_writes_one_multichannel_file() {
    let file = SharedFile::default();
    let options = TrackWavOptions {
        bext: None,
        ..options()
    };
    let mut recorder = TrackWavRecorder::create(
        show_map(),
        TrackFileMode::Interleaved,
        &options,
        vec![file.boxed()],
    )
    .unwrap();
    let buses: [(RecordBus, &[f32]); 2] = [(RecordBus::Master, &MASTER), (aux(7), &AUX)];
    recorder.write(2, &buses).unwrap();
    recorder.finalize().unwrap();

    let bytes = file.bytes();
    let info = read_wav_info(&mut Cursor::new(&bytes)).unwrap();
    assert_eq!(info.spec.channels, 6);
    assert_eq!(&bytes[20..22], &[0xFE, 0xFF], "six channels use EXTENSIBLE");
    assert_eq!(info.data_bytes, 24);
    let mut expected = Vec::new();
    for sample in [
        1.2_f32, 0.1, 0.2, 1.3, 1.0, 0.1, 2.2, 0.3, 0.4, 2.3, 2.0, 0.3,
    ] {
        encode_sample(WavSampleFormat::Pcm16, sample, &mut expected);
    }
    assert_eq!(
        &bytes[usize::try_from(info.data_offset).unwrap()..],
        &expected[..]
    );
}
