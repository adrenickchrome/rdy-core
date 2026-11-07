#![cfg(unix)]
// Unix only: it reads permission bits, which Windows files do not have in this form.
//! The source tool stages the crate files with mode 0644 (Unix). Windows has no mode bits; its
//! staged files are the locked bytes, which the portable test beside this one checks.
use super::release_notices::{CHUNKS_SHA, IMBL_SHA, contents, run};
use rdy_qa::platform;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;

#[test]
fn plx7_source_tool_stages_crate_files_with_mode_0644() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("out");
    let done = run(&[
        "--version".as_ref(),
        OsStr::new(env!("CARGO_PKG_VERSION")),
        "--out".as_ref(),
        out.as_os_str(),
    ]);
    assert_eq!(done.status.code(), Some(0), "{done:?}");
    let staged: Vec<(String, String, u32)> = contents(&out)
        .into_iter()
        .map(|(name, bytes)| {
            let mode = platform::mode_of(&out.join(&name)).expect("mode");
            (name, format!("{:x}", Sha256::digest(&bytes)), mode)
        })
        .collect();
    assert_eq!(
        staged,
        [
            ("imbl-7.0.2.crate".to_string(), IMBL_SHA.to_string(), 0o644),
            (
                "imbl-sized-chunks-0.2.0.crate".to_string(),
                CHUNKS_SHA.to_string(),
                0o644
            ),
        ]
    );
}
