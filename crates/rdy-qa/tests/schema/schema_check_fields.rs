//! ck1: the optional `confirmation` text on a decision, its closed bounds, and that it is part of
//! the frozen ruling; pack rules refuse at load through the loader's own error.
use rdy_engine::{
    DecisionInput, E_CONFIG, E_FROZEN, E_VALIDATION, Rdy, RdyError, Scope, WriteRequest,
};
use rdy_model::{BlockId, EditorialClass, Justification};
use rdy_qa::fixtures::*;
use rdy_qa::packs::use_pack;
use serde_json::{Value, from_value, json};

const TOO_LONG_OR_BLANK: &str = "confirmation must have 1 to 2000 characters";
const THE_FIX: &str = "say how the team knows the decision is kept, in 1 to 2000 characters";

fn decide_with(r: &mut Rdy, section: &str, confirmation: Value) -> Result<String, RdyError> {
    let res = r.decide(
        &writer(),
        "seed",
        DecisionInput {
            parent: section.to_string(),
            statement: "Ship weekly".into(),
            fields: [("confirmation".to_string(), confirmation)]
                .into_iter()
                .collect(),
            ..Default::default()
        },
    )?;
    Ok(res.created["$decision"].clone())
}

fn assert_bad_text(e: RdyError) {
    assert_eq!(e.code, E_VALIDATION, "{e:?}");
    let issue = e
        .issues
        .iter()
        .find(|i| i.code == "E_FIELD_KIND")
        .unwrap_or_else(|| panic!("no E_FIELD_KIND: {e:?}"));
    assert_eq!(issue.message, TOO_LONG_OR_BLANK);
    assert_eq!(issue.fix, THE_FIX);
}

fn update_confirmation(r: &mut Rdy, dec: &str, reason: &str) -> Result<(), RdyError> {
    r.write(
        &writer(),
        WriteRequest {
            view: "later".into(),
            ops: from_value(json!([
                {"op": "update", "id": dec, "fields": {"confirmation": "A new way to know"}}
            ]))
            .unwrap(),
            justification: Some(Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: reason.into(),
            }),
            message: String::new(),
        },
    )
    .map(|_| ())
}

#[test]
fn ck1_confirmation_is_optional_text_and_not_tracking() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");

    // a decision with none still validates
    r.decide(
        &writer(),
        "seed",
        DecisionInput {
            parent: section.clone(),
            statement: "Keep one store".into(),
            ..Default::default()
        },
    )
    .unwrap();

    // text within the bounds passes, and is stored as written
    let kept = decide_with(&mut r, &section, json!("  The weekly report shows it. ")).unwrap();
    let id = BlockId::parse(&kept).unwrap();
    let scope = Scope::View("seed".into());
    assert_eq!(
        r.read()
            .block(&scope, &id)
            .unwrap()
            .field_str("confirmation"),
        Some("  The weekly report shows it. ")
    );
    for at_limit in [
        "c".repeat(2000),
        "\u{1F600}".repeat(2000),
        // the count is of trimmed characters, and the bytes stay within 8000
        format!(" {} ", "c".repeat(2000)),
        format!("{}{}", "c".repeat(2000), " ".repeat(6000)),
    ] {
        decide_with(&mut r, &section, json!(at_limit)).unwrap();
    }

    // blank text and one past each bound
    for bad in [
        String::new(),
        "  \n\t ".to_string(),
        "c".repeat(2001),
        "\u{1F600}".repeat(2001),
        // 8001 bytes: 2000 trimmed characters, over the byte bound
        format!("{}{}", "c".repeat(2000), " ".repeat(6001)),
    ] {
        assert_bad_text(decide_with(&mut r, &section, json!(bad)).unwrap_err());
    }

    // a value of another kind gets the field check every field has
    for bad in [json!(7), json!(true), json!(["a"]), json!({"a": "b"})] {
        let e = decide_with(&mut r, &section, bad).unwrap_err();
        assert_eq!(e.code, E_VALIDATION, "{e:?}");
        let issue = e.issues.iter().find(|i| i.code == "E_FIELD_KIND").unwrap();
        assert_eq!(issue.message, "field \"confirmation\" must be Str");
        assert_eq!(issue.fix, "write confirmation as a str value");
    }

    // no pack marks it as tracking, and an accepted decision cannot change it
    assert!(
        !r.read()
            .pack()
            .tracking()
            .is_tracking("decision", "confirmation")
    );
    land(&mut r, "seed").unwrap();
    let b = r.read().block(&Scope::Canon, &id).unwrap();
    assert_eq!(b.field_str("status"), Some("accepted"));
    let e = update_confirmation(&mut r, &kept, "reworded").unwrap_err();
    assert_eq!(e.code, E_FROZEN, "{e:?}");
}

#[test]
fn ck1_query_still_reads_the_landing_clock() {
    let (_d, mut r) = project();
    let (_doc, section, _claim) = seed_doc(&mut r, "seed");
    let dec = decide_with(&mut r, &section, json!("The weekly report shows it.")).unwrap();
    land(&mut r, "seed").unwrap();
    let reply = r
        .read()
        .query(&Scope::Canon, "has fields.decided_at")
        .unwrap();
    assert!(
        reply.matches.iter().any(|row| row.id == dec),
        "{:?}",
        reply.matches
    );
}

#[test]
fn ck1_a_bad_rule_stops_the_pack_from_loading() {
    let dir = tempfile::tempdir().unwrap();
    drop(Rdy::init(dir.path(), "rdy-standard").unwrap());
    use_pack(
        dir.path(),
        "bad",
        "name: bad\nversion: \"1\"\nextends: rdy-standard\nrules:\n  - name: clock\n    select: has fields.decided_at\n    require: type = decision\n    message: m\n    fix: f\n",
        "",
    );
    let e = match Rdy::open(dir.path()) {
        Ok(_) => panic!("the pack loaded"),
        Err(e) => e,
    };
    assert_eq!(e.code, E_CONFIG);
    assert_eq!(
        e.message,
        "invalid pack: rule clock select: a rule cannot read fields.decided_at, because the landing sets it"
    );
    assert_eq!(e.fix, "fix the pack file");
}
