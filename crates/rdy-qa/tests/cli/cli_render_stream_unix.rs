#![cfg(unix)]
// Unix only: it sets a directory's mode bits through rdy_qa::platform::set_mode.
//! hpb9: `rdy render-stream` writes nothing beside its standard output, so a read-only working
//! directory changes no byte of its output.
use crate::cli_render_stream::{artifact_of, rdy_in, source};
use rdy_engine::publication::IMPLICIT_ORG;
use rdy_render::process::RenderFormat;

#[test]
fn hpb9_process_runs_from_an_unwritable_directory() {
    let s = source();
    let home = tempfile::tempdir().unwrap();
    let writable = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let dir = base.path().join("read-only");
    std::fs::create_dir(&dir).unwrap();
    let inputs = [
        s.request(IMPLICIT_ORG, RenderFormat::Site),
        s.request(&s.profile, RenderFormat::Markdown),
        s.request(&s.profile, RenderFormat::Techdocs),
    ];
    let want: Vec<Vec<u8>> = inputs
        .iter()
        .map(|i| rdy_in(writable.path(), home.path(), &["render-stream"], i).stdout)
        .collect();
    rdy_qa::platform::set_mode(&dir, 0o555).unwrap();
    let probe = std::fs::File::create(dir.join("probe")).is_ok();
    let got: Vec<_> = inputs
        .iter()
        .map(|i| rdy_in(&dir, home.path(), &["render-stream"], i))
        .collect();
    rdy_qa::platform::set_mode(&dir, 0o755).unwrap();
    if probe {
        // a user the mode does not bind (root): nothing to test; RDY_QA_REQUIRE_ALL=1 fails it
        rdy_qa::skip::skip(
            "hpb9_process_runs_from_an_unwritable_directory",
            rdy_qa::skip::Why::Prerequisite,
            "a read-only directory takes writes here (running as root?)",
        );
        return;
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
}
