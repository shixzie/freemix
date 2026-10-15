//! Paces the designer's exact scene previews (`scene_preview_request`,
//! Protocol 2.32).
//!
//! The server composes the scene as stored, with its live sources, so the
//! designer refreshes the picture every [`PREVIEW_REFRESH`] while it is
//! open and idle, and at once when the shown scene or the replicated
//! layout changes. Requests are held back while a gesture runs (the
//! outlines follow the pointer locally), at most one is in flight, issues
//! are at least [`PREVIEW_MIN_INTERVAL`] apart, and an unanswered request
//! is given up after [`PREVIEW_TIMEOUT`].

use core::time::Duration;

use fm_protocol::{CURRENT_PROTOCOL_VERSION, ScenePreviewRequest, TitlePreviewSample, WireInputId};
use fm_types::InputId;

pub use crate::title_animation::{PREVIEW_MIN_INTERVAL, PREVIEW_TIMEOUT};

/// How often an idle designer refreshes its picture of live sources.
pub const PREVIEW_REFRESH: Duration = Duration::from_secs(1);
/// The request id prefix of designer previews.
pub const PREVIEW_PREFIX: &str = "design-preview-";

/// What the designer wants pictured: a Scene input showing the scene and a
/// stamp that changes with the replicated layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreviewSubject {
    pub input: InputId,
    pub layout_stamp: u64,
    pub size: (u16, u16),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InFlight {
    request_id: String,
    subject: PreviewSubject,
    sent: Duration,
}

/// The designer's preview pacing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PreviewPacer {
    issued: u64,
    in_flight: Option<InFlight>,
    /// The subject of the picture shown and when it was asked for.
    shown: Option<(PreviewSubject, Duration)>,
    last_issue: Option<Duration>,
}

impl PreviewPacer {
    /// The request to send now for `subject`, if one is due. `held` holds
    /// requests back (a gesture runs); `allowed` is false when previews
    /// cannot be asked for at all.
    pub fn poll(
        &mut self,
        subject: Option<PreviewSubject>,
        now: Duration,
        held: bool,
        allowed: bool,
    ) -> Option<ScenePreviewRequest> {
        if let Some(flight) = &self.in_flight
            && now.saturating_sub(flight.sent) >= PREVIEW_TIMEOUT
        {
            self.in_flight = None;
        }
        let subject = subject?;
        if !allowed || held || self.in_flight.is_some() {
            return None;
        }
        if self
            .last_issue
            .is_some_and(|last| now.saturating_sub(last) < PREVIEW_MIN_INTERVAL)
        {
            return None;
        }
        let due = match self.shown {
            Some((shown, at)) => shown != subject || now.saturating_sub(at) >= PREVIEW_REFRESH,
            None => true,
        };
        if !due {
            return None;
        }
        self.issued += 1;
        let request_id = format!("{PREVIEW_PREFIX}{}", self.issued);
        self.in_flight = Some(InFlight {
            request_id: request_id.clone(),
            subject,
            sent: now,
        });
        self.last_issue = Some(now);
        Some(ScenePreviewRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id,
            input: WireInputId::from_domain(subject.input),
            sample: TitlePreviewSample::Static,
            width: subject.size.0,
            height: subject.size.1,
            clip: None,
        })
    }

    /// The answer to `request_id` arrived: returns whether it is this
    /// pacer's request (and so the picture to show).
    pub fn received(&mut self, request_id: &str) -> bool {
        match self
            .in_flight
            .take_if(|flight| flight.request_id == request_id)
        {
            Some(flight) => {
                self.shown = Some((flight.subject, flight.sent));
                true
            }
            None => false,
        }
    }

    /// `request_id` was refused, lost, or never sent.
    pub fn failed(&mut self, request_id: &str, now: Duration) {
        if let Some(flight) = self
            .in_flight
            .take_if(|flight| flight.request_id == request_id)
        {
            // Wait a full refresh before asking again for the same picture.
            self.shown = Some((flight.subject, now));
        }
    }

    /// Whether `request_id` is a designer preview.
    #[must_use]
    pub fn owns(request_id: &str) -> bool {
        request_id.starts_with(PREVIEW_PREFIX)
    }

    /// How long until the next poll may issue, for repaint scheduling.
    #[must_use]
    pub fn next_due(&self, now: Duration) -> Duration {
        if let Some(flight) = &self.in_flight {
            return (flight.sent + PREVIEW_TIMEOUT).saturating_sub(now);
        }
        match self.shown {
            Some((_, at)) => (at + PREVIEW_REFRESH).saturating_sub(now),
            None => PREVIEW_MIN_INTERVAL,
        }
    }

    /// Forgets the request in flight (the worker stopped).
    pub fn abandon(&mut self) {
        self.in_flight = None;
    }
}
