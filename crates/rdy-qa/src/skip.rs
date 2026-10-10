//! Honest skips: a test whose prerequisite is missing writes one line the gate counts, and fails
//! when the run requires every prerequisite (`RDY_QA_REQUIRE_ALL=1`).
use std::io::Write;

/// The start of the line a skip writes.
pub const SKIP_MARK: &str = "RDY-QA-SKIP";
/// Set to `1`, a missing prerequisite fails its test.
pub const REQUIRE_ALL: &str = "RDY_QA_REQUIRE_ALL";

/// Why a test skips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// A tool, a permission or a filesystem the test needs is missing here.
    Prerequisite,
    /// The test runs only in a deep gate (`RDY_QA_DEEP=1`).
    DeepOnly,
}

/// Whether this run requires every prerequisite.
pub fn require_all() -> bool {
    std::env::var(REQUIRE_ALL).as_deref() == Ok("1")
}

/// The line a skip of `test` writes.
pub fn skip_line(test: &str, reason: &str) -> String {
    format!("{SKIP_MARK} {test}: {reason}")
}

/// Whether a skip fails its test: a missing prerequisite when every prerequisite is required.
pub fn fails(why: Why, require_all: bool) -> bool {
    require_all && why == Why::Prerequisite
}

/// Records a skip of `test`. The line goes to stderr by a direct write, because libtest captures
/// the print macros of a passing test and drops them. Panics when the skip fails (`fails`). The
/// caller returns after it.
pub fn skip(test: &str, why: Why, reason: &str) {
    let _ = writeln!(std::io::stderr().lock(), "{}", skip_line(test, reason));
    if fails(why, require_all()) {
        panic!("{test}: {reason}; {REQUIRE_ALL}=1 needs every prerequisite");
    }
}

/// Whether `node --version` runs and succeeds.
pub fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hd10_only_a_missing_prerequisite_under_require_all_fails() {
        assert!(fails(Why::Prerequisite, true));
        assert!(!fails(Why::Prerequisite, false));
        assert!(
            !fails(Why::DeepOnly, true),
            "deep-only tests skip in a standard run"
        );
        assert_eq!(
            skip_line("render::a", "node is not installed"),
            "RDY-QA-SKIP render::a: node is not installed"
        );
    }
}
