use crate::{CommandReceipt, IdempotencyKey};
use std::collections::{BTreeSet, HashMap};
use std::num::NonZeroUsize;

/// Recency of a receipt: the revision it was recorded at, with a rejection
/// ranked just after the accepted command that produced the same revision,
/// and ties broken by key so eviction is deterministic.
type Recency = (u64, bool, IdempotencyKey);

fn recency<R>(key: IdempotencyKey, receipt: &CommandReceipt<R>) -> Recency {
    match receipt {
        CommandReceipt::Accepted { acceptance, .. } => (acceptance.revision.get(), false, key),
        CommandReceipt::Rejected { rejection, .. } => (rejection.current_revision.get(), true, key),
    }
}

/// Idempotency receipts with optional bounded retention.
///
/// Lookups are `O(1)`; inserting and evicting are `O(log n)`.
#[derive(Clone, Debug)]
pub(super) struct ReceiptLog<R> {
    entries: HashMap<IdempotencyKey, CommandReceipt<R>>,
    order: BTreeSet<Recency>,
    limit: Option<NonZeroUsize>,
}

impl<R> ReceiptLog<R> {
    pub(super) fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeSet::new(),
            limit: None,
        }
    }

    pub(super) fn from_receipts(
        receipts: impl IntoIterator<Item = (IdempotencyKey, CommandReceipt<R>)>,
    ) -> Self {
        let mut log = Self::new();
        for (key, receipt) in receipts {
            log.insert_unbounded(key, receipt);
        }
        log
    }

    pub(super) const fn limit(&self) -> Option<NonZeroUsize> {
        self.limit
    }

    /// Bounds retention, evicting the oldest receipts beyond `limit`.
    pub(super) fn set_limit(&mut self, limit: NonZeroUsize) {
        self.limit = Some(limit);
        self.evict_beyond_limit(None);
    }

    pub(super) fn get(&self, key: &IdempotencyKey) -> Option<&CommandReceipt<R>> {
        self.entries.get(key)
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&IdempotencyKey, &CommandReceipt<R>)> {
        self.entries.iter()
    }

    /// Records a receipt, then evicts the oldest receipts beyond the limit.
    ///
    /// The receipt just recorded is never evicted.
    pub(super) fn insert(&mut self, key: IdempotencyKey, receipt: CommandReceipt<R>) {
        let pinned = key.clone();
        self.insert_unbounded(key, receipt);
        self.evict_beyond_limit(Some(&pinned));
    }

    fn insert_unbounded(&mut self, key: IdempotencyKey, receipt: CommandReceipt<R>) {
        let rank = recency(key, &receipt);
        if let Some(previous) = self.entries.insert(rank.2.clone(), receipt) {
            self.order.remove(&recency(rank.2.clone(), &previous));
        }
        self.order.insert(rank);
    }

    fn evict_beyond_limit(&mut self, pinned: Option<&IdempotencyKey>) {
        let Some(limit) = self.limit else {
            return;
        };
        while self.entries.len() > limit.get() {
            let Some(oldest) = self
                .order
                .iter()
                .take(2)
                .find(|(_, _, key)| Some(key) != pinned)
                .cloned()
            else {
                return;
            };
            self.order.remove(&oldest);
            self.entries.remove(&oldest.2);
        }
    }
}
