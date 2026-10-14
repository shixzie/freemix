//! Deterministic offer/answer signaling state machine for one guest session.
//!
//! The machine follows the "perfect negotiation" pattern: a polite endpoint
//! rolls back its own offer on glare, an impolite endpoint ignores the remote
//! offer. Remote ICE candidates are trickled with bounded counts, buffered
//! (bounded) until a remote description exists, and rejected when their ufrag
//! belongs to a previous ICE generation. Reconnect is an ICE restart with
//! caller-supplied fresh credentials. No network or randomness is involved.

use core::fmt;

use crate::sdp::{IceCredentials, LocalEndpoint, Sdp, SdpCandidate, SdpError};

/// Default bound on remote candidates accepted per ICE generation.
pub const DEFAULT_MAX_REMOTE_CANDIDATES: usize = 32;
/// Default bound on candidates buffered before a remote description exists.
pub const DEFAULT_MAX_BUFFERED_CANDIDATES: usize = 16;
/// Default bound on ICE restarts in one session.
pub const DEFAULT_MAX_ICE_RESTARTS: u32 = 8;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NegotiationRole {
    /// Rolls back its own offer when offers collide.
    Polite,
    /// Ignores a colliding remote offer.
    Impolite,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SignalingPhase {
    Idle,
    HaveLocalOffer,
    HaveRemoteOffer,
    /// Descriptions are applied; waiting for ICE/DTLS connectivity.
    Connecting,
    Connected,
    /// Connectivity was lost; awaiting an ICE restart.
    Disconnected,
    Closed,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OfferOutcome {
    Accepted,
    /// Glare on a polite endpoint: the local offer was rolled back.
    AcceptedAfterRollback,
    /// Glare on an impolite endpoint: the remote offer was dropped.
    IgnoredGlare,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CandidateOutcome {
    Applied,
    Buffered,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SignalingError {
    Sdp(SdpError),
    InvalidTransition {
        operation: &'static str,
        phase: SignalingPhase,
    },
    TooManyCandidates {
        maximum: usize,
    },
    TooManyBufferedCandidates {
        maximum: usize,
    },
    UnknownMid(String),
    StaleCandidate,
    DuplicateCandidate,
    CandidateAfterEndOfCandidates,
    IceRestartLimit {
        maximum: u32,
    },
    IceCredentialsReused,
    /// The remote offer restarts ICE; answer with `create_restart_answer`.
    IceRestartCredentialsRequired,
    InvalidConfig(&'static str),
}

impl fmt::Display for SignalingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sdp(error) => write!(formatter, "SDP error: {error}"),
            Self::InvalidTransition { operation, phase } => {
                write!(formatter, "cannot {operation} while signaling is {phase:?}")
            }
            Self::TooManyCandidates { maximum } => {
                write!(formatter, "more than {maximum} remote ICE candidates")
            }
            Self::TooManyBufferedCandidates { maximum } => {
                write!(formatter, "more than {maximum} buffered ICE candidates")
            }
            Self::UnknownMid(mid) => write!(formatter, "candidate names unknown mid {mid}"),
            Self::StaleCandidate => {
                formatter.write_str("candidate belongs to a previous ICE generation")
            }
            Self::DuplicateCandidate => formatter.write_str("candidate already applied"),
            Self::CandidateAfterEndOfCandidates => {
                formatter.write_str("candidate received after end-of-candidates")
            }
            Self::IceRestartLimit { maximum } => {
                write!(formatter, "ICE restart limit {maximum} reached")
            }
            Self::IceRestartCredentialsRequired => {
                formatter.write_str("remote ICE restart requires new local ICE credentials")
            }
            Self::IceCredentialsReused => {
                formatter.write_str("ICE restart must use a new ufrag and password")
            }
            Self::InvalidConfig(reason) => write!(formatter, "invalid signaling config: {reason}"),
        }
    }
}

impl std::error::Error for SignalingError {}

impl From<SdpError> for SignalingError {
    fn from(error: SdpError) -> Self {
        Self::Sdp(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignalingConfig {
    pub role: NegotiationRole,
    pub local: LocalEndpoint,
    pub session_id: u64,
    pub max_remote_candidates: usize,
    pub max_buffered_candidates: usize,
    pub max_ice_restarts: u32,
}

impl SignalingConfig {
    #[must_use]
    pub fn new(role: NegotiationRole, local: LocalEndpoint, session_id: u64) -> Self {
        Self {
            role,
            local,
            session_id,
            max_remote_candidates: DEFAULT_MAX_REMOTE_CANDIDATES,
            max_buffered_candidates: DEFAULT_MAX_BUFFERED_CANDIDATES,
            max_ice_restarts: DEFAULT_MAX_ICE_RESTARTS,
        }
    }
}

/// A remote candidate tagged with its media id.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteCandidate {
    pub mid: String,
    pub candidate: SdpCandidate,
}

#[derive(Clone, Debug)]
pub struct SignalingSession {
    config: SignalingConfig,
    phase: SignalingPhase,
    /// Phase before the current negotiation began (rollback target, and
    /// whether a renegotiation without an ICE change stays connected).
    before_offer: SignalingPhase,
    session_version: u64,
    ice_generation: u32,
    ice_restarts: u32,
    /// Set when the pending negotiation changes ICE credentials.
    ice_changed: bool,
    /// The applied remote offer changed the remote ICE credentials.
    remote_restart_in_offer: bool,
    pending_local_offer: Option<Sdp>,
    local_description: Option<Sdp>,
    remote_description: Option<Sdp>,
    remote_candidates: Vec<RemoteCandidate>,
    buffered_candidates: Vec<RemoteCandidate>,
    remote_end_of_candidates: bool,
    dropped_buffered_candidates: u64,
}

impl SignalingSession {
    /// # Errors
    /// Returns an SDP error for an invalid local endpoint or a config error
    /// for zero candidate bounds.
    pub fn new(config: SignalingConfig) -> Result<Self, SignalingError> {
        config.local.validate()?;
        if config.max_remote_candidates == 0 || config.max_buffered_candidates == 0 {
            return Err(SignalingError::InvalidConfig(
                "candidate bounds must be nonzero",
            ));
        }
        Ok(Self {
            config,
            phase: SignalingPhase::Idle,
            before_offer: SignalingPhase::Idle,
            session_version: 0,
            ice_generation: 0,
            ice_restarts: 0,
            ice_changed: true,
            remote_restart_in_offer: false,
            pending_local_offer: None,
            local_description: None,
            remote_description: None,
            remote_candidates: Vec::new(),
            buffered_candidates: Vec::new(),
            remote_end_of_candidates: false,
            dropped_buffered_candidates: 0,
        })
    }

    #[must_use]
    pub const fn phase(&self) -> SignalingPhase {
        self.phase
    }

    #[must_use]
    pub const fn role(&self) -> NegotiationRole {
        self.config.role
    }

    /// Number of completed or pending ICE restarts (0 for the first connect).
    #[must_use]
    pub const fn ice_generation(&self) -> u32 {
        self.ice_generation
    }

    #[must_use]
    pub fn local_ice(&self) -> &IceCredentials {
        &self.config.local.ice
    }

    #[must_use]
    pub const fn local_description(&self) -> Option<&Sdp> {
        self.local_description.as_ref()
    }

    #[must_use]
    pub const fn pending_local_offer(&self) -> Option<&Sdp> {
        self.pending_local_offer.as_ref()
    }

    #[must_use]
    pub const fn remote_description(&self) -> Option<&Sdp> {
        self.remote_description.as_ref()
    }

    #[must_use]
    pub fn remote_candidates(&self) -> &[RemoteCandidate] {
        &self.remote_candidates
    }

    #[must_use]
    pub fn buffered_candidates(&self) -> &[RemoteCandidate] {
        &self.buffered_candidates
    }

    #[must_use]
    pub const fn remote_end_of_candidates(&self) -> bool {
        self.remote_end_of_candidates
    }

    /// Buffered candidates discarded when flushed against the new remote
    /// description (stale ufrag, unknown mid, or over the bound).
    #[must_use]
    pub const fn dropped_buffered_candidates(&self) -> u64 {
        self.dropped_buffered_candidates
    }

    /// Creates and records a local offer, returning its SDP text.
    ///
    /// # Errors
    /// Fails unless the session is idle or connected (renegotiation).
    pub fn create_offer(&mut self) -> Result<String, SignalingError> {
        self.require(
            "create an offer",
            &[SignalingPhase::Idle, SignalingPhase::Connected],
        )?;
        self.make_offer()
    }

    /// Applies a remote offer.
    ///
    /// # Errors
    /// Returns an SDP parse error, or a transition error while connecting.
    pub fn receive_offer(&mut self, text: &str) -> Result<OfferOutcome, SignalingError> {
        self.require(
            "receive an offer",
            &[
                SignalingPhase::Idle,
                SignalingPhase::HaveLocalOffer,
                SignalingPhase::Connected,
                SignalingPhase::Disconnected,
            ],
        )?;
        let offer = Sdp::parse(text)?;
        let mut outcome = OfferOutcome::Accepted;
        if self.phase == SignalingPhase::HaveLocalOffer {
            match self.config.role {
                NegotiationRole::Impolite => return Ok(OfferOutcome::IgnoredGlare),
                NegotiationRole::Polite => {
                    self.pending_local_offer = None;
                    self.phase = self.before_offer;
                    outcome = OfferOutcome::AcceptedAfterRollback;
                }
            }
        }
        self.before_offer = self.phase;
        let had_remote = self.remote_description.is_some();
        self.remote_restart_in_offer = self.apply_remote(offer) && had_remote;
        self.phase = SignalingPhase::HaveRemoteOffer;
        Ok(outcome)
    }

    /// True when the applied remote offer restarts ICE and the local
    /// credentials have not yet been replaced.
    #[must_use]
    pub fn remote_ice_restart_pending(&self) -> bool {
        self.phase == SignalingPhase::HaveRemoteOffer
            && self.remote_restart_in_offer
            && self
                .local_description
                .as_ref()
                .is_some_and(|local| local.ice() == &self.config.local.ice)
    }

    /// Answers the applied remote offer, returning the answer SDP text.
    ///
    /// # Errors
    /// Fails outside `HaveRemoteOffer`, when no codec is shared, or with
    /// [`SignalingError::IceRestartCredentialsRequired`] when the offer
    /// restarts ICE (answer with [`Self::create_restart_answer`]).
    pub fn create_answer(&mut self) -> Result<String, SignalingError> {
        self.require("create an answer", &[SignalingPhase::HaveRemoteOffer])?;
        if self.remote_ice_restart_pending() {
            return Err(SignalingError::IceRestartCredentialsRequired);
        }
        let offer = self
            .remote_description
            .as_ref()
            .ok_or(SignalingError::InvalidTransition {
                operation: "create an answer",
                phase: self.phase,
            })?;
        let answer = Sdp::answer(
            offer,
            &self.config.local,
            self.config.session_id,
            self.session_version,
        )?;
        self.session_version += 1;
        let text = answer.render();
        self.local_description = Some(answer);
        self.remote_restart_in_offer = false;
        self.finish_negotiation();
        Ok(text)
    }

    /// Answers a remote ICE-restart offer with fresh local credentials.
    ///
    /// # Errors
    /// Fails outside `HaveRemoteOffer`, when either credential is reused, or
    /// when no codec is shared.
    pub fn create_restart_answer(
        &mut self,
        credentials: IceCredentials,
    ) -> Result<String, SignalingError> {
        self.require("create an answer", &[SignalingPhase::HaveRemoteOffer])?;
        let current = &self.config.local.ice;
        if credentials.ufrag() == current.ufrag() || credentials.pwd() == current.pwd() {
            return Err(SignalingError::IceCredentialsReused);
        }
        let previous = core::mem::replace(&mut self.config.local.ice, credentials);
        match self.create_answer() {
            Ok(text) => {
                self.ice_generation += 1;
                Ok(text)
            }
            Err(error) => {
                self.config.local.ice = previous;
                Err(error)
            }
        }
    }

    /// Applies the remote answer to the pending local offer.
    ///
    /// # Errors
    /// Fails outside `HaveLocalOffer` or when the answer does not match.
    pub fn receive_answer(&mut self, text: &str) -> Result<(), SignalingError> {
        self.require("receive an answer", &[SignalingPhase::HaveLocalOffer])?;
        let answer = Sdp::parse(text)?;
        let offer = self
            .pending_local_offer
            .as_ref()
            .ok_or(SignalingError::InvalidTransition {
                operation: "receive an answer",
                phase: self.phase,
            })?;
        answer.validate_answer_for(offer)?;
        self.local_description = self.pending_local_offer.take();
        self.apply_remote(answer);
        self.finish_negotiation();
        Ok(())
    }

    /// Abandons the pending local offer.
    ///
    /// # Errors
    /// Fails outside `HaveLocalOffer`.
    pub fn rollback(&mut self) -> Result<(), SignalingError> {
        self.require("roll back", &[SignalingPhase::HaveLocalOffer])?;
        self.pending_local_offer = None;
        self.phase = self.before_offer;
        Ok(())
    }

    /// Adds a trickled remote candidate (`candidate:...` text) for `mid`.
    ///
    /// # Errors
    /// Returns parse, unknown-mid, stale-generation, duplicate, bound, or
    /// end-of-candidates errors.
    pub fn add_remote_candidate(
        &mut self,
        mid: &str,
        candidate: &str,
    ) -> Result<CandidateOutcome, SignalingError> {
        if self.phase == SignalingPhase::Closed {
            return Err(SignalingError::InvalidTransition {
                operation: "add a candidate",
                phase: self.phase,
            });
        }
        let candidate = SdpCandidate::parse(candidate)?;
        let entry = RemoteCandidate {
            mid: mid.to_owned(),
            candidate,
        };
        // Before any remote description, or while our ICE-restart offer is
        // outstanding, the remote may already trickle next-generation
        // candidates; hold those until the description that names them.
        let awaiting_new_generation = self.phase == SignalingPhase::HaveLocalOffer
            && self.ice_changed
            && entry.candidate.ufrag.as_deref().is_some_and(|ufrag| {
                self.remote_description
                    .as_ref()
                    .is_some_and(|remote| remote.ice().ufrag() != ufrag)
            });
        if self.remote_description.is_none() || awaiting_new_generation {
            if self.buffered_candidates.contains(&entry) {
                return Err(SignalingError::DuplicateCandidate);
            }
            if self.buffered_candidates.len() >= self.config.max_buffered_candidates {
                return Err(SignalingError::TooManyBufferedCandidates {
                    maximum: self.config.max_buffered_candidates,
                });
            }
            self.buffered_candidates.push(entry);
            return Ok(CandidateOutcome::Buffered);
        }
        self.apply_candidate(entry)?;
        Ok(CandidateOutcome::Applied)
    }

    /// Records the remote `end-of-candidates` signal for this generation.
    ///
    /// # Errors
    /// Fails before a remote description exists or after close.
    pub fn remote_end_of_candidates_received(&mut self) -> Result<(), SignalingError> {
        if self.remote_description.is_none() || self.phase == SignalingPhase::Closed {
            return Err(SignalingError::InvalidTransition {
                operation: "end remote candidates",
                phase: self.phase,
            });
        }
        self.remote_end_of_candidates = true;
        Ok(())
    }

    /// ICE/DTLS connectivity established.
    ///
    /// # Errors
    /// Fails outside `Connecting`.
    pub fn ice_connected(&mut self) -> Result<(), SignalingError> {
        self.require("mark connected", &[SignalingPhase::Connecting])?;
        self.phase = SignalingPhase::Connected;
        Ok(())
    }

    /// ICE connectivity lost or failed.
    ///
    /// # Errors
    /// Fails outside `Connecting` or `Connected`.
    pub fn ice_disconnected(&mut self) -> Result<(), SignalingError> {
        self.require(
            "mark disconnected",
            &[SignalingPhase::Connecting, SignalingPhase::Connected],
        )?;
        self.phase = SignalingPhase::Disconnected;
        Ok(())
    }

    /// Starts an ICE restart with fresh credentials and returns the offer.
    ///
    /// # Errors
    /// Fails outside `Connecting`/`Connected`/`Disconnected`, when the restart
    /// limit is reached, or when either credential is reused.
    pub fn restart_ice(&mut self, credentials: IceCredentials) -> Result<String, SignalingError> {
        self.require(
            "restart ICE",
            &[
                SignalingPhase::Connecting,
                SignalingPhase::Connected,
                SignalingPhase::Disconnected,
            ],
        )?;
        if self.ice_restarts >= self.config.max_ice_restarts {
            return Err(SignalingError::IceRestartLimit {
                maximum: self.config.max_ice_restarts,
            });
        }
        let current = &self.config.local.ice;
        if credentials.ufrag() == current.ufrag() || credentials.pwd() == current.pwd() {
            return Err(SignalingError::IceCredentialsReused);
        }
        self.config.local.ice = credentials;
        self.ice_restarts += 1;
        self.ice_generation += 1;
        self.ice_changed = true;
        self.make_offer()
    }

    pub fn close(&mut self) {
        self.phase = SignalingPhase::Closed;
        self.pending_local_offer = None;
        self.buffered_candidates.clear();
    }

    fn require(
        &self,
        operation: &'static str,
        allowed: &[SignalingPhase],
    ) -> Result<(), SignalingError> {
        if allowed.contains(&self.phase) {
            Ok(())
        } else {
            Err(SignalingError::InvalidTransition {
                operation,
                phase: self.phase,
            })
        }
    }

    fn make_offer(&mut self) -> Result<String, SignalingError> {
        let offer = Sdp::offer(
            &self.config.local,
            self.config.session_id,
            self.session_version,
        )?;
        self.session_version += 1;
        let text = offer.render();
        self.pending_local_offer = Some(offer);
        self.before_offer = self.phase;
        self.phase = SignalingPhase::HaveLocalOffer;
        Ok(text)
    }

    /// Installs a remote description, resetting candidate state when the
    /// remote ICE generation changes.
    fn apply_remote(&mut self, description: Sdp) -> bool {
        let remote_restart = self
            .remote_description
            .as_ref()
            .is_none_or(|previous| previous.ice() != description.ice());
        if remote_restart {
            self.remote_candidates.clear();
            self.remote_end_of_candidates = false;
            self.ice_changed = true;
        }
        self.remote_description = Some(description);
        remote_restart
    }

    fn finish_negotiation(&mut self) {
        let resumes = self.before_offer == SignalingPhase::Connected && !self.ice_changed;
        self.phase = if resumes {
            SignalingPhase::Connected
        } else {
            SignalingPhase::Connecting
        };
        self.ice_changed = false;
        self.before_offer = self.phase;
        for entry in core::mem::take(&mut self.buffered_candidates) {
            match self.apply_candidate(entry) {
                Ok(()) | Err(SignalingError::DuplicateCandidate) => {}
                Err(_) => self.dropped_buffered_candidates += 1,
            }
        }
    }

    fn apply_candidate(&mut self, entry: RemoteCandidate) -> Result<(), SignalingError> {
        let remote = self
            .remote_description
            .as_ref()
            .ok_or(SignalingError::StaleCandidate)?;
        if remote.section(&entry.mid).is_none() {
            return Err(SignalingError::UnknownMid(entry.mid));
        }
        if entry
            .candidate
            .ufrag
            .as_deref()
            .is_some_and(|ufrag| ufrag != remote.ice().ufrag())
        {
            return Err(SignalingError::StaleCandidate);
        }
        if self.remote_end_of_candidates {
            return Err(SignalingError::CandidateAfterEndOfCandidates);
        }
        // Bundled sections share one transport: compare candidates only.
        if self
            .remote_candidates
            .iter()
            .any(|existing| existing.candidate == entry.candidate)
        {
            return Err(SignalingError::DuplicateCandidate);
        }
        if self.remote_candidates.len() >= self.config.max_remote_candidates {
            return Err(SignalingError::TooManyCandidates {
                maximum: self.config.max_remote_candidates,
            });
        }
        self.remote_candidates.push(entry);
        Ok(())
    }
}
