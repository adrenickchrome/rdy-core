//! Test-only instrumentation for hpb13 (ruling task-13-round1); compiled out unless the probe feature is on.
//!
//! A count of the renderer's entry points, for a test that must show a caller renders nothing.
//!
//! This module exists only with the `probe` feature. [`renders`] returns how many times this
//! thread has entered a render: the render process ([`crate::process::run`]), a full publication
//! build, a one-page render or the preparation they share. Without the feature the module is
//! private to the crate and holds a no-op `hit()` (see `lib.rs`), so production builds expose
//! and carry no counter. The count is per thread, so tests that run side by side do not see each
//! other's renders. A second count, [`preparations`], counts the preparations of a source or a
//! profile apart from the renders.

thread_local! {
    static COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static PREPARATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many render entry points this thread has entered so far.
pub fn renders() -> u64 {
    COUNT.get()
}

/// How many source or profile preparations this thread has run so far. A render entry point
/// does not add to it; see [`renders`].
pub fn preparations() -> u64 {
    PREPARATIONS.get()
}

/// Records one preparation of a source or a profile. Called first in each preparation.
#[inline]
pub(crate) fn prepared() {
    PREPARATIONS.set(PREPARATIONS.get().wrapping_add(1));
}

/// Records one entry into a render. Called first in every render entry point.
#[inline]
pub(crate) fn hit() {
    COUNT.set(COUNT.get().wrapping_add(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two counters are separate: a preparation is not a render, and a render is not a
    /// preparation. Each check compares deltas on this thread.
    #[test]
    fn rdb2_preparation_probe_is_separate() {
        let (renders_before, preparations_before) = (renders(), preparations());
        prepared();
        assert_eq!(preparations() - preparations_before, 1, "prepared adds one");
        assert_eq!(renders() - renders_before, 0, "prepared leaves renders");
        let (renders_before, preparations_before) = (renders(), preparations());
        hit();
        assert_eq!(renders() - renders_before, 1, "hit adds one render");
        assert_eq!(
            preparations() - preparations_before,
            0,
            "hit leaves preparations"
        );
    }
}
