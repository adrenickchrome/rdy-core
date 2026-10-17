//! ec11 fix round 1: `print::export` applies O29's path rules for every format before it builds
//! the print. The project's `.rdy` directory is found by its resolved path (a symlink, its
//! physical path or `..` does not pass); a directory named `.rdy` elsewhere is an ordinary path;
//! an `out` with no file name is refused the same way for every format.
use rdy_engine::{E_BAD_INPUT, Rdy, Scope};
use rdy_qa::golden::*;
use rdy_render::print::{self, Format};
use std::path::Path;

const FORMATS: [Format; 3] = [Format::Pdf, Format::Html, Format::Md];

/// Every path under `dir` (files and directories), relative to it, sorted.
fn paths(dir: &Path) -> Vec<String> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if e.file_type().unwrap().is_dir() {
                stack.push(p.clone());
            }
            out.push(p.strip_prefix(dir).unwrap().to_string_lossy().into_owned());
        }
    }
    out.sort();
    out
}

/// `out` is refused as inside the project's `.rdy` directory for every format, and `store`
/// holds the same paths after each refusal.
fn assert_refused_inside(r: &Rdy, out: &Path, store: &Path) {
    for format in FORMATS {
        let before = paths(store);
        let e = print::export(&r.read(), &Scope::Canon, out, format, None)
            .expect_err("an out inside the project .rdy directory");
        assert_eq!(e.code, E_BAD_INPUT, "{format:?}: {}", e.message);
        assert_eq!(
            e.message,
            format!("{} is inside the project .rdy directory", out.display()),
            "{format:?}"
        );
        assert_eq!(e.fix, "export to a path outside .rdy", "{format:?}");
        assert!(
            std::fs::symlink_metadata(out).is_err(),
            "{format:?}: no out"
        );
        assert_eq!(paths(store), before, "{format:?}: no new file in the store");
    }
}

/// Finding 1: the project's `.rdy` is a symlink to a directory with no `.rdy` in its path. A
/// Markdown export into the store, by the symlink or the physical path, must not make the store
/// unopenable (a `seg-000002.log` directory without a `seg-000001.log` fails the next open).
#[test]
fn ec11_every_format_refuses_a_symlinked_project_store_by_either_path() {
    let (d, r, _ids) = project();
    drop(r);
    let storage = tempfile::tempdir().unwrap();
    let physical = storage.path().join("physical-store");
    let rdy = d.path().join(".rdy");
    std::fs::rename(&rdy, &physical).unwrap();
    rdy_qa::platform::symlink(&physical, &rdy).unwrap();
    let r = Rdy::open(d.path()).expect("the store opens through the symlink");
    let segments = physical.join("store").join("segments");
    assert!(segments.is_dir(), "guard: the store's segments directory");
    for out in [
        rdy.join("store").join("segments").join("seg-000002.log"),
        segments.join("seg-000002.log"),
        rdy.join("md"),
        physical.join("md"),
    ] {
        assert_refused_inside(&r, &out, &physical);
    }
    drop(r);
    let r = Rdy::open(d.path()).expect("the store opens again");
    r.fsck().expect("fsck is clean");
}

/// Finding 1, the plain layout: the physical `.rdy` directory, a path through `..`, and the
/// store's own segments directory.
#[test]
fn ec11_every_format_refuses_the_project_store_by_its_physical_path() {
    let (d, r, _ids) = project();
    let rdy = d.path().join(".rdy");
    let other = d.path().join("other");
    std::fs::create_dir(&other).unwrap();
    for out in [
        rdy.join("store").join("segments").join("seg-000002.log"),
        rdy.join("md"),
        other.join("..").join(".rdy").join("md"),
    ] {
        assert_refused_inside(&r, &out, &rdy);
    }
    drop(r);
    let r = Rdy::open(d.path()).expect("the store opens again");
    r.fsck().expect("fsck is clean");
}

/// Finding 2: O29 is containment in this project's `.rdy` directory, not a reserved name. Every
/// format writes beneath an unrelated directory named `.rdy`, and to a new directory or file
/// named `.rdy` elsewhere.
#[test]
fn ec11_every_format_exports_beneath_an_unrelated_rdy_directory() {
    let (_d, r, _ids) = project();
    let scratch = tempfile::tempdir().unwrap();
    let external = scratch.path().join("external").join(".rdy");
    std::fs::create_dir_all(&external).unwrap();
    for (format, name) in [
        (Format::Pdf, "out.pdf"),
        (Format::Html, "out.html"),
        (Format::Md, "out-md"),
    ] {
        let out = external.join(name);
        let s = print::export(&r.read(), &Scope::Canon, &out, format, None)
            .unwrap_or_else(|e| panic!("{format:?} under an unrelated .rdy: {e}"));
        assert_eq!(s.out, out.display().to_string());
        assert!(out.exists(), "{format:?}");
    }
    assert!(external.join("out-md").join("index.md").is_file());
    // an `out` that is itself named `.rdy`, in a directory of its own for each format
    for (format, dir) in [(Format::Pdf, "p"), (Format::Html, "h"), (Format::Md, "m")] {
        let parent = scratch.path().join(dir);
        std::fs::create_dir(&parent).unwrap();
        let out = parent.join(".rdy");
        print::export(&r.read(), &Scope::Canon, &out, format, None)
            .unwrap_or_else(|e| panic!("{format:?} to a path named .rdy: {e}"));
        assert!(out.exists(), "{format:?}");
    }
    assert!(scratch.path().join("m/.rdy/index.md").is_file());
}

/// Finding 4: an `out` with no file name is refused alike for every format, with O29's text.
#[test]
fn ec11_every_format_refuses_an_out_with_no_file_name() {
    let (_d, r, _ids) = project();
    for format in FORMATS {
        let e = print::export(&r.read(), &Scope::Canon, Path::new("/"), format, None)
            .expect_err("no file name");
        assert_eq!(e.code, E_BAD_INPUT, "{format:?}: {}", e.message);
        assert_eq!(e.message, "/ does not name a directory", "{format:?}");
        assert_eq!(
            e.fix, "pass a directory path such as ./docs-md",
            "{format:?}"
        );
    }
    // the file writers apply the same rule when they are called on their own
    let p = print::build(&r.read(), &Scope::Canon).unwrap();
    let e = print::single::write_single_html(&p, Path::new("/")).expect_err("html");
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BAD_INPUT, "/ does not name a directory")
    );
    let e = print::pdf::write_pdf(&p, Path::new("/")).expect_err("pdf");
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BAD_INPUT, "/ does not name a directory")
    );
}
