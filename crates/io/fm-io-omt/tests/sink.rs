//! The `fm-io-api` OMT output sink: format, lifecycle, and conversion.

use std::num::{NonZeroU128, NonZeroUsize};
use std::thread;
use std::time::{Duration, Instant};

use fm_capabilities::{FormatDescriptor, StableId};
use fm_frame::{
    AudioBlock, ClockDomainId, CpuVideoFrame, CpuVideoPayload, CpuVideoPlane, MediaTimestamp,
    MediaTiming, NormalizedDuration, NormalizedTimestamp, OriginalTimestamp, SequenceNumber,
    TimeBase,
};
use fm_io_api::{
    DeviceId, IoError, LifecycleState, MediaSink, MemoryDomain, OpenOptions, SignalLossPolicy,
    SinkId, WriteError,
};
use fm_io_omt::{Frame, FrameKind, OmtMedia, OmtSink, OmtSinkConfig, Receiver, VideoCodec};
use fm_types::{ChannelLayout, PixelFormat, SampleRate, VideoDimensions};

fn nonzero(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).unwrap()
}

fn timing() -> MediaTiming {
    MediaTiming::new(
        OriginalTimestamp::new(
            MediaTimestamp::new(0),
            TimeBase::new(1, 1_000_000_000).unwrap(),
        ),
        NormalizedTimestamp::from_nanos(2_000_000_000),
        NormalizedDuration::from_nanos(40_000_000).unwrap(),
        ClockDomainId::new(nonzero(7)),
        SequenceNumber::new(1),
    )
    .unwrap()
}

fn config() -> OmtSinkConfig {
    OmtSinkConfig {
        id: SinkId::new(nonzero(11)),
        device_id: DeviceId::new(nonzero(12)),
        hostname: String::new(),
        source: "Program".to_owned(),
        port: 0,
        clock_domain: ClockDomainId::new(nonzero(7)),
        codec: VideoCodec::Bgra,
        width: 2,
        height: 2,
        frame_rate: (25, 1),
        info: None,
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

fn bgra_frame(pixel: [u8; 4]) -> CpuVideoFrame {
    let mut bytes = Vec::new();
    for _ in 0..4 {
        bytes.extend_from_slice(&pixel);
    }
    CpuVideoFrame::new(
        timing(),
        CpuVideoPayload::new(
            PixelFormat::Bgra8,
            VideoDimensions::new(2, 2).unwrap(),
            vec![CpuVideoPlane::new(8, bytes).unwrap()],
        )
        .unwrap(),
    )
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
fn the_sink_advertises_the_omt_format() {
    let sink = OmtSink::new(config()).unwrap();
    assert_eq!(sink.lifecycle(), LifecycleState::Closed);
    let descriptor = sink.descriptor();
    assert_eq!(descriptor.name, "Program");
    assert_eq!(descriptor.capabilities.formats, vec![sink.format().clone()]);
    assert_eq!(sink.format().kind, StableId::new("video.omt").unwrap());
    assert_eq!(
        sink.format().fields.get(&StableId::new("codec").unwrap()),
        Some(&fm_capabilities::FormatValue::Text("bgra".to_owned()))
    );
    assert!(
        OmtSink::new(OmtSinkConfig {
            codec: VideoCodec::Yuy2,
            ..config()
        })
        .is_err()
    );
}

#[test]
fn the_sink_publishes_video_and_audio_to_a_receiver() {
    let mut sink = OmtSink::new(config()).unwrap();
    let format = sink.format().clone();
    sink.open(options(format.clone())).unwrap();
    sink.start().unwrap();
    let address = sink.local_addr().unwrap();
    let mut receiver = Receiver::connect(address).unwrap();
    receiver.subscribe(true, true, false).unwrap();
    wait_for("the video subscription", || {
        sink.subscribers(FrameKind::Video) == 1
    });

    sink.try_send(OmtMedia::Video(bgra_frame([1, 2, 3, 4])))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let video = loop {
        assert!(Instant::now() < deadline, "no OMT video arrived");
        if let Some(Frame::Video(video)) =
            receiver.receive(Some(Duration::from_millis(200))).unwrap()
        {
            break video;
        }
    };
    assert_eq!(video.codec, VideoCodec::Bgra);
    assert_eq!(video.width, 2);
    assert_eq!(video.height, 2);
    assert_eq!(video.timestamp, 20_000_000, "2 s in 100 ns ticks");
    assert_eq!(
        video.data,
        vec![1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4]
    );

    let audio = AudioBlock::new(
        timing(),
        SampleRate::new(48_000).unwrap(),
        ChannelLayout::stereo(),
        vec![vec![0.25; 4], vec![-0.5; 4]],
    )
    .unwrap();
    sink.try_send(OmtMedia::Audio(audio)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let sound = loop {
        assert!(Instant::now() < deadline, "no OMT audio arrived");
        if let Some(Frame::Audio(audio)) =
            receiver.receive(Some(Duration::from_millis(200))).unwrap()
        {
            break audio;
        }
    };
    assert_eq!(sound.sample_rate, 48_000);
    assert_eq!(sound.channels.len(), 2);
    assert_eq!(sound.channels[0], vec![0.25; 4]);
    assert_eq!(sound.channels[1], vec![-0.5; 4]);

    sink.stop().unwrap();
    assert_eq!(sink.lifecycle(), LifecycleState::Open);
    assert!(matches!(
        sink.try_send(OmtMedia::Video(bgra_frame([0, 0, 0, 255]))),
        Err(WriteError::Rejected {
            error: IoError::InvalidState { .. },
            ..
        })
    ));
    sink.close().unwrap();
    assert_eq!(sink.lifecycle(), LifecycleState::Closed);
}

#[test]
fn a_sink_refuses_another_format() {
    let mut sink = OmtSink::new(config()).unwrap();
    let other = FormatDescriptor::new(StableId::new("video.other").unwrap());
    assert!(matches!(
        sink.open(options(other)),
        Err(IoError::UnsupportedFormat)
    ));
    assert_eq!(sink.lifecycle(), LifecycleState::Closed);
}
