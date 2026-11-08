//! The source build id (D-CCC6E): framing, closed inputs, bounds and rerun directives.
#[path = "../build_support.rs"]
mod build_support;

use build_support::{BuildIdError, LIMITS, Limits, Phase, source_build_id, source_build_inputs};
use std::fs;
use std::path::Path;

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

fn tiny(root: &Path) {
    put(root, "Cargo.toml", b"a");
    put(root, "Cargo.lock", b"b");
    put(root, "crates/x/src/lib.rs", b"c");
}

fn reference(parts: &[(&str, &[u8])]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"rdy-source-build-v1\0");
    for (p, c) in parts {
        h.update(&(p.len() as u64).to_le_bytes());
        h.update(p.as_bytes());
        h.update(&(c.len() as u64).to_le_bytes());
        h.update(c);
    }
    h.finalize().to_hex().as_str()[..16].to_string()
}

fn reason(r: Result<String, BuildIdError>) -> String {
    r.unwrap_err().to_string()
}

fn with_limits(root: &Path, l: &Limits) -> Result<String, BuildIdError> {
    source_build_inputs(root, l, &mut |_, _| {}).map(|o| o.id)
}

#[test]
fn mh2_same_sources_in_other_roots_have_one_id() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    tiny(a.path());
    put(b.path(), "crates/x/src/lib.rs", b"c");
    put(b.path(), "Cargo.lock", b"b");
    put(b.path(), "Cargo.toml", b"a");
    let id = source_build_id(a.path()).unwrap();
    assert_eq!(id.len(), 16);
    assert!(
        id.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    );
    assert_eq!(source_build_id(b.path()).unwrap(), id);
    assert_eq!(
        id,
        reference(&[
            ("Cargo.lock", b"b"),
            ("Cargo.toml", b"a"),
            ("crates/x/src/lib.rs", b"c")
        ])
    );
    assert_eq!(id, PINNED);
}

const PINNED: &str = "5fce2e01e9effb3e";

#[test]
fn mh2_every_admitted_input_changes_the_id() {
    let d = tempfile::tempdir().unwrap();
    tiny(d.path());
    put(d.path(), "crates/x/assets/a.css", b"asset");
    let base = source_build_id(d.path()).unwrap();
    for rel in [
        "Cargo.toml",
        "Cargo.lock",
        "crates/x/src/lib.rs",
        "crates/x/assets/a.css",
    ] {
        let old = fs::read(d.path().join(rel)).unwrap();
        put(d.path(), rel, b"changed");
        assert_ne!(source_build_id(d.path()).unwrap(), base, "{rel}");
        put(d.path(), rel, &old);
        assert_eq!(source_build_id(d.path()).unwrap(), base, "{rel}");
    }
    put(d.path(), "outside.txt", b"x");
    put(d.path(), "target/y", b"x");
    assert_eq!(source_build_id(d.path()).unwrap(), base);
}

#[test]
fn mh2_framed_paths_cannot_alias_contents() {
    let id = |files: &[(&str, &[u8])]| {
        let d = tempfile::tempdir().unwrap();
        put(d.path(), "Cargo.toml", b"");
        put(d.path(), "Cargo.lock", b"");
        fs::create_dir_all(d.path().join("crates")).unwrap();
        for (p, c) in files {
            put(d.path(), p, c);
        }
        source_build_id(d.path()).unwrap()
    };
    let one = id(&[("crates/ab", b"c")]);
    let split = id(&[("crates/a", b"bc")]);
    let renamed = id(&[("crates/abc", b"")]);
    let empty = id(&[("crates/ab", b"")]);
    let none = id(&[]);
    let all = [&one, &split, &renamed, &empty, &none];
    for (i, x) in all.iter().enumerate() {
        for y in &all[i + 1..] {
            assert_ne!(x, y);
        }
    }
    let d = tempfile::tempdir().unwrap();
    tiny(d.path());
    let before = source_build_id(d.path()).unwrap();
    fs::create_dir_all(d.path().join("crates/empty/deeper")).unwrap();
    assert_eq!(source_build_id(d.path()).unwrap(), before);
}

#[test]
fn mh2_walk_refuses_links_and_bounds_before_allocation() {
    let d = tempfile::tempdir().unwrap();
    tiny(d.path());
    let link = d.path().join("crates/x/src/link");
    // The ordinary account that runs the Windows tests holds the symbolic-link privilege.
    rdy_store::pio::symlink("lib.rs", &link).unwrap();
    assert_eq!(
        reason(source_build_id(d.path())),
        "input is not a regular file: crates/x/src/link"
    );
    fs::remove_file(&link).unwrap();
    // The name has no UTF-8 form: a stray byte on Unix, an unpaired surrogate on Windows.
    let bad = rdy_store::pio::not_unicode("bad");
    // Some file systems refuse such names; the check applies where they exist.
    if fs::write(d.path().join("crates/x").join(&bad), b"z").is_ok() {
        assert_eq!(reason(source_build_id(d.path())), "input path is not UTF-8");
        fs::remove_file(d.path().join("crates/x").join(&bad)).unwrap();
    }
    // Entries: 2 root files + crates/x + crates/x/src + lib.rs + crates = 5 entries below crates and root.
    let base = source_build_id(d.path()).unwrap();
    let at = |files, entries, name_bytes, content_bytes| Limits {
        files,
        entries,
        name_bytes,
        content_bytes,
    };
    // Files: 3 in total.
    assert_eq!(with_limits(d.path(), &at(3, 100, 4096, 100)).unwrap(), base);
    assert_eq!(
        reason(with_limits(d.path(), &at(2, 100, 4096, 100))),
        "more than 2 input files"
    );
    // Entries: 3 files + dirs x and src (crates itself is the walk root).
    assert_eq!(with_limits(d.path(), &at(3, 5, 4096, 100)).unwrap(), base);
    assert_eq!(
        reason(with_limits(d.path(), &at(3, 4, 4096, 100))),
        "more than 4 input entries"
    );
    // Name: "crates/x/src/lib.rs" is 19 bytes.
    assert_eq!(with_limits(d.path(), &at(3, 5, 19, 100)).unwrap(), base);
    assert_eq!(
        reason(with_limits(d.path(), &at(3, 5, 18, 100))),
        "input path exceeds 18 bytes"
    );
    // Content: 3 bytes in total.
    assert_eq!(with_limits(d.path(), &at(3, 5, 4096, 3)).unwrap(), base);
    assert_eq!(
        reason(with_limits(d.path(), &at(3, 5, 4096, 2))),
        "input contents exceed 2 bytes"
    );
    // A stream that grows or shrinks after the walk fails the build.
    for phase in [Phase::BeforeRead, Phase::AfterRead] {
        let r = source_build_inputs(d.path(), &LIMITS, &mut |p, ph| {
            if ph == phase && p.ends_with("lib.rs") {
                fs::write(p, b"cc").unwrap();
            }
        });
        assert_eq!(
            r.unwrap_err().to_string(),
            "input changed while being read: crates/x/src/lib.rs"
        );
        put(d.path(), "crates/x/src/lib.rs", b"c");
    }
    // A missing crates directory is a read failure with a relative path.
    let e = tempfile::tempdir().unwrap();
    put(e.path(), "Cargo.toml", b"a");
    put(e.path(), "Cargo.lock", b"b");
    assert_eq!(
        reason(source_build_id(e.path())),
        "cannot read input: crates"
    );
    assert_eq!(LIMITS.files, 100_000);
    assert_eq!(LIMITS.entries, 100_000);
    assert_eq!(LIMITS.name_bytes, 4_096);
    assert_eq!(LIMITS.content_bytes, 1_073_741_824);
}

#[test]
fn mh2_build_emits_complete_rerun_inputs() {
    let d = tempfile::tempdir().unwrap();
    tiny(d.path());
    put(d.path(), "crates/x/deep/more/f.txt", b"f");
    fs::create_dir_all(d.path().join("crates/empty")).unwrap();
    let out = source_build_inputs(d.path(), &LIMITS, &mut |_, _| {}).unwrap();
    let root = d.path();
    for want in [
        "Cargo.toml",
        "Cargo.lock",
        "crates",
        "crates/x",
        "crates/x/src",
        "crates/x/src/lib.rs",
        "crates/x/deep/more",
        "crates/x/deep/more/f.txt",
        "crates/empty",
    ] {
        assert!(out.rerun.contains(&root.join(want)), "{want}");
    }
    assert!(!out.rerun.contains(&root.to_path_buf()));
    // The directories are watched, so an addition or a deletion reruns the script.
    // Absolute paths are for Cargo only; the id does not depend on the root.
    let other = tempfile::tempdir().unwrap();
    tiny(other.path());
    put(other.path(), "crates/x/deep/more/f.txt", b"f");
    assert_eq!(out.id, source_build_id(other.path()).unwrap());
}

#[test]
fn agi1_guide_change_moves_the_build_id() {
    let d = tempfile::tempdir().unwrap();
    tiny(d.path());
    let without = source_build_id(d.path()).unwrap();
    put(d.path(), "release/agent-guide.json", b"guide");
    put(d.path(), "release/agent-guide.sha256", b"digest");
    let base = source_build_id(d.path()).unwrap();
    assert_ne!(base, without);
    assert_eq!(
        base,
        reference(&[
            ("Cargo.lock", b"b"),
            ("Cargo.toml", b"a"),
            ("crates/x/src/lib.rs", b"c"),
            ("release/agent-guide.json", b"guide"),
            ("release/agent-guide.sha256", b"digest"),
        ])
    );
    for rel in ["release/agent-guide.json", "release/agent-guide.sha256"] {
        let old = fs::read(d.path().join(rel)).unwrap();
        put(d.path(), rel, b"changed");
        assert_ne!(source_build_id(d.path()).unwrap(), base, "{rel}");
        put(d.path(), rel, &old);
        assert_eq!(source_build_id(d.path()).unwrap(), base, "{rel}");
    }
    let rerun = source_build_inputs(d.path(), &LIMITS, &mut |_, _| {})
        .unwrap()
        .rerun;
    for rel in ["release/agent-guide.json", "release/agent-guide.sha256"] {
        assert!(rerun.contains(&d.path().join(rel)), "{rel}");
    }
    // Only the two guide files of that folder count: the rest of a release changes no binary.
    put(d.path(), "release/notes.md", b"notes");
    assert_eq!(source_build_id(d.path()).unwrap(), base);
    // Anything but a file in their place refuses, as for every other input.
    fs::remove_file(d.path().join("release/agent-guide.sha256")).unwrap();
    fs::create_dir(d.path().join("release/agent-guide.sha256")).unwrap();
    assert_eq!(
        reason(source_build_id(d.path())),
        "input is not a regular file: release/agent-guide.sha256"
    );
}
