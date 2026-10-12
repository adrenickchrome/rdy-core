//! ib1: the inbox/ ref: one session's cursor, areas, role and seen sets.
use rdy_model::Hash;
use rdy_store::Store;
use rdy_store::inbox::*;
use rdy_store::segment::Location;

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
        seen_tensions: vec![],
        seen_suspects: vec![],
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
    s.seen_tensions = vec![Hash::of(b"t1"), Hash::of(b"t2")];
    s.seen_tensions.sort();
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
    let mut bad = sub("agent:a", Some("r1"));
    bad.seen_suspects = (0..1001u32).map(|i| Hash::of(&i.to_le_bytes())).collect();
    bad.seen_suspects.sort();
    assert!(
        st.put_inbox(&bad)
            .unwrap_err()
            .to_string()
            .contains("more than 1000 seen entries")
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
