//! The record shapes (plan M2a1, repr1). A shape's postcard encoding is its fields in declaration
//! order, so comparing it with the tuple of its fields freezes the order; an id is the hash of
//! that encoding.
use rdy_model::*;

fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

fn prov() -> Provenance {
    Provenance {
        actor: "agent:designer".into(),
        run: Some("run-1".into()),
        role: Role::Writer,
        at_ms: 1_759_449_600_000,
    }
}

fn alt() -> AltChoice {
    AltChoice {
        set: Hash::of(b"set"),
        member: "alt-a".into(),
    }
}

fn links() -> ChangeLinks {
    ChangeLinks {
        intent: Some(Hash::of(b"intent")),
        alternative: Some(alt()),
        client_nonce: Some("nonce-1".into()),
        imported_by: Some(ActorRun {
            actor: "human:imp".into(),
            run: Some("run-2".into()),
        }),
    }
}

fn relay() -> ActorRun {
    ActorRun {
        actor: "agent:relay".into(),
        run: Some("run-9".into()),
    }
}

fn change(links: ChangeLinks) -> Change {
    Change {
        view: "work".into(),
        ops: vec![],
        deps: vec![Hash::of(b"dep")],
        provenance: prov(),
        justification: None,
        message: "m".into(),
        links,
    }
}

/// A landing with every post-spec field set to a non-default value.
fn full_landing() -> LandingRecord {
    LandingRecord {
        proposal: Hash::of(b"p"),
        prev: Some(Hash::of(b"prev")),
        state: Hash::of(b"s"),
        approved_by: prov(),
        changes: vec![Hash::of(b"c")],
        chosen: Some(alt()),
        checks: vec![],
        assurance: Assurance::Relayed,
        relayed_by: Some(relay()),
        relay_evidence: Some(RelayEvidence {
            quoted: "yes, land it".into(),
            session_ref: "sess-1".into(),
            question: Some("land it?".into()),
        }),
        approved_version: Some(Hash::of(b"p@1")),
        rebased_from: vec![Hash::of(b"l1"), Hash::of(b"l2")],
        evidence: Some(ApprovalEvidence {
            kind: EvidenceKind::Card,
            options: vec!["land".into(), "wait".into()],
            choice: Some("land".into()),
            card_hash: Some(Hash::of(b"card")),
        }),
    }
}

/// What M2 writes: local, no relay, the approved version, nothing else.
fn local_landing(approved_version: Option<Hash>) -> LandingRecord {
    LandingRecord {
        assurance: Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        approved_version,
        rebased_from: vec![],
        evidence: None,
        ..full_landing()
    }
}

/// Review Focus 1: the change encoding is the six old fields followed by the links.
/// cc1 (FW-F7b, D-JCVPD): `imported_by` is the last field of the links.
#[test]
fn cc1_change_links_end_with_imported_by() {
    let imp = ActorRun {
        actor: "human:imp".into(),
        run: Some("run-2".into()),
    };
    assert_eq!(
        enc(&ChangeLinks {
            imported_by: Some(imp.clone()),
            ..ChangeLinks::default()
        }),
        enc(&(None::<Hash>, None::<AltChoice>, None::<String>, Some(imp)))
    );
}

#[test]
fn repr1_a_change_is_its_old_fields_followed_by_its_links() {
    let c = change(links());
    let mut want = enc(&(
        &c.view,
        &c.ops,
        &c.deps,
        &c.provenance,
        &c.justification,
        &c.message,
    ));
    want.extend(enc(&links()));
    assert_eq!(enc(&c), want);
    assert_eq!(c.id(), Hash::of(&want));
    assert_eq!(
        enc(&ChangeLinks::default()),
        vec![0, 0, 0, 0],
        "four absent options"
    );
    assert_eq!(
        enc(&links()),
        enc(&(
            Some(Hash::of(b"intent")),
            Some(alt()),
            Some("nonce-1".to_string()),
            Some(ActorRun {
                actor: "human:imp".into(),
                run: Some("run-2".into()),
            })
        ))
    );
    assert_eq!(enc(&alt()), enc(&(Hash::of(b"set"), "alt-a")));
    assert_ne!(
        change(ChangeLinks::default()).id(),
        c.id(),
        "the links are part of the id"
    );
}

#[test]
fn repr1_proposal_and_landing_field_order_is_frozen() {
    let p = Proposal {
        stable: Some(Hash::of(b"first")),
        prev: Some(Hash::of(b"prev")),
        title: "t".into(),
        view: "work".into(),
        changes: vec![Hash::of(b"c1"), Hash::of(b"c2")],
        alternative: Some(alt()),
        provenance: prov(),
        canon_at: Some(Hash::of(b"canon")),
        summary: Some("what it does".into()),
    };
    assert_eq!(
        enc(&p),
        enc(&(
            p.stable,
            p.prev,
            &p.title,
            &p.view,
            &p.changes,
            &p.alternative,
            &p.provenance,
            p.canon_at,
            &p.summary,
        ))
    );
    assert_eq!(p.id(), Hash::of(&enc(&p)));
    let l = full_landing();
    assert_eq!(
        enc(&l),
        enc(&(
            l.proposal,
            l.prev,
            l.state,
            &l.approved_by,
            &l.changes,
            &l.chosen,
            &l.checks,
            l.assurance,
            &l.relayed_by,
            &l.relay_evidence,
            l.approved_version,
            &l.rebased_from,
            &l.evidence,
        ))
    );
    assert_eq!(l.id(), Hash::of(&enc(&l)));
    assert_ne!(
        local_landing(l.approved_version).id(),
        l.id(),
        "the new fields are part of the id"
    );
}

/// Review Focus 6: the new enum and structs have fixed encodings.
#[test]
fn repr1_landing_assurance_and_relay_fields_have_fixed_encodings() {
    let all = [
        Assurance::Policy,
        Assurance::Relayed,
        Assurance::Local,
        Assurance::Authoritative,
    ];
    assert_eq!(
        all.map(|a| enc(&a)),
        [vec![0x00], vec![0x01], vec![0x02], vec![0x03]]
    );
    assert_eq!(
        all.map(Assurance::as_str),
        ["policy", "relayed", "local", "authoritative"]
    );
    assert!(
        all.windows(2).all(|w| w[0] < w[1]),
        "declaration order is strength order"
    );
    assert_eq!(enc(&relay()), enc(&("agent:relay", Some("run-9"))));
    let ev = RelayEvidence {
        quoted: "yes, land it".into(),
        session_ref: "sess-1".into(),
        question: Some("land it?".into()),
    };
    assert_eq!(enc(&ev), enc(&("yes, land it", "sess-1", Some("land it?"))));
}

/// Review Focus 6: what M2 writes after `checks` is a fixed tail of bytes.
#[test]
fn repr1_an_m2_landings_new_fields_encode_as_a_fixed_tail() {
    let l = local_landing(Some(Hash::of(b"p")));
    let head = enc(&(
        l.proposal,
        l.prev,
        l.state,
        &l.approved_by,
        &l.changes,
        &l.chosen,
        &l.checks,
    ));
    let mut want = head.clone();
    want.extend([0x02, 0x00, 0x00, 0x01]);
    want.extend(Hash::of(b"p").0);
    want.extend([0x00, 0x00]);
    assert_eq!(
        enc(&l),
        want,
        "local, no relay, no evidence, the version, no rebase, no approval evidence"
    );
    let mut want = head;
    want.extend([0x02, 0x00, 0x00, 0x00, 0x00, 0x00]);
    assert_eq!(enc(&local_landing(None)), want);
}

#[test]
fn evk1_evidence_kinds_and_approval_evidence_have_fixed_encodings() {
    let all = [
        EvidenceKind::Elicited,
        EvidenceKind::Card,
        EvidenceKind::Text,
    ];
    assert_eq!(all.map(|k| enc(&k)), [vec![0x00], vec![0x01], vec![0x02]]);
    assert_eq!(all.map(EvidenceKind::as_str), ["elicited", "card", "text"]);
    let e = ApprovalEvidence {
        kind: EvidenceKind::Text,
        options: vec![],
        choice: None,
        card_hash: None,
    };
    assert_eq!(
        enc(&e),
        enc(&(
            EvidenceKind::Text,
            Vec::<String>::new(),
            None::<String>,
            None::<Hash>
        ))
    );
}

fn decision_block(fields: &[(&str, &str)]) -> Block {
    Block {
        id: BlockId::parse("D-AAAAA").unwrap(),
        block_type: decision::DECISION.into(),
        parent: None,
        order: "V".into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str(v.to_string())))
            .collect(),
        body: String::new(),
        links: Default::default(),
        pins: Default::default(),
    }
}

/// Review Focus 6: a decision without effective_from takes effect when it was recorded.
#[test]
fn eff1_a_decision_takes_effect_when_recorded_unless_it_says_otherwise() {
    let rec = ("recorded_at", "2026-10-04T10:00:00Z");
    assert_eq!(
        decision::effective_from(&decision_block(&[rec])),
        Some("2026-10-04T10:00:00Z")
    );
    let earlier = decision_block(&[rec, (decision::EFFECTIVE_FROM, "2026-09-01T00:00:00Z")]);
    assert_eq!(
        decision::effective_from(&earlier),
        Some("2026-09-01T00:00:00Z")
    );
    assert_eq!(decision::effective_from(&decision_block(&[])), None);
    let claim = Block {
        block_type: "claim".into(),
        ..decision_block(&[rec])
    };
    assert_eq!(decision::effective_from(&claim), None);
}

/// The encoding of `full_answer()`, pinned when the ANSWER kind was added (store format 11).
const ANSWER_HEX: &str = concat!(
    "09502d4141414141403103010672656a656374010652656a6563740307436f6e6669726d0652656a656374054c",
    "6174657201bccd670673e993991bd31a5245563902bcea170b4f6e4646c8a5b2623b4c26f0010538661423236a",
    "5d8df6286a8633369e2a03c5f90d48eded333222dbee8cdbdd0a68756d616e3a6e69636b00038080e682b96601",
    "0b6167656e743a72656c6179010572756e2d3901106e6f2c206e6f742074686174206f6e6506736573732d310001010109746f6f206561726c79",
);

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A fixed answer with every field set.
fn full_answer() -> Answer {
    Answer {
        item: "P-AAAAA@1".into(),
        act: AnswerAct::RejectDecision,
        option: Some("reject".into()),
        label: Some("Reject".into()),
        options: vec!["Confirm".into(), "Reject".into(), "Later".into()],
        card_hash: Some(Hash::of(b"card")),
        targets: vec![Hash::of(b"t")],
        person: Provenance {
            actor: "human:nick".into(),
            run: None,
            role: Role::Approver,
            at_ms: 1_760_000_000_000,
        },
        relayed_by: Some(relay()),
        relay_evidence: Some(RelayEvidence {
            quoted: "no, not that one".into(),
            session_ref: "sess-1".into(),
            question: None,
        }),
        evidence_kind: Some(EvidenceKind::Card),
        note: Some("too early".into()),
    }
}

/// D-F0904 and O1: an answer's encoding is its fields in declaration order, each act has a fixed
/// variant index, and the bytes of one fixed answer are pinned.
#[test]
fn di1_an_answer_encodes_field_by_field() {
    let acts = [
        AnswerAct::RejectProposal,
        AnswerAct::Later,
        AnswerAct::Confirm,
        AnswerAct::RejectDecision,
        AnswerAct::AnswerQuestion,
        AnswerAct::Dismiss,
        AnswerAct::Tag,
        AnswerAct::ViewOwn,
        AnswerAct::CloseStale,
        AnswerAct::Propose,
    ];
    for (i, a) in acts.iter().enumerate() {
        assert_eq!(enc(a), vec![i as u8], "{a:?}");
    }
    assert_eq!(
        acts.map(AnswerAct::as_str),
        [
            "reject_proposal",
            "later",
            "confirm",
            "reject_decision",
            "answer_question",
            "dismiss",
            "tag",
            "view_own",
            "close_stale",
            "propose",
        ]
    );
    let a = full_answer();
    let fields = enc(&(
        &a.item,
        a.act,
        &a.option,
        &a.label,
        &a.options,
        a.card_hash,
        &a.targets,
        &a.person,
        &a.relayed_by,
        &a.relay_evidence,
        a.evidence_kind,
        &a.note,
    ));
    let bytes = enc(&a);
    assert_eq!(bytes, fields, "the fields in declaration order");
    assert_eq!(hex(&bytes), ANSWER_HEX, "the pinned bytes");
    assert_eq!(
        a.id(),
        Hash::of(&bytes),
        "the id is the hash of the encoding"
    );
    let back: Answer = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(back, a);
}
