//! Role-gated guest manager commands.
//!
//! A host application's authorization adapter decides which
//! [`GuestManagerRole`] an operator holds. This module only checks that the
//! role covers the exact action and resource before mutating guest state, so
//! a denied command never has side effects.

use super::{
    ChatAudience, ChatMessage, ChatMessageId, ChatScope, GuestAudioControl, GuestError, GuestId,
    GuestManager, GuestManagerPermission, GuestScope, GuestSession, GuestSessionId, GuestSlot,
    HealthSeverity, InvitationId, LatencyEchoHealth, RedactionFlag, ReturnAudioSource,
    ReturnVideoSelection, SessionConnection, TalkbackState,
};

/// Maximum number of permissions held by one manager role.
pub const MAX_ROLE_PERMISSIONS: usize = 32;

/// A bounded, deduplicated set of guest manager permissions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuestManagerRole {
    permissions: Vec<GuestManagerPermission>,
}

impl GuestManagerRole {
    /// Creates a role from explicit grants, dropping exact duplicates while
    /// preserving first-seen order.
    ///
    /// # Errors
    ///
    /// Returns [`GuestError::RolePermissionLimitReached`] when more than
    /// [`MAX_ROLE_PERMISSIONS`] distinct permissions are supplied.
    pub fn new(
        permissions: impl IntoIterator<Item = GuestManagerPermission>,
    ) -> Result<Self, GuestError> {
        let mut unique = Vec::new();
        for permission in permissions {
            if unique.contains(&permission) {
                continue;
            }
            if unique.len() == MAX_ROLE_PERMISSIONS {
                return Err(GuestError::RolePermissionLimitReached);
            }
            unique.push(permission);
        }
        Ok(Self {
            permissions: unique,
        })
    }

    /// Full control of invitations, lobby, sessions, returns, talkback, chat,
    /// and health for every guest.
    #[must_use]
    pub fn producer() -> Self {
        Self {
            permissions: vec![
                GuestManagerPermission::ManageInvitations,
                GuestManagerPermission::ManageLobby,
                GuestManagerPermission::ManageSession(GuestScope::All),
                GuestManagerPermission::SelectReturnVideo(GuestScope::All),
                GuestManagerPermission::UseTalkback(GuestScope::All),
                GuestManagerPermission::ModerateChat(ChatScope::All),
                GuestManagerPermission::ViewHealth(GuestScope::All),
            ],
        }
    }

    /// Chat moderation plus read-only health for every guest.
    #[must_use]
    pub fn chat_moderator() -> Self {
        Self {
            permissions: vec![
                GuestManagerPermission::ModerateChat(ChatScope::All),
                GuestManagerPermission::ViewHealth(GuestScope::All),
            ],
        }
    }

    /// Read-only health for every guest.
    #[must_use]
    pub fn observer() -> Self {
        Self {
            permissions: vec![GuestManagerPermission::ViewHealth(GuestScope::All)],
        }
    }

    #[must_use]
    pub fn permissions(&self) -> &[GuestManagerPermission] {
        &self.permissions
    }

    /// Returns whether any held permission covers `required`.
    #[must_use]
    pub fn allows(&self, required: GuestManagerPermission) -> bool {
        self.permissions
            .iter()
            .any(|granted| granted.allows(required))
    }

    fn require(&self, required: GuestManagerPermission) -> Result<(), GuestError> {
        if self.allows(required) {
            Ok(())
        } else {
            Err(GuestError::PermissionDenied(required))
        }
    }

    fn require_chat(&self, audience: ChatAudience) -> Result<(), GuestError> {
        match audience {
            ChatAudience::Group(group_id) => self.require(GuestManagerPermission::ModerateChat(
                ChatScope::Group(group_id),
            )),
            ChatAudience::Private { first, second } => {
                let second_scope =
                    GuestManagerPermission::ModerateChat(ChatScope::PrivateWith(second));
                if self.allows(second_scope) {
                    Ok(())
                } else {
                    self.require(GuestManagerPermission::ModerateChat(
                        ChatScope::PrivateWith(first),
                    ))
                }
            }
        }
    }
}

/// A manager-issued mutation of guest state.
#[derive(Clone, Debug, PartialEq)]
pub enum GuestCommand {
    CreateInvitation {
        id: InvitationId,
        expires_at_ms: u64,
        reserved_slot: Option<GuestSlot>,
    },
    RevokeInvitation(InvitationId),
    AdmitLobbyGuest {
        guest_id: GuestId,
        session_id: GuestSessionId,
    },
    RejectLobbyGuest {
        guest_id: GuestId,
        reason: String,
    },
    EndSession(GuestId),
    SetReturnVideo {
        guest_id: GuestId,
        selection: ReturnVideoSelection,
    },
    SetReturnAudio {
        guest_id: GuestId,
        source: ReturnAudioSource,
    },
    SetGuestAudio {
        guest_id: GuestId,
        control: GuestAudioControl,
    },
    SetTalkback {
        guest_id: GuestId,
        state: TalkbackState,
    },
    RedactChat(ChatMessageId),
}

/// Successful result of [`GuestManager::execute`].
#[derive(Clone, Debug, PartialEq)]
pub enum GuestCommandOutcome {
    Applied,
    Admitted(GuestSlot),
    Ended(GuestSession),
}

/// One guest's health as visible to a manager role.
#[derive(Clone, Debug, PartialEq)]
pub struct GuestHealthRow {
    pub guest_id: GuestId,
    pub display_name: String,
    pub slot: GuestSlot,
    pub connection: SessionConnection,
    pub health: LatencyEchoHealth,
    pub severity: HealthSeverity,
}

impl GuestManager {
    /// Checks that `role` may issue `command` without mutating anything.
    ///
    /// Return video uses [`GuestManagerPermission::SelectReturnVideo`];
    /// return audio, microphone mute/gain, and ending a session use
    /// [`GuestManagerPermission::ManageSession`]. Private chat redaction is
    /// allowed by a `PrivateWith` scope naming either participant.
    ///
    /// # Errors
    ///
    /// Returns [`GuestError::PermissionDenied`] naming the missing permission,
    /// or [`GuestError::ChatMessageNotFound`] when a redaction target is not
    /// retained (its audience, and therefore its scope, is unknown).
    pub fn authorize(
        &self,
        role: &GuestManagerRole,
        command: &GuestCommand,
    ) -> Result<(), GuestError> {
        use GuestManagerPermission as P;
        match command {
            GuestCommand::CreateInvitation { .. } | GuestCommand::RevokeInvitation(_) => {
                role.require(P::ManageInvitations)
            }
            GuestCommand::AdmitLobbyGuest { .. } | GuestCommand::RejectLobbyGuest { .. } => {
                role.require(P::ManageLobby)
            }
            GuestCommand::EndSession(guest_id)
            | GuestCommand::SetReturnAudio { guest_id, .. }
            | GuestCommand::SetGuestAudio { guest_id, .. } => {
                role.require(P::ManageSession(GuestScope::Guest(*guest_id)))
            }
            GuestCommand::SetReturnVideo { guest_id, .. } => {
                role.require(P::SelectReturnVideo(GuestScope::Guest(*guest_id)))
            }
            GuestCommand::SetTalkback { guest_id, .. } => {
                role.require(P::UseTalkback(GuestScope::Guest(*guest_id)))
            }
            GuestCommand::RedactChat(message_id) => {
                let audience = self
                    .chat_messages
                    .iter()
                    .find(|message| message.id == *message_id)
                    .ok_or(GuestError::ChatMessageNotFound)?
                    .audience;
                role.require_chat(audience)
            }
        }
    }

    /// Authorizes and then applies `command` at caller time `now_ms`.
    ///
    /// # Errors
    ///
    /// Returns any [`GuestManager::authorize`] error before mutation, or the
    /// error of the underlying lifecycle operation.
    pub fn execute(
        &mut self,
        role: &GuestManagerRole,
        command: GuestCommand,
        now_ms: u64,
    ) -> Result<GuestCommandOutcome, GuestError> {
        self.authorize(role, &command)?;
        match command {
            GuestCommand::CreateInvitation {
                id,
                expires_at_ms,
                reserved_slot,
            } => self.create_invitation(id, expires_at_ms, reserved_slot, now_ms)?,
            GuestCommand::RevokeInvitation(id) => self.revoke_invitation(id)?,
            GuestCommand::AdmitLobbyGuest {
                guest_id,
                session_id,
            } => {
                return self
                    .admit_lobby_guest(guest_id, session_id, now_ms)
                    .map(GuestCommandOutcome::Admitted);
            }
            GuestCommand::RejectLobbyGuest { guest_id, reason } => {
                self.reject_lobby_guest(guest_id, reason)?;
            }
            GuestCommand::EndSession(guest_id) => {
                return self.end_session(guest_id).map(GuestCommandOutcome::Ended);
            }
            GuestCommand::SetReturnVideo {
                guest_id,
                selection,
            } => self.set_return_video(guest_id, selection)?,
            GuestCommand::SetReturnAudio { guest_id, source } => {
                self.set_return_audio(guest_id, source)?;
            }
            GuestCommand::SetGuestAudio { guest_id, control } => {
                self.set_guest_audio(guest_id, control)?;
            }
            GuestCommand::SetTalkback { guest_id, state } => self.set_talkback(guest_id, state)?,
            GuestCommand::RedactChat(message_id) => {
                self.redact_chat(message_id, RedactionFlag::Moderator)?;
            }
        }
        Ok(GuestCommandOutcome::Applied)
    }

    /// Returns health rows, ordered by slot, for sessions the role may view.
    /// Sessions outside the role's `ViewHealth` scope are omitted.
    #[must_use]
    pub fn health_report(&self, role: &GuestManagerRole) -> Vec<GuestHealthRow> {
        let mut rows: Vec<_> = self
            .sessions
            .values()
            .filter(|session| {
                role.allows(GuestManagerPermission::ViewHealth(GuestScope::Guest(
                    session.identity.id,
                )))
            })
            .map(|session| GuestHealthRow {
                guest_id: session.identity.id,
                display_name: session.identity.display_name().to_owned(),
                slot: session.slot,
                connection: session.connection,
                health: session.health,
                severity: session.health.severity(),
            })
            .collect();
        rows.sort_by_key(|row| row.slot);
        rows
    }

    /// Returns a conversation to a manager whose role may moderate it.
    ///
    /// # Errors
    ///
    /// Returns [`GuestError::PermissionDenied`] when the role's chat scope does
    /// not cover `audience`.
    pub fn chat_messages_for_role(
        &self,
        role: &GuestManagerRole,
        audience: ChatAudience,
    ) -> Result<Vec<&ChatMessage>, GuestError> {
        role.require_chat(audience)?;
        Ok(self.chat_messages_for_manager(audience))
    }
}
