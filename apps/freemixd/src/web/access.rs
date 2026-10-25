//! Who may use the web gateway: the optional `FREEMIXD_WEB_TOKEN` and the
//! paired devices, the outstanding pairing, and the console-facing
//! `web_access` answers (Protocol 2.71).
//!
//! A device token is 32 random bytes (`fmd1_` + base64url), handed to the
//! device once when it redeems a pairing code; the daemon keeps only its
//! SHA-256 in memory and in `web-devices.txt` (mode 0600) in the state
//! directory. Neither tokens nor pairing codes are ever logged.

use std::{
    fmt::Write as _,
    fs,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use fm_auth::{
    AttemptLimiter, DeviceError, DeviceRegistry, DeviceRole, PairedDevice, PairingCode,
    PairingError, RedeemError, TokenDigest, sanitize_device_name,
};
use fm_protocol::{
    WEB_PAIRING_LIFETIME_MS, WebAccessAction, WebAccessRefusal, WebDeviceEntry, WebDeviceRole,
    WebGatewayStatus, WebPairing, WebTrust,
};

use super::{AppResult, base64url, tls, tls::TlsState};

/// The paired devices in the state directory.
pub(super) const DEVICES_FILE: &str = "web-devices.txt";
const DEVICES_HEADER: &str = "freemix-web-devices v1";
const DEVICE_TOKEN_PREFIX: &str = "fmd1_";
/// Pairing attempts: per client address and in total, per minute.
const ATTEMPT_WINDOW_MS: u64 = 60_000;
const ATTEMPTS_PER_ADDRESS: u32 = 5;
const ATTEMPTS_TOTAL: u32 = 20;
const TRACKED_ADDRESSES: usize = 64;

/// Who an authenticated gateway client is.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WebIdentity {
    /// The operator's `FREEMIXD_WEB_TOKEN`: every role, as before pairing.
    StaticToken,
    /// A paired device, limited to its role.
    Device { id: String, role: DeviceRole },
}

/// A device that just paired: the token is shown to it once.
pub(super) struct NewDevice {
    pub(super) token: String,
    pub(super) id: String,
    pub(super) name: String,
    pub(super) role: DeviceRole,
}

/// Why a pairing attempt was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PairRefusal {
    BadRequest,
    Invalid,
    Expired,
    RateLimited { retry_after_s: u64 },
    Unavailable,
}

/// How stale a device's stored last-seen time may grow before an
/// authentication writes the device file again.
const LAST_SEEN_WRITE_MS: u64 = 60_000;

struct State {
    registry: DeviceRegistry,
    limiter: AttemptLimiter<IpAddr>,
    /// When each device's last-seen time was last written, by device id
    /// (empty after a restart, so a device's first authentication writes).
    seen_written: std::collections::BTreeMap<String, u64>,
}

/// Where phones reach the gateway; follows the LAN addresses.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Reach {
    /// The origin a phone opens, such as `https://192.168.1.20:8443`.
    pub(crate) url: String,
    /// Other origins that reach it (the mDNS name, other addresses).
    pub(crate) urls: Vec<String>,
    /// The advertised `<label>.local`, if any.
    pub(crate) mdns: Option<String>,
}

/// Shared by every gateway connection thread and the session scheduler.
pub(crate) struct WebAccess {
    reach: Mutex<Reach>,
    tls: Option<Arc<TlsState>>,
    static_token: Option<Vec<u8>>,
    store: Option<PathBuf>,
    state: Mutex<State>,
}

impl std::fmt::Debug for WebAccess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WebAccess")
            .field("reach", &self.reach())
            .field("tls", &self.tls)
            .finish_non_exhaustive()
    }
}

impl WebAccess {
    /// Loads the paired devices from `state_dir` (none without one).
    ///
    /// # Errors
    ///
    /// An unreadable or malformed device file.
    pub(crate) fn open(
        url: String,
        tls: Option<Arc<TlsState>>,
        static_token: Option<Vec<u8>>,
        state_dir: Option<&Path>,
    ) -> AppResult<Self> {
        let store = state_dir.map(|directory| directory.join(DEVICES_FILE));
        let devices = match &store {
            Some(path) if path.exists() => read_devices(path)?,
            _ => Vec::new(),
        };
        Ok(Self {
            reach: Mutex::new(Reach {
                url,
                ..Reach::default()
            }),
            tls,
            static_token,
            store,
            state: Mutex::new(State {
                registry: DeviceRegistry::with_devices(devices)
                    .map_err(|error| format!("invalid web device store: {error}"))?,
                limiter: AttemptLimiter::new(
                    ATTEMPT_WINDOW_MS,
                    ATTEMPTS_PER_ADDRESS,
                    ATTEMPTS_TOTAL,
                    TRACKED_ADDRESSES,
                ),
                seen_written: std::collections::BTreeMap::new(),
            }),
        })
    }

    /// Where phones reach the gateway now.
    pub(crate) fn reach(&self) -> Reach {
        self.reach
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn set_reach(&self, reach: Reach) {
        *self.reach.lock().unwrap_or_else(PoisonError::into_inner) = reach;
    }

    pub(crate) fn url(&self) -> String {
        self.reach().url
    }

    /// The served certificate, with TLS.
    pub(crate) fn tls(&self) -> Option<&Arc<TlsState>> {
        self.tls.as_ref()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The identity a presented token proves, if any. The static token is
    /// compared in constant time; device tokens by their digest. A device's
    /// last-seen time is updated in memory and written to the device file
    /// at most once per [`LAST_SEEN_WRITE_MS`].
    pub(crate) fn authenticate(&self, token: &[u8]) -> Option<WebIdentity> {
        if let Some(expected) = &self.static_token
            && constant_time_eq(expected, token)
        {
            return Some(WebIdentity::StaticToken);
        }
        let digest = digest(token);
        let now = now_ms();
        let mut state = self.lock();
        let device = state.registry.authenticate(&digest, now)?;
        let identity = WebIdentity::Device {
            id: device.id.clone(),
            role: device.role,
        };
        let id = device.id.clone();
        // Last-seen times are best effort: the file is rewritten only when
        // the stored time is a minute stale, not on every request (each
        // `/v1/whoami` authenticates), and a failed write only loses them.
        let due = state
            .seen_written
            .get(&id)
            .is_none_or(|written| now.saturating_sub(*written) >= LAST_SEEN_WRITE_MS);
        if due && self.persist(&state.registry).is_ok() {
            let State {
                registry,
                seen_written,
                ..
            } = &mut *state;
            seen_written.retain(|known, _| registry.contains(known));
            seen_written.insert(id, now);
        }
        Some(identity)
    }

    /// Whether `identity` may still be served (its device is still paired).
    pub(crate) fn is_current(&self, identity: &WebIdentity) -> bool {
        match identity {
            WebIdentity::StaticToken => true,
            WebIdentity::Device { id, .. } => self.lock().registry.contains(id),
        }
    }

    /// Redeems a pairing code from `peer`, which reached the gateway at
    /// `local`, for a new device token.
    pub(super) fn redeem(
        &self,
        code: &str,
        name: &str,
        peer: IpAddr,
        local: Option<IpAddr>,
    ) -> Result<NewDevice, PairRefusal> {
        let now = now_ms();
        let mut state = self.lock();
        state
            .limiter
            // A remote IPv6 host's /64 shares one allowance.
            .attempt(&super::worker::peer_group_at(peer, local), now)
            .map_err(|retry_ms| PairRefusal::RateLimited {
                retry_after_s: retry_ms.div_ceil(1_000).max(1),
            })?;
        let candidate = PairingCode::new(code).map_err(|_| PairRefusal::BadRequest)?;
        let name = sanitize_device_name(name).unwrap_or_else(|| "Web browser".to_owned());
        let mut token = None;
        let result = state.registry.redeem(&candidate, &name, now, || {
            let (secret, digest) = new_token();
            token = Some(secret);
            (new_device_id(), digest)
        });
        let device = match result {
            Ok(device) => device.clone(),
            Err(RedeemError::Pairing(PairingError::Invalid)) => return Err(PairRefusal::Invalid),
            Err(RedeemError::Pairing(_)) => return Err(PairRefusal::Expired),
            Err(RedeemError::Device(_)) => return Err(PairRefusal::Unavailable),
        };
        if self.persist(&state.registry).is_err() {
            // A device that would vanish on restart is not paired.
            let _ = state.registry.revoke(&device.id);
            return Err(PairRefusal::Unavailable);
        }
        eprintln!(
            "FREEMIXD_WEB_PAIRED\tv=1\tdevice={}\trole={}",
            device.id,
            device.role.token()
        );
        Ok(NewDevice {
            token: token.expect("a redeemed pairing produced a token"),
            id: device.id,
            name: device.name,
            role: device.role,
        })
    }

    /// The paired device's name and role for `/v1/whoami`.
    pub(super) fn describe(&self, identity: &WebIdentity) -> Option<(String, DeviceRole)> {
        let WebIdentity::Device { id, .. } = identity else {
            return None;
        };
        self.lock()
            .registry
            .devices()
            .iter()
            .find(|device| &device.id == id)
            .map(|device| (device.name.clone(), device.role))
    }

    /// Carries out one console request (already authorized).
    pub(crate) fn handle(
        &self,
        action: &WebAccessAction,
    ) -> (Option<WebAccessRefusal>, WebGatewayStatus) {
        let now = now_ms();
        let mut state = self.lock();
        let refusal = match action {
            WebAccessAction::Status => None,
            WebAccessAction::Pair { role } => {
                let (short, link) = new_codes();
                match state.registry.start_pairing(
                    short,
                    link,
                    device_role(*role),
                    now,
                    u64::from(WEB_PAIRING_LIFETIME_MS),
                ) {
                    Ok(_) => None,
                    Err(DeviceError::TooManyDevices) => Some(WebAccessRefusal::TooManyDevices),
                    Err(_) => Some(WebAccessRefusal::StorageFailed),
                }
            }
            WebAccessAction::CancelPairing => {
                state.registry.cancel_pairing();
                None
            }
            WebAccessAction::Revoke { device } => match state.registry.revoke(device) {
                Ok(revoked) => {
                    eprintln!("FREEMIXD_WEB_REVOKED\tv=1\tdevice={}", revoked.id);
                    // Revoked now either way; a failed write is reported so
                    // the operator knows it would return after a restart.
                    self.persist(&state.registry)
                        .err()
                        .map(|_| WebAccessRefusal::StorageFailed)
                }
                Err(_) => Some(WebAccessRefusal::UnknownDevice),
            },
        };
        (refusal, self.status(&mut state, now))
    }

    fn status(&self, state: &mut State, now: u64) -> WebGatewayStatus {
        let pairing = state.registry.pairing(now).map(|pairing| WebPairing {
            code: pairing.short_code().to_owned(),
            link_code: pairing.link_code().to_owned(),
            role: protocol_role(pairing.role()),
            expires_in_ms: u32::try_from(pairing.expires_at_ms().saturating_sub(now))
                .unwrap_or(WEB_PAIRING_LIFETIME_MS)
                .min(WEB_PAIRING_LIFETIME_MS),
        });
        let mut devices: Vec<WebDeviceEntry> = state
            .registry
            .devices()
            .iter()
            .map(|device| WebDeviceEntry {
                id: device.id.clone(),
                name: device.name.clone(),
                role: protocol_role(device.role),
                paired_at_ms: device.paired_at_ms,
                last_seen_ms: device.last_seen_ms,
            })
            .collect();
        devices.sort_by(|left, right| left.id.cmp(&right.id));
        let Reach { url, urls, mdns } = self.reach();
        let tls = self.tls.as_deref();
        let host = url_host(&url);
        WebGatewayStatus {
            tls: tls.is_some(),
            fingerprint: tls.map(TlsState::fingerprint),
            trust: tls.map_or(WebTrust::Plain, TlsState::trust),
            ca_fingerprint: tls
                .and_then(TlsState::ca)
                .map(|ca| ca.fingerprint().to_owned()),
            cert_not_after_ms: tls.map(TlsState::not_after_ms),
            cert_problem: tls.and_then(|tls| tls.problem(host, now)),
            url,
            urls,
            mdns,
            pairing,
            devices,
        }
    }

    fn persist(&self, registry: &DeviceRegistry) -> AppResult<()> {
        let Some(path) = &self.store else {
            return Ok(());
        };
        if let Some(directory) = path.parent() {
            tls::create_private_dir(directory)?;
        }
        tls::write_private(path, encode_devices(registry.devices()).as_bytes())
    }
}

/// The host of an origin such as `https://[fd00::1]:8443`.
fn url_host(url: &str) -> &str {
    let authority = url.split_once("://").map_or(url, |(_, rest)| rest);
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split_once(']').map_or(rest, |(host, _)| host);
    }
    authority
        .rsplit_once(':')
        .map_or(authority, |(host, _)| host)
}

const fn device_role(role: WebDeviceRole) -> DeviceRole {
    match role {
        WebDeviceRole::Viewer => DeviceRole::Viewer,
        WebDeviceRole::Audio => DeviceRole::Audio,
        WebDeviceRole::Operator => DeviceRole::Operator,
        WebDeviceRole::Graphics => DeviceRole::Graphics,
    }
}

const fn protocol_role(role: DeviceRole) -> WebDeviceRole {
    match role {
        DeviceRole::Viewer => WebDeviceRole::Viewer,
        DeviceRole::Audio => WebDeviceRole::Audio,
        DeviceRole::Operator => WebDeviceRole::Operator,
        DeviceRole::Graphics => WebDeviceRole::Graphics,
    }
}

fn constant_time_eq(expected: &[u8], presented: &[u8]) -> bool {
    let mut difference = expected.len() ^ presented.len();
    for (index, byte) in expected.iter().enumerate() {
        let other = presented.get(index).copied().unwrap_or(0);
        difference |= usize::from(std::hint::black_box(byte ^ other));
    }
    difference == 0
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).expect("the operating system random source is available");
    bytes
}

fn digest(token: &[u8]) -> TokenDigest {
    let digest = ring::digest::digest(&ring::digest::SHA256, token);
    let mut bytes = [0; 32];
    bytes.copy_from_slice(digest.as_ref());
    TokenDigest::new(bytes)
}

/// A new device token and its digest.
fn new_token() -> (String, TokenDigest) {
    let token = format!("{DEVICE_TOKEN_PREFIX}{}", base64url(&random::<32>()));
    let digest = digest(token.as_bytes());
    (token, digest)
}

fn new_device_id() -> String {
    let mut id = String::from("dev-");
    for byte in random::<6>() {
        let _ = write!(id, "{byte:02x}");
    }
    id
}

/// A uniform six-digit code and a 144-bit link code.
fn new_codes() -> (PairingCode, PairingCode) {
    // Rejection sampling keeps every code equally likely.
    let short = loop {
        let value = u32::from_le_bytes(random::<4>());
        if value < 4_294_000_000 {
            break format!("{:06}", value % 1_000_000);
        }
    };
    let link = base64url(&random::<18>());
    (
        PairingCode::new(short).expect("six digits are not empty"),
        PairingCode::new(link).expect("the link code is not empty"),
    )
}

fn encode_devices(devices: &[PairedDevice]) -> String {
    let mut text = format!("{DEVICES_HEADER}\n");
    for device in devices {
        let mut hex = String::with_capacity(64);
        for byte in device.digest.as_bytes() {
            let _ = write!(hex, "{byte:02x}");
        }
        let _ = writeln!(
            text,
            "{}\t{}\t{}\t{}\t{hex}\t{}",
            device.id,
            device.role.token(),
            device.paired_at_ms,
            device
                .last_seen_ms
                .map_or_else(|| "-".to_owned(), |at| at.to_string()),
            device.name,
        );
    }
    text
}

fn read_devices(path: &Path) -> AppResult<Vec<PairedDevice>> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    decode_devices(&text)
        .ok_or_else(|| format!("{} is not a valid web device store", path.display()).into())
}

fn decode_devices(text: &str) -> Option<Vec<PairedDevice>> {
    let mut lines = text.lines();
    if lines.next()? != DEVICES_HEADER {
        return None;
    }
    lines
        .map(|line| {
            let mut fields = line.split('\t');
            let id = fields.next()?.to_owned();
            let role = DeviceRole::from_token(fields.next()?)?;
            let paired_at_ms = fields.next()?.parse().ok()?;
            let last_seen_ms = match fields.next()? {
                "-" => None,
                value => Some(value.parse().ok()?),
            };
            let hex = fields.next()?;
            let name = fields.next()?.to_owned();
            if fields.next().is_some() || hex.len() != 64 {
                return None;
            }
            let mut bytes = [0; 32];
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
            }
            let device = PairedDevice {
                id,
                name,
                role,
                paired_at_ms,
                last_seen_ms,
                digest: TokenDigest::new(bytes),
            };
            device.validate().ok()?;
            Some(device)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use fm_protocol::{WebAccessAction, WebAccessRefusal, WebDeviceRole};

    use super::{DEVICES_FILE, PairRefusal, WebAccess, WebIdentity, decode_devices};

    const PHONE: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));

    fn access(directory: &std::path::Path) -> WebAccess {
        WebAccess::open(
            "https://192.168.1.20:8443".into(),
            None,
            Some(b"static-token-0123456789abcdef0123456789".to_vec()),
            Some(directory),
        )
        .unwrap()
    }

    fn pair(access: &WebAccess, role: WebDeviceRole) -> (String, String) {
        let (_, status) = access.handle(&WebAccessAction::Pair { role });
        let pairing = status.pairing.unwrap();
        (pairing.code, pairing.link_code)
    }

    #[test]
    fn pairing_issues_a_token_stored_only_as_a_digest() {
        let directory = tempfile::tempdir().unwrap();
        let access = access(directory.path());
        let (code, _) = pair(&access, WebDeviceRole::Operator);
        let device = access.redeem(&code, " Floor\nphone ", PHONE, None).unwrap();
        assert!(device.token.starts_with("fmd1_"));
        assert_eq!(device.name, "Floor phone");
        assert_eq!(
            access.authenticate(device.token.as_bytes()),
            Some(WebIdentity::Device {
                id: device.id.clone(),
                role: fm_auth::DeviceRole::Operator
            })
        );
        assert_eq!(
            access.authenticate(b"static-token-0123456789abcdef0123456789"),
            Some(WebIdentity::StaticToken)
        );
        assert_eq!(access.authenticate(b"fmd1_wrong"), None);
        // The store holds the digest, never the token or the code.
        let stored = std::fs::read_to_string(directory.path().join(DEVICES_FILE)).unwrap();
        assert!(!stored.contains(&device.token));
        assert!(!stored.contains(&code));
        assert_eq!(decode_devices(&stored).unwrap().len(), 1);
        // A restarted daemon still knows the device.
        let restarted = super::WebAccess::open(
            "https://192.168.1.20:8443".into(),
            None,
            None,
            Some(directory.path()),
        )
        .unwrap();
        assert!(restarted.authenticate(device.token.as_bytes()).is_some());
        assert_eq!(
            restarted.authenticate(b"static-token-0123456789abcdef0123456789"),
            None
        );
    }

    #[test]
    fn authentication_does_not_rewrite_the_device_file_every_time() {
        let directory = tempfile::tempdir().unwrap();
        let access = access(directory.path());
        let (code, _) = pair(&access, WebDeviceRole::Viewer);
        let device = access.redeem(&code, "Tally", PHONE, None).unwrap();
        let file = directory.path().join(DEVICES_FILE);
        // The first authentication records the device as seen.
        assert!(access.authenticate(device.token.as_bytes()).is_some());
        let stored = decode_devices(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert!(stored[0].last_seen_ms.is_some());
        // Within the minute, authentications leave the file alone.
        std::fs::remove_file(&file).unwrap();
        for _ in 0..3 {
            assert!(access.authenticate(device.token.as_bytes()).is_some());
        }
        assert!(!file.exists());
        // The console still sees the latest time.
        let (_, status) = access.handle(&WebAccessAction::Status);
        assert!(status.devices[0].last_seen_ms.is_some());
    }

    #[test]
    fn revoke_ends_the_identity_and_survives_restart() {
        let directory = tempfile::tempdir().unwrap();
        let access = access(directory.path());
        let (_, link) = pair(&access, WebDeviceRole::Viewer);
        let device = access.redeem(&link, "Tally", PHONE, None).unwrap();
        let identity = access.authenticate(device.token.as_bytes()).unwrap();
        assert!(access.is_current(&identity));
        let (refusal, status) = access.handle(&WebAccessAction::Revoke {
            device: device.id.clone(),
        });
        assert_eq!(refusal, None);
        assert_eq!(status.devices, []);
        assert!(!access.is_current(&identity));
        assert_eq!(access.authenticate(device.token.as_bytes()), None);
        let (refusal, _) = access.handle(&WebAccessAction::Revoke { device: device.id });
        assert_eq!(refusal, Some(WebAccessRefusal::UnknownDevice));
        let restarted =
            WebAccess::open("https://h:1".into(), None, None, Some(directory.path())).unwrap();
        assert_eq!(restarted.authenticate(device.token.as_bytes()), None);
    }

    #[test]
    fn wrong_codes_exhaust_the_pairing_and_addresses_are_rate_limited() {
        let directory = tempfile::tempdir().unwrap();
        let access = access(directory.path());
        let (code, _) = pair(&access, WebDeviceRole::Audio);
        let wrong = if code == "000000" { "000001" } else { "000000" };
        for _ in 0..4 {
            assert_eq!(
                access.redeem(wrong, "x", PHONE, None).err(),
                Some(PairRefusal::Invalid)
            );
        }
        // The fifth wrong code kills the pairing: the right code is late.
        assert_eq!(
            access.redeem(wrong, "x", PHONE, None).err(),
            Some(PairRefusal::Expired)
        );
        // And the address has spent its attempts for this minute.
        assert!(matches!(
            access.redeem(&code, "x", PHONE, None).err(),
            Some(PairRefusal::RateLimited { retry_after_s }) if retry_after_s <= 60
        ));
        let other = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 51));
        assert_eq!(
            access.redeem(&code, "x", other, None).err(),
            Some(PairRefusal::Expired)
        );
    }

    /// Pairing attempts from one IPv6 `/64` share one allowance, however
    /// many addresses the host rotates through.
    #[test]
    fn ipv6_pairing_attempts_share_their_slash_64() {
        use std::net::Ipv6Addr;
        let directory = tempfile::tempdir().unwrap();
        let access = access(directory.path());
        let (code, _) = pair(&access, WebDeviceRole::Audio);
        let wrong = if code == "000000" { "000001" } else { "000000" };
        let host = |last: u16| IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 7, 0, 0, 0, last));
        for last in 1..=5 {
            assert!(!matches!(
                access.redeem(wrong, "x", host(last), None).err(),
                Some(PairRefusal::RateLimited { .. })
            ));
        }
        assert!(matches!(
            access.redeem(wrong, "x", host(6), None).err(),
            Some(PairRefusal::RateLimited { .. })
        ));
    }

    /// Phones on the gateway's own LAN share its `/64`: each pairs on its
    /// own allowance, as on IPv4.
    #[test]
    fn on_link_ipv6_phones_pair_on_their_own_allowance() {
        use std::net::Ipv6Addr;
        let directory = tempfile::tempdir().unwrap();
        let access = access(directory.path());
        let host = |last: u16| IpAddr::V6(Ipv6Addr::new(0x2001, 0xdb8, 0, 7, 0, 0, 0, last));
        let local = Some(host(0x20));
        for last in 1..=8 {
            let (code, _) = pair(&access, WebDeviceRole::Viewer);
            assert!(
                access.redeem(&code, "Phone", host(last), local).is_ok(),
                "phone {last}"
            );
        }
    }

    #[test]
    fn cancelled_pairings_and_status_hide_nothing_they_should_show() {
        let directory = tempfile::tempdir().unwrap();
        let access = access(directory.path());
        let (code, _) = pair(&access, WebDeviceRole::Viewer);
        let (_, status) = access.handle(&WebAccessAction::CancelPairing);
        assert!(status.pairing.is_none());
        assert_eq!(
            access.redeem(&code, "x", PHONE, None).err(),
            Some(PairRefusal::Expired)
        );
        assert_eq!(status.url, "https://192.168.1.20:8443");
        assert!(!status.tls);
    }
}
