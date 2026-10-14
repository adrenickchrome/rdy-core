use rdy_engine::RdyError;
use rdy_model::{BlockId, Hash, Provenance, Role, Tag};
use rdy_store::StoreError;

fn id() -> BlockId {
    BlockId::parse("c-AAAAA").unwrap()
}

#[test]
fn stale_base_with_actual_carries_full_hash() {
    let actual = Hash([7; 32]);
    let e: RdyError = StoreError::StaleBase {
        id: id(),
        expected: Hash([1; 32]),
        actual: Some(actual),
    }
    .into();
    assert_eq!(e.code, "E_STALE_BASE");
    assert!(e.fix.contains(&"07".repeat(32)));
}

#[test]
fn stale_base_deleted_mentions_deleted() {
    let e: RdyError = StoreError::StaleBase {
        id: id(),
        expected: Hash([1; 32]),
        actual: None,
    }
    .into();
    assert_eq!(e.code, "E_STALE_BASE");
    assert!(e.fix.contains("deleted"));
    assert!(e.message.contains("deleted"));
}

#[test]
fn other_mappings() {
    assert_eq!(
        RdyError::from(StoreError::UnknownView("w".into())).code,
        "E_NOT_FOUND"
    );
    assert_eq!(
        RdyError::from(StoreError::Conflict { id: id() }).code,
        "E_STALE_BASE"
    );
    assert_eq!(RdyError::from(StoreError::Frozen(id())).code, "E_FROZEN");
    assert_eq!(
        RdyError::from(StoreError::Io(std::io::Error::other("x"))).code,
        "E_IO"
    );
}

#[test]
fn damaged_log_hint_does_not_point_at_commands_that_need_an_open_store() {
    let e = RdyError::from(StoreError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "corruption at offset 0 ...",
    )));
    assert_eq!(e.code, "E_IO");
    assert!(!e.fix.contains("rdy fsck"), "{}", e.fix);
    assert!(e.fix.contains("message"), "{}", e.fix);
}

#[test]
fn a_malformed_canon_ref_is_an_io_error_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let mut r = rdy_engine::Rdy::init(dir.path(), "rdy-standard").unwrap();
    r.store_mut()
        .set_refs(vec![("canon".into(), b"not a hash".to_vec())])
        .unwrap();
    // a store write outside the engine is followed by invalidate (snap1)
    r.invalidate();
    let e = r
        .read()
        .materialized(&rdy_engine::Scope::Canon)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(e.code, "E_IO", "{e}");
    assert!(e.message.contains("canon ref"), "{e}");
    assert_eq!(r.fsck().unwrap_err().code, "E_IO");
}

fn fsck_project(n: usize) -> (tempfile::TempDir, rdy_engine::Rdy, Vec<Hash>) {
    use rdy_engine::*;
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = Session {
        actor: "agent:a".into(),
        run: None,
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    };
    let a = Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    };
    let mut out = vec![];
    for i in 0..n {
        r.write(&w, WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(serde_json::json!([
                {"op": "create", "type": "doc", "fields": {"title": format!("D{i}"), "doc_type": "page"}}
            ])).unwrap(),
            justification: None,
            message: String::new(),
        }).unwrap();
        let p = r.propose(&w, "work", "p").unwrap();
        out.push(r.land(&a, &p.id).unwrap().parse().unwrap());
    }
    (dir, r, out)
}

fn raw_tag(name: &str, landing: Hash) -> Tag {
    Tag {
        name: name.into(),
        landing,
        message: None,
        tagged_by: Provenance {
            actor: "x".into(),
            run: None,
            role: Role::Approver,
            at_ms: 0,
        },
    }
}

/// The raw writes above join the open batch (plan M2e2): it is sealed before fsck reads the log.
fn fsck_message(r: &mut rdy_engine::Rdy) -> String {
    r.seal_open().unwrap();
    let e = r.fsck().unwrap_err();
    assert_eq!(e.code, "E_IO", "{e}");
    e.message
}

#[test]
fn fsck_counts_valid_versions() {
    let (_d, mut r, _) = fsck_project(1);
    let a = rdy_engine::Session {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        kind: rdy_engine::SessionKind::Human,
    };
    r.tag(&a, "v0.1.0", None, None).unwrap();
    assert_eq!(r.fsck().unwrap().versions, 1);
}

#[test]
fn fsck_reports_damaged_and_dangling_tag_refs() {
    let (_d, mut r, l) = fsck_project(1);
    r.store_mut()
        .set_refs(vec![("tag/v1.0.0".into(), b"short".to_vec())])
        .unwrap();
    assert!(fsck_message(&mut r).contains("tag/v1.0.0"));
    r.store_mut()
        .set_refs(vec![("tag/v1.0.0".into(), [9u8; 32].to_vec())])
        .unwrap();
    assert!(fsck_message(&mut r).contains("not in the store"));
    r.store_mut()
        .set_refs(vec![("tag/v1.0.0".into(), l[0].0.to_vec())])
        .unwrap();
    assert!(fsck_message(&mut r).contains("not a tag"));
}

type TagCase = Box<dyn Fn(&[Hash]) -> Vec<Tag>>;

#[test]
fn fsck_reports_tags_written_around_the_rules() {
    let cases: Vec<(&str, TagCase)> = vec![
        (
            "is not a valid version",
            Box::new(|l| vec![raw_tag("release-1", l[0])]),
        ),
        (
            "not in canon's chain",
            Box::new(|_| vec![raw_tag("v1.0.0", Hash([1; 32]))]),
        ),
        (
            "same version",
            Box::new(|l| vec![raw_tag("v1.0.0", l[0]), raw_tag("1.0.0", l[1])]),
        ),
        (
            "earlier landing",
            Box::new(|l| vec![raw_tag("v2.0.0", l[0]), raw_tag("v1.0.0", l[1])]),
        ),
    ];
    for (needle, tags) in cases {
        let (_d, mut r, l) = fsck_project(2);
        for t in tags(&l) {
            r.store_mut().put_tag(&t).unwrap();
        }
        let m = fsck_message(&mut r);
        assert!(m.contains(needle), "{needle}: {m}");
    }
    // a ref whose name doesn't match its tag
    let (_d, mut r, l) = fsck_project(1);
    let h = r.store_mut().put_tag(&raw_tag("v1.0.0", l[0])).unwrap();
    r.store_mut()
        .set_refs(vec![("tag/v9.9.9".into(), h.0.to_vec())])
        .unwrap();
    let m = fsck_message(&mut r);
    assert!(m.contains("tag/v9.9.9"));
    assert!(
        m.starts_with("1 version problem(s)"),
        "one bad ref, one message: {m}"
    );
}
