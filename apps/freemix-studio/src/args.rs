use std::{fmt, net::SocketAddr, num::NonZeroU128, path::PathBuf};

use fm_protocol::Role;
use fm_types::ProjectId;
use fm_ui_model::i18n::{HourCycle, Language, Locale};

use crate::{DEFAULT_DAEMON, RestartPolicy, SavedPreferences};

pub const HELP: &str = "\
FreeMix Studio native control application

Usage:
  freemix-studio --project <SHOW.FREEMIX> [OPTIONS] [--diagnose]
  freemix-studio --connect <ADDR> --project-id <ID> [OPTIONS] [--diagnose]
  freemix-studio --web-connect <ADDR> --project-id <ID> [--diagnose]
  freemix-studio --help
  freemix-studio --version

Connection options:
  --project <PATH>       Launch and supervise a daemon for this project bundle
  --daemon <PATH>        Daemon executable [default: freemixd]
  --listen <ADDR>        Supervised daemon listen address [default: 127.0.0.1:0]
  --web-listen <ADDR>    Let phones and tablets pair with the supervised daemon's
                         web pages at this address (for example 0.0.0.0:8443),
                         over TLS with a certificate from this computer's
                         FreeMix certificate authority (install it on each
                         phone from Output > Web access)
  --web-tls-cert <PEM>   Serve your own certificate chain instead (for example
                         from mkcert); reloaded when the file changes
  --web-tls-key <PEM>    The key of --web-tls-cert (readable by you only)
  --web-mdns-name <NAME> Advertise the pages as NAME.local over mDNS
                         [default: freemix-<computer name>]
  --no-web-mdns          Advertise nothing over mDNS
  --connect <ADDR>       Connect to an existing daemon
  --project-id <ID>      Expected full nonzero u128 project ID in existing mode

Client options:
  --client-id <ID>       Protocol client identity [default: freemix-studio]
  --role <ROLE>          viewer, graphics, audio, replay, operator, or admin
                         [default: operator; viewer with --diagnose]
  --max-restarts <COUNT> Maximum supervised daemon restarts [default: 3]
  --osc-listen <ADDR>    Receive OSC control on a loopback address and nonzero port
  --web-connect <ADDR>   Connect to an existing loopback WebSocket daemon
  --diagnose             Run the one-shot connection diagnostic instead of Studio

Interface options:
  --lang <LANG>          Console language: en or es
                         [default: the saved choice, else LC_ALL, LC_MESSAGES,
                         or LANG; else en]
  --clock <CYCLE>        Top-bar clock: 24h or 12h [default: the saved choice,
                         else 24h]

  View › Language and View › 12-hour clock are saved to studio.conf in the
  user configuration directory (or the FREEMIX_STUDIO_PREFS path); --lang and
  --clock override them for one session without changing the saved choice.
  -h, --help             Print help
  -V, --version          Print version";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Help,
    Version,
    Open(StudioConfig),
    Diagnose(StudioConfig),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControlTransport {
    Tcp,
    WebSocket,
}

/// Settings common to both runtime ownership modes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StudioConfig {
    pub connection: ConnectionConfig,
    pub client_id: String,
    pub desired_role: Role,
    pub restart_policy: RestartPolicy,
    pub osc_listen: Option<SocketAddr>,
    pub transport: ControlTransport,
    /// Local console presentation; never sent to the daemon.
    pub ui: UiPreferences,
}

/// Console language and clock chosen on the command line.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct UiPreferences {
    /// `--lang`; `None` defers to the environment.
    pub language: Option<Language>,
    /// `--clock`; `None` keeps the 24-hour broadcast clock.
    pub hour_cycle: Option<HourCycle>,
}

impl UiPreferences {
    /// The console locale without saved preferences; see
    /// [`Self::locale_with`].
    #[must_use]
    pub fn locale(self, env: impl Fn(&str) -> Option<String>) -> Locale {
        self.locale_with(&SavedPreferences::default(), env)
    }

    /// The console locale: `--lang`, else the saved language, else the
    /// first of `LC_ALL`, `LC_MESSAGES`, and `LANG` that is set (POSIX
    /// precedence) when it names a language with a catalog, else English;
    /// and `--clock`, else the saved clock, else 24 hours.
    #[must_use]
    pub fn locale_with(
        self,
        saved: &SavedPreferences,
        env: impl Fn(&str) -> Option<String>,
    ) -> Locale {
        let language = self.language.or(saved.language).unwrap_or_else(|| {
            ["LC_ALL", "LC_MESSAGES", "LANG"]
                .into_iter()
                .find_map(|name| env(name).filter(|value| !value.is_empty()))
                .and_then(|value| Language::from_tag(&value))
                .unwrap_or_default()
        });
        Locale::new(language)
            .with_hour_cycle(self.hour_cycle.or(saved.hour_cycle).unwrap_or_default())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConnectionConfig {
    Supervised(SupervisedConfig),
    Existing(ExistingConfig),
}

/// A Studio-owned daemon. The project identity comes from daemon readiness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisedConfig {
    pub project_bundle: PathBuf,
    pub daemon_executable: PathBuf,
    pub listen: SocketAddr,
    /// `--web-listen`: the supervised daemon also serves the phone and
    /// tablet pages here over TLS with a certificate from its local CA
    /// (RC-007); devices pair from Output > Web access.
    pub web_listen: Option<SocketAddr>,
    /// The gateway's own certificate and mDNS name, with `web_listen`.
    pub web: SupervisedWeb,
}

/// `--web-tls-cert`/`--web-tls-key` and the mDNS options, passed to the
/// supervised daemon with `--web-listen`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SupervisedWeb {
    /// The operator's certificate chain and key; the daemon's local CA
    /// issues the certificate without them.
    pub tls_files: Option<(PathBuf, PathBuf)>,
    /// `--web-mdns-name`: the advertised `.local` label.
    pub mdns_name: Option<String>,
    /// `--no-web-mdns`.
    pub no_mdns: bool,
}

impl SupervisedConfig {
    #[must_use]
    pub fn new(project_bundle: impl Into<PathBuf>) -> Self {
        Self {
            project_bundle: project_bundle.into(),
            daemon_executable: PathBuf::from(DEFAULT_DAEMON),
            listen: default_listen(),
            web_listen: None,
            web: SupervisedWeb::default(),
        }
    }
}

/// An independently-owned daemon with an identity that must match its handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExistingConfig {
    pub address: SocketAddr,
    pub expected_project_id: ProjectId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArgsError {
    MissingValue(&'static str),
    MissingRequired(&'static str),
    DuplicateOption(&'static str),
    BlankValue(&'static str),
    InvalidAddress {
        option: &'static str,
        value: String,
    },
    InvalidProjectId(String),
    InvalidRestartCount(String),
    InvalidRole(String),
    InvalidLanguage(String),
    InvalidClock(String),
    ConflictingModes,
    SupervisedOnly(&'static str),
    ExistingOnly(&'static str),
    NonLoopbackListen(SocketAddr),
    InvalidOscListen(SocketAddr),
    OscUnavailableInDiagnose,
    WebConnectConflicting,
    InvalidWebConnect(SocketAddr),
    /// A web gateway option without `--web-listen`.
    WebListenOnly(&'static str),
    /// `--web-tls-cert` and `--web-tls-key` go together.
    WebTlsIncomplete,
    /// `--web-mdns-name` with `--no-web-mdns`.
    WebMdnsConflicting,
    UnknownArgument(String),
}

impl fmt::Display for ArgsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingValue(option) => write!(formatter, "missing value for {option}"),
            Self::MissingRequired(option) => {
                write!(formatter, "required option {option} is missing")
            }
            Self::DuplicateOption(option) => {
                write!(formatter, "option {option} was provided more than once")
            }
            Self::BlankValue(option) => write!(formatter, "value for {option} must not be blank"),
            Self::InvalidAddress { option, value } => {
                write!(formatter, "invalid socket address `{value}` for {option}")
            }
            Self::InvalidProjectId(value) => {
                write!(formatter, "project ID `{value}` must be a nonzero u128")
            }
            Self::InvalidRestartCount(value) => {
                write!(formatter, "restart count `{value}` must be a u8")
            }
            Self::InvalidRole(role) => write!(formatter, "unknown role `{role}`"),
            Self::InvalidLanguage(value) => {
                write!(formatter, "unknown language `{value}`; expected en or es")
            }
            Self::InvalidClock(value) => {
                write!(formatter, "unknown clock `{value}`; expected 24h or 12h")
            }
            Self::ConflictingModes => {
                formatter.write_str("--project and --connect cannot be used together")
            }
            Self::SupervisedOnly(option) => {
                write!(formatter, "option {option} is only valid with --project")
            }
            Self::ExistingOnly(option) => {
                write!(formatter, "option {option} is only valid with --connect")
            }
            Self::NonLoopbackListen(address) => write!(
                formatter,
                "supervised listen address must be loopback, got {address}"
            ),
            Self::InvalidOscListen(address) => write!(
                formatter,
                "OSC listen address must be loopback with a nonzero port, got {address}"
            ),
            Self::OscUnavailableInDiagnose => {
                formatter.write_str("--osc-listen cannot be used with --diagnose")
            }
            Self::WebConnectConflicting => formatter.write_str(
                "--web-connect cannot be used with --project, --connect, --daemon, or --listen",
            ),
            Self::InvalidWebConnect(address) => write!(
                formatter,
                "WebSocket address must be loopback with a nonzero port, got {address}"
            ),
            Self::WebListenOnly(option) => {
                write!(formatter, "option {option} is only valid with --web-listen")
            }
            Self::WebTlsIncomplete => {
                formatter.write_str("--web-tls-cert and --web-tls-key must be given together")
            }
            Self::WebMdnsConflicting => {
                formatter.write_str("--web-mdns-name cannot be used with --no-web-mdns")
            }
            Self::UnknownArgument(argument) => write!(formatter, "unknown argument `{argument}`"),
        }
    }
}

impl std::error::Error for ArgsError {}

/// Parses process arguments after the executable name.
///
/// # Errors
///
/// Returns an error for unknown, duplicate, conflicting, missing, or invalid options.
#[allow(clippy::too_many_lines)]
pub fn parse_args(arguments: impl IntoIterator<Item = String>) -> Result<Command, ArgsError> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    if arguments.as_slice() == ["--help"] || arguments.as_slice() == ["-h"] {
        return Ok(Command::Help);
    }
    if arguments.as_slice() == ["--version"] || arguments.as_slice() == ["-V"] {
        return Ok(Command::Version);
    }

    let mut project = None;
    let mut daemon = None;
    let mut listen = None;
    let mut connect = None;
    let mut project_id = None;
    let mut client_id = None;
    let mut role = None;
    let mut maximum_restarts = None;
    let mut osc_listen = None;
    let mut diagnose = None;
    let mut web_connect = None;
    let mut web_listen = None;
    let mut web_tls_cert = None;
    let mut web_tls_key = None;
    let mut web_mdns_name = None;
    let mut no_web_mdns = None;
    let mut language = None;
    let mut hour_cycle = None;
    let mut arguments = arguments.into_iter();
    while let Some(option) = arguments.next() {
        match option.as_str() {
            "--project" => set_once(
                &mut project,
                PathBuf::from(nonblank(&mut arguments, "--project")?),
                "--project",
            )?,
            "--daemon" => set_once(
                &mut daemon,
                PathBuf::from(nonblank(&mut arguments, "--daemon")?),
                "--daemon",
            )?,
            "--listen" => {
                let value = required(&mut arguments, "--listen")?;
                let address = socket_address("--listen", &value)?;
                set_once(&mut listen, address, "--listen")?;
            }
            "--connect" => {
                let value = required(&mut arguments, "--connect")?;
                let address = socket_address("--connect", &value)?;
                set_once(&mut connect, address, "--connect")?;
            }
            "--project-id" => {
                let value = required(&mut arguments, "--project-id")?;
                set_once(&mut project_id, parse_project_id(&value)?, "--project-id")?;
            }
            "--client-id" => set_once(
                &mut client_id,
                nonblank(&mut arguments, "--client-id")?,
                "--client-id",
            )?,
            "--role" => {
                let value = required(&mut arguments, "--role")?;
                set_once(&mut role, parse_role(&value)?, "--role")?;
            }
            "--max-restarts" => {
                let value = required(&mut arguments, "--max-restarts")?;
                let count = value
                    .parse()
                    .map_err(|_| ArgsError::InvalidRestartCount(value))?;
                set_once(&mut maximum_restarts, count, "--max-restarts")?;
            }
            "--osc-listen" => {
                let value = required(&mut arguments, "--osc-listen")?;
                let address = socket_address("--osc-listen", &value)?;
                set_once(&mut osc_listen, address, "--osc-listen")?;
            }
            "--diagnose" => set_once(&mut diagnose, (), "--diagnose")?,
            "--web-listen" => {
                let value = required(&mut arguments, "--web-listen")?;
                let address = socket_address("--web-listen", &value)?;
                set_once(&mut web_listen, address, "--web-listen")?;
            }
            "--web-tls-cert" => set_once(
                &mut web_tls_cert,
                PathBuf::from(nonblank(&mut arguments, "--web-tls-cert")?),
                "--web-tls-cert",
            )?,
            "--web-tls-key" => set_once(
                &mut web_tls_key,
                PathBuf::from(nonblank(&mut arguments, "--web-tls-key")?),
                "--web-tls-key",
            )?,
            "--web-mdns-name" => set_once(
                &mut web_mdns_name,
                nonblank(&mut arguments, "--web-mdns-name")?,
                "--web-mdns-name",
            )?,
            "--no-web-mdns" => set_once(&mut no_web_mdns, (), "--no-web-mdns")?,
            "--web-connect" => {
                let value = required(&mut arguments, "--web-connect")?;
                let address = socket_address("--web-connect", &value)?;
                set_once(&mut web_connect, address, "--web-connect")?;
            }
            "--lang" => {
                let value = required(&mut arguments, "--lang")?;
                let parsed = Language::from_tag(&value).ok_or(ArgsError::InvalidLanguage(value))?;
                set_once(&mut language, parsed, "--lang")?;
            }
            "--clock" => {
                let value = required(&mut arguments, "--clock")?;
                let parsed = HourCycle::from_flag(&value).ok_or(ArgsError::InvalidClock(value))?;
                set_once(&mut hour_cycle, parsed, "--clock")?;
            }
            _ => return Err(ArgsError::UnknownArgument(option)),
        }
    }

    if let Some(address) = web_connect {
        if project.is_some() || connect.is_some() || daemon.is_some() || listen.is_some() {
            return Err(ArgsError::WebConnectConflicting);
        }
        if !address.ip().is_loopback() || address.port() == 0 {
            return Err(ArgsError::InvalidWebConnect(address));
        }
    }
    let connection = if let Some(address) = web_connect {
        ConnectionConfig::Existing(ExistingConfig {
            address,
            expected_project_id: project_id.ok_or(ArgsError::MissingRequired("--project-id"))?,
        })
    } else {
        connection_config(project, daemon, listen, connect, project_id)?
    };
    if web_listen.is_none() {
        for (option, given) in [
            ("--web-tls-cert", web_tls_cert.is_some()),
            ("--web-tls-key", web_tls_key.is_some()),
            ("--web-mdns-name", web_mdns_name.is_some()),
            ("--no-web-mdns", no_web_mdns.is_some()),
        ] {
            if given {
                return Err(ArgsError::WebListenOnly(option));
            }
        }
    }
    let tls_files = match (web_tls_cert, web_tls_key) {
        (Some(cert), Some(key)) => Some((cert, key)),
        (None, None) => None,
        _ => return Err(ArgsError::WebTlsIncomplete),
    };
    if web_mdns_name.is_some() && no_web_mdns.is_some() {
        return Err(ArgsError::WebMdnsConflicting);
    }
    let web = SupervisedWeb {
        tls_files,
        mdns_name: web_mdns_name,
        no_mdns: no_web_mdns.is_some(),
    };
    let connection = match (connection, web_listen) {
        (ConnectionConfig::Supervised(supervised), web_listen) => {
            ConnectionConfig::Supervised(SupervisedConfig {
                web_listen,
                web,
                ..supervised
            })
        }
        (existing, None) => existing,
        (ConnectionConfig::Existing(_), Some(_)) => {
            return Err(ArgsError::SupervisedOnly("--web-listen"));
        }
    };
    if diagnose.is_some() && osc_listen.is_some() {
        return Err(ArgsError::OscUnavailableInDiagnose);
    }
    if let Some(address) = osc_listen
        && (!address.ip().is_loopback() || address.port() == 0)
    {
        return Err(ArgsError::InvalidOscListen(address));
    }

    let config = StudioConfig {
        connection,
        client_id: client_id.unwrap_or_else(|| "freemix-studio".to_owned()),
        desired_role: role.unwrap_or(if diagnose.is_some() {
            Role::Viewer
        } else {
            Role::Operator
        }),
        restart_policy: RestartPolicy {
            maximum_restarts: maximum_restarts.unwrap_or(3),
        },
        osc_listen,
        transport: if web_connect.is_some() {
            ControlTransport::WebSocket
        } else {
            ControlTransport::Tcp
        },
        ui: UiPreferences {
            language,
            hour_cycle,
        },
    };
    Ok(if diagnose.is_some() {
        Command::Diagnose(config)
    } else {
        Command::Open(config)
    })
}

fn connection_config(
    project: Option<PathBuf>,
    daemon: Option<PathBuf>,
    listen: Option<SocketAddr>,
    connect: Option<SocketAddr>,
    project_id: Option<ProjectId>,
) -> Result<ConnectionConfig, ArgsError> {
    match (project, connect) {
        (Some(_), Some(_)) => Err(ArgsError::ConflictingModes),
        (Some(project_bundle), None) => {
            if project_id.is_some() {
                return Err(ArgsError::ExistingOnly("--project-id"));
            }
            let listen = listen.unwrap_or_else(default_listen);
            if !listen.ip().is_loopback() {
                return Err(ArgsError::NonLoopbackListen(listen));
            }
            Ok(ConnectionConfig::Supervised(SupervisedConfig {
                project_bundle,
                daemon_executable: daemon.unwrap_or_else(|| PathBuf::from(DEFAULT_DAEMON)),
                listen,
                web_listen: None,
                web: SupervisedWeb::default(),
            }))
        }
        (None, Some(address)) => {
            if daemon.is_some() {
                return Err(ArgsError::SupervisedOnly("--daemon"));
            }
            if listen.is_some() {
                return Err(ArgsError::SupervisedOnly("--listen"));
            }
            Ok(ConnectionConfig::Existing(ExistingConfig {
                address,
                expected_project_id: project_id
                    .ok_or(ArgsError::MissingRequired("--project-id"))?,
            }))
        }
        (None, None) => Err(ArgsError::MissingRequired("--project or --connect")),
    }
}

fn required(
    arguments: &mut impl Iterator<Item = String>,
    option: &'static str,
) -> Result<String, ArgsError> {
    arguments.next().ok_or(ArgsError::MissingValue(option))
}

fn nonblank(
    arguments: &mut impl Iterator<Item = String>,
    option: &'static str,
) -> Result<String, ArgsError> {
    let value = required(arguments, option)?;
    if value.trim().is_empty() {
        Err(ArgsError::BlankValue(option))
    } else {
        Ok(value)
    }
}

fn set_once<T>(slot: &mut Option<T>, value: T, option: &'static str) -> Result<(), ArgsError> {
    if slot.replace(value).is_some() {
        Err(ArgsError::DuplicateOption(option))
    } else {
        Ok(())
    }
}

fn socket_address(option: &'static str, value: &str) -> Result<SocketAddr, ArgsError> {
    value.parse().map_err(|_| ArgsError::InvalidAddress {
        option,
        value: value.to_owned(),
    })
}

fn parse_project_id(value: &str) -> Result<ProjectId, ArgsError> {
    value
        .parse::<NonZeroU128>()
        .map(ProjectId::new)
        .map_err(|_| ArgsError::InvalidProjectId(value.to_owned()))
}

fn parse_role(value: &str) -> Result<Role, ArgsError> {
    match value {
        "viewer" => Ok(Role::Viewer),
        "graphics" => Ok(Role::Graphics),
        "audio" => Ok(Role::Audio),
        "replay" => Ok(Role::Replay),
        "operator" => Ok(Role::Operator),
        "admin" => Ok(Role::Admin),
        _ => Err(ArgsError::InvalidRole(value.to_owned())),
    }
}

fn default_listen() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 0))
}
