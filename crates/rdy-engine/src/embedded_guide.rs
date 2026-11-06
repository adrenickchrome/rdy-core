//! The agent guide this binary ships (D-GEKKY). `build.rs` checks the committed public export and
//! compiles it in, so nothing here reads a disk, a network or a guide file at run time. A
//! development build carries the last committed export, which may be older than the live guide.

/// One delivered row. Its id is an opaque string, not a block id, and it stores no reason.
#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub id: &'static str,
    pub text: &'static str,
    pub locked: bool,
}

/// One built-in ruleset. `name` has no `rdy:` prefix. Its rows are in delivery order.
#[derive(Debug, PartialEq, Eq)]
pub struct Ruleset {
    pub name: &'static str,
    pub title: &'static str,
    pub when: &'static str,
    pub required: bool,
    pub rows: &'static [Row],
}

include!(concat!(env!("OUT_DIR"), "/embedded_guide.rs"));
