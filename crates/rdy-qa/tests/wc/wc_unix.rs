#![cfg(unix)]
// Unix only: a FIFO in a directory, and a file name of raw bytes. Windows has neither.
//! wcly1: what only a Unix tree can hold is untracked. The Windows counterparts are a socket entry
//! and a name with a lone surrogate, in `wcly1_hostile_entries_are_untracked_never_followed`.
use crate::wc_layout::{DOC, pack, put};
use rdy_engine::wc::ignore::Ignore;
use rdy_engine::wc::layout::*;

#[test]
fn wcly1_a_fifo_and_a_name_of_raw_bytes_are_untracked() {
    let t = tempfile::tempdir().unwrap();
    let r = t.path();
    put(r, "ok.yaml", DOC);
    assert!(
        std::process::Command::new("mkfifo")
            .arg(r.join("pipe.yaml"))
            .status()
            .unwrap()
            .success()
    );
    // APFS refuses a name that is not UTF-8
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        std::fs::write(r.join(std::ffi::OsStr::from_bytes(b"bad\xff.yaml")), DOC).unwrap();
    }
    let w = read_tree(r, &Ignore::empty(), &pack());
    assert!(w.files.contains_key("ok.yaml"));
    let reasons: std::collections::BTreeMap<&str, &str> = w
        .untracked
        .iter()
        .map(|u| (u.path.as_str(), u.reason.as_str()))
        .collect();
    assert!(
        reasons["pipe.yaml"].contains("not a regular file"),
        "{reasons:?}"
    );
    #[cfg(target_os = "linux")]
    assert!(
        reasons
            .iter()
            .any(|(k, v)| k.starts_with("bad") && v.contains("UTF-8")),
        "{reasons:?}"
    );
}
