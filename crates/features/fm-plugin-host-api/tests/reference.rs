//! AU-006: the built-in reference processors a daemon renders through a
//! plugin chain, their saved state, bypass, and latency.

use fm_plugin_host_api::{
    PluginChain, PluginChainError, PluginId, PluginSlot, ProtocolLimits, REFERENCE_POLARITY,
    REFERENCE_SOFTCLIP, REFERENCE_TRIM, ReferenceError, ReferenceRack, StateSnapshot, StateVersion,
    is_reference_plugin, state_bytes,
};

fn slot(plugin: &str, milli_db: Option<i32>) -> PluginSlot {
    let mut slot = PluginSlot::new(PluginId::new(plugin));
    if let Some(milli_db) = milli_db {
        let snapshot = StateSnapshot::new(
            slot.plugin().clone(),
            StateVersion::new(1),
            state_bytes(milli_db),
            &ProtocolLimits::default(),
        )
        .unwrap();
        slot.store_snapshot(snapshot).unwrap();
    }
    slot
}

#[test]
fn a_reference_trim_scales_and_bypass_restores() {
    let mut chain = PluginChain::new();
    chain.push(slot(REFERENCE_TRIM, Some(-12_000))).unwrap();
    let mut rack = ReferenceRack::new(&chain, 48_000.0).unwrap();
    assert_eq!(rack.active_plugins().count(), 1);
    assert_eq!(rack.latency_frames(), 0);
    let mut planes = vec![vec![1.0_f32; 8], vec![-1.0_f32; 8]];
    rack.process(&mut planes, 8);
    for sample in &planes[0] {
        assert!((sample - 0.251_188_6).abs() < 1e-6, "{sample}");
    }
    for sample in &planes[1] {
        assert!((sample + 0.251_188_6).abs() < 1e-6, "{sample}");
    }

    chain.set_bypassed(0, true).unwrap();
    let mut rack = ReferenceRack::new(&chain, 48_000.0).unwrap();
    assert!(rack.is_empty());
    let mut planes = vec![vec![1.0_f32; 8]];
    rack.process(&mut planes, 8);
    assert_eq!(planes[0], vec![1.0_f32; 8]);
}

#[test]
fn polarity_soft_clip_and_latency_render_in_order() {
    let mut chain = PluginChain::new();
    chain.push(slot(REFERENCE_POLARITY, None)).unwrap();
    chain.push(slot(REFERENCE_SOFTCLIP, None)).unwrap();
    chain.set_latency(1, 4).unwrap();
    let mut rack = ReferenceRack::new(&chain, 48_000.0).unwrap();
    assert_eq!(
        rack.active_plugins()
            .map(PluginId::as_str)
            .collect::<Vec<_>>(),
        [REFERENCE_POLARITY, REFERENCE_SOFTCLIP]
    );
    assert_eq!(rack.latency_frames(), 4);
    let mut planes = vec![vec![2.0_f32, 0.5, -0.5, -2.0]];
    rack.process(&mut planes, 4);
    // Polarity first, then the 0 dBFS soft knee: a 2.0 magnitude clips to 1.
    assert!((planes[0][0] + 1.0).abs() < 1e-6, "{:?}", planes[0]);
    assert!((planes[0][1] + 0.6875).abs() < 1e-5, "{:?}", planes[0]);
    assert!((planes[0][2] - 0.6875).abs() < 1e-5, "{:?}", planes[0]);
    assert!((planes[0][3] - 1.0).abs() < 1e-6, "{:?}", planes[0]);
}

#[test]
fn a_rack_refuses_unknown_plugins_and_bad_state() {
    assert!(is_reference_plugin(&PluginId::new(REFERENCE_TRIM)));
    assert!(!is_reference_plugin(&PluginId::new("vendor.vst3")));
    let mut chain = PluginChain::new();
    chain.push(slot("vendor.vst3", None)).unwrap();
    assert_eq!(
        ReferenceRack::new(&chain, 48_000.0).unwrap_err(),
        ReferenceError::UnsupportedPlugin(PluginId::new("vendor.vst3"))
    );

    let mut chain = PluginChain::new();
    chain.push(slot(REFERENCE_TRIM, Some(24_001))).unwrap();
    assert!(matches!(
        ReferenceRack::new(&chain, 48_000.0),
        Err(ReferenceError::InvalidState(plugin)) if plugin == PluginId::new(REFERENCE_TRIM)
    ));
    assert!(matches!(
        ReferenceRack::new(&PluginChain::new(), 0.0),
        Err(ReferenceError::InvalidSampleRate)
    ));

    // A polarity slot carries no state.
    let mut chain = PluginChain::new();
    chain.push(slot(REFERENCE_POLARITY, Some(0))).unwrap();
    assert!(matches!(
        ReferenceRack::new(&chain, 48_000.0),
        Err(ReferenceError::InvalidState(plugin)) if plugin == PluginId::new(REFERENCE_POLARITY)
    ));
    let _ = PluginChainError::TooManySlots { maximum: 8 };
}
