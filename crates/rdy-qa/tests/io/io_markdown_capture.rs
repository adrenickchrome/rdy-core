//! m4bc4 (D-19BZD): a Markdown import never follows a link inside its source. A source entry
//! that is a second name for a directory (a symbolic link on Unix, a junction on Windows) is an
//! unsafe source path: the capture refuses it, and the preview and the import refuse before any
//! append or view claim. An account whose system refuses to follow such a link (Windows error
//! 448) gets a refusal of another wording; the point stays: no write, nothing read through it.
use rdy_engine::engine::{Engine, Local};
use rdy_engine::import_api::{ImportOptions, run_import_with};
use rdy_engine::markdown_import::files;
use rdy_engine::*;
use rdy_model::Role;
use rdy_qa::platform;
use rdy_store::Store;
use std::path::Path;

const VIEW: &str = "md";

fn writer() -> Session {
    Session {
        actor: "human:imp".into(),
        run: None,
        role: Role::Writer,
        kind: SessionKind::Human,
    }
}

/// The project's log boundary, refs and frame count after reopen and reindex.
fn durable(project: &Path) -> (String, Vec<(String, Vec<u8>)>, usize) {
    let mut s = Store::open(&project.join(".rdy").join("store")).unwrap();
    s.reindex().unwrap();
    (
        format!("{:?}", s.log_end()),
        s.reader().refs_with_prefix("").unwrap(),
        s.verify().unwrap(),
    )
}

#[test]
fn m4bc4_linked_source_entries_refuse_without_a_write() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project).unwrap();
    Rdy::init(&project, "rdy-standard")
        .unwrap()
        .close()
        .unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret.md"), "# Secret\n").unwrap();

    // a link at the top of the source, one a level down, and one to a directory inside it
    for (rel, inside) in [("linked", false), ("guide/linked", false), ("again", true)] {
        let src = dir.path().join(format!("src-{}", rel.replace('/', "-")));
        std::fs::create_dir_all(src.join("guide")).unwrap();
        std::fs::write(src.join("a.md"), "# A\n").unwrap();
        let target = if inside {
            src.join("guide")
        } else {
            outside.clone()
        };
        let link = rel.split('/').fold(src.clone(), |p, s| p.join(s));
        platform::dir_alias(&target, &link).unwrap();
        let refused = platform::follow_refused(&link.join("x"));
        let want = (
            E_BAD_INPUT,
            format!("Markdown source path is not a contained regular file or directory: {rel}"),
            "copy regular files into a private source directory",
        );
        let check = |e: &RdyError, what: &str| {
            if !refused {
                assert_eq!(
                    (e.code, e.message.clone(), e.fix.as_str()),
                    (want.0, want.1.clone(), want.2),
                    "{rel}: {what}"
                );
            }
        };
        check(&files::capture(&src).unwrap_err(), "the capture");
        for dry_run in [true, false] {
            let was = durable(&project);
            let engine = Local::new(Rdy::open(&project).unwrap());
            let options = ImportOptions {
                md: true,
                dry_run,
                expected_source: None,
            };
            let e = run_import_with(&engine, &writer(), VIEW, &src, &options).unwrap_err();
            check(&e, if dry_run { "the preview" } else { "the import" });
            assert!(engine.read().view_head(VIEW).is_none(), "{rel}: no view");
            engine.close().unwrap();
            assert_eq!(durable(&project), was, "{rel}: no write");
        }
        platform::remove_dir_link(&link).unwrap();
        // without the link the same source imports
        let engine = Local::new(Rdy::open(&project).unwrap());
        let options = ImportOptions {
            md: true,
            dry_run: true,
            expected_source: None,
        };
        run_import_with(&engine, &writer(), VIEW, &src, &options).unwrap();
        engine.close().unwrap();
    }
}
