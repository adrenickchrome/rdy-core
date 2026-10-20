//! A count of the renderer's entry points, for a test that must show a caller renders nothing.
//!
//! With the `probe` feature, [`renders`] returns how many times this thread has entered a render:
//! the render process ([`crate::process::run`]), a full publication build, a one-page render or
//! the preparation they share. Without the feature nothing is counted and [`renders`] is 0, so
//! production builds carry no counter. The count is per thread, so tests that run side by side
//! do not see each other's renders.

/// How many render entry points this thread has entered so far (0 without the `probe` feature).
pub fn renders() -> u64 {
    #[cfg(feature = "probe")]
    {
        COUNT.get()
    }
    #[cfg(not(feature = "probe"))]
    {
        0
    }
}

#[cfg(feature = "probe")]
thread_local! {
    static COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Records one entry into a render. Called first in every render entry point.
#[inline]
pub(crate) fn hit() {
    #[cfg(feature = "probe")]
    COUNT.set(COUNT.get().wrapping_add(1));
}
