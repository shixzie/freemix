//! AU-009: high-rate coalesced control input and paced, echo-free fader and
//! activator feedback. Every test injects its own timestamps and ticks.

use fm_automation::{
    ActionValue, ActivatorRule, BindingId, CoalesceError, CoalesceOutcome, CoalescePolicy,
    CoalescedEvent, ControlBinding, ControlCoalescer, ControlEvent, ControlId, ControlMap,
    ControlValue, DeviceKey, DeviceProfile, DeviceTransport, FeedbackBinding, FeedbackError,
    FeedbackPayload, FeedbackRouter, FeedbackSchedule, FeedbackSource, FeedbackTarget,
    MAX_COALESCER_CAPACITY, MidiChannel, MidiControlDecoder, MidiMessage, OscArg, OscMessage,
    OscPacket, Pickup, Response, Span, Suppressed, TallySnapshot, osc_control_events,
};
use fm_types::{InputId, MediaDuration, MediaTimestamp};
use std::num::{NonZeroU128, NonZeroUsize};

fn ch(index: u8) -> MidiChannel {
    MidiChannel::new(index).unwrap()
}

fn ts(ticks: i64) -> MediaTimestamp {
    MediaTimestamp::new(ticks)
}

fn key(text: &str) -> DeviceKey {
    DeviceKey::new(text).unwrap()
}

fn id(text: &str) -> BindingId {
    BindingId::new(text).unwrap()
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn cc(controller: u8, raw: u16) -> ControlEvent {
    ControlEvent {
        control: ControlId::MidiCc {
            channel: ch(0),
            controller,
        },
        value: ControlValue::Absolute { raw, max: 127 },
    }
}

fn note(number: u8, pressed: bool) -> ControlEvent {
    ControlEvent {
        control: ControlId::MidiNote {
            channel: ch(0),
            note: number,
        },
        value: ControlValue::Button {
            pressed,
            velocity: if pressed { 127 } else { 0 },
        },
    }
}

fn summary(events: &[CoalescedEvent]) -> Vec<(ControlId, ControlValue, u32)> {
    events
        .iter()
        .map(|event| (event.event.control.clone(), event.event.value, event.merged))
        .collect()
}

// ------------------------------------------------------- inbound coalescing

#[test]
fn seven_bit_fader_burst_coalesces_to_latest_value_per_tick() {
    let mut coalescer = ControlCoalescer::new(64).unwrap();
    // A full-throw fader move: 128 CC messages inside one 10 ms tick.
    for raw in 0..=127_u16 {
        let outcome = coalescer
            .offer("APC", cc(7, raw), ts(i64::from(raw)))
            .unwrap();
        let expected = if raw == 0 {
            CoalesceOutcome::Queued
        } else {
            CoalesceOutcome::Merged
        };
        assert_eq!(outcome, expected);
    }
    assert_eq!(coalescer.pending(), 1);
    let tick = coalescer.drain();
    assert_eq!(tick.len(), 1);
    assert_eq!(tick[0].port, "APC");
    assert_eq!(tick[0].event, cc(7, 127));
    assert_eq!((tick[0].first_at, tick[0].last_at), (ts(0), ts(127)));
    assert_eq!(tick[0].merged, 127);
    // Nothing carries into the next tick.
    assert_eq!(coalescer.drain().len(), 0);
    let telemetry = coalescer.telemetry();
    assert_eq!(
        (
            telemetry.offered,
            telemetry.merged,
            telemetry.drained,
            telemetry.ticks
        ),
        (128, 127, 1, 2)
    );
}

#[test]
fn fourteen_bit_pairs_never_surface_the_msb_only_intermediate_within_a_tick() {
    let mut decoder = MidiControlDecoder::default();
    decoder.enable_14bit(ch(0), 7).unwrap();
    let mut coalescer = ControlCoalescer::new(8).unwrap();
    // Three MSB/LSB pairs moving 0x1000 -> 0x1F7F. Each MSB alone decodes to
    // `msb << 7` (MIDI resets the LSB), a transient glitch.
    let mut raw_values = Vec::new();
    for (msb, lsb) in [(0x20, 0x00), (0x30, 0x40), (0x3E, 0x7F)] {
        for message in [
            MidiMessage::ControlChange {
                channel: ch(0),
                controller: 7,
                value: msb,
            },
            MidiMessage::ControlChange {
                channel: ch(0),
                controller: 39,
                value: lsb,
            },
        ] {
            let event = decoder.decode(&message).unwrap();
            raw_values.push(event.value);
            coalescer.offer("X-Touch", event, ts(1)).unwrap();
        }
    }
    // The raw decoded stream contains the MSB-only value `0x3E << 7` just
    // before the final combined value; the tick only surfaces the latter.
    assert!(raw_values.contains(&ControlValue::Absolute {
        raw: 0x3E << 7,
        max: 16_383
    }));
    let tick = coalescer.drain();
    assert_eq!(
        summary(&tick),
        vec![(
            ControlId::MidiCc14 {
                channel: ch(0),
                controller: 7
            },
            ControlValue::Absolute {
                raw: (0x3E << 7) | 0x7F,
                max: 16_383
            },
            5
        )]
    );
}

#[test]
fn osc_float_stream_coalesces_per_address() {
    let mut coalescer = ControlCoalescer::new(8).unwrap();
    for step in 0..=100_u8 {
        let level = f32::from(step) / 100.0;
        let packet = OscPacket::Message(OscMessage::new("/ch/1/fader", vec![OscArg::Float(level)]));
        for event in osc_control_events(&packet).unwrap() {
            coalescer
                .offer("touchosc", event, ts(i64::from(step)))
                .unwrap();
        }
        let other = OscPacket::Message(OscMessage::new(
            "/ch/2/fader",
            vec![OscArg::Float(1.0 - level)],
        ));
        for event in osc_control_events(&other).unwrap() {
            coalescer
                .offer("touchosc", event, ts(i64::from(step)))
                .unwrap();
        }
    }
    let tick = coalescer.drain();
    assert_eq!(
        summary(&tick),
        vec![
            (
                ControlId::Osc("/ch/1/fader".into()),
                ControlValue::Float(1.0),
                100
            ),
            (
                ControlId::Osc("/ch/2/fader".into()),
                ControlValue::Float(0.0),
                100
            ),
        ]
    );
    assert_eq!(
        coalescer.offer(
            "touchosc",
            ControlEvent {
                control: ControlId::Osc("/ch/1/fader".into()),
                value: ControlValue::Float(f32::NAN),
            },
            ts(200),
        ),
        Err(CoalesceError::NonFiniteValue)
    );
}

#[test]
fn button_edges_stay_ordered_while_faders_merge_in_place() {
    let mut coalescer = ControlCoalescer::new(16).unwrap();
    coalescer.offer("APC", cc(7, 10), ts(1)).unwrap();
    coalescer.offer("APC", note(40, true), ts(2)).unwrap();
    coalescer.offer("APC", cc(7, 20), ts(3)).unwrap();
    coalescer.offer("APC", note(40, false), ts(4)).unwrap();
    coalescer.offer("APC", note(40, true), ts(5)).unwrap();
    coalescer.offer("APC", cc(7, 30), ts(6)).unwrap();
    coalescer.offer("APC", note(40, false), ts(7)).unwrap();
    let tick = coalescer.drain();
    // The fader holds its first-arrival position with its newest value; all
    // four button edges survive in order.
    assert_eq!(
        tick.iter()
            .map(|event| (event.event.clone(), event.first_at, event.last_at))
            .collect::<Vec<_>>(),
        vec![
            (cc(7, 30), ts(1), ts(6)),
            (note(40, true), ts(2), ts(2)),
            (note(40, false), ts(4), ts(4)),
            (note(40, true), ts(5), ts(5)),
            (note(40, false), ts(7), ts(7)),
        ]
    );
}

#[test]
fn deltas_sum_and_ordered_overrides_keep_relative_encoder_steps() {
    let mut coalescer = ControlCoalescer::new(16).unwrap();
    let jog = ControlId::Hid {
        usage_page: 0x01,
        usage: 0x37,
    };
    for delta in [3, -1, 4, i32::MAX] {
        coalescer
            .offer(
                "shuttle",
                ControlEvent {
                    control: jog.clone(),
                    value: ControlValue::Delta(delta),
                },
                ts(1),
            )
            .unwrap();
    }
    // A 7-bit two's-complement encoder sends steps as absolute CC values;
    // keeping only the latest would lose turns, so it is marked ordered.
    coalescer
        .set_policy("APC", cc(48, 0).control, CoalescePolicy::Ordered)
        .unwrap();
    for raw in [1, 1, 127] {
        coalescer.offer("APC", cc(48, raw), ts(2)).unwrap();
    }
    let tick = coalescer.drain();
    assert_eq!(
        summary(&tick),
        vec![
            (jog, ControlValue::Delta(i32::MAX), 3),
            (
                cc(48, 0).control,
                ControlValue::Absolute { raw: 1, max: 127 },
                0
            ),
            (
                cc(48, 0).control,
                ControlValue::Absolute { raw: 1, max: 127 },
                0
            ),
            (
                cc(48, 0).control,
                ControlValue::Absolute { raw: 127, max: 127 },
                0
            ),
        ]
    );
    assert_eq!(
        coalescer.clear_policy("APC", &cc(48, 0).control),
        Some(CoalescePolicy::Ordered)
    );
}

#[test]
fn discarding_a_port_keeps_slots_queued_as_ordered_unmergeable() {
    let mut coalescer = ControlCoalescer::new(16).unwrap();
    coalescer
        .set_policy("APC", cc(48, 0).control, CoalescePolicy::Ordered)
        .unwrap();
    coalescer.offer("APC", cc(48, 1), ts(1)).unwrap();
    coalescer.offer("APC", cc(48, 1), ts(2)).unwrap();
    coalescer.offer("other", cc(7, 5), ts(3)).unwrap();
    // The override ends mid-tick: only later events coalesce, so the two
    // encoder steps already queued must survive a port discard.
    coalescer.clear_policy("APC", &cc(48, 0).control);
    assert_eq!(coalescer.discard_port("other"), 1);
    assert_eq!(
        coalescer.offer("APC", cc(48, 9), ts(4)),
        Ok(CoalesceOutcome::Queued)
    );
    assert_eq!(
        coalescer.offer("APC", cc(48, 10), ts(5)),
        Ok(CoalesceOutcome::Merged)
    );
    let tick = coalescer.drain();
    assert_eq!(
        tick.iter()
            .map(|event| event.event.clone())
            .collect::<Vec<_>>(),
        vec![cc(48, 1), cc(48, 1), cc(48, 10)]
    );
}

#[test]
fn capacity_is_bounded_and_rejections_are_reported_not_silent() {
    assert_eq!(
        ControlCoalescer::new(0).unwrap_err(),
        CoalesceError::InvalidCapacity(0)
    );
    assert_eq!(
        ControlCoalescer::new(MAX_COALESCER_CAPACITY + 1).unwrap_err(),
        CoalesceError::InvalidCapacity(MAX_COALESCER_CAPACITY + 1)
    );
    let mut coalescer = ControlCoalescer::new(2).unwrap();
    coalescer.offer("APC", cc(1, 0), ts(1)).unwrap();
    coalescer.offer("APC", cc(2, 0), ts(1)).unwrap();
    assert_eq!(
        coalescer.offer("APC", cc(3, 0), ts(1)),
        Err(CoalesceError::Full { capacity: 2 })
    );
    assert_eq!(
        coalescer.offer("APC", note(1, true), ts(1)),
        Err(CoalesceError::Full { capacity: 2 })
    );
    // Updates to pending controls never need a new slot.
    for raw in 0..1000_u16 {
        assert_eq!(
            coalescer.offer("APC", cc(1, raw % 128), ts(2)),
            Ok(CoalesceOutcome::Merged)
        );
    }
    assert_eq!(coalescer.pending(), 2);
    assert_eq!(coalescer.telemetry().rejected, 2);
    assert_eq!(coalescer.drain().len(), 2);
    assert_eq!(
        coalescer.offer("APC", cc(3, 0), ts(3)),
        Ok(CoalesceOutcome::Queued)
    );
    assert_eq!(
        coalescer.offer("", cc(3, 0), ts(3)),
        Err(CoalesceError::InvalidPort(String::new()))
    );
}

#[test]
fn ports_are_separate_scopes_and_detach_discards_one_port() {
    let mut coalescer = ControlCoalescer::new(8).unwrap();
    coalescer.offer("A", cc(7, 1), ts(1)).unwrap();
    coalescer.offer("B", cc(7, 2), ts(2)).unwrap();
    coalescer.offer("A", cc(7, 3), ts(3)).unwrap();
    coalescer.offer("B", note(1, true), ts(4)).unwrap();
    assert_eq!(coalescer.discard_port("B"), 2);
    // A's slot is still mergeable after the discard re-indexes.
    assert_eq!(
        coalescer.offer("A", cc(7, 4), ts(5)),
        Ok(CoalesceOutcome::Merged)
    );
    let tick = coalescer.drain();
    assert_eq!(tick.len(), 1);
    assert_eq!(
        (tick[0].port.as_str(), tick[0].event.clone()),
        ("A", cc(7, 4))
    );
}

fn midi_map() -> ControlMap<u8> {
    let mut map = ControlMap::default();
    map.add_profile(DeviceProfile {
        key: key("akai:apc:01"),
        name: "APC".into(),
        transport: DeviceTransport::Midi,
        port_name: "APC MINI".into(),
    })
    .unwrap();
    map.attach("APC MINI", &key("akai:apc:01")).unwrap();
    map.add_binding(ControlBinding {
        id: id("vol"),
        device: key("akai:apc:01"),
        control: cc(7, 0).control,
        action: 1,
        response: Response::Absolute {
            input: None,
            output: Span::new(0.0, 127.0).unwrap(),
            invert: false,
            deadband: 0.0,
            pickup: Pickup::SoftTakeover { window: 0.02 },
        },
    })
    .unwrap();
    map
}

#[test]
fn coalesced_ticks_keep_soft_takeover_from_jumping_the_live_gain() {
    let mut map = midi_map();
    // Live volume is 64; the physical fader starts at the bottom.
    map.set_target_value(&id("vol"), 64.0).unwrap();
    let mut coalescer = ControlCoalescer::new(8).unwrap();
    let mut live = 64.0;
    let mut applied = Vec::new();
    let mut awaiting = 0;
    // The operator pushes the fader up at 2 steps per message, 8 messages
    // per tick: every tick moves the hardware 16 steps, so ticks jump over
    // the pickup window without landing in it.
    let mut raw = 0_u16;
    for tick in 0..8_i64 {
        for message in 0..8_i64 {
            raw = (raw + 2).min(127);
            coalescer
                .offer("APC MINI", cc(7, raw), ts(tick * 10 + message))
                .unwrap();
        }
        for event in coalescer.drain() {
            let report = map
                .process(&event.port, &event.event, event.last_at)
                .unwrap();
            for (_, reason) in &report.suppressed {
                assert!(matches!(reason, Suppressed::AwaitingPickup { .. }));
                awaiting += 1;
            }
            for output in report.outputs {
                let ActionValue::Level(level) = output.into_command().value else {
                    panic!("fader must emit levels");
                };
                applied.push(level);
                live = level;
            }
        }
    }
    // Ticks 0..=3 end at 16, 32, 48, 64 = target: tick 3 lands exactly on
    // the target (within the window), so the first three are suppressed.
    assert_eq!(awaiting, 3);
    assert_eq!(applied.first().copied(), Some(64.0));
    // After pickup the live value follows the hardware one tick at a time,
    // never jumping by more than one tick of travel.
    assert!(
        applied
            .windows(2)
            .all(|pair| (pair[1] - pair[0]).abs() <= 16.0)
    );
    assert!((live - 127.0).abs() < f64::EPSILON);
}

// ------------------------------------------------------- outbound feedback

fn motor(name: &str, device: &DeviceKey, controller: u8) -> FeedbackBinding {
    FeedbackBinding {
        id: id(name),
        device: device.clone(),
        source: FeedbackSource::Level {
            key: format!("{name}.volume"),
            range: Span::new(0.0, 100.0).unwrap(),
        },
        target: FeedbackTarget::MidiCc14 {
            channel: ch(0),
            controller,
        },
    }
}

fn schedule(budget: usize, interval: u64) -> FeedbackSchedule {
    FeedbackSchedule {
        per_device_budget: NonZeroUsize::new(budget).unwrap(),
        min_interval: MediaDuration::new(interval),
        per_device_bytes_per_second: None,
        settle_after_release: MediaDuration::new(0),
    }
}

fn names(messages: &[fm_automation::FeedbackMessage]) -> Vec<&str> {
    messages
        .iter()
        .map(|message| message.binding.as_str())
        .collect()
}

fn midi(payload: &FeedbackPayload) -> &[u8] {
    match payload {
        FeedbackPayload::Midi(bytes) => bytes,
        FeedbackPayload::Osc(_) => panic!("expected MIDI"),
    }
}

fn motor_bank(device: &DeviceKey, count: u8) -> FeedbackRouter {
    let mut router = FeedbackRouter::default();
    for index in 0..count {
        router
            .add(motor(&format!("f{index}"), device, index))
            .unwrap();
    }
    router.attach(device);
    router
}

#[test]
fn high_rate_level_changes_send_only_the_latest_changed_value() {
    let device = key("behringer:xtouch:1");
    let mut router = motor_bank(&device, 1);
    for step in 0..=1000 {
        router
            .observe_level("f0.volume", f64::from(step) / 10.0)
            .unwrap();
    }
    let tick = router.drain_tick(ts(0), schedule(8, 0));
    assert_eq!(names(&tick.messages), ["f0"]);
    assert_eq!(
        midi(&tick.messages[0].payload),
        [0xB0, 0x00, 0x7F, 0xB0, 0x20, 0x7F]
    );
    // Same value again: deduplicated, nothing to send.
    router.observe_level("f0.volume", 100.0).unwrap();
    let idle = router.drain_tick(ts(10), schedule(8, 0));
    assert_eq!(idle.messages.len(), 0);
    assert_eq!(router.pending(), 0);
}

#[test]
fn per_device_budget_rotates_so_no_fader_starves() {
    let device = key("behringer:xtouch:1");
    let mut router = motor_bank(&device, 4);
    let mut seen = vec![0_u32; 4];
    for tick in 0..4_i64 {
        // Every fader changes every tick, more than the budget allows.
        for index in 0..4 {
            router
                .observe_level(
                    &format!("f{index}.volume"),
                    f64::from(u8::try_from(tick).unwrap()) + 1.0,
                )
                .unwrap();
        }
        let out = router.drain_tick(ts(tick * 10), schedule(2, 0));
        assert_eq!(out.messages.len(), 2);
        assert_eq!(out.deferred_budget, 2);
        for message in &out.messages {
            let index: usize = message.binding.as_str()[1..].parse().unwrap();
            seen[index] += 1;
        }
    }
    // Round-robin: each of the four faders got exactly two of eight slots.
    assert_eq!(seen, [2, 2, 2, 2]);
}

#[test]
fn budgets_are_independent_per_device() {
    let desk = key("behringer:xtouch:1");
    let lights = key("akai:apc:01");
    let mut router = motor_bank(&desk, 3);
    for (index, rule) in [
        ActivatorRule::Program(input(1)),
        ActivatorRule::Preview(input(1)),
        ActivatorRule::Recording,
    ]
    .into_iter()
    .enumerate()
    {
        router
            .add(FeedbackBinding {
                id: id(&format!("led{index}")),
                device: lights.clone(),
                source: FeedbackSource::Activator(rule),
                target: FeedbackTarget::MidiNote {
                    channel: ch(0),
                    note: u8::try_from(index).unwrap(),
                    on_velocity: 1,
                    off_velocity: 0,
                },
            })
            .unwrap();
    }
    router.attach(&lights);
    for index in 0..3 {
        router
            .observe_level(&format!("f{index}.volume"), 50.0)
            .unwrap();
    }
    router
        .observe_tally(&TallySnapshot {
            program: Some(input(1)),
            preview: Some(input(1)),
            recording: true,
            ..TallySnapshot::default()
        })
        .unwrap();
    let tick = router.drain_tick(ts(0), schedule(1, 0));
    // Device key order: the APC LEDs, then the X-Touch faders.
    assert_eq!(names(&tick.messages), ["led0", "f0"]);
    assert_eq!(tick.deferred_budget, 4);
    let tick = router.drain_tick(ts(1), schedule(1, 0));
    assert_eq!(names(&tick.messages), ["led1", "f1"]);
}

#[test]
fn min_interval_paces_a_motor_and_sends_the_latest_value_when_due() {
    let device = key("behringer:xtouch:1");
    let mut router = motor_bank(&device, 1);
    router.observe_level("f0.volume", 10.0).unwrap();
    assert_eq!(
        router.drain_tick(ts(100), schedule(4, 20)).messages.len(),
        1
    );
    for (time, level) in [(105, 20.0), (110, 30.0), (119, 40.0)] {
        router.observe_level("f0.volume", level).unwrap();
        let tick = router.drain_tick(ts(time), schedule(4, 20));
        assert_eq!(tick.messages.len(), 0);
        assert_eq!(tick.deferred_interval, 1);
    }
    let due = router.drain_tick(ts(120), schedule(4, 20));
    assert_eq!(names(&due.messages), ["f0"]);
    let expected = {
        let mut probe = motor_bank(&device, 1);
        probe.observe_level("f0.volume", 40.0).unwrap();
        probe.drain(1).remove(0).payload
    };
    assert_eq!(due.messages[0].payload, expected);
    // A clock that runs backwards does not stall feedback.
    router.observe_level("f0.volume", 50.0).unwrap();
    assert_eq!(router.drain_tick(ts(0), schedule(4, 20)).messages.len(), 1);
}

#[test]
fn touched_fader_is_never_driven_and_resyncs_once_on_release() {
    let device = key("behringer:xtouch:1");
    let mut router = motor_bank(&device, 2);
    router.observe_level("f0.volume", 50.0).unwrap();
    let first = router.drain_tick(ts(0), schedule(8, 0));
    assert_eq!(names(&first.messages), ["f0"]);

    assert_eq!(router.set_touched(&id("f0"), true), Ok(true));
    assert_eq!(router.set_touched(&id("f0"), true), Ok(false));
    assert!(router.is_touched(&id("f0")));
    // The operator rides the fader; the engine follows it. None of it is
    // echoed to the motor, but other faders still update.
    for level in [55.0, 60.0, 72.5] {
        router.observe_level("f0.volume", level).unwrap();
        router.observe_level("f1.volume", level).unwrap();
        let tick = router.drain_tick(ts(1), schedule(8, 0));
        assert_eq!(names(&tick.messages), ["f1"]);
        assert_eq!(tick.withheld_touched, 1);
    }
    // The legacy unpaced drain respects touch as well.
    router.observe_level("f0.volume", 50.0).unwrap();
    assert_eq!(router.drain(8).len(), 0);

    // Release: the engine value is 50.0, the same bytes last sent before the
    // touch, yet the hand moved the motor, so it is resent exactly once.
    assert_eq!(router.set_touched(&id("f0"), false), Ok(true));
    let release = router.drain_tick(ts(2), schedule(8, 0));
    assert_eq!(names(&release.messages), ["f0"]);
    assert_eq!(release.messages[0].payload, first.messages[0].payload);
    assert_eq!(router.drain_tick(ts(3), schedule(8, 0)).messages.len(), 0);

    assert_eq!(
        router.set_touched(&id("missing"), true),
        Err(FeedbackError::UnknownBinding(id("missing")))
    );
    // Re-attaching the device clears stale touch state.
    router.set_touched(&id("f1"), true).unwrap();
    router.attach(&device);
    assert!(!router.is_touched(&id("f1")));
}

#[test]
fn a_released_fader_waits_out_the_settle_hold() {
    let device = key("behringer:xtouch:1");
    let mut router = motor_bank(&device, 1);
    router.observe_level("f0.volume", 50.0).unwrap();
    let mut paced = schedule(8, 0);
    paced.settle_after_release = MediaDuration::new(100);
    router.drain_tick(ts(0), paced);

    router.set_touched(&id("f0"), true).unwrap();
    router.observe_level("f0.volume", 70.0).unwrap();
    assert!(router.drain_tick(ts(10), paced).messages.is_empty());

    // Release: the first drain starts the hold; nothing goes out yet.
    router.set_touched(&id("f0"), false).unwrap();
    let held = router.drain_tick(ts(20), paced);
    assert!(held.messages.is_empty());
    assert_eq!(held.deferred_settle, 1);
    let still_held = router.drain_tick(ts(50), paced);
    assert!(still_held.messages.is_empty());
    assert_eq!(still_held.deferred_settle, 1);

    // Past the hold, the authoritative value is resent once.
    let released = router.drain_tick(ts(120), paced);
    assert_eq!(names(&released.messages), ["f0"]);
    assert_eq!(released.deferred_settle, 0);
    assert!(router.drain_tick(ts(130), paced).messages.is_empty());
}

#[test]
fn a_device_byte_rate_limit_defers_to_the_next_second() {
    let device = key("behringer:xtouch:1");
    let mut router = motor_bank(&device, 2);
    router.observe_level("f0.volume", 40.0).unwrap();
    router.observe_level("f1.volume", 50.0).unwrap();
    // One 14-bit CC message is six bytes: the limit fits one, not two.
    let mut paced = schedule(8, 0);
    paced.per_device_bytes_per_second = NonZeroUsize::new(9);
    let first = router.drain_tick(ts(0), paced);
    assert_eq!(first.messages.len(), 1);
    assert_eq!(first.deferred_bytes, 1);
    // The one-second window resets and the second message goes out.
    let next = router.drain_tick(ts(1_000_000_000), paced);
    assert_eq!(next.messages.len(), 1);
    assert_eq!(next.deferred_bytes, 0);
}

#[test]
fn acknowledged_hardware_values_are_not_echoed_but_corrections_are() {
    let device = key("behringer:xtouch:1");
    let mut router = motor_bank(&device, 1);
    router.observe_level("f0.volume", 20.0).unwrap();
    router.drain_tick(ts(0), schedule(8, 0));
    // A non-touch-sensing fader moved to 35: the device already shows it.
    router.acknowledge_level(&id("f0"), 35.0).unwrap();
    router.observe_level("f0.volume", 35.0).unwrap();
    assert_eq!(router.drain_tick(ts(1), schedule(8, 0)).messages.len(), 0);
    // The engine clamps the next move to 90 while the hardware reads 100:
    // the correction is sent.
    router.acknowledge_level(&id("f0"), 100.0).unwrap();
    router.observe_level("f0.volume", 90.0).unwrap();
    assert_eq!(router.drain_tick(ts(2), schedule(8, 0)).messages.len(), 1);

    assert_eq!(
        router.acknowledge_level(&id("f0"), f64::INFINITY),
        Err(FeedbackError::NonFiniteValue)
    );
    router
        .add(FeedbackBinding {
            id: id("pgm"),
            device: device.clone(),
            source: FeedbackSource::Activator(ActivatorRule::Program(input(1))),
            target: FeedbackTarget::MidiNote {
                channel: ch(1),
                note: 0,
                on_velocity: 127,
                off_velocity: 0,
            },
        })
        .unwrap();
    assert_eq!(
        router.acknowledge_level(&id("pgm"), 1.0),
        Err(FeedbackError::NotALevel(id("pgm")))
    );
}

#[test]
fn activator_flicker_within_a_tick_sends_nothing() {
    let device = key("akai:apc:01");
    let mut router = FeedbackRouter::default();
    router
        .add(FeedbackBinding {
            id: id("pgm"),
            device: device.clone(),
            source: FeedbackSource::Activator(ActivatorRule::Program(input(1))),
            target: FeedbackTarget::MidiNote {
                channel: ch(0),
                note: 0,
                on_velocity: 3,
                off_velocity: 0,
            },
        })
        .unwrap();
    router.attach(&device);
    let on = TallySnapshot {
        program: Some(input(1)),
        ..TallySnapshot::default()
    };
    router.observe_tally(&on).unwrap();
    assert_eq!(router.drain_tick(ts(0), schedule(4, 0)).messages.len(), 1);
    for _ in 0..50 {
        router.observe_tally(&TallySnapshot::default()).unwrap();
        router.observe_tally(&on).unwrap();
    }
    assert_eq!(router.drain_tick(ts(1), schedule(4, 0)).messages.len(), 0);
}
