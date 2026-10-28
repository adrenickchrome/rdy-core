//! rg7 (D-ME2HD check 1; O1, O2): the key change a propose adds (K1) is written after the
//! propose checks, in its own batch, with a briefing of the blocks it re-keys. The version and
//! its plan follow in the next batch and list the key change among their changes.
use rdy_engine::plan::{PlanAnswerInput, ProposePlanRequest};
use rdy_engine::{E_PLAN_ITEM, ProposalInfo, Rdy, RdyError};
use rdy_model::plan::PlanAct;
use rdy_model::{BlockId, Change, FieldValue, Hash, key};
use rdy_qa::fixtures::{approver, project, writer};
use rdy_qa::intake::TEST_SUMMARY;
use rdy_store::kinds;
use rdy_store::segment::Location;

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

/// Canon holds a doc whose derived key the doc created in view `b` also derives (both docs are
/// created before either lands); view `b` holds that doc as its first change.
fn clash(r: &mut Rdy) {
    r.new_doc(
        &writer(),
        "a",
        "engineering-handbook/onboarding/overview",
        None,
        Some("O"),
    )
    .unwrap();
    r.new_doc(
        &writer(),
        "b",
        "product-handbook/onboarding/overview",
        None,
        Some("O"),
    )
    .unwrap();
    let pa = r.propose(&writer(), "a", "a", Some(TEST_SUMMARY)).unwrap();
    r.land(&approver(), &pa.id).unwrap();
}

fn propose(r: &mut Rdy, view: &str, changes: Option<&[String]>) -> Result<ProposalInfo, RdyError> {
    r.propose_plan(
        &writer(),
        ProposePlanRequest {
            changes,
            summary: Some(TEST_SUMMARY),
            ..ProposePlanRequest::new(view, view)
        },
    )
}

fn kinds_from(r: &Rdy, from: Location) -> Vec<u8> {
    let mut out = vec![];
    r.store()
        .reader()
        .for_each_frame_from(from, r.store().log_end(), |_, kind, _, _, _| {
            out.push(rdy_store::codec::logical_kind(kind));
            Ok(())
        })
        .unwrap();
    out
}

fn commits(kinds_of: &[u8]) -> usize {
    kinds_of
        .iter()
        .filter(|k| **k == kinds::BATCH_COMMIT)
        .count()
}

/// The changes of the stored version `p`, and the key change, which is the last of them.
fn version_changes(r: &Rdy, p: &ProposalInfo) -> (Vec<Hash>, Change) {
    let st = r.read();
    let stored = st.reader().proposal(&h(&p.id)).unwrap();
    let last = *stored.changes.last().unwrap();
    let change = st.reader().change(&last).unwrap();
    (stored.changes, change)
}

#[test]
fn rg7_a_key_clash_commits_the_key_change_before_the_version() {
    let (_d, mut r) = project();
    clash(&mut r);
    let in_view = r.store().included("b").unwrap().len();
    let start = r.store().log_end();
    let p = propose(&mut r, "b", None).unwrap();
    assert_eq!(p.keys.len(), 1, "{:?}", p.keys);
    assert_eq!(p.changes, in_view + 1);
    let kinds_of = kinds_from(&r, start);
    assert_eq!(commits(&kinds_of), 2, "{kinds_of:?}");
    let first = kinds_of
        .iter()
        .position(|k| *k == kinds::BATCH_COMMIT)
        .unwrap();
    let at = |k: u8| kinds_of.iter().position(|x| *x == k).unwrap();
    assert!(at(kinds::CHANGE) < first, "{kinds_of:?}");
    assert!(
        first < at(kinds::PROPOSAL) && first < at(kinds::PLAN),
        "{kinds_of:?}"
    );
    let (changes, key_change) = version_changes(&r, &p);
    assert_eq!(
        changes.len(),
        in_view + 1,
        "the version lists the key change"
    );
    assert!(
        key_change.message.starts_with("new keys rdy derived"),
        "{}",
        key_change.message
    );
    assert_eq!(
        r.store().included("b").unwrap().last(),
        changes.last(),
        "the key change is the view's newest"
    );
    r.fsck().unwrap();
}

#[test]
fn rg7_the_key_change_records_a_briefing() {
    let (_d, mut r) = project();
    clash(&mut r);
    let p = propose(&mut r, "b", None).unwrap();
    let (_, key_change) = version_changes(&r, &p);
    assert!(
        key_change.links.briefing.is_some(),
        "the briefing is recorded"
    );
    let st = r.read();
    let m = st
        .materialized(&rdy_engine::Scope::View("b".into()))
        .unwrap();
    for id in p.keys.keys() {
        let b = m.get(&BlockId::parse(id).unwrap()).unwrap();
        assert_eq!(
            b.fields.get(key::AUTO_FIELD),
            Some(&FieldValue::Bool(true)),
            "{id} carries key_auto"
        );
        assert_eq!(b.field_str("key"), p.keys.get(id).map(String::as_str));
    }
}

/// Guard: a refused plan answer is refused after the checks and before the key change.
#[test]
fn rg7_a_refused_plan_answer_writes_no_key_change() {
    let (_d, mut r) = project();
    clash(&mut r);
    let before = (
        r.store().log_end(),
        r.read().reader().refs_with_prefix("").unwrap(),
    );
    let in_view = r.store().included("b").unwrap().len();
    let answers = [PlanAnswerInput {
        item: "pi-0123456789abcdef".into(),
        act: PlanAct::Defer,
        reason: Some("later".into()),
    }];
    let e = r
        .propose_plan(
            &writer(),
            ProposePlanRequest {
                plan_answers: &answers,
                summary: Some(TEST_SUMMARY),
                ..ProposePlanRequest::new("b", "b")
            },
        )
        .unwrap_err();
    assert_eq!(e.code, E_PLAN_ITEM, "{e}");
    let after = (
        r.store().log_end(),
        r.read().reader().refs_with_prefix("").unwrap(),
    );
    assert_eq!(after, before, "the log end does not move");
    assert_eq!(r.store().included("b").unwrap().len(), in_view);
}

#[test]
fn rg7_a_subset_propose_with_a_clash_lists_only_its_changes_and_the_key_change() {
    let (_d, mut r) = project();
    clash(&mut r);
    r.new_doc(&writer(), "b", "misc/one", None, Some("One"))
        .unwrap();
    r.new_doc(&writer(), "b", "misc/two", None, Some("Two"))
        .unwrap();
    let included = r.store().included("b").unwrap();
    assert_eq!(included.len(), 3);
    let first = included[0];
    let p = propose(&mut r, "b", Some(&[first.to_string()])).unwrap();
    assert_eq!(p.keys.len(), 1, "{:?}", p.keys);
    assert_eq!(p.changes, 2);
    let (changes, key_change) = version_changes(&r, &p);
    assert_eq!(changes.len(), 2, "{changes:?}");
    assert_eq!(changes[0], first);
    assert!(
        key_change.message.starts_with("new keys rdy derived"),
        "{}",
        key_change.message
    );
    assert!(
        !changes.contains(&included[1]) && !changes.contains(&included[2]),
        "the other two changes are not in the version"
    );
    r.fsck().unwrap();
}

/// Guard: a version with no clash is written in one batch.
#[test]
fn rg7_a_version_with_no_clash_is_one_batch() {
    let (_d, mut r) = project();
    r.new_doc(
        &writer(),
        "b",
        "product-handbook/onboarding/overview",
        None,
        Some("O"),
    )
    .unwrap();
    let start = r.store().log_end();
    let p = propose(&mut r, "b", None).unwrap();
    assert!(p.keys.is_empty(), "{:?}", p.keys);
    let kinds_of = kinds_from(&r, start);
    assert_eq!(commits(&kinds_of), 1, "{kinds_of:?}");
    assert_eq!(
        kinds_of.iter().filter(|k| **k == kinds::CHANGE).count(),
        0,
        "{kinds_of:?}"
    );
}
