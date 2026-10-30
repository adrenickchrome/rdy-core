//! The per-state memos of the gate's landing reads. One propose reads the landing of the same
//! changes onto the same canon several times: its change plan, the readiness checks and the
//! reach of its assessment (D-ME2HD, one readiness function). A [`ReadState`] is immutable, so
//! each of these reads gives the same result every time on one state. A memo keeps that result
//! for its own state only:
//! - every key holds every input of its read that can differ between calls on one state;
//! - only an `Ok` result is kept, so an error is computed again (and worded by its caller);
//! - each memo holds at most [`READ_MEMO_SLOTS`] entries, and drops the least recently used one
//!   first;
//! - a publish builds the next state with empty memos, so a memo never outlives its state or
//!   crosses a write.
//!
//! [`ReadState`]: crate::ReadState

use crate::Materialized;
use crate::session::Scope;
use crate::tensions::Tension;
use rdy_model::Hash;
use rdy_schema::Issue;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The most entries one memo holds. A propose reads one change list, and the gate reads at most
/// two (with and without rdy's key change); the rest is room for a reader that lists several
/// proposals on one state.
pub(crate) const READ_MEMO_SLOTS: usize = 8;

/// The id of the next state's memos.
static NEXT_STATE_ID: AtomicU64 = AtomicU64::new(1);

/// The memos of one state.
pub(crate) struct ReadMemo {
    /// This state's id, unique in the process: a plan computed on one state is used on that
    /// state only (`plan::compute::CarriedPlan`).
    pub(crate) id: u64,
    /// `ReadState::landing_order` and `gate::prospective`: the merge's applied order onto canon
    /// of the changes with these ids, when the merge succeeds.
    pub(crate) order: Slots<Vec<Hash>, Arc<Vec<usize>>>,
    /// `gate::prospective`: the canon that landing the changes with these ids gives.
    pub(crate) prospective: Slots<Vec<Hash>, Arc<Materialized>>,
    /// `gate::prospective_issues_of`: the issues of that canon.
    pub(crate) issues: Slots<Vec<Hash>, Arc<Vec<Issue>>>,
    /// `ReadState::landing_detect_in` with the state's own detectors.
    pub(crate) landing: Slots<LandingKey, Arc<Vec<Arc<Tension>>>>,
}

impl Default for ReadMemo {
    fn default() -> ReadMemo {
        ReadMemo {
            id: NEXT_STATE_ID.fetch_add(1, Ordering::Relaxed),
            order: Slots::default(),
            prospective: Slots::default(),
            issues: Slots::default(),
            landing: Slots::default(),
        }
    }
}

/// What one landing detection reads besides the state and its detectors: the proposing view, the
/// ids of the changes it judges, the ids of the changes it applies, their applied order, and the
/// canon it runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LandingKey {
    pub(crate) view: String,
    pub(crate) changes: Vec<Hash>,
    pub(crate) read: Vec<Hash>,
    pub(crate) applied: Vec<usize>,
    pub(crate) canon: Scope,
}

/// One memo: at most [`READ_MEMO_SLOTS`] (key, value) entries, the least recently used at the
/// front. No lock is held while a value is computed.
pub(crate) struct Slots<K, V>(Mutex<VecDeque<(K, V)>>);

impl<K, V> Default for Slots<K, V> {
    fn default() -> Slots<K, V> {
        Slots(Mutex::new(VecDeque::new()))
    }
}

impl<K: PartialEq, V: Clone> Slots<K, V> {
    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<(K, V)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The value for `key`, marked most recently used.
    pub(crate) fn get(&self, key: &K) -> Option<V> {
        let mut slots = self.lock();
        let at = slots.iter().position(|(k, _)| k == key)?;
        let hit = slots.remove(at)?;
        let v = hit.1.clone();
        slots.push_back(hit);
        Some(v)
    }

    /// Keeps `value` for `key` as the most recently used entry, in place of an entry for the same
    /// key, and drops the least recently used beyond the cap.
    pub(crate) fn put(&self, key: K, value: V) {
        let mut slots = self.lock();
        slots.retain(|(k, _)| k != &key);
        slots.push_back((key, value));
        while slots.len() > READ_MEMO_SLOTS {
            slots.pop_front();
        }
    }

    /// The value for `key`, computed by `f` and kept when it is not held. An error is returned
    /// and not kept.
    pub(crate) fn get_or_try<E>(&self, key: K, f: impl FnOnce() -> Result<V, E>) -> Result<V, E> {
        if let Some(v) = self.get(&key) {
            return Ok(v);
        }
        let v = f()?;
        self.put(key, v.clone());
        Ok(v)
    }

    /// How many entries it holds.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgperf_slots_keep_at_most_the_cap_and_drop_the_least_recently_used() {
        let s: Slots<usize, usize> = Slots::default();
        for k in 0..READ_MEMO_SLOTS {
            s.put(k, k * 10);
        }
        // a read marks 0 used, so 1 is the least recently used
        assert_eq!(s.get(&0), Some(0));
        s.put(READ_MEMO_SLOTS, 1);
        assert_eq!(s.len(), READ_MEMO_SLOTS);
        assert_eq!(s.get(&1), None);
        assert_eq!(s.get(&0), Some(0));
        // a second put for one key replaces its entry
        s.put(0, 7);
        assert_eq!(s.get(&0), Some(7));
        assert_eq!(s.len(), READ_MEMO_SLOTS);
    }

    #[test]
    fn rgperf_slots_keep_no_error() {
        let s: Slots<u8, u8> = Slots::default();
        assert_eq!(s.get_or_try(1, || Err::<u8, &str>("no")), Err("no"));
        assert_eq!(s.len(), 0);
        assert_eq!(s.get_or_try(1, || Ok::<u8, &str>(2)), Ok(2));
        // kept: the second call does not compute
        assert_eq!(s.get_or_try(1, || Err::<u8, &str>("not called")), Ok(2));
    }

    #[test]
    fn rgperf_each_state_has_its_own_id() {
        let (a, b) = (ReadMemo::default(), ReadMemo::default());
        assert_ne!(a.id, b.id);
    }
}
