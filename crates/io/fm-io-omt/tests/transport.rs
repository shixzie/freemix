//! OMT 1.0: frames, control metadata, discovery, and a sender/receiver pair.

use std::net::{IpAddr, Ipv4Addr};
use std::thread;
use std::time::{Duration, Instant};

use fm_io_omt::{
    AudioFrame, Command, DiscoveryClient, DiscoveryRegistry, DiscoveryServer, Frame, FrameKind,
    MetadataFrame, OMT_SERVICE_TYPE, OmtAddress, Quality, Receiver, Sender, SenderInfo, Tally,
    VideoCodec, VideoFlags, VideoFrame,
};

fn uyvy(width: u32, height: u32) -> VideoFrame {
    let len = usize::try_from(width * height * 2).unwrap();
    let mut data = Vec::with_capacity(len);
    for index in 0..len {
        #[allow(clippy::cast_possible_truncation)]
        data.push((index % 251) as u8);
    }
    VideoFrame::new(VideoCodec::Uyvy, width, height, (60, 1), data).unwrap()
}

fn audio() -> AudioFrame {
    AudioFrame::new(48_000, vec![vec![0.0, 0.5, -0.25], vec![0.0, 0.0, 0.0]]).unwrap()
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if ready() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("{what} did not happen in time");
}

fn receive_kind(receiver: &mut Receiver, kind: FrameKind) -> Frame {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(frame) = receiver.receive(Some(Duration::from_millis(200))).unwrap()
            && frame.kind() == kind
        {
            return frame;
        }
    }
    panic!("no {kind:?} frame arrived");
}

fn receive_metadata_containing(receiver: &mut Receiver, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(frame) = receiver.receive(Some(Duration::from_millis(200))).unwrap()
            && let Frame::Metadata(metadata) = frame
            && metadata.xml.contains(needle)
        {
            return metadata.xml;
        }
    }
    panic!("no metadata containing {needle:?} arrived");
}

fn wait_record(
    client: &mut DiscoveryClient,
    mut wanted: impl FnMut(&OmtAddress) -> bool,
) -> OmtAddress {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(record) = client.receive().unwrap()
            && wanted(&record)
        {
            return record;
        }
    }
    panic!("the discovery record did not arrive");
}

#[test]
fn frames_round_trip_with_their_metadata() {
    let video = uyvy(4, 2).at(120).with_metadata("<Vendor/>");
    let bytes = Frame::Video(video.clone()).encode().unwrap();
    assert_eq!(bytes[0], 1);
    assert_eq!(bytes[1], 2);
    let length = i32::from_le_bytes(bytes[12..16].try_into().unwrap());
    assert_eq!(usize::try_from(length).unwrap(), bytes.len() - 16);
    assert_eq!(fm_io_omt::frame_len(&bytes[..16]).unwrap(), bytes.len());
    assert_eq!(Frame::decode(&bytes).unwrap(), Frame::Video(video));

    let sound = audio().at(3).with_metadata("<Meter>1</Meter>");
    let bytes = Frame::Audio(sound.clone()).encode().unwrap();
    assert_eq!(bytes[1], 4);
    assert_eq!(Frame::decode(&bytes).unwrap(), Frame::Audio(sound));

    let metadata = MetadataFrame::new("<OMTTally Preview=\"true\" Program==\"false\" />")
        .unwrap()
        .at(9);
    let bytes = Frame::Metadata(metadata.clone()).encode().unwrap();
    assert_eq!(bytes[1], 1);
    assert_eq!(Frame::decode(&bytes).unwrap(), Frame::Metadata(metadata));
    assert_eq!(fm_io_omt::frame_len(&bytes[..16]).unwrap(), bytes.len());
}

#[test]
fn frames_are_bounded_and_strict() {
    let bytes = Frame::Video(uyvy(2, 2)).encode().unwrap();
    let mut wrong = bytes.clone();
    wrong[0] = 2;
    assert!(matches!(
        Frame::decode(&wrong),
        Err(fm_io_omt::OmtError::Version(2))
    ));
    wrong = bytes.clone();
    wrong[1] = 3;
    assert!(matches!(
        Frame::decode(&wrong),
        Err(fm_io_omt::OmtError::FrameKind(3))
    ));
    wrong = bytes.clone();
    wrong[12..16].copy_from_slice(&0_i32.to_le_bytes());
    assert!(matches!(
        Frame::decode(&wrong),
        Err(fm_io_omt::OmtError::Length)
    ));
    wrong = bytes.clone();
    wrong[16..20].copy_from_slice(&0xDEAD_BEEF_u32.to_le_bytes());
    assert!(matches!(
        Frame::decode(&wrong),
        Err(fm_io_omt::OmtError::Codec(_))
    ));

    let sound = Frame::Audio(audio()).encode().unwrap();
    let mut wrong = sound.clone();
    wrong[16..20].copy_from_slice(&VideoCodec::Uyvy.token().to_le_bytes());
    assert!(matches!(
        Frame::decode(&wrong),
        Err(fm_io_omt::OmtError::Codec(_))
    ));
    let mut wrong = sound.clone();
    wrong[16 + 16..16 + 20].copy_from_slice(&0x8000_0000_u32.to_le_bytes());
    assert!(matches!(
        Frame::decode(&wrong),
        Err(fm_io_omt::OmtError::Audio(_))
    ));

    let metadata = Frame::Metadata(MetadataFrame::new("<A/>").unwrap())
        .encode()
        .unwrap();
    let mut wrong = metadata.clone();
    let last = wrong.len() - 1;
    wrong[last] = b'x';
    assert!(matches!(
        Frame::decode(&wrong),
        Err(fm_io_omt::OmtError::Metadata(_))
    ));

    assert!(
        Frame::Video(uyvy(2, 2).with_flags(VideoFlags::ALPHA))
            .encode()
            .is_err()
    );
    assert!(
        VideoFrame::new(VideoCodec::Nv12, 3, 2, (25, 1), vec![0; 9]).is_err(),
        "NV12 needs even dimensions"
    );
    assert!(AudioFrame::new(48_000, vec![vec![0.0; 2]; 33]).is_err());
    assert!(
        Frame::Video(
            VideoFrame::new(VideoCodec::Bgra, 2, 2, (25, 1), vec![0; 16])
                .unwrap()
                .with_flags(VideoFlags::PRE_MULTIPLIED)
        )
        .encode()
        .is_err()
    );
}

#[test]
fn control_metadata_is_exact() {
    assert_eq!(
        Command::parse(fm_io_omt::SUBSCRIBE_VIDEO),
        Command::Subscribe(FrameKind::Video)
    );
    assert_eq!(
        Command::parse(fm_io_omt::SUBSCRIBE_AUDIO),
        Command::Subscribe(FrameKind::Audio)
    );
    assert_eq!(
        Command::parse(fm_io_omt::SUBSCRIBE_METADATA),
        Command::Subscribe(FrameKind::Metadata)
    );
    assert_eq!(
        Command::parse(fm_io_omt::PREVIEW_ON),
        Command::Preview(true)
    );
    assert_eq!(
        Command::parse(fm_io_omt::PREVIEW_OFF),
        Command::Preview(false)
    );
    for tally in [
        Tally::default(),
        Tally {
            preview: true,
            program: false,
        },
        Tally {
            preview: false,
            program: true,
        },
        Tally {
            preview: true,
            program: true,
        },
    ] {
        assert_eq!(Command::parse(tally.command()), Command::Tally(tally));
    }
    assert_eq!(
        Command::parse(r#"<OMTTally Preview="true" Program="false" />"#),
        Command::Tally(Tally {
            preview: true,
            program: false
        })
    );
    for quality in Quality::ALL {
        assert_eq!(
            Command::parse(&quality.command()),
            Command::Quality(quality)
        );
    }
    let info = SenderInfo {
        product_name: "FreeMix \"Studio\"".to_owned(),
        manufacturer: "FreeMix & Co".to_owned(),
        version: "1.0".to_owned(),
    };
    assert_eq!(Command::parse(&info.command()), Command::Info(info));
    assert_eq!(
        Command::parse("<Custom>1</Custom>"),
        Command::Metadata("<Custom>1</Custom>".to_owned())
    );
    assert_eq!(
        Tally {
            preview: true,
            program: false
        }
        .or(Tally {
            preview: false,
            program: true
        }),
        Tally {
            preview: true,
            program: true
        }
    );
    assert_eq!(Quality::Low.strongest(Quality::High), Quality::High);
    assert_eq!(Quality::Default.strongest(Quality::Medium), Quality::Medium);
}

#[test]
fn dns_sd_names_the_source() {
    let info = fm_io_omt::service_info(
        "studio",
        "Camera 1",
        6500,
        &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
    )
    .unwrap();
    assert_eq!(info.get_fullname(), "studio (Camera 1)._omt._tcp.local.");
    assert_eq!(info.get_hostname(), "studio.local.");
    assert_eq!(info.get_port(), 6500);
    assert_eq!(fm_io_omt::OMT_DNS_SD_SERVICE, "_omt._tcp.local.");
    assert!(fm_io_omt::service_info("bad\nname", "x", 6500, &[]).is_err());
}

#[test]
fn discovery_registry_uses_the_observed_address() {
    let mut registry = DiscoveryRegistry::default();
    let registered = registry.register(
        7,
        OmtAddress::new("studio", "Camera 1", 6500),
        IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9)),
    );
    assert_eq!(
        registered.addresses,
        vec![IpAddr::V4(Ipv4Addr::new(192, 168, 1, 9))]
    );
    assert_eq!(registry.len(), 1);
    assert_eq!(registry.records()[0], registered);
    assert_eq!(
        registered.service_name(),
        format!("studio (Camera 1).{OMT_SERVICE_TYPE}.local")
    );
    let xml = registered.to_xml();
    assert_eq!(OmtAddress::from_xml(&xml), Some(registered.clone()));
    let removed = registry.deregister(7, "studio (Camera 1)").unwrap();
    assert!(removed.removed);
    assert!(registry.is_empty());
    assert_eq!(
        OmtAddress::from_xml(&removed.to_xml()),
        Some(removed.clone())
    );

    registry.register(
        8,
        OmtAddress::new("studio", "Camera 2", 6501),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    );
    registry.register(
        9,
        OmtAddress::new("studio", "Camera 3", 6502),
        IpAddr::V4(Ipv4Addr::LOCALHOST),
    );
    assert_eq!(registry.remove_client(8).len(), 1);
    assert_eq!(registry.len(), 1);
}

#[test]
fn discovery_server_repeats_registrations_and_removals() {
    let server = DiscoveryServer::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr();
    let mut first = DiscoveryClient::connect(address).unwrap();
    let name = "studio (Camera 1)";
    first
        .register(&OmtAddress::new("studio", "Camera 1", 6500))
        .unwrap();
    let echoed = wait_record(&mut first, |record| record.name == name && !record.removed);
    assert_eq!(echoed.addresses, vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);
    assert_eq!(echoed.port, 6500);

    // A later client hears the registrations already present.
    let mut second = DiscoveryClient::connect(address).unwrap();
    let existing = wait_record(&mut second, |record| record.name == name && !record.removed);
    assert_eq!(existing.addresses, vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]);

    first.deregister(name).unwrap();
    let removed = wait_record(&mut second, |record| record.name == name && record.removed);
    assert_eq!(removed.port, 6500);
    assert!(server.registry().is_empty());
    server.stop();
}

#[test]
fn sender_gates_frames_and_combines_tally_quality_and_preview() {
    let sender = Sender::bind("127.0.0.1:0").unwrap();
    let address = sender.local_addr();
    sender
        .set_info(&SenderInfo {
            product_name: "FreeMix".to_owned(),
            manufacturer: "FreeMix".to_owned(),
            version: "0.1".to_owned(),
        })
        .unwrap();
    let mut first = Receiver::connect(address).unwrap();
    let mut second = Receiver::connect(address).unwrap();
    // Both hear the sender information and the initial tally.
    receive_metadata_containing(&mut first, "OMTInfo");
    receive_metadata_containing(&mut first, "OMTTally");
    receive_metadata_containing(&mut second, "OMTInfo");

    // Nothing is sent before a subscription.
    let full = uyvy(8, 4);
    assert_eq!(sender.send_video(&full, None).unwrap(), 0);
    first.subscribe(true, true, true).unwrap();
    second.subscribe(false, false, true).unwrap();
    // Subscriptions arrive over the wire: wait for both receivers' metadata
    // subscriptions too, so the metadata count below is not a race.
    wait_until("the video subscriber", || {
        sender.subscribers(FrameKind::Video) == 1
    });
    wait_until("both metadata subscribers", || {
        sender.subscribers(FrameKind::Metadata) == 2
    });
    assert_eq!(sender.send_video(&full, None).unwrap(), 1);
    let Frame::Video(received) = receive_kind(&mut first, FrameKind::Video) else {
        unreachable!()
    };
    assert_eq!(received, full);

    assert_eq!(sender.send_audio(&audio()).unwrap(), 1);
    assert!(matches!(
        receive_kind(&mut first, FrameKind::Audio),
        Frame::Audio(_)
    ));
    assert_eq!(sender.send_metadata(0, "<Custom/>").unwrap(), 2);
    assert_eq!(
        receive_metadata_containing(&mut first, "<Custom/>"),
        "<Custom/>"
    );

    // Tally is combined across receivers and repeated to all.
    first
        .set_tally(Tally {
            preview: false,
            program: true,
        })
        .unwrap();
    wait_until("the program tally", || sender.tally().program);
    let tally = receive_metadata_containing(&mut second, r#"Program=="true""#);
    assert!(tally.contains(r#"Program=="true""#), "{tally}");

    // Quality is the strongest suggestion.
    first.suggest_quality(Quality::High).unwrap();
    wait_until("the quality suggestion", || {
        sender.quality() == Quality::High
    });

    // A preview receiver gets the one-eighth frame, not the full one.
    first.set_preview(true).unwrap();
    wait_until("the preview request", || sender.preview_requested());
    let preview = uyvy(1, 1).with_flags(VideoFlags::PREVIEW);
    assert_eq!(sender.send_video(&full, Some(&preview)).unwrap(), 1);
    let Frame::Video(received) = receive_kind(&mut first, FrameKind::Video) else {
        unreachable!()
    };
    assert_eq!(received.width, 1);
    assert!(received.flags.contains(VideoFlags::PREVIEW));

    sender.stop();
}
