//! wx28b: load, dump and the io area on Windows. A load keeps its start lock against a racing
//! open, and a failed check removes the store while that lock is held. A hostile dump or target
//! is an error and leaves the target as it was, never a crash. The dumps and the released
//! fixtures hold the same bytes and hashes on Windows and Unix. Every released fixture, single
//! file and chain, loads, passes fsck and reindexes to the state it was loaded in. The expected
//! hashes are the file `fixtures/windows/dump-hashes.sha256`; `RDY_QA_BLESS=1` rewrites it, and
//! a Windows run never does. A dump's pin is of its bytes with the writer's version text fixed
//! (`version_free`), so a version bump does not move it.
use super::io_load::{dumped, failed_check_undoes_while_locked, race, tree};
use rdy_engine::load::{load, load_chain};
use rdy_engine::{Rdy, RdyError};
use rdy_qa::reindex_check::{compare, derived};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

fn dump_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/dump")
}

fn pin_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/windows/dump-hashes.sha256")
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A released fixture: one file, or a full dump and two increments, for a project or a holder.
struct Fixture {
    /// The name without `.rdydump`, or the chain's prefix.
    name: String,
    holder: bool,
    /// One file, or the chain in order.
    files: Vec<PathBuf>,
}

/// Every released fixture under `fixtures/dump`, sorted by name.
fn fixtures() -> Vec<Fixture> {
    let mut names: Vec<String> = std::fs::read_dir(dump_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".rdydump"))
        .collect();
    names.sort();
    let mut out = vec![];
    for n in &names {
        let stem = n.trim_end_matches(".rdydump");
        let holder = stem.contains("holder");
        if let Some(prefix) = stem.strip_suffix("-chain-full") {
            let files = ["full", "1", "2"]
                .iter()
                .map(|p| dump_dir().join(format!("{prefix}-chain-{p}.rdydump")))
                .collect();
            out.push(Fixture {
                name: format!("{prefix}-chain"),
                holder,
                files,
            });
        } else if !(stem.ends_with("-chain-1") || stem.ends_with("-chain-2")) {
            out.push(Fixture {
                name: stem.to_string(),
                holder,
                files: vec![dump_dir().join(n)],
            });
        }
    }
    out
}

fn load_fixture(f: &Fixture, to: &Path) -> Result<rdy_engine::LoadSummary, RdyError> {
    if f.files.len() == 1 {
        load(&f.files[0], to)
    } else {
        load_chain(&f.files, to, None)
    }
}

fn open(dir: &Path, holder: bool) -> Rdy {
    if holder {
        Rdy::open_holder(dir).unwrap()
    } else {
        Rdy::open(dir).unwrap()
    }
}

/// A native path string for messages, so an assertion names the path the way the platform
/// writes it.
fn code_of(r: Result<rdy_engine::LoadSummary, RdyError>) -> String {
    match r {
        Ok(_) => "ok".to_string(),
        Err(e) => e.code.to_string(),
    }
}

#[test]
fn wx28b_load_races_preserve_start_lock_exclusion() {
    let (src, scratch, file) = dumped();
    let mut raced = false;
    for round in 0..4 {
        let to = scratch.path().join(format!("new{round}"));
        let (out, seen) = race(&file, &to);
        raced |= !seen.is_empty();
        let s = out.unwrap_or_else(|e| panic!("round {round}: {e}; the racer saw {seen:?}"));
        // the only answers a racing open may get: refused for the lock, or the store itself
        assert!(
            seen.iter().all(|c| c == "E_LOCKED" || c == "ok"),
            "round {round}: {seen:?}"
        );
        assert!(s.landings >= 3, "round {round}: {s:?}");
        // the loaded log starts with the source's bytes, segment by segment
        for (path, bytes) in super::io_load::segs(src.path()) {
            let have = std::fs::read(to.join(".rdy/store/segments").join(&path)).ok();
            let want = bytes.unwrap_or_default();
            assert!(
                have.is_some_and(|h| h.starts_with(&want)),
                "round {round}: segment {path:?} lost bytes"
            );
        }
    }
    assert!(raced, "the racer never tried an open while the load ran");
    // the lock a finished load leaves is the ordinary one: one holder, then E_LOCKED for a
    // second open, and a load into the held project is refused and changes nothing
    let to = scratch.path().join("new0");
    let held = Rdy::open(&to).unwrap();
    let e = Rdy::open(&to).err().expect("a second open is refused");
    assert_eq!(e.code, "E_LOCKED", "{e}");
    let before = tree(&to);
    let e = load(&file, &to).unwrap_err();
    assert_eq!(e.code, "E_ALREADY_EXISTS", "{e}");
    assert_eq!(tree(&to), before, "a refused load touches nothing");
    held.close().unwrap();
    Rdy::open(&to).unwrap().close().unwrap();
}

#[test]
fn wx28b_failed_check_rolls_back_while_locked() {
    // four rounds against an opener: the store is removed only while the load holds it, and the
    // target ends as it began (README.md alone)
    failed_check_undoes_while_locked();
    // a failed check leaves no half-built project to open or to block a later load
    let scratch = tempfile::tempdir().unwrap();
    let (_src, _s, good) = dumped();
    let forged = super::io_load::fsck_failing_dump(scratch.path());
    let to = scratch.path().join("again");
    let e = load(&forged, &to).unwrap_err();
    assert_eq!(e.code, "E_DUMP", "{e}");
    assert!(!to.join(".rdy/store").exists(), "no store is left");
    let before = tree(&to);
    // the same target takes a good load, and the rollback left no lock behind
    let s = load(&good, &to).unwrap_or_else(|e| panic!("after the rollback: {e}; {before:?}"));
    assert!(s.landings >= 3, "{s:?}");
    Rdy::open(&to).unwrap().close().unwrap();
}

#[test]
fn wx28b_hostile_trees_remain_bounded() {
    let (_src, scratch, file) = dumped();
    let bytes = std::fs::read(&file).unwrap();

    // a dump cut anywhere short of its end is refused, and the target is left as it was
    let len = bytes.len();
    let mut cuts: Vec<usize> = vec![0, 1, 2, 3, 4, 7, 8, 16, 31, 32, 33, 64, 127, 128, 255];
    cuts.extend((1..16).map(|i| len * i / 16));
    cuts.push(len - 1);
    for (i, cut) in cuts.into_iter().filter(|c| *c < len).enumerate() {
        let part = scratch.path().join(format!("cut{i}.rdydump"));
        std::fs::write(&part, &bytes[..cut]).unwrap();
        let to = scratch.path().join(format!("cut-target{i}"));
        let r = load(&part, &to);
        let e = r.expect_err(&format!("a dump cut at {cut} of {len} bytes loaded"));
        assert!(
            ["E_DUMP", "E_BAD_INPUT", "E_IO"].contains(&e.code),
            "cut {cut}: {e}"
        );
        assert!(
            e.message.len() < 1024,
            "cut {cut}: {} bytes",
            e.message.len()
        );
        assert!(!to.join(".rdy").exists(), "cut {cut}: a store was left");
    }

    // a flipped byte, a huge length where a length belongs and trailing bytes: an error or a
    // load that passes its own fsck, never a crash, and an error leaves no store
    let mut hostile: Vec<(String, Vec<u8>)> = vec![];
    for i in 1..17 {
        let mut b = bytes.clone();
        let at = len * i / 17;
        b[at] ^= 0xff;
        hostile.push((format!("flip{at}"), b));
    }
    for at in [0usize, 4, 8, 12, 16, 20] {
        let mut b = bytes.clone();
        for x in b.iter_mut().skip(at).take(4) {
            *x = 0xff;
        }
        hostile.push((format!("ff{at}"), b));
    }
    let mut tail = bytes.clone();
    tail.extend_from_slice(&[0u8; 4096]);
    hostile.push(("tail".into(), tail));
    hostile.push(("garbage".into(), vec![0x5a; 1 << 16]));
    hostile.push(("crlf".into(), {
        let mut b = Vec::with_capacity(len * 2);
        for x in &bytes {
            if *x == b'\n' {
                b.push(b'\r');
            }
            b.push(*x);
        }
        b
    }));
    for (what, b) in hostile {
        let bad = scratch.path().join(format!("{what}.rdydump"));
        std::fs::write(&bad, &b).unwrap();
        let to = scratch.path().join(format!("hostile-{what}"));
        match load(&bad, &to) {
            Err(e) => {
                assert!(e.message.len() < 1024, "{what}: {} bytes", e.message.len());
                assert!(!to.join(".rdy/store").exists(), "{what}: a store was left");
            }
            Ok(_) => {
                let r = Rdy::open(&to).unwrap();
                r.fsck().unwrap_or_else(|e| panic!("{what}: {e}"));
                r.close().unwrap();
            }
        }
    }

    // a target that is a file, and a target below a file, are refused and the file is kept
    let plain = scratch.path().join("plain-file");
    std::fs::write(&plain, "kept").unwrap();
    for to in [plain.clone(), plain.join("below")] {
        let e = load(&file, &to).expect_err("a load into a file");
        assert!(e.message.len() < 1024, "{e}");
        assert_eq!(std::fs::read(&plain).unwrap(), b"kept");
    }

    // a target path past the classic path limit loads, or is refused with a bounded error and
    // nothing half built
    let mut deep = scratch.path().to_path_buf();
    while deep.as_os_str().len() < 300 {
        deep.push("a-directory-name-of-some-length");
    }
    deep.push("project");
    match load(&file, &deep) {
        Ok(_) => Rdy::open(&deep).unwrap().close().unwrap(),
        Err(e) => {
            assert!(e.message.len() < 1024, "{e}");
            assert!(!deep.join(".rdy/store").exists(), "a half-built store");
        }
    }

    // names Windows refuses: the load is an error, and nothing is written for the name
    #[cfg(windows)]
    for name in rdy_qa::platform::native_invalid_names() {
        let to = scratch.path().join(&name);
        let e = load(&file, &to).expect_err("a name the system refuses");
        assert!(e.message.len() < 1024, "{name:?}: {e}");
        assert!(std::fs::symlink_metadata(&to).is_err(), "{name:?}");
    }
    // a name that is not Unicode loads, or is refused whole
    #[cfg(windows)]
    {
        let to = scratch.path().join(rdy_qa::platform::not_unicode("odd-"));
        match load(&file, &to) {
            Ok(_) => Rdy::open(&to).unwrap().close().unwrap(),
            Err(e) => {
                assert!(e.message.len() < 1024, "{e}");
                assert!(!to.join(".rdy/store").exists());
            }
        }
    }
}

/// Every pinned row of the file `dump-hashes.sha256`: `<sha256>  <key>`.
fn pinned_rows() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(pin_file())
        .unwrap_or_else(|e| panic!("{}: {e}; bless with RDY_QA_BLESS=1", pin_file().display()));
    text.replace("\r\n", "\n")
        .lines()
        .map(|l| {
            let (h, k) = l.split_once("  ").unwrap();
            (h.to_string(), k.to_string())
        })
        .collect()
}

/// The `rdy` text a pinned redump carries in place of the writing binary's version.
const PINNED_RDY: &str = "0.0.0";

/// `dump` (dump format 4, as `rdy_store::dump` documents it) as the same writer would give it
/// with the header's `rdy` text set to [`PINNED_RDY`].
///
/// The header names the version of the rdy that wrote the dump (O17), so a pin of the raw bytes
/// moves with every version bump. That text is for people only, never checked, and it is the
/// only input of a dump that changes with the version; but three other places depend on it:
/// the `HEADER` record's length and hash, and the `TRAILER`'s digest over every record's
/// `kind || len || hash` (and with it the trailer record's hash). This rewrites exactly those:
/// the text, the header record, and the trailer's digest and hash, from the records' own stored
/// hashes. Every other byte is copied, so a Windows/Unix difference anywhere else in the dump
/// still moves the pin. It checks the walk against the dump: the trailer's digest must be the
/// one the records give before the rewrite, and the trailer must end the file.
fn version_free(dump: &[u8]) -> Vec<u8> {
    use rdy_model::Hash;
    use rdy_store::dump::{DUMP_FORMAT, HEADER_FIXED, TRAILER_LEN, rec};
    const PRELUDE: usize = 8 + 4;
    const FRAME: usize = 4 + 1 + 32;
    let u32_at = |b: &[u8], at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap());
    // one record at `at`: its kind, stored hash and payload
    let record = |at: usize| {
        let len = u32_at(dump, at) as usize;
        let hash: [u8; 32] = dump[at + 5..at + FRAME].try_into().unwrap();
        (dump[at + 4], hash, &dump[at + FRAME..at + FRAME + len])
    };
    let put = |out: &mut Vec<u8>, kind: u8, payload: &[u8]| {
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.push(kind);
        out.extend_from_slice(&Hash::of(payload).0);
        out.extend_from_slice(payload);
    };
    let link = |chain: &mut Vec<u8>, kind: u8, len: usize, hash: &[u8; 32]| {
        chain.push(kind);
        chain.extend_from_slice(&(len as u64).to_le_bytes());
        chain.extend_from_slice(hash);
    };
    assert_eq!(&dump[..8], b"RDYDUMP\n");
    assert_eq!(
        u32_at(dump, 8),
        DUMP_FORMAT,
        "the dump format the walk reads"
    );
    // the header: its manifest's length is at 1, and the `rdy` text ends the payload
    let (kind, _, header) = record(PRELUDE);
    assert_eq!(kind, rec::HEADER);
    let rdy_at = HEADER_FIXED - 2 + u32_at(header, 1) as usize;
    let rdy_len = u16::from_le_bytes(header[rdy_at..rdy_at + 2].try_into().unwrap()) as usize;
    assert_eq!(
        rdy_at + 2 + rdy_len,
        header.len(),
        "the rdy text ends the header"
    );
    let mut new_header = header[..rdy_at].to_vec();
    new_header.extend_from_slice(&(PINNED_RDY.len() as u16).to_le_bytes());
    new_header.extend_from_slice(PINNED_RDY.as_bytes());
    let mut out = dump[..PRELUDE].to_vec();
    put(&mut out, rec::HEADER, &new_header);
    let (mut old_chain, mut new_chain) = (vec![], vec![]);
    link(
        &mut old_chain,
        rec::HEADER,
        header.len(),
        &Hash::of(header).0,
    );
    link(
        &mut new_chain,
        rec::HEADER,
        new_header.len(),
        &Hash::of(&new_header).0,
    );
    // the records after the header (files, batches, their frames), copied as they are
    let mut at = PRELUDE + FRAME + header.len();
    loop {
        let (kind, hash, payload) = record(at);
        let next = at + FRAME + payload.len();
        if kind == rec::TRAILER {
            assert_eq!(payload.len(), TRAILER_LEN);
            assert_eq!(next, dump.len(), "the trailer ends the dump");
            let digest = TRAILER_LEN - 32;
            assert_eq!(
                payload[digest..],
                Hash::of(&old_chain).0,
                "the walk gives the trailer's digest"
            );
            let mut trailer = payload[..digest].to_vec();
            trailer.extend_from_slice(&Hash::of(&new_chain).0);
            put(&mut out, rec::TRAILER, &trailer);
            return out;
        }
        link(&mut old_chain, kind, payload.len(), &hash);
        link(&mut new_chain, kind, payload.len(), &hash);
        out.extend_from_slice(&dump[at..next]);
        at = next;
    }
}

#[test]
fn wx28b_windows_and_unix_dumps_have_identical_content_hashes() {
    let mut rows: Vec<(String, String)> = vec![];
    // the bytes of every released file are the bytes the commit holds: a checkout converted
    // nothing
    let mut files: Vec<_> = std::fs::read_dir(dump_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    for p in &files {
        let name = p.file_name().unwrap().to_str().unwrap();
        rows.push((sha(&std::fs::read(p).unwrap()), format!("fixture/{name}")));
    }
    // A dump taken after a load: two loads of one fixture give the same dump bytes, or differ
    // only because the load made a repo id (the pin is then `-`, and the dump of that dump is
    // checked instead). A dump of a loaded dump is always those same bytes.
    for f in fixtures().into_iter().filter(|f| !f.holder) {
        let scratch = tempfile::tempdir().unwrap();
        let mut dumps = vec![];
        for round in 0..2 {
            let to = scratch.path().join(format!("p{round}"));
            load_fixture(&f, &to).unwrap_or_else(|e| panic!("{}: {e}", f.name));
            let out = scratch.path().join(format!("out{round}.rdydump"));
            let r = Rdy::open(&to).unwrap();
            r.read().dump(&out).unwrap();
            r.close().unwrap();
            dumps.push(std::fs::read(&out).unwrap());
        }
        let again = scratch.path().join("again");
        load(&scratch.path().join("out0.rdydump"), &again).unwrap();
        let out = scratch.path().join("again.rdydump");
        let r = Rdy::open(&again).unwrap();
        r.read().dump(&out).unwrap();
        r.close().unwrap();
        assert!(
            std::fs::read(&out).unwrap() == dumps[0],
            "{}: a dump of a loaded dump is the same bytes",
            f.name
        );
        // the pin holds for every rdy version: it is of the dump with the version text fixed
        // (`version_free`), and it still sees any other byte, as a flipped byte of the last
        // record before the trailer shows
        let pinned = version_free(&dumps[0]);
        let mut flipped = dumps[0].clone();
        let last = flipped.len() - (4 + 1 + 32 + rdy_store::dump::TRAILER_LEN) - 1;
        flipped[last] ^= 1;
        assert_ne!(
            version_free(&flipped),
            pinned,
            "{}: a changed byte moves the pin",
            f.name
        );
        let pin = if dumps[0] == dumps[1] {
            sha(&pinned)
        } else {
            "-".to_string()
        };
        rows.push((pin, format!("redump/{}", f.name)));
    }
    let got: String = rows.iter().map(|(h, k)| format!("{h}  {k}\n")).collect();
    if std::env::var_os("RDY_QA_BLESS").is_some() && !cfg!(windows) {
        std::fs::write(pin_file(), &got).unwrap();
    }
    let want = pinned_rows();
    let want: String = want.iter().map(|(h, k)| format!("{h}  {k}\n")).collect();
    assert_eq!(got, want, "the pinned hashes");
}

#[test]
fn wx28b_chain_load_and_reindex_keep_every_released_fixture() {
    let all = fixtures();
    assert!(
        all.iter().any(|f| f.files.len() == 3) && all.iter().any(|f| f.holder),
        "chains and holders are among the fixtures"
    );
    for f in &all {
        let scratch = tempfile::tempdir().unwrap();
        let (a, b) = (scratch.path().join("a"), scratch.path().join("b"));
        load_fixture(f, &a).unwrap_or_else(|e| panic!("{}: {e}", f.name));
        load_fixture(f, &b).unwrap();
        // every file of the fixture is still the released bytes after the loads
        for p in &f.files {
            let want = pinned_rows()
                .into_iter()
                .find(|(_, k)| {
                    *k == format!("fixture/{}", p.file_name().unwrap().to_str().unwrap())
                })
                .unwrap_or_else(|| panic!("{}: no pin", p.display()))
                .0;
            assert_eq!(sha(&std::fs::read(p).unwrap()), want, "{}", p.display());
        }
        // the loaded state passes fsck, and a reindex reproduces it
        let mut ra = open(&a, f.holder);
        ra.fsck().unwrap_or_else(|e| panic!("{}: {e}", f.name));
        if f.holder {
            // a holder answers derived reads only for a tag or a landing, so it is reindexed
            // and checked again after a reopen
            ra.reindex().unwrap_or_else(|e| panic!("{}: {e}", f.name));
            ra.close().unwrap();
            let rb = open(&a, true);
            rb.fsck().unwrap_or_else(|e| panic!("{}: {e}", f.name));
            rb.close().unwrap();
        } else {
            let before = derived(&ra).unwrap();
            ra.reindex().unwrap_or_else(|e| panic!("{}: {e}", f.name));
            let v = compare(&before, &derived(&ra).unwrap());
            assert!(
                v.is_empty(),
                "{}: a reindex changed the state: {v:#?}",
                f.name
            );
            ra.close().unwrap();
            // and the reopened project agrees with a second, independent load
            let ra = open(&a, false);
            let rb = open(&b, false);
            let v = compare(&derived(&ra).unwrap(), &derived(&rb).unwrap());
            assert!(v.is_empty(), "{}: two loads differ: {v:#?}", f.name);
            ra.close().unwrap();
            rb.close().unwrap();
        }
        assert_eq!(
            code_of(load_fixture(f, &a)),
            "E_ALREADY_EXISTS",
            "{}: a second load into the project",
            f.name
        );
    }
}
