//! Deterministic URL normalization, host allow/deny rules, and bounded
//! redirect-chain validation layered over [`NavigationPolicy`].
//!
//! Nothing here performs DNS or network I/O. Adapters call
//! [`NetworkPolicy::check`] before navigation, [`RedirectChain::follow`] for
//! every redirect, and [`NetworkPolicy::check_resolved`] after every DNS
//! resolution and before opening a socket.

use core::fmt;
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
};

use crate::{
    NavigationBlocked, NavigationPolicy, NetworkLimits,
    contract::{parse_whatwg_ipv4, split_scheme},
    css::CustomCss,
};

const MAX_HOST_BYTES: usize = 253;
const MAX_LABEL_BYTES: usize = 63;

/// Why a URL, host rule, or redirect was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkPolicyError {
    UrlTooLong {
        actual: usize,
        maximum: usize,
    },
    MalformedUrl {
        reason: &'static str,
    },
    CredentialsInUrl,
    InvalidHostPattern {
        pattern: String,
        reason: &'static str,
    },
    HostRuleLimit {
        maximum: usize,
    },
    Navigation(NavigationBlocked),
    HostDenied {
        host: String,
        pattern: String,
    },
    HostNotAllowed {
        host: String,
    },
    TooManyRedirects {
        maximum: usize,
    },
    RedirectLoop {
        url: String,
    },
    InsecureRedirect {
        from: String,
        to: String,
    },
    FileRedirect {
        to: String,
    },
}

impl fmt::Display for NetworkPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UrlTooLong { actual, maximum } => {
                write!(formatter, "URL length {actual} exceeds {maximum} bytes")
            }
            Self::MalformedUrl { reason } => write!(formatter, "malformed URL: {reason}"),
            Self::CredentialsInUrl => formatter.write_str("URL credentials are not permitted"),
            Self::InvalidHostPattern { pattern, reason } => {
                write!(formatter, "invalid host pattern {pattern:?}: {reason}")
            }
            Self::HostRuleLimit { maximum } => {
                write!(formatter, "host rule count exceeds {maximum}")
            }
            Self::Navigation(error) => error.fmt(formatter),
            Self::HostDenied { host, pattern } => {
                write!(formatter, "host {host} is denied by rule {pattern}")
            }
            Self::HostNotAllowed { host } => {
                write!(formatter, "host {host} matches no allow rule")
            }
            Self::TooManyRedirects { maximum } => {
                write!(formatter, "redirect count exceeds {maximum}")
            }
            Self::RedirectLoop { url } => write!(formatter, "redirect loop revisits {url}"),
            Self::InsecureRedirect { from, to } => {
                write!(formatter, "redirect from {from} to {to} downgrades HTTPS")
            }
            Self::FileRedirect { to } => {
                write!(formatter, "redirect to local file {to} is never followed")
            }
        }
    }
}

impl std::error::Error for NetworkPolicyError {}

impl From<NavigationBlocked> for NetworkPolicyError {
    fn from(value: NavigationBlocked) -> Self {
        Self::Navigation(value)
    }
}

const fn malformed(reason: &'static str) -> NetworkPolicyError {
    NetworkPolicyError::MalformedUrl { reason }
}

/// A canonical URL host.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum UrlHost {
    /// Lowercase ASCII domain name without a trailing dot.
    Domain(String),
    Ipv4(Ipv4Addr),
    Ipv6(Ipv6Addr),
}

impl fmt::Display for UrlHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain(domain) => formatter.write_str(domain),
            Self::Ipv4(address) => address.fmt(formatter),
            Self::Ipv6(address) => write!(formatter, "[{address}]"),
        }
    }
}

impl UrlHost {
    /// Parses and canonicalizes a host: lowercases domains, strips trailing
    /// dots, and rewrites legacy numeric IPv4 forms to dotted quads.
    ///
    /// # Errors
    ///
    /// Returns [`NetworkPolicyError::MalformedUrl`] for empty, non-ASCII,
    /// over-long, or syntactically invalid hosts.
    pub fn parse(host: &str) -> Result<Self, NetworkPolicyError> {
        if let Some(inner) = host.strip_prefix('[') {
            let inner = inner
                .strip_suffix(']')
                .ok_or(malformed("unterminated IPv6 literal"))?;
            return inner
                .parse::<Ipv6Addr>()
                .map(Self::Ipv6)
                .map_err(|_| malformed("invalid IPv6 literal"));
        }
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return Err(malformed("empty host"));
        }
        if host.len() > MAX_HOST_BYTES {
            return Err(malformed("host exceeds 253 bytes"));
        }
        if !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_'))
        {
            return Err(malformed("invalid host character"));
        }
        let last = host.rsplit('.').next().unwrap_or_default();
        let numeric_last = !last.is_empty()
            && (last.bytes().all(|byte| byte.is_ascii_digit())
                || ((last.starts_with("0x"))
                    && last[2..].bytes().all(|byte| byte.is_ascii_hexdigit())));
        if numeric_last {
            return parse_whatwg_ipv4(&host)
                .map(Self::Ipv4)
                .ok_or(malformed("invalid IPv4 address"));
        }
        for label in host.split('.') {
            if label.is_empty() {
                return Err(malformed("empty host label"));
            }
            if label.len() > MAX_LABEL_BYTES {
                return Err(malformed("host label exceeds 63 bytes"));
            }
            if label.starts_with('-') || label.ends_with('-') {
                return Err(malformed("host label starts or ends with a hyphen"));
            }
        }
        Ok(Self::Domain(host))
    }

    #[must_use]
    pub fn ip(&self) -> Option<IpAddr> {
        match self {
            Self::Domain(_) => None,
            Self::Ipv4(address) => Some(IpAddr::V4(*address)),
            Self::Ipv6(address) => Some(IpAddr::V6(*address)),
        }
    }
}

/// A canonical, policy-ready URL. Fragments are preserved for the renderer but
/// ignored by [`NormalizedUrl::network_key`] and redirect-loop detection.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NormalizedUrl {
    serialized: String,
    network_end: usize,
    scheme: String,
    host: Option<UrlHost>,
    port: Option<u16>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

impl fmt::Display for NormalizedUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.serialized)
    }
}

impl NormalizedUrl {
    /// Parses and normalizes an absolute URL.
    ///
    /// Normalization lowercases the scheme and host, canonicalizes IP
    /// literals, elides default ports, defaults an empty hierarchical path to
    /// `/`, and removes `.`/`..` (including `%2e`) path segments. Credentials,
    /// backslashes, whitespace, control characters, and non-ASCII input are
    /// rejected rather than repaired; callers must percent-encode and use
    /// punycode for internationalized hosts.
    ///
    /// # Errors
    ///
    /// Returns a length, malformed-URL, or credentials error.
    pub fn parse(input: &str, max_bytes: usize) -> Result<Self, NetworkPolicyError> {
        if input.len() > max_bytes {
            return Err(NetworkPolicyError::UrlTooLong {
                actual: input.len(),
                maximum: max_bytes,
            });
        }
        if input.is_empty() {
            return Err(malformed("empty URL"));
        }
        for byte in input.bytes() {
            if !byte.is_ascii() {
                return Err(malformed("non-ASCII character"));
            }
            if byte.is_ascii_control() || byte == b' ' {
                return Err(malformed("whitespace or control character"));
            }
            if byte == b'\\' {
                return Err(malformed("backslash"));
            }
        }
        let (scheme, rest) = split_scheme(input).ok_or(malformed("missing or invalid scheme"))?;
        let scheme = scheme.to_ascii_lowercase();
        let (rest, fragment) = match rest.split_once('#') {
            Some((rest, fragment)) => (rest, Some(fragment.to_owned())),
            None => (rest, None),
        };
        let (hier, query) = match rest.split_once('?') {
            Some((hier, query)) => (hier, Some(query.to_owned())),
            None => (rest, None),
        };
        let special = matches!(scheme.as_str(), "http" | "https" | "file");

        let (host, port, path) = if let Some(after) = hier.strip_prefix("//") {
            let (authority, path) = after
                .find('/')
                .map_or((after, ""), |index| (&after[..index], &after[index..]));
            if authority.contains('@') {
                return Err(NetworkPolicyError::CredentialsInUrl);
            }
            let (host, port) = if authority.is_empty() {
                if scheme != "file" {
                    return Err(malformed("empty host"));
                }
                (None, None)
            } else {
                let (host, port) = split_host_port(authority)?;
                if scheme == "file" && port.is_some() {
                    return Err(malformed("file URL with a port"));
                }
                let port = port.filter(|port| Some(*port) != default_port(&scheme));
                (Some(UrlHost::parse(host)?), port)
            };
            let path = if path.is_empty() {
                "/".to_owned()
            } else {
                remove_dot_segments(path)
            };
            (host, port, path)
        } else {
            if special {
                return Err(malformed("scheme requires an authority"));
            }
            if hier.is_empty() {
                return Err(malformed("empty opaque URL"));
            }
            (None, None, hier.to_owned())
        };

        let (serialized, network_end) = serialize(
            &scheme,
            host.as_ref(),
            port,
            &path,
            query.as_deref(),
            fragment.as_deref(),
            host.is_some() || special,
        );
        if serialized.len() > max_bytes {
            return Err(NetworkPolicyError::UrlTooLong {
                actual: serialized.len(),
                maximum: max_bytes,
            });
        }
        Ok(Self {
            serialized,
            network_end,
            scheme,
            host,
            port,
            path,
            query,
            fragment,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.serialized
    }

    /// The URL without its fragment, which is what reaches the network.
    #[must_use]
    pub fn network_key(&self) -> &str {
        &self.serialized[..self.network_end]
    }

    #[must_use]
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    #[must_use]
    pub const fn host(&self) -> Option<&UrlHost> {
        self.host.as_ref()
    }

    /// The explicit non-default port, if any.
    #[must_use]
    pub const fn port(&self) -> Option<u16> {
        self.port
    }

    /// The effective port including the scheme default.
    #[must_use]
    pub fn effective_port(&self) -> Option<u16> {
        self.port.or_else(|| default_port(&self.scheme))
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }

    #[must_use]
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// Resolves an absolute, scheme-relative, path-absolute, path-relative,
    /// query-only, or fragment-only reference against this URL.
    ///
    /// # Errors
    ///
    /// Returns a malformed-URL error for empty references or references
    /// relative to an opaque URL, plus any [`NormalizedUrl::parse`] error.
    pub fn join(&self, reference: &str, max_bytes: usize) -> Result<Self, NetworkPolicyError> {
        if reference.is_empty() {
            return Err(malformed("empty URL reference"));
        }
        if reference.contains('\\') {
            return Err(malformed("backslash"));
        }
        if split_scheme(reference).is_some() {
            return Self::parse(reference, max_bytes);
        }
        let Some(host) = &self.host else {
            if self.scheme == "file" && reference.starts_with('/') && !reference.starts_with("//") {
                return Self::parse(&format!("file://{reference}"), max_bytes);
            }
            return Err(malformed("relative reference without a hierarchical base"));
        };
        let mut origin = format!("{}://{host}", self.scheme);
        if let Some(port) = self.port {
            origin.push(':');
            origin.push_str(&port.to_string());
        }
        let candidate = if reference.starts_with("//") {
            format!("{}:{reference}", self.scheme)
        } else if reference.starts_with('/') {
            format!("{origin}{reference}")
        } else if reference.starts_with('?') {
            format!("{origin}{}{reference}", self.path)
        } else if reference.starts_with('#') {
            format!("{}{reference}", self.network_key())
        } else {
            let directory = self
                .path
                .rfind('/')
                .map_or("/", |index| &self.path[..=index]);
            format!("{origin}{directory}{reference}")
        };
        if candidate.len() > max_bytes {
            return Err(NetworkPolicyError::UrlTooLong {
                actual: candidate.len(),
                maximum: max_bytes,
            });
        }
        Self::parse(&candidate, max_bytes)
    }
}

fn serialize(
    scheme: &str,
    host: Option<&UrlHost>,
    port: Option<u16>,
    path: &str,
    query: Option<&str>,
    fragment: Option<&str>,
    authority: bool,
) -> (String, usize) {
    let mut serialized = String::with_capacity(scheme.len() + path.len() + 16);
    serialized.push_str(scheme);
    serialized.push(':');
    if authority {
        serialized.push_str("//");
    }
    if let Some(host) = host {
        serialized.push_str(&host.to_string());
    }
    if let Some(port) = port {
        serialized.push(':');
        serialized.push_str(&port.to_string());
    }
    serialized.push_str(path);
    if let Some(query) = query {
        serialized.push('?');
        serialized.push_str(query);
    }
    let network_end = serialized.len();
    if let Some(fragment) = fragment {
        serialized.push('#');
        serialized.push_str(fragment);
    }
    (serialized, network_end)
}

fn split_host_port(authority: &str) -> Result<(&str, Option<u16>), NetworkPolicyError> {
    let (host, port) = if authority.starts_with('[') {
        let end = authority
            .find(']')
            .ok_or(malformed("unterminated IPv6 literal"))?;
        let (host, rest) = authority.split_at(end + 1);
        if rest.is_empty() {
            (host, None)
        } else {
            let port = rest
                .strip_prefix(':')
                .ok_or(malformed("unexpected text after IPv6 literal"))?;
            (host, Some(port))
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    let port = match port {
        None | Some("") => None,
        Some(port) => {
            if !port.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(malformed("invalid port"));
            }
            Some(
                port.parse::<u16>()
                    .map_err(|_| malformed("port exceeds 65535"))?,
            )
        }
    };
    Ok((host, port))
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

fn remove_dot_segments(path: &str) -> String {
    let segments: Vec<&str> = path[1..].split('/').collect();
    let mut output: Vec<&str> = Vec::with_capacity(segments.len());
    let last_index = segments.len() - 1;
    for (index, segment) in segments.iter().enumerate() {
        let lower = segment.to_ascii_lowercase();
        let is_dot = matches!(lower.as_str(), "." | "%2e");
        let is_dot_dot = matches!(lower.as_str(), ".." | ".%2e" | "%2e." | "%2e%2e");
        if is_dot || is_dot_dot {
            if is_dot_dot {
                output.pop();
            }
            if index == last_index {
                output.push("");
            }
        } else {
            output.push(segment);
        }
    }
    format!("/{}", output.join("/"))
}

/// A host rule. `example.com` matches only that host (or IP literal);
/// `*.example.com` matches strict subdomains but not the apex.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HostPattern {
    Exact(UrlHost),
    Subdomains(String),
}

impl fmt::Display for HostPattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(host) => host.fmt(formatter),
            Self::Subdomains(suffix) => write!(formatter, "*.{suffix}"),
        }
    }
}

impl HostPattern {
    /// Parses an exact host or a leading `*.` wildcard over a registrable-looking
    /// domain with at least two labels.
    ///
    /// # Errors
    ///
    /// Returns [`NetworkPolicyError::InvalidHostPattern`].
    pub fn parse(pattern: &str) -> Result<Self, NetworkPolicyError> {
        let invalid = |reason| NetworkPolicyError::InvalidHostPattern {
            pattern: pattern.to_owned(),
            reason,
        };
        if let Some(suffix) = pattern.strip_prefix("*.") {
            if suffix.contains('*') {
                return Err(invalid("wildcard is only allowed as the leftmost label"));
            }
            match UrlHost::parse(suffix) {
                Ok(UrlHost::Domain(domain)) => {
                    if !domain.contains('.') {
                        return Err(invalid("wildcard suffix must have at least two labels"));
                    }
                    Ok(Self::Subdomains(domain))
                }
                Ok(_) => Err(invalid("wildcards cannot apply to IP literals")),
                Err(_) => Err(invalid("wildcard suffix is not a valid domain")),
            }
        } else if pattern.contains('*') {
            Err(invalid("wildcard is only allowed as a leading `*.` label"))
        } else {
            UrlHost::parse(pattern)
                .map(Self::Exact)
                .map_err(|_| invalid("not a valid host"))
        }
    }

    #[must_use]
    pub fn matches(&self, host: &UrlHost) -> bool {
        match (self, host) {
            (Self::Exact(expected), host) => expected == host,
            (Self::Subdomains(suffix), UrlHost::Domain(domain)) => domain
                .strip_suffix(suffix.as_str())
                .is_some_and(|prefix| prefix.len() > 1 && prefix.ends_with('.')),
            (Self::Subdomains(_), _) => false,
        }
    }
}

/// Bounded host allow/deny rules. Deny rules always win; an empty allow list
/// permits every host not denied (other policy checks still apply).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostRules {
    allow: BTreeSet<HostPattern>,
    deny: BTreeSet<HostPattern>,
    max_rules: usize,
}

impl Default for HostRules {
    fn default() -> Self {
        Self::new(Self::DEFAULT_MAX_RULES)
    }
}

impl HostRules {
    pub const DEFAULT_MAX_RULES: usize = 256;

    #[must_use]
    pub const fn new(max_rules: usize) -> Self {
        Self {
            allow: BTreeSet::new(),
            deny: BTreeSet::new(),
            max_rules,
        }
    }

    /// Adds an allow rule. Returns `false` when it was already present.
    ///
    /// # Errors
    ///
    /// Returns a pattern or rule-count error without changing the rules.
    pub fn allow(&mut self, pattern: &str) -> Result<bool, NetworkPolicyError> {
        let pattern = HostPattern::parse(pattern)?;
        if self.allow.contains(&pattern) {
            return Ok(false);
        }
        self.ensure_capacity()?;
        Ok(self.allow.insert(pattern))
    }

    /// Adds a deny rule. Returns `false` when it was already present.
    ///
    /// # Errors
    ///
    /// Returns a pattern or rule-count error without changing the rules.
    pub fn deny(&mut self, pattern: &str) -> Result<bool, NetworkPolicyError> {
        let pattern = HostPattern::parse(pattern)?;
        if self.deny.contains(&pattern) {
            return Ok(false);
        }
        self.ensure_capacity()?;
        Ok(self.deny.insert(pattern))
    }

    pub fn remove(&mut self, pattern: &HostPattern) -> bool {
        let allowed = self.allow.remove(pattern);
        let denied = self.deny.remove(pattern);
        allowed || denied
    }

    #[must_use]
    pub const fn allowed(&self) -> &BTreeSet<HostPattern> {
        &self.allow
    }

    #[must_use]
    pub const fn denied(&self) -> &BTreeSet<HostPattern> {
        &self.deny
    }

    /// Evaluates one canonical host.
    ///
    /// # Errors
    ///
    /// Returns [`NetworkPolicyError::HostDenied`] or
    /// [`NetworkPolicyError::HostNotAllowed`].
    pub fn evaluate(&self, host: &UrlHost) -> Result<(), NetworkPolicyError> {
        if let Some(pattern) = self.deny.iter().find(|pattern| pattern.matches(host)) {
            return Err(NetworkPolicyError::HostDenied {
                host: host.to_string(),
                pattern: pattern.to_string(),
            });
        }
        if !self.allow.is_empty() && !self.allow.iter().any(|pattern| pattern.matches(host)) {
            return Err(NetworkPolicyError::HostNotAllowed {
                host: host.to_string(),
            });
        }
        Ok(())
    }

    fn ensure_capacity(&self) -> Result<(), NetworkPolicyError> {
        if self.allow.len().saturating_add(self.deny.len()) >= self.max_rules {
            Err(NetworkPolicyError::HostRuleLimit {
                maximum: self.max_rules,
            })
        } else {
            Ok(())
        }
    }
}

/// Navigation policy plus host rules, URL bounds, and redirect rules.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NetworkPolicy {
    navigation: NavigationPolicy,
    hosts: HostRules,
    max_url_bytes: usize,
    allow_insecure_redirects: bool,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self::new(NavigationPolicy::new(), HostRules::default())
    }
}

impl NetworkPolicy {
    pub const DEFAULT_MAX_URL_BYTES: usize = 8 * 1024;

    #[must_use]
    pub const fn new(navigation: NavigationPolicy, hosts: HostRules) -> Self {
        Self {
            navigation,
            hosts,
            max_url_bytes: Self::DEFAULT_MAX_URL_BYTES,
            allow_insecure_redirects: false,
        }
    }

    #[must_use]
    pub const fn with_max_url_bytes(mut self, max_url_bytes: usize) -> Self {
        self.max_url_bytes = max_url_bytes;
        self
    }

    /// Permits HTTPS-to-HTTP redirects, which are denied by default.
    pub const fn set_allow_insecure_redirects(&mut self, allow: bool) {
        self.allow_insecure_redirects = allow;
    }

    #[must_use]
    pub const fn max_url_bytes(&self) -> usize {
        self.max_url_bytes
    }

    #[must_use]
    pub const fn navigation(&self) -> &NavigationPolicy {
        &self.navigation
    }

    pub const fn navigation_mut(&mut self) -> &mut NavigationPolicy {
        &mut self.navigation
    }

    #[must_use]
    pub const fn hosts(&self) -> &HostRules {
        &self.hosts
    }

    pub const fn hosts_mut(&mut self) -> &mut HostRules {
        &mut self.hosts
    }

    /// Normalizes a URL and applies scheme, file, private-network, and host rules.
    ///
    /// # Errors
    ///
    /// Returns the first rule which denies the URL.
    pub fn check(&self, url: &str) -> Result<NormalizedUrl, NetworkPolicyError> {
        let normalized = NormalizedUrl::parse(url, self.max_url_bytes)?;
        self.check_normalized(&normalized)?;
        Ok(normalized)
    }

    /// Rechecks a URL against every address DNS returned for its host.
    ///
    /// # Errors
    ///
    /// Returns a URL-policy, host-rule, or private-network denial.
    pub fn check_resolved(
        &self,
        url: &str,
        resolved_addresses: &[IpAddr],
    ) -> Result<NormalizedUrl, NetworkPolicyError> {
        let normalized = self.check(url)?;
        self.navigation
            .check_resolved(normalized.as_str(), resolved_addresses)?;
        Ok(normalized)
    }

    /// Starts a redirect chain at a policy-approved URL.
    ///
    /// # Errors
    ///
    /// Returns the first rule which denies the initial URL.
    pub fn begin_navigation(
        &self,
        url: &str,
        limits: NetworkLimits,
    ) -> Result<RedirectChain, NetworkPolicyError> {
        let initial = self.check(url)?;
        Ok(RedirectChain {
            initial,
            redirects: Vec::new(),
            max_redirects: limits.max_redirects,
        })
    }

    /// Checks every external `url(...)` reference in validated custom CSS.
    ///
    /// # Errors
    ///
    /// Returns the first reference denied by this policy.
    pub fn check_css(&self, css: &CustomCss) -> Result<Vec<NormalizedUrl>, NetworkPolicyError> {
        css.external_urls()
            .iter()
            .map(|url| self.check(url))
            .collect()
    }

    fn check_normalized(&self, url: &NormalizedUrl) -> Result<(), NetworkPolicyError> {
        self.navigation.check(url.as_str())?;
        if let Some(host) = url.host() {
            self.hosts.evaluate(host)?;
        }
        Ok(())
    }
}

/// A bounded, append-only record of the hops one navigation has followed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedirectChain {
    initial: NormalizedUrl,
    redirects: Vec<NormalizedUrl>,
    max_redirects: usize,
}

impl RedirectChain {
    #[must_use]
    pub fn current(&self) -> &NormalizedUrl {
        self.redirects.last().unwrap_or(&self.initial)
    }

    #[must_use]
    pub const fn initial(&self) -> &NormalizedUrl {
        &self.initial
    }

    /// Every hop in order, starting with the initial URL.
    pub fn hops(&self) -> impl Iterator<Item = &NormalizedUrl> {
        core::iter::once(&self.initial).chain(&self.redirects)
    }

    #[must_use]
    pub const fn redirects(&self) -> usize {
        self.redirects.len()
    }

    /// Validates and records one redirect `Location`, resolved against the
    /// current hop. The chain is unchanged when the redirect is rejected.
    ///
    /// # Errors
    ///
    /// Returns a redirect-count, policy, loop, file-target, or HTTPS-downgrade error.
    pub fn follow(
        &mut self,
        policy: &NetworkPolicy,
        location: &str,
    ) -> Result<&NormalizedUrl, NetworkPolicyError> {
        if self.redirects() >= self.max_redirects {
            return Err(NetworkPolicyError::TooManyRedirects {
                maximum: self.max_redirects,
            });
        }
        let current = self.current();
        let next = current.join(location, policy.max_url_bytes)?;
        if next.scheme() == "file" {
            return Err(NetworkPolicyError::FileRedirect {
                to: next.as_str().to_owned(),
            });
        }
        if current.scheme() == "https"
            && next.scheme() == "http"
            && !policy.allow_insecure_redirects
        {
            return Err(NetworkPolicyError::InsecureRedirect {
                from: current.as_str().to_owned(),
                to: next.as_str().to_owned(),
            });
        }
        policy.check_normalized(&next)?;
        if self
            .hops()
            .any(|hop| hop.network_key() == next.network_key())
        {
            return Err(NetworkPolicyError::RedirectLoop {
                url: next.network_key().to_owned(),
            });
        }
        self.redirects.push(next);
        Ok(self.current())
    }
}
