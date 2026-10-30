//! AU-006: the ordered plugin chain, its bypass, latency compensation, and
//! per-plugin state snapshots.

use fm_plugin_host_api::{
    MAX_PLUGIN_CHAIN_SLOTS, MAX_PLUGIN_LATENCY_FRAMES, PluginChain, PluginChainError, PluginId,
    PluginSlot, ProtocolLimits, StateSnapshot, StateVersion,
};

fn plugin(name: &str) -> PluginId {
    PluginId::new(name)
}

fn slot(name: &str) -> PluginSlot {
    PluginSlot::new(plugin(name))
}

#[test]
fn a_chain_keeps_order_and_refuses_duplicates_bounds_and_bad_indices() {
    let mut chain = PluginChain::new();
    assert!(chain.is_empty());
    chain.push(slot("eq")).unwrap();
    chain.push(slot("comp")).unwrap();
    chain.insert(0, slot("gate")).unwrap();
    let order: Vec<_> = chain
        .slots()
        .iter()
        .map(|slot| slot.plugin().as_str().to_owned())
        .collect();
    assert_eq!(order, ["gate", "eq", "comp"]);
    assert!(chain.contains(&plugin("eq")));
    assert_eq!(
        chain.slot(&plugin("comp")).unwrap().plugin(),
        &plugin("comp")
    );

    // A plugin appears once, an index names a slot or the end, and the
    // chain holds at most eight slots.
    assert!(matches!(
        chain.push(slot("eq")),
        Err(PluginChainError::DuplicatePlugin(found)) if found == plugin("eq")
    ));
    assert!(matches!(
        chain.insert(4, slot("limiter")),
        Err(PluginChainError::OutOfRange { index: 4, len: 3 })
    ));
    for name in ["limiter", "deesser", "reverb", "delay", "chorus"] {
        chain.push(slot(name)).unwrap();
    }
    assert_eq!(chain.len(), MAX_PLUGIN_CHAIN_SLOTS);
    assert!(matches!(
        chain.push(slot("one-too-many")),
        Err(PluginChainError::TooManySlots {
            maximum: MAX_PLUGIN_CHAIN_SLOTS
        })
    ));

    let removed = chain.remove(0).expect("the first slot");
    assert_eq!(removed.plugin(), &plugin("gate"));
    assert!(!chain.contains(&plugin("gate")));
    assert!(chain.remove(chain.len()).is_none());
}

#[test]
fn bypass_and_order_drive_the_effective_latency() {
    let mut chain = PluginChain::new();
    for (name, frames) in [("eq", 2), ("comp", 3), ("gate", 4)] {
        let mut slot = slot(name);
        slot.set_latency(frames).unwrap();
        chain.push(slot).unwrap();
    }
    assert_eq!(chain.effective_latency_frames(), 9);
    assert_eq!(
        chain
            .active_plugins()
            .map(PluginId::as_str)
            .collect::<Vec<_>>(),
        ["eq", "comp", "gate"]
    );

    // A bypassed plugin neither processes nor adds latency.
    chain.set_bypassed(1, true).unwrap();
    assert!(chain.slots()[1].is_bypassed());
    assert_eq!(chain.effective_latency_frames(), 6);
    assert_eq!(
        chain
            .active_plugins()
            .map(PluginId::as_str)
            .collect::<Vec<_>>(),
        ["eq", "gate"]
    );
    chain.set_bypassed(1, false).unwrap();
    assert_eq!(chain.effective_latency_frames(), 9);

    // A slot's latency is bounded, and indices must name a slot.
    assert!(matches!(
        chain.set_latency(0, MAX_PLUGIN_LATENCY_FRAMES + 1),
        Err(PluginChainError::LatencyTooLarge { .. })
    ));
    assert!(matches!(
        chain.set_latency(3, 1),
        Err(PluginChainError::OutOfRange { index: 3, len: 3 })
    ));
    assert!(matches!(
        chain.set_bypassed(3, true),
        Err(PluginChainError::OutOfRange { index: 3, len: 3 })
    ));

    // Moving a slot reorders processing without duplicating it.
    chain.move_to(0, 2).unwrap();
    assert_eq!(
        chain
            .slots()
            .iter()
            .map(|slot| slot.plugin().as_str())
            .collect::<Vec<_>>(),
        ["comp", "gate", "eq"]
    );
    assert!(matches!(
        chain.move_to(0, 3),
        Err(PluginChainError::OutOfRange { index: 3, len: 3 })
    ));
}

#[test]
fn a_slot_keeps_only_its_own_plugins_snapshot() {
    let limits = ProtocolLimits::default();
    let mut eq = slot("eq");
    let snapshot = StateSnapshot::new(
        plugin("eq"),
        StateVersion::new(4),
        vec![1, 2, 3, 4],
        &limits,
    )
    .unwrap();
    eq.store_snapshot(snapshot.clone()).unwrap();
    assert_eq!(eq.snapshot(), Some(&snapshot));
    eq.clear_snapshot();
    assert!(eq.snapshot().is_none());

    let other =
        StateSnapshot::new(plugin("comp"), StateVersion::new(1), Vec::new(), &limits).unwrap();
    assert_eq!(
        eq.store_snapshot(other),
        Err(PluginChainError::SnapshotPluginMismatch {
            expected: plugin("eq"),
            found: plugin("comp"),
        })
    );
}
