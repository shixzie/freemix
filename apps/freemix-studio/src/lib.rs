//! Native Studio application, TCP runtime, and optional daemon supervision.

mod animation;
mod args;
#[cfg(feature = "capture-meter-signal")]
mod capture_signal;
mod midi;
mod native;
mod omt;
mod osc;
mod prefs;
mod preset_files;
mod runtime;
mod supervisor;
mod titles;

pub use args::{
    ArgsError, Command, ConnectionConfig, ControlTransport, ExistingConfig, HELP, StudioConfig,
    SupervisedConfig, SupervisedWeb, UiPreferences, parse_args,
};
pub use native::{launch_native, studio_intent_payload};
pub use prefs::{PREFERENCES_ENV, PreferenceStore, SAVE_DELAY, SavedPreferences, preferences_path};
pub use runtime::{LifecycleState, StudioError, StudioRuntime};
pub use supervisor::{
    DaemonSupervisor, ReadinessParseError, ReadinessRecord, RestartPolicy, SupervisorError,
    SupervisorState,
};

use fm_client::ClientConfig;
use fm_protocol::{ClientType, Role};
use fm_types::ProjectId;

pub const DEFAULT_DAEMON: &str = "freemixd";
pub const DEFAULT_LISTEN: &str = "127.0.0.1:0";

/// Constructs protocol client settings for a native Studio session.
#[must_use]
pub fn native_client_config(
    desired_role: Role,
    client_id: impl Into<String>,
    project_id: ProjectId,
) -> ClientConfig {
    ClientConfig::new(
        concat!("freemix-studio ", env!("CARGO_PKG_VERSION")),
        ClientType::Studio,
        desired_role,
        client_id,
        project_id,
    )
}
