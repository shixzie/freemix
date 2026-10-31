// Public only so the daemon binary can use these items; this crate is not a
// stable API surface.
#![allow(
    clippy::must_use_candidate,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::return_self_not_must_use
)]

//! The daemon's command line: the serve options, display-output shorthand,
//! recording bounds, and web gateway settings.

use std::{
    error::Error,
    fmt,
    net::{IpAddr, SocketAddr},
    num::{NonZeroU32, NonZeroU128},
    path::{Path, PathBuf},
    time::Duration,
};

use fm_io_api::display::{DisplayOutputConfig, DisplayOutputSet, DisplaySelector};

/// The default control listener: one ephemeral loopback port.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:0";
/// The daemon's recorder slots.
pub const RECORDER_SLOTS: usize = 2;

/// A command-line failure whose message is already operator-facing.
pub type AppResult<T> = Result<T, Box<dyn Error>>;

/// A failure with a stable, already-worded message.
#[derive(Debug)]
pub struct AppFailure(pub String);

impl fmt::Display for AppFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for AppFailure {}

/// The most `--web-host` names.
const MAX_WEB_HOSTS: usize = 8;

/// How the gateway serves its pages and WebSocket.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum WebTls {
    /// Plain HTTP and `ws://`: loopback addresses only.
    #[default]
    Off,
    /// A leaf issued by the per-install local CA kept in the state
    /// directory (`--web-tls`), whose root phones install once.
    LocalCa,
    /// The operator's own certificate chain and key (`--web-tls-cert`,
    /// `--web-tls-key`).
    Files { cert: PathBuf, key: PathBuf },
}

/// `--web-tls`, `--web-tls-cert`, `--web-tls-key`, `--web-state-dir`,
/// `--web-host`, `--web-mdns-name`, and `--no-web-mdns`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WebSettings {
    pub tls: WebTls,
    pub state_dir: Option<PathBuf>,
    /// Extra DNS names (or addresses) phones may use to reach the gateway.
    /// The first one, when given, names the pairing URL.
    pub hosts: Vec<String>,
    /// `--web-mdns-name`: the `.local` label to advertise instead of the
    /// one derived from the computer's name.
    pub mdns_name: Option<String>,
    /// `--no-web-mdns`: advertise nothing over mDNS.
    pub no_mdns: bool,
    local_ca: bool,
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
}

impl WebSettings {
    /// Consumes `option` (and its value from `arguments`) when it is a web
    /// gateway option; returns whether it was.
    ///
    /// # Errors
    ///
    /// A missing or invalid value, or a repeated option.
    pub fn take_option(
        &mut self,
        option: &str,
        arguments: &mut impl Iterator<Item = String>,
    ) -> AppResult<bool> {
        let (name, inline) = option
            .split_once('=')
            .map_or((option, None), |(name, value)| {
                (name, Some(value.to_owned()))
            });
        let mut value = |name: &str| -> AppResult<String> {
            inline
                .clone()
                .or_else(|| arguments.next())
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("missing value for {name}").into())
        };
        match name {
            "--web-tls" if inline.is_none() => {
                if self.local_ca {
                    return Err("duplicate option `--web-tls`".into());
                }
                self.local_ca = true;
            }
            "--no-web-mdns" if inline.is_none() => {
                if self.no_mdns {
                    return Err("duplicate option `--no-web-mdns`".into());
                }
                self.no_mdns = true;
            }
            "--web-mdns-name" => {
                if self.mdns_name.is_some() {
                    return Err("duplicate option `--web-mdns-name`".into());
                }
                let name = value(name)?.to_ascii_lowercase();
                let label = name.strip_suffix(".local").unwrap_or(&name).to_owned();
                if !valid_label(&label) {
                    return Err(format!(
                        "invalid --web-mdns-name `{name}`: one label of letters, digits, and hyphens"
                    )
                    .into());
                }
                self.mdns_name = Some(label);
            }
            "--web-tls-cert" => {
                if self.cert.is_some() {
                    return Err("duplicate option `--web-tls-cert`".into());
                }
                self.cert = Some(PathBuf::from(value(name)?));
            }
            "--web-tls-key" => {
                if self.key.is_some() {
                    return Err("duplicate option `--web-tls-key`".into());
                }
                self.key = Some(PathBuf::from(value(name)?));
            }
            "--web-state-dir" => {
                if self.state_dir.is_some() {
                    return Err("duplicate option `--web-state-dir`".into());
                }
                self.state_dir = Some(PathBuf::from(value(name)?));
            }
            "--web-host" => {
                let host = value(name)?.to_ascii_lowercase();
                if !valid_host_name(&host) {
                    return Err(format!("invalid --web-host `{host}`").into());
                }
                if self.hosts.len() == MAX_WEB_HOSTS {
                    return Err(format!("at most {MAX_WEB_HOSTS} --web-host names").into());
                }
                if !self.hosts.contains(&host) {
                    self.hosts.push(host);
                }
            }
            _ => return Ok(false),
        }
        self.tls = match (self.local_ca, &self.cert, &self.key) {
            (true, None, None) => WebTls::LocalCa,
            (false, Some(cert), Some(key)) => WebTls::Files {
                cert: cert.clone(),
                key: key.clone(),
            },
            // Incomplete or conflicting until validated.
            _ => WebTls::Off,
        };
        Ok(true)
    }

    /// Checks the settings against `--web-listen`.
    ///
    /// Plain HTTP is loopback-only: a phone on the LAN must reach the gateway
    /// over TLS, so its token and pairing code never cross the network in
    /// the clear.
    ///
    /// # Errors
    ///
    /// Conflicting or incomplete TLS options, web options without
    /// `--web-listen`, or a non-loopback address without TLS.
    pub fn validate(&self, web_listen: Option<SocketAddr>) -> AppResult<()> {
        if self.local_ca && (self.cert.is_some() || self.key.is_some()) {
            return Err("--web-tls cannot be combined with --web-tls-cert/--web-tls-key".into());
        }
        if self.cert.is_some() != self.key.is_some() {
            return Err("--web-tls-cert and --web-tls-key must be given together".into());
        }
        let Some(address) = web_listen else {
            if *self != Self::default() {
                return Err("web gateway options require --web-listen".into());
            }
            return Ok(());
        };
        if !address.ip().is_loopback() && self.tls == WebTls::Off {
            return Err(
                "--web-listen on a non-loopback address requires TLS (--web-tls, or --web-tls-cert with --web-tls-key); plain HTTP is loopback-only"
                    .into(),
            );
        }
        if self.mdns_name.is_some() && self.no_mdns {
            return Err("--web-mdns-name cannot be combined with --no-web-mdns".into());
        }
        if self.mdns_name.is_some() && address.ip().is_loopback() {
            return Err(
                "--web-mdns-name needs a LAN --web-listen address (mDNS is never advertised on loopback)"
                    .into(),
            );
        }
        Ok(())
    }

    /// Whether the gateway at `address` advertises itself over mDNS: on a
    /// LAN address, unless `--no-web-mdns`.
    pub fn advertises(&self, address: SocketAddr) -> bool {
        !self.no_mdns && !address.ip().is_loopback()
    }

    /// The directory holding the generated certificate and the paired
    /// devices: `--web-state-dir`, else the platform's per-user state
    /// directory.
    pub fn resolved_state_dir(&self, env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
        if let Some(directory) = &self.state_dir {
            return Some(directory.clone());
        }
        let set = |name: &str| env(name).filter(|value| !value.is_empty());
        if cfg!(windows) {
            set("LOCALAPPDATA").map(|base| PathBuf::from(base).join("FreeMix").join("web"))
        } else if cfg!(target_os = "macos") {
            set("HOME").map(|home| {
                PathBuf::from(home)
                    .join("Library")
                    .join("Application Support")
                    .join("FreeMix")
                    .join("web")
            })
        } else {
            set("XDG_STATE_HOME")
                .map(PathBuf::from)
                .filter(|base| base.is_absolute())
                .or_else(|| {
                    set("HOME").map(|home| PathBuf::from(home).join(".local").join("state"))
                })
                .map(|base| base.join("freemix").join("web"))
        }
    }
}

/// One mDNS label: 1..=63 letters, digits, and hyphens, not at an edge.
fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label.len() <= 63
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// A DNS name (`[a-z0-9-]` labels) or an IP address literal.
pub fn valid_host_name(host: &str) -> bool {
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::{WebSettings, WebTls};

    fn parse(options: &[&str]) -> Result<WebSettings, String> {
        let mut settings = WebSettings::default();
        let mut arguments = options.iter().map(|option| (*option).to_owned());
        while let Some(option) = arguments.next() {
            if !settings
                .take_option(&option, &mut arguments)
                .map_err(|error| error.to_string())?
            {
                return Err(format!("not a web option: {option}"));
            }
        }
        Ok(settings)
    }

    #[test]
    fn plain_http_is_loopback_only() {
        let plain = parse(&[]).unwrap();
        assert!(plain.validate(Some("127.0.0.1:0".parse().unwrap())).is_ok());
        assert!(plain.validate(Some("[::1]:0".parse().unwrap())).is_ok());
        for address in ["0.0.0.0:0", "192.168.1.20:8443", "[::]:0"] {
            let error = plain
                .validate(Some(address.parse().unwrap()))
                .unwrap_err()
                .to_string();
            assert!(error.contains("requires TLS"), "{address}: {error}");
        }
        let tls = parse(&["--web-tls"]).unwrap();
        assert_eq!(tls.tls, WebTls::LocalCa);
        assert!(tls.validate(Some("0.0.0.0:0".parse().unwrap())).is_ok());
    }

    #[test]
    fn tls_options_are_complete_and_exclusive() {
        let files = parse(&["--web-tls-cert", "c.pem", "--web-tls-key=k.pem"]).unwrap();
        assert!(matches!(files.tls, WebTls::Files { .. }));
        assert!(files.validate(Some("0.0.0.0:0".parse().unwrap())).is_ok());
        let half = parse(&["--web-tls-cert", "c.pem"]).unwrap();
        assert!(half.validate(Some("0.0.0.0:0".parse().unwrap())).is_err());
        let both = parse(&["--web-tls", "--web-tls-cert", "c", "--web-tls-key", "k"]).unwrap();
        assert!(both.validate(Some("0.0.0.0:0".parse().unwrap())).is_err());
        assert!(parse(&["--web-tls", "--web-tls"]).is_err());
        assert!(parse(&["--web-tls-cert"]).is_err());
        // Web options mean nothing without the gateway.
        assert!(parse(&["--web-tls"]).unwrap().validate(None).is_err());
        assert!(parse(&[]).unwrap().validate(None).is_ok());
    }

    #[test]
    fn mdns_names_are_one_label_on_a_lan_address() {
        let address = "0.0.0.0:8443".parse().unwrap();
        let lan = Some(address);
        let named = parse(&["--web-tls", "--web-mdns-name", "Studio-A.local"]).unwrap();
        assert_eq!(named.mdns_name.as_deref(), Some("studio-a"));
        assert!(named.validate(lan).is_ok());
        assert!(named.advertises(address));
        assert!(
            named
                .validate(Some("127.0.0.1:0".parse().unwrap()))
                .is_err()
        );
        for bad in ["a.b", "-x", "evil host", ""] {
            assert!(parse(&["--web-mdns-name", bad]).is_err(), "{bad:?}");
        }
        let quiet = parse(&["--web-tls", "--no-web-mdns"]).unwrap();
        assert!(quiet.validate(lan).is_ok());
        assert!(!quiet.advertises(address));
        let both = parse(&["--web-tls", "--no-web-mdns", "--web-mdns-name", "x"]).unwrap();
        assert!(both.validate(lan).is_err());
        assert!(!WebSettings::default().advertises("127.0.0.1:1".parse().unwrap()));
    }

    #[test]
    fn host_names_are_validated() {
        let hosts = parse(&["--web-host", "Studio.local", "--web-host", "10.0.0.5"]).unwrap();
        assert_eq!(hosts.hosts, ["studio.local", "10.0.0.5"]);
        assert!(parse(&["--web-host", "evil host"]).is_err());
        assert!(parse(&["--web-host", "a..b"]).is_err());
        assert!(parse(&["--web-host", "-x.local"]).is_err());
    }

    #[test]
    fn the_state_directory_defaults_per_platform() {
        let explicit = parse(&["--web-state-dir", "/srv/fm"]).unwrap();
        assert_eq!(
            explicit.resolved_state_dir(|_| None).unwrap(),
            std::path::PathBuf::from("/srv/fm")
        );
        let (variable, base) = if cfg!(windows) {
            ("LOCALAPPDATA", r"C:\Users\op\AppData\Local")
        } else {
            ("HOME", "/home/op")
        };
        let env = |name: &str| (name == variable).then(|| base.to_owned());
        let resolved = WebSettings::default().resolved_state_dir(env).unwrap();
        assert!(resolved.ends_with("web"), "{}", resolved.display());
        assert!(resolved.starts_with(base), "{}", resolved.display());
    }
}

pub const MAX_RECORD_SEGMENT_SECONDS: u32 = 86_400;
/// CLI envelope of `--record-bitrate-kbps`/`--record-bitrate-kbps-2`, matching
/// `fm_codec_ffmpeg::record::{MIN,MAX}_VIDEO_BITRATE_KBPS`. The daemon must
/// accept these options in builds without the optional codec dependency, so
/// the bounds are restated here rather than imported.
pub const MIN_RECORD_VIDEO_BITRATE_KBPS: u32 = 1_000;
pub const MAX_RECORD_VIDEO_BITRATE_KBPS: u32 = 100_000;
/// CLI envelope of `--record-audio-bitrate-kbps`/`--record-audio-bitrate-kbps-2`,
/// matching `fm_codec_ffmpeg::record::{MIN,MAX}_AUDIO_BITRATE_KBPS`.
pub const MIN_RECORD_AUDIO_BITRATE_KBPS: u32 = 8;
pub const MAX_RECORD_AUDIO_BITRATE_KBPS: u32 = 1_024;
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Serve {
        project: PathBuf,
        listen: SocketAddr,
        web_listen: Option<SocketAddr>,
        /// The gateway's TLS, state directory, and host names (RC-007).
        web: WebSettings,
        status_listen: Option<SocketAddr>,
        hls_listen: Option<SocketAddr>,
        /// The `LiveLAN` viewer listener (OR-017); any address, LAN included.
        live_lan_listen: Option<SocketAddr>,
        once: bool,
        native_media: bool,
        /// True when display outputs open real borderless windows.
        fullscreen_program: bool,
        /// One or two display outputs, from `--display-output` or the
        /// `--fullscreen-program [--fullscreen-display N]` shorthand. They
        /// replace the project's display-output slots on load.
        display_outputs: Vec<DisplayOutputConfig>,
        /// Diagnostic inventory file presenting display outputs headlessly.
        display_inventory: Option<PathBuf>,
        /// `--display-presenter-if-available`: a presenter that cannot open
        /// (no window system or GPU) falls back to headless native serving
        /// with every slot waiting for a presenter.
        presenter_fallback: bool,
        camera_helper: Option<PathBuf>,
        /// `--diagnostic-capture-inventory`: capture inputs acquire the fake
        /// devices listed in this file instead of real ones (IN-001).
        capture_inventory: Option<PathBuf>,
        record_program: Option<PathBuf>,
        record_output: Option<fm_types::OutputId>,
        record_program_2: Option<PathBuf>,
        record_output_2: Option<fm_types::OutputId>,
        record_wav: bool,
        record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS],
        record_bitrate_kbps: Option<u32>,
        record_bitrate_kbps_2: Option<u32>,
        record_audio_bitrate_kbps: Option<u32>,
        record_audio_bitrate_kbps_2: Option<u32>,
        record_fallback_dir: Option<PathBuf>,
        diagnostic_stop_after: Option<Duration>,
        recover_to_checkpoint: bool,
    },
    Help,
    Version,
}

pub fn resolve_display_outputs(
    native_media: bool,
    fullscreen_program: bool,
    fullscreen_display: Option<usize>,
    display_presenter: bool,
    mut display_outputs: Vec<DisplayOutputConfig>,
    display_inventory: Option<&Path>,
) -> AppResult<Vec<DisplayOutputConfig>> {
    if !display_outputs.is_empty() && !native_media {
        return Err(AppFailure("--display-output requires --native-media".into()).into());
    }
    if display_presenter && !native_media {
        return Err(AppFailure("--display-presenter requires --native-media".into()).into());
    }
    if display_inventory.is_some() && !native_media {
        return Err(
            AppFailure("--diagnostic-display-inventory requires --native-media".into()).into(),
        );
    }
    if !display_outputs.is_empty() && (fullscreen_program || fullscreen_display.is_some()) {
        return Err(AppFailure(
            "--display-output replaces --fullscreen-program and --fullscreen-display; \
             use display=index:<n> instead"
                .into(),
        )
        .into());
    }
    if fullscreen_program {
        display_outputs.push(DisplayOutputConfig::program_on(DisplaySelector::Index(
            fullscreen_display.unwrap_or(0),
        )));
    }
    if display_inventory.is_some() && (fullscreen_program || display_presenter) {
        return Err(AppFailure(
            "--diagnostic-display-inventory presents headlessly and cannot be combined \
             with --fullscreen-program or --display-presenter"
                .into(),
        )
        .into());
    }
    if display_outputs.is_empty() {
        return Ok(display_outputs);
    }
    DisplayOutputSet::new(display_outputs)
        .map(|set| set.outputs().to_vec())
        .map_err(|error| AppFailure(format!("invalid display outputs: {error}")).into())
}

#[allow(clippy::too_many_lines)]
pub fn parse_args(arguments: impl IntoIterator<Item = String>) -> AppResult<Command> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Err(AppFailure("a command is required".into()).into());
    };
    match command.as_str() {
        "serve" => {
            let project = arguments
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| AppFailure("missing project path".into()))?;
            let mut listen = DEFAULT_LISTEN.parse()?;
            let mut listen_set = false;
            let mut web_listen: Option<SocketAddr> = None;
            let mut web = WebSettings::default();
            let mut status_listen: Option<SocketAddr> = None;
            let mut hls_listen: Option<SocketAddr> = None;
            let mut live_lan_listen: Option<SocketAddr> = None;
            let mut once = false;
            let mut native_media = false;
            let mut fullscreen_program = false;
            let mut fullscreen_display = None;
            let mut display_outputs: Vec<DisplayOutputConfig> = Vec::new();
            let mut display_inventory: Option<PathBuf> = None;
            let mut capture_inventory: Option<PathBuf> = None;
            let mut display_presenter = false;
            let mut presenter_if_available = false;
            let mut camera_helper = None;
            let mut record_program = None;
            let mut record_output = None;
            let mut record_program_2 = None;
            let mut record_output_2 = None;
            let mut record_wav = false;
            let mut record_segment_seconds: [Option<NonZeroU32>; RECORDER_SLOTS] =
                [None; RECORDER_SLOTS];
            let mut record_bitrate_kbps = None;
            let mut record_bitrate_kbps_2 = None;
            let mut record_audio_bitrate_kbps = None;
            let mut record_audio_bitrate_kbps_2 = None;
            let mut record_fallback_dir: Option<PathBuf> = None;
            let mut diagnostic_stop_after = None;
            let mut recover_to_checkpoint = false;
            while let Some(option) = arguments.next() {
                if web
                    .take_option(&option, &mut arguments)
                    .map_err(|error| AppFailure(error.to_string()))?
                {
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-program=") {
                    if record_program.is_some() {
                        return Err(AppFailure("duplicate option `--record-program`".into()).into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure("missing value for --record-program".into()).into());
                    }
                    record_program = Some(PathBuf::from(value));
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-output=") {
                    if record_output.is_some() {
                        return Err(AppFailure("duplicate option `--record-output`".into()).into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure("missing value for --record-output".into()).into());
                    }
                    record_output = Some(parse_record_output_id("--record-output", value)?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-fallback-dir=") {
                    if record_fallback_dir.is_some() {
                        return Err(
                            AppFailure("duplicate option `--record-fallback-dir`".into()).into(),
                        );
                    }
                    if value.is_empty() {
                        return Err(
                            AppFailure("missing value for --record-fallback-dir".into()).into()
                        );
                    }
                    record_fallback_dir = Some(PathBuf::from(value));
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-program-2=") {
                    if record_program_2.is_some() {
                        return Err(
                            AppFailure("duplicate option `--record-program-2`".into()).into()
                        );
                    }
                    if value.is_empty() {
                        return Err(
                            AppFailure("missing value for --record-program-2".into()).into()
                        );
                    }
                    record_program_2 = Some(PathBuf::from(value));
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-output-2=") {
                    if record_output_2.is_some() {
                        return Err(
                            AppFailure("duplicate option `--record-output-2`".into()).into()
                        );
                    }
                    if value.is_empty() {
                        return Err(AppFailure("missing value for --record-output-2".into()).into());
                    }
                    record_output_2 = Some(parse_record_output_id("--record-output-2", value)?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-segment-seconds=") {
                    if record_segment_seconds[0].is_some() {
                        return Err(AppFailure(
                            "duplicate option `--record-segment-seconds`".into(),
                        )
                        .into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure(
                            "missing value for --record-segment-seconds".into(),
                        )
                        .into());
                    }
                    record_segment_seconds[0] = Some(parse_record_segment_seconds(
                        "--record-segment-seconds",
                        value,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-segment-seconds-2=") {
                    if record_segment_seconds[1].is_some() {
                        return Err(AppFailure(
                            "duplicate option `--record-segment-seconds-2`".into(),
                        )
                        .into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure(
                            "missing value for --record-segment-seconds-2".into(),
                        )
                        .into());
                    }
                    record_segment_seconds[1] = Some(parse_record_segment_seconds(
                        "--record-segment-seconds-2",
                        value,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-bitrate-kbps=") {
                    if record_bitrate_kbps.is_some() {
                        return Err(
                            AppFailure("duplicate option `--record-bitrate-kbps`".into()).into(),
                        );
                    }
                    if value.is_empty() {
                        return Err(
                            AppFailure("missing value for --record-bitrate-kbps".into()).into()
                        );
                    }
                    record_bitrate_kbps = Some(parse_record_bitrate_kbps(
                        "--record-bitrate-kbps",
                        value,
                        MIN_RECORD_VIDEO_BITRATE_KBPS,
                        MAX_RECORD_VIDEO_BITRATE_KBPS,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-bitrate-kbps-2=") {
                    if record_bitrate_kbps_2.is_some() {
                        return Err(AppFailure(
                            "duplicate option `--record-bitrate-kbps-2`".into(),
                        )
                        .into());
                    }
                    if value.is_empty() {
                        return Err(
                            AppFailure("missing value for --record-bitrate-kbps-2".into()).into(),
                        );
                    }
                    record_bitrate_kbps_2 = Some(parse_record_bitrate_kbps(
                        "--record-bitrate-kbps-2",
                        value,
                        MIN_RECORD_VIDEO_BITRATE_KBPS,
                        MAX_RECORD_VIDEO_BITRATE_KBPS,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-audio-bitrate-kbps=") {
                    if record_audio_bitrate_kbps.is_some() {
                        return Err(AppFailure(
                            "duplicate option `--record-audio-bitrate-kbps`".into(),
                        )
                        .into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure(
                            "missing value for --record-audio-bitrate-kbps".into(),
                        )
                        .into());
                    }
                    record_audio_bitrate_kbps = Some(parse_record_bitrate_kbps(
                        "--record-audio-bitrate-kbps",
                        value,
                        MIN_RECORD_AUDIO_BITRATE_KBPS,
                        MAX_RECORD_AUDIO_BITRATE_KBPS,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--record-audio-bitrate-kbps-2=") {
                    if record_audio_bitrate_kbps_2.is_some() {
                        return Err(AppFailure(
                            "duplicate option `--record-audio-bitrate-kbps-2`".into(),
                        )
                        .into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure(
                            "missing value for --record-audio-bitrate-kbps-2".into(),
                        )
                        .into());
                    }
                    record_audio_bitrate_kbps_2 = Some(parse_record_bitrate_kbps(
                        "--record-audio-bitrate-kbps-2",
                        value,
                        MIN_RECORD_AUDIO_BITRATE_KBPS,
                        MAX_RECORD_AUDIO_BITRATE_KBPS,
                    )?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--camera-helper=") {
                    if camera_helper.is_some() {
                        return Err(AppFailure("duplicate option `--camera-helper`".into()).into());
                    }
                    if value.is_empty() {
                        return Err(AppFailure("missing value for --camera-helper".into()).into());
                    }
                    camera_helper = Some(PathBuf::from(value));
                    continue;
                }
                if let Some(value) = option.strip_prefix("--diagnostic-stop-after=") {
                    if diagnostic_stop_after.is_some() {
                        return Err(AppFailure(
                            "duplicate option `--diagnostic-stop-after`".into(),
                        )
                        .into());
                    }
                    diagnostic_stop_after = Some(parse_diagnostic_duration(value)?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--web-listen=") {
                    if web_listen.is_some() {
                        return Err(AppFailure("duplicate option `--web-listen`".into()).into());
                    }
                    web_listen = Some(value.parse().map_err(|error| {
                        AppFailure(format!("invalid web listen address: {error}"))
                    })?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--status-listen=") {
                    if status_listen.is_some() {
                        return Err(AppFailure("duplicate option `--status-listen`".into()).into());
                    }
                    status_listen = Some(value.parse().map_err(|error| {
                        AppFailure(format!("invalid status listen address: {error}"))
                    })?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--live-lan-listen=") {
                    if live_lan_listen.is_some() {
                        return Err(
                            AppFailure("duplicate option `--live-lan-listen`".into()).into()
                        );
                    }
                    live_lan_listen = Some(value.parse().map_err(|error| {
                        AppFailure(format!("invalid LiveLAN listen address: {error}"))
                    })?);
                    continue;
                }
                if let Some(value) = option.strip_prefix("--hls-listen=") {
                    if hls_listen.is_some() {
                        return Err(AppFailure("duplicate option `--hls-listen`".into()).into());
                    }
                    hls_listen = Some(value.parse().map_err(|error| {
                        AppFailure(format!("invalid hls listen address: {error}"))
                    })?);
                    continue;
                }
                match option.as_str() {
                    "--listen" if listen_set => {
                        return Err(AppFailure("duplicate option `--listen`".into()).into());
                    }
                    "--listen" => {
                        listen_set = true;
                        listen = arguments
                            .next()
                            .ok_or_else(|| AppFailure("missing value for --listen".into()))?
                            .parse()
                            .map_err(|error| {
                                AppFailure(format!("invalid listen address: {error}"))
                            })?;
                    }
                    "--web-listen" => {
                        if web_listen.is_some() {
                            return Err(AppFailure("duplicate option `--web-listen`".into()).into());
                        }
                        let value = arguments
                            .next()
                            .ok_or_else(|| AppFailure("missing value for --web-listen".into()))?;
                        web_listen = Some(value.parse().map_err(|error| {
                            AppFailure(format!("invalid web listen address: {error}"))
                        })?);
                    }
                    "--status-listen" => {
                        if status_listen.is_some() {
                            return Err(
                                AppFailure("duplicate option `--status-listen`".into()).into()
                            );
                        }
                        let value = arguments.next().ok_or_else(|| {
                            AppFailure("missing value for --status-listen".into())
                        })?;
                        status_listen = Some(value.parse().map_err(|error| {
                            AppFailure(format!("invalid status listen address: {error}"))
                        })?);
                    }
                    "--live-lan-listen" => {
                        if live_lan_listen.is_some() {
                            return Err(
                                AppFailure("duplicate option `--live-lan-listen`".into()).into()
                            );
                        }
                        let value = arguments.next().ok_or_else(|| {
                            AppFailure("missing value for --live-lan-listen".into())
                        })?;
                        live_lan_listen = Some(value.parse().map_err(|error| {
                            AppFailure(format!("invalid LiveLAN listen address: {error}"))
                        })?);
                    }
                    "--hls-listen" => {
                        if hls_listen.is_some() {
                            return Err(AppFailure("duplicate option `--hls-listen`".into()).into());
                        }
                        let value = arguments
                            .next()
                            .ok_or_else(|| AppFailure("missing value for --hls-listen".into()))?;
                        hls_listen = Some(value.parse().map_err(|error| {
                            AppFailure(format!("invalid hls listen address: {error}"))
                        })?);
                    }
                    "--once" => once = true,
                    "--native-media" if native_media => {
                        return Err(AppFailure("duplicate option `--native-media`".into()).into());
                    }
                    "--native-media" => native_media = true,
                    "--recover-to-checkpoint" if recover_to_checkpoint => {
                        return Err(AppFailure(
                            "duplicate option `--recover-to-checkpoint`".into(),
                        )
                        .into());
                    }
                    "--recover-to-checkpoint" => recover_to_checkpoint = true,
                    "--fullscreen-program" if fullscreen_program => {
                        return Err(
                            AppFailure("duplicate option `--fullscreen-program`".into()).into()
                        );
                    }
                    "--fullscreen-program" => fullscreen_program = true,
                    "--display-presenter" if display_presenter => {
                        return Err(
                            AppFailure("duplicate option `--display-presenter`".into()).into()
                        );
                    }
                    "--display-presenter" => display_presenter = true,
                    "--display-presenter-if-available" if presenter_if_available => {
                        return Err(AppFailure(
                            "duplicate option `--display-presenter-if-available`".into(),
                        )
                        .into());
                    }
                    "--display-presenter-if-available" => presenter_if_available = true,
                    "--fullscreen-display" if fullscreen_display.is_some() => {
                        return Err(
                            AppFailure("duplicate option `--fullscreen-display`".into()).into()
                        );
                    }
                    "--fullscreen-display" => {
                        let value = arguments.next().ok_or_else(|| {
                            AppFailure("missing value for --fullscreen-display".into())
                        })?;
                        fullscreen_display = Some(value.parse::<usize>().map_err(|_| {
                            AppFailure(format!(
                                "invalid fullscreen display index `{value}`; expected a zero-based non-negative integer"
                            ))
                        })?);
                    }
                    "--camera-helper" if camera_helper.is_some() => {
                        return Err(AppFailure("duplicate option `--camera-helper`".into()).into());
                    }
                    "--camera-helper" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --camera-helper".into())
                            })?;
                        camera_helper = Some(PathBuf::from(value));
                    }
                    "--record-program" if record_program.is_some() => {
                        return Err(AppFailure("duplicate option `--record-program`".into()).into());
                    }
                    "--record-program" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-program".into())
                            })?;
                        record_program = Some(PathBuf::from(value));
                    }
                    "--record-output" if record_output.is_some() => {
                        return Err(AppFailure("duplicate option `--record-output`".into()).into());
                    }
                    "--record-output" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-output".into())
                            })?;
                        record_output = Some(parse_record_output_id("--record-output", &value)?);
                    }
                    "--record-program-2" if record_program_2.is_some() => {
                        return Err(
                            AppFailure("duplicate option `--record-program-2`".into()).into()
                        );
                    }
                    "--record-program-2" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-program-2".into())
                            })?;
                        record_program_2 = Some(PathBuf::from(value));
                    }
                    "--record-output-2" if record_output_2.is_some() => {
                        return Err(
                            AppFailure("duplicate option `--record-output-2`".into()).into()
                        );
                    }
                    "--record-output-2" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-output-2".into())
                            })?;
                        record_output_2 =
                            Some(parse_record_output_id("--record-output-2", &value)?);
                    }
                    "--record-fallback-dir" if record_fallback_dir.is_some() => {
                        return Err(
                            AppFailure("duplicate option `--record-fallback-dir`".into()).into(),
                        );
                    }
                    "--record-fallback-dir" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-fallback-dir".into())
                            })?;
                        record_fallback_dir = Some(PathBuf::from(value));
                    }
                    "--record-wav" if record_wav => {
                        return Err(AppFailure("duplicate option `--record-wav`".into()).into());
                    }
                    "--record-wav" => record_wav = true,
                    "--record-segment-seconds" if record_segment_seconds[0].is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--record-segment-seconds`".into(),
                        )
                        .into());
                    }
                    "--record-segment-seconds" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-segment-seconds".into())
                            })?;
                        record_segment_seconds[0] = Some(parse_record_segment_seconds(
                            "--record-segment-seconds",
                            &value,
                        )?);
                    }
                    "--record-segment-seconds-2" if record_segment_seconds[1].is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--record-segment-seconds-2`".into(),
                        )
                        .into());
                    }
                    "--record-segment-seconds-2" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-segment-seconds-2".into())
                            })?;
                        record_segment_seconds[1] = Some(parse_record_segment_seconds(
                            "--record-segment-seconds-2",
                            &value,
                        )?);
                    }
                    "--record-bitrate-kbps" if record_bitrate_kbps.is_some() => {
                        return Err(
                            AppFailure("duplicate option `--record-bitrate-kbps`".into()).into(),
                        );
                    }
                    "--record-bitrate-kbps" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-bitrate-kbps".into())
                            })?;
                        record_bitrate_kbps = Some(parse_record_bitrate_kbps(
                            "--record-bitrate-kbps",
                            &value,
                            MIN_RECORD_VIDEO_BITRATE_KBPS,
                            MAX_RECORD_VIDEO_BITRATE_KBPS,
                        )?);
                    }
                    "--record-bitrate-kbps-2" if record_bitrate_kbps_2.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--record-bitrate-kbps-2`".into(),
                        )
                        .into());
                    }
                    "--record-bitrate-kbps-2" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-bitrate-kbps-2".into())
                            })?;
                        record_bitrate_kbps_2 = Some(parse_record_bitrate_kbps(
                            "--record-bitrate-kbps-2",
                            &value,
                            MIN_RECORD_VIDEO_BITRATE_KBPS,
                            MAX_RECORD_VIDEO_BITRATE_KBPS,
                        )?);
                    }
                    "--record-audio-bitrate-kbps" if record_audio_bitrate_kbps.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--record-audio-bitrate-kbps`".into(),
                        )
                        .into());
                    }
                    "--record-audio-bitrate-kbps" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-audio-bitrate-kbps".into())
                            })?;
                        record_audio_bitrate_kbps = Some(parse_record_bitrate_kbps(
                            "--record-audio-bitrate-kbps",
                            &value,
                            MIN_RECORD_AUDIO_BITRATE_KBPS,
                            MAX_RECORD_AUDIO_BITRATE_KBPS,
                        )?);
                    }
                    "--record-audio-bitrate-kbps-2" if record_audio_bitrate_kbps_2.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--record-audio-bitrate-kbps-2`".into(),
                        )
                        .into());
                    }
                    "--record-audio-bitrate-kbps-2" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure("missing value for --record-audio-bitrate-kbps-2".into())
                            })?;
                        record_audio_bitrate_kbps_2 = Some(parse_record_bitrate_kbps(
                            "--record-audio-bitrate-kbps-2",
                            &value,
                            MIN_RECORD_AUDIO_BITRATE_KBPS,
                            MAX_RECORD_AUDIO_BITRATE_KBPS,
                        )?);
                    }
                    "--display-output" => {
                        let value = arguments.next().ok_or_else(|| {
                            AppFailure("missing value for --display-output".into())
                        })?;
                        display_outputs.push(value.parse::<DisplayOutputConfig>().map_err(
                            |error| AppFailure(format!("invalid --display-output: {error}")),
                        )?);
                    }
                    "--diagnostic-display-inventory" if display_inventory.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--diagnostic-display-inventory`".into(),
                        )
                        .into());
                    }
                    "--diagnostic-display-inventory" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.is_empty() && !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure(
                                    "missing value for --diagnostic-display-inventory".into(),
                                )
                            })?;
                        display_inventory = Some(PathBuf::from(value));
                    }
                    "--diagnostic-capture-inventory" if capture_inventory.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--diagnostic-capture-inventory`".into(),
                        )
                        .into());
                    }
                    "--diagnostic-capture-inventory" => {
                        let value = arguments
                            .next()
                            .filter(|value| !value.is_empty() && !value.starts_with("--"))
                            .ok_or_else(|| {
                                AppFailure(
                                    "missing value for --diagnostic-capture-inventory".into(),
                                )
                            })?;
                        capture_inventory = Some(PathBuf::from(value));
                    }
                    "--diagnostic-stop-after" if diagnostic_stop_after.is_some() => {
                        return Err(AppFailure(
                            "duplicate option `--diagnostic-stop-after`".into(),
                        )
                        .into());
                    }
                    "--diagnostic-stop-after" => {
                        let value = arguments.next().ok_or_else(|| {
                            AppFailure("missing value for --diagnostic-stop-after".into())
                        })?;
                        diagnostic_stop_after = Some(parse_diagnostic_duration(&value)?);
                    }
                    _ => return Err(AppFailure(format!("unknown option `{option}`")).into()),
                }
            }
            // A build with display surfaces opens the presenter (natively)
            // unless an option the windowed daemon does not serve is given;
            // otherwise, and when the presenter cannot open, the daemon
            // serves exactly as without the option (its slots wait with
            // `no-presenter`).
            let presenter_fallback = presenter_if_available
                && cfg!(feature = "display-surface")
                && !display_presenter
                && display_outputs.is_empty()
                && !fullscreen_program
                && display_inventory.is_none()
                && status_listen.is_none()
                && hls_listen.is_none()
                && live_lan_listen.is_none()
                && record_fallback_dir.is_none()
                && diagnostic_stop_after.is_none()
                && !recover_to_checkpoint;
            if presenter_fallback {
                display_presenter = true;
            }
            if fullscreen_program && !native_media {
                return Err(
                    AppFailure("--fullscreen-program requires --native-media".into()).into(),
                );
            }
            if fullscreen_display.is_some() && !fullscreen_program {
                return Err(AppFailure(
                    "--fullscreen-display requires --fullscreen-program".into(),
                )
                .into());
            }
            let display_outputs = resolve_display_outputs(
                native_media || presenter_fallback,
                fullscreen_program,
                fullscreen_display,
                display_presenter,
                display_outputs,
                display_inventory.as_deref(),
            )?;
            // Real borderless windows open for `--display-output` and
            // `--display-presenter` unless the outputs are presented to the
            // diagnostic inventory host.
            let fullscreen_program =
                (!display_outputs.is_empty() || display_presenter) && display_inventory.is_none();
            if record_program.is_some() && !native_media {
                return Err(AppFailure("--record-program requires --native-media".into()).into());
            }
            if record_program_2.is_some() && !native_media {
                return Err(AppFailure("--record-program-2 requires --native-media".into()).into());
            }
            if record_output.is_some() && record_program.is_none() {
                return Err(AppFailure("--record-output requires --record-program".into()).into());
            }
            if record_output_2.is_some() && record_program_2.is_none() {
                return Err(
                    AppFailure("--record-output-2 requires --record-program-2".into()).into(),
                );
            }
            if record_bitrate_kbps.is_some() && record_program.is_none() {
                return Err(
                    AppFailure("--record-bitrate-kbps requires --record-program".into()).into(),
                );
            }
            if record_bitrate_kbps_2.is_some() && record_program_2.is_none() {
                return Err(AppFailure(
                    "--record-bitrate-kbps-2 requires --record-program-2".into(),
                )
                .into());
            }
            if record_audio_bitrate_kbps.is_some() && record_program.is_none() {
                return Err(AppFailure(
                    "--record-audio-bitrate-kbps requires --record-program".into(),
                )
                .into());
            }
            if record_audio_bitrate_kbps_2.is_some() && record_program_2.is_none() {
                return Err(AppFailure(
                    "--record-audio-bitrate-kbps-2 requires --record-program-2".into(),
                )
                .into());
            }
            if record_wav && record_program.is_none() && record_program_2.is_none() {
                return Err(AppFailure(
                    "--record-wav requires --record-program or --record-program-2".into(),
                )
                .into());
            }
            if record_fallback_dir.is_some()
                && record_program.is_none()
                && record_program_2.is_none()
            {
                return Err(AppFailure(
                    "--record-fallback-dir requires --record-program or --record-program-2".into(),
                )
                .into());
            }
            if record_fallback_dir.is_some() && fullscreen_program {
                return Err(AppFailure(
                    "--record-fallback-dir currently supports headless native mode only".into(),
                )
                .into());
            }
            if record_segment_seconds[0].is_some() && record_program.is_none() {
                return Err(AppFailure(
                    "--record-segment-seconds requires --record-program".into(),
                )
                .into());
            }
            if record_segment_seconds[1].is_some() && record_program_2.is_none() {
                return Err(AppFailure(
                    "--record-segment-seconds-2 requires --record-program-2".into(),
                )
                .into());
            }
            if camera_helper.is_some() && !native_media {
                return Err(AppFailure("--camera-helper requires --native-media".into()).into());
            }
            if recover_to_checkpoint && fullscreen_program {
                return Err(AppFailure(
                    "--recover-to-checkpoint cannot be combined with --fullscreen-program; \
                     abandon the batches with one headless start, then start fullscreen"
                        .into(),
                )
                .into());
            }
            if diagnostic_stop_after.is_some() && once {
                return Err(AppFailure(
                    "--diagnostic-stop-after cannot be combined with --once".into(),
                )
                .into());
            }
            if diagnostic_stop_after.is_some() && fullscreen_program {
                return Err(AppFailure(
                    "--diagnostic-stop-after currently supports headless native mode only".into(),
                )
                .into());
            }
            web.validate(web_listen)
                .map_err(|error| AppFailure(error.to_string()))?;
            // The windowed program daemon serves the gateway too, with the
            // same settings, pairing, and permissions as the headless one.
            if web_listen.is_some() && once {
                return Err(
                    AppFailure("--web-listen cannot be combined with --once".into()).into(),
                );
            }
            if let Some(address) = status_listen {
                if !address.ip().is_loopback() {
                    return Err(
                        AppFailure("--status-listen must use a loopback address".into()).into(),
                    );
                }
                if fullscreen_program {
                    return Err(AppFailure(
                        "--status-listen cannot be combined with --fullscreen-program".into(),
                    )
                    .into());
                }
            }
            if let Some(address) = hls_listen {
                if !address.ip().is_loopback() {
                    return Err(
                        AppFailure("--hls-listen must use a loopback address".into()).into(),
                    );
                }
                if !native_media {
                    return Err(AppFailure("--hls-listen requires --native-media".into()).into());
                }
                if fullscreen_program {
                    return Err(AppFailure(
                        "--hls-listen cannot be combined with --fullscreen-program".into(),
                    )
                    .into());
                }
            }
            if live_lan_listen.is_some() {
                if !native_media {
                    return Err(
                        AppFailure("--live-lan-listen requires --native-media".into()).into(),
                    );
                }
                if fullscreen_program {
                    return Err(AppFailure(
                        "--live-lan-listen cannot be combined with --fullscreen-program".into(),
                    )
                    .into());
                }
            }
            if capture_inventory.is_some() && !native_media {
                return Err(AppFailure(
                    "--diagnostic-capture-inventory requires --native-media".into(),
                )
                .into());
            }
            Ok(Command::Serve {
                project,
                listen,
                web_listen,
                web,
                status_listen,
                hls_listen,
                live_lan_listen,
                once,
                native_media,
                fullscreen_program,
                display_outputs,
                display_inventory,
                presenter_fallback,
                camera_helper,
                capture_inventory,
                record_program,
                record_output,
                record_program_2,
                record_output_2,
                record_wav,
                record_segment_seconds,
                record_bitrate_kbps,
                record_bitrate_kbps_2,
                record_audio_bitrate_kbps,
                record_audio_bitrate_kbps_2,
                record_fallback_dir,
                diagnostic_stop_after,
                recover_to_checkpoint,
            })
        }
        "help" | "--help" | "-h" => {
            reject_extra(arguments)?;
            Ok(Command::Help)
        }
        "--version" | "version" => {
            reject_extra(arguments)?;
            Ok(Command::Version)
        }
        _ => Err(AppFailure(format!("unknown command `{command}`")).into()),
    }
}

/// Parses one recorder segment-duration option as an integer in
/// `1..=MAX_RECORD_SEGMENT_SECONDS`.
///
/// # Errors
///
/// Returns an [`AppFailure`] naming `option` when `value` is not an integer
/// in `1..=86400`.
fn parse_record_segment_seconds(option: &str, value: &str) -> AppResult<NonZeroU32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|&seconds| seconds <= MAX_RECORD_SEGMENT_SECONDS)
        .and_then(NonZeroU32::new)
        .ok_or_else(|| {
            AppFailure(format!(
                "invalid value for {option} `{value}`; expected an integer in 1..=86400"
            ))
            .into()
        })
}

/// Parses one recorder bitrate option as an integer in `min..=max` kbps.
///
/// # Errors
///
/// Returns an [`AppFailure`] when `value` is not an integer in the inclusive
/// range.
fn parse_record_bitrate_kbps(option: &str, value: &str, min: u32, max: u32) -> AppResult<u32> {
    value
        .parse::<u32>()
        .ok()
        .filter(|&kbps| (min..=max).contains(&kbps))
        .ok_or_else(|| {
            AppFailure(format!(
                "invalid value for {option} `{value}`; expected an integer in {min}..={max}"
            ))
            .into()
        })
}

/// Parses a nonzero [`fm_types::OutputId`] for `--record-output` or
/// `--record-output-2`.
///
/// # Errors
///
/// Returns an [`AppFailure`] when `value` is not a positive `u128`.
fn parse_record_output_id(flag: &str, value: &str) -> AppResult<fm_types::OutputId> {
    let parsed = value
        .parse::<u128>()
        .ok()
        .and_then(NonZeroU128::new)
        .ok_or_else(|| {
            AppFailure(format!(
                "invalid value for {flag} `{value}`; expected a positive integer output id"
            ))
        })?;
    Ok(fm_types::OutputId::new(parsed))
}

pub fn parse_diagnostic_duration(value: &str) -> AppResult<Duration> {
    let (amount, unit) = if let Some(amount) = value.strip_suffix("ms") {
        (amount, "ms")
    } else if let Some(amount) = value.strip_suffix('s') {
        (amount, "s")
    } else if let Some(amount) = value.strip_suffix('m') {
        (amount, "m")
    } else if let Some(amount) = value.strip_suffix('h') {
        (amount, "h")
    } else {
        return Err(AppFailure(format!(
            "invalid diagnostic duration `{value}`; expected a positive integer followed by ms, s, m, or h"
        ))
        .into());
    };
    let amount = amount.parse::<u64>().map_err(|_| {
        AppFailure(format!(
            "invalid diagnostic duration `{value}`; expected a positive integer followed by ms, s, m, or h"
        ))
    })?;
    if amount == 0 {
        return Err(AppFailure("diagnostic duration must be nonzero".into()).into());
    }
    let duration = match unit {
        "ms" => Duration::from_millis(amount),
        "s" => Duration::from_secs(amount),
        "m" => Duration::from_secs(
            amount
                .checked_mul(60)
                .ok_or_else(|| AppFailure("diagnostic duration overflow".into()))?,
        ),
        "h" => Duration::from_secs(
            amount
                .checked_mul(3_600)
                .ok_or_else(|| AppFailure("diagnostic duration overflow".into()))?,
        ),
        _ => unreachable!("duration units are matched above"),
    };
    if duration > Duration::from_hours(24) {
        return Err(AppFailure("diagnostic duration cannot exceed 24h".into()).into());
    }
    Ok(duration)
}

fn reject_extra(mut arguments: impl Iterator<Item = String>) -> AppResult<()> {
    if let Some(argument) = arguments.next() {
        Err(AppFailure(format!("unexpected argument `{argument}`")).into())
    } else {
        Ok(())
    }
}
pub fn print_help() {
    println!(
        "FreeMix headless production daemon\n\n\
Usage:\n  freemixd serve <show.freemix> [--listen 127.0.0.1:0] [--web-listen 127.0.0.1:0 [--web-tls | --web-tls-cert PEM --web-tls-key PEM] [--web-state-dir DIR] [--web-host NAME] [--web-mdns-name NAME | --no-web-mdns]] [--status-listen 127.0.0.1:0] [--hls-listen 127.0.0.1:0] [--live-lan-listen 0.0.0.0:8090] [--once] [--native-media [--camera-helper PATH]] [--record-program output.mp4 [--record-output <output-id>]] [--record-program-2 output-2.mp4 [--record-output-2 <output-id>]] [--record-wav] [--record-segment-seconds <1..=86400>] [--record-segment-seconds-2 <1..=86400>] [--record-bitrate-kbps <1000..=100000>] [--record-bitrate-kbps-2 <1000..=100000>] [--record-audio-bitrate-kbps <8..=1024>] [--record-audio-bitrate-kbps-2 <8..=1024>] [--record-fallback-dir DIR] [--diagnostic-stop-after 10m] [--recover-to-checkpoint] [--fullscreen-program [--fullscreen-display 0]] [--display-output <spec> [--display-output <spec>]] [--display-presenter | --display-presenter-if-available] [--diagnostic-display-inventory FILE] [--diagnostic-capture-inventory FILE]\n  freemixd help\n  freemixd --version\n\n\
Native media is opt-in; without it the daemon uses simulated frame realization.\n\
--camera-helper overrides the developer AVFoundation helper path for exact macOS Device inputs; it never requests permission.\n\
Program recording requires native media and an existing output parent; segments are created exclusively, never overwritten. The first segment uses the configured path itself and later record-start segments use <stem>-<NNN>.mp4 names through 999. Both --record-program and --record-program-2 follow the engine's recording desired flag, and each slot derives its own segment names, latches its own failures, and emits its own slot-annotated FREEMIXD_RECORDER report. --record-output and --record-output-2 select which project output each slot records.\n\
--record-wav additionally writes a 16-bit linear-PCM WAV sidecar for each segment of every configured recorder, sharing its stem (`program-002.mp4` is accompanied by `program-002.wav`) and following the program's channel layout. The sidecar is its own crash-safe session (<stem>.wav.fmsession) that fails over with the video: a write or sync failure continues whole sample frames in <stem>-<NNNN>.wav on the same directory or on --record-fallback-dir. Header sizes are fixed when each file closes, and startup recovery repairs the sizes of sidecars a crash left open; FREEMIXD_RECORDER_WAV_SESSION and FREEMIXD_RECORDING_WAV_RECOVERY report them, and a sidecar failure is reported as failure=WavOutput(...).\n\
--record-segment-seconds and --record-segment-seconds-2 rotate the active --record-program and --record-program-2 slot respectively to its next <stem>-<NNN>.mp4 segment on a frame boundary after at least the configured number of seconds; each accepts <option> <value> or <option>=<value> and requires its own recorder, and a slot without its own duration never rotates automatically.\n\
--record-bitrate-kbps and --record-bitrate-kbps-2 set each slot's libx264 video bitrate in kbps (1000..=100000), and --record-audio-bitrate-kbps and --record-audio-bitrate-kbps-2 set its AAC audio bitrate in kbps (8..=1024); each accepts <option> <value> or <option>=<value>, applies only to its own --record-program/--record-program-2 slot, and leaves that slot's encoder defaults untouched when unset.\n\
Every program segment is a crash-safe recording session with a <stem>.fmsession manifest beside it, so output stems must be 1 to 60 ASCII letters, digits, `-`, `_`, or `.`. A write or sync failure re-homes the fragment in flight into <stem>-<NNNN>.mp4 on the same directory, or, when that directory is full, read-only, missing, or forbidden, on --record-fallback-dir (accepts <option> <value> or <option>=<value>, applies to every slot, must differ from each output directory, and is unavailable with --fullscreen-program); the recording continues instead of failing. Startup recovers every session of the configured slots that a crash left open, cutting torn segments back to their last complete fragment, before anything records. FREEMIXD_RECORDER_SPACE reports each segment's free-space preflight (a start is refused only when every measured directory is exhausted), FREEMIXD_RECORDER_FAILOVER each failover, FREEMIXD_RECORDER_SESSION each finished session, and FREEMIXD_RECORDING_RECOVERY what each recovery kept and lost.\n\
Configured network stream endpoints are probed once at startup and reported as FREEMIXD_PREFLIGHT; the probe is bounded and never blocks or refuses startup, and it is a reachability check rather than a bandwidth or health guarantee.\n\
Use --record-program=<path> or --record-program-2=<path> when the output name begins with --. Recorder capability digests describe configured startup support; FREEMIXD_RECORDER reports runtime health.\n\
Fullscreen display selection is a zero-based index ordered by desktop position, then name, then platform display id; --fullscreen-program --fullscreen-display N is shorthand for --display-output display=index:N.\n\
--display-output replaces the project's display-output slots (slot 1, then slot 2) and opens one borderless, cursor-hidden fullscreen output per occurrence (at most two, each on a distinct display); it requires --native-media plus a build with feature `display-surface`. --display-presenter opens the project's own display-output slots the same way. --display-presenter-if-available (what Studio's supervised daemon uses) does so (the presenter always runs native media) in a build with feature `display-surface` unless --status-listen, --hls-listen, --record-fallback-dir, --diagnostic-stop-after, or --recover-to-checkpoint is given; when no window system, window, or GPU is available it emits FREEMIXD_DISPLAY_PRESENTER state=unavailable and serves as without the option, which is also all it does in any other build. Slots are project configuration edited live by set_display_output/clear_display_output commands (Protocol 2.42); a daemon without a presenter keeps them waiting with reason no-presenter. A spec is comma-separated key=value fields: display=index:<n>|name:<name>|id:<platform-id> (required), source=program|preview|output:<output-id> (default program; preview is composited only while an output showing it is active), mode=current|WIDTHxHEIGHT[@HZ] (default current; an unsupported mode falls back to the current one and is reported), scaling=fit|fill|stretch|1:1 (default fit), and hdr=off|auto (default off; outputs present SDR and report why). A missing display leaves its output waiting; it reattaches when the display returns, matched by platform id and then by name. Each state change emits FREEMIXD_DISPLAY_OUTPUT and shutdown emits FREEMIXD_DISPLAY_OUTPUTS.\n\
--diagnostic-display-inventory FILE presents the project's display outputs headlessly to off-screen rasters sized to a tab-separated display inventory re-read from FILE (one display per line: id=, mode=, optional name=, x=, y=, modes=, hdr=; a missing file means no display is connected) and emits sampled FREEMIXD_DISPLAY_FRAME probes. It is a test and diagnostic aid, not an operator output.\n\
--diagnostic-capture-inventory FILE (requires --native-media) replaces the capture backend with a deterministic fake whose devices follow a tab-separated inventory re-read from FILE (one device per line: key=, path=, formats=, optional name=, rgba=, audio=, stalled=, no_signal=; a missing file means no device is connected). Capture inputs acquire solid pictures from it and report capture_status as with real devices. It is a test and diagnostic aid.\n\
--diagnostic-stop-after schedules cooperative simulated or headless native shutdown after readiness; accepted units are ms, s, m, and h up to 24h.\n\
--recover-to-checkpoint opens the project at its last checkpoint and permanently discards every acknowledged command the journal recorded after it, printing each abandoned sequence, revision and command id first. It loses work; use it only when a replay is refused and the show must go on. It is unavailable with --fullscreen-program.\n\
The daemon, simulated or native, serves up to eight raw `--listen` control sessions at once (a further connection's handshake is refused as retryable `resource_exhausted`) next to the Web clients; each session keeps its own role, heartbeat, subscriptions, rate limits, and outbound queue.\n\
--web-listen enables the `/v1/control` WebSocket gateway (up to 16 clients at once, each its own session, plus the /tally and /operator pages and /v1/pair device pairing); it works with or without --native-media, and the windowed program daemon (--fullscreen-program, --display-output, --display-presenter, or Studio's --display-presenter-if-available) serves it too. Plain HTTP is loopback-only: a LAN or 0.0.0.0 address needs --web-tls (a per-install local CA generated once in the web state directory issues the gateway's certificate for its LAN addresses, localhost, the --web-host names, and the mDNS name, and reissues it when the addresses change; phones install the CA once from /freemix-ca.crt; FREEMIXD_WEB_TLS prints the certificate's and the CA's SHA-256 fingerprints) or --web-tls-cert PEM --web-tls-key PEM (your own chain and key, for example from mkcert, validated before use and reloaded when the files change; the key must not be readable by other users). --web-state-dir DIR holds the CA, the certificate, and the paired devices (default: $XDG_STATE_HOME/freemix/web or ~/.local/state/freemix/web; ~/Library/Application Support/FreeMix/web on macOS; %LOCALAPPDATA%\\FreeMix\\web on Windows). --web-host NAME (repeatable) adds a DNS name phones may use; the first one names the pairing URL, and requests for any other name than an IP address, localhost, a --web-host, or the advertised mDNS name are refused (DNS-rebinding defence). On a LAN address the gateway advertises itself over mDNS / DNS-SD (_freemix._tcp and _https._tcp) as freemix-<computer name>.local, or --web-mdns-name NAME.local; --no-web-mdns advertises nothing. Devices pair from Studio's Web access window; FREEMIXD_WEB_TOKEN, when set, is a 32..=256-byte operator token with every role.\n\
--status-listen enables the loopback-only operator status listener serving /healthz, /readyz, and the token-guarded /v1/support-bundle; it requires FREEMIXD_STATUS_TOKEN and is unavailable with --fullscreen-program.\n\
--hls-listen enables the loopback-only HLS listener for local hls:// stream destinations. It serves GET /v1/hls/<target>/index.m3u8 and GET /v1/hls/<target>/segment-NNNNN.ts, plus the same shapes under rung directories 0..=7 for a multi-bitrate ladder, and requires both --native-media and FREEMIXD_HLS_TOKEN; TLS, LAN binding, Web Controller roles, and per-viewer capacity remain future work.\n\
Native mode continues across client disconnects; close the Program window or press Escape for bounded shutdown."
    );
}
