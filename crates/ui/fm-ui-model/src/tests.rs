use core::num::NonZeroU128;

use fm_protocol::{
    CommandResult, EngineIdentity, FadeToBlackPosition,
    StreamRealizedState as ProtocolStreamRealizedState,
};

use super::*;

fn project(value: u128) -> ProjectId {
    ProjectId::new(NonZeroU128::new(value).unwrap())
}

fn input(value: u128) -> InputId {
    InputId::new(NonZeroU128::new(value).unwrap())
}

fn output(value: u128) -> OutputId {
    OutputId::new(NonZeroU128::new(value).unwrap())
}

fn engine(id: &str, epoch: u64, log: &str) -> EngineIdentity {
    EngineIdentity {
        engine_id: id.into(),
        state_epoch: epoch,
        log_id: log.into(),
    }
}

fn overlays() -> Vec<OverlayStatus> {
    (1..=8)
        .map(|channel| OverlayStatus {
            channel,
            source: None,
            active: false,
            opacity: 0,
            transition: fm_protocol::OverlayTransitionKind::Cut,
            duration_frames: 1,
            position: fm_protocol::OverlayPositionPreset::FullFrame,
            border: fm_protocol::OverlayBorderPreset::None,
            queued_sources: Vec::new(),
            included_outputs: Vec::new(),
        })
        .collect()
}

fn input_audio_strips() -> Vec<InputAudioStripStatus> {
    [input(1), input(2), input(3)]
        .into_iter()
        .map(|input| InputAudioStripStatus {
            pan_basis_points: 0,
            group: None,
            input,
            gain_millidb: 0,
            balance_basis_points: 0,
            muted: false,
            soloed: false,
            follow_video: true,
            delay_samples: 0,
        })
        .collect()
}

fn recorder_slot(value: u8) -> WireRecorderSlot {
    WireRecorderSlot::new(value).unwrap()
}

fn recordings() -> Vec<RecordingStatus> {
    vec![
        RecordingStatus {
            recorder: recorder_slot(1),
            desired_active: false,
        },
        RecordingStatus {
            recorder: recorder_slot(2),
            desired_active: false,
        },
    ]
}

fn snapshot(project_id: ProjectId, revision: u64) -> ProjectSnapshot {
    ProjectSnapshot {
        audio_mixer: fm_protocol::AudioMixerStatus::default(),
        fade_to_black_excluded_outputs: Vec::new(),
        fade_to_black_output_durations: Vec::new(),
        fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
        display_outputs: Vec::new(),
        network_streams: Vec::new(),
        system_audio_inputs: Vec::new(),
        screen_capture_inputs: Vec::new(),
        capture_inputs: Vec::new(),
        display_output_states: Vec::new(),
        cursor: ProjectCursor {
            project_id,
            engine: engine("engine-a", 1, "log-a"),
            revision: Revision::new(revision),
        },
        show_name: "Show".into(),
        inputs: vec![input(1), input(2), input(3)],
        input_names: vec!["Camera".into(), "Slides".into(), "Guest".into()],
        outputs: vec![
            OutputStatus {
                output: output(1),
                name: "Main".into(),
            },
            OutputStatus {
                output: output(2),
                name: "Aux".into(),
            },
        ],
        input_audio_strips: input_audio_strips(),
        stingers: Vec::new(),
        streams: Vec::new(),
        recordings: recordings(),
        desired_overlays: overlays(),
        realized_overlays: overlays(),
        switcher: SwitcherState {
            desired: BusSelection::new(input(1), input(2)),
            realized: BusSelection::new(input(1), input(2)),
            desired_manual_transition: ManualTransitionStatus::Inactive,
            realized_manual_transition: ManualTransitionStatus::Inactive,
            desired_fade_to_black: FadeToBlackState {
                target_active: false,
                position: FadeToBlackPosition::LIVE,
            },
            realized_fade_to_black: FadeToBlackState {
                target_active: false,
                position: FadeToBlackPosition::LIVE,
            },
            runtime_generation: Some(4),
        },
        edit_locks: EditLocks::default(),
        edit_history: EditHistoryStatus::EMPTY,
        titles: Vec::new(),
        scenes: Vec::new(),
        scene_layouts: Vec::new(),
        frame_rate: fm_types::FrameRate::new(25, 1).unwrap(),
        playlist_follows: false,
        grade_states: 0,
        animation_preset_states: 0,
        media_states: 0,
        paged_states: 0,
        cue_list_states: 0,
        script_states: 0,
        shortcut_states: 0,
        still_states: 0,
        media_list_states: 0,
        input_lifecycle: vec![fm_protocol::InputLifecycleStatus::default(); 3],
        input_categories: Vec::new(),
        transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
        stream_targets: Vec::new(),
        live_lan: fm_protocol::LiveLanEntry::default(),
        live_lan_status: fm_protocol::LiveLanStatus::default(),
    }
}

fn event(
    project_id: ProjectId,
    engine: EngineIdentity,
    revision: u64,
    change: DurableChange,
) -> DurableProjectEvent {
    DurableProjectEvent {
        cursor: ProjectCursor {
            project_id,
            engine,
            revision: Revision::new(revision),
        },
        change,
        edit_history: EditHistoryStatus::EMPTY,
    }
}

fn realization(
    project_id: ProjectId,
    engine: EngineIdentity,
    revision: u64,
    generation: u64,
    sequence: u64,
) -> RuntimeRealization {
    RuntimeRealization {
        project_id,
        engine,
        revision: Revision::new(revision),
        generation,
        sequence,
        manual_transition: ManualTransitionStatus::Inactive,
        fade_to_black: fade_to_black(false, 0),
    }
}

fn desired(selection: BusSelection) -> DurableChange {
    DurableChange::DesiredSwitcher {
        selection,
        manual_transition: ManualTransitionStatus::Inactive,
        fade_to_black: FadeToBlackState {
            target_active: false,
            position: FadeToBlackPosition::LIVE,
        },
        overlays: overlays(),
        input_audio_strips: input_audio_strips(),
    }
}

fn fade_to_black(target_active: bool, numerator: u16) -> FadeToBlackState {
    FadeToBlackState {
        target_active,
        position: FadeToBlackPosition::new(numerator),
    }
}

#[test]
fn installs_and_validates_snapshot() {
    let project_id = project(10);
    let mut invalid_names = snapshot(project_id, 1);
    invalid_names.input_names.pop();
    assert_eq!(
        ClientModel::new(project_id)
            .install_snapshot(invalid_names)
            .unwrap_err(),
        ModelError::InvalidInputNames
    );
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();

    let state = model.state().unwrap();
    assert_eq!(state.show_name(), "Show");
    assert_eq!(state.input_name(input(1)), Some("Camera"));
    assert_eq!(model.view().unwrap().input_names[1], "Slides");
    assert_eq!(state.switcher().desired.program, input(1));
    assert_eq!(model.sync_status(), &SyncStatus::Current);

    let error = model
        .install_snapshot(snapshot(project(11), 8))
        .unwrap_err();
    assert!(matches!(error, ModelError::ProjectMismatch { .. }));
    assert_eq!(model.reconnect_cursor().unwrap().revision, Revision::new(7));

    let error = model.install_snapshot(snapshot(project_id, 6)).unwrap_err();
    assert_eq!(
        error,
        ModelError::OutOfOrder {
            current: Revision::new(7),
            observed: Revision::new(6),
        }
    );
    assert_eq!(model.reconnect_cursor().unwrap().revision, Revision::new(7));
}

#[test]
fn reduces_exact_desired_and_realized_fade_to_black_state() {
    let project_id = project(10);
    let mut initial = snapshot(project_id, 4);
    initial.switcher.desired_fade_to_black = fade_to_black(true, 40_000);
    initial.switcher.realized_fade_to_black = fade_to_black(true, 20_000);

    let mut model = ClientModel::new(project_id);
    model.install_snapshot(initial).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    assert_eq!(
        model.state().unwrap().switcher().desired_fade_to_black,
        fade_to_black(true, 40_000)
    );
    assert_eq!(
        model.state().unwrap().switcher().realized_fade_to_black,
        fade_to_black(true, 20_000)
    );

    model
        .apply_event(event(
            project_id,
            identity.clone(),
            5,
            DurableChange::DesiredSwitcher {
                selection: BusSelection::new(input(1), input(2)),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: fade_to_black(false, 12_345),
                overlays: overlays(),
                input_audio_strips: input_audio_strips(),
            },
        ))
        .unwrap();
    assert_eq!(
        model.state().unwrap().switcher().desired_fade_to_black,
        fade_to_black(false, 12_345)
    );
    assert_eq!(
        model.state().unwrap().switcher().realized_fade_to_black,
        fade_to_black(true, 20_000)
    );

    let mut realized = realization(project_id, identity, 5, 9, 1);
    realized.fade_to_black = fade_to_black(false, 12_345);
    model.apply_runtime_realization(realized).unwrap();
    assert_eq!(
        model.state().unwrap().switcher().realized_fade_to_black,
        fade_to_black(false, 12_345)
    );
}

#[test]
fn applies_contiguous_events_and_tracks_desired_separately_from_realized() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();

    model
        .apply_event(event(
            project_id,
            identity.clone(),
            8,
            desired(BusSelection::new(input(2), input(3))),
        ))
        .unwrap();
    let after_desired = model.state().unwrap().switcher();
    assert_eq!(after_desired.desired, BusSelection::new(input(2), input(3)));
    assert_eq!(
        after_desired.realized,
        BusSelection::new(input(1), input(2))
    );

    model
        .apply_runtime_realization(realization(project_id, identity, 8, 5, 1))
        .unwrap();
    let realized = model.state().unwrap().switcher();
    assert_eq!(realized.realized, BusSelection::new(input(2), input(3)));
    assert_eq!(realized.runtime_generation, Some(5));
}

#[test]
fn ignores_exact_duplicates_but_rejects_conflicting_ones() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 1)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    let first = event(
        project_id,
        identity.clone(),
        2,
        desired(BusSelection::new(input(2), input(1))),
    );

    assert!(matches!(
        model.apply_event(first.clone()).unwrap(),
        EventApplied::Applied { .. }
    ));
    assert_eq!(model.apply_event(first), Ok(EventApplied::Duplicate));

    let conflict = event(
        project_id,
        identity,
        2,
        desired(BusSelection::new(input(3), input(1))),
    );
    assert!(matches!(
        model.apply_event(conflict),
        Err(ModelError::ConflictingDuplicate { .. })
    ));
    assert_eq!(model.sync_status(), &SyncStatus::RequiresSnapshot);
}

#[test]
fn exposes_ordered_outputs_and_rejects_invalid_output_references() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 1)).unwrap();
    let view = model.view().unwrap();
    assert_eq!(
        view.outputs
            .iter()
            .map(|output| output.output)
            .collect::<Vec<_>>(),
        vec![output(1), output(2)]
    );

    let mut duplicate = snapshot(project_id, 2);
    duplicate.outputs[1].output = output(1);
    assert_eq!(
        model.install_snapshot(duplicate),
        Err(ModelError::DuplicateOutput(output(1)))
    );

    let mut unknown_snapshot = snapshot(project_id, 2);
    unknown_snapshot.desired_overlays[0]
        .included_outputs
        .push(output(3));
    assert_eq!(
        model.install_snapshot(unknown_snapshot),
        Err(ModelError::UnknownOutput(output(3)))
    );

    let identity = model.reconnect_cursor().unwrap().engine.clone();
    let mut unknown_event = desired(BusSelection::new(input(1), input(2)));
    let DurableChange::DesiredSwitcher { overlays, .. } = &mut unknown_event else {
        unreachable!()
    };
    overlays[0].included_outputs.push(output(3));
    assert_eq!(
        model.apply_event(event(project_id, identity, 2, unknown_event)),
        Err(ModelError::UnknownOutput(output(3)))
    );
}

#[test]
fn rejects_gaps_without_advancing_and_recovers_when_missing_event_arrives() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 4)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    let change = desired(BusSelection::new(input(2), input(1)));

    assert_eq!(
        model.apply_event(event(project_id, identity.clone(), 6, change.clone())),
        Err(ModelError::RevisionGap {
            expected: Revision::new(5),
            observed: Revision::new(6),
        })
    );
    assert_eq!(model.reconnect_cursor().unwrap().revision, Revision::new(4));
    assert!(model.is_stale());

    model
        .apply_event(event(project_id, identity, 5, change))
        .unwrap();
    assert_eq!(model.reconnect_cursor().unwrap().revision, Revision::new(5));
    assert!(model.is_stale());
}

#[test]
fn rejects_project_and_engine_identity_changes() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 4)).unwrap();
    let change = desired(BusSelection::new(input(2), input(1)));

    assert!(matches!(
        model.apply_event(event(
            project(11),
            engine("engine-a", 1, "log-a"),
            5,
            change.clone(),
        )),
        Err(ModelError::ProjectMismatch { .. })
    ));
    assert!(matches!(
        model.apply_event(event(project_id, engine("engine-a", 2, "log-b"), 5, change,)),
        Err(ModelError::EngineMismatch { .. })
    ));
    assert_eq!(model.sync_status(), &SyncStatus::RequiresSnapshot);
    assert_eq!(model.reconnect_cursor().unwrap().revision, Revision::new(4));
}

#[test]
fn optimistic_accept_remains_until_accepted_revision_is_applied() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 1)).unwrap();
    model
        .track_command(
            CommandId::new("select-3"),
            Some(OptimisticChange::DesiredPreview(input(3))),
        )
        .unwrap();
    assert_eq!(model.view().unwrap().switcher.desired.preview, input(3));
    assert_eq!(model.state().unwrap().switcher().desired.preview, input(2));

    let result = CommandResult::Accepted {
        id: "select-3".into(),
        revision: 2,
        scheduled_frame: None,
    };
    assert_eq!(
        model.reconcile_command(&result).unwrap(),
        CommandReconciled::Accepted {
            revision: Revision::new(2),
            awaiting_event: true,
        }
    );
    assert_eq!(model.pending_commands().len(), 1);

    let identity = model.reconnect_cursor().unwrap().engine.clone();
    let applied = model
        .apply_event(event(
            project_id,
            identity,
            2,
            desired(BusSelection::new(input(1), input(3))),
        ))
        .unwrap();
    assert_eq!(
        applied,
        EventApplied::Applied {
            reconciled_commands: vec![CommandId::new("select-3")],
        }
    );
    assert_eq!(model.pending_commands(), []);
    assert_eq!(model.view().unwrap().switcher.desired.preview, input(3));
}

#[test]
fn optimistic_rejection_snaps_to_authoritative_state_and_retains_reason() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 1)).unwrap();
    model
        .track_command(
            CommandId::new("select-3"),
            Some(OptimisticChange::DesiredPreview(input(3))),
        )
        .unwrap();

    let result = CommandResult::Rejected {
        id: "select-3".into(),
        code: "permission_denied".into(),
        message: "operator role required".into(),
        fields: Vec::new(),
        current_revision: 1,
        retryable: false,
        reason: None,
    };
    assert!(matches!(
        model.reconcile_command(&result).unwrap(),
        CommandReconciled::Rejected(_)
    ));
    assert_eq!(model.pending_commands(), []);
    assert_eq!(model.view().unwrap().switcher.desired.preview, input(2));
    assert_eq!(model.last_rejection().unwrap().code, "permission_denied");
}

#[test]
fn reconnect_cursor_advances_only_for_contiguous_authoritative_events() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 20)).unwrap();
    let initial = model.reconnect_cursor().unwrap().clone();
    assert_eq!(initial.protocol_cursor().revision, 20);

    model
        .track_command(CommandId::new("local-only"), None)
        .unwrap();
    assert_eq!(model.reconnect_cursor(), Some(&initial));

    model
        .apply_event(event(
            project_id,
            initial.engine.clone(),
            21,
            desired(BusSelection::new(input(2), input(3))),
        ))
        .unwrap();
    let reconnect = model.reconnect_cursor().unwrap();
    assert_eq!(reconnect.project_id, project_id);
    assert_eq!(reconnect.revision, Revision::new(21));
    assert_eq!(reconnect.engine, initial.engine);
    let protocol = model.protocol_reconnect_cursor().unwrap();
    assert_eq!(protocol.server.project_id, project_id.to_string());
    assert_eq!(protocol.revision, 21);
}

#[test]
fn runtime_realization_leaves_durable_and_command_tracking_unchanged() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    model
        .track_command(CommandId::new("pending"), None)
        .unwrap();
    model
        .reconcile_command(&CommandResult::Accepted {
            id: "pending".into(),
            revision: 8,
            scheduled_frame: None,
        })
        .unwrap();
    let cursor = model.reconnect_cursor().unwrap().clone();
    let sync_status = model.sync_status().clone();
    let pending = model.pending_commands().to_vec();

    assert_eq!(
        model
            .reduce(Action::ApplyRuntimeRealization(realization(
                project_id, identity, 7, 5, 1,
            )))
            .unwrap(),
        Reduction::RuntimeRealizationApplied(RuntimeRealizationApplied::Applied)
    );

    assert_eq!(model.reconnect_cursor(), Some(&cursor));
    assert_eq!(model.sync_status(), &sync_status);
    assert_eq!(model.pending_commands(), pending);
}

/// A retained revision realized after a newer one is a timed transition
/// completing on the native frame loop after later commands were realized
/// on its frames (Protocol 2.88): it stands for the newest realized
/// revision, whose desired state already holds the transition's target.
#[test]
fn runtime_realization_of_a_retained_revision_keeps_the_newest_realized_desired_state() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    let retained = BusSelection::new(input(2), input(3));
    let current = BusSelection::new(input(3), input(1));
    model
        .apply_event(event(project_id, identity.clone(), 8, desired(retained)))
        .unwrap();
    model
        .apply_event(event(project_id, identity.clone(), 9, desired(current)))
        .unwrap();

    model
        .apply_runtime_realization(realization(project_id, identity.clone(), 9, 5, 1))
        .unwrap();
    assert_eq!(model.state().unwrap().switcher().realized, current);

    model
        .apply_runtime_realization(realization(project_id, identity.clone(), 8, 6, 2))
        .unwrap();
    let switcher = model.state().unwrap().switcher();
    assert_eq!(switcher.realized, current);
    assert_eq!(switcher.runtime_generation, Some(6));
    assert_eq!(model.reconnect_cursor().unwrap().revision, Revision::new(9));

    // A newer revision realized afterwards is its own desired state.
    let newest = BusSelection::new(input(1), input(2));
    model
        .apply_event(event(project_id, identity.clone(), 10, desired(newest)))
        .unwrap();
    model
        .apply_runtime_realization(realization(project_id, identity, 10, 6, 3))
        .unwrap();
    assert_eq!(model.state().unwrap().switcher().realized, newest);
}

#[test]
fn runtime_realization_ordering_is_scoped_to_generation() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            8,
            desired(BusSelection::new(input(2), input(3))),
        ))
        .unwrap();

    model
        .apply_runtime_realization(realization(project_id, identity.clone(), 7, 1, 1))
        .unwrap();
    model
        .apply_runtime_realization(realization(project_id, identity.clone(), 8, 2, 1))
        .unwrap();
    assert_eq!(
        model.state().unwrap().switcher().runtime_generation,
        Some(2)
    );

    assert_eq!(
        model.apply_runtime_realization(realization(project_id, identity.clone(), 7, 1, 2,)),
        Err(ModelError::RuntimeGenerationOutOfOrder {
            current_generation: 2,
            observed_generation: 1,
        })
    );
    assert_eq!(
        model.apply_runtime_realization(realization(project_id, identity.clone(), 7, 2, 0,)),
        Err(ModelError::RuntimeOutOfOrder {
            current_sequence: 1,
            observed_sequence: 0,
        })
    );
    assert_eq!(
        model.apply_runtime_realization(realization(project_id, identity, 7, 2, 1)),
        Err(ModelError::ConflictingRuntimeSequence { sequence: 1 })
    );
}

#[test]
fn runtime_realization_rejects_unknown_revision_and_identity() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();

    assert_eq!(
        model.apply_runtime_realization(realization(project_id, identity.clone(), 8, 5, 1,)),
        Err(ModelError::UnknownDurableRevision {
            revision: Revision::new(8),
        })
    );
    assert!(matches!(
        model.apply_runtime_realization(realization(project(11), identity, 7, 5, 1,)),
        Err(ModelError::ProjectMismatch { .. })
    ));
    assert!(matches!(
        model.apply_runtime_realization(realization(
            project_id,
            engine("engine-b", 1, "log-b"),
            7,
            5,
            1,
        )),
        Err(ModelError::EngineMismatch { .. })
    ));
    assert_eq!(model.sync_status(), &SyncStatus::Current);
    assert_eq!(model.reconnect_cursor().unwrap().revision, Revision::new(7));
}

#[test]
fn runtime_realization_rejects_invalid_manual_endpoints_transactionally() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    let before = model.state().unwrap().clone();

    let mut unknown = realization(project_id, identity.clone(), 7, 5, 1);
    unknown.manual_transition = ManualTransitionStatus::Active(ActiveManualTransition {
        kind: ManualTransitionKind::Fade,
        from: input(1),
        to: input(99),
        interval_start: ManualTransitionPosition::START,
        position: ManualTransitionPosition::new(2_500).unwrap(),
    });
    assert_eq!(
        model.apply_runtime_realization(unknown),
        Err(ModelError::UnknownInput(input(99)))
    );
    assert_eq!(model.state(), Some(&before));

    let mut mismatched = realization(project_id, identity.clone(), 7, 5, 1);
    mismatched.manual_transition = ManualTransitionStatus::Active(ActiveManualTransition {
        kind: ManualTransitionKind::Fade,
        from: input(1),
        to: input(3),
        interval_start: ManualTransitionPosition::START,
        position: ManualTransitionPosition::new(2_500).unwrap(),
    });
    assert_eq!(
        model.apply_runtime_realization(mismatched),
        Err(ModelError::InvalidManualTransitionRouting)
    );
    assert_eq!(model.state(), Some(&before));

    assert_eq!(
        model.apply_runtime_realization(realization(project_id, identity, 7, 5, 1)),
        Ok(RuntimeRealizationApplied::Applied)
    );
}

#[test]
fn runtime_revision_errors_are_separate_from_durable_gaps() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 4)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();

    assert!(matches!(
        model.apply_runtime_realization(realization(project_id, identity.clone(), 6, 5, 1,)),
        Err(ModelError::UnknownDurableRevision { .. })
    ));
    assert_eq!(model.sync_status(), &SyncStatus::Current);

    assert!(matches!(
        model.apply_event(event(
            project_id,
            identity,
            6,
            desired(BusSelection::new(input(2), input(1))),
        )),
        Err(ModelError::RevisionGap { .. })
    ));
    assert!(matches!(model.sync_status(), SyncStatus::Behind { .. }));
}

#[test]
fn input_order_change_reorders_pairs_and_rejects_wrong_set_atomically() {
    let project_id = project(11);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 4)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();

    assert!(matches!(
        model.apply_event(event(
            project_id,
            identity.clone(),
            5,
            DurableChange::InputOrderChanged {
                inputs: vec![input(3), input(1), input(2)],
            },
        )),
        Ok(EventApplied::Applied { .. })
    ));
    let state = model.state().unwrap();
    assert_eq!(state.inputs(), &[input(3), input(1), input(2)]);
    assert_eq!(state.input_name(input(3)), Some("Guest"));
    assert_eq!(state.input_name(input(1)), Some("Camera"));
    assert_eq!(state.input_audio_strips(), &input_audio_strips());

    let before = state.clone();
    assert_eq!(
        model.apply_event(event(
            project_id,
            identity,
            6,
            DurableChange::InputOrderChanged {
                inputs: vec![input(3), input(1), input(99)],
            },
        )),
        Err(ModelError::InvalidInputOrder)
    );
    assert_eq!(model.state().unwrap(), &before);
}

fn stream(value: u128, name: &str, desired_running: bool) -> StreamStatus {
    StreamStatus {
        target: NonZeroU128::new(value).unwrap(),
        name: name.to_owned(),
        desired_running,
        realized: ProtocolStreamRealizedState::Stopped,
        detail: None,
    }
}

#[test]
fn snapshot_stream_validation_rejects_overflow_duplicates_and_bad_names() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);

    let mut duplicate = snapshot(project_id, 1);
    duplicate.streams = vec![stream(5, "Twitch", false), stream(5, "Again", true)];
    assert_eq!(
        model.install_snapshot(duplicate),
        Err(ModelError::DuplicateStreamTarget(
            NonZeroU128::new(5).unwrap()
        ))
    );

    let mut overflow = snapshot(project_id, 1);
    overflow.streams = (0..=MAX_STREAM_STATUSES)
        .map(|index| stream(index as u128 + 1, "Stream", false))
        .collect();
    assert_eq!(
        model.install_snapshot(overflow),
        Err(ModelError::InvalidStreamCount(MAX_STREAM_STATUSES + 1))
    );

    for bad_name in ["   ".to_owned(), "x".repeat(MAX_STREAM_NAME_BYTES + 1)] {
        let mut invalid = snapshot(project_id, 1);
        invalid.streams = vec![stream(5, &bad_name, false)];
        assert_eq!(
            model.install_snapshot(invalid),
            Err(ModelError::InvalidStreamNames)
        );
    }

    let mut valid = snapshot(project_id, 1);
    valid.streams = vec![stream(5, "Twitch", false), stream(9, "YouTube", true)];
    model.install_snapshot(valid.clone()).unwrap();
    assert_eq!(model.state().unwrap().streams(), valid.streams.as_slice());
    assert_eq!(model.view().unwrap().streams, valid.streams);
}

#[test]
fn streams_changed_events_replace_the_latest_replicated_projection() {
    let project_id = project(10);
    let mut initial = snapshot(project_id, 4);
    initial.streams = vec![stream(5, "Twitch", false), stream(9, "YouTube", false)];
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(initial).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();

    model
        .apply_event(event(
            project_id,
            identity.clone(),
            5,
            DurableChange::StreamsChanged {
                streams: vec![stream(5, "Twitch", true), stream(9, "YouTube", false)],
            },
        ))
        .unwrap();
    assert_eq!(
        model.state().unwrap().streams(),
        [stream(5, "Twitch", true), stream(9, "YouTube", false),].as_slice()
    );
    assert_eq!(model.sync_status(), &SyncStatus::Current);

    assert_eq!(
        model.apply_event(event(
            project_id,
            identity,
            6,
            DurableChange::StreamsChanged {
                streams: vec![stream(5, "Twitch", true), stream(5, "Duplicate", false),],
            },
        )),
        Err(ModelError::DuplicateStreamTarget(
            NonZeroU128::new(5).unwrap()
        ))
    );
    assert_eq!(
        model.state().unwrap().streams(),
        [stream(5, "Twitch", true), stream(9, "YouTube", false),].as_slice()
    );
}

fn stream_target_entry(value: u128, name: &str, has_key: bool) -> fm_protocol::StreamTargetEntry {
    fm_protocol::StreamTargetEntry {
        target: fm_protocol::WireStreamTargetId::new(NonZeroU128::new(value).unwrap()),
        config: fm_protocol::StreamTargetConfig {
            name: name.to_owned(),
            protocol: fm_protocol::StreamTargetProtocol::Rtmp,
            endpoint: "ingest.example/live".to_owned(),
            backup_endpoint: None,
            output: fm_protocol::WireOutputId::from_domain(output(1)),
            reconcile_on_startup: false,
            video_bitrate_kbps: 6_000,
            reconnect: fm_protocol::StreamReconnect::DEFAULT,
            srt_latency_ms: None,
        },
        has_key,
        ladder_rungs: 0,
        has_passphrase: false,
    }
}

#[test]
fn snapshot_stream_targets_install_into_state_and_view() {
    let project_id = project(10);
    let mut initial = snapshot(project_id, 1);
    initial.streams = vec![stream(5, "Twitch", false), stream(9, "YouTube", true)];
    initial.stream_targets = vec![
        stream_target_entry(5, "Twitch", true),
        stream_target_entry(9, "YouTube", false),
    ];
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(initial.clone()).unwrap();
    assert_eq!(
        model.state().unwrap().stream_targets(),
        initial.stream_targets.as_slice()
    );
    let view = model.view().unwrap();
    assert_eq!(view.stream_targets, initial.stream_targets);
    assert!(
        view.stream_target(NonZeroU128::new(5).unwrap())
            .unwrap()
            .has_key
    );
    assert!(view.stream_target(NonZeroU128::new(7).unwrap()).is_none());
}

#[test]
fn stream_target_changed_events_add_replace_and_remove_destinations() {
    let project_id = project(10);
    let mut initial = snapshot(project_id, 4);
    initial.streams = vec![stream(5, "Twitch", false)];
    initial.stream_targets = vec![stream_target_entry(5, "Twitch", true)];
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(initial).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();

    // Add: the entry is appended in authored order with the new roster.
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            5,
            DurableChange::StreamTargetChanged {
                target: NonZeroU128::new(9).unwrap(),
                entry: Some(Box::new(stream_target_entry(9, "YouTube", true))),
                streams: vec![stream(5, "Twitch", false), stream(9, "YouTube", false)],
            },
        ))
        .unwrap();
    let names = |model: &ClientModel| -> Vec<String> {
        model
            .view()
            .unwrap()
            .stream_targets
            .iter()
            .map(|entry| entry.config.name.clone())
            .collect()
    };
    assert_eq!(names(&model), ["Twitch", "YouTube"]);
    assert_eq!(model.view().unwrap().streams.len(), 2);

    // Replace in place: order is kept, the key flag follows the entry.
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            6,
            DurableChange::StreamTargetChanged {
                target: NonZeroU128::new(5).unwrap(),
                entry: Some(Box::new(stream_target_entry(5, "Twitch backup", false))),
                streams: vec![
                    stream(5, "Twitch backup", false),
                    stream(9, "YouTube", false),
                ],
            },
        ))
        .unwrap();
    assert_eq!(names(&model), ["Twitch backup", "YouTube"]);
    assert!(
        !model
            .view()
            .unwrap()
            .stream_target(NonZeroU128::new(5).unwrap())
            .unwrap()
            .has_key
    );
    assert_eq!(model.state().unwrap().streams()[0].name, "Twitch backup");

    // Remove: the entry and its roster row go.
    model
        .apply_event(event(
            project_id,
            identity,
            7,
            DurableChange::StreamTargetChanged {
                target: NonZeroU128::new(5).unwrap(),
                entry: None,
                streams: vec![stream(9, "YouTube", false)],
            },
        ))
        .unwrap();
    assert_eq!(names(&model), ["YouTube"]);
    assert_eq!(
        model.state().unwrap().streams(),
        [stream(9, "YouTube", false)].as_slice()
    );
    assert_eq!(model.sync_status(), &SyncStatus::Current);
}

/// Protocol 2.54: undoing a removal brings a destination back at its old
/// place; the mirrored list follows the event's roster order, exactly like a
/// fresh snapshot, instead of appending the returning entry.
#[test]
fn a_returning_destination_takes_its_roster_place() {
    let project_id = project(10);
    let mut initial = snapshot(project_id, 4);
    initial.streams = vec![stream(1, "A", false), stream(3, "C", false)];
    initial.stream_targets = vec![
        stream_target_entry(1, "A", true),
        stream_target_entry(3, "C", true),
    ];
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(initial).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    let ids = |model: &ClientModel| -> Vec<u128> {
        model
            .state()
            .unwrap()
            .stream_targets()
            .iter()
            .map(|entry| entry.target.get().get())
            .collect()
    };
    // B returns between A and C (the undo of its removal).
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            5,
            DurableChange::StreamTargetChanged {
                target: NonZeroU128::new(2).unwrap(),
                entry: Some(Box::new(stream_target_entry(2, "B", true))),
                streams: vec![
                    stream(1, "A", false),
                    stream(2, "B", false),
                    stream(3, "C", false),
                ],
            },
        ))
        .unwrap();
    assert_eq!(ids(&model), [1, 2, 3]);
    let view: Vec<u128> = model
        .view()
        .unwrap()
        .stream_targets
        .iter()
        .map(|entry| entry.target.get().get())
        .collect();
    assert_eq!(view, [1, 2, 3]);
    // An existing destination moved to the front follows the roster too.
    model
        .apply_event(event(
            project_id,
            identity,
            6,
            DurableChange::StreamTargetChanged {
                target: NonZeroU128::new(3).unwrap(),
                entry: Some(Box::new(stream_target_entry(3, "C", true))),
                streams: vec![
                    stream(3, "C", false),
                    stream(1, "A", false),
                    stream(2, "B", false),
                ],
            },
        ))
        .unwrap();
    assert_eq!(ids(&model), [3, 1, 2]);
}

#[test]
fn snapshot_recordings_project_per_slot_into_state_and_view() {
    let project_id = project(10);
    let inactive = snapshot(project_id, 1);
    assert_eq!(inactive.recordings, recordings());
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(inactive.clone()).unwrap();
    for recorder in [recorder_slot(1), recorder_slot(2)] {
        assert!(!model.state().unwrap().recording_desired(recorder));
        assert!(!model.view().unwrap().recording_desired(recorder));
    }

    let mut active = snapshot(project_id, 2);
    active.recordings = vec![
        RecordingStatus {
            recorder: recorder_slot(1),
            desired_active: true,
        },
        RecordingStatus {
            recorder: recorder_slot(2),
            desired_active: false,
        },
    ];
    model.install_snapshot(active.clone()).unwrap();
    assert!(model.state().unwrap().recording_desired(recorder_slot(1)));
    assert!(!model.state().unwrap().recording_desired(recorder_slot(2)));
    assert!(model.view().unwrap().recording_desired(recorder_slot(1)));
    assert!(!model.view().unwrap().recording_desired(recorder_slot(2)));
    assert_eq!(
        model.state().unwrap().recordings(),
        active.recordings.as_slice()
    );
    assert_eq!(
        model.view().unwrap().streams,
        inactive.streams,
        "recording projection is independent of the stream roster"
    );
}

#[test]
fn snapshot_recording_validation_rejects_missing_duplicate_extra_and_reordered_slots() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);

    let mut missing = snapshot(project_id, 1);
    missing.recordings.truncate(1);
    assert_eq!(
        model.install_snapshot(missing),
        Err(ModelError::InvalidRecordingCount(1))
    );

    let mut duplicate = snapshot(project_id, 1);
    duplicate.recordings[1].recorder = recorder_slot(1);
    assert_eq!(
        model.install_snapshot(duplicate),
        Err(ModelError::DuplicateRecorderSlot(1))
    );

    let mut extra = snapshot(project_id, 1);
    extra.recordings.push(RecordingStatus {
        recorder: recorder_slot(1),
        desired_active: false,
    });
    assert_eq!(
        model.install_snapshot(extra),
        Err(ModelError::InvalidRecordingCount(3))
    );

    let mut reversed = snapshot(project_id, 1);
    reversed.recordings.reverse();
    assert_eq!(
        model.install_snapshot(reversed),
        Err(ModelError::InvalidRecordingOrder)
    );
}

#[test]
fn recording_changed_events_update_only_the_addressed_slot() {
    let project_id = project(10);
    let initial = snapshot(project_id, 4);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(initial).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();

    model
        .apply_event(event(
            project_id,
            identity.clone(),
            5,
            DurableChange::RecordingChanged {
                recorder: recorder_slot(1),
                active: true,
            },
        ))
        .unwrap();
    assert!(model.state().unwrap().recording_desired(recorder_slot(1)));
    assert!(
        !model.state().unwrap().recording_desired(recorder_slot(2)),
        "slot one must not change slot two"
    );
    assert!(model.view().unwrap().recording_desired(recorder_slot(1)));
    assert_eq!(model.sync_status(), &SyncStatus::Current);

    model
        .apply_event(event(
            project_id,
            identity.clone(),
            6,
            DurableChange::RecordingChanged {
                recorder: recorder_slot(2),
                active: true,
            },
        ))
        .unwrap();
    assert!(model.state().unwrap().recording_desired(recorder_slot(1)));
    assert!(model.state().unwrap().recording_desired(recorder_slot(2)));

    model
        .apply_event(event(
            project_id,
            identity,
            7,
            DurableChange::RecordingChanged {
                recorder: recorder_slot(1),
                active: false,
            },
        ))
        .unwrap();
    assert!(!model.state().unwrap().recording_desired(recorder_slot(1)));
    assert!(
        model.state().unwrap().recording_desired(recorder_slot(2)),
        "stopping slot one must leave slot two running"
    );
}

#[test]
fn edit_locks_and_history_reduce_from_snapshots_and_events() {
    let project = project(1);
    let mut model = ClientModel::new(project);
    let mut base = snapshot(project, 4);
    base.edit_locks = EditLocks {
        project: false,
        inputs: vec![input(3)],
    };
    base.edit_history = EditHistoryStatus {
        undo_depth: 2,
        next_undo: Some(4),
        redo_depth: 0,
        next_redo: None,
    };
    model.install_snapshot(base.clone()).unwrap();
    let view = model.view().unwrap();
    assert!(view.edit_locks.is_input_locked(input(3)));
    assert!(view.edit_locks.blocks_input_edit(input(3)));
    assert!(!view.edit_locks.blocks_input_edit(input(1)));
    assert_eq!(view.edit_history.next_undo, Some(4));

    let engine = base.cursor.engine.clone();
    let mut locked = event(
        project,
        engine.clone(),
        5,
        DurableChange::EditLockChanged {
            target: EditLockTarget::Input(input(1)),
            locked: true,
        },
    );
    locked.edit_history = base.edit_history;
    model.apply_event(locked).unwrap();
    let mut project_lock = event(
        project,
        engine.clone(),
        6,
        DurableChange::EditLockChanged {
            target: EditLockTarget::Project,
            locked: true,
        },
    );
    project_lock.edit_history = base.edit_history;
    model.apply_event(project_lock).unwrap();
    let state = model.state().unwrap();
    assert_eq!(state.edit_locks().inputs, [input(1), input(3)]);
    assert!(state.edit_locks().project);
    assert!(state.edit_locks().blocks_input_edit(input(2)));

    // Any event replaces the history summary with the post-revision one.
    let mut undone = event(
        project,
        engine.clone(),
        7,
        DurableChange::InputRenamed {
            input: input(2),
            name: "Slides 2".into(),
        },
    );
    undone.edit_history = EditHistoryStatus {
        undo_depth: 1,
        next_undo: Some(2),
        redo_depth: 1,
        next_redo: Some(4),
    };
    model.apply_event(undone).unwrap();
    let mut unlocked = event(
        project,
        engine.clone(),
        8,
        DurableChange::EditLockChanged {
            target: EditLockTarget::Input(input(3)),
            locked: false,
        },
    );
    unlocked.edit_history = model.state().unwrap().edit_history();
    model.apply_event(unlocked).unwrap();
    let view = model.view().unwrap();
    assert_eq!(view.edit_locks.inputs, [input(1)]);
    assert_eq!(view.edit_history.next_redo, Some(4));
    assert!(view.edit_history.can_undo() && view.edit_history.can_redo());

    // Unknown inputs are refused for snapshots and events alike.
    let unknown = event(
        project,
        engine,
        9,
        DurableChange::EditLockChanged {
            target: EditLockTarget::Input(input(9)),
            locked: true,
        },
    );
    assert_eq!(
        model.apply_event(unknown),
        Err(ModelError::UnknownInput(input(9)))
    );
    let mut invalid = snapshot(project, 20);
    invalid.edit_locks.inputs = vec![input(2), input(1)];
    assert_eq!(
        ClientModel::new(project).install_snapshot(invalid),
        Err(ModelError::InvalidEditLocks)
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn input_added_and_removed_reduce_into_the_catalogue_strips_and_locks() {
    let project = project(1);
    let mut model = ClientModel::new(project);
    let mut base = snapshot(project, 4);
    base.edit_locks.inputs = vec![input(3)];
    // Channel 2 is off but still retains input 3 as its source.
    base.desired_overlays[1].source = Some(input(3));
    base.realized_overlays[1].source = Some(input(3));
    model.install_snapshot(base.clone()).unwrap();
    let engine = base.cursor.engine.clone();

    // Strips stay in identifier order even when the new id sorts first.
    model
        .apply_event(event(
            project,
            engine.clone(),
            5,
            DurableChange::InputAdded {
                input: input(9),
                name: "Red".into(),
                network_stream: None,
                system_audio: None,
                screen_capture: None,
                capture: None,
                still: None,
            },
        ))
        .unwrap();
    let view = model.view().unwrap();
    assert_eq!(view.inputs, [input(1), input(2), input(3), input(9)]);
    assert_eq!(view.input_names.last().map(String::as_str), Some("Red"));
    assert_eq!(
        view.input_audio_strips
            .iter()
            .map(|strip| strip.input)
            .collect::<Vec<_>>(),
        [input(1), input(2), input(3), input(9)]
    );
    assert!(view.input_audio_strips[3].follow_video && !view.input_audio_strips[3].muted);

    // The new input is switchable: a later event may route it to Program.
    model
        .apply_event(event(
            project,
            engine.clone(),
            6,
            DurableChange::DesiredSwitcher {
                selection: BusSelection::new(input(9), input(1)),
                manual_transition: ManualTransitionStatus::Inactive,
                fade_to_black: base.switcher.desired_fade_to_black,
                overlays: base.desired_overlays.clone(),
                input_audio_strips: model.state().unwrap().input_audio_strips().to_vec(),
            },
        ))
        .unwrap();
    assert_eq!(model.view().unwrap().switcher.desired.program, input(9));

    // Removal of an input on desired Program is refused; tally state stays.
    let on_program = event(
        project,
        engine.clone(),
        7,
        DurableChange::InputRemoved { input: input(9) },
    );
    assert_eq!(
        model.apply_event(on_program),
        Err(ModelError::InputInUse(input(9)))
    );
    for (change, error) in [
        (
            DurableChange::InputAdded {
                input: input(2),
                name: "Twin".into(),
                network_stream: None,
                system_audio: None,
                screen_capture: None,
                capture: None,
                still: None,
            },
            ModelError::DuplicateInput(input(2)),
        ),
        (
            DurableChange::InputAdded {
                input: input(10),
                name: "Camera".into(),
                network_stream: None,
                system_audio: None,
                screen_capture: None,
                capture: None,
                still: None,
            },
            ModelError::InvalidInputNames,
        ),
        (
            DurableChange::InputAdded {
                input: input(10),
                name: " ".into(),
                network_stream: None,
                system_audio: None,
                screen_capture: None,
                capture: None,
                still: None,
            },
            ModelError::InvalidInputNames,
        ),
        (
            DurableChange::InputRemoved { input: input(42) },
            ModelError::UnknownInput(input(42)),
        ),
    ] {
        assert_eq!(
            model.apply_event(event(project, engine.clone(), 7, change)),
            Err(error)
        );
    }

    // Removing the locked, off-overlay input drops its strip, padlock, and
    // the source the settled-off overlay retained.
    model
        .apply_event(event(
            project,
            engine.clone(),
            7,
            DurableChange::InputRemoved { input: input(3) },
        ))
        .unwrap();
    let state = model.state().unwrap();
    assert_eq!(state.inputs(), [input(1), input(2), input(9)]);
    assert_eq!(state.input_name(input(3)), None);
    assert_eq!(state.input_audio_strips().len(), 3);
    assert_eq!(state.edit_locks().inputs, Vec::new());
    assert_eq!(state.desired_overlays()[1].source, None);
    assert_eq!(state.realized_overlays()[1].source, None);

    // The protocol events convert to the same changes.
    let added = DurableProjectEvent::from_protocol(
        project,
        EventMessage {
            cursor: EventCursor {
                engine: engine.clone(),
                revision: 8,
            },
            payload: EventPayload::InputAdded {
                input: fm_protocol::WireInputId::from_domain(input(11)),
                name: "Bars".into(),
                network_stream: None,
                system_audio: None,
                screen_capture: None,
                capture: None,
                still: None,
            },
            edit_history: EditHistoryStatus::EMPTY,
        },
    );
    assert_eq!(
        added.change,
        DurableChange::InputAdded {
            input: input(11),
            name: "Bars".into(),
            network_stream: None,
            system_audio: None,
            screen_capture: None,
            capture: None,
            still: None,
        }
    );
    model.apply_event(added).unwrap();
    let removed = DurableProjectEvent::from_protocol(
        project,
        EventMessage {
            cursor: EventCursor {
                engine,
                revision: 9,
            },
            payload: EventPayload::InputRemoved {
                input: fm_protocol::WireInputId::from_domain(input(11)),
            },
            edit_history: EditHistoryStatus::EMPTY,
        },
    );
    model.apply_event(removed).unwrap();
    assert_eq!(
        model.state().unwrap().inputs(),
        [input(1), input(2), input(9)]
    );
}

mod titles {
    use fm_protocol::{
        TitleAnimationStateMessage, TitleFieldStatus, TitleFieldUpdate, TitleFieldValue,
        TitleStateMessage, WireInputId, WireTitleFieldId,
    };

    use super::*;

    const TITLE: u128 = 3;

    fn field(value: u128) -> WireTitleFieldId {
        WireTitleFieldId::new(NonZeroU128::new(value).unwrap())
    }

    fn titled_snapshot(revision: u64) -> ProjectSnapshot {
        let mut snapshot = snapshot(project(1), revision);
        snapshot.titles = vec![TitleStatus::announced(input(TITLE), 2)];
        snapshot
    }

    fn title_state(revision: u64, field_revision: u64) -> TitleStateMessage {
        TitleStateMessage {
            revision,
            input: WireInputId::from_domain(input(TITLE)),
            name: "Lower third".into(),
            field_revision,
            phase: fm_protocol::TitlePhase::In,
            phase_frame: 3,
            phase_frames: 20,
            timeline: None,
            fields: vec![
                TitleFieldStatus {
                    field: field(1),
                    name: "Name".into(),
                    value: TitleFieldValue::Text("Ana".into()),
                },
                TitleFieldStatus {
                    field: field(2),
                    name: "Score".into(),
                    value: TitleFieldValue::Number(0),
                },
            ],
        }
    }

    fn installed() -> ClientModel {
        let mut model = ClientModel::new(project(1));
        model.install_snapshot(titled_snapshot(10)).unwrap();
        model
    }

    fn completed() -> ClientModel {
        let mut model = installed();
        model.apply_title_state(title_state(10, 2)).unwrap();
        model
    }

    fn on_channel_two(source: InputId) -> Vec<OverlayStatus> {
        let mut overlays = overlays();
        overlays[1].source = Some(source);
        overlays[1].active = true;
        overlays[1].opacity = 255;
        overlays
    }

    fn title_changed(
        field_revision: u64,
        name: &str,
        overlays: Vec<OverlayStatus>,
    ) -> DurableChange {
        DurableChange::TitleChanged {
            input: input(TITLE),
            field_revision,
            fields: vec![
                TitleFieldUpdate {
                    field: field(1),
                    value: TitleFieldValue::Text(name.into()),
                },
                TitleFieldUpdate {
                    field: field(2),
                    value: TitleFieldValue::Number(7),
                },
            ],
            overlays,
        }
    }

    #[test]
    fn snapshot_installs_incomplete_summaries_that_title_states_complete() {
        let model = installed();
        let state = model.state().unwrap();
        assert_eq!(state.titles(), &[TitleStatus::announced(input(TITLE), 2)]);
        assert!(!state.titles_complete());

        let model = completed();
        let state = model.state().unwrap();
        let title = state.title(input(TITLE)).unwrap();
        assert!(title.complete && state.titles_complete());
        assert_eq!(title.name, "Lower third");
        assert_eq!(title.field_revision, 2);
        assert_eq!(title.fields, title_state(10, 2).fields);
        // Animation progress at the snapshot's frame cursor (Protocol 2.27).
        assert_eq!(
            (title.phase, title.phase_frame, title.phase_frames),
            (fm_protocol::TitlePhase::In, 3, 20)
        );

        // The protocol snapshot DTO announces the same incomplete summaries.
        let protocol = fm_protocol::SnapshotMessage {
            titles: vec![fm_protocol::TitleSummary {
                input: WireInputId::from_domain(input(TITLE)),
                field_revision: 2,
            }],
            ..edit_history_protocol_snapshot()
        };
        assert_eq!(
            ProjectSnapshot::from_protocol(project(1), protocol).titles,
            vec![TitleStatus::announced(input(TITLE), 2)]
        );
    }

    fn edit_history_protocol_snapshot() -> fm_protocol::SnapshotMessage {
        let wire = |value| WireInputId::from_domain(input(value));
        fm_protocol::SnapshotMessage {
            audio_mixer: fm_protocol::AudioMixerStatus::default(),
            fade_to_black_excluded_outputs: Vec::new(),
            fade_to_black_output_durations: Vec::new(),
            fade_to_black_curve: fm_protocol::FadeToBlackCurve::default(),
            display_outputs: Vec::new(),
            display_output_states: Vec::new(),
            network_streams: Vec::new(),
            system_audio_inputs: Vec::new(),
            screen_capture_inputs: Vec::new(),
            capture_inputs: Vec::new(),
            engine: engine("engine-a", 1, "log-a"),
            revision: 10,
            show_name: "Show".into(),
            inputs: (1..=3)
                .map(|value| fm_protocol::InputStatus {
                    input: wire(value),
                    name: format!("Input {value}"),
                    lifecycle: fm_protocol::InputLifecycleStatus::default(),
                })
                .collect(),
            outputs: Vec::new(),
            input_audio_strips: Vec::new(),
            desired_program: wire(1),
            desired_preview: wire(2),
            realized_program: wire(1),
            realized_preview: wire(2),
            desired_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            realized_manual_transition: fm_protocol::ManualTransitionStatus::Inactive,
            desired_fade_to_black: fade_to_black(false, 0),
            realized_fade_to_black: fade_to_black(false, 0),
            stingers: Vec::new(),
            streams: Vec::new(),
            recordings: recordings(),
            desired_overlays: fm_protocol::OverlayStatus::empty_channels(),
            realized_overlays: fm_protocol::OverlayStatus::empty_channels(),
            edit_locks: fm_protocol::EditLockStatus::default(),
            edit_history: EditHistoryStatus::EMPTY,
            titles: Vec::new(),
            scenes: Vec::new(),
            layout_scenes: Vec::new(),
            frame_rate: fm_types::FrameRate::new(30, 1).unwrap(),
            playlist_follows: false,
            grade_states: 0,
            animation_preset_states: 0,
            media_states: 0,
            paged_states: 0,
            cue_list_states: 0,
            script_states: 0,
            shortcut_states: 0,
            still_states: 0,
            media_list_states: 0,
            input_categories: Vec::new(),
            transition_favorites: fm_protocol::TransitionFavoriteStatus::defaults(),
            stream_targets: Vec::new(),
            live_lan: fm_protocol::LiveLanEntry::default(),
            live_lan_status: fm_protocol::LiveLanStatus::default(),
        }
    }

    #[test]
    fn mismatched_title_states_are_refused_without_change() {
        let mut stale_revision = title_state(9, 2);
        let wrong_field_revision = title_state(10, 3);
        let mut unknown = title_state(10, 2);
        unknown.input = WireInputId::from_domain(input(1));
        let mut unordered = title_state(10, 2);
        unordered.fields.reverse();
        for message in [
            stale_revision.clone(),
            wrong_field_revision,
            unknown,
            unordered,
        ] {
            let mut model = installed();
            let input = message.input.to_domain();
            assert_eq!(
                model.apply_title_state(message),
                Err(ModelError::TitleStateMismatch(input))
            );
            assert!(!model.state().unwrap().titles_complete());
        }
        // A title completes once.
        let mut model = completed();
        assert_eq!(
            model.apply_title_state(title_state(10, 2)),
            Err(ModelError::TitleStateMismatch(input(TITLE)))
        );
        // No snapshot, no titles.
        stale_revision.revision = 10;
        assert_eq!(
            ClientModel::new(project(1)).apply_title_state(stale_revision),
            Err(ModelError::StateUnavailable)
        );
        // The reducer surface dispatches the same operation.
        let mut model = installed();
        assert_eq!(
            model.reduce(Action::ApplyTitleState(title_state(10, 2))),
            Ok(Reduction::TitleStateApplied)
        );
    }

    #[test]
    fn title_changed_merges_values_and_replaces_desired_overlays() {
        let mut model = completed();
        let engine_a = engine("engine-a", 1, "log-a");
        assert!(!model.state().unwrap().title_on_air(input(TITLE)));
        model
            .apply_event(event(
                project(1),
                engine_a.clone(),
                11,
                title_changed(3, "Bea", on_channel_two(input(TITLE))),
            ))
            .unwrap();
        let state = model.state().unwrap();
        let title = state.title(input(TITLE)).unwrap();
        assert_eq!(title.field_revision, 3);
        assert_eq!(title.fields[0].name, "Name", "names are kept");
        assert_eq!(title.fields[0].value, TitleFieldValue::Text("Bea".into()));
        assert_eq!(title.fields[1].value, TitleFieldValue::Number(7));
        assert_eq!(
            state.desired_overlays(),
            on_channel_two(input(TITLE)).as_slice()
        );
        assert!(state.title_on_air(input(TITLE)));
        assert!(!state.title_on_air(input(2)));
        // Program is on air too.
        assert!(state.title_on_air(input(1)));

        // A fading-out overlay keeps its source on air until opacity is zero.
        let mut fading = on_channel_two(input(TITLE));
        fading[1].active = false;
        fading[1].opacity = 40;
        model
            .apply_event(event(
                project(1),
                engine_a.clone(),
                12,
                title_changed(3, "Bea", fading),
            ))
            .unwrap();
        assert!(model.state().unwrap().title_on_air(input(TITLE)));
    }

    #[test]
    fn invalid_title_changes_are_refused_atomically() {
        let engine_a = engine("engine-a", 1, "log-a");
        let mut regressed = completed();
        assert_eq!(
            regressed.apply_event(event(
                project(1),
                engine_a.clone(),
                11,
                title_changed(1, "Bea", overlays()),
            )),
            Err(ModelError::InvalidTitleChange(input(TITLE)))
        );
        let mut missing_field = completed();
        let DurableChange::TitleChanged {
            input: title,
            field_revision,
            mut fields,
            overlays: channels,
        } = title_changed(3, "Bea", overlays())
        else {
            unreachable!();
        };
        fields.pop();
        assert_eq!(
            missing_field.apply_event(event(
                project(1),
                engine_a.clone(),
                11,
                DurableChange::TitleChanged {
                    input: title,
                    field_revision,
                    fields,
                    overlays: channels,
                },
            )),
            Err(ModelError::InvalidTitleChange(input(TITLE)))
        );
        let mut incomplete = installed();
        assert_eq!(
            incomplete.apply_event(event(
                project(1),
                engine_a.clone(),
                11,
                title_changed(3, "Bea", overlays()),
            )),
            Err(ModelError::TitleIncomplete(input(TITLE)))
        );
        let mut not_a_title = completed();
        assert_eq!(
            not_a_title.apply_event(event(
                project(1),
                engine_a.clone(),
                11,
                DurableChange::TitleChanged {
                    input: input(2),
                    field_revision: 1,
                    fields: Vec::new(),
                    overlays: overlays(),
                },
            )),
            Err(ModelError::UnknownTitle(input(2)))
        );
        let mut bad_overlay = completed();
        assert_eq!(
            bad_overlay.apply_event(event(
                project(1),
                engine_a,
                11,
                title_changed(3, "Bea", on_channel_two(input(99))),
            )),
            Err(ModelError::UnknownInput(input(99)))
        );
        for model in [regressed, missing_field, bad_overlay] {
            let state = model.state().unwrap();
            assert_eq!(state.title(input(TITLE)).unwrap().field_revision, 2);
            assert_eq!(
                model.reconnect_cursor().unwrap().revision,
                Revision::new(10)
            );
        }
    }

    #[test]
    fn snapshot_titles_must_be_ascending_show_inputs() {
        for titles in [
            vec![
                TitleStatus::announced(input(3), 0),
                TitleStatus::announced(input(2), 0),
            ],
            vec![
                TitleStatus::announced(input(3), 0),
                TitleStatus::announced(input(3), 0),
            ],
            vec![TitleStatus::announced(input(9), 0)],
        ] {
            let mut snapshot = snapshot(project(1), 10);
            snapshot.titles = titles;
            assert_eq!(
                ClientModel::new(project(1)).install_snapshot(snapshot),
                Err(ModelError::InvalidTitles)
            );
        }
    }

    #[test]
    fn removing_a_title_input_drops_its_title() {
        let mut model = completed();
        let mut snapshot_inputs = model.state().unwrap().inputs().to_vec();
        snapshot_inputs.retain(|candidate| *candidate != input(TITLE));
        model
            .apply_event(event(
                project(1),
                engine("engine-a", 1, "log-a"),
                11,
                DurableChange::InputRemoved {
                    input: input(TITLE),
                },
            ))
            .unwrap();
        let state = model.state().unwrap();
        assert_eq!(state.titles(), &[] as &[crate::TitleStatus]);
        assert_eq!(state.inputs(), snapshot_inputs.as_slice());
    }

    fn animation_clip(in_frames: u32) -> fm_protocol::WireTitleClip {
        use fm_animation::{Easing, Keyframe, Phase, Property, Timebase, Track, TrackKey, Value};
        fm_protocol::WireTitleClip::from_tracks(
            Timebase::new(fm_types::FrameRate::new(30, 1).unwrap()),
            in_frames,
            0,
            vec![Track::new(
                TrackKey::new(
                    fm_protocol::WireTitleElementId::new(NonZeroU128::new(1).unwrap()),
                    Phase::In,
                    Property::Opacity,
                ),
                vec![
                    Keyframe::new(0, Value::Scalar(0), Easing::Linear),
                    Keyframe::new(in_frames, Value::Scalar(255), Easing::Linear),
                ],
            )],
        )
        .unwrap()
    }

    fn animation_state(revision: u64, animation_revision: u64) -> TitleAnimationStateMessage {
        TitleAnimationStateMessage {
            revision,
            input: WireInputId::from_domain(input(TITLE)),
            animation_revision,
            clip: Box::new(animation_clip(10)),
        }
    }

    fn animation_changed(animation_revision: u64, in_frames: u32) -> DurableChange {
        DurableChange::TitleAnimationChanged {
            inputs: vec![input(TITLE)],
            animation: crate::TitleAnimationStatus {
                revision: animation_revision,
                clip: std::sync::Arc::new(animation_clip(in_frames)),
            },
        }
    }

    #[test]
    fn animation_states_complete_titles_and_changes_advance_them() {
        let mut model = completed();
        assert!(!model.state().unwrap().title_animations_complete());
        // Wrong revision, unknown title: refused without change.
        let mut unknown = animation_state(10, 4);
        unknown.input = WireInputId::from_domain(input(1));
        for message in [animation_state(9, 4), unknown] {
            let input = message.input.to_domain();
            assert_eq!(
                model.apply_title_animation_state(message),
                Err(ModelError::TitleStateMismatch(input))
            );
        }
        // A change before the animation arrived is refused.
        let engine_a = engine("engine-a", 1, "log-a");
        assert_eq!(
            model.apply_event(event(
                project(1),
                engine_a.clone(),
                11,
                animation_changed(5, 12)
            )),
            Err(ModelError::TitleIncomplete(input(TITLE)))
        );

        model
            .apply_title_animation_state(animation_state(10, 4))
            .unwrap();
        assert_eq!(
            model.apply_title_animation_state(animation_state(10, 4)),
            Err(ModelError::TitleStateMismatch(input(TITLE))),
            "an animation arrives once"
        );
        let state = model.state().unwrap();
        assert!(state.title_animations_complete());
        let animation = state
            .title(input(TITLE))
            .unwrap()
            .animation
            .clone()
            .unwrap();
        assert_eq!(animation.revision, 4);
        assert_eq!(*animation.clip, animation_clip(10));

        // A change must advance the revision.
        assert_eq!(
            model.apply_event(event(
                project(1),
                engine_a.clone(),
                11,
                animation_changed(4, 12)
            )),
            Err(ModelError::InvalidTitleChange(input(TITLE)))
        );
        model
            .apply_event(event(project(1), engine_a, 11, animation_changed(5, 12)))
            .unwrap();
        let animation = model
            .state()
            .unwrap()
            .title(input(TITLE))
            .unwrap()
            .animation
            .clone()
            .unwrap();
        assert_eq!(animation.revision, 5);
        assert_eq!(*animation.clip, animation_clip(12));
    }

    #[test]
    fn layout_states_follow_the_animation_states_once_per_title() {
        use fm_protocol::{TitleLayoutStateMessage, WireTitleLayout};

        let layout_state = |revision: u64| TitleLayoutStateMessage {
            revision,
            input: WireInputId::from_domain(input(TITLE)),
            layout: Box::new(WireTitleLayout {
                width: 1_920,
                height: 1_080,
                elements: Vec::new(),
            }),
        };
        let mut model = completed();
        assert!(!model.state().unwrap().title_layouts_complete());
        // Before the title's animation state it is refused.
        assert_eq!(
            model.apply_title_layout_state(layout_state(10)),
            Err(ModelError::TitleStateMismatch(input(TITLE)))
        );
        model
            .apply_title_animation_state(animation_state(10, 4))
            .unwrap();
        // Wrong revision, unknown title: refused without change.
        let mut unknown = layout_state(10);
        unknown.input = WireInputId::from_domain(input(1));
        for message in [layout_state(9), unknown] {
            let input = message.input.to_domain();
            assert_eq!(
                model.apply_title_layout_state(message),
                Err(ModelError::TitleStateMismatch(input))
            );
        }
        assert!(!model.state().unwrap().title_layouts_complete());
        model.apply_title_layout_state(layout_state(10)).unwrap();
        assert_eq!(
            model.apply_title_layout_state(layout_state(10)),
            Err(ModelError::TitleStateMismatch(input(TITLE))),
            "a layout arrives once"
        );
        let state = model.state().unwrap();
        assert!(state.title_layouts_complete());
        assert_eq!(
            state.title(input(TITLE)).unwrap().layout.as_deref(),
            Some(&*layout_state(10).layout)
        );
    }

    #[test]
    fn the_document_adapter_follows_the_authority_and_builds_requests() {
        use crate::animation_editor::{
            AnimDocument, ExactPreviewRequest, PreviewKey, PreviewRequestId,
        };
        use crate::title_animation::{
            AuthoritySync, preview_request, preview_request_id, preview_wire_id, save_payload,
            sync_document,
        };
        use fm_animation::Phase;
        use fm_protocol::{CommandPayload, TitlePreviewSample};

        let status = |revision, in_frames| crate::TitleAnimationStatus {
            revision,
            clip: std::sync::Arc::new(animation_clip(in_frames)),
        };
        let mut document = AnimDocument::new(animation_clip(10), 4, 32, 18);
        assert_eq!(
            sync_document(&mut document, &status(4, 10)),
            AuthoritySync::Unchanged
        );
        assert_eq!(save_payload(&document, input(TITLE)), None);
        assert_eq!(
            sync_document(&mut document, &status(5, 12)),
            AuthoritySync::Reset
        );
        assert_eq!(document.base_revision(), 5);
        assert_eq!(document.clip(), &animation_clip(12));

        // A local edit makes the draft dirty: it saves and previews as a draft.
        document.extend_phase(Phase::In, 20).unwrap();
        assert!(document.is_dirty());
        let draft = document.clip().clone();
        assert_eq!(
            save_payload(&document, input(TITLE)),
            Some(CommandPayload::SetTitleAnimation {
                input: WireInputId::from_domain(input(TITLE)),
                base_revision: 5,
                clip: Box::new(draft.clone()),
            })
        );
        let request = ExactPreviewRequest {
            id: PreviewRequestId(7),
            key: PreviewKey {
                generation: document.generation(),
                phase: Phase::Out,
                frame: 0,
            },
        };
        let wire = preview_request(&document, input(TITLE), request, 320, 180);
        assert_eq!(wire.request_id, "anim-preview-7");
        assert_eq!(wire.sample, TitlePreviewSample::Out { frame: 0 });
        assert_eq!(wire.clip.as_deref(), Some(&draft));
        assert_eq!((wire.width, wire.height), (320, 180));
        assert_eq!(
            fm_protocol::decode_line(
                &fm_protocol::encode_line(&fm_protocol::WireMessage::TitlePreviewRequest(
                    wire.clone()
                ))
                .unwrap()
            )
            .unwrap(),
            fm_protocol::WireMessage::TitlePreviewRequest(wire)
        );
        assert_eq!(
            preview_request_id(&preview_wire_id(PreviewRequestId(7))),
            Some(PreviewRequestId(7))
        );
        assert_eq!(preview_request_id("anim-preview-07"), None);
        assert_eq!(preview_request_id("diagnostics-1"), None);

        // Another editor's change conflicts with the dirty draft; our own
        // save coming back marks it saved.
        assert_eq!(
            sync_document(&mut document, &status(6, 30)),
            AuthoritySync::Conflict
        );
        assert!(document.is_dirty());
        assert_eq!(document.base_revision(), 5);
        let saved = crate::TitleAnimationStatus {
            revision: 7,
            clip: std::sync::Arc::new(draft),
        };
        assert_eq!(sync_document(&mut document, &saved), AuthoritySync::Saved);
        assert!(!document.is_dirty());
        assert_eq!(document.base_revision(), 7);
    }
}

#[test]
fn playlist_changed_replicates_the_playlist_and_desired_selection() {
    use fm_protocol::{PlaylistEntrySpec, PlaylistLoopMode, PlaylistRunState, WireInputId};

    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    // A snapshot that announces no playlist implies the empty one.
    assert_eq!(
        model.state().unwrap().playlist(),
        Some(&PlaylistStatus::empty())
    );

    let entry = |input_value: u128| PlaylistEntrySpec {
        id: 4,
        input: WireInputId::from_domain(input(input_value)),
        duration_frames: 50,
        transition: fm_protocol::PlaylistTransitionKind::Fade,
        transition_frames: 5,
        enabled: true,
    };
    let message = |revision: u64, entry_input: u128| EventMessage {
        cursor: EventCursor {
            engine: identity.clone(),
            revision,
        },
        payload: EventPayload::PlaylistChanged {
            frame: 120,
            loop_mode: PlaylistLoopMode::All,
            entries: vec![entry(entry_input)],
            state: PlaylistRunState::Playing {
                entry: 4,
                take: 1,
                cycle: 0,
                started_frame: 120,
            },
            program: WireInputId::from_domain(input(3)),
            preview: WireInputId::from_domain(input(1)),
        },
        edit_history: EditHistoryStatus::EMPTY,
    };

    // An entry naming an unknown input is refused without applying.
    let unknown = DurableProjectEvent::from_protocol(project_id, message(8, 99));
    assert_eq!(
        model.apply_event(unknown),
        Err(ModelError::UnknownInput(input(99)))
    );
    assert_eq!(
        model.state().unwrap().playlist(),
        Some(&PlaylistStatus::empty())
    );

    let changed = DurableProjectEvent::from_protocol(project_id, message(8, 3));
    model.apply_event(changed).unwrap();
    let state = model.state().unwrap();
    assert_eq!(
        state.playlist(),
        Some(&PlaylistStatus {
            frame: 120,
            loop_mode: PlaylistLoopMode::All,
            entries: vec![entry(3)],
            state: PlaylistRunState::Playing {
                entry: 4,
                take: 1,
                cycle: 0,
                started_frame: 120,
            },
        })
    );
    assert_eq!(
        state.switcher().desired,
        BusSelection::new(input(3), input(1))
    );
    assert_eq!(
        state.switcher().realized,
        BusSelection::new(input(1), input(2))
    );
}

#[test]
fn an_announced_playlist_state_completes_the_snapshot() {
    use fm_protocol::{
        PlaylistEntrySpec, PlaylistLoopMode, PlaylistRunState, PlaylistStateMessage,
        PlaylistTransitionKind, WireInputId,
    };

    let project_id = project(10);
    let message = |revision: u64, entry_input: u128| PlaylistStateMessage {
        revision,
        frame: 400,
        loop_mode: PlaylistLoopMode::One,
        entries: vec![PlaylistEntrySpec {
            id: 2,
            input: WireInputId::from_domain(input(entry_input)),
            duration_frames: 250,
            transition: PlaylistTransitionKind::Cut,
            transition_frames: 0,
            enabled: true,
        }],
        state: PlaylistRunState::Paused {
            entry: 2,
            take: 3,
            cycle: 1,
            elapsed_frames: 100,
        },
    };
    let mut model = ClientModel::new(project_id);
    assert_eq!(
        model.apply_playlist_state(message(7, 1)),
        Err(ModelError::StateUnavailable)
    );
    let mut announced = snapshot(project_id, 7);
    announced.playlist_follows = true;
    model.install_snapshot(announced).unwrap();
    let state = model.state().unwrap();
    assert!(!state.playlist_complete());
    assert_eq!(state.playlist(), None);
    assert_eq!(state.frame_rate(), fm_types::FrameRate::new(25, 1).unwrap());

    assert_eq!(
        model.apply_playlist_state(message(6, 1)),
        Err(ModelError::PlaylistStateMismatch)
    );
    assert_eq!(
        model.apply_playlist_state(message(7, 99)),
        Err(ModelError::UnknownInput(input(99)))
    );
    assert!(!model.state().unwrap().playlist_complete());
    assert_eq!(
        model.reduce(Action::ApplyPlaylistState(message(7, 3))),
        Ok(Reduction::PlaylistStateApplied)
    );
    let playlist = model.state().unwrap().playlist().unwrap();
    assert_eq!(playlist, &PlaylistStatus::from_protocol(message(7, 3)));
    assert_eq!(playlist.frame, 400);
    // Only the announced one completes the snapshot.
    assert_eq!(
        model.apply_playlist_state(message(7, 3)),
        Err(ModelError::PlaylistStateMismatch)
    );
    assert_eq!(
        ModelError::PlaylistStateMismatch.to_string(),
        "playlist state does not match the snapshot it follows"
    );
}

fn preset_entry(preset: u32, name: &str) -> fm_protocol::AnimationPresetEntry {
    use fm_animation::{
        Easing, Keyframe, MotionClip, MotionTarget, Phase, PhaseMotion, Property, Timebase, Track,
        TrackKey, Value,
    };
    let clip = MotionClip::from_tracks(
        Timebase::new(fm_types::FrameRate::new(25, 1).unwrap()),
        10,
        0,
        vec![Track::new(
            TrackKey::new(MotionTarget, Phase::In, Property::Opacity),
            vec![
                Keyframe::new(0, Value::Scalar(0), Easing::Linear),
                Keyframe::new(10, Value::Scalar(255), Easing::Linear),
            ],
        )],
    )
    .unwrap();
    fm_protocol::AnimationPresetEntry {
        preset: core::num::NonZeroU32::new(preset).unwrap(),
        name: name.into(),
        phase: Phase::In,
        motion: Box::new(PhaseMotion::new(clip, 100, 100).unwrap()),
    }
}

#[test]
#[allow(clippy::too_many_lines)] // one golden: records, then events
fn animation_preset_states_and_changes_replicate_the_library() {
    use fm_protocol::{AnimationPresetChange, AnimationPresetStateMessage};
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    let mut announced = snapshot(project_id, 7);
    announced.animation_preset_states = 2;
    model.install_snapshot(announced).unwrap();
    assert!(!model.state().unwrap().animation_presets_complete());
    let record = |revision, preset, name| AnimationPresetStateMessage {
        revision,
        entry: preset_entry(preset, name),
    };

    // Records at another revision, or repeating an id or name, are refused.
    for refused in [record(6, 1, "Fade"), record(8, 1, "Fade")] {
        assert_eq!(
            model.apply_animation_preset_state(refused).unwrap_err(),
            ModelError::AnimationPresetStateMismatch
        );
    }
    model
        .apply_animation_preset_state(record(7, 4, "Fade"))
        .unwrap();
    for refused in [record(7, 4, "Other"), record(7, 5, "Fade")] {
        assert_eq!(
            model.apply_animation_preset_state(refused).unwrap_err(),
            ModelError::AnimationPresetStateMismatch
        );
    }
    model
        .apply_animation_preset_state(record(7, 2, "Swoosh"))
        .unwrap();
    assert!(model.state().unwrap().animation_presets_complete());
    assert_eq!(
        model
            .apply_animation_preset_state(record(7, 9, "Extra"))
            .unwrap_err(),
        ModelError::AnimationPresetStateMismatch
    );
    let presets = model.state().unwrap().animation_presets();
    assert_eq!(
        presets
            .entries()
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["Fade", "Swoosh"]
    );
    assert_eq!(presets.next_id().unwrap().get(), 5);

    // An addition at index 1, a rename in place, and a removal apply.
    let engine = engine("engine-a", 1, "log-a");
    let change = |revision, change| {
        event(
            project_id,
            engine.clone(),
            revision,
            DurableChange::AnimationPresetChanged(change),
        )
    };
    model
        .apply_event(change(
            8,
            AnimationPresetChange::Set {
                index: 1,
                entry: preset_entry(5, "Pop"),
            },
        ))
        .unwrap();
    model
        .apply_event(change(
            9,
            AnimationPresetChange::Set {
                index: 0,
                entry: preset_entry(4, "Soft fade"),
            },
        ))
        .unwrap();
    model
        .apply_event(change(
            10,
            AnimationPresetChange::Removed {
                preset: core::num::NonZeroU32::new(2).unwrap(),
            },
        ))
        .unwrap();
    let names = |model: &ClientModel| {
        model
            .state()
            .unwrap()
            .animation_presets()
            .entries()
            .iter()
            .map(|entry| (entry.preset.get(), entry.name.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&model),
        [(4, "Soft fade".to_owned()), (5, "Pop".to_owned())]
    );

    // Changes that contradict the library are refused and leave it intact:
    // a rename that moves a preset, a duplicate name, an index past the
    // end, a missing removal.
    for refused in [
        AnimationPresetChange::Set {
            index: 1,
            entry: preset_entry(4, "Moved"),
        },
        AnimationPresetChange::Set {
            index: 2,
            entry: preset_entry(6, "Pop"),
        },
        AnimationPresetChange::Set {
            index: 3,
            entry: preset_entry(6, "Late"),
        },
        AnimationPresetChange::Removed {
            preset: core::num::NonZeroU32::new(2).unwrap(),
        },
    ] {
        let mut copy = model.clone();
        assert!(
            copy.apply_event(change(11, refused.clone())).is_err(),
            "{refused:?}"
        );
        assert_eq!(names(&copy), names(&model));
    }
}

#[test]
fn animation_preset_ids_wrap_past_the_highest_id() {
    // Any client may name any preset id, `u32::MAX` included; the next id
    // then wraps to the lowest free one, as the project model allocates.
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    let mut announced = snapshot(project_id, 7);
    announced.animation_preset_states = 2;
    model.install_snapshot(announced).unwrap();
    for (preset, name) in [(u32::MAX, "Last"), (1, "First")] {
        model
            .apply_animation_preset_state(fm_protocol::AnimationPresetStateMessage {
                revision: 7,
                entry: preset_entry(preset, name),
            })
            .unwrap();
    }
    let presets = model.state().unwrap().animation_presets();
    assert_eq!(presets.next_id().map(core::num::NonZeroU32::get), Some(2));
}

#[test]
fn transition_favourites_replicate_from_snapshot_and_change_events() {
    use fm_protocol::{AutoTransitionKind, TransitionFavoriteEffect, WireStingerSlotId};
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    let mut announced = snapshot(project_id, 7);
    announced.transition_favorites[2] = TransitionFavoriteStatus {
        effect: TransitionFavoriteEffect::Cut,
        duration_frames: 1,
    };
    model.install_snapshot(announced).unwrap();
    let view = model.view().unwrap();
    assert_eq!(
        view.transition_favorites.map(|favorite| favorite.effect),
        [
            TransitionFavoriteEffect::Mix(AutoTransitionKind::Fade),
            TransitionFavoriteEffect::Mix(AutoTransitionKind::Wipe),
            TransitionFavoriteEffect::Cut,
            TransitionFavoriteEffect::Mix(AutoTransitionKind::Cube),
        ]
    );

    let engine = engine("engine-a", 1, "log-a");
    let stinger = TransitionFavoriteStatus {
        effect: TransitionFavoriteEffect::Stinger(WireStingerSlotId::new(3).unwrap()),
        duration_frames: 45,
    };
    model
        .apply_event(event(
            project_id,
            engine.clone(),
            8,
            DurableChange::TransitionFavoriteChanged {
                slot: 4,
                favorite: stinger,
            },
        ))
        .unwrap();
    assert_eq!(model.view().unwrap().transition_favorites[3], stinger);

    // A slot outside 1 through 4 is refused and changes nothing.
    for slot in [0, 5] {
        assert_eq!(
            model
                .apply_event(event(
                    project_id,
                    engine.clone(),
                    9,
                    DurableChange::TransitionFavoriteChanged {
                        slot,
                        favorite: stinger,
                    },
                ))
                .unwrap_err(),
            ModelError::InvalidTransitionFavorite(slot)
        );
    }
    assert_eq!(
        ModelError::InvalidTransitionFavorite(5).to_string(),
        "favourite transition change names slot 5, not 1 through 4"
    );
    assert_eq!(model.view().unwrap().transition_favorites[3], stinger);
}

/// IN-026, Protocol 2.38: lifecycle edits reduce into the replicated input
/// labels and catalogue; a duplicate copies its source's strip and labels;
/// availability is runtime state; malformed changes are refused.
#[test]
#[allow(clippy::too_many_lines)] // one scenario across every lifecycle edit
fn input_lifecycle_changes_reduce_into_labels_catalogue_and_availability() {
    use fm_protocol::{InputColorLabel, InputLifecycleOperation as Operation};
    let wire = fm_protocol::WireInputId::from_domain;
    let project = project(1);
    let mut model = ClientModel::new(project);
    let base = snapshot(project, 4);
    model.install_snapshot(base.clone()).unwrap();
    let engine = base.cursor.engine.clone();
    let mut revision = 4;
    let mut apply = |model: &mut ClientModel, operation: Operation, added: Vec<(InputId, &str)>| {
        revision += 1;
        model.apply_event(event(
            project,
            engine.clone(),
            revision,
            DurableChange::InputLifecycleChanged {
                operation,
                added: added
                    .into_iter()
                    .map(|(input, name)| (input, name.to_owned()))
                    .collect(),
            },
        ))
    };
    apply(
        &mut model,
        Operation::SetCategories {
            categories: vec![InputCategoryStatus {
                id: 5,
                name: "Cameras".into(),
            }],
        },
        Vec::new(),
    )
    .unwrap();
    apply(
        &mut model,
        Operation::SetCategory {
            input: wire(input(3)),
            category: Some(5),
        },
        Vec::new(),
    )
    .unwrap();
    apply(
        &mut model,
        Operation::SetColor {
            input: wire(input(3)),
            color: Some(InputColorLabel::Blue),
        },
        Vec::new(),
    )
    .unwrap();
    apply(
        &mut model,
        Operation::SetPaused {
            input: wire(input(3)),
            paused: true,
        },
        Vec::new(),
    )
    .unwrap();
    // An unknown category, and a category list dropping one in use, are
    // refused without changing the model.
    assert_eq!(
        apply(
            &mut model,
            Operation::SetCategory {
                input: wire(input(2)),
                category: Some(6),
            },
            Vec::new(),
        ),
        Err(ModelError::InvalidInputLifecycleChange)
    );
    let view = model.view().unwrap();
    assert_eq!(view.input_categories.len(), 1);
    let three = view.lifecycle(input(3));
    assert_eq!(
        (three.category, three.color, three.paused, three.closed),
        (Some(5), Some(InputColorLabel::Blue), true, false)
    );

    // A duplicate is appended with the source's strip and labels.
    model
        .apply_event(event(
            project,
            engine.clone(),
            9,
            DurableChange::InputLifecycleChanged {
                operation: Operation::Duplicate {
                    source: wire(input(3)),
                    input: wire(input(8)),
                    name: None,
                },
                added: vec![(input(8), "Guest 2".into())],
            },
        ))
        .unwrap();
    let view = model.view().unwrap();
    assert_eq!(view.inputs.last(), Some(&input(8)));
    assert_eq!(view.input_names.last().map(String::as_str), Some("Guest 2"));
    let copy = view.lifecycle(input(8));
    assert_eq!(
        (copy.category, copy.color, copy.paused),
        (Some(5), Some(InputColorLabel::Blue), false)
    );
    assert_eq!(view.input_lifecycle.len(), view.inputs.len());

    // A batch import appends every added input; a removal drops them.
    model
        .apply_event(event(
            project,
            engine.clone(),
            10,
            DurableChange::InputLifecycleChanged {
                operation: Operation::Import { items: Vec::new() },
                added: vec![(input(11), "Clip".into()), (input(12), "Clip 2".into())],
            },
        ))
        .unwrap();
    assert_eq!(model.view().unwrap().inputs.len(), 6);
    model
        .apply_runtime_input_availability(
            input(11),
            Some(fm_protocol::InputOfflineReason::DecodeFailed),
        )
        .unwrap();
    assert_eq!(
        model.view().unwrap().lifecycle(input(11)).offline,
        Some(fm_protocol::InputOfflineReason::DecodeFailed)
    );
    assert!(
        model
            .apply_runtime_input_availability(input(99), None)
            .is_err()
    );
    model
        .apply_event(event(
            project,
            engine,
            11,
            DurableChange::InputLifecycleChanged {
                operation: Operation::RemoveInputs {
                    inputs: vec![wire(input(11)), wire(input(12))],
                },
                added: Vec::new(),
            },
        ))
        .unwrap();
    let view = model.view().unwrap();
    assert_eq!(view.inputs, [input(1), input(2), input(3), input(8)]);
    assert!(
        view.input_lifecycle
            .iter()
            .all(|status| status.offline.is_none())
    );
}

/// Protocol 2.40 (AU-001, SW-004): mixer labels, groups, and the
/// Fade-to-Black output selection replicate from the snapshot and events.
#[test]
fn replicates_mixer_labels_groups_and_fade_to_black_outputs() {
    use fm_protocol::{AudioMixerOperation, WireInputId};

    let project_id = project(12);
    let mut model = ClientModel::new(project_id);
    let mut initial = snapshot(project_id, 1);
    initial.fade_to_black_excluded_outputs = vec![output(2)];
    initial.audio_mixer.labels = vec![fm_protocol::InputAudioLabel {
        input: WireInputId::from_domain(input(1)),
        label: "Host".into(),
    }];
    model.install_snapshot(initial).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    let view = model.view().unwrap();
    assert_eq!(view.audio_label(input(1)), "Host");
    assert!(!view.fade_to_black_affects(output(2)));
    assert!(view.fade_to_black_affects(output(1)));

    let changes = [
        DurableChange::AudioMixerChanged(AudioMixerOperation::SetGroup {
            input: WireInputId::from_domain(input(3)),
            group: Some(2),
        }),
        DurableChange::AudioMixerChanged(AudioMixerOperation::SetLabel {
            input: WireInputId::from_domain(input(3)),
            label: "Guest".into(),
        }),
        DurableChange::AudioMixerChanged(AudioMixerOperation::SetGroupLevel {
            group: 2,
            gain_millidb: -6_000,
            muted: true,
        }),
        DurableChange::FadeToBlackOutputChanged {
            output: output(1),
            affected: false,
        },
        DurableChange::FadeToBlackOutputChanged {
            output: output(2),
            affected: true,
        },
    ];
    for (revision, change) in (2..).zip(changes) {
        model
            .apply_event(event(project_id, identity.clone(), revision, change))
            .unwrap();
    }
    let view = model.view().unwrap();
    assert_eq!(view.audio_label(input(3)), "Guest");
    assert_eq!(
        view.input_audio_strips
            .iter()
            .find(|strip| strip.input == input(3))
            .unwrap()
            .group,
        Some(2)
    );
    assert!(view.audio_mixer.groups[1].muted);
    assert_eq!(view.audio_mixer.groups[1].gain_millidb, -6_000);
    assert_eq!(view.fade_to_black_excluded_outputs, vec![output(1)]);

    // Changes naming an unknown input, group, or output are refused.
    for (revision, change) in [
        DurableChange::AudioMixerChanged(AudioMixerOperation::SetGroup {
            input: WireInputId::from_domain(input(9)),
            group: Some(1),
        }),
        DurableChange::FadeToBlackOutputChanged {
            output: output(9),
            affected: false,
        },
    ]
    .into_iter()
    .map(|change| (7, change))
    {
        assert!(
            model
                .apply_event(event(project_id, identity.clone(), revision, change))
                .is_err()
        );
    }
}

/// Protocol 2.48 (SW-004): per-output Fade-to-Black durations and the
/// curve replicate from the snapshot and events, in output order.
#[test]
fn replicates_fade_to_black_output_durations_and_curve() {
    let project_id = project(14);
    let mut model = ClientModel::new(project_id);
    let mut initial = snapshot(project_id, 1);
    initial.fade_to_black_output_durations = vec![(output(2), 90)];
    initial.fade_to_black_curve = fm_protocol::FadeToBlackCurve::Ease;
    model.install_snapshot(initial).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    let view = model.view().unwrap();
    assert_eq!(view.fade_to_black_output_duration(output(2)), Some(90));
    assert_eq!(view.fade_to_black_output_duration(output(1)), None);
    assert_eq!(
        view.fade_to_black_curve,
        fm_protocol::FadeToBlackCurve::Ease
    );

    let changes = [
        DurableChange::FadeToBlackOutputDurationChanged {
            output: output(1),
            duration_frames: Some(12),
        },
        DurableChange::FadeToBlackOutputDurationChanged {
            output: output(2),
            duration_frames: Some(45),
        },
        DurableChange::FadeToBlackCurveChanged {
            curve: fm_protocol::FadeToBlackCurve::Linear,
        },
    ];
    for (revision, change) in (2..).zip(changes) {
        model
            .apply_event(event(project_id, identity.clone(), revision, change))
            .unwrap();
    }
    let view = model.view().unwrap();
    assert_eq!(
        view.fade_to_black_output_durations,
        vec![(output(1), 12), (output(2), 45)]
    );
    assert_eq!(
        view.fade_to_black_curve,
        fm_protocol::FadeToBlackCurve::Linear
    );
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            5,
            DurableChange::FadeToBlackOutputDurationChanged {
                output: output(1),
                duration_frames: None,
            },
        ))
        .unwrap();
    assert_eq!(
        model.view().unwrap().fade_to_black_output_durations,
        vec![(output(2), 45)]
    );
    // An unknown output is refused.
    assert!(
        model
            .apply_event(event(
                project_id,
                identity,
                6,
                DurableChange::FadeToBlackOutputDurationChanged {
                    output: output(9),
                    duration_frames: Some(12),
                },
            ))
            .is_err()
    );
}

/// Protocol 2.44 (AU-001): group solo, group parents, and the solo mode
/// replicate from events; a parent that would close a cycle is refused.
#[test]
fn replicates_group_solo_parents_and_solo_mode() {
    use fm_protocol::{AudioMixerOperation, AudioSoloMode};

    let project_id = project(13);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 1)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    assert_eq!(
        model.view().unwrap().audio_mixer.solo_mode,
        AudioSoloMode::InPlace
    );
    let changes = [
        AudioMixerOperation::SetSoloMode {
            mode: AudioSoloMode::Pfl,
        },
        AudioMixerOperation::SetGroupParent {
            group: 1,
            parent: Some(2),
        },
        AudioMixerOperation::SetGroupSolo {
            group: 2,
            soloed: true,
        },
    ];
    for (revision, change) in (2..).zip(changes) {
        model
            .apply_event(event(
                project_id,
                identity.clone(),
                revision,
                DurableChange::AudioMixerChanged(change),
            ))
            .unwrap();
    }
    let view = model.view().unwrap();
    assert_eq!(view.audio_mixer.solo_mode, AudioSoloMode::Pfl);
    assert_eq!(view.audio_mixer.groups[0].parent, Some(2));
    assert!(view.audio_mixer.groups[1].soloed);
    assert!(view.audio_mixer.group_chain_soloed(Some(1)));

    for change in [
        AudioMixerOperation::SetGroupParent {
            group: 2,
            parent: Some(1),
        },
        AudioMixerOperation::SetGroupSolo {
            group: 5,
            soloed: true,
        },
    ] {
        assert!(
            model
                .apply_event(event(
                    project_id,
                    identity.clone(),
                    5,
                    DurableChange::AudioMixerChanged(change),
                ))
                .is_err()
        );
    }
}

/// IN-001, Protocol 2.47: capture inputs come from the snapshot, follow
/// settings edits, keep their settings when their device is replaced or
/// duplicated (as the engine does), and leave with another source.
#[test]
#[allow(clippy::too_many_lines)]
fn capture_inputs_follow_snapshot_edits_replacements_and_duplicates() {
    use fm_protocol::{
        CaptureAudio, CaptureInputConfig, CaptureInputEntry, CaptureSettings, CaptureStatus,
        InputLifecycleOperation, WireInputSource,
    };

    let project = project(1);
    let mut model = ClientModel::new(project);
    let mut base = snapshot(project, 4);
    let settings = CaptureSettings {
        audio: CaptureAudio::None,
        ..CaptureSettings::default()
    };
    base.capture_inputs = vec![CaptureInputEntry {
        input: fm_protocol::WireInputId::from_domain(input(1)),
        config: CaptureInputConfig {
            stable_key: "cam-a".into(),
            settings: settings.clone(),
        },
        status: CaptureStatus::default(),
    }];
    model.install_snapshot(base.clone()).unwrap();
    let engine = base.cursor.engine.clone();
    assert_eq!(
        model
            .view()
            .unwrap()
            .capture_input(input(1))
            .map(|entry| &entry.config.settings),
        Some(&settings)
    );
    assert!(model.view().unwrap().capture_input(input(2)).is_none());

    model
        .apply_event(event(
            project,
            engine.clone(),
            5,
            DurableChange::InputLifecycleChanged {
                operation: InputLifecycleOperation::ReplaceSource {
                    input: fm_protocol::WireInputId::from_domain(input(1)),
                    source: WireInputSource::Device {
                        stable_key: "cam-b".into(),
                    },
                },
                added: Vec::new(),
            },
        ))
        .unwrap();
    let view = model.view().unwrap();
    let entry = view.capture_input(input(1)).unwrap();
    assert_eq!(entry.config.stable_key, "cam-b");
    assert_eq!(entry.config.settings, settings);

    model
        .apply_event(event(
            project,
            engine.clone(),
            6,
            DurableChange::InputLifecycleChanged {
                operation: InputLifecycleOperation::Duplicate {
                    source: fm_protocol::WireInputId::from_domain(input(1)),
                    input: fm_protocol::WireInputId::from_domain(input(7)),
                    name: None,
                },
                added: vec![(input(7), "Camera 2".into())],
            },
        ))
        .unwrap();
    assert_eq!(
        model
            .view()
            .unwrap()
            .capture_input(input(7))
            .map(|entry| &entry.config),
        Some(&CaptureInputConfig {
            stable_key: "cam-b".into(),
            settings: settings.clone(),
        })
    );

    model
        .apply_event(event(
            project,
            engine.clone(),
            7,
            DurableChange::CaptureSettingsChanged {
                input: input(7),
                settings: Box::new(CaptureSettings::default()),
            },
        ))
        .unwrap();
    assert_eq!(
        model
            .view()
            .unwrap()
            .capture_input(input(7))
            .map(|entry| &entry.config.settings),
        Some(&CaptureSettings::default())
    );

    model
        .apply_event(event(
            project,
            engine,
            8,
            DurableChange::InputLifecycleChanged {
                operation: InputLifecycleOperation::ReplaceSource {
                    input: fm_protocol::WireInputId::from_domain(input(1)),
                    source: WireInputSource::Generator {
                        video: fm_protocol::WireGeneratorVideo::Bars,
                        tone_hz: None,
                    },
                },
                added: Vec::new(),
            },
        ))
        .unwrap();
    assert!(model.view().unwrap().capture_input(input(1)).is_none());
    assert_eq!(model.view().unwrap().capture_inputs.len(), 1);
}

/// AU-011: a batch removal (also how a duplicate is undone) forgets the
/// removed inputs' system audio, capture, and network stream entries, so an
/// input later added under the same id is not shown as one.
#[test]
#[allow(clippy::too_many_lines)]
fn a_batch_removal_forgets_the_removed_inputs_source_entries() {
    use fm_protocol::{
        CaptureInputConfig, InputLifecycleOperation, NetworkStreamConfig, SystemAudioBackend,
        SystemAudioConfig, SystemAudioEntry, SystemAudioStatus, WireInputId,
    };

    let project = project(1);
    let mut model = ClientModel::new(project);
    let mut base = snapshot(project, 4);
    base.system_audio_inputs = vec![SystemAudioEntry {
        input: WireInputId::from_domain(input(3)),
        config: SystemAudioConfig::new(SystemAudioBackend::Pulse, "@DEFAULT_MONITOR@"),
        status: SystemAudioStatus::default(),
    }];
    model.install_snapshot(base.clone()).unwrap();
    let engine = base.cursor.engine.clone();
    let mut revision = 4;
    let mut apply = |model: &mut ClientModel, change| {
        revision += 1;
        model
            .apply_event(event(project, engine.clone(), revision, change))
            .unwrap();
    };
    let remove_seven = || DurableChange::InputLifecycleChanged {
        operation: InputLifecycleOperation::RemoveInputs {
            inputs: vec![WireInputId::from_domain(input(7))],
        },
        added: Vec::new(),
    };
    let add_seven = |system_audio, capture, network_stream| DurableChange::InputAdded {
        input: input(7),
        name: "Seven".into(),
        network_stream,
        system_audio,
        screen_capture: None,
        capture,
        still: None,
    };

    // A system audio duplicate, undone by a batch removal.
    apply(
        &mut model,
        DurableChange::InputLifecycleChanged {
            operation: InputLifecycleOperation::Duplicate {
                source: WireInputId::from_domain(input(3)),
                input: WireInputId::from_domain(input(7)),
                name: None,
            },
            added: vec![(input(7), "Desktop 2".into())],
        },
    );
    assert!(model.view().unwrap().system_audio_input(input(7)).is_some());
    apply(&mut model, remove_seven());
    // A capture input under the same id, removed the same way.
    apply(
        &mut model,
        add_seven(None, Some(Box::new(CaptureInputConfig::new("cam-a"))), None),
    );
    assert!(model.view().unwrap().system_audio_input(input(7)).is_none());
    apply(&mut model, remove_seven());
    // A network stream under the same id, removed the same way.
    apply(
        &mut model,
        add_seven(
            None,
            None,
            Some(Box::new(NetworkStreamConfig::new("srt://127.0.0.1:9000"))),
        ),
    );
    assert!(model.view().unwrap().capture_input(input(7)).is_none());
    apply(&mut model, remove_seven());
    // A plain input under the same id is none of them.
    apply(&mut model, add_seven(None, None, None));
    let view = model.view().unwrap();
    assert!(view.system_audio_input(input(7)).is_none());
    assert!(view.capture_input(input(7)).is_none());
    assert!(view.network_stream(input(7)).is_none());
    assert_eq!(view.system_audio_inputs.len(), 1);
    assert_eq!(view.capture_inputs, []);
    assert_eq!(view.network_streams, []);
}

/// RC-011: a snapshot's `script_state` records complete it in order;
/// `script_changed`, `script_removed`, and `script_ran` keep the scripts and
/// the desired show current; mismatched records are refused.
#[test]
#[allow(clippy::too_many_lines)] // one replication walk
fn scripts_replicate_from_snapshot_records_and_events() {
    use fm_protocol::{ScriptSpec, ScriptStateMessage, ScriptStatus};

    let script = |id: u64, source: &str| ScriptStatus {
        id,
        spec: ScriptSpec {
            name: format!("Script {id}"),
            source: source.into(),
        },
    };
    let waiting = fm_protocol::ScriptRunStatus::Waiting {
        run: 1,
        started_frame: 20,
        due_frame: 30,
        waits: 1,
        wait_frames: 10,
        line: 2,
    };
    let waiting_2 = fm_protocol::ScriptRunStatus::Waiting {
        run: 2,
        started_frame: 40,
        due_frame: 50,
        waits: 1,
        wait_frames: 10,
        line: 2,
    };
    let project_id = project(10);
    let identity = engine("engine-a", 1, "log-a");
    let mut model = ClientModel::new(project_id);
    let mut installed = snapshot(project_id, 7);
    installed.script_states = 1;
    model.install_snapshot(installed).unwrap();
    assert!(!model.state().unwrap().scripts_complete());
    assert_eq!(
        model.apply_script_state(ScriptStateMessage {
            revision: 6,
            script: script(1, "cut 2"),
            run: fm_protocol::ScriptRunStatus::Idle,
        }),
        Err(ModelError::ScriptStateMismatch)
    );
    assert_eq!(
        model.apply_script_state(ScriptStateMessage {
            revision: 7,
            script: script(0, "cut 2"),
            run: fm_protocol::ScriptRunStatus::Idle,
        }),
        Err(ModelError::InvalidScript(0))
    );
    model
        .apply_script_state(ScriptStateMessage {
            revision: 7,
            script: script(1, "cut 2"),
            run: waiting.clone(),
        })
        .unwrap();
    let state = model.state().unwrap();
    assert!(state.scripts_complete());
    // Protocol 2.62: the record carries the run paused at a wait.
    assert_eq!(state.script_run_status(1), waiting);
    assert_eq!(state.active_script_runs(), vec![(1, waiting.clone())]);
    assert_eq!(state.scripts(), [script(1, "cut 2")]);
    assert_eq!(
        model.apply_script_state(ScriptStateMessage {
            revision: 7,
            script: script(2, "cut 3"),
            run: fm_protocol::ScriptRunStatus::Idle,
        }),
        Err(ModelError::ScriptStateMismatch)
    );

    model
        .apply_event(event(
            project_id,
            identity.clone(),
            8,
            DurableChange::ScriptChanged(Box::new(script(2, "preview 3\ncut"))),
        ))
        .unwrap();
    let run = ScriptRunUpdate {
        script: 2,
        frame: 40,
        actions: 2,
        instructions: 5,
        run: waiting_2.clone(),
        effects: CueRunUpdate {
            list: 0,
            frame: 40,
            state: fm_protocol::CueRunStatus::default(),
            fired: Vec::new(),
            selection: BusSelection::new(input(3), input(1)),
            manual_transition: ManualTransitionStatus::Inactive,
            fade_to_black: fade_to_black(false, 0),
            overlays: overlays(),
            input_audio_strips: input_audio_strips(),
            streams: Vec::new(),
            recordings: recordings(),
            playlist: fm_protocol::PlaylistRunState::Stopped,
            titles: Vec::new(),
            media: Vec::new(),
            scripts: Vec::new(),
        },
    };
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            9,
            DurableChange::ScriptRan(Box::new(run)),
        ))
        .unwrap();
    let state = model.state().unwrap();
    assert_eq!(state.switcher().desired.program, input(3));
    // Protocol 2.69: script 2's run waits alongside script 1's.
    assert_eq!(
        state.active_script_runs(),
        vec![(1, waiting.clone()), (2, waiting_2)]
    );
    assert_eq!(state.script_run_status(1), waiting);
    assert_eq!(
        state.last_script_run(),
        Some(ScriptRunReport {
            script: 2,
            frame: 40,
            actions: 2,
            instructions: 5,
            cue_list: None,
        })
    );
    assert_eq!(
        state
            .scripts()
            .iter()
            .map(|script| script.id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    // A part that failed on its frame faults the run.
    let faulted = fm_protocol::ScriptRunStatus::Faulted {
        run: 2,
        frame: 30,
        failure: fm_protocol::ScriptFailureKind::Refused,
        line: 2,
        column: 1,
        detail: fm_protocol::FaultDetail {
            reason: Some(fm_protocol::RefusalReason::ProjectLocked),
            message: "the project padlock is engaged".to_owned(),
        },
    };
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            10,
            DurableChange::ScriptRunChanged {
                script: 2,
                frame: 30,
                run: faulted.clone(),
            },
        ))
        .unwrap();
    assert_eq!(model.state().unwrap().script_run_status(2), faulted);
    assert_eq!(
        model.apply_event(event(
            project_id,
            identity.clone(),
            11,
            DurableChange::ScriptRunChanged {
                script: 9,
                frame: 31,
                run: fm_protocol::ScriptRunStatus::Idle,
            },
        )),
        Err(ModelError::InvalidScript(9))
    );
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            11,
            DurableChange::ScriptRemoved { script: 1 },
        ))
        .unwrap();
    assert_eq!(model.state().unwrap().script_run_status(2), faulted);
    assert_eq!(
        model.state().unwrap().scripts(),
        [script(2, "preview 3\ncut")]
    );
    assert_eq!(
        model.apply_event(event(
            project_id,
            identity.clone(),
            12,
            DurableChange::ScriptRemoved { script: 1 },
        )),
        Err(ModelError::InvalidScript(1))
    );
    // Removing the faulted script forgets its run.
    model
        .apply_event(event(
            project_id,
            identity,
            12,
            DurableChange::ScriptRemoved { script: 2 },
        ))
        .unwrap();
    assert_eq!(model.state().unwrap().active_script_runs(), []);
}

/// Protocol 2.62: the authority clears a fault when its script is edited
/// (the fault's line named the old source), so a `script_changed` for the
/// faulted script clears it here too; an edit of another script keeps it.
#[test]
fn an_edit_of_the_faulted_script_clears_its_fault() {
    use fm_protocol::{ScriptSpec, ScriptStateMessage, ScriptStatus};

    let script = |id: u64, source: &str| ScriptStatus {
        id,
        spec: ScriptSpec {
            name: format!("Script {id}"),
            source: source.into(),
        },
    };
    let faulted = fm_protocol::ScriptRunStatus::Faulted {
        run: 1,
        frame: 30,
        failure: fm_protocol::ScriptFailureKind::Refused,
        line: 2,
        column: 1,
        detail: fm_protocol::FaultDetail {
            reason: Some(fm_protocol::RefusalReason::ProjectLocked),
            message: "the project padlock is engaged".to_owned(),
        },
    };
    let project_id = project(10);
    let identity = engine("engine-a", 1, "log-a");
    let mut model = ClientModel::new(project_id);
    let mut installed = snapshot(project_id, 7);
    installed.script_states = 2;
    model.install_snapshot(installed).unwrap();
    for (id, run) in [
        (1, faulted.clone()),
        (2, fm_protocol::ScriptRunStatus::Idle),
    ] {
        model
            .apply_script_state(ScriptStateMessage {
                revision: 7,
                script: script(id, "wait 3\ncut 9"),
                run,
            })
            .unwrap();
    }
    assert_eq!(
        model.state().unwrap().active_script_runs(),
        vec![(1, faulted.clone())]
    );

    model
        .apply_event(event(
            project_id,
            identity.clone(),
            8,
            DurableChange::ScriptChanged(Box::new(script(2, "cut 2"))),
        ))
        .unwrap();
    assert_eq!(model.state().unwrap().script_run_status(1), faulted);

    model
        .apply_event(event(
            project_id,
            identity,
            9,
            DurableChange::ScriptChanged(Box::new(script(1, "wait 3\ncut 2"))),
        ))
        .unwrap();
    let state = model.state().unwrap();
    assert_eq!(state.active_script_runs(), []);
    assert_eq!(
        state.script_run_status(1),
        fm_protocol::ScriptRunStatus::Idle
    );
}

/// Protocol 2.69: a snapshot carries several scripts' runs, each with its
/// own id; stopping one leaves the others, `script_runs_stopped` idles
/// every one, and a snapshot naming one run id twice is refused.
#[test]
fn concurrent_script_runs_replicate_and_stop() {
    use fm_protocol::{ScriptSpec, ScriptStateMessage, ScriptStatus};

    let script = |id: u64| ScriptStatus {
        id,
        spec: ScriptSpec {
            name: format!("Script {id}"),
            source: "wait 5\ncut 2".into(),
        },
    };
    let waiting = |run: u64| fm_protocol::ScriptRunStatus::Waiting {
        run,
        started_frame: 20,
        due_frame: 25,
        waits: 1,
        wait_frames: 5,
        line: 1,
    };
    let project_id = project(10);
    let identity = engine("engine-a", 1, "log-a");
    let install = |runs: &[(u64, u64)]| {
        let mut model = ClientModel::new(project_id);
        let mut installed = snapshot(project_id, 7);
        installed.script_states = 3;
        model.install_snapshot(installed).unwrap();
        let mut result = Ok(());
        for (id, run) in runs {
            result = result.and_then(|()| {
                model.apply_script_state(ScriptStateMessage {
                    revision: 7,
                    script: script(*id),
                    run: waiting(*run),
                })
            });
        }
        (model, result)
    };
    assert_eq!(
        install(&[(1, 4), (2, 4)]).1,
        Err(ModelError::ScriptStateMismatch)
    );
    let (mut model, result) = install(&[(1, 9), (2, 4), (3, 6)]);
    result.unwrap();
    let state = model.state().unwrap();
    // Listed in run order.
    assert_eq!(
        state.active_script_runs(),
        vec![(2, waiting(4)), (3, waiting(6)), (1, waiting(9))]
    );

    model
        .apply_event(event(
            project_id,
            identity.clone(),
            8,
            DurableChange::ScriptRunChanged {
                script: 3,
                frame: 22,
                run: fm_protocol::ScriptRunStatus::Idle,
            },
        ))
        .unwrap();
    assert_eq!(
        model.state().unwrap().active_script_runs(),
        vec![(2, waiting(4)), (1, waiting(9))]
    );
    assert_eq!(
        model.apply_event(event(
            project_id,
            identity.clone(),
            9,
            DurableChange::ScriptRunsStopped {
                frame: 23,
                scripts: vec![1, 7],
            },
        )),
        Err(ModelError::InvalidScript(7))
    );
    model
        .apply_event(event(
            project_id,
            identity,
            9,
            DurableChange::ScriptRunsStopped {
                frame: 23,
                scripts: vec![1, 2],
            },
        ))
        .unwrap();
    let state = model.state().unwrap();
    assert_eq!(state.active_script_runs(), []);
    assert_eq!(
        state.script_run_status(2),
        fm_protocol::ScriptRunStatus::Idle
    );
}

/// Protocol 2.61: each script step a `cue_run_changed` reports becomes
/// that script's last run, attributed to the list, without touching the
/// other scripts' runs; a run naming a script the project lacks, or more
/// steps than a list holds, is refused; removing a script forgets its run.
#[test]
#[allow(clippy::too_many_lines)] // one replication walk
fn a_cue_run_records_the_last_run_of_each_script_it_ran() {
    use fm_protocol::{
        CueListSpec, CueListStatus, CueScriptRun, MAX_CUE_LIST_SCRIPT_STEPS, ScriptSpec,
        ScriptStatus, WireCue, WireCueAction,
    };

    let project_id = project(10);
    let identity = engine("engine-a", 1, "log-a");
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();
    let mut revision = 7;
    let mut apply = |model: &mut ClientModel, change: DurableChange| {
        let applied = model.apply_event(event(project_id, identity.clone(), revision + 1, change));
        if applied.is_ok() {
            revision += 1;
        }
        applied
    };
    for id in [1, 2, 3] {
        apply(
            &mut model,
            DurableChange::ScriptChanged(Box::new(ScriptStatus {
                id,
                spec: ScriptSpec {
                    name: format!("Script {id}"),
                    source: "cut 2".into(),
                },
            })),
        )
        .unwrap();
    }
    let step = |script| WireCueAction::RunScript { script };
    apply(
        &mut model,
        DurableChange::CueListChanged(Box::new(CueListStatus {
            id: 4,
            spec: CueListSpec {
                name: "Show".into(),
                cues: vec![WireCue {
                    id: 1,
                    name: "Open".into(),
                    auto_follow: false,
                    actions: vec![step(1), step(2), step(1)],
                }],
            },
            state: fm_protocol::CueRunStatus::default(),
        })),
    )
    .unwrap();
    let direct = ScriptRunUpdate {
        run: fm_protocol::ScriptRunStatus::Idle,
        script: 3,
        frame: 30,
        actions: 1,
        instructions: 2,
        effects: CueRunUpdate {
            list: 0,
            frame: 30,
            state: fm_protocol::CueRunStatus::default(),
            fired: Vec::new(),
            selection: BusSelection::new(input(2), input(1)),
            manual_transition: ManualTransitionStatus::Inactive,
            fade_to_black: fade_to_black(false, 0),
            overlays: overlays(),
            input_audio_strips: input_audio_strips(),
            streams: Vec::new(),
            recordings: recordings(),
            playlist: fm_protocol::PlaylistRunState::Stopped,
            titles: Vec::new(),
            media: Vec::new(),
            scripts: Vec::new(),
        },
    };
    apply(
        &mut model,
        DurableChange::ScriptRan(Box::new(direct.clone())),
    )
    .unwrap();

    let run = |script, actions, instructions| CueScriptRun {
        script,
        actions,
        instructions,
    };
    let cue_run = |scripts| CueRunUpdate {
        list: 4,
        fired: vec![1],
        state: fm_protocol::CueRunStatus {
            live: Some(1),
            played: vec![1],
            ..fm_protocol::CueRunStatus::default()
        },
        scripts,
        ..direct.effects.clone()
    };
    let before = model.state().unwrap().clone();
    assert_eq!(
        apply(
            &mut model,
            DurableChange::CueRunChanged(Box::new(cue_run(vec![run(1, 1, 2), run(9, 1, 2)])))
        ),
        Err(ModelError::InvalidScript(9))
    );
    assert_eq!(
        apply(
            &mut model,
            DurableChange::CueRunChanged(Box::new(cue_run(vec![
                run(1, 1, 2);
                MAX_CUE_LIST_SCRIPT_STEPS + 1
            ])))
        ),
        Err(ModelError::InvalidCueList(4))
    );
    // Protocol 2.64: an armed GO must name a cue of its list.
    let mut stray = cue_run(Vec::new());
    stray.state.armed = Some(fm_protocol::CueArmedStatus {
        cue: 9,
        due_frame: 90,
    });
    assert_eq!(
        apply(&mut model, DurableChange::CueRunChanged(Box::new(stray))),
        Err(ModelError::InvalidCueList(4))
    );
    assert_eq!(model.state().unwrap(), &before);
    assert_eq!(before.cue_frame(), None);

    let mut update = cue_run(vec![run(1, 1, 2), run(2, 3, 9), run(1, 4, 11)]);
    update.frame = 50;
    update.state.armed = Some(fm_protocol::CueArmedStatus {
        cue: 1,
        due_frame: 90,
    });
    apply(&mut model, DurableChange::CueRunChanged(Box::new(update))).unwrap();
    let state = model.state().unwrap();
    // The run's frame anchors Studio's armed-GO countdown.
    assert_eq!(state.cue_frame(), Some(50));
    assert_eq!(
        state
            .cue_list(4)
            .unwrap()
            .state
            .armed
            .map(|armed| armed.due_frame),
        Some(90)
    );
    let by_cue = |script, actions, instructions| ScriptRunReport {
        script,
        frame: 50,
        actions,
        instructions,
        cue_list: Some(4),
    };
    // The later step of script 1 is its last run.
    assert_eq!(state.script_run(1), Some(by_cue(1, 4, 11)));
    assert_eq!(state.script_run(2), Some(by_cue(2, 3, 9)));
    assert_eq!(
        state.script_run(3),
        Some(ScriptRunReport {
            script: 3,
            frame: 30,
            actions: 1,
            instructions: 2,
            cue_list: None,
        })
    );
    assert_eq!(state.last_script_run(), Some(by_cue(1, 4, 11)));

    apply(&mut model, DurableChange::ScriptRemoved { script: 3 }).unwrap();
    assert_eq!(model.state().unwrap().script_run(3), None);
    assert_eq!(model.state().unwrap().script_run(1), Some(by_cue(1, 4, 11)));
}

/// Regression (RC-011): a `script_changed` adding a script past the project
/// bound was accepted, so the replicated project could hold more scripts
/// than any server allows; replacing one at the bound still applies.
#[test]
fn a_script_changed_past_the_bound_is_refused() {
    use fm_protocol::{MAX_SCRIPTS, ScriptSpec, ScriptStatus};

    let script = |id: u64| ScriptStatus {
        id,
        spec: ScriptSpec {
            name: format!("Script {id}"),
            source: "cut 2".into(),
        },
    };
    let project_id = project(10);
    let identity = engine("engine-a", 1, "log-a");
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();
    let mut revision = 7;
    for id in 1..=MAX_SCRIPTS as u64 {
        revision += 1;
        model
            .apply_event(event(
                project_id,
                identity.clone(),
                revision,
                DurableChange::ScriptChanged(Box::new(script(id))),
            ))
            .unwrap();
    }
    assert_eq!(
        model.apply_event(event(
            project_id,
            identity.clone(),
            revision + 1,
            DurableChange::ScriptChanged(Box::new(script(99))),
        )),
        Err(ModelError::InvalidScript(99))
    );
    assert_eq!(model.state().unwrap().scripts().len(), MAX_SCRIPTS);
    let mut replaced = script(1);
    replaced.spec.source = "cut 3".into();
    model
        .apply_event(event(
            project_id,
            identity,
            revision + 1,
            DurableChange::ScriptChanged(Box::new(replaced.clone())),
        ))
        .unwrap();
    assert_eq!(model.state().unwrap().script(1), Some(&replaced));
}

#[test]
fn script_syntax_is_checked_before_saving() {
    assert_eq!(check_script_syntax("preview 2\ncut\n"), None);
    let check = check_script_syntax("preview 2\nif program = 1\nend").unwrap();
    assert_eq!((check.at.line, check.at.column), (2, 12));
    assert_eq!(check.kind, ScriptCheckKind::Syntax);
}

fn media_list_status(input_id: u128, active: u32) -> fm_protocol::MediaListStatus {
    let item = |id: u32| fm_protocol::MediaListItemSpec {
        id,
        uri: format!("asset://media/{id}.mov"),
        title: None,
        mark_in: 0,
        mark_out: None,
    };
    fm_protocol::MediaListStatus {
        input: fm_protocol::WireInputId::from_domain(input(input_id)),
        items: vec![item(1), item(2)],
        options: fm_protocol::MediaListOptionsSpec::default(),
        active,
        cycle: 0,
        order: vec![1, 2],
        position: active - 1,
    }
}

/// IN-006: the snapshot's `media_list_state` records complete it; a
/// creation adds the list input with its name and strip; later changes
/// replace the list; removing the input drops it; a change for an input
/// that is not a list, or a record the snapshot did not announce, is
/// refused.
#[test]
fn media_lists_replicate_from_records_and_events() {
    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    let mut announced = snapshot(project_id, 7);
    announced.media_list_states = 1;
    model.install_snapshot(announced).unwrap();
    assert!(!model.state().unwrap().media_lists_complete());
    let mut record = fm_protocol::MediaListStateMessage {
        revision: 7,
        status: media_list_status(3, 1),
    };
    record.status.order = vec![2, 1];
    assert_eq!(
        model.apply_media_list_state(record),
        Err(ModelError::MediaListStateMismatch)
    );
    model
        .apply_media_list_state(fm_protocol::MediaListStateMessage {
            revision: 7,
            status: media_list_status(3, 1),
        })
        .unwrap();
    assert!(model.state().unwrap().media_lists_complete());
    assert_eq!(
        model.apply_media_list_state(fm_protocol::MediaListStateMessage {
            revision: 7,
            status: media_list_status(2, 1),
        }),
        Err(ModelError::MediaListStateMismatch)
    );

    let identity = model.reconnect_cursor().unwrap().engine.clone();
    let changed = |created: Option<&str>, status| DurableChange::MediaListChanged {
        frame: 40,
        operation: if created.is_some() {
            "create"
        } else {
            "navigate"
        },
        created: created.map(Into::into),
        status: Box::new(status),
    };
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            8,
            changed(Some("Walk-in"), media_list_status(9, 1)),
        ))
        .unwrap();
    let state = model.state().unwrap();
    assert_eq!(state.inputs().last(), Some(&input(9)));
    assert_eq!(state.input_name(input(9)), Some("Walk-in"));
    assert_eq!(state.media_lists().len(), 2);
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            9,
            changed(None, media_list_status(9, 2)),
        ))
        .unwrap();
    assert_eq!(
        model.state().unwrap().media_list(input(9)).unwrap().active,
        2
    );
    // A change for an input that is no list is refused.
    assert_eq!(
        model.apply_event(event(
            project_id,
            identity.clone(),
            10,
            changed(None, media_list_status(2, 1)),
        )),
        Err(ModelError::InvalidMediaListChange(input(2)))
    );
    model
        .apply_event(event(
            project_id,
            identity,
            10,
            DurableChange::InputRemoved { input: input(9) },
        ))
        .unwrap();
    assert!(model.state().unwrap().media_list(input(9)).is_none());
}

#[test]
#[allow(clippy::too_many_lines)] // one sync and its edits
fn strip_dsp_records_complete_the_snapshot_and_events_keep_them_current() {
    use fm_protocol::{
        AudioDspTarget, AudioMixerOperation, AudioStripDspStateMessage, WireInputId,
    };
    use fm_types::strip_dsp::{StripDsp, StripDspPreset, StripDspTarget};
    let record = |revision, value, chain| AudioStripDspStateMessage {
        revision,
        input: WireInputId::from_domain(input(value)),
        chain,
    };
    let project_id = project(10);
    let identity = engine("engine-a", 1, "log-a");
    let mut model = ClientModel::new(project_id);
    let mut installed = snapshot(project_id, 7);
    installed.audio_mixer.master_dsp = StripDspPreset::Master.chain();
    installed.audio_mixer.strip_dsp_states = 2;
    model.install_snapshot(installed).unwrap();
    assert!(!model.state().unwrap().audio_strip_dsps_complete());
    let voice = StripDspPreset::Voice.chain();
    let music = StripDspPreset::Music.chain();
    // Another revision, an unknown input, or a default chain is refused.
    for refused in [
        record(6, 1, voice),
        record(7, 9, voice),
        record(7, 1, StripDsp::DEFAULT),
    ] {
        assert_eq!(
            model.apply_audio_strip_dsp_state(refused),
            Err(ModelError::AudioStripDspStateMismatch)
        );
    }
    model
        .apply_audio_strip_dsp_state(record(7, 2, voice))
        .unwrap();
    // Records ascend by input.
    assert_eq!(
        model.apply_audio_strip_dsp_state(record(7, 1, music)),
        Err(ModelError::AudioStripDspStateMismatch)
    );
    model
        .apply_audio_strip_dsp_state(record(7, 3, music))
        .unwrap();
    let state = model.state().unwrap();
    assert!(state.audio_strip_dsps_complete());
    assert_eq!(state.input_audio_dsp(input(1)), StripDsp::DEFAULT);
    assert_eq!(state.input_audio_dsp(input(2)), voice);
    assert_eq!(
        state.audio_dsp(StripDspTarget::Master),
        StripDspPreset::Master.chain()
    );
    // Beyond the announced count.
    assert_eq!(
        model.apply_audio_strip_dsp_state(record(7, 1, music)),
        Err(ModelError::AudioStripDspStateMismatch)
    );

    // An accepted edit replaces a chain; a default chain clears it.
    let set = |target, chain| {
        DurableChange::AudioMixerChanged(AudioMixerOperation::SetStripDsp {
            target,
            chain: Box::new(chain),
            gesture: None,
        })
    };
    let strip = |value| AudioDspTarget::Input(WireInputId::from_domain(input(value)));
    model
        .apply_event(event(project_id, identity.clone(), 8, set(strip(1), music)))
        .unwrap();
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            9,
            set(strip(2), StripDsp::DEFAULT),
        ))
        .unwrap();
    model
        .apply_event(event(
            project_id,
            identity.clone(),
            10,
            set(AudioDspTarget::Master, StripDsp::DEFAULT),
        ))
        .unwrap();
    let state = model.state().unwrap();
    assert_eq!(state.input_audio_dsp(input(1)), music);
    assert_eq!(state.input_audio_dsp(input(2)), StripDsp::DEFAULT);
    assert_eq!(state.master_audio_dsp(), &StripDsp::DEFAULT);
    // An edit for an unknown input does not apply.
    assert!(
        model
            .apply_event(event(
                project_id,
                identity.clone(),
                11,
                set(strip(9), music)
            ))
            .is_err()
    );
    // A removed input's chain leaves with it.
    model
        .apply_event(event(
            project_id,
            identity,
            11,
            DurableChange::InputRemoved { input: input(3) },
        ))
        .unwrap();
    assert_eq!(
        model.state().unwrap().input_audio_dsp(input(3)),
        StripDsp::DEFAULT
    );
}
