use std::collections::BTreeMap;

use crate::Redactor;

/// Component condition independent of liveness and readiness gates.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ComponentHealth {
    Healthy,
    Degraded,
    Unhealthy,
}

impl ComponentHealth {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Unhealthy => "unhealthy",
        }
    }
}

/// Latest health observation for one named component.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthCheck {
    pub component: String,
    pub live: bool,
    pub ready: bool,
    pub health: ComponentHealth,
    pub detail: Option<String>,
}

impl HealthCheck {
    #[must_use]
    pub fn new(
        component: impl Into<String>,
        live: bool,
        ready: bool,
        health: ComponentHealth,
    ) -> Self {
        Self {
            component: component.into(),
            live,
            ready,
            health,
            detail: None,
        }
    }

    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Process-level aggregation of all registered checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HealthAggregate {
    pub live: bool,
    pub ready: bool,
    pub degraded: bool,
    pub health: ComponentHealth,
    pub failing_components: Vec<String>,
}

/// Explicitly owned latest-value registry, deterministically ordered by name.
///
/// Checks are keyed by the caller's original component name so `get` and
/// `remove` work with the same name passed to `update`. The stored check (and
/// therefore everything `iter`, `aggregate`, and support bundles expose) has
/// its component name and detail redacted.
///
/// The registry holds at most [`HealthRegistry::MAX_COMPONENTS`] components.
/// Updates for a new component beyond that bound are dropped and counted; an
/// incomplete registry reports itself as not ready (degraded) so the overflow
/// cannot hide a failing component behind a healthy aggregate.
#[derive(Clone, Debug, Default)]
pub struct HealthRegistry {
    checks: BTreeMap<String, HealthCheck>,
    redactor: Redactor,
    dropped: u64,
}

impl HealthRegistry {
    /// Upper bound on distinct registered components.
    pub const MAX_COMPONENTS: usize = 1_024;

    #[must_use]
    pub const fn new() -> Self {
        Self {
            checks: BTreeMap::new(),
            redactor: Redactor,
            dropped: 0,
        }
    }

    #[must_use]
    pub const fn with_redactor(redactor: Redactor) -> Self {
        Self {
            checks: BTreeMap::new(),
            redactor,
            dropped: 0,
        }
    }

    /// Inserts or replaces a component's latest observation, keyed by its
    /// original name and stored with its name and detail redacted.
    ///
    /// A new component arriving when the registry already holds
    /// [`Self::MAX_COMPONENTS`] is dropped and counted in [`Self::dropped`].
    pub fn update(&mut self, mut check: HealthCheck) -> Option<HealthCheck> {
        let key = std::mem::take(&mut check.component);
        if !self.checks.contains_key(&key) && self.checks.len() >= Self::MAX_COMPONENTS {
            self.dropped = self.dropped.saturating_add(1);
            return None;
        }
        check.component = self.redactor.redact(&key);
        check.detail = check
            .detail
            .as_deref()
            .map(|detail| self.redactor.redact(detail));
        self.checks.insert(key, check)
    }

    /// Removes a component by the original name passed to [`Self::update`].
    pub fn remove(&mut self, component: &str) -> Option<HealthCheck> {
        self.checks.remove(component)
    }

    /// Looks up a component by the original name passed to [`Self::update`].
    #[must_use]
    pub fn get(&self, component: &str) -> Option<&HealthCheck> {
        self.checks.get(component)
    }

    /// Updates for new components rejected because the registry was full.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &HealthCheck> {
        self.checks.values()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.checks.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checks.is_empty()
    }

    #[must_use]
    pub fn aggregate(&self) -> HealthAggregate {
        let live = self.checks.values().all(|check| check.live);
        let has_unhealthy = self
            .checks
            .values()
            .any(|check| check.health == ComponentHealth::Unhealthy);
        let ready = live
            && !has_unhealthy
            && self.dropped == 0
            && self.checks.values().all(|check| check.ready);
        let has_degraded = self
            .checks
            .values()
            .any(|check| check.health == ComponentHealth::Degraded);
        let health = if !live || has_unhealthy {
            ComponentHealth::Unhealthy
        } else if !ready || has_degraded {
            ComponentHealth::Degraded
        } else {
            ComponentHealth::Healthy
        };
        let failing_components = self
            .checks
            .values()
            .filter(|check| !check.live || !check.ready || check.health != ComponentHealth::Healthy)
            .map(|check| check.component.clone())
            .collect();
        HealthAggregate {
            live,
            ready,
            degraded: health == ComponentHealth::Degraded,
            health,
            failing_components,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_with_redacted_names_are_found_and_removed_by_original_name() {
        let mut registry = HealthRegistry::new();
        let name = "camera 192.168.1.20 /dev/video0";
        registry.update(
            HealthCheck::new(name, true, true, ComponentHealth::Healthy)
                .with_detail("opened /dev/video0"),
        );
        let stored = registry.get(name).expect("lookup by original name");
        assert!(!stored.component.contains("192.168.1.20"), "{stored:?}");
        assert!(!stored.component.contains("/dev/video0"), "{stored:?}");
        assert!(
            registry
                .iter()
                .all(|check| !check.component.contains("192.168"))
        );
        assert!(
            registry
                .update(HealthCheck::new(
                    name,
                    true,
                    false,
                    ComponentHealth::Degraded
                ))
                .is_some()
        );
        assert_eq!(registry.len(), 1);
        assert!(registry.remove(name).is_some());
        assert!(registry.is_empty());
    }

    #[test]
    fn registry_is_bounded_and_overflow_is_not_ready() {
        let mut registry = HealthRegistry::new();
        for index in 0..HealthRegistry::MAX_COMPONENTS {
            registry.update(HealthCheck::new(
                format!("c{index}"),
                true,
                true,
                ComponentHealth::Healthy,
            ));
        }
        assert!(registry.aggregate().ready);
        registry.update(HealthCheck::new(
            "extra",
            true,
            true,
            ComponentHealth::Healthy,
        ));
        assert_eq!(registry.len(), HealthRegistry::MAX_COMPONENTS);
        assert_eq!(registry.dropped(), 1);
        assert!(registry.get("extra").is_none());
        let aggregate = registry.aggregate();
        assert!(!aggregate.ready);
        assert_eq!(aggregate.health, ComponentHealth::Degraded);
        // Existing components still update at capacity.
        registry.update(HealthCheck::new(
            "c0",
            false,
            false,
            ComponentHealth::Unhealthy,
        ));
        assert!(!registry.get("c0").unwrap().live);
    }

    #[test]
    fn aggregation_tracks_readiness_degradation_and_liveness() {
        let mut registry = HealthRegistry::new();
        registry.update(HealthCheck::new(
            "engine",
            true,
            true,
            ComponentHealth::Healthy,
        ));
        assert_eq!(registry.aggregate().health, ComponentHealth::Healthy);

        registry.update(HealthCheck::new(
            "output",
            true,
            false,
            ComponentHealth::Healthy,
        ));
        let not_ready = registry.aggregate();
        assert!(not_ready.live);
        assert!(!not_ready.ready);
        assert!(not_ready.degraded);

        registry.update(HealthCheck::new(
            "output",
            false,
            false,
            ComponentHealth::Unhealthy,
        ));
        let dead = registry.aggregate();
        assert!(!dead.live);
        assert!(!dead.ready);
        assert!(!dead.degraded);
        assert_eq!(dead.health, ComponentHealth::Unhealthy);
    }
}
