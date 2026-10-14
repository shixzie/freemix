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

#[test]
fn runtime_realization_can_use_current_or_retained_desired_revision() {
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
        .apply_runtime_realization(realization(project_id, identity, 8, 6, 2))
        .unwrap();
    let switcher = model.state().unwrap().switcher();
    assert_eq!(switcher.realized, retained);
    assert_eq!(switcher.runtime_generation, Some(6));
    assert_eq!(model.reconnect_cursor().unwrap().revision, Revision::new(9));
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
            },
            ModelError::DuplicateInput(input(2)),
        ),
        (
            DurableChange::InputAdded {
                input: input(10),
                name: "Camera".into(),
            },
            ModelError::InvalidInputNames,
        ),
        (
            DurableChange::InputAdded {
                input: input(10),
                name: " ".into(),
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
            },
            edit_history: EditHistoryStatus::EMPTY,
        },
    );
    assert_eq!(
        added.change,
        DurableChange::InputAdded {
            input: input(11),
            name: "Bars".into(),
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
        TitleFieldStatus, TitleFieldUpdate, TitleFieldValue, TitleStateMessage, WireInputId,
        WireTitleFieldId,
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
            engine: engine("engine-a", 1, "log-a"),
            revision: 10,
            show_name: "Show".into(),
            inputs: (1..=3)
                .map(|value| fm_protocol::InputStatus {
                    input: wire(value),
                    name: format!("Input {value}"),
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
}

#[test]
fn playlist_changed_replicates_the_playlist_and_desired_selection() {
    use fm_protocol::{PlaylistEntrySpec, PlaylistLoopMode, PlaylistRunState, WireInputId};

    let project_id = project(10);
    let mut model = ClientModel::new(project_id);
    model.install_snapshot(snapshot(project_id, 7)).unwrap();
    let identity = model.reconnect_cursor().unwrap().engine.clone();
    assert_eq!(model.state().unwrap().playlist(), None);

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
    assert_eq!(model.state().unwrap().playlist(), None);

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
