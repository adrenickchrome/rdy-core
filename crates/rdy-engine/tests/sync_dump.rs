//! scc9 (D-7CDEQ, D-3RSCW, D-47HVF): the fetch of a hub dump to a new file. The body streams
//! into `<file>.partial-<pid>`, is checked as `rdy load` checks a dump, and is linked in place as
//! `rdy dump` links a file, with `rdy pull --dump` in the texts. The fetch writes no store, ref or
//! sync state and takes no sync lock. Every test runs the sync service over the fake hub of
//! `support/sync_env.rs`, with a real project and a real sign-in file behind it.
mod common;
#[path = "support/sync_env.rs"]
mod sync_env;

use rdy_engine::sync_dump::{DUMP_FETCH_NEXT, DumpFetched};
use rdy_engine::sync_state::RemoteGuard;
use rdy_engine::{E_ALREADY_EXISTS, E_BAD_INPUT, Engine, Rdy, RdyError};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const ID: &str = "dmp_01J9";

/// A loopback install origin on a port made at run time; nothing connects to it.
fn install() -> &'static str {
    static ORIGIN: OnceLock<String> = OnceLock::new();
    ORIGIN.get_or_init(|| {
        let held = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        format!("https://127.0.0.1:{}", held.local_addr().unwrap().port())
    })
}

fn env() -> sync_env::Env {
    sync_env::env_with(install(), "# person settings\n", Some("shared"), 2)
}

/// A fake hub repo of `e`'s project that serves `bytes` as dump [`ID`].
fn hub_with(e: &sync_env::Env, bytes: Vec<u8>) -> sync_env::Hub {
    let hub = sync_env::Hub::new().with_state(|x| x.repo_id = Some(sync_env::repo_of(e)));
    hub.dumps.lock().unwrap().insert(ID.into(), bytes);
    hub
}

fn fetch(e: &sync_env::Env, hub: &sync_env::Hub, out: &Path) -> Result<DumpFetched, RdyError> {
    sync_env::Sync::new(e, hub)
        .service()
        .fetch_dump(&e.engine, &sync_env::agent(), None, ID, out)
}

#[track_caller]
fn err<T: std::fmt::Debug>(r: Result<T, RdyError>) -> RdyError {
    match r {
        Err(e) => e,
        Ok(v) => panic!("expected an error, got {v:?}"),
    }
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// A holder dump of `e`'s repo (a hub repo's dump is a holder's, D-J3YA0) in `dir`.
fn holder_dump(e: &sync_env::Env, dir: &Path) -> PathBuf {
    let at = dir.join("holder");
    Rdy::create_holder(&at, sync_env::repo_of(e))
        .unwrap()
        .close()
        .unwrap();
    let h = Rdy::open_holder(&at).unwrap();
    let out = dir.join("holder.rdydump");
    h.read().dump(&out).unwrap();
    h.close().unwrap();
    out
}

/// scc9: the fetched bytes are checked before the link: a project dump and a holder dump each
/// place whole, and the reply names the file as `rdy dump` reports it (length, dump format,
/// kind, repo id, trailer digest) with the next step. A cut body fails the check with the load's
/// code, and no file or partial file is left.
#[test]
fn scc9_dump_stream_checks_before_atomic_place() {
    let e = env();
    let src = sync_env::private_dir();
    let project = src.path().join("project.rdydump");
    let summary = e.engine.read().dump(&project).unwrap();
    let holder = holder_dump(&e, src.path());
    for file in [&project, &holder] {
        let bytes = fs::read(file).unwrap();
        let hub = hub_with(&e, bytes.clone());
        let dir = sync_env::private_dir();
        let out = dir.path().join("fetched.rdydump");
        let got = fetch(&e, &hub, &out).unwrap();
        assert_eq!(fs::read(&out).unwrap(), bytes);
        assert_eq!(names(dir.path()), ["fetched.rdydump"]);
        assert_eq!(got.out, out.display().to_string());
        assert_eq!(got.bytes, bytes.len() as u64);
        assert_eq!(got.repo_id, sync_env::repo_of(&e));
        assert_eq!(got.next, DUMP_FETCH_NEXT);
        assert_eq!(
            DUMP_FETCH_NEXT,
            "rdy push --restore <file>, or rdy load <file> <dir> for a copy"
        );
        if file == &project {
            assert_eq!(
                (got.dump_format, got.kind, got.digest),
                (summary.dump_format, summary.kind, summary.digest)
            );
        }
        let v = serde_json::to_value(&got).unwrap();
        for m in [
            "out",
            "bytes",
            "dump_format",
            "kind",
            "repo_id",
            "digest",
            "next",
        ] {
            assert!(!v[m].is_null(), "{m}");
        }
        assert_eq!(v["digest"], summary_digest(file));
    }
    // a cut body: the load's damage refusal, nothing placed
    let bytes = fs::read(&project).unwrap();
    let hub = hub_with(&e, bytes[..bytes.len() / 2].to_vec());
    let dir = sync_env::private_dir();
    let out = dir.path().join("cut.rdydump");
    assert_eq!(err(fetch(&e, &hub, &out)).code, "E_DUMP");
    assert!(names(dir.path()).is_empty());
}

/// The trailer digest of the dump `file`, as the load reads it, in hex.
fn summary_digest(file: &Path) -> String {
    let mut f = fs::File::open(file).unwrap();
    let end = rdy_engine::dump::chain_end_of(file, &mut f).unwrap();
    end.digest.to_string()
}

/// scc9: a damaged dump gets the code and text that `rdy load` gives the same bytes at the same
/// path; a dump of another repo than the state step's is `E_REPO_MISMATCH`.
#[test]
fn scc9_dump_check_refusals_match_load() {
    let e = env();
    let src = sync_env::private_dir();
    let good = src.path().join("good.rdydump");
    e.engine.read().dump(&good).unwrap();
    let bytes = fs::read(&good).unwrap();
    // a changed byte of the trailer's digest
    let mut damaged = bytes.clone();
    let at = damaged.len() - 1;
    damaged[at] ^= 0xff;
    let mut no_trailer = bytes.clone();
    no_trailer.truncate(bytes.len() - 8);
    for (case, body) in [
        ("damaged", damaged),
        ("no trailer", no_trailer),
        ("not a dump", b"hello".to_vec()),
    ] {
        let dir = sync_env::private_dir();
        let out = dir.path().join("fetched.rdydump");
        fs::write(&out, &body).unwrap();
        let target = dir.path().join("loaded");
        let want = err(rdy_engine::load::load(&out, &target));
        fs::remove_file(&out).unwrap();
        let got = err(fetch(&e, &hub_with(&e, body), &out));
        assert_eq!(
            (&got.code, &got.message, &got.fix),
            (&want.code, &want.message, &want.fix),
            "{case}"
        );
        assert!(names(dir.path()).is_empty(), "{case}");
    }
    // the state step names another repo than the dump's: E_REPO_MISMATCH, nothing placed
    let other = env();
    let hub = sync_env::Hub::new().with_state(|x| x.repo_id = Some(sync_env::repo_of(&other)));
    hub.dumps.lock().unwrap().insert(ID.into(), bytes);
    let dir = sync_env::private_dir();
    let out = dir.path().join("fetched.rdydump");
    let r = sync_env::Sync::new(&e, &hub).service().fetch_dump_at(
        &sync_env::agent(),
        &format!("{}/acme/docs", install()),
        ID,
        &out,
    );
    assert_eq!(err(r).code, "E_REPO_MISMATCH");
    assert!(names(dir.path()).is_empty());
}

/// scc9: the placement texts are `rdy dump`'s with `rdy pull --dump` as the command: an existing
/// file, an existing partial file (which is never removed), and a missing directory.
#[test]
fn scc9_dump_place_refusals_keep_dump_texts() {
    let e = env();
    let src = sync_env::private_dir();
    let file = src.path().join("d.rdydump");
    e.engine.read().dump(&file).unwrap();
    let bytes = fs::read(&file).unwrap();
    let hub = hub_with(&e, bytes);
    let dir = sync_env::private_dir();

    // the file exists
    let out = dir.path().join("taken.rdydump");
    fs::write(&out, b"mine").unwrap();
    let got = err(fetch(&e, &hub, &out));
    assert_eq!(
        (got.code, got.message.clone(), got.fix.as_ref()),
        (
            E_ALREADY_EXISTS,
            format!("{} exists", out.display()),
            "pick a new file name; rdy pull --dump never overwrites a file"
        )
    );
    assert_eq!(fs::read(&out).unwrap(), b"mine");

    // another call's partial file: refused, and kept
    let out = dir.path().join("busy.rdydump");
    let partial = dir
        .path()
        .join(format!("busy.rdydump.partial-{}", std::process::id()));
    fs::write(&partial, b"theirs").unwrap();
    let got = err(fetch(&e, &hub, &out));
    assert_eq!(
        (got.code, got.message.clone(), got.fix.as_ref()),
        (
            E_ALREADY_EXISTS,
            format!(
                "{} exists: another pull --dump may be writing it",
                partial.display()
            ),
            "remove it if no pull --dump is running, then retry"
        )
    );
    assert_eq!(fs::read(&partial).unwrap(), b"theirs");
    assert!(!out.exists());

    // the directory does not exist
    let missing = dir.path().join("no-such-dir");
    let got = err(fetch(&e, &hub, &missing.join("x.rdydump")));
    assert_eq!(
        (got.code, got.message.clone(), got.fix.as_ref()),
        (
            E_BAD_INPUT,
            format!("{} does not exist", missing.display()),
            "create the directory first, or pick another path"
        )
    );
}

/// scc9: the fetch writes no store, ref or sync state, and takes no sync lock: it runs while a
/// push or pull of the remote holds the remote's guard, and the store and its sync state are as
/// they were.
#[test]
fn scc9_dump_fetch_writes_no_store_refs_or_sync_state_and_takes_no_lock() {
    let e = env();
    let src = sync_env::private_dir();
    let file = src.path().join("d.rdydump");
    e.engine.read().dump(&file).unwrap();
    let hub = hub_with(&e, fs::read(&file).unwrap());
    let before = e.engine.read().seq();
    let state = |e: &sync_env::Env| {
        (
            sync_env::canon_of(e, "shared"),
            sync_env::pending_of(e, "shared"),
            sync_env::upload_of(e, "shared"),
        )
    };
    let sync_before = state(&e);
    let _held = RemoteGuard::take(&e.project, "shared").unwrap();
    let dir = sync_env::private_dir();
    fetch(&e, &hub, &dir.path().join("f.rdydump")).unwrap();
    assert_eq!(e.engine.read().seq(), before);
    assert_eq!(state(&e), sync_before);
    assert_eq!(hub.steps(), ["discovery", "state", "dump"]);
}
