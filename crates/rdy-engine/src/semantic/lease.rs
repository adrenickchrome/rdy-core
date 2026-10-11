//! Leases on review requests: in memory only, never stored (D-TF2W6). A restart drops them; the request is derived and is offered again.
use rdy_model::{BlockId, Hash};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::{AtomicU64, Ordering};

/// A claimed review request: who holds it, until when, and the pair it names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lease {
    /// The session that claimed it (the daemon's session id, or [`LeaseRegistry::local_owner`]).
    pub owner: u64,
    /// The first millisecond at which the lease no longer holds.
    pub until_ms: i64,
    /// The two blocks of the request, each with the version it was claimed at.
    pub pair: Vec<(BlockId, Hash)>,
}

/// The live leases of one open project, by request key.
#[derive(Default)]
pub struct LeaseRegistry {
    inner: Mutex<BTreeMap<Hash, Lease>>,
}

/// The next owner for a session with no daemon id, counting down from `u64::MAX`.
static NEXT_LOCAL_OWNER: AtomicU64 = AtomicU64::new(u64::MAX);

impl LeaseRegistry {
    pub fn new() -> LeaseRegistry {
        LeaseRegistry::default()
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<Hash, Lease>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A process-unique owner for a session with no daemon id.
    pub fn local_owner() -> u64 {
        NEXT_LOCAL_OWNER.fetch_sub(1, Ordering::Relaxed)
    }

    /// Leases `key` to `owner` for `ttl_ms` from `now_ms`. False when another live lease holds
    /// `key`. With `ttl_ms <= 0` nothing is recorded and the answer is true. An expired lease
    /// is replaced; the owner's own live lease is renewed.
    pub fn claim(
        &self,
        key: Hash,
        pair: Vec<(BlockId, Hash)>,
        owner: u64,
        now_ms: i64,
        ttl_ms: i64,
    ) -> bool {
        if ttl_ms <= 0 {
            return true;
        }
        let mut leases = self.lock();
        if leases
            .get(&key)
            .is_some_and(|l| l.until_ms > now_ms && l.owner != owner)
        {
            return false;
        }
        leases.insert(
            key,
            Lease {
                owner,
                until_ms: now_ms.saturating_add(ttl_ms),
                pair,
            },
        );
        true
    }

    /// The live lease on `key`; an expired one is dropped and counts as none.
    pub fn get(&self, key: &Hash, now_ms: i64) -> Option<Lease> {
        let mut leases = self.lock();
        Self::drop_expired(&mut leases, now_ms);
        leases.get(key).cloned()
    }

    /// The keys of the live leases; expired ones are dropped.
    pub fn leased(&self, now_ms: i64) -> BTreeSet<Hash> {
        let mut leases = self.lock();
        Self::drop_expired(&mut leases, now_ms);
        leases.keys().copied().collect()
    }

    /// The live leases whose key starts with `hex`, in key order.
    pub fn with_prefix(&self, hex: &str, now_ms: i64) -> Vec<(Hash, Lease)> {
        let mut leases = self.lock();
        Self::drop_expired(&mut leases, now_ms);
        leases
            .iter()
            .filter(|(k, _)| k.to_string().starts_with(hex))
            .map(|(k, l)| (*k, l.clone()))
            .collect()
    }

    /// Frees the lease on `key`.
    pub fn release(&self, key: &Hash) {
        self.lock().remove(key);
    }

    /// Frees every lease `owner` holds; how many it held.
    pub fn release_owner(&self, owner: u64) -> usize {
        let mut leases = self.lock();
        let before = leases.len();
        leases.retain(|_, l| l.owner != owner);
        before - leases.len()
    }

    fn drop_expired(leases: &mut BTreeMap<Hash, Lease>, now_ms: i64) {
        leases.retain(|_, l| l.until_ms > now_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> Vec<(BlockId, Hash)> {
        vec![(BlockId::parse("c-AAAAA").unwrap(), Hash([1; 32]))]
    }

    #[test]
    fn se2_a_claim_holds_until_its_ttl_and_then_lapses() {
        let r = LeaseRegistry::new();
        let k = Hash([5; 32]);
        assert!(r.claim(k, pair(), 7, 0, 600_000));
        assert!(!r.claim(k, pair(), 8, 1, 600_000));
        assert!(r.leased(599_999).contains(&k));
        assert!(r.leased(600_000).is_empty());
        assert!(r.claim(k, pair(), 8, 600_000, 600_000));
        assert_eq!(r.get(&k, 600_001).unwrap().owner, 8);
    }

    #[test]
    fn se2_release_and_release_owner_free_the_leases() {
        let r = LeaseRegistry::new();
        let (a, b, c) = (Hash([1; 32]), Hash([2; 32]), Hash([3; 32]));
        assert!(r.claim(a, pair(), 7, 0, 1000));
        assert!(r.claim(b, pair(), 7, 0, 1000));
        assert!(r.claim(c, pair(), 8, 0, 1000));
        assert_eq!(r.release_owner(7), 2);
        assert_eq!(r.leased(1).into_iter().collect::<Vec<_>>(), vec![c]);
        r.release(&c);
        assert!(r.leased(1).is_empty());
    }

    #[test]
    fn se2_a_zero_ttl_records_no_lease() {
        let r = LeaseRegistry::new();
        let k = Hash([5; 32]);
        assert!(r.claim(k, pair(), 7, 0, 0));
        assert!(r.claim(k, pair(), 8, 0, -5));
        assert!(r.leased(0).is_empty());
        assert!(r.get(&k, 0).is_none());
    }

    #[test]
    fn se2_with_prefix_finds_live_leases_by_hex() {
        let r = LeaseRegistry::new();
        let k = Hash([0xab; 32]);
        assert!(r.claim(k, pair(), 7, 0, 1000));
        let hex = k.to_string();
        let found = r.with_prefix(&hex[..8], 1);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, k);
        assert_eq!(found[0].1.owner, 7);
        assert!(r.with_prefix("00", 1).is_empty());
        assert!(r.with_prefix(&hex[..8], 1000).is_empty());
    }

    #[test]
    fn se2_local_owners_are_distinct() {
        let (a, b) = (LeaseRegistry::local_owner(), LeaseRegistry::local_owner());
        assert_ne!(a, b);
        assert!(a > u64::MAX - 1000 && b > u64::MAX - 1000);
    }
}
