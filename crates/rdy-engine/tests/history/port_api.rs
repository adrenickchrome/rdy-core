//! hpt4/hpr4/hpc4 local and dump ports (D-G4PZX, D-1QT23, D-08M5W, D-7AT4S): a replay with a
//! source ports landed changes of another project, or of a dump of one, into an open view of the
//! target as new changes. The source opens for reading only and never changes; a dump loads into
//! private staging, which goes before the read returns. Only changes the source's canon carries
//! can be ported. The port is one undoable Include and cites a briefing of the target.
use crate::port_bundle::*;

use rdy_engine::engine::{Engine, Local};
use rdy_engine::redaction::RedactInput;
use rdy_engine::view_api::{ViewRequest, run_view};
use rdy_engine::views::ReplayResult;
use rdy_engine::views::port::{self as local_port, PortSource};
use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::*;
use rdy_store::{ViewOpKind, codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------------
// helpers

/// The view a port goes to in each test.
const MINE: &str = "mine";

fn mine(view: &str) -> Scope {
    Scope::View(view.into())
}

/// Opens view `view` in `t`, owned by [`writer`], on canon's tip.
fn open_view(t: &mut Rdy, view: &str) {
    t.view_create(&writer(), view, None, false)
        .unwrap_or_else(|e| panic!("create {view}: {e}"));
}

/// A registered briefing of `ids` in view `view` of `t`: its token.
fn token(t: &Rdy, view: &str, ids: &[&str]) -> String {
    brief(t, &mine(view), ids).base.token
}

fn texts(hs: &[Hash]) -> Vec<String> {
    hs.iter().map(ToString::to_string).collect()
}

fn read(from: &Path, sel: &[Hash]) -> Result<PortSource, RdyError> {
    local_port::read_source(from, &texts(sel))
}

/// Ports `sel` of the source at `from` into `view` of `t`, citing `brief`.
fn port(
    t: &mut Rdy,
    from: &Path,
    sel: &[Hash],
    view: &str,
    brief: Option<&str>,
) -> Result<ReplayResult, RdyError> {
    let source = read(from, sel)?;
    t.view_port(&writer(), view, &source, brief)
}

/// Ports `sel` into a new view `view` of `t`, with a fresh briefing of `ids` there.
fn port_briefed(
    t: &mut Rdy,
    from: &Path,
    sel: &[Hash],
    view: &str,
    ids: &[&str],
) -> Result<ReplayResult, RdyError> {
    if !t.read().has_view(view) {
        open_view(t, view);
    }
    let tok = token(t, view, ids);
    port(t, from, sel, view, Some(&tok))
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

fn stored(r: &Rdy, c: &Hash) -> Change {
    r.store().reader().change(c).unwrap()
}

/// What a refusal leaves as it was: the log's end and the view's head.
fn state(t: &Rdy, view: &str) -> (rdy_store::segment::Location, Option<Hash>) {
    (
        t.store().log_end(),
        t.read().view_head(view).map(|v| v.head),
    )
}

/// Every file under `dir`, by relative path, with its bytes.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(dir).unwrap().to_path_buf();
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// Asserts that `after` holds the same files as `before`, byte for byte; names the paths that
/// differ.
fn assert_same(
    before: &BTreeMap<PathBuf, Vec<u8>>,
    after: &BTreeMap<PathBuf, Vec<u8>>,
    what: &str,
) {
    let differ: Vec<&PathBuf> = before
        .keys()
        .chain(after.keys())
        .filter(|k| before.get(*k) != after.get(*k))
        .collect();
    assert!(differ.is_empty(), "{what}: {differ:?} differ");
}

/// Sets every file and directory under `dir` (itself included) read-only, or writable again.
fn set_read_only(dir: &Path, on: bool) {
    let mut all = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p.clone());
            }
            all.push(p);
        }
        all.push(d);
    }
    // directories last on the way in, first on the way out
    if !on {
        all.reverse();
    }
    for p in all {
        let mut perm = std::fs::metadata(&p).unwrap().permissions();
        perm.set_readonly(on);
        std::fs::set_permissions(&p, perm).unwrap();
    }
}

/// The source line `s` of [`world`], closed, with a dump of it: `(project, dump)`.
fn close_with_dump(s: Rdy, root: &Path) -> (PathBuf, PathBuf) {
    let dump = root.join("s.rdydump");
    s.read().dump(&dump).unwrap();
    s.close().unwrap();
    (root.join("s"), dump)
}

/// Records the staging directory of each dump port on this thread, and whether the dump loaded
/// there.
fn watch_staging() -> std::rc::Rc<std::cell::RefCell<Vec<(PathBuf, bool)>>> {
    let seen = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
    let into = std::rc::Rc::clone(&seen);
    local_port::on_staging_for_tests(Box::new(move |p: &Path| {
        into.borrow_mut()
            .push((p.to_path_buf(), p.join("source").is_dir()));
    }));
    seen
}

// ---------------------------------------------------------------------------------------------
// parents (written first)

#[test]
fn hpc4_project_and_dump_parent_checks() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        a,
        b,
        ..
    } = world();
    // l2 creates a section; l3 puts a claim under it
    let w = write(
        &mut s,
        "p",
        json!([{"op": "create", "type": "section", "id": "$p", "parent": notes, "fields": {"title": "New section"}}]),
    );
    let sec = w.created["$p"].clone();
    let l2 = land(&mut s, "p");
    write(
        &mut s,
        "q",
        json!([{"op": "create", "type": "claim", "parent": sec, "body": "Under it."}]),
    );
    let l3 = land(&mut s, "q");
    // l4 retitles the notes doc and puts a claim under it
    write(
        &mut s,
        "r",
        json!([
            {"op": "update", "id": notes, "fields": {"title": "Notes, in the source"}},
            {"op": "create", "type": "claim", "parent": notes, "body": "Under notes."}
        ]),
    );
    let l4 = land(&mut s, "r");
    // l5 moves section a under section b
    write(&mut s, "m", json!([{"op": "move", "id": a, "parent": b}]));
    let l5 = land(&mut s, "m");
    let (project, dump) = close_with_dump(s, root.path());
    let sources = [("project", &project), ("dump", &dump)];
    let refused = |t: &mut Rdy, view: &str, sel: &[Hash]| -> RdyError {
        open_view(t, view);
        let tok = token(t, view, &[&notes]);
        let before = state(t, view);
        let e = port(t, sources_of(view, &sources), sel, view, Some(&tok)).unwrap_err();
        assert_eq!(state(t, view), before, "{view}: nothing written");
        assert!(t.read().included(view).is_empty(), "{view}");
        e
    };

    for (kind, from) in sources {
        // a missing parent
        let view = format!("missing-{kind}");
        let e = refused(&mut t, &view, &[l3]);
        assert_eq!(
            refusal(&e),
            (
                E_DEPS_MISSING,
                format!("the port needs parent {sec} present and clean in the target").as_str(),
                "carry the change that creates the parent first, or resolve its conflict"
            ),
            "{kind}"
        );
        // the parent created cleanly in the same call
        let res = port_briefed(&mut t, from, &[l2, l3], &format!("clean-{kind}"), &[&notes])
            .unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert!(res.conflicts.is_empty(), "{kind}");
        assert_eq!(res.replayed.len(), 2, "{kind}");
    }

    // a conflicted parent: the target retitled the notes doc its own way
    write(
        &mut t,
        "title",
        json!([{"op": "update", "id": notes, "fields": {"title": "Notes, in the target"}}]),
    );
    land(&mut t, "title");
    for (kind, _) in sources {
        let e = refused(&mut t, &format!("conflicted-{kind}"), &[l4]);
        assert_eq!(e.code, E_DEPS_MISSING, "{kind}: {e:?}");
        assert_eq!(
            e.message,
            format!("the port needs parent {notes} present and clean in the target")
        );
    }

    // a move that makes a loop only against the target: the target put b under a
    write(&mut t, "mv", json!([{"op": "move", "id": b, "parent": a}]));
    land(&mut t, "mv");
    for (kind, _) in sources {
        let e = refused(&mut t, &format!("loop-{kind}"), &[l5]);
        let fix = "move one block out of the other in the target, then carry again";
        let one = format!("the port would make {a} and {b} each other's ancestors");
        let other = format!("the port would make {b} and {a} each other's ancestors");
        assert_eq!(
            (e.code, e.fix.as_str()),
            (E_BAD_INPUT, fix),
            "{kind}: {e:?}"
        );
        assert!(e.message == one || e.message == other, "{kind}: {e:?}");
    }
}

/// The source a test view of [`hpc4_project_and_dump_parent_checks`] names by its suffix.
fn sources_of<'a>(view: &str, sources: &[(&str, &'a PathBuf); 2]) -> &'a Path {
    let kind = view.rsplit('-').next().unwrap();
    sources.iter().find(|(k, _)| *k == kind).unwrap().1
}

// ---------------------------------------------------------------------------------------------
// one source, two forms

#[test]
fn hpt4_local_and_dump_ports_match() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        dc,
        ..
    } = world();
    let w = write_as(
        &mut s,
        "a",
        json!([{"op": "create", "type": "claim", "id": "$n", "parent": notes, "body": "New."}]),
        &[],
        "add a claim",
        editorial("add it"),
    );
    let n = w.created["$n"].clone();
    write_as(
        &mut s,
        "a",
        json!([{"op": "update", "id": dc, "body": "dc two"}]),
        &[],
        "edit the note",
        None,
    );
    let l2 = land(&mut s, "a");
    let l3 = edit(&mut s, "b", &dc, "dc three");
    let src: Vec<Hash> = [l2, l3]
        .iter()
        .flat_map(|l| s.store().reader().landing(l).unwrap().changes)
        .collect();
    let repo = s.read().repo_id().unwrap();
    let canon = |id: &str| version_in(&s, &Scope::Canon, id);
    let want = (canon(&dc), canon(&n));
    let (project, dump) = close_with_dump(s, root.path());

    // a landing and a change of a later landing, in any order: oldest first, once each
    let sel = [src[2], l2, src[0]];
    let mut results = vec![];
    for (view, from) in [("from-project", &project), ("from-dump", &dump)] {
        let res = port_briefed(&mut t, from, &sel, view, &[&notes]).unwrap();
        assert_eq!(res.view, view);
        assert_eq!(
            res.replayed
                .iter()
                .map(|r| h(&r.source))
                .collect::<Vec<_>>(),
            src,
            "{view}"
        );
        assert!(res.removed.is_empty() && res.cascaded.is_empty() && res.added_deps.is_empty());
        assert_eq!(
            res.added,
            res.replayed
                .iter()
                .map(|r| r.change.clone())
                .collect::<Vec<_>>()
        );
        // one Include of every new change, in port order
        let head = t.read().view_head(view).unwrap().head;
        assert_eq!(res.head, head.to_string());
        let op = t.store().reader().view_op(&head).unwrap();
        let ViewOpKind::Include { changes } = &op.op else {
            panic!("{view}: {:?}", op.op)
        };
        assert_eq!(texts(changes), res.added, "{view}");
        assert_eq!(t.read().included(view).to_vec(), changes.clone());
        let tok = stored(&t, &changes[0]).links.briefing;
        for (r, from) in res.replayed.iter().zip(&src) {
            let c = stored(&t, &h(&r.change));
            assert_eq!(
                c.links.ported_from,
                Some(PortedFrom {
                    repo_id: repo,
                    change: *from,
                }),
                "{view}"
            );
            assert_eq!(c.links.replayed_from, None, "{view}");
            assert_eq!(c.links.briefing, tok, "{view}: one target briefing");
            assert_eq!(c.view, view);
            assert_eq!(c.provenance.actor, "human:qa");
            assert!(
                !t.store().reader().has_kind(kinds::CHANGE, from).unwrap(),
                "no source copy"
            );
        }
        let got = (
            version_in(&t, &mine(view), &dc),
            version_in(&t, &mine(view), &n),
        );
        assert_eq!(got, want, "{view}");
        results.push((
            res.replayed
                .iter()
                .map(|r| {
                    let c = stored(&t, &h(&r.change));
                    (c.ops, c.message, c.justification, c.links.ported_from)
                })
                .collect::<Vec<_>>(),
            got,
        ));
    }
    // the project and the dump give the same port
    assert_eq!(results[0], results[1]);
    // no source landing enters the target
    for l in [l2, l3] {
        assert!(!t.store().reader().has_kind(kinds::LANDING, &l).unwrap());
    }

    // one undo removes the whole port
    let before = t.read().view_head("from-project").unwrap().head;
    t.undo(&writer(), "from-project", None, false).unwrap();
    assert!(t.read().included("from-project").is_empty());
    assert_ne!(t.read().view_head("from-project").unwrap().head, before);
    assert_eq!(version_in(&t, &mine("from-project"), &n), None);
}

// ---------------------------------------------------------------------------------------------
// the source

#[test]
fn hpt4_source_stays_unchanged_and_read_only() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        dc,
        ..
    } = world();
    let l2 = edit(&mut s, "a", &dc, "dc two");
    write(
        &mut s,
        "wip",
        json!([{"op": "update", "id": dc, "body": "unlanded"}]),
    );
    let (project, dump) = close_with_dump(s, root.path());
    let before = tree(&project);
    assert!(
        before.keys().any(|p| p.ends_with("index.redb")),
        "{:?}",
        before.keys()
    );
    let dump_bytes = std::fs::read(&dump).unwrap();

    // success and failure leave every byte of the source as it was
    open_view(&mut t, MINE);
    let tok = token(&t, MINE, &[&notes]);
    port(&mut t, &project, &[l2], MINE, Some(&tok)).unwrap();
    assert_same(&before, &tree(&project), "after a port");
    let e = port(&mut t, &project, &[Hash([3; 32])], MINE, Some(&tok)).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND, "{e:?}");
    assert_same(&before, &tree(&project), "after a refusal");
    assert_eq!(std::fs::read(&dump).unwrap(), dump_bytes);

    // a write through the source handle refuses
    let (mut r, staging) = local_port::open_port_source(&project).unwrap();
    assert!(staging.is_none());
    let e = r
        .write(
            &writer(),
            WriteRequest {
                view: "x".into(),
                ops: ops(json!([{"op": "update", "id": dc, "body": "no"}])),
                justification: None,
                message: "a write".into(),
            },
        )
        .unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_STORE_READ_ONLY,
            "this store was opened as a read-only port source",
            "open the target project for writes instead"
        )
    );
    r.close().unwrap();
    assert_same(&before, &tree(&project), "after a refused write");

    // the handle is closed when the read returns: the project opens for writes at once
    read(&project, &[l2]).unwrap();
    Rdy::open(&project).unwrap().close().unwrap();
    let before = tree(&project);

    // a read-only source directory ports the same
    set_read_only(&project, true);
    open_view(&mut t, "ro");
    let res = port_briefed(&mut t, &project, &[l2], "ro", &[&notes]);
    let refused = read(&project, &[Hash([3; 32])]).map(|_| ());
    set_read_only(&project, false);
    let res = res.unwrap();
    assert_eq!(res.replayed.len(), 1);
    assert_eq!(refused.unwrap_err().code, E_NOT_FOUND);
    assert_same(&before, &tree(&project), "read-only");
}

#[test]
fn hpt4_old_format_source_is_never_upgraded() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("old");
    let (seed, l2) = {
        let _legacy = rdy_store::format::legacy_writer_for_tests();
        let mut s = Rdy::init(&dir, "rdy-standard").unwrap();
        let w = write(
            &mut s,
            "seed",
            json!([
                {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
                {"op": "create", "type": "claim", "id": "$c", "parent": "$n", "body": "Old words."}
            ]),
        );
        let seed = land(&mut s, "seed");
        let c = w.created["$c"].clone();
        let l2 = edit(&mut s, "a", &c, "Old words, edited.");
        s.close().unwrap();
        (seed, l2)
    };
    let before = tree(&dir);
    // the target: a new project of this rdy with a doc of its own
    let mut t = Rdy::init(&root.path().join("t"), "rdy-standard").unwrap();
    let w = write(
        &mut t,
        "seed",
        json!([{"op": "create", "type": "doc", "id": "$o", "fields": {"name": "own", "title": "Own", "doc_type": "overview"}}]),
    );
    land(&mut t, "seed");
    let own = w.created["$o"].clone();
    let res = port_briefed(&mut t, &dir, &[seed, l2], MINE, &[&own]).unwrap();
    assert_eq!(res.replayed.len(), 2);
    assert_same(&before, &tree(&dir), "not upgraded, not reconciled");
    // an ordinary open would upgrade it
    let opened = Rdy::open(&dir).unwrap();
    opened.close().unwrap();
    assert!(
        tree(&dir) != before,
        "the ordinary open upgrades the old source"
    );
}

#[test]
fn hpt4_dump_staging_is_removed() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        dc,
        ..
    } = world();
    let l2 = edit(&mut s, "a", &dc, "dc two");
    let (_, dump) = close_with_dump(s, root.path());
    let seen = watch_staging();
    open_view(&mut t, MINE);
    let tok = token(&t, MINE, &[&notes]);
    // success
    port(&mut t, &dump, &[l2], MINE, Some(&tok)).unwrap();
    // failure while reading: an unknown change
    let e = read(&dump, &[Hash([3; 32])]).unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND);
    // failure while loading: a damaged dump
    let bad = root.path().join("bad.rdydump");
    let mut bytes = std::fs::read(&dump).unwrap();
    let mid = bytes.len() / 2;
    bytes.truncate(mid);
    std::fs::write(&bad, bytes).unwrap();
    assert!(read(&bad, &[l2]).is_err());
    // a missing source: no dump to load
    assert!(read(&root.path().join("nowhere"), &[l2]).is_err());
    let seen = seen.borrow();
    assert_eq!(seen.len(), 4, "{seen:?}");
    let loaded: Vec<bool> = seen.iter().map(|(_, l)| *l).collect();
    assert_eq!(loaded, [true, true, false, false]);
    for (p, _) in seen.iter() {
        assert!(!p.exists(), "{} was left", p.display());
    }
}

// ---------------------------------------------------------------------------------------------
// what can be ported

#[test]
fn hpt4_only_canon_changes_can_be_ported() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        dc,
        ..
    } = world();
    let l2 = edit(&mut s, "a", &dc, "dc two");
    let wip = h(&write(
        &mut s,
        "wip",
        json!([{"op": "update", "id": dc, "body": "unlanded"}]),
    )
    .change);
    let (project, dump) = close_with_dump(s, root.path());
    open_view(&mut t, MINE);
    let tok = token(&t, MINE, &[&notes]);
    let before = state(&t, MINE);
    for from in [&project, &dump] {
        // an unlanded view change
        let e = port(&mut t, from, &[l2, wip], MINE, Some(&tok)).unwrap_err();
        assert_eq!(
            refusal(&e),
            (
                E_BAD_INPUT,
                format!("change {wip} has not landed in {}", from.display()).as_str(),
                "land it there first, or replay it inside that project"
            )
        );
        // a hash the source does not hold
        let e = port(&mut t, from, &[Hash([3; 32])], MINE, Some(&tok)).unwrap_err();
        assert_eq!(e.code, E_NOT_FOUND, "{e:?}");
        // not a full hash
        let e = local_port::read_source(from, &["abc".into()]).unwrap_err();
        assert_eq!(e.code, E_BAD_INPUT, "{e:?}");
        assert_eq!(state(&t, MINE), before);
    }
    // empty input and too many entries refuse before the source opens
    let nowhere = root.path().join("nowhere");
    let e = local_port::read_source(&nowhere, &[]).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_BAD_INPUT,
            "name at least one change to replay",
            "call rdy_view with op replay and changes, or rdy view replay <view> <change>..."
        )
    );
    let many: Vec<String> = (0..1001).map(|_| l2.to_string()).collect();
    let e = local_port::read_source(&nowhere, &many).unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_LIMIT,
            "replay accepts at most 1000 changes, 10000 ops and 16777216 encoded bytes",
            "replay a smaller set of changes"
        )
    );

    // D-99CWC: a change a fork holds only as later evidence is no change of it
    let (fork, evidence) = evidence_fork(root.path());
    for c in &evidence {
        let e = read(&fork, &[*c]).unwrap_err();
        assert_eq!(
            refusal(&e),
            (
                E_NOT_FOUND,
                format!("no change {}", c.short()).as_str(),
                "check the id with rdy view show <view>"
            ),
            "{e:?}"
        );
    }
}

/// A project that lands a secret, tags v1.0.0, then removes the secret; and its fork at v1.0.0,
/// closed. Returns the fork and the removal's changes, which the fork holds as evidence only.
fn evidence_fork(root: &Path) -> (PathBuf, Vec<Hash>) {
    let mut p = Rdy::init(&root.join("ev"), "rdy-standard").unwrap();
    let w = write(
        &mut p,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "intro", "title": "Intro", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$d", "body": "The code is ZEBRA-7731."}
        ]),
    );
    land(&mut p, "work");
    p.tag(&approver(), "v1.0.0", None, None).unwrap();
    p.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![w.created["$x"].clone()],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .unwrap();
    let p3 = land(&mut p, "redact");
    let dir = root.join("ev-fork");
    rdy_engine::clone::clone_at(
        rdy_engine::clone::CloneSource::State(&p.read()),
        &dir,
        true,
        Some("v1.0.0"),
    )
    .unwrap();
    p.close().unwrap();
    let f = Rdy::open(&dir).unwrap();
    let evidence: Vec<Hash> = {
        let reader = f.store().reader();
        reader
            .landing(&p3)
            .map(|l| l.changes)
            .unwrap_or_default()
            .into_iter()
            .filter(|c| reader.has_kind(kinds::CHANGE, c).unwrap())
            .collect()
    };
    f.close().unwrap();
    assert!(!evidence.is_empty(), "the fork holds the removal's changes");
    (dir, evidence)
}

// ---------------------------------------------------------------------------------------------
// the target's rules, before any write

#[test]
fn hpt4_target_rules_come_before_any_write() {
    let World {
        root,
        mut s,
        mut t,
        c,
        dc,
        ..
    } = world();
    // the source edits stable content under its own briefing
    write_as(
        &mut s,
        "a",
        json!([{"op": "update", "id": c, "body": "Refunds take three days."}]),
        &[&c],
        "faster refunds",
        editorial("say it as it is"),
    );
    let l2 = land(&mut s, "a");
    let l3 = edit(&mut s, "b", &dc, "dc two");
    let source_token = s
        .store()
        .reader()
        .change(&s.store().reader().landing(&l2).unwrap().changes[0])
        .unwrap()
        .links
        .briefing
        .unwrap();
    // the target's own claim moves on its canon, so no target briefing equals the source's
    write_as(
        &mut t,
        "ctx",
        json!([{"op": "update", "id": c, "body": "Refunds take four days."}]),
        &[&c],
        "a target edit",
        editorial("the target's own words"),
    );
    land(&mut t, "ctx");
    let (project, _) = close_with_dump(s, root.path());
    let source = read(&project, &[l2]).unwrap();
    open_view(&mut t, MINE);
    let same = |t: &mut Rdy, view: &str, port: RdyError| {
        let replay = t
            .view_replay(&writer(), view, &[Hash([9; 32]).to_string()])
            .unwrap_err();
        assert_eq!(refusal(&port), refusal(&replay), "as a replay refuses");
    };

    // a view of another session
    let other = Session::agent("agent:z".into(), Some("z1".into()), Role::Writer).unwrap();
    t.view_create(&other, "theirs", None, false).unwrap();
    let tok = token(&t, "theirs", &[&c]);
    let before = state(&t, "theirs");
    let e = t
        .view_port(&writer(), "theirs", &source, Some(&tok))
        .unwrap_err();
    assert_eq!(e.code, E_VIEW_OWNER, "{e:?}");
    same(&mut t, "theirs", e);
    assert_eq!(state(&t, "theirs"), before);
    // a closed view
    open_view(&mut t, "shut");
    t.view_close(&writer(), &["shut".into()], "done").unwrap();
    let before = state(&t, "shut");
    let e = t
        .view_port(&writer(), "shut", &source, Some(&tok))
        .unwrap_err();
    same(&mut t, "shut", e);
    assert_eq!(state(&t, "shut"), before);
    // a missing view
    let e = t
        .view_port(&writer(), "nowhere", &source, Some(&tok))
        .unwrap_err();
    same(&mut t, "nowhere", e);

    let before = state(&t, MINE);
    let briefs = t.briefs().len();
    // no briefing, drafts too
    for (sel, what) in [(l2, "stable"), (l3, "draft")] {
        let src = read(&project, &[sel]).unwrap();
        let e = t.view_port(&writer(), MINE, &src, None).unwrap_err();
        assert_eq!(
            refusal(&e),
            (
                E_BRIEF_REQUIRED,
                "a port needs a target briefing",
                "brief the target blocks and retry with the token"
            ),
            "{what}"
        );
    }
    // an unknown token, and the source's own token, which the target never made
    let unknown = "ab".repeat(32);
    let e = t
        .view_port(&writer(), MINE, &source, Some(&unknown))
        .unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_BRIEF_STALE,
            "the cited briefing abababab is unknown: the daemon restarted, or the briefing was evicted",
            "brief these blocks (rdy_brief) and retry with the token"
        )
    );
    let e = t
        .view_port(&writer(), MINE, &source, Some(&source_token.to_string()))
        .unwrap_err();
    assert_eq!(e.code, E_BRIEF_STALE, "{e:?}");
    assert_eq!(
        t.briefs().len(),
        briefs,
        "a refused port registers no briefing"
    );
    assert_eq!(state(&t, MINE), before);
    // a stale token: the claim moves in the view after the briefing
    let stale = token(&t, MINE, &[&c]);
    let mid = token(&t, MINE, &[&c]);
    t.write_briefed(
        &writer(),
        WriteRequest {
            view: MINE.into(),
            ops: ops(json!([{"op": "update", "id": c, "body": "Refunds take six days."}])),
            justification: editorial("the target's own words"),
            message: "a target edit".into(),
        },
        Some(mid),
    )
    .unwrap();
    let before = state(&t, MINE);
    let briefs = t.briefs().len();
    let e = t
        .view_port(&writer(), MINE, &source, Some(&stale))
        .unwrap_err();
    assert_eq!(
        refusal(&e),
        (
            E_BRIEF_STALE,
            format!("the cited briefing is stale: {c} moved since it was made").as_str(),
            "brief these blocks (rdy_brief) and retry with the token"
        )
    );
    assert_eq!(state(&t, MINE), before);
    assert_eq!(
        t.briefs().len(),
        briefs,
        "a refused port registers no briefing"
    );
    // a fresh target briefing ports; each new change records it, never the source's
    let fresh = token(&t, MINE, &[&c]);
    let res = t.view_port(&writer(), MINE, &source, Some(&fresh)).unwrap();
    let new = stored(&t, &h(&res.replayed[0].change));
    assert_eq!(new.links.briefing.map(|b| b.to_string()), Some(fresh));
    assert_ne!(new.links.briefing, Some(source_token));
}

// ---------------------------------------------------------------------------------------------
// blobs and atomic refusals

#[test]
fn hpt4_blob_closure_and_atomic_refusals() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        dc,
        ..
    } = world();
    let bytes: Vec<u8> = (0..(1 << 20) + 9).map(|i: usize| (i % 251) as u8).collect();
    let (lf, blob, fig) = land_figure(&mut s, "f", &notes, &bytes);
    let (lg, gone, _) = land_figure(&mut s, "g", &notes, &[7u8; 64]);
    remove(&mut s, "rm", &format!("blob:{gone}"));
    let l3 = edit(&mut s, "e", &dc, "An edit.");
    let (project, dump) = close_with_dump(s, root.path());

    for (view, from) in [("fig-project", &project), ("fig-dump", &dump)] {
        // the figure comes with its blob: every chunk, then the manifest
        let res = port_briefed(&mut t, from, &[lf], view, &[&notes]).unwrap();
        assert_eq!(res.replayed.len(), 1);
        assert!(t.store().reader().has_blob(&blob).unwrap(), "{view}");
        assert!(version_in(&t, &mine(view), &fig).is_some());
        t.store().check_objects().unwrap();
        // a removed source blob refuses the whole selection
        open_view(&mut t, &format!("gone-{view}"));
        let before = t.store().log_end();
        let e = read(from, &[l3, lg]).unwrap_err();
        assert_eq!(
            refusal(&e),
            (
                E_REDACTED_STATE,
                "a source blob of the ported change was removed",
                "port the change after its figure is replaced"
            ),
            "{view}"
        );
        assert_eq!(t.store().log_end(), before);
    }

    // a target removal refuses the port, and nothing is written
    remove(&mut t, "trm", &dc);
    open_view(&mut t, MINE);
    let tok = token(&t, MINE, &[&notes]);
    let before = state(&t, MINE);
    let e = port(&mut t, &project, &[l3], MINE, Some(&tok)).unwrap_err();
    assert_eq!(e.code, E_REDACTED_STATE, "{e:?}");
    assert_eq!(
        e.message,
        "the port bundle holds a version or blob removed in this line"
    );
    assert_eq!(state(&t, MINE), before);
    // the figure ports again into another view: the held blob is not stored twice
    let end = t.store().log_end();
    port_briefed(&mut t, &dump, &[lf], "fig-again", &[&notes]).unwrap();
    assert!(t.store().log_end() > end);
    t.store().check_objects().unwrap();
}

// ---------------------------------------------------------------------------------------------
// the dispatcher

fn req(v: Value) -> ViewRequest {
    serde_json::from_value(v).unwrap()
}

#[test]
fn hpc4_source_selectors_dispatch() {
    let World {
        root,
        mut s,
        t,
        notes,
        dc,
        ..
    } = world();
    let l2 = edit(&mut s, "a", &dc, "dc two");
    let (project, dump) = close_with_dump(s, root.path());
    let mut t = t;
    open_view(&mut t, MINE);
    open_view(&mut t, "other");
    let e = Local::new(t);
    let w = writer();
    let run = |v: Value| run_view(&e, &w, "human:qa", MINE, req(v));
    let brief_of = |view: &str| -> String {
        let b = e
            .read()
            .brief(
                &mine(view),
                &rdy_engine::brief::Coverage {
                    ids: vec![notes.clone()],
                    path: None,
                },
                None,
            )
            .unwrap();
        b.register(&e.briefs());
        b.base.token
    };
    let head = |view: &str| e.read().view_head(view).unwrap().head;
    let before = head(MINE);
    let bad = |v: Value, message: &str, fix: &str| {
        let err = run(v.clone()).unwrap_err();
        assert_eq!(refusal(&err), (E_BAD_INPUT, message, fix), "{v}");
    };

    // two selectors refuse, before either opens
    bad(
        json!({"op": "replay", "changes": [l2.to_string()], "from": "/nowhere", "from_file": "/nowhere.rdydump"}),
        "replay takes only one source",
        "pass from or from_file, not both",
    );
    // a briefing needs a source
    bad(
        json!({"op": "replay", "changes": [l2.to_string()], "brief": "ab"}),
        "replay without a source does not take brief",
        "remove brief, or name a port source",
    );
    // the source fields belong to replay; fork keeps its from
    for (op, field) in [
        ("include", "from"),
        ("create", "from_file"),
        ("show", "brief"),
        ("fork", "from_file"),
        ("undo", "brief"),
    ] {
        let mut v = if op == "create" || op == "fork" {
            json!({"op": op, "name": "n", "from": MINE})
        } else {
            json!({"op": op, "view": MINE, "changes": [l2.to_string()]})
        };
        if op == "create" {
            v.as_object_mut().unwrap().remove("from");
        }
        v[field] = json!("x");
        bad(
            v,
            &format!("{op} does not take {field}"),
            "use the source field only with replay",
        );
    }
    let forked = run(json!({"op": "fork", "name": "forked", "from": MINE})).unwrap();
    assert!(e.read().view_head("forked").is_some(), "{forked}");
    assert_eq!(head(MINE), before);

    // a project from and a dump from_file both reach the port preparation
    for (field, path) in [("from", &project), ("from_file", &dump)] {
        let mut v = json!({"op": "replay", "changes": [l2.to_string()]});
        v[field] = json!(path.display().to_string());
        let err = run(v.clone()).unwrap_err();
        assert_eq!(err.code, E_BRIEF_REQUIRED, "{field}: {err:?}");
        assert_eq!(head(MINE), before);
        v["view"] = json!("other");
        v["brief"] = json!(brief_of("other"));
        let out = run(v).unwrap();
        assert_eq!(out["view"], "other", "{out}");
        let replayed = out["replayed"].as_array().unwrap();
        assert_eq!(replayed.len(), 1, "{out}");
        assert_eq!(
            replayed[0]["source"],
            json!(
                // the one change of l2
                change_of(&project, l2).to_string()
            )
        );
        // the reply carries consequences as a replay's does
        assert!(out.get("consequences").is_some(), "{out}");
    }
    // a relative path names one under the project root
    let rel =
        run(json!({"op": "replay", "changes": [l2.to_string()], "from_file": "../s.rdydump"}))
            .unwrap_err();
    assert_eq!(rel.code, E_BRIEF_REQUIRED, "{rel:?}");
    assert_eq!(head(MINE), before);
}

/// The one change of landing `l` of the closed project at `dir`.
fn change_of(dir: &Path, l: Hash) -> Hash {
    let r = Rdy::open(dir).unwrap();
    let c = r.store().reader().landing(&l).unwrap().changes[0];
    r.close().unwrap();
    c
}

#[test]
fn hpt4_without_source_keeps_replay_contract() {
    let World {
        root, mut t, dc, ..
    } = world();
    // a change of the target's own view, replayed with no source
    let own = h(&write(
        &mut t,
        "src",
        json!([{"op": "update", "id": dc, "body": "own"}]),
    )
    .change);
    open_view(&mut t, MINE);
    let e = Local::new(t);
    let w = writer();
    let run = |v: Value| run_view(&e, &w, "human:qa", MINE, req(v));
    // no briefing needed, and replayed_from names the source
    let out = run(json!({"op": "replay", "changes": [own.to_string()]})).unwrap();
    let new = h(out["replayed"][0]["change"].as_str().unwrap());
    let c = e.read().reader().change(&new).unwrap();
    assert_eq!(c.links.replayed_from, Some(own));
    assert_eq!(c.links.ported_from, None);
    assert_eq!(c.links.briefing, None);
    assert!(out.get("consequences").is_some(), "{out}");
    // the replay's own refusals stay: a change of no view here is no change
    let err = run(json!({"op": "replay", "changes": [Hash([3; 32]).to_string()]})).unwrap_err();
    assert_eq!(err.code, E_NOT_FOUND, "{err:?}");
    let _ = root;
}

// ---------------------------------------------------------------------------------------------
// an empty target: a port needs a briefing only for the target blocks it touches

/// The source line of [`world`] with one more landing, a new claim under `notes`, closed with
/// its dump: `(root, project, dump, seed, later, notes)`.
fn bootstrap_source() -> (tempfile::TempDir, PathBuf, PathBuf, Hash, Hash, String) {
    let World {
        root,
        mut s,
        notes,
        seed,
        ..
    } = world();
    write(
        &mut s,
        "more",
        json!([{"op": "create", "type": "claim", "parent": notes, "body": "A later note."}]),
    );
    let later = land(&mut s, "more");
    let (project, dump) = close_with_dump(s, root.path());
    (root, project, dump, seed, later, notes)
}

#[test]
fn hpt4_empty_target_port_needs_no_briefing() {
    let (root, project, dump, seed, _, notes) = bootstrap_source();
    for (kind, from) in [("project", &project), ("dump", &dump)] {
        let mut t = Rdy::init(&root.path().join(format!("empty-{kind}")), "rdy-standard").unwrap();
        open_view(&mut t, MINE);
        let briefs = t.briefs().len();
        // the target holds no block the port touches: no briefing and no token
        let res =
            port(&mut t, from, &[seed], MINE, None).unwrap_or_else(|e| panic!("{kind}: {e:?}"));
        assert_eq!(res.replayed.len(), 1, "{kind}");
        let new = stored(&t, &h(&res.replayed[0].change));
        assert_eq!(new.links.briefing, None, "{kind}: records no briefing");
        assert!(new.links.ported_from.is_some(), "{kind}");
        assert_eq!(t.briefs().len(), briefs, "{kind}: registers no briefing");
        assert!(
            t.read()
                .materialized(&mine(MINE))
                .unwrap()
                .get(&bid(&notes))
                .is_some(),
            "{kind}"
        );
        // it lands: no stable change lacks a briefing
        land(&mut t, MINE);
        t.close().unwrap();
    }
}

#[test]
fn hpt4_port_that_anchors_to_a_target_block_needs_briefing() {
    let (root, project, dump, seed, later, notes) = bootstrap_source();
    for (kind, from) in [("project", &project), ("dump", &dump)] {
        let mut t = Rdy::init(&root.path().join(format!("boot-{kind}")), "rdy-standard").unwrap();
        open_view(&mut t, "first");
        port(&mut t, from, &[seed], "first", None).unwrap_or_else(|e| panic!("{kind}: {e:?}"));
        land(&mut t, "first");
        // the target now holds notes, and the later claim hangs under it
        open_view(&mut t, MINE);
        let before = state(&t, MINE);
        let briefs = t.briefs().len();
        let e = port(&mut t, from, &[later], MINE, None).unwrap_err();
        assert_eq!(
            refusal(&e),
            (
                E_BRIEF_REQUIRED,
                "a port needs a target briefing",
                "brief the target blocks and retry with the token"
            ),
            "{kind}"
        );
        assert_eq!(state(&t, MINE), before, "{kind}: nothing written");
        assert_eq!(t.briefs().len(), briefs, "{kind}");
        // a briefing of the anchor lets the same port through
        let res = port_briefed(&mut t, from, &[later], MINE, &[&notes])
            .unwrap_or_else(|e| panic!("{kind}: {e:?}"));
        let new = stored(&t, &h(&res.replayed[0].change));
        assert!(new.links.briefing.is_some(), "{kind}");
        t.close().unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// ruled ops and pins (D-1QT23, D-08M5W)

const AGENTS: &str = "pf-AGNT1";

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn cited_x() -> Block {
    Block {
        id: bid("d-FRGNX"),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::from([("title".to_string(), FieldValue::Str("X".into()))]),
        body: "Use X.".into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn version_of(b: &Block) -> Hash {
    Hash::of(&codec::item_new(kinds::BLOCK, b).1)
}

fn content_of(b: &Block) -> Hash {
    Hash::of(format!("content of {} {}", b.id, b.body).as_bytes())
}

/// Delivers `x` to `r` through its link `link`, straight to the store.
fn deliver(r: &mut Rdy, link: &str, x: &Block) {
    let entry = ForeignEntry {
        repo: cited(),
        source: ForeignSource::Link {
            link: link.into(),
            site: "hub.example".into(),
            at_ms: 1_760_000_000_000,
        },
        event: ForeignEvent::Delivery {
            prev: None,
            publication: ForeignPublication {
                tag: "v1".into(),
                profile: AGENTS.into(),
                profile_version: None,
            },
            set: None,
            added: vec![ForeignVersion {
                id: x.id.clone(),
                version: version_of(x),
                content: content_of(x),
                encoding: 0,
                derived: ForeignDerived::default(),
            }],
            changed: vec![],
            removed: vec![],
            shells: vec![],
            selection: selection_identity(BTreeMap::from([(x.id.clone(), version_of(x))]).iter())
                .unwrap(),
        },
    };
    r.store_mut()
        .put_foreign(&entry, std::slice::from_ref(x))
        .unwrap();
    r.publish_after_foreign_for_tests().unwrap();
}

#[test]
fn hpr4_local_dump_follow_ruled_ops_and_pins() {
    let World {
        root,
        mut s,
        mut t,
        notes,
        dc,
        ..
    } = world();
    // l2: an edit of the note and a new claim m; l3 edits both again
    let w = write(
        &mut s,
        "a",
        json!([
            {"op": "update", "id": dc, "body": "one"},
            {"op": "create", "type": "claim", "id": "$m", "parent": notes, "body": "m one"}
        ]),
    );
    let m = w.created["$m"].clone();
    let l2 = land(&mut s, "a");
    write(
        &mut s,
        "b",
        json!([
            {"op": "update", "id": dc, "body": "two"},
            {"op": "update", "id": m, "body": "m two"}
        ]),
    );
    let l3 = land(&mut s, "b");
    let m2 = version_in(&s, &Scope::Canon, &m);
    // l4: a claim k, which l5 deletes; l6 deletes the note
    let w = write(
        &mut s,
        "k",
        json!([{"op": "create", "type": "claim", "id": "$k", "parent": notes, "body": "k"}]),
    );
    let k = w.created["$k"].clone();
    land(&mut s, "k");
    write(&mut s, "dk", json!([{"op": "delete", "id": k}]));
    let l5 = land(&mut s, "dk");
    write(&mut s, "dd", json!([{"op": "delete", "id": dc}]));
    let l6 = land(&mut s, "dd");
    // l7: a write with an intent and an abandoned alternative
    for v in ["alt-a", "alt-b"] {
        s.view_create(&writer(), v, None, false).unwrap();
    }
    let set = s
        .alt_create(
            &writer(),
            "Which note?",
            None,
            &[
                rdy_engine::alt::AltMemberInput {
                    label: "a".into(),
                    view: "alt-a".into(),
                },
                rdy_engine::alt::AltMemberInput {
                    label: "b".into(),
                    view: "alt-b".into(),
                },
            ],
        )
        .unwrap();
    s.alt_abandon(&writer(), &set.id, "neither").unwrap();
    let intent = s
        .intent_post(
            &writer(),
            Some("w"),
            None,
            std::slice::from_ref(&m),
            "edit m",
            None,
        )
        .unwrap();
    let linked = s
        .write_linked(
            &writer(),
            WriteRequest {
                view: "w".into(),
                ops: ops(json!([{"op": "update", "id": m, "body": "m with links"}])),
                justification: None,
                message: "with an intent and an alternative".into(),
            },
            ChangeLinks {
                intent: Some(s.read().resolve_intent(&intent.id).unwrap().0),
                alternative: Some(AltChoice {
                    set: h(&set.set),
                    member: "alt-a".into(),
                }),
                ..Default::default()
            },
            None,
        )
        .unwrap();
    let linked = stored(&s, &h(&linked.change));
    assert!(linked.links.intent.is_some() && linked.links.alternative.is_some());
    let l7 = land(&mut s, "w");
    // l8: a foreign pin
    let w = write(
        &mut s,
        "c",
        json!([{"op": "create", "type": "repo_link", "id": "$l", "parent": notes,
            "fields": {"alias": "core", "repo": cited().to_string(), "profile": AGENTS}}]),
    );
    let link = w.created["$l"].clone();
    land(&mut s, "c");
    let x = cited_x();
    deliver(&mut s, &link, &x);
    write(
        &mut s,
        "d",
        json!([{"op": "create", "type": "claim", "id": "$f", "parent": notes, "body": "Per {{core:d-FRGNX}}."}]),
    );
    let l8 = land(&mut s, "d");
    let src3: Vec<Change> = s
        .store()
        .reader()
        .landing(&l3)
        .unwrap()
        .changes
        .iter()
        .map(|c| stored(&s, c))
        .collect();
    let (project, dump) = close_with_dump(s, root.path());
    let as_create = |c: &Change| -> Vec<Op> {
        c.ops
            .iter()
            .map(|op| match op {
                Op::Update { block, .. } => Op::Create {
                    block: block.clone(),
                },
                other => other.clone(),
            })
            .collect()
    };

    // a delete whose base the target lacks, of a block the target holds: refused, nothing
    // written
    for (kind, from) in [("project", &project), ("dump", &dump)] {
        let view = format!("del-{kind}");
        open_view(&mut t, &view);
        let tok = token(&t, &view, &[&notes]);
        let before = state(&t, &view);
        let e = port(&mut t, from, &[l6], &view, Some(&tok)).unwrap_err();
        assert_eq!(
            refusal(&e),
            (
                E_BAD_INPUT,
                "the target line lacks the base of a ported delete",
                "carry the landing that wrote it first"
            ),
            "{kind}"
        );
        assert_eq!(state(&t, &view), before);
    }
    // missing bases become creates: the held note conflicts, the never-held claim applies
    for (kind, from) in [("project", &project), ("dump", &dump)] {
        let view = format!("late-{kind}");
        let res = port_briefed(&mut t, from, &[l3], &view, &[&notes]).unwrap();
        let new = stored(&t, &h(&res.replayed[0].change));
        assert_eq!(new.ops, as_create(&src3[0]), "{kind}: stored as a create");
        assert!(new.ops.iter().all(|op| op.base().is_none()));
        assert_eq!(version_in(&t, &mine(&view), &m), m2, "{kind}");
        let conflicts: Vec<String> = res.conflicts.iter().map(|c| c.block.clone()).collect();
        assert_eq!(conflicts, vec![dc.clone()], "{kind}");
    }
    for (kind, from) in [("project", &project), ("dump", &dump)] {
        // held bases stay: an earlier port stored them
        let res =
            port_briefed(&mut t, from, &[l2, l3], &format!("held-{kind}"), &[&notes]).unwrap();
        assert!(res.conflicts.is_empty(), "{kind}");
        let new = stored(&t, &h(&res.replayed[1].change));
        assert_eq!(new.ops, src3[0].ops, "{kind}");
        // a missing-base delete of a block the target lacks: an empty change keeps its place
        let res = port_briefed(&mut t, from, &[l5], &format!("empty-{kind}"), &[&notes]).unwrap();
        assert_eq!(res.replayed.len(), 1);
        let new = stored(&t, &h(&res.replayed[0].change));
        assert!(new.ops.is_empty(), "{kind}");
        assert_eq!(
            t.read().included(&format!("empty-{kind}")).to_vec(),
            vec![new.id()]
        );
        // intent and alternative are dropped
        let res =
            port_briefed(&mut t, from, &[l2, l7], &format!("links-{kind}"), &[&notes]).unwrap();
        let new = stored(&t, &h(&res.replayed.last().unwrap().change));
        assert_eq!(new.ops, linked.ops);
        assert_eq!((new.links.intent, new.links.alternative), (None, None));
        t.store().check_objects().unwrap();
    }

    // a foreign pin the target holds no delivery of refuses; with its own delivery, it ports
    let link_landing = |t: &mut Rdy| {
        let w = write(
            t,
            "link",
            json!([{"op": "create", "type": "repo_link", "id": "$l", "parent": notes,
                "fields": {"alias": "core", "repo": cited().to_string(), "profile": AGENTS}}]),
        );
        land(t, "link");
        w.created["$l"].clone()
    };
    for (kind, from) in [("project", &project), ("dump", &dump)] {
        let view = format!("foreign-{kind}");
        open_view(&mut t, &view);
        let tok = token(&t, &view, &[&notes]);
        let before = state(&t, &view);
        let e = port(&mut t, from, &[l8], &view, Some(&tok)).unwrap_err();
        let repo12 = &cited().to_string()[..12];
        let short = &content_of(&x).to_string()[..12];
        assert_eq!(
            refusal(&e),
            (
                E_FOREIGN_VERSION,
                format!("{repo12}:d-FRGNX has no delivered version {short}").as_str(),
                format!(
                    "wait for the delivery that holds it, or pin a delivered version: rdy_links {{op: \"show\", alias}} / rdy links show {repo12}"
                )
                .as_str()
            ),
            "{kind}"
        );
        assert_eq!(state(&t, &view), before);
    }
    let tlink = link_landing(&mut t);
    deliver(&mut t, &tlink, &x);
    let res = port_briefed(&mut t, &dump, &[l8], "foreign-held", &[&notes]).unwrap();
    assert_eq!(res.replayed.len(), 1);

    // direct replay ancestry: a same-store replay of a ported change keeps its origin
    let ported: Vec<Hash> = t.read().included("late-project").to_vec();
    open_view(&mut t, "again");
    let res = t.view_replay(&writer(), "again", &texts(&ported)).unwrap();
    for (r, from) in res.replayed.iter().zip(&ported) {
        let c = stored(&t, &h(&r.change));
        let p = stored(&t, from);
        assert_eq!(c.links.replayed_from, Some(*from));
        assert_eq!(c.links.ported_from, p.links.ported_from);
        assert!(c.links.ported_from.is_some());
    }
}
