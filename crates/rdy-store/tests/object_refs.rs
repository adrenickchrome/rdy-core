//! Object references (plan HA1 Task 1; D-D2NPN `refs_of`): every assigned portable kind and every
//! enum arm, identity hashes that are not dependencies, sorted unique output, the ObjectKey JSON
//! form, refused kinds, and the text bound on every stored text field.
use rdy_model::plan::{
    Plan, PlanAct, PlanAnswer, PlanHead, PlanKind, PlanOutcome, PlanSeal, SealedItem,
};
use rdy_model::redaction::{
    AssessmentSupport, BlockResidue, ChangeResidue, CopyKind, CopyRow, CopyStatus, DropSpan,
    FramePlace, ImpactKind, ImpactRelation, ImpactRow, ReasonClass, RedactTarget, Redaction,
    RedactionApply, RedactionManifest, RedactionResidue, ResidualOp,
};
use rdy_model::signature::Signature;
use rdy_model::{
    ActorRun, AltChoice, AltMember, AltSet, AltStatus, Answer, AnswerAct, ApprovalEvidence,
    Assurance, Block, BlockId, Change, ChangeLinks, Citation, EditorialClass, EvidenceKind,
    FieldValue, Hash, Intent, IntentStatus, Justification, LandingRecord, Link, Op, PairItem,
    Proposal, Provenance, RelayEvidence, Review, ReviewSubject, Role, SemanticTension,
    StateSnapshot, Tag, Verdict,
};
use rdy_store::bounded::{RecordError, TEXT_BYTES};
use rdy_store::kinds::{self, *};
use rdy_store::object_refs::{ObjectKey, ObjectRef, PORTABLE_KINDS, refs_of};
use rdy_store::transition::{
    Author, Cause, Side as MoveSide, SuspectMove, TensionMove, Transitions, Why,
};
use rdy_store::{Conflict, Owner, Side, ViewOp, ViewOpKind};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

fn h(n: u8) -> Hash {
    Hash([n; 32])
}

fn some(kind: u8, n: u8) -> ObjectRef {
    ObjectRef {
        kind: Some(kind),
        hash: h(n),
    }
}

/// An edge to a computed hash.
fn edge(kind: u8, hash: Hash) -> ObjectRef {
    ObjectRef {
        kind: Some(kind),
        hash,
    }
}

fn any(n: u8) -> ObjectRef {
    ObjectRef {
        kind: None,
        hash: h(n),
    }
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// The text marker of field `name`: every stored text field of a sample holds one.
fn t(name: &str) -> String {
    format!("~{name}~")
}

fn prov(of: &str) -> Provenance {
    Provenance {
        actor: t(&format!("{of}.actor")),
        run: Some(t(&format!("{of}.run"))),
        role: Role::Writer,
        at_ms: 5,
    }
}

fn actor_run(of: &str) -> ActorRun {
    ActorRun {
        actor: t(&format!("{of}.actor")),
        run: Some(t(&format!("{of}.run"))),
    }
}

fn relay(of: &str) -> RelayEvidence {
    RelayEvidence {
        quoted: t(&format!("{of}.quoted")),
        session_ref: t(&format!("{of}.session_ref")),
        question: Some(t(&format!("{of}.question"))),
    }
}

fn alt(n: u8, of: &str) -> Option<AltChoice> {
    Some(AltChoice {
        set: h(n),
        member: t(&format!("{of}.member")),
    })
}

/// A block of type `ty` with every text field marked under `of`, a parent, links and a pin.
fn block(of: &str, ty: &str, fields: BTreeMap<String, FieldValue>) -> Block {
    let mut all = BTreeMap::from([
        (
            t(&format!("{of}.field_key")),
            FieldValue::Str(t(&format!("{of}.field_value"))),
        ),
        (
            "nested".to_string(),
            FieldValue::List(vec![
                FieldValue::Str(t(&format!("{of}.list_item"))),
                FieldValue::Map(BTreeMap::from([(
                    t(&format!("{of}.map_key")),
                    FieldValue::Str(t(&format!("{of}.map_value"))),
                )])),
                FieldValue::Int(3),
                FieldValue::Bool(true),
                FieldValue::Null,
            ]),
        ),
    ]);
    all.extend(fields);
    Block {
        id: id("c-00001"),
        block_type: ty.to_string(),
        parent: Some(id("s-00002")),
        order: t(&format!("{of}.order")),
        fields: all,
        body: t(&format!("{of}.body")),
        links: BTreeMap::from([(
            t(&format!("{of}.link_key")),
            vec![Link {
                target: id("d-00003"),
                part: Some(t(&format!("{of}.link_part"))),
            }],
        )]),
        // a pin names a content hash: an identity, not an edge
        pins: BTreeMap::from([(id("d-00003"), h(250))]),
        read_identity: None,
    }
}

fn blob(n: u8) -> BTreeMap<String, FieldValue> {
    BTreeMap::from([("blob".to_string(), FieldValue::Str(h(n).to_string()))])
}

fn change(of: &str, ops: Vec<Op>, links: ChangeLinks, justification: Justification) -> Change {
    Change {
        view: t(&format!("{of}.view")),
        ops,
        deps: vec![h(5)],
        provenance: prov(of),
        justification: Some(justification),
        message: t(&format!("{of}.message")),
        links,
        read_identity: None,
    }
}

fn links(of: &str) -> ChangeLinks {
    ChangeLinks {
        intent: Some(h(6)),
        alternative: alt(7, of),
        client_nonce: Some(t(&format!("{of}.client_nonce"))),
        imported_by: Some(actor_run(&format!("{of}.imported_by"))),
    }
}

fn view_op(op: ViewOpKind) -> ViewOp {
    ViewOp {
        view: t("view_op.view"),
        prev: Some(h(40)),
        op,
        actor: t("view_op.actor"),
        run: Some(t("view_op.run")),
        at_ms: 9,
    }
}

fn owner() -> Option<Owner> {
    Some(Owner {
        actor: t("view_op.owner.actor"),
        run: Some(t("view_op.owner.run")),
    })
}

fn alt_set(status: AltStatus) -> AltSet {
    AltSet {
        stable: Some(h(58)),
        prev: Some(h(59)),
        title: t("alt_set.title"),
        question: Some(id("q-00004")),
        members: vec![AltMember {
            label: t("alt_set.member.label"),
            view: t("alt_set.member.view"),
        }],
        status,
        provenance: prov("alt_set"),
    }
}

fn review(subject: ReviewSubject, verdict: Verdict) -> Review {
    Review {
        subject,
        case_hash: Some(h(65)),
        verdict,
        reason: t("review.reason"),
        citations: vec![Citation {
            block: id("c-00001"),
            note: t("review.finding.note"),
        }],
        provenance: prov("review"),
    }
}

fn why(cause: Cause, n: Option<u8>) -> Why {
    let dismissed = cause == Cause::Dismissed;
    Why {
        cause,
        id: n.map(h),
        actor: dismissed.then(|| t("transition.why.actor")),
        run: dismissed.then(|| t("transition.why.run")),
    }
}

fn tension_move(n: u8, why: Why) -> TensionMove {
    TensionMove {
        tension: h(n),
        kind: t("transition.kind"),
        severity: t("transition.severity"),
        sides: vec![
            MoveSide {
                change: Some(h(71)),
                actor: Some(t("transition.side.actor")),
                run: Some(t("transition.side.run")),
                landing: None,
            },
            MoveSide {
                change: None,
                actor: None,
                run: None,
                landing: Some(h(72)),
            },
        ],
        // a re-key names another tension's id: an identity
        rekey: Some(h(73)),
        why,
    }
}

fn suspect_move(why: Why) -> SuspectMove {
    SuspectMove {
        block: id("c-00001"),
        target: id("d-00003"),
        reason: t("transition.suspect.reason"),
        writers: vec![Author {
            actor: t("transition.writer.actor"),
            run: Some(t("transition.writer.run")),
        }],
        why,
    }
}

fn transitions(resolved: Vec<TensionMove>, gone: Vec<SuspectMove>) -> Transitions {
    Transitions {
        at_ms: 3,
        resolved,
        opened: vec![],
        gone,
        new: vec![],
    }
}

fn redaction() -> Redaction {
    Redaction {
        carrier: h(80),
        actor: prov("redaction"),
        targets: vec![
            RedactTarget::AllVersions(id("c-00001")),
            RedactTarget::Version {
                id: id("c-00001"),
                version: h(81),
            },
            RedactTarget::Blob(h(82)),
        ],
        reason_class: ReasonClass::Secret,
        reason: t("redaction.reason"),
        manifest: RedactionManifest {
            versions: vec![(id("c-00001"), h(83))],
            blobs: vec![h(84)],
            shared_blobs: vec![h(85)],
            spans: vec![
                DropSpan {
                    place: FramePlace {
                        segment: 0,
                        offset: 10,
                    },
                    kind: BLOCK,
                    hash: h(86),
                    len: 40,
                    residue: Some(h(87)),
                },
                DropSpan {
                    place: FramePlace {
                        segment: 1,
                        offset: 0,
                    },
                    kind: BLOB_CHUNK,
                    hash: h(88),
                    len: 4,
                    residue: None,
                },
            ],
            inside: vec![
                ImpactRow {
                    kind: ImpactKind::Tag,
                    record: t("redaction.impact.record"),
                    relation: ImpactRelation::References,
                    erasable: false,
                },
                ImpactRow {
                    kind: ImpactKind::Landing,
                    // report text, even when it spells a hash
                    record: h(89).to_string(),
                    relation: ImpactRelation::Quotes,
                    erasable: true,
                },
            ],
            outside: vec![CopyRow {
                kind: CopyKind::Dump,
                status: CopyStatus::Unknown,
            }],
            quarantine: vec![t("redaction.quarantine")],
            assessment: AssessmentSupport::NotImplemented,
        },
    }
}

fn change_residue() -> ChangeResidue {
    ChangeResidue {
        // the removed change's own id: an identity
        change: h(92),
        view: t("residue.view"),
        ops: vec![
            ResidualOp::Create {
                id: id("c-00001"),
                version: h(93),
            },
            ResidualOp::Update {
                base: h(94),
                id: id("c-00001"),
                version: h(95),
            },
            ResidualOp::Delete {
                base: h(96),
                id: id("c-00001"),
            },
        ],
        deps: vec![h(97)],
        provenance: prov("residue"),
        justification: Some(Justification::Editorial {
            class: EditorialClass::Typo,
            reason: t("residue.reason"),
        }),
        message: t("residue.message"),
        links: ChangeLinks {
            intent: Some(h(98)),
            alternative: alt(99, "residue.alt"),
            client_nonce: Some(t("residue.client_nonce")),
            imported_by: Some(actor_run("residue.imported_by")),
        },
    }
}

fn enc<T: Serialize>(v: &T) -> Vec<u8> {
    postcard::to_allocvec(v).unwrap()
}

/// One sample record: its arm's name, kind, bytes and expected edges (in any order).
struct Sample {
    arm: &'static str,
    kind: u8,
    bytes: Vec<u8>,
    edges: Vec<ObjectRef>,
}

fn s<T: Serialize>(arm: &'static str, kind: u8, v: &T, edges: Vec<ObjectRef>) -> Sample {
    Sample {
        arm,
        kind,
        bytes: enc(v),
        edges,
    }
}

/// A sample of every assigned portable kind and every enum arm that can hold an edge.
fn samples() -> Vec<Sample> {
    let figure = block("block", "figure", blob(1));
    let claim = block(
        "block",
        &t("block.type"),
        // on a type other than figure, a blob field is plain text
        BTreeMap::from([("blob".to_string(), FieldValue::Str("not a hash".into()))]),
    );
    let created = block("change.block", "figure", blob(2));
    let updated = block("change.block", "figure", blob(8));
    let edit = change(
        "change",
        vec![
            Op::Create {
                block: created.clone(),
            },
            Op::Update {
                base: h(3),
                block: updated.clone(),
            },
            Op::Delete {
                base: h(4),
                id: id("c-00001"),
            },
        ],
        links("change.alt"),
        Justification::Editorial {
            class: EditorialClass::Clarify,
            reason: t("change.reason"),
        },
    );
    let plain = Change {
        links: ChangeLinks::default(),
        justification: Some(Justification::Implements(vec![id("d-00003")])),
        ops: vec![],
        ..edit.clone()
    };
    let proposal = Proposal {
        stable: Some(h(10)),
        prev: Some(h(11)),
        title: t("proposal.title"),
        view: t("proposal.view"),
        changes: vec![h(12)],
        alternative: alt(13, "proposal.alt"),
        provenance: prov("proposal"),
        canon_at: Some(h(14)),
        summary: Some(t("proposal.summary")),
    };
    let first = Proposal {
        stable: None,
        prev: None,
        alternative: None,
        canon_at: None,
        ..proposal.clone()
    };
    let landing = LandingRecord {
        proposal: h(20),
        prev: Some(h(21)),
        state: h(22),
        approved_by: prov("landing.approved_by"),
        changes: vec![h(23)],
        chosen: alt(24, "landing.chosen"),
        checks: vec![h(25)],
        assurance: Assurance::Relayed,
        relayed_by: Some(actor_run("landing.relayed_by")),
        relay_evidence: Some(relay("landing.relay")),
        approved_version: Some(h(26)),
        rebased_from: vec![h(27)],
        evidence: Some(ApprovalEvidence {
            kind: EvidenceKind::Card,
            options: vec![t("landing.evidence.option")],
            choice: Some(t("landing.evidence.choice")),
            // the card shown: an identity
            card_hash: Some(h(28)),
        }),
    };
    let snapshot = StateSnapshot {
        blocks: BTreeMap::from([(id("c-00001"), h(30)), (id("d-00003"), h(31))]),
    };
    let manifest = kinds::BlobManifest {
        len: 10,
        chunks: vec![h(32), h(33)],
    };
    let tag = Tag {
        name: t("tag.name"),
        landing: h(34),
        message: Some(t("tag.message")),
        tagged_by: prov("tag"),
    };
    let answer = Answer {
        item: t("answer.item"),
        act: AnswerAct::Confirm,
        option: Some(t("answer.option")),
        label: Some(t("answer.label")),
        options: vec![t("answer.options")],
        card_hash: Some(h(35)),
        targets: vec![h(36), h(37)],
        person: prov("answer.person"),
        relayed_by: Some(actor_run("answer.relayed_by")),
        relay_evidence: Some(relay("answer.relay")),
        evidence_kind: Some(EvidenceKind::Text),
        note: Some(t("answer.note")),
    };
    // a relayed tag's answer has no targets (Task 5 attaches it by its item)
    let tag_answer = Answer {
        item: "tag:v1".into(),
        act: AnswerAct::Tag,
        targets: vec![],
        ..answer.clone()
    };
    let signature = Signature {
        subject: h(38),
        namespace: t("signature.namespace"),
        statement: b"rdy-statement: 1\n".to_vec(),
        sig: vec![1, 2, 3],
        signed_by: prov("signature"),
    };
    let intent = Intent {
        stable: Some(h(56)),
        prev: Some(h(57)),
        actor: t("intent.actor"),
        run: Some(t("intent.run")),
        view: t("intent.view"),
        area: Some(t("intent.area")),
        blocks: vec![id("c-00001")],
        angle: t("intent.angle"),
        expires_at_ms: 100,
        status: IntentStatus::Open,
        at_ms: 1,
    };
    let withdrawn = Intent {
        status: IntentStatus::Withdrawn,
        ..intent.clone()
    };
    let pair = vec![PairItem {
        block: id("c-00001"),
        content: h(64),
    }];
    let tension = SemanticTension {
        key: h(66),
        items: vec![PairItem {
            block: id("c-00001"),
            content: h(67),
        }],
        review: h(68),
        advice: Some(t("tension.advice")),
        blocking: true,
        relay: Some(relay("tension.relay")),
        relayed_by: Some(actor_run("tension.relayed_by")),
        prev: Some(h(69)),
        provenance: prov("tension"),
    };
    let conflict = Conflict {
        // the conflict's own id: an identity
        id: h(51),
        block: id("c-00001"),
        base: Some(h(52)),
        sides: vec![
            Side {
                change: Some(h(53)),
                version: Some(h(54)),
            },
            Side {
                change: None,
                version: None,
            },
        ],
    };
    let causes = [
        (Cause::Committed, Some(74), Some(CHANGE)),
        (Cause::Landed, Some(75), Some(LANDING)),
        (Cause::Dismissed, Some(76), Some(REVIEW)),
        (Cause::ViewChanged, Some(77), Some(VIEW_OP)),
        (Cause::Reviewed, Some(78), Some(REVIEW)),
        (Cause::AltSet, Some(79), Some(ALT_SET)),
        (Cause::Written, None, None),
        (Cause::PackReload, None, None),
        (Cause::Reopened, None, None),
        (Cause::Recheck, None, None),
    ];
    let mut out = vec![
        s("block figure", BLOCK, &figure, vec![some(BLOB_MANIFEST, 1)]),
        s("block other type", BLOCK, &claim, vec![]),
        s(
            "block figure without blob",
            BLOCK,
            &block("block", "figure", BTreeMap::new()),
            vec![],
        ),
        s(
            "change ops",
            CHANGE,
            &edit,
            vec![
                some(CHANGE, 5),
                edge(BLOCK, created.version_hash()),
                some(BLOB_MANIFEST, 2),
                some(BLOCK, 3),
                edge(BLOCK, updated.version_hash()),
                some(BLOB_MANIFEST, 8),
                some(BLOCK, 4),
                some(INTENT, 6),
                some(ALT_SET, 7),
            ],
        ),
        s("change plain", CHANGE, &plain, vec![some(CHANGE, 5)]),
        s(
            "proposal",
            PROPOSAL,
            &proposal,
            vec![
                some(PROPOSAL, 10),
                some(PROPOSAL, 11),
                some(CHANGE, 12),
                some(ALT_SET, 13),
                some(LANDING, 14),
            ],
        ),
        s("proposal first", PROPOSAL, &first, vec![some(CHANGE, 12)]),
        s(
            "landing",
            LANDING,
            &landing,
            vec![
                some(PROPOSAL, 20),
                some(LANDING, 21),
                some(SNAPSHOT, 22),
                some(CHANGE, 23),
                some(ALT_SET, 24),
                any(25),
                some(PROPOSAL, 26),
                some(LANDING, 27),
            ],
        ),
        s(
            "snapshot",
            SNAPSHOT,
            &snapshot,
            vec![some(BLOCK, 30), some(BLOCK, 31)],
        ),
        Sample {
            arm: "blob chunk",
            kind: BLOB_CHUNK,
            bytes: b"raw chunk bytes".to_vec(),
            edges: vec![],
        },
        s(
            "blob manifest",
            BLOB_MANIFEST,
            &manifest,
            vec![some(BLOB_CHUNK, 32), some(BLOB_CHUNK, 33)],
        ),
        s("tag", TAG, &tag, vec![some(LANDING, 34)]),
        s("answer", ANSWER, &answer, vec![any(36), any(37)]),
        s("answer relayed tag", ANSWER, &tag_answer, vec![]),
        s("signature", SIGNATURE, &signature, vec![any(38)]),
        s(
            "view_op create",
            VIEW_OP,
            &view_op(ViewOpKind::Create {
                base: Some(h(41)),
                owner: owner(),
                pinned: true,
            }),
            vec![some(VIEW_OP, 40), some(LANDING, 41)],
        ),
        s(
            "view_op fork",
            VIEW_OP,
            &view_op(ViewOpKind::Fork {
                from: t("view_op.fork.from"),
                from_head: h(42),
                base: Some(h(43)),
                changes: vec![h(44)],
                owner: owner(),
            }),
            vec![
                some(VIEW_OP, 40),
                some(VIEW_OP, 42),
                some(LANDING, 43),
                some(CHANGE, 44),
            ],
        ),
        s(
            "view_op commit",
            VIEW_OP,
            &view_op(ViewOpKind::Commit { change: h(45) }),
            vec![some(VIEW_OP, 40), some(CHANGE, 45)],
        ),
        s(
            "view_op include",
            VIEW_OP,
            &view_op(ViewOpKind::Include {
                changes: vec![h(46)],
            }),
            vec![some(VIEW_OP, 40), some(CHANGE, 46)],
        ),
        s(
            "view_op exclude",
            VIEW_OP,
            &view_op(ViewOpKind::Exclude {
                changes: vec![h(47)],
            }),
            vec![some(VIEW_OP, 40), some(CHANGE, 47)],
        ),
        s(
            "view_op rebase",
            VIEW_OP,
            &view_op(ViewOpKind::Rebase {
                landing: h(48),
                dropped: vec![h(49)],
                remaining: vec![h(50)],
                conflicts: vec![conflict],
            }),
            vec![
                some(VIEW_OP, 40),
                some(LANDING, 48),
                some(CHANGE, 49),
                some(CHANGE, 50),
                some(BLOCK, 52),
                some(CHANGE, 53),
                some(BLOCK, 54),
            ],
        ),
        s(
            "view_op undo",
            VIEW_OP,
            &view_op(ViewOpKind::Undo { target: h(55) }),
            vec![some(VIEW_OP, 40), some(VIEW_OP, 55)],
        ),
        s(
            "view_op close",
            VIEW_OP,
            &view_op(ViewOpKind::Close {
                reason: t("view_op.close.reason"),
            }),
            vec![some(VIEW_OP, 40)],
        ),
        s(
            "view_op own",
            VIEW_OP,
            &view_op(ViewOpKind::Own { owner: owner() }),
            vec![some(VIEW_OP, 40)],
        ),
        s(
            "intent",
            INTENT,
            &intent,
            vec![some(INTENT, 56), some(INTENT, 57)],
        ),
        s(
            "intent withdrawn",
            INTENT,
            &withdrawn,
            vec![some(INTENT, 56), some(INTENT, 57)],
        ),
        s(
            "alt_set open",
            ALT_SET,
            &alt_set(AltStatus::Open),
            vec![some(ALT_SET, 58), some(ALT_SET, 59)],
        ),
        s(
            "alt_set chosen",
            ALT_SET,
            &alt_set(AltStatus::Chosen {
                member: t("alt_set.chosen.member"),
                landing: h(60),
            }),
            vec![some(ALT_SET, 58), some(ALT_SET, 59), some(LANDING, 60)],
        ),
        s(
            "alt_set abandoned",
            ALT_SET,
            &alt_set(AltStatus::Abandoned {
                reason: t("alt_set.abandoned.reason"),
            }),
            vec![some(ALT_SET, 58), some(ALT_SET, 59)],
        ),
        s(
            "review tension",
            REVIEW,
            &review(
                ReviewSubject::Tension {
                    id: h(61),
                    vkey: h(62),
                    kind: t("review.tension.kind"),
                    severity: t("review.tension.severity"),
                    sides: vec![t("review.tension.side")],
                    blocks: vec![id("c-00001")],
                },
                Verdict::Dismiss,
            ),
            vec![],
        ),
        s(
            "review mention",
            REVIEW,
            &review(
                ReviewSubject::Mention {
                    block: id("c-00001"),
                    entity: id("e-00005"),
                },
                Verdict::Restore,
            ),
            vec![],
        ),
        s(
            "review pair",
            REVIEW,
            &review(
                ReviewSubject::Pair {
                    key: h(63),
                    items: pair.clone(),
                },
                Verdict::Tension,
            ),
            vec![],
        ),
        s(
            "review pair no tension",
            REVIEW,
            &review(
                ReviewSubject::Pair {
                    key: h(63),
                    items: pair,
                },
                Verdict::NoTension,
            ),
            vec![],
        ),
        s(
            "tension",
            TENSION,
            &tension,
            vec![some(REVIEW, 68), some(TENSION, 69)],
        ),
        s(
            "redaction",
            REDACTION,
            &redaction(),
            vec![
                some(CHANGE, 80),
                some(BLOCK, 81),
                some(BLOB_MANIFEST, 82),
                some(BLOCK, 83),
                some(BLOB_MANIFEST, 84),
                some(BLOB_MANIFEST, 85),
                some(BLOCK, 86),
                some(REDACTION_RESIDUE, 87),
                some(BLOB_CHUNK, 88),
            ],
        ),
        s(
            "residue block",
            REDACTION_RESIDUE,
            &RedactionResidue::Block(BlockResidue {
                version: h(90),
                id: id("c-00001"),
                block_type: t("residue.block_type"),
                parent: Some(id("s-00002")),
                order: t("residue.order"),
                content_hash: h(91),
                frozen: true,
            }),
            vec![],
        ),
        s(
            "residue change",
            REDACTION_RESIDUE,
            &RedactionResidue::Change(change_residue()),
            vec![
                some(BLOCK, 93),
                some(BLOCK, 94),
                some(BLOCK, 95),
                some(BLOCK, 96),
                some(CHANGE, 97),
                some(INTENT, 98),
                some(ALT_SET, 99),
            ],
        ),
        s(
            "redaction apply",
            REDACTION_APPLY,
            &RedactionApply {
                request: h(100),
                landing: h(101),
                report_hash: h(102),
            },
            vec![some(REDACTION, 100), some(LANDING, 101)],
        ),
    ];
    // every cause, on a tension move (resolved and opened) and a suspect move (gone and new)
    for (cause, n, kind) in causes {
        let mut edges = vec![some(CHANGE, 71), some(LANDING, 72)];
        if let (Some(n), Some(kind)) = (n, kind) {
            edges.push(some(kind, n));
        }
        let mut record = transitions(vec![tension_move(70, why(cause, n))], vec![]);
        out.push(s("transition tension", TRANSITION, &record, edges.clone()));
        std::mem::swap(&mut record.resolved, &mut record.opened);
        out.push(s("transition opened", TRANSITION, &record, edges));
        let mut edges = vec![];
        if let (Some(n), Some(kind)) = (n, kind) {
            edges.push(some(kind, n));
        }
        let mut record = transitions(vec![], vec![suspect_move(why(cause, n))]);
        out.push(s("transition suspect", TRANSITION, &record, edges.clone()));
        std::mem::swap(&mut record.gone, &mut record.new);
        out.push(s("transition new suspect", TRANSITION, &record, edges));
    }
    out.extend(plan_samples());
    out
}

/// A plan version's sealed item: its identity, subject and cause are no edges; its accepting
/// answer is.
fn sealed_item(id: u8, answer: Option<u8>) -> SealedItem {
    SealedItem {
        id: h(id),
        kind: PlanKind::Reaffirm,
        subject: "b-00001".into(),
        cause: id_of("D-00002"),
        edge: "reflects".into(),
        outcome: if answer.is_some() {
            PlanOutcome::Deferred
        } else {
            PlanOutcome::Open
        },
        answer: answer.map(h),
    }
}

fn id_of(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A plan version with the chain links, bound proposal and seal given.
fn plan(
    stable: Option<u8>,
    prev: Option<u8>,
    proposal: Option<u8>,
    seal: Option<PlanSeal>,
) -> Plan {
    Plan {
        stable: stable.map(h),
        prev: prev.map(h),
        view: "work".into(),
        proposal: proposal.map(h),
        answers: vec![PlanAnswer {
            item: h(120),
            act: PlanAct::Defer,
            reason: Some("later".into()),
            subject: Some(h(121)),
            cause: h(122),
            by: Provenance {
                actor: "agent:w".into(),
                run: None,
                role: Role::Writer,
                at_ms: 5,
            },
        }],
        heads: vec![PlanHead {
            repo: "app".into(),
            head: Some("c".repeat(40)),
        }],
        provenance: Provenance {
            actor: "agent:w".into(),
            run: None,
            role: Role::Writer,
            at_ms: 5,
        },
        seal,
    }
}

/// The PLAN samples: a first version, a bound later version, and a seal whose items name an
/// accepting answer and none. No text marker: a plan's text is bounded by the decoder alone.
fn plan_samples() -> Vec<Sample> {
    let seal = PlanSeal {
        landing: h(114),
        version: h(113),
        decisions: vec![id_of("D-00002")],
        items: vec![sealed_item(123, None), sealed_item(124, Some(115))],
    };
    vec![
        s("plan first", PLAN, &plan(None, None, None, None), vec![]),
        s(
            "plan bound",
            PLAN,
            &plan(Some(110), Some(111), Some(112), None),
            vec![some(PLAN, 110), some(PLAN, 111), some(PROPOSAL, 112)],
        ),
        s(
            "plan seal",
            PLAN,
            &plan(Some(110), Some(111), Some(113), Some(seal)),
            vec![
                some(PLAN, 110),
                some(PLAN, 111),
                some(PROPOSAL, 113),
                some(LANDING, 114),
                some(ANSWER, 115),
            ],
        ),
    ]
}

fn sorted(mut v: Vec<ObjectRef>) -> Vec<ObjectRef> {
    v.sort();
    v.dedup();
    v
}

#[test]
fn ha1_refs_cover_every_assigned_object_kind() {
    let all = samples();
    for sample in &all {
        assert_eq!(
            refs_of(sample.kind, &sample.bytes),
            Ok(sorted(sample.edges.clone())),
            "{}",
            sample.arm
        );
    }
    let covered: BTreeSet<u8> = all.iter().map(|s| s.kind).collect();
    let assigned: BTreeSet<u8> = (1..=7)
        .chain([9, 10, 11, 13])
        .chain(15..=22)
        .chain([24])
        .collect();
    assert_eq!(covered, assigned);
    assert_eq!(
        PORTABLE_KINDS.iter().copied().collect::<BTreeSet<u8>>(),
        assigned
    );

    // A figure's present blob must be a full hash.
    let bad = block(
        "block",
        "figure",
        BTreeMap::from([("blob".to_string(), FieldValue::Str("abc".into()))]),
    );
    assert_eq!(refs_of(BLOCK, &enc(&bad)), Err(RecordError::Decode));
    let upper = block(
        "block",
        "figure",
        BTreeMap::from([(
            "blob".to_string(),
            FieldValue::Str(h(0xab).to_string().to_uppercase()),
        )]),
    );
    assert_eq!(refs_of(BLOCK, &enc(&upper)), Err(RecordError::Decode));
    let not_text = block(
        "block",
        "figure",
        BTreeMap::from([("blob".to_string(), FieldValue::Int(1))]),
    );
    assert_eq!(refs_of(BLOCK, &enc(&not_text)), Err(RecordError::Decode));
    // The same malformed figure embedded in a change.
    let embeds = change(
        "change",
        vec![Op::Create { block: bad }],
        ChangeLinks::default(),
        Justification::Implements(vec![]),
    );
    assert_eq!(refs_of(CHANGE, &enc(&embeds)), Err(RecordError::Decode));
    // A span names its object by a portable kind only.
    let mut r = redaction();
    r.manifest.spans[0].kind = 12;
    assert_eq!(refs_of(REDACTION, &enc(&r)), Err(RecordError::Decode));
    r.manifest.spans[0].kind = REF_UPDATE;
    assert_eq!(refs_of(REDACTION, &enc(&r)), Err(RecordError::Decode));
}

#[test]
fn ha1_identity_hashes_are_not_dependencies() {
    // Hashes the samples hold only as identities: pins, card hashes, residue identities,
    // report hashes, pair and content keys, review case and tension ids, conflict ids, re-keys,
    // and a hash spelled in report text.
    let identities: BTreeSet<Hash> = [
        250, 28, 35, 51, 61, 62, 63, 64, 65, 66, 67, 70, 73, 89, 90, 91, 92, 102,
    ]
    .into_iter()
    .map(h)
    .collect();
    for sample in samples() {
        let refs = refs_of(sample.kind, &sample.bytes).unwrap();
        for r in &refs {
            assert!(
                !identities.contains(&r.hash),
                "{}: identity {:?} became an edge",
                sample.arm,
                r.hash
            );
        }
    }
    // A pin-only block, the relayed tag answer, a block residue and a review have no edges.
    let mut pinned = block("block", "claim", BTreeMap::new());
    pinned.pins.insert(id("e-00005"), h(1));
    assert_eq!(refs_of(BLOCK, &enc(&pinned)), Ok(vec![]));
    // An answer's item and card hash add none; only its targets do.
    let answer = Answer {
        item: format!("tag:{}", h(3)),
        act: AnswerAct::Tag,
        option: None,
        label: None,
        options: vec![],
        card_hash: Some(h(2)),
        targets: vec![h(4)],
        person: prov("answer"),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: None,
    };
    assert_eq!(refs_of(ANSWER, &enc(&answer)), Ok(vec![any(4)]));
    // A change residue's own id is not a dependency of it.
    let refs = refs_of(
        REDACTION_RESIDUE,
        &enc(&RedactionResidue::Change(change_residue())),
    );
    assert!(!refs.unwrap().iter().any(|r| r.hash == h(92)));
}

/// Every variant of `$ty`, listed through an exhaustive match: a new variant does not compile
/// until it is listed here and so sampled below.
macro_rules! every {
    ($ty:ident: $($v:ident),+ $(,)?) => {{
        let all = vec![$($ty::$v),+];
        for v in &all {
            match v {
                $($ty::$v)|+ => {}
            }
        }
        all
    }};
}

#[test]
fn ha1_refs_cover_every_edge_free_enum_arm() {
    // An arm that holds no hash adds no edge: each record below has the edges of its base, for
    // every variant of every closed enum it holds.
    let answer = |act: AnswerAct, role: Role, evidence: Option<EvidenceKind>| Answer {
        item: t("answer.item"),
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: Some(h(35)),
        targets: vec![h(36), h(37)],
        person: Provenance {
            role,
            ..prov("answer.person")
        },
        relayed_by: Some(actor_run("answer.relayed_by")),
        relay_evidence: Some(relay("answer.relay")),
        evidence_kind: evidence,
        note: None,
    };
    let answer_edges = vec![any(36), any(37)];
    let acts = every!(AnswerAct: RejectProposal, Later, Confirm, RejectDecision, AnswerQuestion,
        Dismiss, Tag, ViewOwn, CloseStale, Propose, KeepSuspect, AcceptDeferrals, RefuseDeferrals,
        RejectReaffirm);
    let roles = every!(Role: Reader, Writer, Reviewer, Approver);
    let evidence = every!(EvidenceKind: Elicited, Card, Text);
    assert_eq!((acts.len(), roles.len(), evidence.len()), (14, 4, 3));
    for act in &acts {
        let a = answer(*act, Role::Approver, Some(EvidenceKind::Text));
        assert_eq!(
            refs_of(ANSWER, &enc(&a)),
            Ok(answer_edges.clone()),
            "{act:?}"
        );
    }
    for role in &roles {
        let a = answer(AnswerAct::Confirm, *role, None);
        assert_eq!(
            refs_of(ANSWER, &enc(&a)),
            Ok(answer_edges.clone()),
            "{role:?}"
        );
    }
    for kind in evidence.iter().copied().map(Some).chain([None]) {
        let a = answer(AnswerAct::Confirm, Role::Approver, kind);
        assert_eq!(
            refs_of(ANSWER, &enc(&a)),
            Ok(answer_edges.clone()),
            "{kind:?}"
        );
    }

    // a landing's assurance and evidence kind
    let landing = |assurance: Assurance, kind: EvidenceKind| LandingRecord {
        proposal: h(20),
        prev: None,
        state: h(22),
        approved_by: prov("landing.approved_by"),
        changes: vec![h(23)],
        chosen: None,
        checks: vec![],
        assurance,
        relayed_by: None,
        relay_evidence: None,
        approved_version: None,
        rebased_from: vec![],
        evidence: Some(ApprovalEvidence {
            kind,
            options: vec![],
            choice: None,
            card_hash: Some(h(28)),
        }),
    };
    let landing_edges = vec![some(PROPOSAL, 20), some(SNAPSHOT, 22), some(CHANGE, 23)];
    let assurances = every!(Assurance: Policy, Relayed, Local);
    assert_eq!(assurances.len(), 3);
    for assurance in &assurances {
        for kind in &evidence {
            let l = landing(*assurance, *kind);
            assert_eq!(
                refs_of(LANDING, &enc(&l)),
                Ok(sorted(landing_edges.clone())),
                "{assurance:?} {kind:?}"
            );
        }
    }

    // a change's editorial class, and a change residue's
    let classes = every!(EditorialClass: Typo, Restructure, Clarify);
    assert_eq!(classes.len(), 3);
    for class in &classes {
        let editorial = Justification::Editorial {
            class: *class,
            reason: t("change.reason"),
        };
        let c = change("change", vec![], ChangeLinks::default(), editorial.clone());
        assert_eq!(
            refs_of(CHANGE, &enc(&c)),
            Ok(vec![some(CHANGE, 5)]),
            "{class:?}"
        );
        let mut residue = change_residue();
        residue.ops.clear();
        residue.links = ChangeLinks::default();
        residue.justification = Some(editorial);
        assert_eq!(
            refs_of(REDACTION_RESIDUE, &enc(&RedactionResidue::Change(residue))),
            Ok(vec![some(CHANGE, 97)]),
            "residue {class:?}"
        );
    }

    // a request's reason class and its report rows
    let reasons = every!(ReasonClass: Secret, Personal, Legal, Retraction);
    let impacts = every!(ImpactKind: BlockVersion, Change, Landing, Tag, View, Proposal,
        StateSnapshot, AuditText, Review, Tension, Answer);
    let relations = every!(ImpactRelation: Removed, Embeds, References, Quotes);
    let copies = every!(CopyKind: Render, Export, Dump, Clone, Published, WorkingCopy);
    let statuses = every!(CopyStatus: Unknown);
    let support = every!(AssessmentSupport: NotImplemented);
    assert_eq!(
        (
            reasons.len(),
            impacts.len(),
            relations.len(),
            copies.len(),
            statuses.len(),
            support.len()
        ),
        (4, 11, 4, 6, 1, 1)
    );
    let request = |reason_class: ReasonClass, inside: Vec<ImpactRow>, outside: Vec<CopyRow>| {
        let mut r = redaction();
        r.targets = vec![RedactTarget::AllVersions(id("c-00001"))];
        r.reason_class = reason_class;
        r.manifest.versions = vec![];
        r.manifest.blobs = vec![];
        r.manifest.shared_blobs = vec![];
        r.manifest.spans = vec![];
        r.manifest.inside = inside;
        r.manifest.outside = outside;
        r.manifest.assessment = support[0];
        r
    };
    let request_edges = vec![some(CHANGE, 80)];
    for reason in &reasons {
        let r = request(*reason, vec![], vec![]);
        assert_eq!(
            refs_of(REDACTION, &enc(&r)),
            Ok(request_edges.clone()),
            "{reason:?}"
        );
    }
    let mut inside = vec![];
    for kind in &impacts {
        for relation in &relations {
            inside.push(ImpactRow {
                kind: *kind,
                // report text, even when it spells a hash
                record: h(89).to_string(),
                relation: *relation,
                erasable: true,
            });
        }
    }
    let outside: Vec<CopyRow> = copies
        .iter()
        .flat_map(|kind| {
            statuses.iter().map(|status| CopyRow {
                kind: *kind,
                status: *status,
            })
        })
        .collect();
    let r = request(ReasonClass::Secret, inside, outside);
    assert_eq!(refs_of(REDACTION, &enc(&r)), Ok(request_edges));
}

#[test]
fn ha1_refs_are_sorted_and_unique() {
    let landing = LandingRecord {
        proposal: h(1),
        prev: Some(h(9)),
        state: h(2),
        approved_by: prov("landing"),
        changes: vec![h(1), h(3), h(1)],
        chosen: None,
        checks: vec![h(1), h(1), h(0)],
        assurance: Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        approved_version: Some(h(1)),
        rebased_from: vec![h(9), h(8)],
        evidence: None,
    };
    assert_eq!(
        refs_of(LANDING, &enc(&landing)),
        Ok(vec![
            any(0),
            any(1),
            some(CHANGE, 1),
            some(CHANGE, 3),
            some(PROPOSAL, 1),
            some(LANDING, 8),
            some(LANDING, 9),
            some(SNAPSHOT, 2),
        ])
    );
    let edit = Change {
        deps: vec![h(5), h(5), h(3)],
        ops: vec![
            Op::Delete {
                base: h(9),
                id: id("c-00001"),
            },
            Op::Delete {
                base: h(9),
                id: id("d-00003"),
            },
        ],
        links: ChangeLinks {
            intent: Some(h(3)),
            ..ChangeLinks::default()
        },
        ..change(
            "change",
            vec![],
            ChangeLinks::default(),
            Justification::Implements(vec![]),
        )
    };
    let refs = refs_of(CHANGE, &enc(&edit)).unwrap();
    assert_eq!(
        refs,
        vec![
            some(BLOCK, 9),
            some(CHANGE, 3),
            some(CHANGE, 5),
            some(INTENT, 3)
        ]
    );
    for sample in samples() {
        let refs = refs_of(sample.kind, &sample.bytes).unwrap();
        assert!(
            refs.windows(2).all(|w| w[0] < w[1]),
            "{}: not strictly ascending",
            sample.arm
        );
    }
}

#[test]
fn ha1_object_key_json_names_kind_and_hex_hash() {
    let key = ObjectKey {
        kind: LANDING,
        hash: h(0xab),
    };
    let hex = "ab".repeat(32);
    let json = serde_json::to_string(&key).unwrap();
    assert_eq!(json, format!(r#"{{"kind":4,"hash":"{hex}"}}"#));
    assert_eq!(serde_json::from_str::<ObjectKey>(&json).unwrap(), key);
    assert!(
        serde_json::from_str::<ObjectKey>(&format!(r#"{{"kind":4,"hash":"{hex}","x":1}}"#))
            .is_err()
    );
    assert!(serde_json::from_str::<ObjectKey>(r#"{"kind":4,"hash":"ab"}"#).is_err());
    assert_eq!(key.to_string(), format!("4:{hex}"));
    // The model's own Hash serialization is unchanged: an array of bytes in JSON, raw in postcard.
    assert!(serde_json::to_string(&h(1)).unwrap().starts_with("[1,1,"));
    assert_eq!(enc(&h(7)), vec![7u8; 32]);
    // Binary formats keep the hash's own bytes.
    let mut bin = vec![LANDING];
    bin.extend([0xab; 32]);
    assert_eq!(enc(&key), bin);
    // Ordered by kind, then hash.
    let mut keys = [
        ObjectKey {
            kind: 2,
            hash: h(0),
        },
        ObjectKey {
            kind: 1,
            hash: h(9),
        },
        ObjectKey {
            kind: 1,
            hash: h(3),
        },
    ];
    keys.sort();
    assert_eq!(
        keys.iter()
            .map(|k| (k.kind, k.hash.0[0]))
            .collect::<Vec<_>>(),
        vec![(1, 3), (1, 9), (2, 0)]
    );
}

#[test]
fn ha1_unknown_and_local_effect_kinds_are_refused() {
    let tag = enc(&Tag {
        name: "v1".into(),
        landing: h(1),
        message: None,
        tagged_by: prov("tag"),
    });
    let ref_update = enc(&kinds::RefUpdate {
        name: "canon".into(),
        value: vec![1; 32],
        at: rdy_store::segment::Location {
            segment: 0,
            offset: 0,
        },
    });
    let commit = enc(&kinds::BatchCommit {
        start: rdy_store::segment::Location {
            segment: 0,
            offset: 0,
        },
        frames: 1,
        digest: h(2),
    });
    for kind in [0u8, 8, 12, 14, 23, 255] {
        for payload in [&tag, &ref_update, &commit, &vec![]] {
            assert_eq!(
                refs_of(kind, payload),
                Err(RecordError::Kind),
                "kind {kind}"
            );
        }
    }
    // REF_UPDATE and BATCH_COMMIT are their store constants.
    assert_eq!((REF_UPDATE, BATCH_COMMIT), (8, 14));
}

/// Every position of a text marker in `bytes`: its length byte's index and the marker's length.
fn markers(bytes: &[u8]) -> Vec<(usize, usize, String)> {
    let mut out = vec![];
    for i in 0..bytes.len() {
        let len = usize::from(bytes[i]);
        if !(3..128).contains(&len) || i + 1 + len > bytes.len() {
            continue;
        }
        let text = &bytes[i + 1..i + 1 + len];
        let inner = &text[1..len - 1];
        if text[0] == b'~' && text[len - 1] == b'~' && !inner.contains(&b'~') {
            out.push((i, len, String::from_utf8(inner.to_vec()).unwrap()));
        }
    }
    out
}

/// `bytes` with the marker at `at` (of length `len`) replaced by a text of `n` bytes.
fn widen(bytes: &[u8], at: usize, len: usize, n: usize) -> Vec<u8> {
    let mut out = bytes[..at].to_vec();
    let mut v = n as u64;
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
    out.extend(std::iter::repeat_n(b'x', n));
    out.extend(&bytes[at + 1 + len..]);
    out
}

#[test]
fn ha1_every_stored_text_field_is_bounded() {
    let mut seen = BTreeSet::new();
    for sample in samples() {
        for (at, len, name) in markers(&sample.bytes) {
            seen.insert(name.clone());
            let exact = widen(&sample.bytes, at, len, 1_048_576);
            assert!(
                refs_of(sample.kind, &exact).is_ok(),
                "{}: {name} at the bound",
                sample.arm
            );
            let over = widen(&sample.bytes, at, len, 1_048_577);
            assert_eq!(
                refs_of(sample.kind, &over),
                Err(RecordError::Limit {
                    name: TEXT_BYTES,
                    maximum: 1_048_576
                }),
                "{}: {name} over the bound",
                sample.arm
            );
        }
    }
    // Every stored text field, by sample: summaries, reasons, relay evidence and every other.
    let mut expected: BTreeSet<String> = BTreeSet::new();
    let block_fields = [
        "order",
        "body",
        "field_key",
        "field_value",
        "list_item",
        "map_key",
        "map_value",
        "link_key",
        "link_part",
    ];
    for of in ["block", "change.block"] {
        expected.extend(block_fields.iter().map(|f| format!("{of}.{f}")));
    }
    expected.insert("block.type".into());
    let provs = [
        "change",
        "proposal",
        "landing.approved_by",
        "tag",
        "answer.person",
        "signature",
        "alt_set",
        "review",
        "tension",
        "redaction",
        "residue",
    ];
    let actor_runs = [
        "change.alt.imported_by",
        "landing.relayed_by",
        "answer.relayed_by",
        "tension.relayed_by",
        "residue.imported_by",
        "view_op.owner",
        "view_op",
        "intent",
        "transition.side",
        "transition.why",
        "transition.writer",
    ];
    for p in provs.iter().chain(&actor_runs) {
        expected.insert(format!("{p}.actor"));
        expected.insert(format!("{p}.run"));
    }
    for r in ["landing.relay", "answer.relay", "tension.relay"] {
        for f in ["quoted", "session_ref", "question"] {
            expected.insert(format!("{r}.{f}"));
        }
    }
    expected.extend(
        [
            "change.view",
            "change.message",
            "change.reason",
            "change.alt.member",
            "change.alt.client_nonce",
            "proposal.title",
            "proposal.view",
            "proposal.alt.member",
            "proposal.summary",
            "landing.chosen.member",
            "landing.evidence.option",
            "landing.evidence.choice",
            "tag.name",
            "tag.message",
            "answer.item",
            "answer.option",
            "answer.label",
            "answer.options",
            "answer.note",
            "signature.namespace",
            "view_op.view",
            "view_op.fork.from",
            "view_op.close.reason",
            "intent.view",
            "intent.area",
            "intent.angle",
            "alt_set.title",
            "alt_set.member.label",
            "alt_set.member.view",
            "alt_set.chosen.member",
            "alt_set.abandoned.reason",
            "review.reason",
            "review.finding.note",
            "review.tension.kind",
            "review.tension.severity",
            "review.tension.side",
            "tension.advice",
            "transition.kind",
            "transition.severity",
            "transition.suspect.reason",
            "redaction.reason",
            "redaction.impact.record",
            "redaction.quarantine",
            "residue.block_type",
            "residue.order",
            "residue.view",
            "residue.message",
            "residue.reason",
            "residue.alt.member",
            "residue.client_nonce",
        ]
        .map(String::from),
    );
    assert_eq!(seen, expected);
}

#[test]
fn cp3_plan_refs_are_typed_and_identity_hashes_are_not_edges() {
    // the complete edge set of a sealed version: its chain as PLAN, its bound proposal and the
    // seal's landed version as PROPOSAL, the seal's landing as LANDING, each accepting answer as
    // ANSWER, sorted and once each
    let seal = PlanSeal {
        landing: h(114),
        version: h(113),
        decisions: vec![id_of("D-00002"), id_of("D-00003")],
        items: vec![
            sealed_item(123, None),
            sealed_item(124, Some(115)),
            sealed_item(125, Some(116)),
            sealed_item(126, Some(115)),
        ],
    };
    let sealed = plan(Some(110), Some(111), Some(113), Some(seal));
    assert_eq!(
        refs_of(PLAN, &enc(&sealed)),
        Ok(vec![
            some(PROPOSAL, 113),
            some(LANDING, 114),
            some(ANSWER, 115),
            some(ANSWER, 116),
            some(PLAN, 110),
            some(PLAN, 111),
        ])
    );
    // the identities a version holds: item hashes, subject and cause content hashes, sealed
    // item ids, decision and cause block ids, git heads; none is an edge
    let identities: BTreeSet<Hash> = [120, 121, 122, 123, 124, 125, 126]
        .into_iter()
        .map(h)
        .chain([Hash::of(b"c".repeat(40).as_slice())])
        .collect();
    for sample in plan_samples() {
        for r in refs_of(sample.kind, &sample.bytes).unwrap() {
            assert!(!identities.contains(&r.hash), "{}: {r:?}", sample.arm);
            assert!(r.kind.is_some(), "{}: every plan edge is typed", sample.arm);
        }
    }
    // a first version names nothing; the same link twice is one edge
    assert_eq!(
        refs_of(PLAN, &enc(&plan(None, None, None, None))),
        Ok(vec![])
    );
    assert_eq!(
        refs_of(PLAN, &enc(&plan(Some(110), Some(110), None, None))),
        Ok(vec![some(PLAN, 110)])
    );
    // a version that is not its own canonical encoding, or holds a byte past it, is refused
    let mut trailing = enc(&sealed);
    trailing.push(0);
    assert_eq!(refs_of(PLAN, &trailing), Err(RecordError::Decode));
    // PLAN is a portable kind; kind 25 is not
    assert!(PORTABLE_KINDS.contains(&PLAN));
    assert_eq!(refs_of(25, &enc(&sealed)), Err(RecordError::Kind));
}
