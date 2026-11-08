// Unix only: these fixtures need a shell with a descriptor limit, named pipes, sockets and
// permission modes. The portable notice tests live beside them in the parent module (Task 35b).
//! The Unix-only notice and source fixtures of `release_notices`.

use super::*;
use crate::platform::shell_script;
use crate::platform::test_support::{fifo, unix_socket};

/// Sockets and named pipes among a package's license files refuse the package.
#[test]
fn plx7_notices_refuse_special_license_files_on_unix() {
    type Spoil = fn(&Path);
    let spoils: [(&str, Spoil); 2] = [
        ("a FIFO", |d| fifo(&d.join("LICENSE.fifo"))),
        ("a socket", |d| drop(unix_socket(&d.join("NOTICE.sock")))),
    ];
    for (i, (what, spoil)) in spoils.into_iter().enumerate() {
        // under the short root, so a socket path fits
        let pkg = tempfile::Builder::new()
            .prefix("plx7")
            .tempdir_in(crate::platform::short_tmp_root())
            .expect("tempdir");
        let p = package_dir(pkg.path(), &format!("p{i}"), &[("LICENSE", b"ok\n")]);
        spoil(&p);
        assert_eq!(
            notices_of(&[krate("spoiled", "1.0.0", "MIT", &p)], &[]),
            Err(refused(Kind::Notices, "spoiled 1.0.0")),
            "{what}"
        );
    }
}

/// A source that is a named pipe fails before OUT; staged copies get mode 0644.
#[test]
fn plx7_sources_refuse_a_fifo_and_stage_mode_0644_on_unix() {
    let d = tempfile::tempdir().expect("tempdir");
    let (sources, _) = cached(d.path());
    fs::remove_file(&sources[1].cache).expect("rm");
    fifo(&sources[1].cache);
    let out = d.path().join("out");
    assert_eq!(
        stage_sources(&sources, &out, &|_, _| {}),
        Err(refused(Kind::Source, "beta-2.0.0.crate"))
    );
    assert!(fs::symlink_metadata(&out).is_err(), "no OUT");

    let d = tempfile::tempdir().expect("tempdir");
    let (sources, _) = cached(d.path());
    let out = d.path().join("out");
    stage_sources(&sources, &out, &|_, _| {}).expect("stage");
    for s in &sources {
        assert_eq!(
            platform::mode_of(&out.join(&s.filename)).expect("mode"),
            0o644
        );
    }
}

/// Set in the child that [`plx7_source_staging_owns_its_outputs_when_descriptors_run_out`]
/// re-executes under a 128-descriptor limit.
const DESCRIPTOR_CHILD: &str = "RDY_PLX7_DESCRIPTOR_CHILD";

/// Source staging owns each output from the moment it exists, and its cleanup still has
/// the descriptor it needs. The staging runs in a re-executed child under a 128-descriptor
/// limit, where filling every free descriptor touches no other test; after the first
/// source's first chunk, `during` takes every free descriptor (or all but one).
#[test]
fn plx7_source_staging_owns_its_outputs_when_descriptors_run_out() {
    let name = crate::test_name!("plx7_source_staging_owns_its_outputs_when_descriptors_run_out");
    if std::env::var_os(DESCRIPTOR_CHILD).is_none() {
        let mut child = shell_script("ulimit -n 128 && exec \"$0\" \"$@\"");
        child
            .arg(std::env::current_exe().expect("current exe"))
            .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env(DESCRIPTOR_CHILD, "1");
        let out = crate::fixtures::output_within(&mut child, std::time::Duration::from_secs(600))
            .expect("run the child");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && text.contains("test result: ok. 1 passed"),
            "the child ran this one test and it passed: {out:?}"
        );
        return;
    }
    let base = tempfile::tempfile().expect("a handle to copy");
    let held: std::cell::RefCell<Vec<File>> = std::cell::RefCell::new(Vec::new());
    // Copies `base` until no descriptor is free, then gives `free` of them back.
    let exhaust = |free: usize| {
        let mut held = held.borrow_mut();
        while held.len() < 4096 {
            match base.try_clone() {
                Ok(f) => held.push(f),
                Err(_) => break,
            }
        }
        assert!(held.len() < 4096, "the 128-descriptor limit holds");
        for _ in 0..free {
            held.pop();
        }
    };

    // No descriptor free: the second output cannot be made. The run fails, and its
    // cleanup still removes the first copy and OUT.
    let d = tempfile::tempdir().expect("tempdir");
    let (sources, bytes) = cached(d.path());
    let out = d.path().join("out");
    let filled = Cell::new(false);
    let during = |name: &str, _: u64| {
        if name == "alpha-1.0.0.crate" && !filled.replace(true) {
            exhaust(0);
        }
    };
    let got = stage_sources(&sources, &out, &during);
    held.borrow_mut().clear();
    assert!(filled.get(), "the descriptors ran out mid-run");
    assert_eq!(got, Err(refused(Kind::FileOperation, "beta-2.0.0.crate")));
    assert!(
        fs::symlink_metadata(&out).is_err(),
        "no owned output remains"
    );
    for (s, b) in sources.iter().zip(&bytes) {
        assert_eq!(
            &fs::read(&s.cache).expect("cache"),
            b,
            "the cache is unchanged"
        );
    }
    // A retry with descriptors to spare finds no OUT in its way.
    stage_sources(&sources, &out, &|_, _| {}).expect("the retry stages");
    assert_eq!(listed(&out), ["alpha-1.0.0.crate", "beta-2.0.0.crate"]);

    // One descriptor free: the second output takes it and is copied through that one
    // handle; the run succeeds with both exact copies.
    let d = tempfile::tempdir().expect("tempdir");
    let (sources, bytes) = cached(d.path());
    let out = d.path().join("out");
    let filled = Cell::new(false);
    let during = |name: &str, _: u64| {
        if name == "alpha-1.0.0.crate" && !filled.replace(true) {
            exhaust(1);
        }
    };
    let got = stage_sources(&sources, &out, &during);
    held.borrow_mut().clear();
    assert!(filled.get(), "the descriptors ran out mid-run");
    assert_eq!(got, Ok(()));
    assert_eq!(listed(&out), ["alpha-1.0.0.crate", "beta-2.0.0.crate"]);
    for (s, b) in sources.iter().zip(&bytes) {
        let copy = out.join(&s.filename);
        assert_eq!(&fs::read(&copy).expect("copy"), b);
        assert_eq!(platform::mode_of(&copy).expect("mode"), 0o644);
    }
}
