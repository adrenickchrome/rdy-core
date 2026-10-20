//! Test-only instrumentation for hpb13 (ruling task-13-round1); compiled out unless the probe feature is on.
//!
//! A count of the renderer's entry points, for a test that must show a caller renders nothing.
//!
//! This module exists only with the `probe` feature. [`renders`] returns how many times this
//! thread has entered a render: the render process ([`crate::process::run`]), a full publication
//! build, a one-page render or the preparation they share. Without the feature the module is
//! private to the crate and holds a no-op `hit()` (see `lib.rs`), so production builds expose
//! and carry no counter. The count is per thread, so tests that run side by side do not see each
//! other's renders.

thread_local! {
    static COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many render entry points this thread has entered so far.
pub fn renders() -> u64 {
    COUNT.get()
}

/// Records one entry into a render. Called first in every render entry point.
#[inline]
pub(crate) fn hit() {
    COUNT.set(COUNT.get().wrapping_add(1));
}
