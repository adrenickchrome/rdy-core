//! kasg1: rdy keys assign gives every keyless doc and folder its derived key in one change,
//! leaves given keys alone, avoids keys already held, and needs no prior briefing on stable docs.
use rdy_engine::{E_BAD_INPUT, Rdy, Scope};
use rdy_model::BlockId;
use rdy_qa::fixtures::*;

fn keyless(r: &mut Rdy, extra: Vec<rdy_model::Block>) {
    let mut blocks = vec![
        raw(
            "d-AAAAA",
            "doc",
            None,
            "a1",
            &[
                ("name", "notes"),
                ("title", "Notes"),
                ("doc_type", "overview"),
                ("lifecycle", "stable"),
            ],
            "",
        ),
        raw(
            "fd-AAAAA",
            "folder",
            None,
            "a2",
            &[("name", "guides"), ("title", "Guides")],
            "",
        ),
        raw(
            "d-BBBBB",
            "doc",
            Some("fd-AAAAA"),
            "a1",
            &[
                ("name", "intro"),
                ("title", "Intro"),
                ("doc_type", "overview"),
            ],
            "",
        ),
        raw(
            "d-CCCCC",
            "doc",
            None,
            "a3",
            &[
                ("name", "kept"),
                ("title", "Kept"),
                ("doc_type", "overview"),
                ("key", "KEEP-1"),
            ],
            "",
        ),
    ];
    blocks.extend(extra);
    raw_commit(r, "work", blocks);
}

fn key(r: &Rdy, id: &str) -> Option<String> {
    r.read()
        .block(&Scope::View("work".into()), &BlockId::parse(id).unwrap())
        .unwrap()
        .field_str("key")
        .map(String::from)
}

#[test]
fn kasg1_assign_gives_every_keyless_doc_and_folder_a_key_in_one_change() {
    let (_d, mut r) = project();
    keyless(&mut r, vec![]);
    let w = r.assign_keys(&writer(), "work").unwrap();
    let got: Vec<(&str, &str)> = w
        .keys
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("d-AAAAA", "notes"),
            ("d-BBBBB", "guides.intro"),
            ("fd-AAAAA", "guides")
        ]
    );
    assert_eq!(key(&r, "d-CCCCC").as_deref(), Some("KEEP-1"));
    assert_eq!(key(&r, "fd-AAAAA").as_deref(), Some("guides"));
    let e = r.assign_keys(&writer(), "work").unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BAD_INPUT, "every doc and folder in view work has a key")
    );
}

#[test]
fn kasg1_assign_avoids_keys_already_held() {
    let (_d, mut r) = project();
    keyless(
        &mut r,
        vec![raw(
            "c-AAAAA",
            "claim",
            Some("d-CCCCC"),
            "a1",
            &[("key", "NOTES")],
            "A claim.",
        )],
    );
    r.assign_keys(&writer(), "work").unwrap();
    assert_eq!(key(&r, "d-AAAAA").as_deref(), Some("notes-2"));
}

#[test]
fn kasg1_assigned_keys_are_marked_as_rdys_own() {
    let (_d, mut r) = project();
    keyless(&mut r, vec![]);
    r.assign_keys(&writer(), "work").unwrap();
    for id in ["d-AAAAA", "d-BBBBB", "fd-AAAAA"] {
        let b = r
            .read()
            .block(&Scope::View("work".into()), &BlockId::parse(id).unwrap())
            .unwrap();
        assert_eq!(
            b.fields.get(rdy_model::key::AUTO_FIELD),
            Some(&rdy_model::FieldValue::Bool(true)),
            "{id}"
        );
    }
    let kept = r
        .read()
        .block(
            &Scope::View("work".into()),
            &BlockId::parse("d-CCCCC").unwrap(),
        )
        .unwrap();
    assert!(!kept.fields.contains_key(rdy_model::key::AUTO_FIELD));
}
