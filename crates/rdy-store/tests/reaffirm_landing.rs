//! cp16 (D-CQMA4; reference/store-format p-N50DS, p-CST21): local card evidence is allowed
//! exactly for a re-affirmation card's isolated landing. The landing writer's evidence check and
//! the landing field rule that fsck, the holder and a take apply share one predicate, so the same
//! table holds for both: the card hash, the `They still hold` choice and the view
//! `reaffirm-<card hash, first 12 digits>` of the landed proposal. Any other local card evidence,
//! and all local text evidence, keeps its refusal.
use rdy_model::*;
use rdy_store::{
    LandRequest, REAFFIRM_HOLD_LABEL, Store, StoreError, landing_field_rules,
    local_card_evidence_allowed, reaffirm_view,
};
use std::collections::BTreeMap;

fn block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        read_identity: None,
    }
}

fn prov(actor: &str, role: Role) -> Provenance {
    Provenance {
        actor: actor.into(),
        run: None,
        role,
        at_ms: 1,
    }
}

/// A proposal of one new claim in view `view`.
fn proposal(s: &mut Store, view: &str, id: &str) -> Hash {
    let w = prov("agent:w", Role::Writer);
    s.commit_to_view(
        view,
        Change {
            view: view.into(),
            ops: vec![Op::Create {
                block: block(id, view),
            }],
            deps: vec![],
            provenance: w.clone(),
            justification: None,
            message: String::new(),
            links: ChangeLinks::default(),
            read_identity: None,
        },
    )
    .unwrap();
    s.propose(view, view, w).unwrap()
}

fn evidence(kind: EvidenceKind, card: Option<Hash>, choice: &str) -> ApprovalEvidence {
    ApprovalEvidence {
        kind,
        options: vec![REAFFIRM_HOLD_LABEL.into(), "Keep them suspect".into()],
        choice: Some(choice.into()),
        card_hash: card,
    }
}

#[test]
fn cp16_local_card_field_rule_is_shared() {
    let card = Hash::of(b"a re-affirmation card");
    let exact = reaffirm_view(&card);
    assert_eq!(exact, format!("reaffirm-{}", &card.to_string()[..12]));
    let other = Hash::of(b"another card");
    let suffix = reaffirm_view(&other);
    let prefix = format!("affirm-{}", &card.to_string()[..12]);
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::create(&dir.path().join("s")).unwrap();
    let views = [&exact, &suffix, &prefix];
    let mut proposals = BTreeMap::new();
    for (n, v) in views.iter().enumerate() {
        proposals.insert(v.to_string(), proposal(&mut s, v, &format!("c-0000{n}")));
    }
    let hold = REAFFIRM_HOLD_LABEL;
    // (view, evidence, allowed): the exact view, hash and choice pass; each other case keeps the
    // refusal of local card or text evidence
    let table: Vec<(&str, ApprovalEvidence, bool)> = vec![
        (&exact, evidence(EvidenceKind::Card, Some(card), hold), true),
        (&exact, evidence(EvidenceKind::Card, None, hold), false),
        (
            &prefix,
            evidence(EvidenceKind::Card, Some(card), hold),
            false,
        ),
        (
            &suffix,
            evidence(EvidenceKind::Card, Some(card), hold),
            false,
        ),
        (
            &exact,
            evidence(EvidenceKind::Card, Some(card), "Keep them suspect"),
            false,
        ),
        (
            &exact,
            evidence(EvidenceKind::Text, Some(card), hold),
            false,
        ),
    ];
    for (view, e, allowed) in &table {
        let case = format!("{view} {e:?}");
        // the predicate both checks share
        assert_eq!(
            local_card_evidence_allowed(e, Some(view)),
            *allowed,
            "predicate: {case}"
        );
        // the landing writer's evidence check
        let end = s.log_end();
        let prepared = s.prepare_land(LandRequest {
            proposal: proposals[*view],
            accept: None,
            approver: prov("human:p", Role::Approver),
            alt: None,
            relay: None,
            evidence: Some(e.clone()),
        });
        match (allowed, prepared) {
            (true, Ok(p)) => {
                let rec = p.landing();
                assert_eq!(rec.assurance, Assurance::Local, "{case}");
                assert_eq!(rec.evidence.as_ref(), Some(e), "{case}");
            }
            (false, Err(StoreError::Decode(m))) => assert_eq!(
                m,
                format!("evidence kind {} needs a relay", e.kind.as_str()),
                "writer: {case}"
            ),
            (_, other) => panic!("writer: {case}: {other:?}"),
        }
        assert_eq!(s.log_end(), end, "a prepared landing writes nothing");
        // the field rule of fsck, the holder and a take
        let rec = LandingRecord {
            proposal: proposals[*view],
            prev: None,
            state: Hash::of(b"state"),
            approved_by: prov("human:p", Role::Approver),
            changes: vec![],
            chosen: None,
            checks: vec![],
            assurance: Assurance::Local,
            relayed_by: None,
            relay_evidence: None,
            approved_version: None,
            rebased_from: vec![],
            evidence: Some(e.clone()),
            assessment: None,
            policy: None,
            read_identity: None,
        };
        let l = Hash::of(b"landing");
        let ruled = landing_field_rules::<StoreError>(&l, &rec, Some(view), &mut |_, _| Ok(true));
        match (allowed, ruled) {
            (true, Ok(())) => {}
            (false, Err(StoreError::Decode(m))) => assert_eq!(
                m,
                format!(
                    "landing {l}: evidence kind {} needs assurance relayed",
                    e.kind.as_str()
                ),
                "field rule: {case}"
            ),
            (_, other) => panic!("field rule: {case}: {other:?}"),
        }
        // a caller that did not read the proposal's view never allows it
        assert!(
            landing_field_rules::<StoreError>(&l, &rec, None, &mut |_, _| Ok(true)).is_err(),
            "no view: {case}"
        );
    }
}
