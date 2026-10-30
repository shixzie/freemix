//! The `fm-io-api` OMT input source: subscription, conversion, and tally.

use std::num::{NonZeroU128, NonZeroUsize};
use std::thread;
use std::time::{Duration, Instant};

use fm_capabilities::{FormatDescriptor, StableId};
use fm_frame::ClockDomainId;
use fm_io_api::{
    DeviceId, IoError, LifecycleState, MediaSource, MediaTransfer, MediaUnit, MemoryDomain,
    OpenOptions, SignalLossPolicy, SourceId,
};
use fm_io_omt::{
    AudioFrame, OmtSource, OmtSourceConfig, Quality, Sender, Tally, VideoCodec, VideoFrame,
};
use fm_types::{PixelFormat, SampleRate};

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn config(sender: &Sender) -> OmtSourceConfig {
    OmtSourceConfig {
        id: SourceId::new(nonzero(21)),
        device_id: DeviceId::new(nonzero(22)),
        name: "OMT Camera".to_owned(),
        address: sender.local_addr(),
        clock_domain: ClockDomainId::new(nonzero(7)),
        codec: VideoCodec::Bgra,
        width: 2,
        height: 2,
        frame_rate: (25, 1),
    }
}

fn options(format: FormatDescriptor) -> OpenOptions {
    OpenOptions {
        format,
        clock_domain: ClockDomainId::new(nonzero(7)),
        memory_domain: MemoryDomain::Cpu,
        queue_capacity: NonZeroUsize::new(4).unwrap(),
        signal_loss: SignalLossPolicy::Hold,
    }
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("{what} did not happen in time");
}

#[test]
fn the_source_receives_and_converts_an_omt_stream() {
    let sender = Sender::bind("127.0.0.1:0").unwrap();
    let mut source = OmtSource::new(config(&sender)).unwrap();
    let format = source.format().clone();
    source.open(options(format)).unwrap();
    assert_eq!(source.lifecycle(), LifecycleState::Open);
    source.start().unwrap();
    wait_for("the source subscription", || {
        sender.subscribers(fm_io_omt::FrameKind::Video) == 1
    });

    let data = vec![1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4];
    let video = VideoFrame::new(VideoCodec::Bgra, 2, 2, (25, 1), data.clone())
        .unwrap()
        .at(20_000_000);
    assert_eq!(sender.send_video(&video, None).unwrap(), 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    let media = loop {
        assert!(Instant::now() < deadline, "no OMT video arrived");
        if let Some(MediaTransfer::Live(media)) = source.try_receive().unwrap() {
            break media;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let fm_io_omt::OmtMedia::Video(frame) = media else {
        panic!("a video frame was expected");
    };
    assert_eq!(frame.payload().format(), PixelFormat::Bgra8);
    assert_eq!(frame.payload().plane(0).unwrap().bytes(), data.as_slice());
    assert_eq!(
        frame.timing().presentation_timestamp().as_nanos(),
        2_000_000_000
    );

    let audio = AudioFrame::new(48_000, vec![vec![0.25; 4], vec![-0.5; 4]])
        .unwrap()
        .at(20_000_000);
    assert_eq!(sender.send_audio(&audio).unwrap(), 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    let media = loop {
        assert!(Instant::now() < deadline, "no OMT audio arrived");
        if let Some(MediaTransfer::Live(media)) = source.try_receive().unwrap() {
            break media;
        }
        thread::sleep(Duration::from_millis(5));
    };
    let fm_io_omt::OmtMedia::Audio(block) = media else {
        panic!("an audio block was expected");
    };
    assert_eq!(block.sample_rate(), SampleRate::new(48_000).unwrap());
    assert_eq!(block.planes()[0], vec![0.25; 4]);
    assert_eq!(block.planes()[1], vec![-0.5; 4]);
    assert_eq!(block.byte_len(), 2 * 4 * 4);

    source
        .set_tally(Tally {
            preview: false,
            program: true,
        })
        .unwrap();
    wait_for("the tally", || sender.tally().program);
    source.suggest_quality(Quality::High).unwrap();
    wait_for("the quality", || sender.quality() == Quality::High);

    source.close().unwrap();
    assert_eq!(source.lifecycle(), LifecycleState::Closed);
    assert!(matches!(
        source.try_receive(),
        Err(IoError::InvalidState { .. })
    ));
}

#[test]
fn the_source_refuses_another_format() {
    let sender = Sender::bind("127.0.0.1:0").unwrap();
    let mut source = OmtSource::new(config(&sender)).unwrap();
    let other = FormatDescriptor::new(StableId::new("video.other").unwrap());
    assert!(matches!(
        source.open(options(other)),
        Err(IoError::UnsupportedFormat)
    ));
    assert_eq!(source.lifecycle(), LifecycleState::Closed);
    sender.stop();
}
