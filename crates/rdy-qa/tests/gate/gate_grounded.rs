//! mi1 (D-KVQ7F, D-EYRGM): the assessment method is 2. A stored method-1 record is read as stored:
//! its bytes, hash and approve card stay through reopen, reindex, dump and load, and it is not
//! refused for its method. A new proposal computes coverage with the new rule.
use rdy_engine::load::load;
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, ItemKind, PendingQuery};
use rdy_engine::readiness::assessment::METHOD;
use rdy_engine::{ProposalInfo, Rdy};
use rdy_model::assessment::{Assessment, AssessmentIssue};
use rdy_model::{BlockId, Hash};
use rdy_qa::fixtures::{approver, land, writer};
use rdy_qa::intake::{PERSON, TEST_SUMMARY};
use rdy_qa::tensions::{j_typo, put_j, upd};
use serde_json::json;

struct World {
    _dir: tempfile::TempDir,
    r: Rdy,
    x: BlockId,
    p: BlockId,
}

/// Canon: a term Ledger, a claim `x` and a prose block `p`, all landed.
fn world() -> World {
    let (dir, mut r) = rdy_qa::fixtures::project();
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$t", "parent": "$n", "fields": {"title": "Ideas"}},
            {"op": "create", "type": "term", "id": "$l", "parent": "$t", "fields": {"name": "Ledger"}, "body": "Holds balances."},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$t", "body": "X holds."},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$t", "body": "A small note."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    World {
        x: id("$x"),
        p: id("$p"),
        _dir: dir,
        r,
    }
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    let p = r
        .propose(&writer(), view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e}"));
    assert!(p.ready, "{:#?}", p.issues);
    p
}

fn h(s: &str) -> Hash {
    s.parse().unwrap()
}

fn stored(r: &Rdy, version: &str) -> (Hash, Assessment) {
    let st = r.read();
    let a = st
        .reader()
        .assessment_of_proposal(&h(version), st.seq())
        .unwrap()
        .expect("a stored assessment");
    (a, st.reader().assessment(&a).unwrap())
}

fn approve_item(r: &Rdy, pref: &str) -> Item {
    r.read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.kind == ItemKind::Approve && i.r#ref == pref)
        .unwrap_or_else(|| panic!("no approve item of {pref}"))
}

fn mention_rows(a: &Assessment) -> usize {
    a.issues
        .iter()
        .chain(a.confidence.reasons.iter())
        .filter(|i| i.code == "unlinked_mention")
        .count()
}

#[test]
fn mi1_method_two_keeps_old_assessments() {
    let mut w = world();
    assert_eq!(METHOD, 2);

    // version a edits the claim to name the Ledger: a real record with the new method
    put_j(
        &mut w.r,
        "a",
        upd(&w.x, "X holds, as the Ledger records."),
        j_typo(),
    );
    let pa = propose(&mut w.r, "a");
    let (_, real) = stored(&w.r, &pa.id);
    assert_eq!(real.method, 2, "a new record carries method 2");
    assert!(
        mention_rows(&real) >= 1,
        "a normative mention is a gap: {real:#?}"
    );

    // a stored method-1 record of the same version: old coverage, reasons and card facts
    let mut old = real.clone();
    old.method = 1;
    old.coverage.corpus.after.grounded = 40;
    old.coverage.corpus.after.overall = 60;
    let reason = AssessmentIssue {
        block: w.p.clone(),
        dimension: "grounded".into(),
        code: "unlinked_mention".into(),
        subject: Some("the Ledger".into()),
        fix: "link the prose to the Ledger".into(),
        dismissal: None,
    };
    old.issues.push(reason.clone());
    old.confidence.reasons.push(reason);
    let key = |i: &AssessmentIssue| {
        (
            i.block.clone(),
            i.dimension.clone(),
            i.code.clone(),
            i.subject.clone(),
            i.fix.clone(),
        )
    };
    old.issues.sort_by_key(key);
    old.confidence.reasons.sort_by_key(key);
    old.card.facts = vec!["Coverage: grounded 40, scored under the first rule".into()];
    old.card.hash = Hash::of(b"mi1 old card");
    assert_eq!(old.validate(), Ok(()), "a valid method-1 record");
    let old_hash = w.r.store_mut().put_assessment(&old).unwrap();
    w.r.invalidate();
    assert_eq!(
        stored(&w.r, &pa.id).0,
        old_hash,
        "the old record is the stored one"
    );

    // a real new version b edits the prose to name the Ledger: no mention is scored
    put_j(
        &mut w.r,
        "b",
        upd(&w.p, "A small note about the Ledger."),
        j_typo(),
    );
    let pb = propose(&mut w.r, "b");
    let (hb, b) = stored(&w.r, &pb.id);
    assert_eq!(b.method, 2);
    assert_eq!(
        mention_rows(&b),
        0,
        "an informative mention scores nothing: {b:#?}"
    );

    let check = |r: &Rdy, step: &str| {
        let (ha, a) = stored(r, &pa.id);
        assert_eq!(
            (ha, &a),
            (old_hash, &old),
            "{step}: method-1 bytes and hash"
        );
        assert_eq!(a.method, 1, "{step}");
        let (hb2, b2) = stored(r, &pb.id);
        assert_eq!((hb2, &b2), (hb, &b), "{step}: the new record");
        let item = approve_item(r, &pa.r#ref);
        let lines: Vec<String> = item
            .facts
            .iter()
            .map(|f| format!("{}: {}", f.name, f.value))
            .collect();
        for fact in &old.card.facts {
            assert!(
                lines.iter().any(|l| l == fact),
                "{step}: the approve card keeps the stored fact {fact:?}: {lines:?}"
            );
        }
    };
    check(&w.r, "now");
    w.r.reindex().unwrap();
    check(&w.r, "reindex");
    let dir = w._dir.path().to_path_buf();
    let r = w.r;
    r.close().unwrap();
    let r = Rdy::open(&dir).unwrap();
    check(&r, "reopen");

    // dump and load carry both records
    let scratch = tempfile::tempdir().unwrap();
    let file = scratch.path().join("p.rdydump");
    r.read().dump(&file).unwrap();
    let to = scratch.path().join("new");
    load(&file, &to).unwrap();
    let loaded = Rdy::open(&to).unwrap();
    check(&loaded, "dump and load");
    loaded.close().unwrap();
    let mut r = r;

    // the old version is not refused for its method: it is fresh, so it lands, naming its record
    let landed = r
        .land_report(&approver(), &pa.id)
        .expect("a method-1 record lands");
    assert_eq!(landed.approved, pa.r#ref);
    let st = r.read();
    let head = st.canon_head().unwrap();
    assert_eq!(
        st.reader().landing(&head).unwrap().assessment,
        Some(old_hash)
    );
}
