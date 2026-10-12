//! ib1: the inbox/ ref: one session's cursor, areas and role.
use rdy_store::inbox::*;
use rdy_store::segment::Location;
use rdy_store::{Store, StoreError};

fn fresh() -> (tempfile::TempDir, Store) {
    let d = tempfile::tempdir().unwrap();
    let s = Store::create(&d.path().join("s")).unwrap();
    (d, s)
}

fn reopen(dir: &tempfile::TempDir) -> Store {
    Store::open(&dir.path().join("s")).unwrap()
}

fn sub(actor: &str, run: Option<&str>) -> InboxSub {
    InboxSub {
        actor: actor.into(),
        run: run.map(str::to_string),
        cursor: InboxCursor {
            at: Location {
                segment: 0,
                offset: 0,
            },
            skip: 0,
        },
        areas: vec!["payments".into()],
        role: "writer".into(),
    }
}

#[test]
fn ib1_the_ref_name_splits_at_the_last_slash() {
    assert_eq!(
        inbox_ref("agent:acme/a1", Some("r7")),
        "inbox/agent:acme/a1/r7"
    );
    assert_eq!(inbox_ref("human:nick", Some("cli")), "inbox/human:nick/cli");
    assert_eq!(inbox_ref("agent:x", None), "inbox/agent:x/");
}

#[test]
fn ib1_put_and_read_back_survive_a_reopen_and_a_reindex() {
    let (dir, mut st) = fresh();
    let mut s = sub("agent:a", Some("r1"));
    s.areas = vec!["identity".into(), "payments".into()];
    st.put_inbox(&s).unwrap();
    assert_eq!(
        st.reader().inbox("agent:a", Some("r1")).unwrap(),
        Some(s.clone())
    );
    assert_eq!(st.reader().inbox("agent:a", Some("r2")).unwrap(), None);
    st.reindex().unwrap();
    assert_eq!(
        st.reader().inbox("agent:a", Some("r1")).unwrap(),
        Some(s.clone())
    );
    drop(st);
    let st = reopen(&dir);
    assert_eq!(st.reader().inboxes().unwrap(), vec![s]);
    assert_eq!(st.check_objects().unwrap().inboxes, 1);
}

#[test]
fn ib1_an_invalid_value_is_refused() {
    let (_dir, mut st) = fresh();
    let mut bad = sub("agent:a", Some("r1"));
    bad.role = "approver".into();
    let e = st.put_inbox(&bad).unwrap_err().to_string();
    assert!(
        e.contains("role \"approver\" is not reader, writer or reviewer"),
        "{e}"
    );
    let mut bad = sub("agent:a", Some("r1"));
    bad.areas = vec!["b".into(), "a".into()];
    assert!(
        st.put_inbox(&bad)
            .unwrap_err()
            .to_string()
            .contains("its lists are not sorted and distinct")
    );
}

#[test]
fn ib1_set_refs_cannot_write_an_inbox() {
    let (_dir, mut st) = fresh();
    let e = st
        .set_refs(vec![("inbox/agent:a/r1".into(), vec![1, 2])])
        .unwrap_err()
        .to_string();
    assert!(e.contains("is an inbox: only put_inbox writes one"), "{e}");
}

#[test]
fn ib1_fsck_refuses_an_inbox_that_does_not_decode() {
    let (_dir, mut st) = fresh();
    st.set_refs_unchecked_for_tests(vec![("inbox/agent:a/r1".into(), vec![0xff; 3])])
        .unwrap();
    let e = st.check_objects().unwrap_err().to_string();
    assert!(
        e.contains("inbox ref \"inbox/agent:a/r1\": its value does not decode"),
        "{e}"
    );
}

/// A refused value is a refusal, not damage: `put_inbox` fails with `StoreError::Refused` and
/// the exact text `inbox ref {name:?}: {what}`, and writes nothing (fix round 1, finding 3).
#[test]
fn ib1_a_refused_value_is_a_refusal_not_damage() {
    let (_dir, mut st) = fresh();
    let mut bad = sub("agent:a", Some("r1"));
    bad.role = "approver".into();
    match st.put_inbox(&bad) {
        Err(StoreError::Refused(m)) => assert_eq!(
            m,
            "inbox ref \"inbox/agent:a/r1\": role \"approver\" is not reader, writer or reviewer"
        ),
        other => panic!("expected StoreError::Refused, got {other:?}"),
    }
    assert_eq!(st.reader().inboxes().unwrap(), vec![]);
}

/// The actor is not empty, and a run, when present, is not empty and holds no `/`: so a ref name
/// names one session only (ruling task-1 §2). `put_inbox` refuses each shape.
#[test]
fn ib1_an_empty_actor_or_a_run_that_is_empty_or_holds_a_slash_is_refused() {
    let (_dir, mut st) = fresh();
    for (actor, run, name, what) in [
        (
            "agent:a",
            Some(""),
            "inbox/agent:a/",
            "its run is empty or holds '/'",
        ),
        (
            "a",
            Some("b/c"),
            "inbox/a/b/c",
            "its run is empty or holds '/'",
        ),
        ("", Some("r1"), "inbox//r1", "its actor is empty"),
    ] {
        match st.put_inbox(&sub(actor, run)) {
            Err(StoreError::Refused(m)) => assert_eq!(m, format!("inbox ref {name:?}: {what}")),
            other => panic!("{actor:?} {run:?}: expected StoreError::Refused, got {other:?}"),
        }
    }
    assert_eq!(st.reader().inboxes().unwrap(), vec![]);
}

/// fsck applies the same rules to a hand-written ref and reports each shape.
#[test]
fn ib1_fsck_reports_an_empty_actor_or_a_run_that_is_empty_or_holds_a_slash() {
    for (actor, run, name, what) in [
        (
            "agent:a",
            Some(""),
            "inbox/agent:a/",
            "its run is empty or holds '/'",
        ),
        (
            "a",
            Some("b/c"),
            "inbox/a/b/c",
            "its run is empty or holds '/'",
        ),
        ("", Some("r1"), "inbox//r1", "its actor is empty"),
    ] {
        let (_dir, mut st) = fresh();
        let bytes = postcard::to_allocvec(&sub(actor, run)).unwrap();
        st.set_refs_unchecked_for_tests(vec![(name.into(), bytes)])
            .unwrap();
        let e = st.check_objects().unwrap_err().to_string();
        assert!(e.contains(&format!("inbox ref {name:?}: {what}")), "{e}");
    }
}

/// The value holds the session, its cursor, areas and role only: the inbox reads tension and
/// suspect moves from their own batches (founder decision C, 2026-10-12), so an ack stays small.
#[test]
fn ib1_the_value_holds_cursor_areas_and_role_only() {
    let (_dir, mut st) = fresh();
    let s = InboxSub {
        actor: "agent:a".into(),
        run: Some("r1".into()),
        cursor: InboxCursor {
            at: Location {
                segment: 0,
                offset: 0,
            },
            skip: 0,
        },
        areas: vec!["payments".into()],
        role: "writer".into(),
    };
    st.put_inbox(&s).unwrap();
    assert_eq!(
        st.reader().inbox("agent:a", Some("r1")).unwrap(),
        Some(s.clone())
    );
    let len = postcard::to_allocvec(&s).unwrap().len();
    assert!(len < 64, "{len} bytes");
}
