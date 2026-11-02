//! Normal opens and project-file writes refuse a follower (D-AA4FD), and the refusal keeps its
//! code, message and fix through the engine's error and the MCP row.
use rdy_engine::{
    E_DUMP_CHAIN, E_LIMIT, E_STORE_FOLLOWER, Local, Rdy, RdyError, Session, SessionKind,
    WriteRequest, core_error_code,
};
use rdy_model::{RepoId, Role};
use rdy_store::error::FollowerMarkFailure;
use rdy_store::segment::Location;
use rdy_store::{OpenPolicy, StoreError};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn session(role: Role) -> Session {
    Session {
        actor: "human:founder".into(),
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

/// A landed project, closed; its dump; a follower seeded from the dump.
struct Fixture {
    _base: tempfile::TempDir,
    dump: PathBuf,
    follower: PathBuf,
    repo: RepoId,
}

fn fixture() -> Fixture {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let ops =
        json!([{"op": "create", "type": "doc", "fields": {"title": "T", "doc_type": "page"}}]);
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: String::new(),
        },
    )
    .unwrap();
    let p = r
        .propose(&session(Role::Writer), "work", "T", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    let dump = base.path().join("p.rdydump");
    r.read().dump(&dump).unwrap();
    let repo = r.store().repo_id().unwrap().unwrap();
    r.close().unwrap();
    let follower = base.path().join("f");
    rdy_engine::follower::seed(&dump, &follower).unwrap();
    Fixture {
        _base: base,
        dump,
        follower,
        repo,
    }
}

fn tree(dir: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if std::fs::symlink_metadata(&p).unwrap().is_dir() {
            out.insert(p.clone(), None);
            out.extend(tree(&p));
        } else {
            out.insert(p.clone(), Some(std::fs::read(&p).unwrap()));
        }
    }
    out
}

fn triple(e: &RdyError) -> (&str, &str, &str) {
    (e.code, e.message.as_str(), e.fix.as_str())
}

#[test]
fn mh3_normal_opens_and_project_file_writes_refuse_followers() {
    let f = fixture();
    let root = f.follower.as_path();
    let message = format!("this store is a read-only follower of {}", f.repo);
    let want = (
        E_STORE_FOLLOWER,
        message.as_str(),
        "write through the source store",
    );
    // a broken config or pack is not reached: the refusal comes first
    let config = root.join(".rdy.yaml");
    let good = std::fs::read(&config).unwrap();
    std::fs::write(&config, b"pack: [not a pack").unwrap();
    assert_eq!(triple(&Rdy::open(root).err().unwrap()), want, "bad config");
    std::fs::write(&config, &good).unwrap();
    let before = tree(root);
    let cases: Vec<(&str, Result<(), RdyError>)> = vec![
        ("open", Rdy::open(root).map(drop)),
        (
            "auto",
            Rdy::open_with_policy(root, OpenPolicy::AutoUpgrade).map(drop),
        ),
        (
            "current",
            Rdy::open_with_policy(root, OpenPolicy::RequireCurrent).map(drop),
        ),
        ("local", Local::open(root).map(drop)),
        (
            "local current",
            Local::open_with_policy(root, OpenPolicy::RequireCurrent).map(drop),
        ),
        ("holder path", Rdy::open_holder(root).map(drop)),
        ("init", Rdy::init(root, "rdy-standard").map(drop)),
        ("create holder", Rdy::create_holder(root, f.repo).map(drop)),
        (
            "require_project",
            rdy_engine::config_levels::require_project(root),
        ),
        (
            "upgrade plan",
            rdy_engine::maintenance::plan(root).map(drop),
        ),
        (
            "upgrade dry run",
            rdy_engine::maintenance::run(root, true).map(drop),
        ),
        (
            "upgrade",
            rdy_engine::maintenance::run(root, false).map(drop),
        ),
        ("load", rdy_engine::load::load(&f.dump, root).map(drop)),
    ];
    for (what, out) in cases {
        assert_eq!(triple(&out.expect_err(what)), want, "{what}");
        assert_eq!(tree(root), before, "{what}: no side effect");
    }
    // the follower still opens as one
    rdy_engine::follower::FollowerCore::open(root).unwrap();
}

#[test]
fn mh3_error_survives_engine_and_mcp_envelopes() {
    let repo = RepoId([0xab; 16]);
    let hex = "ab".repeat(16);
    let e: RdyError = StoreError::Follower(repo).into();
    assert_eq!(
        triple(&e),
        (
            "E_STORE_FOLLOWER",
            format!("this store is a read-only follower of {hex}").as_str(),
            "write through the source store",
        )
    );
    assert_eq!(core_error_code(e.code), Some("E_STORE_FOLLOWER"));
    // the rows of the mark, in their order, all with one fix
    let fix = "seed a new follower from a full dump of the source store";
    let at = |segment, offset| Location { segment, offset };
    let (a, b) = (RepoId([1; 16]), RepoId([2; 16]));
    let (ha, hb) = ("01".repeat(16), "02".repeat(16));
    for (reason, message) in [
        (
            FollowerMarkFailure::NotPrivate,
            "the follower mark is not a private regular file".to_string(),
        ),
        (
            FollowerMarkFailure::TooLarge,
            "the follower mark exceeds 262144 bytes".into(),
        ),
        (
            FollowerMarkFailure::Unreadable,
            "the follower mark cannot be read".into(),
        ),
        (
            FollowerMarkFailure::BadHeader,
            "the follower mark does not start with \"rdy follower 1\"".into(),
        ),
        (
            FollowerMarkFailure::BadJson,
            "the follower mark is not valid JSON".into(),
        ),
        (
            FollowerMarkFailure::UnknownField,
            "the follower mark has an unknown field".into(),
        ),
        (
            FollowerMarkFailure::BadFiles,
            "the follower mark has an invalid project file list".into(),
        ),
        (
            FollowerMarkFailure::Repo { mark: a, log: b },
            format!("the follower mark names repo {ha}, but the log holds repo {hb}"),
        ),
        (
            FollowerMarkFailure::PastEnd {
                mark: at(3, 40),
                log: at(2, 9),
            },
            "the follower mark ends at 3.40, past the log's end at 2.9".into(),
        ),
        (
            FollowerMarkFailure::Marker,
            "the follower mark's batch marker differs from the log's at its saved end".into(),
        ),
        (
            FollowerMarkFailure::StoreFormat,
            "the follower mark's store format differs from the log's at its saved end".into(),
        ),
        (
            FollowerMarkFailure::Missing,
            "this store has no follower mark".into(),
        ),
    ] {
        let e: RdyError = StoreError::FollowerMark(reason).into();
        assert_eq!(triple(&e), (E_DUMP_CHAIN, message.as_str(), fix));
    }
    let e: RdyError = StoreError::FollowerMarkCheckLimit.into();
    assert_eq!(
        triple(&e),
        (
            E_LIMIT,
            "the follower mark check exceeds 100000000 frames",
            "report this as a bug in rdy"
        )
    );
    // the MCP row sits right before E_STORE_READ_ONLY, with the exact words; the trailing
    // publication rows do not move
    let rows = include_str!("../../rdy-mcp/src/tools/error_rows.rs");
    let row = rows.find("E_STORE_FOLLOWER,").expect("a row");
    let next = rows.find("E_STORE_READ_ONLY,").expect("its neighbour");
    assert!(row < next);
    let between = &rows[row..next];
    assert!(between.contains("\"this store is a read-only follower of <repo id>\""));
    assert!(between.contains("\"write through the source store\""));
    assert_eq!(
        between
            .lines()
            .filter(|l| l.trim_start().starts_with("E_"))
            .count(),
        1,
        "adjacent: {between}"
    );
}
