//! Return-audio selection, microphone mute/gain mix-minus, reconnect-window
//! expiry, and role-gated guest manager commands.

use std::num::NonZeroU128;

use fm_audio::Gain;
use fm_guest::{
    AuxBus, ChatAudience, ChatGroupId, ChatMessageId, ChatScope, DeviceHealth, GuestAudioControl,
    GuestCommand, GuestCommandOutcome, GuestError, GuestId, GuestIdentity, GuestManager,
    GuestManagerPermission, GuestManagerRole, GuestScope, GuestSessionId, GuestSlot,
    HealthSeverity, InvitationId, LatencyEchoHealth, LobbyPreflight, MAX_ROLE_PERMISSIONS,
    MixDestination, MixRoute, MixSource, RedactionFlag, ReturnAudioSource, ReturnOutput,
    ReturnVideoSelection, SessionConnection, TalkbackState,
};

fn nz(value: u128) -> NonZeroU128 {
    NonZeroU128::new(value).expect("test IDs are non-zero")
}

fn guest(value: u128) -> GuestId {
    GuestId::new(nz(value))
}

fn invitation(value: u128) -> InvitationId {
    InvitationId::new(nz(value))
}

fn session(value: u128) -> GuestSessionId {
    GuestSessionId::new(nz(value))
}

fn ready() -> LobbyPreflight {
    LobbyPreflight {
        camera: DeviceHealth::Ready,
        microphone: DeviceHealth::Ready,
        speaker: DeviceHealth::Ready,
        signal: LatencyEchoHealth::new(Some(60), Some(40.0)).unwrap(),
    }
}

fn enter(manager: &mut GuestManager, number: u128) {
    manager
        .create_invitation(invitation(number), 100_000, None, 0)
        .unwrap();
    manager
        .enter_lobby(
            invitation(number),
            GuestIdentity::new(guest(number), format!("Guest {number}")).unwrap(),
            ready(),
            1,
        )
        .unwrap();
}

fn admit(manager: &mut GuestManager, number: u128) -> GuestSlot {
    enter(manager, number);
    manager
        .admit_lobby_guest(guest(number), session(number), 2)
        .unwrap()
}

fn routes_to(routes: &[MixRoute], destination: MixDestination) -> Vec<MixRoute> {
    routes
        .iter()
        .copied()
        .filter(|route| route.destination == destination)
        .collect()
}

#[test]
fn return_output_and_aux_bus_values_are_validated() {
    assert_eq!(ReturnOutput::new(1).unwrap().number(), 1);
    assert_eq!(ReturnOutput::new(4).unwrap().number(), 4);
    assert_eq!(
        ReturnOutput::new(0),
        Err(GuestError::InvalidReturnOutput(0))
    );
    assert_eq!(
        ReturnOutput::new(5),
        Err(GuestError::InvalidReturnOutput(5))
    );
    assert_eq!(AuxBus::new('a').unwrap().letter(), 'A');
    assert_eq!(AuxBus::new('G').unwrap().letter(), 'G');
    assert_eq!(AuxBus::new('H'), Err(GuestError::InvalidAuxBus('H')));
    assert_eq!(ReturnAudioSource::default(), ReturnAudioSource::Master);
}

#[test]
fn each_guest_return_uses_its_own_clean_bus_and_never_its_own_microphone() {
    let mut manager = GuestManager::new();
    for number in 1..=3 {
        admit(&mut manager, number);
    }
    let output_two = ReturnAudioSource::Output(ReturnOutput::new(2).unwrap());
    let aux_c = ReturnAudioSource::Aux(AuxBus::new('C').unwrap());
    manager.set_return_audio(guest(2), output_two).unwrap();
    manager.set_return_audio(guest(3), aux_c).unwrap();
    // Return video stays independent of return audio.
    manager
        .set_return_video(guest(2), ReturnVideoSelection::Preview)
        .unwrap();

    let routes = manager.mix_minus_matrix().routes().to_vec();
    for (number, bed) in [(1, ReturnAudioSource::Master), (2, output_two), (3, aux_c)] {
        let to_guest = routes_to(&routes, MixDestination::GuestReturn(guest(number)));
        let beds: Vec<_> = to_guest
            .iter()
            .filter_map(|route| match route.source {
                MixSource::CleanReturnBed(source) => Some(source),
                _ => None,
            })
            .collect();
        assert_eq!(beds, vec![bed], "guest {number} hears exactly one bed");
        assert!(
            !to_guest
                .iter()
                .any(|route| route.source == MixSource::GuestMicrophone(guest(number))),
            "guest {number} never hears itself"
        );
        // Two other guests plus the bed.
        assert_eq!(to_guest.len(), 3);
    }
    assert_eq!(
        manager.session(guest(2)).unwrap().return_video,
        ReturnVideoSelection::Preview
    );
    // Beds never reach program.
    assert!(
        routes_to(&routes, MixDestination::Program)
            .iter()
            .all(|route| matches!(route.source, MixSource::GuestMicrophone(_)))
    );
}

#[test]
fn muted_microphone_leaves_program_and_other_returns_but_still_hears_the_show() {
    let mut manager = GuestManager::new();
    admit(&mut manager, 1);
    admit(&mut manager, 2);
    let minus_six = Gain::from_db(-6.0).unwrap();
    manager
        .set_guest_audio(
            guest(2),
            GuestAudioControl {
                microphone_muted: false,
                microphone_gain: minus_six,
            },
        )
        .unwrap();

    let routes = manager.mix_minus_matrix().routes().to_vec();
    let from_two: Vec<_> = routes
        .iter()
        .filter(|route| route.source == MixSource::GuestMicrophone(guest(2)))
        .collect();
    assert_eq!(from_two.len(), 2, "program plus guest 1 return");
    assert!(from_two.iter().all(|route| route.gain == minus_six));

    manager
        .set_guest_audio(
            guest(1),
            GuestAudioControl {
                microphone_muted: true,
                microphone_gain: Gain::UNITY,
            },
        )
        .unwrap();
    manager.set_talkback(guest(1), TalkbackState::On).unwrap();
    let routes = manager.mix_minus_matrix().routes().to_vec();
    assert!(
        !routes
            .iter()
            .any(|route| route.source == MixSource::GuestMicrophone(guest(1)))
    );
    let to_one = routes_to(&routes, MixDestination::GuestReturn(guest(1)));
    assert!(
        to_one
            .iter()
            .any(|route| route.source == MixSource::CleanReturnBed(ReturnAudioSource::Master))
    );
    assert!(
        to_one
            .iter()
            .any(|route| route.source == MixSource::GuestMicrophone(guest(2)))
    );
    assert!(
        to_one
            .iter()
            .any(|route| route.source == MixSource::ManagerTalkback)
    );
    assert!(
        !routes_to(&routes, MixDestination::Program)
            .iter()
            .any(|route| route.source == MixSource::ManagerTalkback)
    );

    // Return and mic controls survive a reconnect.
    manager.disconnect_guest(guest(1), 10, 20).unwrap();
    manager.reconnect_guest(guest(1), 20).unwrap();
    let restored = manager.session(guest(1)).unwrap();
    assert!(restored.audio.microphone_muted);
    assert_eq!(restored.talkback, TalkbackState::On);
}

#[test]
fn expired_reconnect_windows_free_slots_for_new_guests() {
    let mut manager = GuestManager::new();
    let mut slots = Vec::new();
    for number in 1..=8 {
        slots.push(admit(&mut manager, number));
    }
    assert_eq!(slots[2].number(), 3);
    enter(&mut manager, 9);
    assert_eq!(
        manager.admit_lobby_guest(guest(9), session(9), 5),
        Err(GuestError::SessionLimitReached)
    );

    manager.disconnect_guest(guest(5), 10, 100).unwrap();
    manager.disconnect_guest(guest(3), 10, 200).unwrap();
    // Window end is inclusive: nothing expires at exactly 100.
    assert_eq!(manager.expire_reconnect_windows(100).len(), 0);
    let expired = manager.expire_reconnect_windows(201);
    let expired_slots: Vec<_> = expired.iter().map(|s| s.slot.number()).collect();
    assert_eq!(expired_slots, vec![3, 5]);
    assert!(manager.session(guest(3)).is_none());
    assert_eq!(
        manager.reconnect_guest(guest(5), 202),
        Err(GuestError::SessionNotFound)
    );
    // Connected sessions are untouched.
    assert_eq!(manager.sessions().count(), 6);
    assert_eq!(
        manager
            .admit_lobby_guest(guest(9), session(9), 202)
            .unwrap(),
        GuestSlot::new(3).unwrap()
    );
}

#[test]
fn denied_commands_have_no_side_effects_and_report_the_missing_permission() {
    let mut manager = GuestManager::new();
    admit(&mut manager, 1);
    admit(&mut manager, 2);
    let observer = GuestManagerRole::observer();

    let command = GuestCommand::SetTalkback {
        guest_id: guest(1),
        state: TalkbackState::On,
    };
    assert_eq!(
        manager.execute(&observer, command.clone(), 10),
        Err(GuestError::PermissionDenied(
            GuestManagerPermission::UseTalkback(GuestScope::Guest(guest(1)))
        ))
    );
    assert_eq!(
        manager.session(guest(1)).unwrap().talkback,
        TalkbackState::Off
    );
    assert_eq!(
        manager.execute(
            &observer,
            GuestCommand::CreateInvitation {
                id: invitation(50),
                expires_at_ms: 500,
                reserved_slot: None,
            },
            10,
        ),
        Err(GuestError::PermissionDenied(
            GuestManagerPermission::ManageInvitations
        ))
    );
    assert!(manager.invitation(invitation(50)).is_none());

    // A role scoped to guest 1 cannot touch guest 2.
    let scoped = GuestManagerRole::new([
        GuestManagerPermission::UseTalkback(GuestScope::Guest(guest(1))),
        GuestManagerPermission::ManageSession(GuestScope::Guest(guest(1))),
        GuestManagerPermission::UseTalkback(GuestScope::Guest(guest(1))),
    ])
    .unwrap();
    assert_eq!(scoped.permissions().len(), 2, "duplicates are dropped");
    assert_eq!(
        manager.execute(&scoped, command, 10),
        Ok(GuestCommandOutcome::Applied)
    );
    assert_eq!(
        manager.session(guest(1)).unwrap().talkback,
        TalkbackState::On
    );
    assert_eq!(
        manager.execute(&scoped, GuestCommand::EndSession(guest(2)), 10),
        Err(GuestError::PermissionDenied(
            GuestManagerPermission::ManageSession(GuestScope::Guest(guest(2)))
        ))
    );
    assert!(manager.session(guest(2)).is_some());
    // Return video needs its own permission even with session control.
    assert!(matches!(
        manager.execute(
            &scoped,
            GuestCommand::SetReturnVideo {
                guest_id: guest(1),
                selection: ReturnVideoSelection::None,
            },
            10,
        ),
        Err(GuestError::PermissionDenied(
            GuestManagerPermission::SelectReturnVideo(_)
        ))
    ));
    let ended = manager
        .execute(&scoped, GuestCommand::EndSession(guest(1)), 10)
        .unwrap();
    assert!(matches!(ended, GuestCommandOutcome::Ended(s) if s.identity.id == guest(1)));
}

#[test]
fn producer_runs_the_full_invite_admit_route_flow() {
    let mut manager = GuestManager::new();
    let producer = GuestManagerRole::producer();
    let slot = GuestSlot::new(4).unwrap();
    manager
        .execute(
            &producer,
            GuestCommand::CreateInvitation {
                id: invitation(1),
                expires_at_ms: 1_000,
                reserved_slot: Some(slot),
            },
            0,
        )
        .unwrap();
    manager
        .enter_lobby(
            invitation(1),
            GuestIdentity::new(guest(1), "Host").unwrap(),
            ready(),
            5,
        )
        .unwrap();
    assert_eq!(
        manager.execute(
            &producer,
            GuestCommand::AdmitLobbyGuest {
                guest_id: guest(1),
                session_id: session(1),
            },
            6,
        ),
        Ok(GuestCommandOutcome::Admitted(slot))
    );
    let headphones = ReturnAudioSource::Headphones;
    manager
        .execute(
            &producer,
            GuestCommand::SetReturnAudio {
                guest_id: guest(1),
                source: headphones,
            },
            7,
        )
        .unwrap();
    assert_eq!(manager.session(guest(1)).unwrap().return_audio, headphones);

    // Lobby rejection through a command is retained with its reason.
    enter(&mut manager, 2);
    manager
        .execute(
            &producer,
            GuestCommand::RejectLobbyGuest {
                guest_id: guest(2),
                reason: "duplicate".to_owned(),
            },
            8,
        )
        .unwrap();
    assert!(matches!(
        manager.lobby_entry(guest(2)).unwrap().state,
        fm_guest::LobbyState::Rejected { .. }
    ));
}

#[test]
fn chat_redaction_and_review_follow_chat_scopes() {
    let mut manager = GuestManager::new();
    admit(&mut manager, 1);
    admit(&mut manager, 2);
    admit(&mut manager, 3);
    let group = ChatGroupId::new(nz(1));
    manager
        .create_chat_group(group, [guest(1), guest(3)])
        .unwrap();
    let private = ChatAudience::private(guest(2), guest(1)).unwrap();
    manager
        .send_chat(ChatMessageId::new(nz(1)), guest(1), private, "hello", 10)
        .unwrap();
    manager
        .send_chat(
            ChatMessageId::new(nz(2)),
            guest(3),
            ChatAudience::Group(group),
            "group",
            11,
        )
        .unwrap();

    // Either private participant scope covers the conversation.
    let with_second = GuestManagerRole::new([GuestManagerPermission::ModerateChat(
        ChatScope::PrivateWith(guest(2)),
    )])
    .unwrap();
    assert_eq!(
        manager
            .chat_messages_for_role(&with_second, private)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        manager.chat_messages_for_role(&with_second, ChatAudience::Group(group)),
        Err(GuestError::PermissionDenied(
            GuestManagerPermission::ModerateChat(ChatScope::Group(group))
        ))
    );
    assert_eq!(
        manager.execute(
            &with_second,
            GuestCommand::RedactChat(ChatMessageId::new(nz(2))),
            12
        ),
        Err(GuestError::PermissionDenied(
            GuestManagerPermission::ModerateChat(ChatScope::Group(group))
        ))
    );
    manager
        .execute(
            &with_second,
            GuestCommand::RedactChat(ChatMessageId::new(nz(1))),
            12,
        )
        .unwrap();
    let redacted = manager.chat_messages_for_manager(private);
    assert_eq!(redacted[0].body(), None);
    assert!(
        redacted[0]
            .redaction_flags()
            .contains(&RedactionFlag::Moderator)
    );
    assert_eq!(
        manager.execute(
            &GuestManagerRole::chat_moderator(),
            GuestCommand::RedactChat(ChatMessageId::new(nz(99))),
            13
        ),
        Err(GuestError::ChatMessageNotFound)
    );
    assert_eq!(
        manager.chat_messages_for_manager(ChatAudience::Group(group))[0].body(),
        Some("group")
    );
}

#[test]
fn health_report_is_ordered_by_slot_and_filtered_by_scope() {
    let mut manager = GuestManager::new();
    admit(&mut manager, 2);
    admit(&mut manager, 1);
    manager
        .update_session_health(
            guest(1),
            LatencyEchoHealth::new(Some(400), Some(40.0)).unwrap(),
        )
        .unwrap();
    manager.disconnect_guest(guest(2), 5, 50).unwrap();

    let all = manager.health_report(&GuestManagerRole::observer());
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].guest_id, guest(2));
    assert_eq!(all[0].slot.number(), 1);
    assert_eq!(
        all[0].connection,
        SessionConnection::Disconnected {
            reconnect_until_ms: 50
        }
    );
    assert_eq!(all[1].display_name, "Guest 1");
    assert_eq!(all[1].severity, HealthSeverity::Unhealthy);

    let scoped = GuestManagerRole::new([GuestManagerPermission::ViewHealth(GuestScope::Guest(
        guest(1),
    ))])
    .unwrap();
    let rows = manager.health_report(&scoped);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].guest_id, guest(1));
    assert_eq!(
        manager
            .health_report(&GuestManagerRole::new([]).unwrap())
            .len(),
        0
    );
}

#[test]
fn role_permission_count_is_bounded() {
    let many = (1..=(MAX_ROLE_PERMISSIONS as u128 + 1))
        .map(|n| GuestManagerPermission::ViewHealth(GuestScope::Guest(guest(n))));
    assert_eq!(
        GuestManagerRole::new(many),
        Err(GuestError::RolePermissionLimitReached)
    );
    let exact = (1..=MAX_ROLE_PERMISSIONS as u128)
        .map(|n| GuestManagerPermission::ViewHealth(GuestScope::Guest(guest(n))));
    assert_eq!(
        GuestManagerRole::new(exact).unwrap().permissions().len(),
        MAX_ROLE_PERMISSIONS
    );
}
