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

// the tests that leave Windows, with their Unix mechanism and Windows counterpart (Task 32)
pub mod exclusions;

/// Names the directory that [`short_tmp_root`] returns in place of the platform's default. The
/// gate sets it, with `TMPDIR` (or `TEMP` and `TMP`), to its `--tmp-root` for every test child.
pub const SHORT_TMP_VAR: &str = "RDY_QA_SHORT_TMP";

/// The value of [`SHORT_TMP_VAR`], when it is set and not empty.
fn short_tmp_override() -> Option<std::path::PathBuf> {
    std::env::var_os(SHORT_TMP_VAR)
        .filter(|v| !v.is_empty())
        .map(std::path::PathBuf::from)
}

// the test SSH agent's protocol is the same on every platform, only its endpoint differs
mod agent_wire;

/// The release target of `os`/`arch` (`gnu`: the GNU C library; for Windows, the MinGW
/// environment): macOS arm64, Linux (GNU) amd64 or Windows (MSVC) amd64. The one place where
/// the harness names the release targets of a machine, on every platform.
pub fn release_target_of(os: &str, arch: &str, gnu: bool) -> std::io::Result<&'static str> {
    match (os, arch, gnu) {
        ("macos", "aarch64", _) => Ok("aarch64-apple-darwin"),
        ("linux", "x86_64", true) => Ok("x86_64-unknown-linux-gnu"),
        ("windows", "x86_64", false) => Ok("x86_64-pc-windows-msvc"),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("rdy has no release for {os}/{arch}"),
        )),
    }
}

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

// A platform with neither child module has no harness.
#[cfg(not(any(unix, windows)))]
compile_error!("rdy_qa::platform has no implementation for this platform (FW-XP1 D-1RXEA)");
