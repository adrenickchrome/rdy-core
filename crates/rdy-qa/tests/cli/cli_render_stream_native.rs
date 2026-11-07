//! hpb9 and wx26d: `rdy render-stream` writes nothing beside its standard output, so a read-only
//! working directory changes no byte of its output, and neither does a working directory or home
//! whose native path has a space, a quote or a non-ASCII letter. The read-only directory is made
//! by `rdy_qa::platform::ReadOnlyDir`: a mode on Unix, an access list on Windows.
use crate::cli_render_stream::{artifact_of, rdy_in, source};
use rdy_engine::publication::IMPLICIT_ORG;
use rdy_qa::platform::ReadOnlyDir;
use rdy_render::process::RenderFormat;
use std::path::Path;

fn inputs_of(s: &crate::cli_render_stream::Source) -> [Vec<u8>; 3] {
    [
        s.request(IMPLICIT_ORG, RenderFormat::Site),
        s.request(&s.profile, RenderFormat::Markdown),
        s.request(&s.profile, RenderFormat::Techdocs),
    ]
}

/// The outputs of the three requests run in a writable plain directory.
fn golden(inputs: &[Vec<u8>]) -> Vec<Vec<u8>> {
    let writable = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    inputs
        .iter()
        .map(|i| rdy_in(writable.path(), home.path(), &["render-stream"], i).stdout)
        .collect()
}

/// Runs the three requests from a read-only directory called `name`; `false` when this account
/// writes there anyway (root or an elevated token) and the test skipped.
fn unwritable_cwd_keeps_output(name: &str, test: &str) -> bool {
    let s = source();
    let home = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join(name);
    std::fs::create_dir(&dir).unwrap();
    let inputs = inputs_of(&s);
    let want = golden(&inputs);
    let guard = ReadOnlyDir::new(&dir).expect("make the directory read-only");
    let probe = std::fs::File::create(dir.join("probe")).is_ok();
    let got: Vec<_> = inputs
        .iter()
        .map(|i| rdy_in(&dir, home.path(), &["render-stream"], i))
        .collect();
    drop(guard);
    if probe {
        // a user the access does not bind (root, an elevated token): nothing to test;
        // RDY_QA_REQUIRE_ALL=1 fails it
        rdy_qa::skip::skip(
            test,
            rdy_qa::skip::Why::Prerequisite,
            "a read-only directory takes writes here (root or an elevated token)",
        );
        return false;
    }
    for (o, want) in got.iter().zip(&want) {
        artifact_of(o);
        assert!(o.stdout == *want, "the read-only directory changed bytes");
    }
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        0,
        "nothing written"
    );
    // the guard gave the directory back: it takes a write again
    std::fs::write(dir.join("after"), "").expect("the directory is writable again");
    true
}

#[test]
fn hpb9_process_runs_from_an_unwritable_directory() {
    unwritable_cwd_keeps_output(
        "read-only",
        "hpb9_process_runs_from_an_unwritable_directory",
    );
}

#[test]
fn wx26d_unwritable_cwd_preserves_output_contract() {
    // a native name with a space, a quote and a non-ASCII letter, read-only
    unwritable_cwd_keeps_output(
        "read only \u{e9}'s dir",
        "wx26d_unwritable_cwd_preserves_output_contract",
    );
}

#[test]
fn wx26d_native_cli_paths_keep_golden_results() {
    let s = source();
    let inputs = inputs_of(&s);
    let want = golden(&inputs);
    let base = tempfile::tempdir().unwrap();
    let odd = |name: &str| -> std::path::PathBuf {
        let p = base.path().join(name);
        std::fs::create_dir(&p).unwrap();
        p
    };
    let cwd = odd("work dir \u{e9}'s");
    let home = odd("home dir \u{fc}");
    for (i, (input, want)) in inputs.iter().zip(&want).enumerate() {
        let o = rdy_in(&cwd, &home, &["render-stream"], input);
        artifact_of(&o);
        assert!(
            o.stdout == *want,
            "request {i}: a native path changed bytes"
        );
    }
    for p in [&cwd, &home] {
        assert_eq!(count(p), 0, "nothing written in {}", p.display());
    }
}

fn count(p: &Path) -> usize {
    std::fs::read_dir(p).unwrap().count()
}
