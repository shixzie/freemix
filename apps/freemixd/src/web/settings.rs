//! The gateway's command-line settings: TLS, the state directory, and the
//! extra host names the gateway answers to.

use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use super::AppResult;

/// The most `--web-host` names.
const MAX_WEB_HOSTS: usize = 8;

/// How the gateway serves its pages and WebSocket.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum WebTls {
    /// Plain HTTP and `ws://`: loopback addresses only.
    #[default]
    Off,
    /// A self-signed certificate generated once and kept in the state
    /// directory (`--web-tls`).
    SelfSigned,
    /// The operator's own certificate chain and key (`--web-tls-cert`,
    /// `--web-tls-key`).
    Files { cert: PathBuf, key: PathBuf },
}

/// `--web-tls`, `--web-tls-cert`, `--web-tls-key`, `--web-state-dir`, and
/// `--web-host`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct WebSettings {
    pub(crate) tls: WebTls,
    pub(crate) state_dir: Option<PathBuf>,
    /// Extra DNS names (or addresses) phones may use to reach the gateway.
    /// The first one, when given, names the pairing URL.
    pub(crate) hosts: Vec<String>,
    self_signed: bool,
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
    pub(crate) fn take_option(
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
                if self.self_signed {
                    return Err("duplicate option `--web-tls`".into());
                }
                self.self_signed = true;
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
        self.tls = match (self.self_signed, &self.cert, &self.key) {
            (false, None, None) => WebTls::Off,
            (true, None, None) => WebTls::SelfSigned,
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
    pub(crate) fn validate(&self, web_listen: Option<SocketAddr>) -> AppResult<()> {
        if self.self_signed && (self.cert.is_some() || self.key.is_some()) {
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
        Ok(())
    }

    /// The directory holding the generated certificate and the paired
    /// devices: `--web-state-dir`, else the platform's per-user state
    /// directory.
    pub(crate) fn resolved_state_dir(
        &self,
        env: impl Fn(&str) -> Option<String>,
    ) -> Option<PathBuf> {
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

/// A DNS name (`[a-z0-9-]` labels) or an IP address literal.
pub(crate) fn valid_host_name(host: &str) -> bool {
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
        assert_eq!(tls.tls, WebTls::SelfSigned);
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
        let home = |name: &str| (name == "HOME").then(|| "/home/op".to_owned());
        let resolved = WebSettings::default().resolved_state_dir(home).unwrap();
        assert!(resolved.ends_with("web"), "{}", resolved.display());
        assert!(resolved.starts_with("/home/op"), "{}", resolved.display());
    }
}
