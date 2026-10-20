//! Screen capture inputs between the engine and the wire (IN-011).

use std::collections::BTreeMap;

use fm_engine::ShowState;
use fm_protocol::{ScreenCaptureEntry, ScreenCaptureStatus, WireInputId};
use fm_types::InputId;

/// Every screen capture input, ascending, with its configuration and
/// latest status (without error texts). An input nothing has reported on
/// is idle.
pub(super) fn wire_entries(
    show: &ShowState,
    states: &BTreeMap<InputId, ScreenCaptureStatus>,
) -> Vec<ScreenCaptureEntry> {
    show.screen_capture_inputs()
        .map(|(input, config)| ScreenCaptureEntry {
            input: WireInputId::from_domain(input),
            config: config.clone(),
            status: states
                .get(&input)
                .map(|status| ScreenCaptureStatus {
                    last_error: None,
                    audio_error: None,
                    ..status.clone()
                })
                .unwrap_or_default(),
        })
        .collect()
}

/// `status` bounded for the wire: each error one line of at most
/// [`fm_protocol::MAX_SCREEN_CAPTURE_ERROR_BYTES`] bytes, an empty error
/// dropped (and the audio error while the audio is off), and a zero window
/// id dropped.
pub(super) fn wire_status(status: &ScreenCaptureStatus) -> ScreenCaptureStatus {
    let bound = |error: Option<&str>| {
        error
            .map(fm_protocol::bound_screen_capture_error)
            .filter(|error| !error.is_empty())
    };
    ScreenCaptureStatus {
        last_error: bound(status.last_error.as_deref()),
        audio_error: bound(status.audio_error.as_deref())
            .filter(|_| status.audio != fm_protocol::ScreenCaptureAudioState::Off),
        window_id: status.window_id.filter(|id| *id != 0),
        state: match status.state {
            fm_protocol::ScreenCaptureState::Reopening {
                attempt: 0,
                next_in_ms,
            } => fm_protocol::ScreenCaptureState::Reopening {
                attempt: 1,
                next_in_ms,
            },
            state => state,
        },
        ..status.clone()
    }
}
