#![cfg(unix)]
// Unix only: it reads the signal that ended a child. FW-XP1 Phase 4 ports it.
//! vhost1: hostile view names and large sizes for views and rebase.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
use rdy_qa::invariants::{check, check_views};
use rdy_store::segment::CrashPoint;
use rdy_store::view::{Owner, ViewOpKind};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}
fn update(id: &str, base: &str, body: &str) -> serde_json::Value {
    json!([{"op": "update", "id": id, "base": base, "body": body}])
}

/// Every path under `dir` except what is under `skip`, relative to `dir`.
fn walk(dir: &Path, skip: &Path) -> BTreeSet<PathBuf> {
    fn go(root: &Path, dir: &Path, skip: &Path, out: &mut BTreeSet<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.starts_with(skip) {
                continue;
            }
            out.insert(p.strip_prefix(root).unwrap().to_path_buf());
            if p.is_dir() {
                go(root, &p, skip, out);
            }
        }
    }
    let mut out = BTreeSet::new();
    go(dir, dir, skip, &mut out);
    out
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn names() -> Vec<(&'static str, String)> {
    vec![
        ("dotdot", "../x".into()),
        ("300 bytes", "n".repeat(300)),
        ("view:x", "view:x".into()),
        ("canon", "canon".into()),
        ("nul", "a\0b".into()),
        ("emoji", "\u{1F600}\u{1F4A5}".into()),
    ]
}

/// What an operation on a view name did: success or the error code.
fn outcome<T>(r: Result<T, RdyError>) -> Result<(), &'static str> {
    r.map(|_| ()).map_err(|e| e.code)
}

/// The engine's view-name cap (`MAX_VIEW_NAME`, counted in chars).
const MAX_VIEW_NAME_CHARS: usize = 256;

#[test]
fn vhost1_hostile_view_names() {
    for (label, name) in names() {
        let mut results: Vec<(&str, Result<(), &'static str>)> = vec![];
        for op in ["write", "create", "fork", "include"] {
            let base = tempfile::tempdir().unwrap();
            let dir = base.path().join("p");
            std::fs::create_dir_all(&dir).unwrap();
            let mut r = Rdy::init(&dir, "rdy-standard").unwrap();
            let (_doc, section, _c) = seed_doc(&mut r, "seed");
            land(&mut r, "seed").unwrap();
            let a = as_agent("agent:a", "r1");
            let ch = write_as(&mut r, &a, "src", claim(&section, "x"))
                .unwrap()
                .change;
            let before = walk(base.path(), &dir.join(".rdy"));
            let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match op {
                "write" => outcome(write_as(&mut r, &a, &name, claim(&section, "y"))),
                "create" => outcome(r.view_create(&a, &name, None, false)),
                "fork" => outcome(r.view_fork(&a, "src", &name)),
                _ => outcome(r.view_include(&a, &name, std::slice::from_ref(&ch))),
            }));
            let got = got.unwrap_or_else(|_| panic!("{label}: {op} panicked"));
            assert_eq!(
                before,
                walk(base.path(), &dir.join(".rdy")),
                "{label}: {op} wrote a file outside .rdy"
            );
            if let Err(code) = got {
                // the engine caps a view name at 256 characters with E_LIMIT
                if name.chars().count() > MAX_VIEW_NAME_CHARS {
                    assert_eq!(code, E_LIMIT, "{label}: {op} failed with {code}");
                } else {
                    assert!(
                        code == E_RESERVED_VIEW || code == E_BAD_INPUT,
                        "{label}: {op} failed with {code}"
                    );
                }
            } else {
                assert!(
                    name.chars().count() <= MAX_VIEW_NAME_CHARS,
                    "{label}: {op} accepted a view name over {MAX_VIEW_NAME_CHARS} characters"
                );
            }
            if got.is_ok() {
                r.fsck().unwrap_or_else(|e| panic!("{label}: {op}: {e}"));
                check(&r, &Scope::Canon).unwrap_or_else(|e| panic!("{label}: {op}: {e:?}"));
            }
            results.push((op, got));
        }
        let write = results[0].1;
        for (op, got) in &results[1..] {
            assert_eq!(*got, write, "{label}: {op} disagrees with write");
        }
    }
}

#[test]
fn vhost1_a_5000_change_include_and_a_50_view_landing_stay_correct() {
    let (n, views, hot) = if deep() {
        (20_000, 200, 40)
    } else {
        (5_000, 50, 10)
    };
    // one view with n one-claim changes, included into another in one call
    {
        let (_d, mut r) = project();
        let (_doc, section, _c) = seed_doc(&mut r, "seed");
        land(&mut r, "seed").unwrap();
        let a = as_agent("agent:a", "r1");
        let mut changes = vec![];
        for i in 0..n {
            changes.push(
                write_as(&mut r, &a, "src", claim(&section, &format!("c{i}")))
                    .unwrap()
                    .change,
            );
        }
        let res = r.view_include(&a, "dst", &changes).unwrap();
        assert_eq!(res.added.len(), n, "the include added every change");
        assert_eq!(r.read().included("dst").len(), n);
        assert!(check_views(&r).is_ok(), "{:?}", check_views(&r));
        check(&r, &Scope::Canon).unwrap();
        check(&r, &Scope::View("dst".into())).unwrap();
    }
    // `views` unrelated views, `hot` views on the landed claim, and the proposer
    let (_d, mut r) = project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "seed", "title": "Seed", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}}),
    ];
    for i in 0..=views {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{i}"), "parent": "$s", "body": format!("claim {i}")}));
    }
    let seeded = write(&mut r, "seed", serde_json::Value::Array(ops)).unwrap();
    land(&mut r, "seed").unwrap();
    let canon = Scope::Canon;
    let claim_id = |i: usize| seeded.created[&format!("$c{i}")].clone();
    for i in 0..views {
        let s = as_agent(&format!("agent:v{i}"), "r");
        let c = claim_id(i);
        let base = version_of(&r, &canon, &c);
        write_as(
            &mut r,
            &s,
            &format!("v{i}"),
            update(&c, &base, &format!("v{i} edit")),
        )
        .unwrap();
    }
    let hot_claim = claim_id(views);
    let hot_base = version_of(&r, &canon, &hot_claim);
    for j in 0..hot {
        let s = as_agent(&format!("agent:h{j}"), "r");
        write_as(
            &mut r,
            &s,
            &format!("h{j}"),
            update(&hot_claim, &hot_base, &format!("h{j} edit")),
        )
        .unwrap();
    }
    let p = as_agent("agent:p", "r");
    write_as(
        &mut r,
        &p,
        "p",
        update(&hot_claim, &hot_base, "the landing's edit"),
    )
    .unwrap();
    let prop = r.propose(&p, "p", "land", Some(TEST_SUMMARY)).unwrap();
    let info = r.land_report(&approver(), &prop.r#ref).unwrap();
    assert_eq!(
        info.rebased.len(),
        views + hot + 1,
        "the views plus the proposer"
    );
    let conflicted: Vec<&RebasedInfo> = info.rebased.iter().filter(|x| x.conflicts > 0).collect();
    assert_eq!(conflicted.len(), hot, "exactly the hot views conflict");
    assert!(
        conflicted
            .iter()
            .all(|x| x.conflicts == 1 && x.view.starts_with('h'))
    );
    for j in 0..hot {
        assert_eq!(
            r.read()
                .conflicts(&Scope::View(format!("h{j}")))
                .unwrap()
                .len(),
            1
        );
    }
    for i in 0..views {
        assert!(
            r.read()
                .conflicts(&Scope::View(format!("v{i}")))
                .unwrap()
                .is_empty()
        );
    }
    assert!(check_views(&r).is_ok(), "{:?}", check_views(&r));
    check(&r, &Scope::Canon).unwrap();
}

// A kill at every crash point of a landing that rebases views, and of an include into a new
// view, runs in a child process that aborts at the armed point; the parent reopens the store.
const NAME: &str = "vhost1_a_kill_at_any_point_of_a_landing_with_rebases_is_all_or_nothing";

/// Ten views with work, three on the claim the proposal `p` edits; returns the proposal id and
/// the claim.
fn landing_project(dir: &Path) -> String {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let (_doc, section, c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let base = version_of(&r, &Scope::Canon, &c);
    for i in 0..3 {
        let s = as_agent(&format!("agent:t{i}"), "r");
        write_as(
            &mut r,
            &s,
            &format!("t{i}"),
            update(&c, &base, &format!("t{i} edit")),
        )
        .unwrap();
    }
    for i in 0..7 {
        let s = as_agent(&format!("agent:u{i}"), "r");
        write_as(
            &mut r,
            &s,
            &format!("u{i}"),
            claim(&section, &format!("u{i}")),
        )
        .unwrap();
    }
    let p = as_agent("agent:p", "r");
    write_as(&mut r, &p, "p", update(&c, &base, "the landing's edit")).unwrap();
    r.propose(&p, "p", "land", Some(TEST_SUMMARY))
        .unwrap()
        .r#ref
}

/// A project with a change in view `src`; returns the change.
fn include_project(dir: &Path) -> String {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "src", claim(&section, "x"))
        .unwrap()
        .change
}

/// The child's exit code when the armed crash point was never reached and the operation finished.
const FINISHED: i32 = 3;

/// Whether the armed point is on the path of a landing or an include; the others are never
/// reached, so the child finishes (exit code `FINISHED`) instead of aborting.
fn reached(point: CrashPoint) -> bool {
    matches!(
        point,
        CrashPoint::BeforeMarker
            | CrashPoint::BeforeSync
            | CrashPoint::AfterSync
            | CrashPoint::AfterIndex
    )
}

/// The child ended as the point demands: killed by the abort signal where the point is on the
/// operation's path, or exiting `FINISHED` where it is not. Any other end (a panic, an exit 0
/// from a filter that matched nothing) is the child failing before it acted.
fn ended_as_expected(point: CrashPoint, status: &std::process::ExitStatus) -> bool {
    use std::os::unix::process::ExitStatusExt;
    if reached(point) {
        status.signal().is_some()
    } else {
        status.code() == Some(FINISHED)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Included {
    Absent,
    EmptyOwned,
    Holds,
}

/// What a kill (or a finish) at `point` leaves of an include into a new view, which is two
/// batches: the creation, then the include.
fn include_outcome(point: CrashPoint) -> Included {
    match point {
        CrashPoint::BeforeMarker => Included::Absent,
        CrashPoint::BeforeSync | CrashPoint::AfterSync | CrashPoint::AfterIndex => {
            Included::EmptyOwned
        }
        _ => Included::Holds,
    }
}

fn child(dir: &Path, mode: &str, point: CrashPoint, arg: &str) -> std::process::ExitStatus {
    std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &rdy_qa::test_name!(NAME),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("RDY_VH_DIR", dir)
        .env("RDY_VH_MODE", mode)
        .env("RDY_VH_POINT", point.name())
        .env("RDY_VH_ARG", arg)
        .status()
        .unwrap()
}

#[test]
fn vhost1_a_kill_at_any_point_of_a_landing_with_rebases_is_all_or_nothing() {
    if let Ok(dir) = std::env::var("RDY_VH_DIR") {
        let point = CrashPoint::parse(&std::env::var("RDY_VH_POINT").unwrap()).unwrap();
        let arg = std::env::var("RDY_VH_ARG").unwrap();
        let mut r = Rdy::open(Path::new(&dir)).unwrap();
        r.store_mut().crash_at(point);
        if std::env::var("RDY_VH_MODE").unwrap() == "land" {
            let _ = r.land_report(&approver(), &arg);
        } else {
            let _ = r.view_include(&as_agent("agent:b", "r2"), "fresh", &[arg]);
        }
        // a crash point this operation does not reach: it finished, which is also a valid end
        std::process::exit(FINISHED);
    }
    for point in CrashPoint::ALL {
        // the landing
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("p");
        std::fs::create_dir_all(&dir).unwrap();
        let pid = landing_project(&dir);
        let before: Vec<(String, rdy_store::ViewHead)> = {
            let r = Rdy::open(&dir).unwrap();
            let st = r.read();
            st.view_names()
                .into_iter()
                .map(|n| {
                    let h = st.view_head(&n).unwrap().clone();
                    (n, h)
                })
                .collect()
        };
        let canon = Rdy::open(&dir).unwrap().read().canon_head();
        let status = child(&dir, "land", point, &pid);
        assert!(
            ended_as_expected(point, &status),
            "{point:?}: land child ended {status:?}"
        );
        let r = Rdy::open(&dir).unwrap();
        r.fsck().unwrap_or_else(|e| panic!("{point:?}: {e}"));
        let st = r.read();
        let moved = st.canon_head() != canon;
        assert_eq!(moved, point != CrashPoint::BeforeMarker, "{point:?}");
        if moved {
            let landing = st.canon_head().unwrap();
            for (name, old) in &before {
                if matches!(name.as_str(), "seed") {
                    continue;
                }
                let head = st.view_head(name).unwrap();
                let op = r.store().view_op(&head.head).unwrap();
                let ViewOpKind::Rebase {
                    landing: l,
                    conflicts,
                    ..
                } = op.op
                else {
                    panic!("{point:?}: view {name}'s head is not its Rebase ({old:?})");
                };
                assert_eq!(l, landing, "{point:?}: {name}");
                assert_eq!(head.base, Some(landing), "{point:?}: {name}");
                let want = usize::from(name.starts_with('t'));
                assert_eq!(conflicts.len(), want, "{point:?}: {name}");
            }
        } else {
            for (name, old) in &before {
                assert_eq!(st.view_head(name).unwrap(), old, "{point:?}: {name} moved");
            }
        }
        check(&r, &Scope::Canon).unwrap_or_else(|e| panic!("{point:?}: {e:?}"));
        drop(st);
        let mut r = r;
        if !moved {
            // the store recovers: the same landing now goes through and rebases the views
            r.land_report(&approver(), &pid)
                .unwrap_or_else(|e| panic!("{point:?}: re-landing: {e:?}"));
            assert_ne!(r.read().canon_head(), canon, "{point:?}");
            r.fsck().unwrap_or_else(|e| panic!("{point:?}: {e}"));
            check(&r, &Scope::Canon).unwrap_or_else(|e| panic!("{point:?}: {e:?}"));
        }
        drop(r);
        drop(base);

        // the include into a new view
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("p");
        std::fs::create_dir_all(&dir).unwrap();
        let change = include_project(&dir);
        let status = child(&dir, "include", point, &change);
        assert!(
            ended_as_expected(point, &status),
            "{point:?}: include child ended {status:?}"
        );
        let mut r = Rdy::open(&dir).unwrap();
        r.fsck().unwrap_or_else(|e| panic!("{point:?}: {e}"));
        let owner = Owner {
            actor: "agent:b".into(),
            run: Some("r2".into()),
        };
        let (head, included): (Option<rdy_store::ViewHead>, Vec<String>) = {
            let st = r.read();
            (
                st.view_head("fresh").cloned(),
                st.included("fresh").iter().map(|h| h.to_string()).collect(),
            )
        };
        match include_outcome(point) {
            Included::Absent => {
                assert!(head.is_none(), "{point:?}: the view should be absent");
            }
            Included::EmptyOwned => {
                let head = head.unwrap_or_else(|| panic!("{point:?}: the view should exist"));
                assert!(included.is_empty(), "{point:?}: {included:?}");
                assert_eq!(
                    head.owner,
                    Some(owner.clone()),
                    "{point:?}: an empty view is owned"
                );
            }
            Included::Holds => {
                let head = head.unwrap_or_else(|| panic!("{point:?}: the view should exist"));
                assert_eq!(included, vec![change.clone()], "{point:?}");
                assert_eq!(head.owner, Some(owner.clone()), "{point:?}");
            }
        }
        check(&r, &Scope::Canon).unwrap_or_else(|e| panic!("{point:?}: {e:?}"));
        if include_outcome(point) != Included::Holds {
            // the store recovers: the same include now goes through
            r.invalidate();
            r.view_include(
                &as_agent("agent:b", "r2"),
                "fresh",
                std::slice::from_ref(&change),
            )
            .unwrap_or_else(|e| panic!("{point:?}: re-including: {e:?}"));
            let inc: Vec<String> = r
                .read()
                .included("fresh")
                .iter()
                .map(|h| h.to_string())
                .collect();
            assert_eq!(inc, vec![change.clone()], "{point:?}");
            r.fsck().unwrap_or_else(|e| panic!("{point:?}: {e}"));
            check(&r, &Scope::Canon).unwrap_or_else(|e| panic!("{point:?}: {e:?}"));
        }
    }
}

#[test]
fn vhost1_view_list_with_500_views() {
    let (_d, mut r) = project();
    let s = as_agent("agent:a", "r1");
    let mut names: Vec<String> = (0..500).map(|i| format!("v{i:03}")).collect();
    for n in &names {
        r.view_create(&s, n, None, false).unwrap();
    }
    names.sort();
    let all = r.read().view_list(true, 14, now()).unwrap();
    assert_eq!(
        all.iter().map(|v| v.name.clone()).collect::<Vec<_>>(),
        names
    );
    let closing: Vec<String> = names.iter().step_by(2).cloned().collect();
    assert_eq!(closing.len(), 250);
    r.view_close(&s, &closing, "done").unwrap();
    let open = r.read().view_list(false, 14, now()).unwrap();
    assert_eq!(open.len(), 250);
    assert!(open.iter().all(|v| !closing.contains(&v.name)));
    assert_eq!(r.read().view_list(true, 14, now()).unwrap().len(), 500);
    check(&r, &Scope::Canon).unwrap();
}

/// Final review fix round 1 (ledger minor): a bad change, op or proposal id comes back clipped
/// to its first 80 characters and an ellipsis, never verbatim (a 16 MB MCP request's id would
/// otherwise fill the error), and clipping a multi-byte id never splits a character.
#[test]
fn vhost1_a_huge_bad_id_comes_back_clipped_in_the_error() {
    let (_d, mut r) = project();
    let (_doc, section, _c) = seed_doc(&mut r, "seed");
    land(&mut r, "seed").unwrap();
    let a = as_agent("agent:a", "r1");
    write_as(&mut r, &a, "a", claim(&section, "x")).unwrap();
    let huge = format!("{}{}", "\u{e9}".repeat(79), "z".repeat(1 << 20));
    let one = std::slice::from_ref(&huge);
    let errs = vec![
        ("include", r.view_include(&a, "a", one).unwrap_err()),
        ("exclude", r.view_exclude(&a, "a", one, false).unwrap_err()),
        (
            "propose",
            r.propose_subset(&a, "a", "t", Some(one), None).unwrap_err(),
        ),
        ("undo", r.undo(&a, "a", Some(&huge), false).unwrap_err()),
        ("approve", r.resolve_proposal(&huge).unwrap_err()),
    ];
    let clipped = format!("{}z\u{2026}", "\u{e9}".repeat(79));
    for (what, e) in errs {
        assert_eq!(e.code, E_BAD_INPUT, "{what}: {}", e.code);
        assert!(
            e.message.len() < 400 && e.message.contains(&clipped),
            "{what}: {} bytes: {}",
            e.message.len(),
            e.message.chars().take(200).collect::<String>()
        );
    }
}
