//! trk1: a pack marks tracking fields per block type; children add to them; the ruling, derived
//! and reserved fields cannot be tracking.
use rdy_schema::{PackSet, SchemaError};

const TRK: &str = "name: trk\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n      disposition: { kind: str }\n      trigger: { kind: str }\n    tracking: [disposition, trigger]\n";

#[test]
fn trk1_a_pack_marks_tracking_fields_and_a_child_adds_to_them() {
    let mut set = PackSet::with_builtin();
    set.add_yaml(TRK).unwrap();
    let p = set.resolve("trk").unwrap();
    let names: Vec<&str> = p.block_types["decision"]
        .tracking
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(names, ["disposition", "trigger"]);
    assert!(p.tracking().is_tracking("decision", "disposition"));
    assert!(p.block_types["claim"].tracking.is_empty());
    set.add_yaml("name: child\nversion: \"1\"\nextends: trk\nblock_types:\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n      owner: { kind: str }\n    tracking: [owner]\n").unwrap();
    let c = set.resolve("child").unwrap();
    let names: Vec<&str> = c.block_types["decision"]
        .tracking
        .iter()
        .map(String::as_str)
        .collect();
    assert_eq!(names, ["disposition", "owner", "trigger"]);
    assert!(
        PackSet::with_builtin()
            .resolve("rdy-standard")
            .unwrap()
            .tracking()
            .is_empty()
    );
}

#[test]
fn trk1_a_tracking_field_must_be_a_field_and_not_the_ruling() {
    let pack = |tracking: &str| {
        format!(
            "name: bad\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  decision:\n    prefix: D\n    traits: [normative]\n    fields:\n      disposition: {{ kind: str }}\n    tracking: [{tracking}]\n"
        )
    };
    for (t, words) in [
        ("nope", "is not a field of the type"),
        ("statement", "is part of the ruling"),
        ("options", "is part of the ruling"),
        ("status", "is part of the ruling"),
        ("decided_at", "is derived or reserved by the engine"),
        ("key", "is derived or reserved by the engine"),
    ] {
        let mut set = PackSet::with_builtin();
        set.add_yaml(&pack(t)).unwrap();
        match set.resolve("bad") {
            Err(SchemaError::Invalid(m)) => assert!(
                m.contains(&format!("tracking field {t} {words}")),
                "{t}: {m}"
            ),
            other => panic!("{t}: {other:?}"),
        }
    }
}
