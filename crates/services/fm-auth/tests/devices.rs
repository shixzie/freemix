//! Paired web devices: pairing success, expiry, the shared failed-attempt
//! budget, revoke, digest-only storage, and the attempt limiter.

use fm_auth::{
    AttemptLimiter, DeviceError, DeviceRegistry, DeviceRole, MAX_PAIRED_DEVICES,
    MAX_PAIRING_FAILED_ATTEMPTS, PairedDevice, PairingCode, PairingError, RedeemError, Role,
    TokenDigest, sanitize_device_name,
};

const LIFETIME_MS: u64 = 120_000;

fn code(value: &str) -> PairingCode {
    PairingCode::new(value).unwrap()
}

fn digest(byte: u8) -> TokenDigest {
    TokenDigest::new([byte; 32])
}

fn start(registry: &mut DeviceRegistry, role: DeviceRole, now: u64) {
    registry
        .start_pairing(
            code("123456"),
            code("link-code-0123456789abcdef"),
            role,
            now,
            LIFETIME_MS,
        )
        .unwrap();
}

#[test]
fn either_code_pairs_one_device_once() {
    for candidate in ["123456", "link-code-0123456789abcdef"] {
        let mut registry = DeviceRegistry::default();
        start(&mut registry, DeviceRole::Operator, 1_000);
        let paired = registry
            .redeem(&code(candidate), "Floor phone", 2_000, || {
                ("dev-1".into(), digest(7))
            })
            .unwrap();
        assert_eq!(paired.role, DeviceRole::Operator);
        assert_eq!(paired.name, "Floor phone");
        assert_eq!(paired.paired_at_ms, 2_000);
        // One use only.
        assert_eq!(
            registry
                .redeem(&code(candidate), "Again", 2_001, || unreachable!())
                .unwrap_err(),
            RedeemError::Pairing(PairingError::Expired)
        );
        assert!(registry.pairing(2_001).is_none());
        assert_eq!(registry.devices().len(), 1);
    }
}

#[test]
fn pairing_expires() {
    let mut registry = DeviceRegistry::default();
    start(&mut registry, DeviceRole::Viewer, 0);
    assert!(registry.pairing(LIFETIME_MS - 1).is_some());
    assert_eq!(
        registry
            .redeem(&code("123456"), "Late", LIFETIME_MS, || unreachable!())
            .unwrap_err(),
        RedeemError::Pairing(PairingError::Expired)
    );
    assert!(registry.devices().is_empty());
    assert_eq!(
        registry
            .start_pairing(code("1"), code("2"), DeviceRole::Viewer, 0, 600_001)
            .unwrap_err(),
        DeviceError::LifetimeTooLong
    );
}

#[test]
fn wrong_codes_share_one_budget_and_exhaust_the_pairing() {
    let mut registry = DeviceRegistry::default();
    start(&mut registry, DeviceRole::Audio, 0);
    for attempt in 1..MAX_PAIRING_FAILED_ATTEMPTS {
        assert_eq!(
            registry
                .redeem(
                    &code(&format!("00000{attempt}")),
                    "x",
                    10,
                    || unreachable!()
                )
                .unwrap_err(),
            RedeemError::Pairing(PairingError::Invalid)
        );
        assert_eq!(registry.pairing(10).unwrap().failed_attempts(), attempt);
    }
    assert_eq!(
        registry
            .redeem(&code("999999"), "x", 10, || unreachable!())
            .unwrap_err(),
        RedeemError::Pairing(PairingError::AttemptsExhausted)
    );
    // Even the right code no longer pairs.
    assert_eq!(
        registry
            .redeem(&code("123456"), "x", 11, || unreachable!())
            .unwrap_err(),
        RedeemError::Pairing(PairingError::Expired)
    );
    assert!(registry.devices().is_empty());
}

#[test]
fn tokens_authenticate_by_digest_until_revoked() {
    let mut registry = DeviceRegistry::default();
    start(&mut registry, DeviceRole::Operator, 0);
    registry
        .redeem(&code("123456"), "Phone", 5, || ("dev-a".into(), digest(1)))
        .unwrap();
    start(&mut registry, DeviceRole::Viewer, 6);
    registry
        .redeem(&code("123456"), "Tally", 7, || ("dev-b".into(), digest(2)))
        .unwrap();

    let seen = registry.authenticate(&digest(2), 50).unwrap();
    assert_eq!(seen.id, "dev-b");
    assert_eq!(seen.last_seen_ms, Some(50));
    assert!(registry.authenticate(&digest(3), 50).is_none());
    assert_eq!(registry.revoke("dev-b").unwrap().id, "dev-b");
    assert!(registry.authenticate(&digest(2), 60).is_none());
    assert!(!registry.contains("dev-b"));
    assert_eq!(registry.revoke("dev-b"), Err(DeviceError::UnknownDevice));
    assert!(registry.authenticate(&digest(1), 60).is_some());
    // The stored record never holds the token, and Debug hides the digest
    // and the codes.
    let debug = format!("{:?}", registry.devices());
    assert!(debug.contains("[redacted]"));
    start(&mut registry, DeviceRole::Viewer, 70);
    assert!(!format!("{:?}", registry.pairing(70)).contains("123456"));
}

#[test]
fn device_roles_grant_their_page_roles_only() {
    assert_eq!(DeviceRole::Viewer.granted_roles(), &[Role::Viewer]);
    assert!(!DeviceRole::Audio.granted_roles().contains(&Role::Operator));
    assert!(DeviceRole::Operator.granted_roles().contains(&Role::Viewer));
    for role in [DeviceRole::Viewer, DeviceRole::Audio, DeviceRole::Operator] {
        assert!(!role.granted_roles().contains(&Role::Admin));
        assert_eq!(DeviceRole::from_token(role.token()), Some(role));
    }
}

#[test]
fn registry_bounds_devices_and_names() {
    let devices = (0..MAX_PAIRED_DEVICES)
        .map(|index| PairedDevice {
            id: format!("dev-{index}"),
            name: format!("Device {index}"),
            role: DeviceRole::Viewer,
            paired_at_ms: 0,
            last_seen_ms: None,
            digest: digest(u8::try_from(index).unwrap()),
        })
        .collect();
    let mut registry = DeviceRegistry::with_devices(devices).unwrap();
    assert_eq!(
        registry
            .start_pairing(code("1"), code("2"), DeviceRole::Viewer, 0, 1_000)
            .unwrap_err(),
        DeviceError::TooManyDevices
    );
    assert_eq!(
        sanitize_device_name("  Cam\u{7}era\n2  ").as_deref(),
        Some("Cam era 2")
    );
    assert_eq!(sanitize_device_name(" \n "), None);
    assert!(sanitize_device_name(&"x".repeat(200)).unwrap().len() <= 48);
}

#[test]
fn attempt_limiter_bounds_each_source_and_everyone() {
    let mut limiter = AttemptLimiter::new(60_000, 3, 5, 4);
    for at in 0..3 {
        limiter.attempt(&"a", at).unwrap();
    }
    assert_eq!(limiter.attempt(&"a", 10), Err(59_990));
    limiter.attempt(&"b", 10).unwrap();
    limiter.attempt(&"c", 10).unwrap();
    // The global ceiling stops a source that never tried.
    assert!(limiter.attempt(&"d", 11).is_err());
    // The window slides.
    limiter.attempt(&"a", 60_000).unwrap();
}
