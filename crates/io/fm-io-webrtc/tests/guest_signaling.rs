//! Deterministic SDP, signaling, and bandwidth-adaptation contracts for
//! encrypted browser guests (RC-001 slice). No network is involved.

use fm_io_webrtc::bandwidth::{
    BandwidthController, BandwidthError, BandwidthEstimate, BitrateLadder, ChangeReason,
    ControllerConfig, EstimateSource, LadderRung, RungChange,
};
use fm_io_webrtc::sdp::{
    CandidateType, CodecName, CodecPreference, Fingerprint, IceCredentials, LocalEndpoint,
    MAX_SDP_BYTES, RtcpFeedback, Sdp, SdpCandidate, SdpDirection, SdpError, SdpMediaKind,
    SetupRole,
};
use fm_io_webrtc::signaling::{
    CandidateOutcome, NegotiationRole, OfferOutcome, SignalingConfig, SignalingError,
    SignalingPhase, SignalingSession,
};

macro_rules! guest_fp {
    () => {
        "01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF"
    };
}

macro_rules! studio_fp {
    () => {
        "A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5:A5"
    };
}

/// A Chrome-shaped guest offer: extra codecs (RED, G.722, PCMU/PCMA, DTMF,
/// RTX, VP9), extmap/msid/ssrc noise, and two trickle-style candidates.
const BROWSER_OFFER: &str = concat!(
    "v=0\r\n",
    "o=- 4611731400430051336 2 IN IP4 127.0.0.1\r\n",
    "s=-\r\n",
    "t=0 0\r\n",
    "a=group:BUNDLE 0 1\r\n",
    "a=extmap-allow-mixed\r\n",
    "a=msid-semantic: WMS guest\r\n",
    "m=audio 9 UDP/TLS/RTP/SAVPF 111 63 9 0 8 126\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "a=rtcp:9 IN IP4 0.0.0.0\r\n",
    "a=ice-ufrag:gst4\r\n",
    "a=ice-pwd:guestPassword0123456789\r\n",
    "a=ice-options:trickle\r\n",
    "a=fingerprint:sha-256 ",
    guest_fp!(),
    "\r\n",
    "a=setup:actpass\r\n",
    "a=mid:0\r\n",
    "a=extmap:1 urn:ietf:params:rtp-hdrext:ssrc-audio-level\r\n",
    "a=sendrecv\r\n",
    "a=msid:guest mic\r\n",
    "a=rtcp-mux\r\n",
    "a=rtpmap:111 opus/48000/2\r\n",
    "a=rtcp-fb:111 transport-cc\r\n",
    "a=fmtp:111 minptime=10;useinbandfec=1\r\n",
    "a=rtpmap:63 red/48000/2\r\n",
    "a=fmtp:63 111/111\r\n",
    "a=rtpmap:9 G722/8000\r\n",
    "a=rtpmap:0 PCMU/8000\r\n",
    "a=rtpmap:8 PCMA/8000\r\n",
    "a=rtpmap:126 telephone-event/8000\r\n",
    "a=ssrc:1001 cname:guest\r\n",
    "m=video 9 UDP/TLS/RTP/SAVPF 96 97 102 103 98\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "a=rtcp:9 IN IP4 0.0.0.0\r\n",
    "a=ice-ufrag:gst4\r\n",
    "a=ice-pwd:guestPassword0123456789\r\n",
    "a=ice-options:trickle\r\n",
    "a=fingerprint:sha-256 ",
    guest_fp!(),
    "\r\n",
    "a=setup:actpass\r\n",
    "a=mid:1\r\n",
    "a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01\r\n",
    "a=sendrecv\r\n",
    "a=msid:guest camera\r\n",
    "a=rtcp-rsize\r\n",
    "a=rtcp-mux\r\n",
    "a=rtpmap:96 VP8/90000\r\n",
    "a=rtcp-fb:96 goog-remb\r\n",
    "a=rtcp-fb:96 transport-cc\r\n",
    "a=rtcp-fb:96 ccm fir\r\n",
    "a=rtcp-fb:96 nack\r\n",
    "a=rtcp-fb:96 nack pli\r\n",
    "a=rtpmap:97 rtx/90000\r\n",
    "a=fmtp:97 apt=96\r\n",
    "a=rtpmap:102 H264/90000\r\n",
    "a=rtcp-fb:102 goog-remb\r\n",
    "a=rtcp-fb:102 transport-cc\r\n",
    "a=rtcp-fb:102 ccm fir\r\n",
    "a=rtcp-fb:102 nack\r\n",
    "a=rtcp-fb:102 nack pli\r\n",
    "a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n",
    "a=rtpmap:103 rtx/90000\r\n",
    "a=fmtp:103 apt=102\r\n",
    "a=rtpmap:98 VP9/90000\r\n",
    "a=rtcp-fb:98 nack\r\n",
    "a=fmtp:98 profile-id=0\r\n",
    "a=ssrc-group:FID 2001 2002\r\n",
    "a=ssrc:2001 cname:guest\r\n",
    "a=ssrc:2002 cname:guest\r\n",
    "a=candidate:842163049 1 udp 1677729535 203.0.113.7 54321 typ srflx raddr 0.0.0.0 rport 0 generation 0 ufrag gst4 network-cost 999\r\n",
    "a=candidate:1 1 udp 2122260223 4f8e1c2a-1b2c-4d3e-8f90-123456789abc.local 50000 typ host generation 0 ufrag gst4 network-id 1\r\n",
);

/// The studio's exact answer to [`BROWSER_OFFER`]: H.264 is preferred over
/// VP8, offered payload types are kept, RED/G.722/RTX/VP9 are dropped.
const STUDIO_ANSWER: &str = concat!(
    "v=0\r\n",
    "o=- 7 0 IN IP4 127.0.0.1\r\n",
    "s=-\r\n",
    "t=0 0\r\n",
    "a=group:BUNDLE 0 1\r\n",
    "a=ice-options:trickle\r\n",
    "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "a=mid:0\r\n",
    "a=ice-ufrag:fmS1\r\n",
    "a=ice-pwd:freemixStudioPassword01\r\n",
    "a=fingerprint:sha-256 ",
    studio_fp!(),
    "\r\n",
    "a=setup:active\r\n",
    "a=sendrecv\r\n",
    "a=rtcp-mux\r\n",
    "a=rtpmap:111 opus/48000/2\r\n",
    "a=rtcp-fb:111 transport-cc\r\n",
    "a=fmtp:111 minptime=10;useinbandfec=1\r\n",
    "m=video 9 UDP/TLS/RTP/SAVPF 102 96\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "b=TIAS:4000000\r\n",
    "a=mid:1\r\n",
    "a=ice-ufrag:fmS1\r\n",
    "a=ice-pwd:freemixStudioPassword01\r\n",
    "a=fingerprint:sha-256 ",
    studio_fp!(),
    "\r\n",
    "a=setup:active\r\n",
    "a=sendrecv\r\n",
    "a=rtcp-mux\r\n",
    "a=rtpmap:102 H264/90000\r\n",
    "a=rtcp-fb:102 goog-remb\r\n",
    "a=rtcp-fb:102 transport-cc\r\n",
    "a=rtcp-fb:102 ccm fir\r\n",
    "a=rtcp-fb:102 nack\r\n",
    "a=rtcp-fb:102 nack pli\r\n",
    "a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n",
    "a=rtpmap:96 VP8/90000\r\n",
    "a=rtcp-fb:96 goog-remb\r\n",
    "a=rtcp-fb:96 transport-cc\r\n",
    "a=rtcp-fb:96 ccm fir\r\n",
    "a=rtcp-fb:96 nack\r\n",
    "a=rtcp-fb:96 nack pli\r\n",
);

/// The studio's exact initial offer (session 7, version 0).
const STUDIO_OFFER: &str = concat!(
    "v=0\r\n",
    "o=- 7 0 IN IP4 127.0.0.1\r\n",
    "s=-\r\n",
    "t=0 0\r\n",
    "a=group:BUNDLE 0 1\r\n",
    "a=ice-options:trickle\r\n",
    "m=audio 9 UDP/TLS/RTP/SAVPF 96\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "a=mid:0\r\n",
    "a=ice-ufrag:fmS1\r\n",
    "a=ice-pwd:freemixStudioPassword01\r\n",
    "a=fingerprint:sha-256 ",
    studio_fp!(),
    "\r\n",
    "a=setup:actpass\r\n",
    "a=sendrecv\r\n",
    "a=rtcp-mux\r\n",
    "a=rtpmap:96 opus/48000/2\r\n",
    "a=rtcp-fb:96 transport-cc\r\n",
    "a=fmtp:96 minptime=10;useinbandfec=1\r\n",
    "m=video 9 UDP/TLS/RTP/SAVPF 97 98\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "b=TIAS:4000000\r\n",
    "a=mid:1\r\n",
    "a=ice-ufrag:fmS1\r\n",
    "a=ice-pwd:freemixStudioPassword01\r\n",
    "a=fingerprint:sha-256 ",
    studio_fp!(),
    "\r\n",
    "a=setup:actpass\r\n",
    "a=sendrecv\r\n",
    "a=rtcp-mux\r\n",
    "a=rtpmap:97 H264/90000\r\n",
    "a=rtcp-fb:97 goog-remb\r\n",
    "a=rtcp-fb:97 transport-cc\r\n",
    "a=rtcp-fb:97 ccm fir\r\n",
    "a=rtcp-fb:97 nack\r\n",
    "a=rtcp-fb:97 nack pli\r\n",
    "a=fmtp:97 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f\r\n",
    "a=rtpmap:98 VP8/90000\r\n",
    "a=rtcp-fb:98 goog-remb\r\n",
    "a=rtcp-fb:98 transport-cc\r\n",
    "a=rtcp-fb:98 ccm fir\r\n",
    "a=rtcp-fb:98 nack\r\n",
    "a=rtcp-fb:98 nack pli\r\n",
);

/// Firefox-shaped single-section offer with session-level ICE/DTLS
/// parameters, lowercase fingerprint hex, and `b=AS`.
const SESSION_LEVEL_OFFER: &str = concat!(
    "v=0\r\n",
    "o=mozilla...THIS_IS_SDPARTA-99.0 6000000000000000000 0 IN IP4 0.0.0.0\r\n",
    "s=-\r\n",
    "t=0 0\r\n",
    "a=fingerprint:sha-256 a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5:a5\r\n",
    "a=ice-ufrag:ffx1\r\n",
    "a=ice-pwd:firefoxPassword0123456789\r\n",
    "a=msid-semantic:WMS *\r\n",
    "m=video 9 UDP/TLS/RTP/SAVPF 120\r\n",
    "c=IN IP4 0.0.0.0\r\n",
    "b=AS:2500\r\n",
    "a=recvonly\r\n",
    "a=mid:0\r\n",
    "a=setup:actpass\r\n",
    "a=rtcp-mux\r\n",
    "a=rtpmap:120 VP8/90000\r\n",
    "a=fmtp:120 max-fs=12288;max-fr=60\r\n",
);

fn studio_ice() -> IceCredentials {
    IceCredentials::new("fmS1", "freemixStudioPassword01").unwrap()
}

fn studio_endpoint() -> LocalEndpoint {
    LocalEndpoint {
        ice: studio_ice(),
        fingerprint: Fingerprint::new([0xA5; 32]),
        setup_when_actpass: SetupRole::Active,
        audio: vec![CodecPreference::opus()],
        video: vec![
            CodecPreference::h264_constrained_baseline(),
            CodecPreference::vp8(),
        ],
        audio_direction: SdpDirection::SendRecv,
        video_direction: SdpDirection::SendRecv,
        max_video_bitrate_bps: 4_000_000,
    }
}

fn guest_endpoint() -> LocalEndpoint {
    LocalEndpoint {
        ice: IceCredentials::new("gst4", "guestPassword0123456789").unwrap(),
        fingerprint: Fingerprint::parse(concat!("sha-256 ", guest_fp!())).unwrap(),
        setup_when_actpass: SetupRole::Active,
        audio: vec![CodecPreference::opus()],
        video: vec![
            CodecPreference::vp8(),
            CodecPreference::h264_constrained_baseline(),
        ],
        audio_direction: SdpDirection::SendRecv,
        video_direction: SdpDirection::SendRecv,
        max_video_bitrate_bps: 2_500_000,
    }
}

fn without(text: &str, prefix: &str) -> String {
    text.split_inclusive("\r\n")
        .filter(|line| !line.starts_with(prefix))
        .collect()
}

// ---------------------------------------------------------------------------
// SDP parsing and rendering

#[test]
fn browser_offer_fixture_parses_into_the_supported_subset() {
    let offer = Sdp::parse(BROWSER_OFFER).unwrap();
    assert_eq!(offer.origin.session_id, 4_611_731_400_430_051_336);
    assert_eq!(offer.origin.session_version, 2);
    assert_eq!(offer.bundle, ["0", "1"]);
    assert_eq!(offer.media.len(), 2);
    assert_eq!(offer.ice().ufrag(), "gst4");
    assert_eq!(
        offer.fingerprint().digest()[..8],
        [0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF]
    );

    let audio = &offer.media[0];
    assert_eq!(audio.kind, SdpMediaKind::Audio);
    assert_eq!(audio.setup, SetupRole::ActPass);
    assert_eq!(audio.direction, SdpDirection::SendRecv);
    assert_eq!(
        audio.codecs.len(),
        1,
        "RED/G.722/PCMU/PCMA/DTMF are dropped"
    );
    assert_eq!(audio.codecs[0].payload_type, 111);
    assert_eq!(audio.codecs[0].name, CodecName::Opus);
    assert_eq!(audio.codecs[0].fmtp_value("useinbandfec"), Some("1"));
    assert_eq!(audio.codecs[0].feedback, [RtcpFeedback::TransportCc]);

    let video = &offer.media[1];
    let video_codecs: Vec<(u8, CodecName)> = video
        .codecs
        .iter()
        .map(|codec| (codec.payload_type, codec.name))
        .collect();
    assert_eq!(video_codecs, [(96, CodecName::Vp8), (102, CodecName::H264)]);
    assert_eq!(video.codecs[1].h264_profile_idc(), Some(0x42));
    assert_eq!(video.codecs[1].h264_packetization_mode(), Some(1));
    assert_eq!(video.codecs[0].feedback.len(), 5);
    assert_eq!(video.max_bitrate_bps, None);

    assert_eq!(video.candidates.len(), 2);
    let srflx = &video.candidates[0];
    assert_eq!(srflx.kind, CandidateType::ServerReflexive);
    assert_eq!(srflx.related, Some(("0.0.0.0".to_owned(), 0)));
    assert_eq!(srflx.ufrag.as_deref(), Some("gst4"));
    assert_eq!(
        srflx.to_string(),
        "candidate:842163049 1 udp 1677729535 203.0.113.7 54321 typ srflx raddr 0.0.0.0 rport 0 ufrag gst4"
    );
    let host = &video.candidates[1];
    assert_eq!(host.kind, CandidateType::Host);
    assert_eq!(host.address, "4f8e1c2a-1b2c-4d3e-8f90-123456789abc.local");
    assert!(!video.end_of_candidates);
}

#[test]
fn answer_to_browser_offer_is_exact_and_round_trips() {
    let offer = Sdp::parse(BROWSER_OFFER).unwrap();
    let answer = Sdp::answer(&offer, &studio_endpoint(), 7, 0).unwrap();
    assert_eq!(answer.render(), STUDIO_ANSWER);
    let reparsed = Sdp::parse(STUDIO_ANSWER).unwrap();
    assert_eq!(reparsed, answer);
    reparsed.validate_answer_for(&offer).unwrap();
}

#[test]
fn local_offer_is_exact_and_round_trips() {
    let offer = Sdp::offer(&studio_endpoint(), 7, 0).unwrap();
    assert_eq!(offer.render(), STUDIO_OFFER);
    assert_eq!(Sdp::parse(STUDIO_OFFER).unwrap(), offer);
}

#[test]
fn session_level_transport_attributes_are_pushed_down_and_rendered_canonically() {
    let offer = Sdp::parse(SESSION_LEVEL_OFFER).unwrap();
    assert!(
        offer.bundle.is_empty(),
        "single sections need no BUNDLE group"
    );
    let video = &offer.media[0];
    assert_eq!(video.ice.ufrag(), "ffx1");
    assert_eq!(video.fingerprint, Fingerprint::new([0xA5; 32]));
    assert_eq!(video.direction, SdpDirection::RecvOnly);
    assert_eq!(video.max_bitrate_bps, Some(2_500_000));
    assert_eq!(video.codecs[0].fmtp_value("max-fs"), Some("12288"));

    // The guest only receives video, so the studio answer only sends.
    let answer = Sdp::answer(&offer, &studio_endpoint(), 9, 3).unwrap();
    assert_eq!(answer.media[0].direction, SdpDirection::SendOnly);
    assert_eq!(answer.media[0].codecs[0].payload_type, 120);
    assert_eq!(
        Sdp::parse(&answer.render()).unwrap().media[0].fingerprint,
        Fingerprint::new([0xA5; 32])
    );
}

#[test]
fn dtls_encryption_is_mandatory() {
    assert_eq!(
        Sdp::parse(&without(BROWSER_OFFER, "a=fingerprint:")),
        Err(SdpError::MissingFingerprint {
            mid: "0".to_owned()
        })
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replace("UDP/TLS/RTP/SAVPF", "RTP/AVP")),
        Err(SdpError::InsecureProfile("RTP/AVP".to_owned()))
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replace("a=fingerprint:sha-256", "a=fingerprint:sha-1")),
        Err(SdpError::UnsupportedFingerprintAlgorithm(
            "sha-1".to_owned()
        ))
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replace(":CD:EF\r\n", ":CD\r\n")),
        Err(SdpError::InvalidFingerprint)
    );
    assert_eq!(
        Fingerprint::parse("sha-256 0:12:34"),
        Err(SdpError::InvalidFingerprint)
    );
    assert_eq!(
        Sdp::parse(&without(BROWSER_OFFER, "a=setup:")),
        Err(SdpError::MissingSetup {
            mid: "0".to_owned()
        })
    );
    assert_eq!(
        Sdp::parse(&without(BROWSER_OFFER, "a=ice-pwd:")),
        Err(SdpError::MissingIceCredentials {
            mid: "0".to_owned()
        })
    );
}

#[test]
fn framing_and_size_are_strict() {
    assert_eq!(
        Sdp::parse(BROWSER_OFFER.trim_end()),
        Err(SdpError::MissingFinalCrlf)
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("s=-\r\n", "s=-\n", 1)),
        Err(SdpError::BareLineEnding { line: 3 })
    );
    let oversize = format!("{BROWSER_OFFER}{}", "a=ssrc:1 cname:x\r\n".repeat(1000));
    assert_eq!(
        Sdp::parse(&oversize),
        Err(SdpError::TooLarge {
            actual: oversize.len(),
            maximum: MAX_SDP_BYTES
        })
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("t=0 0\r\n", "", 1)),
        Err(SdpError::UnexpectedLine { line: 4, kind: 'a' })
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("s=-\r\n", "s=-\r\ni=info\r\n", 1)),
        Err(SdpError::UnsupportedLine { line: 4, kind: 'i' })
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("t=0 0", "t=1 2", 1)),
        Err(SdpError::InvalidTiming)
    );
}

#[test]
fn unsupported_features_are_rejected_explicitly() {
    let simulcast = BROWSER_OFFER.replacen(
        "a=rtcp-rsize\r\n",
        "a=rtcp-rsize\r\na=rid:h send\r\na=simulcast:send h\r\n",
        1,
    );
    assert_eq!(Sdp::parse(&simulcast), Err(SdpError::SimulcastUnsupported));
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("a=ssrc-group:FID", "a=ssrc-group:SIM", 1)),
        Err(SdpError::SimulcastUnsupported)
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("a=extmap-allow-mixed", "a=ice-lite", 1)),
        Err(SdpError::UnsupportedAttribute("ice-lite".to_owned()))
    );
    assert_eq!(
        Sdp::parse(&without(BROWSER_OFFER, "a=rtcp-mux\r\n")),
        Err(SdpError::RtcpMuxRequired {
            mid: "0".to_owned()
        })
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("a=group:BUNDLE 0 1", "a=group:BUNDLE 0", 1)),
        Err(SdpError::UnbundledMedia {
            mid: "1".to_owned()
        })
    );
    assert_eq!(
        Sdp::parse(&without(BROWSER_OFFER, "a=group:")),
        Err(SdpError::UnbundledMedia {
            mid: "0".to_owned()
        })
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("m=video 9 ", "m=video 0 ", 1)),
        Err(SdpError::RejectedMediaSection {
            mid: Some("1".to_owned())
        })
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("a=mid:1", "a=mid:0", 1)),
        Err(SdpError::DuplicateMid("0".to_owned()))
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen(
            "a=ice-ufrag:gst4\r\na=ice-pwd:guestPassword0123456789\r\na=ice-options:trickle\r\na=fingerprint:sha-256 01",
            "a=ice-ufrag:gst5\r\na=ice-pwd:guestPassword0123456789\r\na=ice-options:trickle\r\na=fingerprint:sha-256 01",
            1
        )),
        Err(SdpError::InconsistentBundleTransport)
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("a=rtcp-fb:96 nack pli", "a=rtcp-fb:96 ack rpsi", 1)),
        Err(SdpError::UnsupportedRtcpFeedback("ack rpsi".to_owned()))
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("packetization-mode=1", "packetization-mode=2", 1)),
        Err(SdpError::InvalidFmtp)
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen("a=rtpmap:96 VP8/90000", "a=rtpmap:96 opus/48000/2", 1)),
        Err(SdpError::CodecKindMismatch { payload_type: 96 })
    );
    assert_eq!(
        Sdp::parse(&BROWSER_OFFER.replacen(
            "a=rtpmap:126 telephone-event",
            "a=rtpmap:125 telephone-event",
            1
        )),
        Err(SdpError::UnlistedPayloadType(125))
    );
    let video_only_vp9 = BROWSER_OFFER
        .replacen("a=rtpmap:96 VP8/90000", "a=rtpmap:96 AV1/90000", 1)
        .replacen("a=rtpmap:102 H264/90000", "a=rtpmap:102 VP9/90000", 1)
        .replacen(
            "a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
            "a=fmtp:102 profile-id=2",
            1,
        );
    assert_eq!(
        Sdp::parse(&video_only_vp9),
        Err(SdpError::NoSupportedCodec {
            mid: "1".to_owned()
        })
    );
}

#[test]
fn candidates_are_bounded_and_strict() {
    let candidate =
        "candidate:7 1 udp 41885439 198.51.100.4 3478 typ relay raddr 203.0.113.7 rport 54321";
    let parsed = SdpCandidate::parse(candidate).unwrap();
    assert_eq!(parsed.kind, CandidateType::Relay);
    assert_eq!(parsed.to_string(), candidate);

    assert_eq!(
        SdpCandidate::parse("candidate:7 1 tcp 1 198.51.100.4 9 typ host tcptype active"),
        Err(SdpError::UnsupportedCandidateTransport("tcp".to_owned()))
    );
    assert_eq!(
        SdpCandidate::parse("candidate:7 2 udp 1 198.51.100.4 9 typ host"),
        Err(SdpError::UnsupportedCandidateComponent)
    );
    assert_eq!(
        SdpCandidate::parse("candidate:7 1 udp 1 198.51.100.4 9 typ prflx"),
        Err(SdpError::UnsupportedCandidateType("prflx".to_owned()))
    );
    assert_eq!(
        SdpCandidate::parse("candidate:7 1 udp 1 198.51.100.4 9 typ host label x"),
        Err(SdpError::UnsupportedCandidateExtension("label".to_owned()))
    );
    for invalid in [
        "7 1 udp 1 198.51.100.4 9 typ host",
        "candidate:7 1 udp 0 198.51.100.4 9 typ host",
        "candidate:7 1 udp 1 example.com 9 typ host",
        "candidate:7 1 udp 1 198.51.100.4 0 typ host",
        "candidate:7 1 udp 1 198.51.100.4 9 typ host raddr 0.0.0.0 rport 0",
        "candidate:7 1 udp 1 198.51.100.4 9 typ srflx raddr 0.0.0.0",
        "candidate:7 1 udp 1 198.51.100.4  9 typ host",
    ] {
        assert_eq!(
            SdpCandidate::parse(invalid),
            Err(SdpError::InvalidCandidate),
            "{invalid}"
        );
    }

    let line = "a=candidate:1 1 udp 2122260223 192.0.2.1 50000 typ host\r\n";
    let crowded = BROWSER_OFFER.replacen(
        "a=ssrc:1001 cname:guest\r\n",
        &format!("a=ssrc:1001 cname:guest\r\n{}", line.repeat(17)),
        1,
    );
    assert_eq!(
        Sdp::parse(&crowded),
        Err(SdpError::TooManyCandidates { maximum: 16 })
    );
}

#[test]
fn answers_negotiate_codecs_roles_and_directions_deterministically() {
    // H.264 High profile is not compatible with constrained baseline.
    let high_only = BROWSER_OFFER.replacen("profile-level-id=42e01f", "profile-level-id=640c1f", 1);
    let offer = Sdp::parse(&high_only).unwrap();
    let mut local = studio_endpoint();
    let answer = Sdp::answer(&offer, &local, 1, 0).unwrap();
    let names: Vec<CodecName> = answer.media[1]
        .codecs
        .iter()
        .map(|codec| codec.name)
        .collect();
    assert_eq!(names, [CodecName::Vp8]);

    local.video = vec![CodecPreference::h264_constrained_baseline()];
    assert_eq!(
        Sdp::answer(&offer, &local, 1, 0),
        Err(SdpError::NoCommonCodec {
            mid: "1".to_owned()
        })
    );

    // An `active` offerer gets a `passive` answer; a sendonly guest gets recvonly.
    let active = BROWSER_OFFER
        .replace("a=setup:actpass", "a=setup:active")
        .replace("a=sendrecv", "a=sendonly");
    let offer = Sdp::parse(&active).unwrap();
    let answer = Sdp::answer(&offer, &studio_endpoint(), 1, 0).unwrap();
    assert!(
        answer
            .media
            .iter()
            .all(|section| section.setup == SetupRole::Passive)
    );
    assert!(
        answer
            .media
            .iter()
            .all(|section| section.direction == SdpDirection::RecvOnly)
    );

    let mut capped = studio_endpoint();
    capped.max_video_bitrate_bps = 5_000_000;
    assert_eq!(
        Sdp::answer(&offer, &capped, 1, 0),
        Err(SdpError::BitrateCapExceeded {
            requested: 5_000_000,
            maximum: 4_000_000
        })
    );
    let mut actpass = studio_endpoint();
    actpass.setup_when_actpass = SetupRole::ActPass;
    assert_eq!(actpass.validate(), Err(SdpError::InvalidAnswerSetup));
    let mut misplaced = studio_endpoint();
    misplaced.audio.push(CodecPreference::vp8());
    assert_eq!(
        misplaced.validate(),
        Err(SdpError::PreferenceKindMismatch(CodecName::Vp8))
    );
}

#[test]
fn received_answers_are_checked_against_the_offer() {
    let offer = Sdp::parse(BROWSER_OFFER).unwrap();
    assert_eq!(
        Sdp::parse(&STUDIO_ANSWER.replace("a=setup:active", "a=setup:actpass"))
            .unwrap()
            .validate_answer_for(&offer),
        Err(SdpError::InvalidAnswerSetup)
    );
    let unknown_pt = STUDIO_ANSWER
        .replace("SAVPF 102 96", "SAVPF 104 96")
        .replace(":102 ", ":104 ");
    assert_eq!(
        Sdp::parse(&unknown_pt).unwrap().validate_answer_for(&offer),
        Err(SdpError::AnswerMismatch("codec was not offered"))
    );
    let recvonly_offer = Sdp::parse(&BROWSER_OFFER.replace("a=sendrecv", "a=recvonly")).unwrap();
    assert_eq!(
        Sdp::parse(STUDIO_ANSWER)
            .unwrap()
            .validate_answer_for(&recvonly_offer),
        Err(SdpError::AnswerMismatch("direction exceeds the offer"))
    );
}

#[test]
fn ice_credentials_are_validated_and_redacted() {
    assert_eq!(
        IceCredentials::new("abc", "freemixStudioPassword01"),
        Err(SdpError::InvalidIceUfrag)
    );
    assert_eq!(
        IceCredentials::new("abcd", "short"),
        Err(SdpError::InvalidIcePwd)
    );
    let debug = format!("{:?}", studio_ice());
    assert!(debug.contains("fmS1"));
    assert!(!debug.contains("freemixStudioPassword01"));
}

// ---------------------------------------------------------------------------
// Signaling

fn studio_session() -> SignalingSession {
    SignalingSession::new(SignalingConfig::new(
        NegotiationRole::Impolite,
        studio_endpoint(),
        7,
    ))
    .unwrap()
}

fn guest_session() -> SignalingSession {
    SignalingSession::new(SignalingConfig::new(
        NegotiationRole::Polite,
        guest_endpoint(),
        11,
    ))
    .unwrap()
}

fn connected_pair() -> (SignalingSession, SignalingSession) {
    let mut studio = studio_session();
    let mut guest = guest_session();
    let offer = studio.create_offer().unwrap();
    assert_eq!(guest.receive_offer(&offer).unwrap(), OfferOutcome::Accepted);
    let answer = guest.create_answer().unwrap();
    studio.receive_answer(&answer).unwrap();
    studio.ice_connected().unwrap();
    guest.ice_connected().unwrap();
    (studio, guest)
}

#[test]
fn offer_answer_reaches_connected() {
    let mut studio = studio_session();
    let mut guest = guest_session();
    assert_eq!(studio.phase(), SignalingPhase::Idle);
    let offer = studio.create_offer().unwrap();
    assert_eq!(offer, STUDIO_OFFER);
    assert_eq!(studio.phase(), SignalingPhase::HaveLocalOffer);

    assert_eq!(guest.receive_offer(&offer).unwrap(), OfferOutcome::Accepted);
    assert_eq!(guest.phase(), SignalingPhase::HaveRemoteOffer);
    let answer = guest.create_answer().unwrap();
    assert_eq!(guest.phase(), SignalingPhase::Connecting);
    // The guest prefers VP8 and keeps the studio's payload types.
    let parsed = Sdp::parse(&answer).unwrap();
    assert_eq!(parsed.media[1].codecs[0].payload_type, 98);
    assert_eq!(parsed.media[1].codecs[0].name, CodecName::Vp8);
    assert_eq!(parsed.media[1].max_bitrate_bps, Some(2_500_000));

    studio.receive_answer(&answer).unwrap();
    assert_eq!(studio.phase(), SignalingPhase::Connecting);
    assert_eq!(studio.remote_description().unwrap().ice().ufrag(), "gst4");
    studio.ice_connected().unwrap();
    assert_eq!(studio.phase(), SignalingPhase::Connected);

    assert_eq!(
        studio.receive_answer(&answer),
        Err(SignalingError::InvalidTransition {
            operation: "receive an answer",
            phase: SignalingPhase::Connected
        })
    );
    assert_eq!(
        guest.create_answer(),
        Err(SignalingError::InvalidTransition {
            operation: "create an answer",
            phase: SignalingPhase::Connecting
        })
    );
}

#[test]
fn malformed_answers_leave_the_offer_pending() {
    let mut studio = studio_session();
    studio.create_offer().unwrap();
    assert_eq!(
        studio.receive_answer(&STUDIO_ANSWER.replace("a=setup:active", "a=setup:actpass")),
        Err(SignalingError::Sdp(SdpError::InvalidAnswerSetup))
    );
    assert_eq!(studio.phase(), SignalingPhase::HaveLocalOffer);
    assert!(studio.pending_local_offer().is_some());
    studio.rollback().unwrap();
    assert_eq!(studio.phase(), SignalingPhase::Idle);
}

#[test]
fn glare_is_resolved_by_politeness() {
    let mut studio = studio_session();
    let mut guest = guest_session();
    let studio_offer = studio.create_offer().unwrap();
    let guest_offer = guest.create_offer().unwrap();

    assert_eq!(
        studio.receive_offer(&guest_offer).unwrap(),
        OfferOutcome::IgnoredGlare
    );
    assert_eq!(studio.phase(), SignalingPhase::HaveLocalOffer);
    assert_eq!(
        guest.receive_offer(&studio_offer).unwrap(),
        OfferOutcome::AcceptedAfterRollback
    );
    assert!(guest.pending_local_offer().is_none());
    let answer = guest.create_answer().unwrap();
    studio.receive_answer(&answer).unwrap();
    assert_eq!(studio.phase(), SignalingPhase::Connecting);
    assert_eq!(guest.phase(), SignalingPhase::Connecting);
}

#[test]
fn trickle_is_bounded_buffered_and_generation_checked() {
    let mut config = SignalingConfig::new(NegotiationRole::Impolite, studio_endpoint(), 7);
    config.max_remote_candidates = 2;
    config.max_buffered_candidates = 2;
    let mut studio = SignalingSession::new(config).unwrap();
    let mut guest = guest_session();
    let host = "candidate:1 1 udp 2122260223 192.0.2.10 50000 typ host ufrag gst4";
    let srflx = "candidate:2 1 udp 1686052607 203.0.113.7 54321 typ srflx raddr 192.0.2.10 rport 50000 ufrag gst4";
    let relay = "candidate:3 1 udp 41885439 198.51.100.4 3478 typ relay raddr 203.0.113.7 rport 54321 ufrag gst4";

    let offer = studio.create_offer().unwrap();
    // Candidates racing ahead of the answer are buffered (bounded).
    assert_eq!(
        studio.add_remote_candidate("0", host),
        Ok(CandidateOutcome::Buffered)
    );
    assert_eq!(
        studio.add_remote_candidate("0", host),
        Err(SignalingError::DuplicateCandidate)
    );
    assert_eq!(
        studio.add_remote_candidate("9", srflx),
        Ok(CandidateOutcome::Buffered)
    );
    assert_eq!(
        studio.add_remote_candidate("0", relay),
        Err(SignalingError::TooManyBufferedCandidates { maximum: 2 })
    );

    guest.receive_offer(&offer).unwrap();
    studio
        .receive_answer(&guest.create_answer().unwrap())
        .unwrap();
    // The buffered host candidate is applied; the unknown-mid one is dropped.
    assert_eq!(studio.remote_candidates().len(), 1);
    assert_eq!(studio.dropped_buffered_candidates(), 1);
    assert_eq!(studio.buffered_candidates(), &[]);

    assert_eq!(
        studio.add_remote_candidate("2", srflx),
        Err(SignalingError::UnknownMid("2".to_owned()))
    );
    assert_eq!(
        studio.add_remote_candidate("1", "candidate:9 1 udp 1 192.0.2.99 9 typ host ufrag old1"),
        Err(SignalingError::StaleCandidate)
    );
    assert_eq!(
        studio.add_remote_candidate("0", srflx),
        Ok(CandidateOutcome::Applied)
    );
    assert_eq!(
        studio.add_remote_candidate("0", relay),
        Err(SignalingError::TooManyCandidates { maximum: 2 })
    );
    assert_eq!(
        studio.add_remote_candidate("1", host),
        Err(SignalingError::DuplicateCandidate),
        "bundled sections share one transport"
    );
    assert_eq!(
        studio.add_remote_candidate("0", "candidate:1 1 tcp 1 192.0.2.10 9 typ host"),
        Err(SignalingError::Sdp(
            SdpError::UnsupportedCandidateTransport("tcp".to_owned())
        ))
    );
    studio.remote_end_of_candidates_received().unwrap();
    assert!(studio.remote_end_of_candidates());
}

#[test]
fn end_of_candidates_closes_the_generation() {
    let (mut studio, _guest) = connected_pair();
    studio.remote_end_of_candidates_received().unwrap();
    assert_eq!(
        studio.add_remote_candidate(
            "0",
            "candidate:1 1 udp 2122260223 192.0.2.10 50000 typ host"
        ),
        Err(SignalingError::CandidateAfterEndOfCandidates)
    );
}

#[test]
fn ice_restart_reconnects_with_fresh_credentials() {
    let (mut studio, mut guest) = connected_pair();
    studio
        .add_remote_candidate(
            "0",
            "candidate:1 1 udp 2122260223 192.0.2.10 50000 typ host ufrag gst4",
        )
        .unwrap();
    studio.ice_disconnected().unwrap();
    assert_eq!(studio.phase(), SignalingPhase::Disconnected);

    assert_eq!(
        studio.restart_ice(studio_ice()),
        Err(SignalingError::IceCredentialsReused)
    );
    let fresh = IceCredentials::new("fmS2", "freemixStudioPassword02").unwrap();
    let restart_offer = studio.restart_ice(fresh).unwrap();
    assert_eq!(studio.ice_generation(), 1);
    assert!(restart_offer.contains("a=ice-ufrag:fmS2\r\n"));
    assert!(restart_offer.starts_with("v=0\r\no=- 7 1 IN IP4 127.0.0.1\r\n"));

    // The guest's next-generation candidates may race ahead of its answer.
    let next = "candidate:5 1 udp 2122260223 192.0.2.10 50002 typ host ufrag gst5";
    assert_eq!(
        studio.add_remote_candidate("0", next),
        Ok(CandidateOutcome::Buffered)
    );

    assert_eq!(
        guest.receive_offer(&restart_offer).unwrap(),
        OfferOutcome::Accepted
    );
    assert!(guest.remote_ice_restart_pending());
    assert_eq!(
        guest.create_answer(),
        Err(SignalingError::IceRestartCredentialsRequired)
    );
    let guest_fresh = IceCredentials::new("gst5", "guestPassword9876543210").unwrap();
    let answer = guest.create_restart_answer(guest_fresh).unwrap();
    assert_eq!(guest.phase(), SignalingPhase::Connecting);
    assert_eq!(guest.ice_generation(), 1);

    studio.receive_answer(&answer).unwrap();
    assert_eq!(studio.phase(), SignalingPhase::Connecting);
    let applied: Vec<&str> = studio
        .remote_candidates()
        .iter()
        .map(|entry| entry.candidate.foundation.as_str())
        .collect();
    assert_eq!(
        applied,
        ["5"],
        "previous-generation candidates are discarded"
    );
    assert_eq!(
        studio.add_remote_candidate(
            "0",
            "candidate:1 1 udp 2122260223 192.0.2.10 50000 typ host ufrag gst4"
        ),
        Err(SignalingError::StaleCandidate)
    );
    studio.ice_connected().unwrap();
    assert_eq!(studio.phase(), SignalingPhase::Connected);
}

#[test]
fn ice_restarts_are_bounded() {
    let mut config = SignalingConfig::new(NegotiationRole::Impolite, studio_endpoint(), 7);
    config.max_ice_restarts = 1;
    let mut studio = SignalingSession::new(config).unwrap();
    let mut guest = guest_session();
    let offer = studio.create_offer().unwrap();
    guest.receive_offer(&offer).unwrap();
    studio
        .receive_answer(&guest.create_answer().unwrap())
        .unwrap();
    studio
        .restart_ice(IceCredentials::new("fmS2", "freemixStudioPassword02").unwrap())
        .unwrap();
    studio.rollback().unwrap();
    assert_eq!(studio.phase(), SignalingPhase::Connecting);
    assert_eq!(
        studio.restart_ice(IceCredentials::new("fmS3", "freemixStudioPassword03").unwrap()),
        Err(SignalingError::IceRestartLimit { maximum: 1 })
    );
}

#[test]
fn renegotiation_without_ice_change_stays_connected() {
    let (mut studio, mut guest) = connected_pair();
    let offer = studio.create_offer().unwrap();
    assert!(offer.starts_with("v=0\r\no=- 7 1 IN IP4 127.0.0.1\r\n"));
    guest.receive_offer(&offer).unwrap();
    assert!(!guest.remote_ice_restart_pending());
    let answer = guest.create_answer().unwrap();
    assert_eq!(guest.phase(), SignalingPhase::Connected);
    studio.receive_answer(&answer).unwrap();
    assert_eq!(studio.phase(), SignalingPhase::Connected);

    studio.close();
    assert_eq!(
        studio.create_offer(),
        Err(SignalingError::InvalidTransition {
            operation: "create an offer",
            phase: SignalingPhase::Closed
        })
    );
    assert_eq!(
        studio.ice_connected(),
        Err(SignalingError::InvalidTransition {
            operation: "mark connected",
            phase: SignalingPhase::Closed
        })
    );
}

// ---------------------------------------------------------------------------
// Bandwidth adaptation

fn estimate(
    at_ms: u64,
    source: EstimateSource,
    bitrate_bps: u64,
    loss_permille: u16,
) -> BandwidthEstimate {
    BandwidthEstimate {
        at_ms,
        source,
        bitrate_bps,
        loss_permille,
    }
}

fn controller() -> BandwidthController {
    BandwidthController::new(BitrateLadder::standard(), ControllerConfig::default()).unwrap()
}

#[test]
fn ladder_ramps_up_with_hysteresis_and_caps_at_1080p_4mbps() {
    let mut controller = controller();
    let mut changes = Vec::new();
    let mut last = None;
    for second in 0..=30 {
        let at = second * 1_000;
        let decision = controller
            .on_estimate(estimate(at, EstimateSource::Twcc, 20_000_000, 0))
            .unwrap();
        if let Some(change) = decision.change {
            changes.push((at, change));
        }
        last = Some(decision);
    }
    let sustained = |from, to| RungChange {
        from,
        to,
        reason: ChangeReason::SustainedHeadroom,
    };
    assert_eq!(
        changes,
        [
            (5_000, sustained(0, 1)),
            (11_000, sustained(1, 2)),
            (17_000, sustained(2, 3)),
            (23_000, sustained(3, 4)),
        ]
    );
    let last = last.unwrap();
    assert_eq!((last.width, last.height, last.frame_rate), (1920, 1080, 30));
    assert_eq!(last.target_bitrate_bps, 4_000_000);
    assert_eq!(last.usable_bitrate_bps, 18_000_000);
}

#[test]
fn downgrades_are_immediate_and_upgrades_need_margin() {
    let mut controller = controller();
    for second in 0..=23 {
        controller
            .on_estimate(estimate(second * 1_000, EstimateSource::Remb, 5_000_000, 0))
            .unwrap();
    }
    assert_eq!(controller.current_rung(), 4);

    let drop = controller
        .on_estimate(estimate(24_000, EstimateSource::Remb, 2_000_000, 0))
        .unwrap();
    assert_eq!(
        drop.change,
        Some(RungChange {
            from: 4,
            to: 3,
            reason: ChangeReason::InsufficientBandwidth
        })
    );
    assert_eq!(drop.target_bitrate_bps, 1_800_000);

    // 2.9 Mbps keeps 720p (usable 2.61 Mbps) but never clears 2.5 Mbps × 1.15.
    for second in 25..=40 {
        let decision = controller
            .on_estimate(estimate(second * 1_000, EstimateSource::Remb, 2_900_000, 0))
            .unwrap();
        assert_eq!(decision.rung, 3);
    }
    // A dip during the hold restarts it.
    for (at, bps) in [
        (41_000, 3_300_000),
        (44_000, 3_300_000),
        (45_000, 3_000_000),
        (49_000, 3_300_000),
    ] {
        assert_eq!(
            controller
                .on_estimate(estimate(at, EstimateSource::Remb, bps, 0))
                .unwrap()
                .rung,
            3
        );
    }
    let up = controller
        .on_estimate(estimate(54_000, EstimateSource::Remb, 3_300_000, 0))
        .unwrap();
    assert_eq!(up.rung, 4);

    let collapse = controller
        .on_estimate(estimate(55_000, EstimateSource::Remb, 100_000, 0))
        .unwrap();
    assert_eq!(collapse.rung, 0);
    assert_eq!(
        collapse.target_bitrate_bps, 150_000,
        "floored at the lowest rung"
    );
}

#[test]
fn loss_steps_down_once_per_hold_and_blocks_upgrades() {
    let mut controller = controller();
    for second in 0..=17 {
        controller
            .on_estimate(estimate(
                second * 1_000,
                EstimateSource::Twcc,
                20_000_000,
                0,
            ))
            .unwrap();
    }
    assert_eq!(controller.current_rung(), 3);
    let lossy = controller
        .on_estimate(estimate(18_000, EstimateSource::Twcc, 20_000_000, 150))
        .unwrap();
    assert_eq!(
        lossy.change,
        Some(RungChange {
            from: 3,
            to: 2,
            reason: ChangeReason::HighLoss
        })
    );
    for second in 19..=22 {
        let decision = controller
            .on_estimate(estimate(
                second * 1_000,
                EstimateSource::Twcc,
                20_000_000,
                150,
            ))
            .unwrap();
        assert_eq!(decision.rung, 2);
        assert_eq!(decision.change, None);
    }
    assert_eq!(
        controller
            .on_estimate(estimate(23_000, EstimateSource::Twcc, 20_000_000, 150))
            .unwrap()
            .rung,
        1
    );
    // Moderate loss (2–10%) holds the rung indefinitely.
    for second in 24..=40 {
        assert_eq!(
            controller
                .on_estimate(estimate(
                    second * 1_000,
                    EstimateSource::Twcc,
                    20_000_000,
                    50
                ))
                .unwrap()
                .rung,
            1
        );
    }
}

#[test]
fn remb_and_twcc_combine_by_minimum_until_stale() {
    let mut controller = controller();
    controller
        .on_estimate(estimate(0, EstimateSource::Twcc, 1_000_000, 0))
        .unwrap();
    let both = controller
        .on_estimate(estimate(100, EstimateSource::Remb, 3_000_000, 0))
        .unwrap();
    assert_eq!(both.usable_bitrate_bps, 900_000);
    let stale = controller
        .on_estimate(estimate(3_100, EstimateSource::Remb, 3_000_000, 0))
        .unwrap();
    assert_eq!(stale.usable_bitrate_bps, 2_700_000);
}

#[test]
fn operator_cap_and_input_validation() {
    let mut controller = controller();
    for second in 0..=23 {
        controller
            .on_estimate(estimate(
                second * 1_000,
                EstimateSource::Twcc,
                20_000_000,
                0,
            ))
            .unwrap();
    }
    controller.set_max_rung(Some(2)).unwrap();
    let capped = controller
        .on_estimate(estimate(24_000, EstimateSource::Twcc, 20_000_000, 0))
        .unwrap();
    assert_eq!(
        capped.change,
        Some(RungChange {
            from: 4,
            to: 2,
            reason: ChangeReason::OperatorCap
        })
    );
    assert_eq!(
        controller.set_max_rung(Some(5)),
        Err(BandwidthError::RungOutOfRange { index: 5, rungs: 5 })
    );
    assert_eq!(
        controller.on_estimate(estimate(23_000, EstimateSource::Twcc, 1, 0)),
        Err(BandwidthError::NonMonotonicTime {
            previous_ms: 24_000,
            actual_ms: 23_000
        })
    );
    assert_eq!(
        controller.on_estimate(estimate(25_000, EstimateSource::Twcc, 1, 1001)),
        Err(BandwidthError::InvalidEstimate("loss above 1000 permille"))
    );
    assert_eq!(
        controller.on_estimate(estimate(25_000, EstimateSource::Twcc, 10_000_000_001, 0)),
        Err(BandwidthError::InvalidEstimate("bitrate above 10 Gbps"))
    );
}

#[test]
fn ladders_are_validated() {
    assert_eq!(
        BitrateLadder::new(Vec::new()),
        Err(BandwidthError::EmptyLadder)
    );
    assert_eq!(
        BitrateLadder::new(vec![LadderRung::new(2560, 1440, 30, 1_000_000, 4_000_000)]),
        Err(BandwidthError::InvalidRung {
            index: 0,
            reason: "exceeds 1920x1080"
        })
    );
    assert_eq!(
        BitrateLadder::new(vec![LadderRung::new(1920, 1080, 30, 1_000_000, 6_000_000)]),
        Err(BandwidthError::InvalidRung {
            index: 0,
            reason: "exceeds 4 Mbps"
        })
    );
    assert_eq!(
        BitrateLadder::new(vec![
            LadderRung::new(1280, 720, 30, 1_500_000, 2_500_000),
            LadderRung::new(640, 360, 30, 1_600_000, 2_500_000),
        ]),
        Err(BandwidthError::LadderNotAscending { index: 1 })
    );
    assert_eq!(
        BitrateLadder::new(vec![LadderRung::new(320, 180, 15, 150_000, 300_000); 9]),
        Err(BandwidthError::TooManyRungs { maximum: 8 })
    );
    let config = ControllerConfig {
        moderate_loss_permille: 100,
        ..ControllerConfig::default()
    };
    assert!(matches!(
        BandwidthController::new(BitrateLadder::standard(), config),
        Err(BandwidthError::InvalidConfig(_))
    ));
}
