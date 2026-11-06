//! The harness's platform helpers, in one place (FW-XP1 D-1RXEA). Each platform's calls live in a
//! child module, `unix` or `windows`; nothing else in the harness names a platform's mechanism.

// the child fixtures are the same on every platform and exist only in a test build
#[cfg(test)]
mod child_fixture;
#[cfg(test)]
#[allow(unused_imports)]
pub use child_fixture::*;
#[cfg(test)]
mod child_fixture_tests;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

// Stays until every export the harness calls exists on Windows. Until then the Windows children
// build only in an isolated probe.
#[cfg(not(unix))]
compile_error!("rdy_qa::platform has no implementation for this platform yet (FW-XP1 Phase 4)");
