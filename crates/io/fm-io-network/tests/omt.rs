//! The OMT transport sink: raw pairs become an OMT source a receiver sees.

use std::net::{Ipv4Addr, SocketAddr};
use std::num::{NonZeroU16, NonZeroU32};
use std::thread;
use std::time::{Duration, Instant};

use fm_codec_ffmpeg::record::RecordFormat;
use fm_frame::SequenceNumber;
use fm_io_network::rtmp::{RtmpSinkConfig, raw_pair_packet};
use fm_io_network::{
    DestinationConfig, DestinationId, Endpoint, OmtTransportSink, OutputProtocol, QueueCapacity,
    ReconnectPolicy, RenditionId, TransportSink,
};
use fm_io_omt::{Frame, FrameKind, Receiver};
use fm_types::{ChannelLayout, FrameRate, SampleRate};

fn format() -> RecordFormat {
    RecordFormat::new(
        2,
        2,
        FrameRate::new(25, 1).unwrap(),
        SampleRate::new(48_000).unwrap(),
        ChannelLayout::stereo(),
        SequenceNumber::new(1),
    )
    .unwrap()
}

#[test]
#[allow(clippy::float_cmp)] // exact f32 round-trip of the same bits
fn a_raw_pair_reaches_an_omt_receiver() {
    let format = format();
    let rendition = RenditionId::new(NonZeroU32::new(1).unwrap());
    let mut sink = OmtTransportSink::new(RtmpSinkConfig::new(rendition, format.clone()));
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let port = NonZeroU16::new(port).unwrap();
    let endpoint = Endpoint::new("0.0.0.0", port.get(), "/").unwrap();
    let destination = DestinationConfig::new(
        DestinationId::new(1).unwrap(),
        OutputProtocol::Omt,
        endpoint.clone(),
        None,
        None,
        None,
        QueueCapacity::new(4).unwrap(),
        ReconnectPolicy::new(50, 100, 2, Some(3)).unwrap(),
    )
    .unwrap();
    sink.connect(&destination, &endpoint).unwrap();
    // The sender listens on the wildcard address, which Windows refuses as
    // a connection target; the receiver dials loopback on its port.
    let port = sink.sender().unwrap().local_addr().port();
    let mut receiver = Receiver::connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port))).unwrap();
    receiver.subscribe(true, true, false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while sink
        .sender()
        .is_none_or(|sender| sender.subscribers(FrameKind::Video) == 0)
        && Instant::now() < deadline
    {
        thread::sleep(Duration::from_millis(10));
    }

    let rgba = vec![1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4];
    let mut audio = Vec::new();
    for _ in 0..1_920 {
        for sample in [0.5_f32, -0.25] {
            audio.extend_from_slice(&sample.to_le_bytes());
        }
    }
    let packet = raw_pair_packet(rendition, &format, 3, &rgba, &audio).unwrap();
    sink.write(&packet).unwrap();

    let mut video = None;
    let mut sound = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    while (video.is_none() || sound.is_none()) && Instant::now() < deadline {
        if let Some(frame) = receiver.receive(Some(Duration::from_millis(100))).unwrap() {
            match frame {
                Frame::Video(frame) => video = Some(frame),
                Frame::Audio(frame) => sound = Some(frame),
                Frame::Metadata(_) => {}
            }
        }
    }
    let video = video.expect("an OMT picture");
    assert_eq!(video.codec, fm_io_omt::VideoCodec::Bgra);
    assert_eq!(video.data[..4], [3, 2, 1, 4]);
    assert_eq!(video.timestamp, packet.timestamp().as_nanos() / 100);
    let sound = sound.expect("OMT audio");
    assert_eq!(sound.channels.len(), 2);
    assert_eq!(sound.channels[0].len(), 1_920);
    assert!(sound.channels[0].iter().all(|sample| *sample == 0.5));
    assert!(sound.channels[1].iter().all(|sample| *sample == -0.25));
    sink.disconnect();
    assert!(sink.sender().is_none());
}
