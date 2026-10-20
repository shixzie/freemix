//! Web access management (RC-007, RC-014): pairing phones and tablets with
//! the daemon's web gateway, and listing or revoking paired devices.
//!
//! A console sends `web_access_request` (`request_id`, `action`) and
//! receives `web_access` with the same `request_id`. Only a session whose
//! role holds the `manage_users` permission, over the raw control
//! connection (never through the web gateway itself), may manage web
//! access.
//!
//! * `action` is `status`, `pair` (with `role`: `viewer`, `audio`, or
//!   `operator`), `cancel_pairing`, or `revoke` (with `device`).
//! * The answer carries an optional `refusal` and `gateway` (`0|1`). With a
//!   gateway it also carries `url` (the page origin a phone opens), `tls`
//!   (`0|1`), `fingerprint` (only with TLS: the SHA-256 of the certificate
//!   as 32 colon-separated uppercase hex pairs), an optional `pairing`
//!   (`<code>~<link code>~<role>~<expires_in_ms>`: the six-digit code to
//!   type and the long one-use code a QR link carries), and `devices`: a
//!   comma-separated list, strictly ascending by id, of
//!   `<id>~<role>~<paired_at_ms>~<last_seen_ms|->~<name length>:<name>`.
//!
//! Device tokens never travel on this connection: a device receives its
//! token from the gateway when it redeems a code, and the daemon keeps only
//! the token's digest.

/// The most paired devices one gateway lists.
pub const MAX_WEB_DEVICES: usize = 32;
/// The longest device name, in bytes.
pub const MAX_WEB_DEVICE_NAME_BYTES: usize = 48;
/// The longest device id, in bytes.
pub const MAX_WEB_DEVICE_ID_BYTES: usize = 32;
/// The longest gateway URL, in bytes.
pub const MAX_WEB_URL_BYTES: usize = 256;
/// Digits in the code an operator types on the phone.
pub const WEB_PAIRING_CODE_DIGITS: usize = 6;
/// The shortest and longest link code a QR carries.
pub const MIN_WEB_LINK_CODE_BYTES: usize = 16;
pub const MAX_WEB_LINK_CODE_BYTES: usize = 64;
/// How long a pairing code stays valid.
pub const WEB_PAIRING_LIFETIME_MS: u32 = 120_000;

/// The role a paired web device is granted.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum WebDeviceRole {
    Viewer,
    Audio,
    Operator,
}

impl WebDeviceRole {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Audio => "audio",
            Self::Operator => "operator",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "viewer" => Some(Self::Viewer),
            "audio" => Some(Self::Audio),
            "operator" => Some(Self::Operator),
            _ => None,
        }
    }

    /// The page a device of this role opens: the tally light for a viewer,
    /// the operator surface otherwise.
    #[must_use]
    pub const fn page_path(self) -> &'static str {
        match self {
            Self::Viewer => "/tally",
            Self::Audio | Self::Operator => "/operator",
        }
    }
}

/// What a `web_access_request` asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WebAccessAction {
    /// The gateway, the outstanding pairing, and the paired devices.
    Status,
    /// Starts pairing one device with `role`, replacing any outstanding
    /// pairing.
    Pair { role: WebDeviceRole },
    /// Withdraws the outstanding pairing.
    CancelPairing,
    /// Unpairs a device and ends its sessions.
    Revoke { device: String },
}

/// A console's web access request (Protocol 2.70).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebAccessRequest {
    pub request_id: String,
    pub action: WebAccessAction,
}

/// Why a web access request was refused.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WebAccessRefusal {
    /// The session's role lacks `manage_users`.
    PermissionDenied,
    /// Web access cannot be managed through the web gateway itself.
    WebSession,
    /// This daemon serves no web gateway.
    Unavailable,
    /// `revoke` named a device that is not paired.
    UnknownDevice,
    /// Every device slot is taken; revoke one first.
    TooManyDevices,
    /// The daemon could not store the change.
    StorageFailed,
}

impl WebAccessRefusal {
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::WebSession => "web_session",
            Self::Unavailable => "unavailable",
            Self::UnknownDevice => "unknown_device",
            Self::TooManyDevices => "too_many_devices",
            Self::StorageFailed => "storage_failed",
        }
    }

    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        [
            Self::PermissionDenied,
            Self::WebSession,
            Self::Unavailable,
            Self::UnknownDevice,
            Self::TooManyDevices,
            Self::StorageFailed,
        ]
        .into_iter()
        .find(|refusal| refusal.token() == token)
    }
}

/// The outstanding pairing, shown only to the console that manages it.
#[derive(Clone, Eq, PartialEq)]
pub struct WebPairing {
    /// Six ASCII digits to type on the phone.
    pub code: String,
    /// The long one-use code a QR link carries (base64url).
    pub link_code: String,
    pub role: WebDeviceRole,
    /// Milliseconds until both codes expire.
    pub expires_in_ms: u32,
}

impl core::fmt::Debug for WebPairing {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("WebPairing")
            .field("codes", &"[redacted]")
            .field("role", &self.role)
            .field("expires_in_ms", &self.expires_in_ms)
            .finish()
    }
}

/// One paired device.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebDeviceEntry {
    pub id: String,
    pub name: String,
    pub role: WebDeviceRole,
    /// Unix milliseconds.
    pub paired_at_ms: u64,
    /// Unix milliseconds of its latest connection, if any.
    pub last_seen_ms: Option<u64>,
}

/// The web gateway as a managing console sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebGatewayStatus {
    /// The origin a phone opens, such as `https://192.168.1.20:8443`.
    pub url: String,
    pub tls: bool,
    /// SHA-256 of the certificate, `AB:CD:...`; present exactly with TLS.
    pub fingerprint: Option<String>,
    pub pairing: Option<WebPairing>,
    /// Strictly ascending by id.
    pub devices: Vec<WebDeviceEntry>,
}

impl WebGatewayStatus {
    /// The link a QR code carries for the outstanding pairing: the page for
    /// its role with the link code and the certificate fingerprint in the
    /// fragment, which browsers never send to the server.
    #[must_use]
    pub fn pairing_link(&self) -> Option<String> {
        let pairing = self.pairing.as_ref()?;
        let mut link = format!(
            "{}{}#pair={}",
            self.url,
            pairing.role.page_path(),
            pairing.link_code
        );
        if let Some(fingerprint) = &self.fingerprint {
            link.push_str("&fp=");
            link.extend(fingerprint.chars().filter(|character| *character != ':'));
        }
        Some(link)
    }
}

/// Answers a [`WebAccessRequest`] with the same `request_id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebAccessMessage {
    pub request_id: String,
    pub refusal: Option<WebAccessRefusal>,
    /// Absent without a gateway or without permission.
    pub gateway: Option<WebGatewayStatus>,
}

/// Whether `id` is a valid device id: `[a-z][a-z0-9-]{0,31}`.
#[must_use]
pub fn valid_web_device_id(id: &str) -> bool {
    id.len() <= MAX_WEB_DEVICE_ID_BYTES
        && id.starts_with(|first: char| first.is_ascii_lowercase())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Whether `name` is a valid device name.
#[must_use]
pub fn valid_web_device_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_WEB_DEVICE_NAME_BYTES
        && name.trim() == name
        && !name.chars().any(char::is_control)
}

/// Whether `fingerprint` is 32 colon-separated uppercase hex pairs.
#[must_use]
pub fn valid_web_fingerprint(fingerprint: &str) -> bool {
    fingerprint.len() == 95
        && fingerprint.split(':').count() == 32
        && fingerprint.split(':').all(|pair| {
            pair.len() == 2
                && pair
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
        })
}
