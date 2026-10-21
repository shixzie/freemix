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
//!
//! [`KeyPicturePacer`] asks for the selected layer's input picture the same
//! way (Protocol 2.97: a `scene_preview_request` naming a non-Scene input
//! answers with its own picture), so auto key samples a picture as large as
//! the preview cap instead of the input thumbnail. The server keeps one
//! preview per session (latest wins), so the designer never has both kinds
//! in flight: each pacer asks only while the other is idle.

use core::time::Duration;

use fm_protocol::{
    CURRENT_PROTOCOL_VERSION, ScenePreviewRequest, TitlePreviewImage, TitlePreviewSample,
    WireInputId,
};
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

    /// Whether a request is in flight.
    #[must_use]
    pub const fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }
}

/// How often the selected layer's input picture is asked for again.
pub const KEY_PICTURE_REFRESH: Duration = Duration::from_secs(3);
/// The request id prefix of the designer's input pictures.
pub const KEY_PICTURE_PREFIX: &str = "design-key-picture-";
/// The request id prefix of the Colour window's pictures of a grade
/// target's source (SW-019).
pub const COLOR_PICTURE_PREFIX: &str = "color-picture-";

/// Paces the requests for the selected layer's input picture that auto key
/// samples (SW-008, see the module documentation): one at a time, at least
/// [`PREVIEW_MIN_INTERVAL`] apart, given up after [`PREVIEW_TIMEOUT`], and
/// again every [`KEY_PICTURE_REFRESH`] (also after a refusal) while the
/// same input stays selected.
///
/// The Colour window paces its pictures of a grade target's source the
/// same way under its own request id prefix ([`Self::with_prefix`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyPicturePacer {
    prefix: &'static str,
    issued: u64,
    in_flight: Option<(String, InputId, Duration)>,
    /// The newest picture and when it was asked for.
    picture: Option<(TitlePreviewImage, Duration)>,
    /// The input last refused and when.
    refused: Option<(InputId, Duration)>,
    last_issue: Option<Duration>,
}

impl Default for KeyPicturePacer {
    fn default() -> Self {
        Self::with_prefix(KEY_PICTURE_PREFIX)
    }
}

impl KeyPicturePacer {
    /// A pacer whose request ids start with `prefix`.
    #[must_use]
    pub const fn with_prefix(prefix: &'static str) -> Self {
        Self {
            prefix,
            issued: 0,
            in_flight: None,
            picture: None,
            refused: None,
            last_issue: None,
        }
    }

    /// Whether `request_id` is one of this pacer's requests (by prefix).
    #[must_use]
    pub fn issues(&self, request_id: &str) -> bool {
        request_id.starts_with(self.prefix)
    }

    /// The request to send now for `input`'s picture at `size`, if one is
    /// due; `allowed` is false when previews cannot be asked for (or the
    /// designer's own preview is in flight).
    pub fn poll(
        &mut self,
        input: Option<InputId>,
        size: (u16, u16),
        now: Duration,
        allowed: bool,
    ) -> Option<ScenePreviewRequest> {
        if let Some((_, _, sent)) = &self.in_flight
            && now.saturating_sub(*sent) >= PREVIEW_TIMEOUT
        {
            self.in_flight = None;
        }
        let input = input?;
        if !allowed || self.in_flight.is_some() {
            return None;
        }
        if self
            .last_issue
            .is_some_and(|last| now.saturating_sub(last) < PREVIEW_MIN_INTERVAL)
        {
            return None;
        }
        let fresh = |at: Duration| now.saturating_sub(at) < KEY_PICTURE_REFRESH;
        let wire = WireInputId::from_domain(input);
        if self
            .picture
            .as_ref()
            .is_some_and(|(image, at)| image.input == wire && fresh(*at))
            || self
                .refused
                .is_some_and(|(refused, at)| refused == input && fresh(at))
        {
            return None;
        }
        self.issued += 1;
        let request_id = format!("{}{}", self.prefix, self.issued);
        self.in_flight = Some((request_id.clone(), input, now));
        self.last_issue = Some(now);
        Some(ScenePreviewRequest {
            protocol: CURRENT_PROTOCOL_VERSION,
            request_id,
            input: wire,
            sample: TitlePreviewSample::Static,
            width: size.0,
            height: size.1,
            clip: None,
        })
    }

    /// `image` arrived: keeps it and returns whether it answers this
    /// pacer's request.
    pub fn received(&mut self, image: &TitlePreviewImage) -> bool {
        match self
            .in_flight
            .take_if(|(request_id, _, _)| *request_id == image.request_id)
        {
            Some((_, _, sent)) => {
                self.picture = Some((image.clone(), sent));
                true
            }
            None => false,
        }
    }

    /// `request_id` was refused, lost, or never sent.
    pub fn failed(&mut self, request_id: &str, now: Duration) {
        if let Some((_, input, _)) = self
            .in_flight
            .take_if(|(pending, _, _)| pending == request_id)
        {
            self.refused = Some((input, now));
        }
    }

    /// Forgets the request in flight (the worker stopped).
    pub fn abandon(&mut self) {
        self.in_flight = None;
    }

    /// Whether a request is in flight.
    #[must_use]
    pub const fn in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    /// The newest picture of `input`, if one arrived.
    #[must_use]
    pub fn picture(&self, input: InputId) -> Option<&TitlePreviewImage> {
        self.picture
            .as_ref()
            .map(|(image, _)| image)
            .filter(|image| image.input == WireInputId::from_domain(input))
    }

    /// Whether `request_id` is a designer input picture.
    #[must_use]
    pub fn owns(request_id: &str) -> bool {
        request_id.starts_with(KEY_PICTURE_PREFIX)
    }
}
