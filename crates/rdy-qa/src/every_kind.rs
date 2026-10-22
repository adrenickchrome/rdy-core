//! The every-kind project: one project that holds an object of every kind of dump format 2 (store
//! format 14 and 15) and a ref of every project family, and one read of it ("facts") that a
//! later rdy can repeat. The project is a clone, so it holds `meta/origin` (O41). Its source's
//! last landing is a person's approval of a content removal, so its log holds dropped payloads
//! (D-3M81W). Shared by the sg6 store test and the dump, load and clone tests (D-69AY3, D-EV4F1).
//!
//! The every-kind holder (store format 15, D-BPYN8): a holder filled through real exports, batch
//! checks and all-files takes from two projects of one repo whose histories diverge, with a later
//! approved removal that both histories share ([`holder`]). It holds the refs only a holder has
//! (`meta/store_mode`, `answer/tag-object:<hash>`), and its removals are authorized by logical
//! proof, never by a source place. The v2-s15 holder fixture and the v1-s15 sync fixture are
//! written from it.
use crate::fixtures::*;
use crate::inbox::on;
use crate::intake::{PERSON, confirm_exceptions, decide_in};
use crate::packs::qa_project;
use crate::signing::{TestKey, sign_record};
use crate::tensions::{ag, created, one_involving, put, upd};
use rdy_engine::alt::AltMemberInput;
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::clone::{CloneSource, clone};
use rdy_engine::inbox::api::{InboxRequest, run_inbox};
use rdy_engine::inbox::sub::subscribe;
use rdy_engine::mention_review::MentionVerdict;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::redaction::RedactInput;
use rdy_engine::signature_check::{check_signature, read_check_context, signature_checks};
use rdy_engine::stream_check::check_batch;
use rdy_engine::stream_take::take;
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::tensions::{TensionKind, tension_label};
use rdy_engine::{DecisionInput, Engine, LandOptions, Local, Rdy, Scope, Session};
use rdy_model::plan::{
    Plan, PlanAct, PlanAnswer, PlanHead, PlanKind, PlanOutcome, PlanSeal, SealedItem, item_hash,
};
use rdy_model::redaction::{Redaction, RedactionApply};
use rdy_model::signature::{Signature, StatementSource};
use rdy_model::{
    ActorRun, Answer, AnswerAct, Assurance, BlockId, Finding, Hash, PairItem, Provenance,
    RelayEvidence, RepoId, Review, ReviewSubject, Role, SemanticTension, Verdict, pair_key,
};
use rdy_store::Relay;
use rdy_store::holder::StoreMode;
use rdy_store::kinds;
use rdy_store::object_refs::{ObjectKey, ObjectRef};
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::{Location, SegmentLog};
use rdy_store::sync_stream::{SYNC_KINDS, StreamHeader, StreamReader, StreamRecord, StreamWriter};
use rdy_store::take_batch::UploadTarget;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};

/// The ref families of an every-kind project (store formats 14 and 15): the ten of an M2 store,
/// `canon`, the open proposals (`proposal/`), the answers (`answer/`), the snoozes (`pending/`),
/// the repo id (`meta/repo_id`) and the clone's origin (`meta/origin`). Store formats 13 and 14
/// add kinds and no family: a redaction and a signature have no ref. Store format 15 adds two
/// families that only a holder holds (`meta/store_mode`, `answer/tag-object:`), so none here. A
/// store format that adds a project family adds it here (store format 17: `plan/`, the change
/// plans); each frozen fixture test keeps its own
/// list (`V1_FAMILIES` in `io_dump_fixture.rs`, `V11_FAMILIES` in `io_dump_fixture_s11.rs`,
/// `V12_FAMILIES` in `io_dump_fixture_s12.rs`, `V13_FAMILIES` in `io_dump_fixture_s13.rs`,
/// `V14_FAMILIES` in `io_dump_fixture_s14.rs`, `V15_FAMILIES` and `HOLDER_FAMILIES` in
/// `io_dump_fixture_s15.rs`).
pub const FAMILIES: [&str; 17] = [
    "meta/store_format",
    "meta/repo_id",
    "meta/origin",
    "view/",
    "tag/",
    "intent/",
    "alt/",
    "dismiss/",
    "mention/",
    "pair/",
    "semantic/",
    "inbox/",
    "canon",
    "proposal/",
    "answer/",
    "pending/",
    "plan/",
];

/// The object kinds of dump format 1 with store format 10 that the project holds: every kind but
/// the batch marker. Frozen with the v1-s10 fixture.
pub const V1_KINDS: [u8; 15] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::REF_UPDATE,
    kinds::TAG,
    kinds::VIEW_OP,
    kinds::INTENT,
    kinds::ALT_SET,
    kinds::REVIEW,
    kinds::TENSION,
    kinds::TRANSITION,
];

/// The object kinds of dump format 1 with store format 11 that the project holds: `V1_KINDS` and
/// `ANSWER`. Frozen with the v1-s11 fixture.
pub const V11_KINDS: [u8; 16] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::REF_UPDATE,
    kinds::TAG,
    kinds::ANSWER,
    kinds::VIEW_OP,
    kinds::INTENT,
    kinds::ALT_SET,
    kinds::REVIEW,
    kinds::TENSION,
    kinds::TRANSITION,
];

/// The object kinds of dump format 1 with store format 12 that the project holds: `V11_KINDS`
/// (store format 12 adds refs, no kind). Frozen with the v1-s12 fixture.
pub const V12_KINDS: [u8; 16] = V11_KINDS;

/// The object kinds of dump format 2 with store format 13 that the project holds: `V12_KINDS`
/// and the three redaction kinds (a request, a residue, an apply). Frozen with the v2-s13
/// fixture.
pub const V13_KINDS: [u8; 19] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::REF_UPDATE,
    kinds::TAG,
    kinds::ANSWER,
    kinds::VIEW_OP,
    kinds::INTENT,
    kinds::ALT_SET,
    kinds::REVIEW,
    kinds::TENSION,
    kinds::TRANSITION,
    kinds::REDACTION,
    kinds::REDACTION_RESIDUE,
    kinds::REDACTION_APPLY,
];

/// The object kinds of dump format 2 with store format 14 that the project holds: `V13_KINDS`
/// and `SIGNATURE` (kind 11), in numeric order. Store format 14 adds that kind and no ref family.
/// Frozen with the v2-s14 fixture.
pub const V14_KINDS: [u8; 20] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::REF_UPDATE,
    kinds::TAG,
    kinds::ANSWER,
    kinds::SIGNATURE,
    kinds::VIEW_OP,
    kinds::INTENT,
    kinds::ALT_SET,
    kinds::REVIEW,
    kinds::TENSION,
    kinds::TRANSITION,
    kinds::REDACTION,
    kinds::REDACTION_RESIDUE,
    kinds::REDACTION_APPLY,
];

/// The object kinds of dump format 2 with store format 15 that the project holds: `V14_KINDS`.
/// Store format 15 adds refs (a holder's `meta/store_mode` and `answer/tag-object:<hash>`), no
/// kind; kind 12 stays unassigned. Frozen with the v2-s15 fixtures.
pub const V15_KINDS: [u8; 20] = [
    kinds::BLOCK,
    kinds::CHANGE,
    kinds::PROPOSAL,
    kinds::LANDING,
    kinds::SNAPSHOT,
    kinds::BLOB_CHUNK,
    kinds::BLOB_MANIFEST,
    kinds::REF_UPDATE,
    kinds::TAG,
    kinds::ANSWER,
    kinds::SIGNATURE,
    kinds::VIEW_OP,
    kinds::INTENT,
    kinds::ALT_SET,
    kinds::REVIEW,
    kinds::TENSION,
    kinds::TRANSITION,
    kinds::REDACTION,
    kinds::REDACTION_RESIDUE,
    kinds::REDACTION_APPLY,
];

/// The removed term's body: a selected canary, held only by the removed payloads.
pub const CANARY_BODY: &str = "Kestrel body canary.";
/// The removed term's name field: a selected canary, held only by the removed payloads.
pub const CANARY_FIELD: &str = "Kestrel field canary";
/// The removed term's alias: a source string that a review finding, a tension's advice and an
/// answer's note and relayed words quote whole. Those records are kept (D-EPK1Z, RULING F3 (a)).
pub const QUOTED_ALIAS: &str = "Osprey quoted alias";
/// The body of the claim the term's change creates beside it, which no request selects.
pub const SIBLING_BODY: &str = "The sibling claim stays readable.";

/// The blocks of the seeded canon doc.
#[derive(Clone)]
struct Seed {
    section: BlockId,
    /// Rebased with a conflict.
    k1: BlockId,
    /// Two views collide on it; the tension is dismissed.
    k2: BlockId,
    /// The semantic pair.
    k3: BlockId,
    k4: BlockId,
    /// An intent names it.
    k5: BlockId,
    /// The entity the unlinked mention names.
    entity: BlockId,
    mention: BlockId,
}

/// Canon: doc `m2` (overview) with section `Rules`: claims k1..k5, entity `Ledger service` (alias
/// ledger), and a claim that names the ledger with no link. Landed by the person.
fn seed(r: &mut Rdy) -> Seed {
    let w = write(r, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "m2", "title": "M2", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
        {"op": "create", "type": "claim", "id": "$k1", "parent": "$s", "body": "One holds."},
        {"op": "create", "type": "claim", "id": "$k2", "parent": "$s", "after": "$k1", "body": "Two holds."},
        {"op": "create", "type": "claim", "id": "$k3", "parent": "$s", "after": "$k2", "body": "Three holds."},
        {"op": "create", "type": "claim", "id": "$k4", "parent": "$s", "after": "$k3", "body": "Four holds."},
        {"op": "create", "type": "claim", "id": "$k5", "parent": "$s", "after": "$k4", "body": "Five holds."},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "after": "$k5", "fields": {"name": "Ledger service", "aliases": ["ledger"]}},
        {"op": "create", "type": "claim", "id": "$m", "parent": "$s", "after": "$e", "body": "The ledger is copied nightly."}
    ]))
    .expect("seed the m2 doc");
    land(r, "work").expect("land the seed");
    Seed {
        section: created(&w, "$s"),
        k1: created(&w, "$k1"),
        k2: created(&w, "$k2"),
        k3: created(&w, "$k3"),
        k4: created(&w, "$k4"),
        k5: created(&w, "$k5"),
        entity: created(&w, "$e"),
        mention: created(&w, "$m"),
    }
}

/// One create op: a claim under `parent`.
fn claim(parent: &BlockId, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent.to_string(), "body": body}])
}

/// Records agent:r's `tension` verdict on `x` and `y` at their contents in flight, advising
/// blocking (as `submit_verdict` records it; as se8 does).
fn advise_blocking(r: &mut Rdy, x: &BlockId, y: &BlockId) {
    verdict(
        r,
        x,
        y,
        Hash([9; 32]),
        "three and four cannot both hold",
        "four contradicts three",
    );
}

/// Records agent:r's `tension` verdict on `x` and `y` at their contents in flight, for case
/// `case`: one finding on `y` with `note`, and a semantic tension with `advice` that does not
/// block (a REVIEW and a TENSION whose text a removal report can find as quotes).
pub fn verdict(r: &mut Rdy, x: &BlockId, y: &BlockId, case: Hash, note: &str, advice: &str) {
    verdict_with_reason(r, x, y, case, note, advice, "");
}

/// `verdict` with the review's own `reason` text, and the stored review's hash back (the hash the
/// semantic tension names, so a caller can read the record again).
pub fn verdict_with_reason(
    r: &mut Rdy,
    x: &BlockId,
    y: &BlockId,
    case: Hash,
    note: &str,
    advice: &str,
    reason: &str,
) -> Hash {
    let m = r.read().materialized(&Scope::Inflight).unwrap();
    let content = |id: &BlockId| m.get(id).unwrap().content_hash_with(r.store().tracking());
    let mut items = vec![
        PairItem {
            block: x.clone(),
            content: content(x),
        },
        PairItem {
            block: y.clone(),
            content: content(y),
        },
    ];
    items.sort();
    let key = pair_key(&items);
    let review = Review {
        subject: ReviewSubject::Pair {
            key,
            items: items.clone(),
        },
        case_hash: Some(case),
        verdict: Verdict::Tension,
        reason: reason.into(),
        findings: vec![Finding {
            block: y.clone(),
            note: note.into(),
        }],
        provenance: Provenance {
            actor: "agent:r".into(),
            run: Some("r".into()),
            role: Role::Reviewer,
            at_ms: 1,
        },
    };
    let review_hash = Hash::of(&postcard::to_allocvec(&review).unwrap());
    let first = SemanticTension {
        key,
        items,
        review: review_hash,
        advice: Some(advice.into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: review.provenance.clone(),
    };
    r.record_verdict(&review, Some(&first)).unwrap();
    review_hash
}

/// The person's approve item of proposal version `r#ref` in their queue.
fn approve_item(r: &Rdy, r#ref: &str) -> rdy_engine::pending::Item {
    let q = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    q.items
        .iter()
        .find(|i| i.kind == ItemKind::Approve && i.r#ref == r#ref)
        .unwrap_or_else(|| panic!("the approve item {ref}: {q:#?}"))
        .clone()
}

/// A canon export of another project: doc `elsewhere` with one prose block and one figure (an
/// asset, so the export holds a blob and an import logs `BLOB_CHUNK` and `BLOB_MANIFEST`).
fn foreign_export(out: &Path) {
    let d = tempfile::tempdir().unwrap();
    let mut o = Rdy::init(d.path(), "rdy-standard").unwrap();
    let blob = o
        .store_mut()
        .put_blob(b"\x89PNG every kind")
        .unwrap()
        .to_string();
    write(&mut o, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "elsewhere", "title": "Elsewhere", "doc_type": "page"}},
        {"op": "create", "type": "prose", "parent": "$d", "body": "Written elsewhere."},
        {"op": "create", "type": "figure", "parent": "$d", "fields": {"blob": blob, "media_type": "image/png", "alt": "A picture"}}
    ]))
    .unwrap();
    land(&mut o, "work").unwrap();
    o.read().export(&Scope::Canon, out).unwrap();
    o.close().unwrap();
}

/// A `qa_project`, written through `Local` (each step seals its batch, as the daemon's writer
/// does), that holds at least one object of every kind of `V13_KINDS` and one ref of every family
/// of `FAMILIES`: an imported blob, the open proposals `sa` and `dec` (`proposal/`), an `ANSWER`
/// of each family (a relayed tag and a local confirm under `answer/`, a relayed snooze under
/// `pending/`), a `.rdyignore` with the line `*.draft.yaml`, and a `.rdy/local.yaml` with
/// `actor: human:qa` (a dump leaves it out). Its last landing is a person's approval of one
/// content removal: a term version (its body and name the canaries, its alias quoted whole by a
/// kept review finding, tension advice and answer), the change that embeds it beside an
/// unaffected sibling op, a duplicate frame of the version, and a blob whose first chunk another
/// blob shares. The built project then holds three valid signatures (store format 14): the
/// person's approval of the seed landing, the agent's relay of the relayed landing and the
/// person's signature of the tag `v0.2.0`, made by the QA signer (`crate::signing`). The project
/// returned is a clone of the one built (O41): it keeps the built project's log byte for byte,
/// then holds its origin batch (`meta/origin`), which names the removal's landing.
/// Closed.
pub fn project() -> tempfile::TempDir {
    build(None)
}

/// `project`, built on a store whose segments hold `segment_max` bytes: its log rolls where a
/// batch and its marker do not fit, and a batch that grows past the limit stays in its segment
/// (the soft limit). The dump format 1 fixtures are written from it (dl8, di2, ec13).
pub fn rolling_project(segment_max: u64) -> tempfile::TempDir {
    build(Some(segment_max))
}

fn build(segment_max: Option<u64>) -> tempfile::TempDir {
    let (dir, mut r) = qa_project();
    if let Some(max) = segment_max {
        r.store_mut().set_segment_max(max);
    }
    let e = Local::new(r);
    let s = on(&e, seed);
    // a version tag on the seed landing (TAG, tag/)
    on(&e, |r| {
        r.tag(&approver(), "v0.1.0", None, None).unwrap();
    });
    // M2e2: an inbox subscription, acked at the end
    subscribe(&e, &ag("a"), None).unwrap();
    // view ops: create, a commit, fork, include, exclude, undo, close
    let sec = s.section.clone();
    on(&e, move |r| {
        let v = ag("v");
        r.view_create(&v, "feat", None, false).unwrap();
        write_as(r, &v, "feat", claim(&sec, "Feature one.")).unwrap();
        r.view_fork(&v, "feat", "feat-2").unwrap();
        let w = put(r, "w", claim(&sec, "From w."));
        r.view_include(&v, "feat-2", std::slice::from_ref(&w.change))
            .unwrap();
        r.view_exclude(&v, "feat-2", std::slice::from_ref(&w.change), false)
            .unwrap();
        r.undo(&v, "feat-2", None, false).unwrap();
        r.view_close(&v, &["feat".to_string()], "done").unwrap();
    });
    // an intent (INTENT, intent/)
    let k5 = s.k5.clone();
    on(&e, move |r| {
        put(r, "b", upd(&k5, "Five, as b sees it."));
        r.intent_post(&ag("b"), Some("b"), None, &[k5.to_string()], "edit", None)
            .unwrap();
    });
    // an alternative set (ALT_SET, alt/)
    let sec = s.section.clone();
    on(&e, move |r| {
        put(r, "x", claim(&sec, "Option x."));
        put(r, "y", claim(&sec, "Option y."));
        let m = |v: &str| AltMemberInput {
            label: v.into(),
            view: v.into(),
        };
        r.alt_create(&ag("x"), "Which option", None, &[m("x"), m("y")])
            .unwrap();
    });
    // an edit collision, dismissed by a reviewer (REVIEW, dismiss/)
    let k2 = s.k2.clone();
    on(&e, move |r| {
        put(r, "p", upd(&k2, "Two, by p."));
        put(r, "q", upd(&k2, "Two, by q."));
    });
    let k2 = s.k2.clone();
    on(&e, move |r| {
        let t = one_involving(r, TensionKind::EditCollision, &k2);
        let rev = Session::agent("agent:rev".into(), Some("rev".into()), Role::Reviewer).unwrap();
        r.dismiss_tension(&rev, &tension_label(&t.id), "both stand for now")
            .unwrap();
    });
    // a mention review (REVIEW, mention/)
    let (m, ent) = (s.mention.clone(), s.entity.clone());
    on(&e, move |r| {
        r.review_mention(
            &writer(),
            &Scope::Canon,
            &m.to_string(),
            &ent.to_string(),
            MentionVerdict::Dismiss,
            "an everyday word here",
        )
        .unwrap();
    });
    // M2e1: a pair verdict with blocking advice (REVIEW, pair/; TENSION, semantic/), which a
    // relayed acceptance accepts
    let (k3, k4) = (s.k3.clone(), s.k4.clone());
    let p = on(&e, move |r| {
        put(r, "sa", upd(&k4, "Four never holds with three."));
        advise_blocking(r, &k3, &k4);
        r.propose(&ag("sa"), "sa", "sa", None).unwrap()
    });
    on(&e, move |r| {
        let req = AcceptRequest {
            proposal: p.r#ref.clone(),
            advised_digest: p.advised_digest.clone().unwrap_or_default(),
            evidence: Some("yes, block on it".into()),
            question: Some("Accept the reviewer's advice to block?".into()),
            session_ref: Some("chat-1".into()),
            ..Default::default()
        };
        let a = r.accept_advice(&ag("relay"), "human:qa", req).unwrap();
        assert_eq!(a.accepted.len(), 1, "the advice is accepted");
    });
    // a rebase that records a conflict: rb edits k1, the person lands another edit of k1
    let k1 = s.k1.clone();
    on(&e, move |r| {
        put(r, "rb", upd(&k1, "One, by rb."));
    });
    let k1 = s.k1.clone();
    on(&e, move |r| {
        write(r, "work", upd(&k1, "One, by the person.")).unwrap();
        land(r, "work").unwrap();
    });
    // a relayed landing (relayed)
    let sec = s.section.clone();
    on(&e, move |r| {
        put(r, "rel", claim(&sec, "Relayed claim."));
        let p = r.propose(&ag("rel"), "rel", "rel", None).unwrap();
        let relay = Relay {
            by: ActorRun {
                actor: "agent:relay".into(),
                run: Some("chat".into()),
            },
            evidence: RelayEvidence {
                quoted: "yes, land it".into(),
                session_ref: "chat-2".into(),
                question: Some("Land it?".into()),
            },
        };
        r.land_with(
            &approver(),
            &p.r#ref,
            LandOptions {
                relay: Some(relay),
                ..Default::default()
            },
        )
        .unwrap();
    });
    // an imported change (imported_by)
    let out = tempfile::tempdir().unwrap();
    let out_dir = out.path().join("x");
    foreign_export(&out_dir);
    let imported = on(&e, move |r| {
        r.import(&approver(), "imported", &out_dir).unwrap();
        let st = r.read();
        st.included("imported")
            .iter()
            .any(|c| st.reader().change(c).unwrap().links.imported_by.is_some())
    });
    assert!(imported, "the import records its importer");
    // store format 13: agent cv's one change creates term `Kestrel` (its body and name are the
    // canaries a person removes at the end, its alias the text audit records quote) and an
    // unaffected sibling claim. The person snoozes its approve item through a relay whose note
    // and words quote the alias (an ANSWER quote candidate), then lands it; a reviewer's verdict
    // on the pair quotes the alias in its finding and its advice (REVIEW and TENSION quote
    // candidates, RULING F3 (a)). The v0.2.0 tag below names a landing that holds the term.
    let sec = s.section.clone();
    let (term, sibling, cv) = on(&e, move |r| {
        let w = put(
            r,
            "cv",
            json!([
                {"op": "create", "type": "term", "id": "$t", "parent": sec.to_string(), "fields": {"name": CANARY_FIELD, "aliases": [QUOTED_ALIAS]}, "body": CANARY_BODY},
                {"op": "create", "type": "claim", "id": "$c", "parent": sec.to_string(), "after": "$t", "body": SIBLING_BODY}
            ]),
        );
        let p = r
            .propose(&ag("cv"), "cv", "cv", Some("Defines the kestrel term."))
            .unwrap();
        (created(&w, "$t"), created(&w, "$c"), p)
    });
    on(&e, move |r| {
        let item = approve_item(r, &cv.r#ref);
        let later = r
            .answer(
                &relaying_agent(),
                PERSON,
                AnswerRequest {
                    r#ref: item.r#ref.clone(),
                    option: "later".into(),
                    card_hash: Some(item.card_hash.clone()),
                    note: Some(format!("Ask again after {QUOTED_ALIAS} is sourced.")),
                    relay: relay_words(&format!("Not yet: {QUOTED_ALIAS}."), "chat-4"),
                    brief: None,
                },
            )
            .unwrap();
        assert_eq!(later.act, "later");
        r.land(&approver(), &cv.id).unwrap();
    });
    let (t, c) = (term.clone(), sibling.clone());
    on(&e, move |r| {
        verdict(
            r,
            &t,
            &c,
            Hash([10; 32]),
            &format!("The claim restates {QUOTED_ALIAS}."),
            &format!("Source {QUOTED_ALIAS} first."),
        );
    });
    // store format 11: a relayed tag on canon's head (TAG, and an ANSWER under answer/ that
    // holds the relay, D-DGCH7)
    on(&e, |r| {
        let info = r
            .tag_as(
                &relaying_agent(),
                PERSON,
                &relay_words("Tag it v0.2.0.", "chat-3"),
                true,
                "v0.2.0",
                None,
                None,
            )
            .unwrap();
        assert!(info.answer.is_some(), "the relayed tag writes an ANSWER");
    });
    // a batch that holds an exception: a `one_way` decision, proposed with a summary; it does
    // not land. The person snoozes its approve item through a relay (ANSWER, pending/) and
    // confirms the exception as themselves (ANSWER, answer/).
    let (sec, ent) = (s.section.clone(), s.entity.clone());
    on(&e, move |r| {
        decide_in(
            r,
            "dec",
            DecisionInput {
                parent: sec.to_string(),
                statement: "The ledger is copied once a night".into(),
                title: Some("Copy the ledger nightly".into()),
                why: Some("the person decides how often the ledger is copied".into()),
                reversibility: Some("one_way".into()),
                affects: vec![ent.to_string()],
                ..Default::default()
            },
        );
        let p = r
            .propose_subset(
                &ag("dec"),
                "dec",
                "dec",
                None,
                Some("Records how often the ledger is copied."),
            )
            .unwrap();
        let q = r
            .read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .unwrap();
        let batch = q
            .items
            .iter()
            .find(|i| i.kind == ItemKind::Approve && i.r#ref == p.r#ref)
            .unwrap_or_else(|| panic!("the approve item {}: {q:#?}", p.r#ref))
            .clone();
        let later = r
            .answer(
                &relaying_agent(),
                PERSON,
                AnswerRequest {
                    r#ref: batch.r#ref.clone(),
                    option: "later".into(),
                    card_hash: Some(batch.card_hash.clone()),
                    note: None,
                    relay: relay_words("Not now.", "chat-3"),
                    brief: None,
                },
            )
            .unwrap();
        assert_eq!(later.act, "later");
        let confirmed = confirm_exceptions(r, &approver(), &p.r#ref);
        assert_eq!(confirmed.len(), 1, "one exception: the one_way decision");
    });
    // the inbox ack moves the persisted cursor
    let before = e.read().reader().inbox("agent:a", Some("a")).unwrap();
    let req: InboxRequest = serde_json::from_value(json!({"ack": true})).unwrap();
    run_inbox(&e, &ag("a"), req).unwrap();
    let after = e.read().reader().inbox("agent:a", Some("a")).unwrap();
    assert!(
        after.as_ref().unwrap().cursor > before.unwrap().cursor,
        "the ack moved the cursor"
    );
    // store format 17: a plan chain with its seal, an unsealed plan, every plan act and the four
    // plan answer acts (`add_plan_fixture_records`)
    let sec = s.section.clone();
    let cause = s.k3.clone();
    on(&e, move |r| {
        add_plan_fixture_records(r, &sec, &cause);
    });
    // store format 13: two blobs that share their first chunk (the request selects one), and
    // the bytes of the term's version
    let t = term.clone();
    let (blob, version, bytes) = on(&e, move |r| {
        let shared = vec![0x5a_u8; 1 << 20];
        let mut one = shared.clone();
        one.extend_from_slice(b"the removed blob's own tail");
        let mut two = shared;
        two.extend_from_slice(b"the other blob's own tail");
        let blob = r.store_mut().put_blob(&one).unwrap();
        r.store_mut().put_blob(&two).unwrap();
        let st = r.read();
        let version = st
            .materialized(&Scope::Canon)
            .unwrap()
            .version_of(&t)
            .unwrap();
        let bytes = st.reader().get_kind(kinds::BLOCK, &version).unwrap();
        (blob, version, bytes)
    });
    e.close().unwrap();
    // a duplicate frame of the term's version, appended raw while the store is closed: no
    // writer logs an object twice, but a log may hold two copies, and a removal drops both
    {
        let (mut log, torn) = SegmentLog::open_marked(
            &dir.path().join(".rdy/store/segments"),
            segment_max.unwrap_or(64 * 1024 * 1024),
            None,
        )
        .unwrap();
        assert!(torn.is_empty(), "the source log ends at a batch marker");
        log.append_batch(&[(kinds::BLOCK, bytes)]).unwrap();
        log.sync().unwrap();
    }
    let mut r = Rdy::open(dir.path()).unwrap();
    if let Some(max) = segment_max {
        r.store_mut().set_segment_max(max);
    }
    let e = Local::new(r);
    // the request: agent rq records the removal of the term's version and of the blob; the
    // person approves it as themselves, and the landing erases the payloads (D-CC90E)
    let removed = on(&e, move |r| {
        let rq = ag("rq");
        let res = r
            .redact_in(
                &rq,
                "rq",
                RedactInput {
                    targets: vec![format!("{term}@{version}"), format!("blob:{blob}")],
                    reason_class: "personal".into(),
                    reason: "the term names a person".into(),
                    dry_run: false,
                },
            )
            .unwrap();
        let p = r
            .propose(&rq, "rq", "rq", Some("Removes the kestrel term."))
            .unwrap();
        r.land(&approver(), &p.id).unwrap();
        res.redaction.expect("the request is recorded")
    });
    e.close().unwrap();
    // store format 14: three signatures of the final records, before the clone, so the clone's
    // log still ends with its origin batch
    sign_acts(dir.path());
    // O41: the project returned is a clone of the one built. A clone carries `.rdyignore`, so it
    // is written first; a clone never carries `.rdy/local.yaml`, so it is written into the clone.
    std::fs::write(dir.path().join(".rdyignore"), "*.draft.yaml\n").unwrap();
    let src = Rdy::open(dir.path()).unwrap();
    assert_eq!(removal(&src).request, removed, "the request is applied");
    let to = tempfile::TempDir::new().unwrap();
    {
        let st = src.read();
        clone(CloneSource::State(&st), to.path(), false).unwrap();
    }
    src.close().unwrap();
    std::fs::write(to.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    to
}

/// What [`add_plan_fixture_records`] wrote: the view of its chain, the chain's bound, answer-only
/// and sealed versions, the landing the seal names, and the unsealed version of another view.
#[derive(Clone, Debug)]
pub struct PlanFixture {
    pub view: String,
    pub bound: Hash,
    pub answered: Hash,
    pub seal: Hash,
    pub landing: Hash,
    pub unsealed: Hash,
    /// The four plan answer acts' answers: keep, accept, refuse, reject.
    pub answers: [Hash; 4],
}

/// The view of the every-kind plan chain.
pub const PLAN_VIEW: &str = "plan-a";

/// The PLAN frame of `p` and its `plan/<view>` ref, as the plan writers compose them.
pub fn plan_frames(p: &Plan) -> Vec<(u8, Vec<u8>)> {
    let refs = vec![(rdy_store::plan::plan_ref(&p.view), p.id().0.to_vec())];
    let mut out = vec![(kinds::PLAN, postcard::to_allocvec(p).unwrap())];
    out.extend(refs.into_iter().map(|(name, value)| {
        (
            kinds::REF_UPDATE,
            postcard::to_allocvec(&rdy_store::kinds::RefUpdate {
                name,
                value,
                at: Location {
                    segment: 0,
                    offset: 0,
                },
            })
            .unwrap(),
        )
    }));
    out
}

/// The fixture-only typed builder of store format 17's records (D-4JMPH): it composes them as
/// the plan writers do, through the store's own writers and the doc-hidden
/// `write_guarded_for_tests` hook, each in one held batch that `invalidate` seals.
///
/// - View [`PLAN_VIEW`] gets a claim under `parent`, then a version of it whose fields hold both
///   engine metadata shapes (`reaffirmed` against `cause`, and `reflects_at`).
/// - Its proposal and the chain's bound version, in one batch.
/// - An answer-only version through `Store::put_plan`, with each plan act: confirm, defer and
///   done (a code item).
/// - The four plan answer acts: KeepSuspect of the claim for `cause`, AcceptDeferrals,
///   RefuseDeferrals and RejectReaffirm.
/// - The landing and its seal, in one batch: the confirmed, deferred (with the acceptance) and
///   done items, and `cause` among its decisions.
/// - An unsealed answer-only version of view `sa`, which stays open (it continues the chain
///   the view's propose bound).
///
/// Production callers compose plans through the typed proposal and landing paths, never this.
pub fn add_plan_fixture_records(r: &mut Rdy, parent: &BlockId, cause: &BlockId) -> PlanFixture {
    use rdy_model::engine_fields::{
        Against, REAFFIRMED, REFLECTS_AT, Reaffirmation, ReflectsAt, encode_reaffirmed,
        encode_reflects_at,
    };
    let view = PLAN_VIEW;
    let w = put(
        r,
        view,
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": parent.to_string(), "body": "The planned claim."}]),
    );
    let subject = created(&w, "$c");
    let agent = Provenance {
        actor: "agent:plan".into(),
        run: Some("plan".into()),
        role: Role::Writer,
        at_ms: 2,
    };
    let person = Provenance {
        actor: PERSON.into(),
        run: None,
        role: Role::Approver,
        at_ms: 3,
    };
    // both metadata shapes on the claim's next version
    let mut next = r.read().block(&Scope::View(view.into()), &subject).unwrap();
    let base = next.version_hash();
    next.fields.insert(
        REAFFIRMED.into(),
        encode_reaffirmed(&BTreeMap::from([(
            cause.clone(),
            Reaffirmation {
                against: Against::Retiring(vec![]),
                reason: "the claim still follows it".into(),
            },
        )])),
    );
    next.fields.insert(
        REFLECTS_AT.into(),
        encode_reflects_at(&ReflectsAt {
            files: Hash([0x5c; 32]),
            commits: BTreeMap::from([
                ("app".to_string(), Some("b".repeat(40))),
                ("web".to_string(), None),
            ]),
        }),
    );
    let version = next.version_hash();
    let deps = r.read().included(view).to_vec();
    r.store_mut()
        .commit_to_view(
            view,
            rdy_model::Change {
                view: view.into(),
                ops: vec![rdy_model::Op::Update { base, block: next }],
                deps,
                provenance: agent.clone(),
                justification: None,
                message: "records the re-affirmation".into(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
    // the proposal and its bound version, one batch
    let heads = vec![
        PlanHead {
            repo: "app".into(),
            head: Some("b".repeat(40)),
        },
        PlanHead {
            repo: "web".into(),
            head: None,
        },
    ];
    let proposal = r
        .store_mut()
        .propose(view, "Plans the planned claim", agent.clone())
        .unwrap();
    let bound = Plan {
        stable: None,
        prev: None,
        view: view.into(),
        proposal: Some(proposal),
        answers: vec![],
        heads: heads.clone(),
        provenance: agent.clone(),
        seal: None,
    };
    r.store_mut()
        .write_guarded_for_tests(plan_frames(&bound))
        .unwrap();
    r.invalidate();
    // an answer-only version with each plan act
    let code = "app:src/plan.rs";
    let items = [
        (
            item_hash(cause, subject.as_str(), PlanKind::Confirm),
            PlanAct::Confirm,
        ),
        (
            item_hash(cause, subject.as_str(), PlanKind::Reaffirm),
            PlanAct::Defer,
        ),
        (
            item_hash(cause, code, PlanKind::UpdateImplementation),
            PlanAct::Done,
        ),
    ];
    let mut answers: Vec<PlanAnswer> = items
        .iter()
        .map(|(item, act)| PlanAnswer {
            item: *item,
            act: *act,
            reason: (*act != PlanAct::Done).then(|| format!("{} for now", act.as_str())),
            subject: (*act != PlanAct::Done).then_some(Hash([0x51; 32])),
            cause: Hash([0x52; 32]),
            by: agent.clone(),
        })
        .collect();
    answers.sort_by_key(|a| a.item);
    let answered = Plan {
        stable: Some(bound.id()),
        prev: Some(bound.id()),
        proposal: None,
        answers: answers.clone(),
        ..bound.clone()
    };
    let answered = r.store_mut().put_plan(&answered).unwrap();
    // the four plan answer acts
    let short = |h: &Hash| h.to_string()[..12].to_string();
    let answer = |item: String, act: AnswerAct, targets: Vec<Hash>, note: Option<&str>| Answer {
        item,
        act,
        option: None,
        label: None,
        options: vec![],
        card_hash: None,
        targets,
        person: person.clone(),
        relayed_by: None,
        relay_evidence: None,
        evidence_kind: None,
        note: note.map(str::to_string),
    };
    let p_ref = format!("P-{}", &proposal.to_string()[..8]);
    let keep = answer(
        format!("reaffirm:{cause}@{}", short(&version)),
        AnswerAct::KeepSuspect,
        vec![version],
        Some("the planned claim still holds"),
    );
    let accept = answer(
        format!("defer:{p_ref}@1"),
        AnswerAct::AcceptDeferrals,
        vec![],
        None,
    );
    let refuse = answer(
        format!("defer:{p_ref}@2"),
        AnswerAct::RefuseDeferrals,
        vec![],
        None,
    );
    let reject = answer(
        format!("{subject}@{}", short(&version)),
        AnswerAct::RejectReaffirm,
        vec![],
        None,
    );
    let mut acts = [Hash([0; 32]); 4];
    for (i, a) in [keep, accept, refuse, reject].iter().enumerate() {
        acts[i] = r.store_mut().put_answer(a).unwrap();
    }
    r.invalidate();
    // the landing and its seal, one batch
    let landing = r.store_mut().land(&proposal, person.clone()).unwrap();
    let landed = r.store().landing(&landing).unwrap().proposal;
    let mut sealed_items: Vec<SealedItem> = items
        .iter()
        .map(|(item, act)| {
            let (kind, subject_text, outcome, answer) = match act {
                PlanAct::Confirm => (
                    PlanKind::Confirm,
                    subject.to_string(),
                    PlanOutcome::Confirmed,
                    None,
                ),
                PlanAct::Defer => (
                    PlanKind::Reaffirm,
                    subject.to_string(),
                    PlanOutcome::Deferred,
                    Some(acts[1]),
                ),
                PlanAct::Done => (
                    PlanKind::UpdateImplementation,
                    code.to_string(),
                    PlanOutcome::Done,
                    None,
                ),
            };
            SealedItem {
                id: *item,
                kind,
                subject: subject_text,
                cause: cause.clone(),
                edge: if kind == PlanKind::UpdateImplementation {
                    "implements".into()
                } else {
                    "reflects".into()
                },
                outcome,
                answer,
            }
        })
        .collect();
    sealed_items.sort_by_key(|i| i.id);
    let seal = Plan {
        stable: Some(bound.id()),
        prev: Some(answered),
        proposal: Some(landed),
        answers,
        provenance: person.clone(),
        seal: Some(PlanSeal {
            landing,
            version: landed,
            decisions: vec![cause.clone()],
            items: sealed_items,
        }),
        ..bound.clone()
    };
    r.store_mut()
        .write_guarded_for_tests(plan_frames(&seal))
        .unwrap();
    r.invalidate();
    // an unsealed version of another view, which stays open: it continues the view's live
    // chain, whose first version the view's propose bound (D-4JMPH)
    let sa_head = r
        .read()
        .reader()
        .get_ref(&rdy_store::plan::plan_ref("sa"))
        .unwrap()
        .map(|v| Hash(v.as_slice().try_into().unwrap()));
    let (stable, prev, answers) = match sa_head {
        Some(h) => {
            let p = r.read().reader().plan(&h).unwrap();
            (Some(p.stable.unwrap_or(h)), Some(h), p.answers)
        }
        None => (None, None, vec![]),
    };
    let unsealed = r
        .store_mut()
        .put_plan(&Plan {
            stable,
            prev,
            view: "sa".into(),
            proposal: None,
            answers,
            heads,
            provenance: agent,
            seal: None,
        })
        .unwrap();
    r.invalidate();
    PlanFixture {
        view: view.into(),
        bound: bound.id(),
        answered,
        seal: seal.id(),
        landing,
        unsealed,
        answers: acts,
    }
}

/// Appends three valid signatures to the project at `dir`, each in its own batch, with the
/// project's repo id, which its clone keeps (D-1Z951): the person's key signs the approval of the seed landing
/// and the tag `v0.2.0`; the agent's key signs the relay of the relayed landing. The records are
/// the final ones the build made. The open batch is sealed, then fsck and the pure check of each
/// signature pass before the project is returned. No signing act runs: the QA signer is a test
/// harness (`crate::signing`).
fn sign_acts(dir: &Path) {
    let mut r = Rdy::open(dir).unwrap();
    let repo = r.read().repo_id().unwrap();
    let person = Provenance {
        actor: PERSON.into(),
        run: None,
        role: Role::Approver,
        at_ms: 1,
    };
    let agent = Provenance {
        actor: "agent:relay".into(),
        run: Some("chat".into()),
        role: Role::Writer,
        at_ms: 2,
    };
    let (landings, tags) = {
        let st = r.read();
        (st.landings().unwrap(), st.reader().tags().unwrap())
    };
    // `landings` runs newest first: the seed landing is the oldest local approval
    let (seed_hash, seed) = landings
        .iter()
        .rev()
        .find(|(_, l)| l.assurance == Assurance::Local)
        .expect("the seed landing");
    let relayed: Vec<_> = landings
        .iter()
        .filter(|(_, l)| l.assurance == Assurance::Relayed && l.relayed_by.is_some())
        .collect();
    assert_eq!(relayed.len(), 1, "one relayed landing");
    let (relayed_hash, relayed) = relayed[0];
    let (_, tag_hash, tag) = tags
        .iter()
        .find(|(name, ..)| name == "v0.2.0")
        .expect("the tag v0.2.0");
    let signatures = [
        sign_record(
            *seed_hash,
            StatementSource::Approval(seed),
            repo,
            person.clone(),
            TestKey::Person,
        ),
        sign_record(
            *relayed_hash,
            StatementSource::RelayApproval(relayed),
            repo,
            agent,
            TestKey::Agent,
        ),
        sign_record(
            *tag_hash,
            StatementSource::Tag {
                hash: *tag_hash,
                tag,
            },
            repo,
            person,
            TestKey::Person,
        ),
    ];
    for g in &signatures {
        r.store_mut().put_signature(g).unwrap();
    }
    r.seal_open().unwrap();
    r.fsck().unwrap();
    let st = r.read();
    for g in &signatures {
        let checks = signature_checks(&st, &g.subject, None).unwrap();
        assert_eq!(checks.len(), 1, "one signature per subject");
        let c = &checks[0];
        assert_eq!(
            (c.signed.as_str(), c.statement.as_str(), c.code.as_deref()),
            ("valid", "match", None),
            "{c:?}"
        );
    }
    drop(st);
    r.close().unwrap();
}

/// The agent that relays the person's words from chat.
fn relaying_agent() -> Session {
    Session::agent("agent:relay".into(), Some("chat".into()), Role::Writer).unwrap()
}

/// The person's words `quote`, relayed over MCP from chat session `session_ref`.
fn relay_words(quote: &str, session_ref: &str) -> RelayInput {
    RelayInput {
        evidence: Some(quote.into()),
        question: Some("Shall I record your answer?".into()),
        session_ref: Some(session_ref.into()),
        ..Default::default()
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What a later rdy must read the same from this project, after a dump and a load:
/// `{"refs": {<name>: <hex value>}, "objects": {<kind>: <count>}, "landings": [<hash>], "fsck": <FsckReport>, "signatures": {<hash>: {..}}}`.
/// `refs` is every ref. `objects` counts the data frames by their original kind from the log start
/// to `seq()`, through the record walker: a frame whose payload a removal dropped counts as its
/// kind (batch markers left out). `landings` is canon's landing chain, oldest first.
pub fn facts(r: &Rdy) -> serde_json::Value {
    let st = r.read();
    let refs: BTreeMap<String, String> = st
        .reader()
        .refs_with_prefix("")
        .unwrap()
        .into_iter()
        .map(|(name, value)| (name, hex(&value)))
        .collect();
    let mut objects: BTreeMap<String, u64> = BTreeMap::new();
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    st.reader()
        .for_each_record_from(origin, st.seq(), |_, f, _| {
            let kind = match f {
                StoredFrame::Present { kind, .. } | StoredFrame::Dropped { kind, .. } => kind,
            };
            if kind != kinds::BATCH_COMMIT {
                *objects.entry(kind.to_string()).or_default() += 1;
            }
            Ok(())
        })
        .unwrap();
    let mut landings: Vec<String> = st
        .landings()
        .unwrap()
        .into_iter()
        .map(|(h, _)| h.to_string())
        .collect();
    landings.reverse();
    json!({
        "refs": refs,
        "objects": objects,
        "landings": landings,
        "fsck": r.fsck().unwrap(),
        "signatures": signature_facts(&st),
    })
}

/// The stored signatures, keyed by full signature hash: `{subject, namespace, statement_hash,
/// sig_hash, check}`. Both byte hashes are BLAKE3 of the statement and of the SSHSIG blob. The
/// check is the offline check with no trust data (`trust` is `unknown`), as `rdy fsck` and
/// `rdy verify` run it. Empty for a store that holds none.
fn signature_facts(st: &rdy_engine::ReadState) -> serde_json::Value {
    let mut stored: Vec<(Hash, Signature)> = vec![];
    st.reader()
        .visit_signatures(&mut |h, g| {
            stored.push((h, g.clone()));
            Ok(())
        })
        .unwrap();
    let mut out = serde_json::Map::new();
    for (hash, g) in stored {
        let context = read_check_context(st, &g).unwrap();
        out.insert(
            hash.to_string(),
            json!({
                "subject": g.subject.to_string(),
                "namespace": g.namespace,
                "statement_hash": Hash::of(&g.statement).to_string(),
                "sig_hash": Hash::of(&g.sig).to_string(),
                "check": check_signature(&g, &context, None),
            }),
        );
    }
    serde_json::Value::Object(out)
}

/// The one applied content removal of a project: the request's hash and record, and its apply's
/// hash and record.
pub struct Removal {
    pub request: Hash,
    pub record: Redaction,
    pub apply: Hash,
    pub applied: RedactionApply,
}

/// The project's applied removal: its one REDACTION_APPLY, read through the record walker, and
/// the request it activates. Panics unless the log holds exactly one apply.
pub fn removal(r: &Rdy) -> Removal {
    let st = r.read();
    let reader = st.reader();
    let mut applies = vec![];
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    reader
        .for_each_record_from(origin, st.seq(), |_, f, _| {
            if let StoredFrame::Present {
                kind: kinds::REDACTION_APPLY,
                hash,
                ..
            } = f
            {
                applies.push(hash);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(applies.len(), 1, "one applied removal: {applies:?}");
    let apply = applies[0];
    let applied: RedactionApply = reader.get_typed(kinds::REDACTION_APPLY, &apply).unwrap();
    let record = reader.redaction_request(&applied.request).unwrap();
    Removal {
        request: applied.request,
        record,
        apply,
        applied,
    }
}

/// What a later rdy must read the same of a store's identity and log after a dump and a load,
/// from the log start to `upto` (the store's sealed end when None): `{"repo_id", "origin",
/// "mode", "canon", "tags": {<name>: {"tag", "landing"}}, "keys": [<kind>:<hash>], "redacted":
/// {<kind>: <count>}}`. `keys` is every distinct data frame's kind and hash, a dropped one too
/// (batch markers left out), sorted; `redacted` counts the dropped frames by their original
/// kind. No clock, path or time is in it.
pub fn identity_facts(r: &Rdy, upto: Option<Location>) -> serde_json::Value {
    let st = r.read();
    let reader = st.reader();
    let mut keys = BTreeSet::new();
    let mut redacted: BTreeMap<String, u64> = BTreeMap::new();
    let origin = Location {
        segment: 0,
        offset: 0,
    };
    reader
        .for_each_record_from(origin, upto.unwrap_or(st.seq()), |_, f, _| {
            match f {
                StoredFrame::Present { kind, hash, .. } => {
                    if kind != kinds::BATCH_COMMIT {
                        keys.insert(format!("{kind}:{hash}"));
                    }
                }
                StoredFrame::Dropped { kind, hash, .. } => {
                    keys.insert(format!("{kind}:{hash}"));
                    *redacted.entry(kind.to_string()).or_default() += 1;
                }
            }
            Ok(())
        })
        .unwrap();
    let tags: BTreeMap<String, serde_json::Value> = reader
        .tags()
        .unwrap()
        .into_iter()
        .map(|(name, hash, t)| {
            (
                name,
                json!({"tag": hash.to_string(), "landing": t.landing.to_string()}),
            )
        })
        .collect();
    let origin = st.origin().unwrap().map(|o| {
        json!({
            "fork": o.fork,
            "repo_id": o.repo_id.to_string(),
            "tag": o.tag,
            "landing": o.landing.map(|h| h.to_string()),
        })
    });
    let mode = match reader.mode().unwrap() {
        StoreMode::Project => "project",
        StoreMode::Holder => "holder",
    };
    json!({
        "repo_id": st.repo_id().unwrap().to_string(),
        "origin": origin,
        "mode": mode,
        "canon": st.canon_head().map(|h| h.to_string()),
        "tags": tags,
        "keys": keys,
        "redacted": redacted,
    })
}

/// Each tag's blocks as the store reads them at the tag's landing: `{<tag name>: {<block id>:
/// {"version", "redacted"}}}`. A removed version reads as its stub (`redacted` true) under its
/// original version hash.
pub fn projections(r: &Rdy) -> serde_json::Value {
    let st = r.read();
    let reader = st.reader();
    let mut out = serde_json::Map::new();
    for (name, _, t) in reader.tags().unwrap() {
        let m = st.materialized(&Scope::At(t.landing)).unwrap();
        let mut blocks = serde_json::Map::new();
        for (id, v) in m.versions() {
            let b = reader.get_block(v).unwrap();
            blocks.insert(
                id.to_string(),
                json!({"version": v.to_string(), "redacted": b.is_redacted()}),
            );
        }
        out.insert(name, serde_json::Value::Object(blocks));
    }
    serde_json::Value::Object(out)
}

// ---------------------------------------------------------------------------------------------
// the every-kind holder

/// The body of the prose a removal selects in the holder's sources: a canary held only by the
/// removed payloads.
pub const HOLDER_CANARY: &str = "Merlin holder canary.";
/// The bytes of the figure's blob, which the removal selects too: its one chunk is dropped.
pub const HOLDER_BLOB: &[u8] = b"\x89PNG every-kind holder figure";
/// The body source B gives the removed prose: another version of the same block, which no
/// approval removes. It stays readable at B's tag.
pub const HOLDER_KEPT_BODY: &str = "B keeps its own words.";
/// The holder's tags, in the order of their takes: A's first tag (the person signs it), B's tag
/// (B's history leaves A's after the shared first landing), and A's relayed tag after A's
/// approved removal (the agent signs the relay). The holder's sync batch is the history of the
/// last one.
pub const HOLDER_TAGS: [&str; 3] = ["v1.0.0", "v2.0.0", "v3.0.0"];

/// A built holder: a directory that holds the two source projects (`a`, `b`), the upload files
/// and the holder at `root`, closed.
pub struct HolderBuild {
    pub dir: tempfile::TempDir,
    pub root: PathBuf,
}

/// The every-kind holder (store format 15, D-BPYN8), on a store whose segments hold
/// `segment_max` bytes. Its repo id is its sources' (D-DAGNB), which is fixed on a thread with
/// seeded ids. It is filled only through the engine's export, batch check and all-files take
/// (D-D2NPN), from two projects of one repo:
///
/// - A lands doc `guide` (a prose block whose body is [`HOLDER_CANARY`], a kept prose, and a
///   figure whose blob is [`HOLDER_BLOB`]) and tags it `v1.0.0`; the person signs that landing's
///   approval and the tag. B is a copy of A at that point.
/// - B gives the prose another version ([`HOLDER_KEPT_BODY`]), lands it (the person signs the
///   approval) and tags it `v2.0.0`.
/// - A edits the prose away and back (the change back embeds the first version again), then
///   agent rq asks to remove every version of the prose and the figure's blob, and the person
///   approves it (D-CC90E; the person signs the approval). The removal is later than B's copy, and
///   both histories hold what it selects. An agent relays the person's tag `v3.0.0` (an ANSWER),
///   and the agent signs the relay.
///
/// The takes: `v1.0.0` as one batch, exported before the removal existed; `v2.0.0` split at a
/// record boundary into two batches, the first without the root, checked one by one and taken in
/// reverse order; `v3.0.0` as one batch, which erases every copy of the approved tuples the
/// holder held (the first prose version, the changes that embed it and the blob's chunk); then
/// the uploads of `v2.0.0` and `v1.0.0` again, which append nothing. B's
/// version of the prose is no approved tuple and stays. The holder then holds the three tags, the
/// relay answer's `answer/tag-object:<hash>` ref and no canon, passes fsck, and each of its five
/// signatures checks as valid and matching.
pub fn holder(segment_max: u64) -> HolderBuild {
    let dir = tempfile::tempdir().unwrap();
    let (a_root, b_root) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a_root).unwrap();
    let mut a = Rdy::init(&a_root, "rdy-standard").unwrap();
    let blob = a.store_mut().put_blob(HOLDER_BLOB).unwrap();
    let w = write(&mut a, "work", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
        {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": HOLDER_CANARY},
        {"op": "create", "type": "prose", "id": "$q", "parent": "$d", "after": "$p", "body": "This prose stays."},
        {"op": "create", "type": "figure", "id": "$f", "parent": "$d", "after": "$q", "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "A figure"}}
    ]))
    .unwrap();
    land(&mut a, "work").unwrap();
    let prose = created(&w, "$p");
    let first_version = a
        .read()
        .block(&Scope::Canon, &prose)
        .unwrap()
        .version_hash();
    let first = a.read().canon_head().unwrap();
    sign_approval(&mut a, first);
    a.tag(&approver(), HOLDER_TAGS[0], None, None).unwrap();
    let v1 = tag_named(&a, HOLDER_TAGS[0]);
    sign_tag(&mut a, v1);
    // exported now, before the removal exists: the batch carries the prose's bytes
    let first_upload = export_tag(&a, v1);
    assert!(
        first_upload
            .windows(HOLDER_CANARY.len())
            .any(|w| w == HOLDER_CANARY.as_bytes())
    );
    a.close().unwrap();
    copy_tree(&a_root, &b_root);
    let mut a = Rdy::open(&a_root).unwrap();
    let mut b = Rdy::open(&b_root).unwrap();
    // B: another version of the prose's block
    write(&mut b, "work", upd(&prose, HOLDER_KEPT_BODY)).unwrap();
    land(&mut b, "work").unwrap();
    let lb = b.read().canon_head().unwrap();
    sign_approval(&mut b, lb);
    b.tag(&approver(), HOLDER_TAGS[1], None, None).unwrap();
    let v2 = tag_named(&b, HOLDER_TAGS[1]);
    // A: the prose away and back, then the approved removal and the relayed tag
    write(&mut a, "work", upd(&prose, "Nothing to see here.")).unwrap();
    land(&mut a, "work").unwrap();
    write(&mut a, "work", upd(&prose, HOLDER_CANARY)).unwrap();
    land(&mut a, "work").unwrap();
    assert_eq!(
        a.read()
            .block(&Scope::Canon, &prose)
            .unwrap()
            .version_hash(),
        first_version,
        "the change back embeds the first version"
    );
    let rq = ag("rq");
    a.redact_in(
        &rq,
        "rq",
        RedactInput {
            targets: vec![prose.to_string(), format!("blob:{blob}")],
            reason_class: "personal".into(),
            reason: "the prose names a person".into(),
            dry_run: false,
        },
    )
    .unwrap()
    .redaction
    .expect("the request is recorded");
    let p = a
        .propose(
            &rq,
            "rq",
            "rq",
            Some("Removes the prose that names a person."),
        )
        .unwrap();
    a.land(&approver(), &p.id).unwrap();
    let removal = a.read().canon_head().unwrap();
    sign_approval(&mut a, removal);
    let info = a
        .tag_as(
            &relaying_agent(),
            PERSON,
            &relay_words("Tag it v3.0.0.", "chat-5"),
            true,
            HOLDER_TAGS[2],
            None,
            None,
        )
        .unwrap();
    assert!(info.answer.is_some(), "the relayed tag writes an ANSWER");
    let v3 = tag_named(&a, HOLDER_TAGS[2]);
    sign_relay_tag(&mut a, v3);

    let repo = a.read().repo_id().unwrap();
    let root = dir.path().join("holder");
    let uploads = dir.path().join("uploads");
    std::fs::create_dir_all(&uploads).unwrap();
    let mut h = Rdy::create_holder(&root, repo).unwrap();
    h.store_mut().set_segment_max(segment_max);
    let one = upload(&uploads, "v1", repo, &[first_upload]);
    let got = take(&mut h, &one, target_of(&a, v1)).unwrap();
    assert!(!got.unchanged);
    assert!(
        holds(&root, HOLDER_CANARY.as_bytes()),
        "the holder holds the prose"
    );
    let parts = split_batch(&export_tag(&b, v2), 2);
    assert_eq!(parts.len(), 2);
    let root_key = ObjectKey {
        kind: kinds::TAG,
        hash: v2,
    };
    assert!(
        !batch_keys(&parts[0]).contains(&root_key),
        "the first batch holds no root"
    );
    let two = upload(&uploads, "v2", repo, &[parts[1].clone(), parts[0].clone()]);
    let got = take(&mut h, &two, target_of(&b, v2)).unwrap();
    assert!(!got.unchanged);
    let three = upload(&uploads, "v3", repo, &[export_tag(&a, v3)]);
    let got = take(&mut h, &three, target_of(&a, v3)).unwrap();
    assert!(got.redacted_objects >= 1, "{got:?}");
    // the earlier uploads again: every key is held, and a present copy of a removed object is
    // never restored
    for (files, t) in [(&two, target_of(&b, v2)), (&one, target_of(&a, v1))] {
        let again = take(&mut h, files, t).unwrap();
        assert!(
            again.unchanged,
            "a repeated take appends nothing: {again:?}"
        );
    }
    // the holder: no removed byte, B's version of the prose, every signature as signed
    assert!(!holds(&root, HOLDER_CANARY.as_bytes()), "the canary left");
    assert!(!holds(&root, HOLDER_BLOB), "the blob's chunk left");
    {
        let st = h.read();
        assert_eq!(st.canon_head(), None);
        let kept = st
            .block(&Scope::At(b.read().canon_head().unwrap()), &prose)
            .unwrap();
        assert_eq!(kept.body, HOLDER_KEPT_BODY);
        assert!(!kept.is_redacted());
    }
    assert!(
        !h.store()
            .refs_with_prefix(rdy_store::holder::ANSWER_TAG_OBJECT_REF)
            .unwrap()
            .is_empty(),
        "the relay answer's ref"
    );
    h.fsck().unwrap();
    let mut subjects = vec![];
    h.store()
        .reader()
        .visit_signatures(&mut |_, g| {
            subjects.push(g.subject);
            Ok(())
        })
        .unwrap();
    assert_eq!(subjects.len(), 5, "three approvals, a tag and a relay");
    {
        let st = h.read();
        for s in &subjects {
            for c in signature_checks(&st, s, None).unwrap() {
                assert_eq!(
                    (c.signed.as_str(), c.statement.as_str(), c.code.as_deref()),
                    ("valid", "match", None),
                    "{c:?}"
                );
            }
        }
    }
    h.close().unwrap();
    a.close().unwrap();
    b.close().unwrap();
    HolderBuild { dir, root }
}

/// The hash of tag `name` in `r`.
fn tag_named(r: &Rdy, name: &str) -> Hash {
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == name)
        .unwrap_or_else(|| panic!("the tag {name}"))
        .1
}

/// The upload target of tag `t` in `r`: its name and landing.
fn target_of(r: &Rdy, t: Hash) -> UploadTarget {
    let tag = r.store().read_tag(&t).unwrap();
    UploadTarget {
        tag: tag.name,
        landing: tag.landing,
    }
}

/// The history of tag `t` in `r`, as the export writes it: one full batch.
pub fn export_tag(r: &Rdy, t: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), t, &BTreeSet::new(), &mut out).unwrap();
    out
}

/// `batches` written as files `<name>-<i>.rdysync` under `dir`, each checked alone for `repo`
/// first (the batch check), and opened for reading.
fn upload(dir: &Path, name: &str, repo: RepoId, batches: &[Vec<u8>]) -> Vec<File> {
    batches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            check_batch(b.as_slice(), Some(repo)).unwrap();
            let p = dir.join(format!("{name}-{i}.rdysync"));
            std::fs::write(&p, b).unwrap();
            File::open(&p).unwrap()
        })
        .collect()
}

/// The logical keys of the records of `batch`, in wire order.
pub fn batch_keys(batch: &[u8]) -> Vec<ObjectKey> {
    let (mut rd, _) = StreamReader::open(batch).unwrap();
    let mut out = vec![];
    while let Some(r) = rd.next().unwrap() {
        out.push(r.key());
    }
    rd.finish().unwrap();
    out
}

/// The full batch `full` (no prerequisites) split at record boundaries into `parts` batches of
/// consecutive records, as many in each as fit, in order. Each batch repeats the header's
/// formats, repo, purpose and root; its prerequisites are exactly the keys its records need and
/// it does not hold, with the root when it does not hold it; it has its own trailer.
pub fn split_batch(full: &[u8], parts: usize) -> Vec<Vec<u8>> {
    let (mut rd, header) = StreamReader::open(full).unwrap();
    let mut records = vec![];
    while let Some(r) = rd.next().unwrap() {
        records.push(r);
    }
    rd.finish().unwrap();
    assert!(header.prerequisites.is_empty(), "a full batch");
    let all: BTreeSet<ObjectKey> = records.iter().map(StreamRecord::key).collect();
    let resolve = |l: ObjectRef| match l.kind {
        Some(kind) => ObjectKey { kind, hash: l.hash },
        None => SYNC_KINDS
            .iter()
            .map(|&kind| ObjectKey { kind, hash: l.hash })
            .find(|k| all.contains(k))
            .expect("a link resolves in the batch"),
    };
    let size = records.len().div_ceil(parts);
    records
        .chunks(size)
        .map(|part| {
            let mine: BTreeSet<ObjectKey> = part.iter().map(StreamRecord::key).collect();
            let mut needed = BTreeSet::new();
            for rec in part {
                for l in rec.refs().unwrap() {
                    needed.insert(resolve(l));
                }
                needed.extend(rec.proof());
            }
            needed.insert(header.root());
            needed.retain(|k| !mine.contains(k));
            let h = StreamHeader {
                prerequisites: needed.into_iter().collect(),
                ..header.clone()
            };
            let mut w = StreamWriter::new(Vec::new(), &h).unwrap();
            for rec in part {
                w.record(rec).unwrap();
            }
            w.finish().unwrap().0
        })
        .collect()
}

/// Whether a segment file of the store under `root` holds `bytes`.
fn holds(root: &Path, bytes: &[u8]) -> bool {
    std::fs::read_dir(root.join(".rdy/store/segments"))
        .unwrap()
        .map(|e| std::fs::read(e.unwrap().path()).unwrap())
        .any(|b| b.windows(bytes.len()).any(|w| w == bytes))
}

/// Copies the tree `from` to `to`, files byte for byte.
pub fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

/// The person as a signer: who asked for an approval's or a tag's signature.
fn person_signer() -> Provenance {
    Provenance {
        actor: PERSON.into(),
        run: None,
        role: Role::Approver,
        at_ms: 1,
    }
}

/// Stores `g` in `r` in its own batch.
fn put_signed(r: &mut Rdy, g: &Signature) {
    let repo = r.read().repo_id().unwrap();
    assert!(
        String::from_utf8_lossy(&g.statement).contains(&repo.to_string()),
        "the statement names the repo"
    );
    r.store_mut().put_signature(g).unwrap();
    r.seal_open().unwrap();
}

/// The person's signature of the approval of landing `l` of `r`.
fn sign_approval(r: &mut Rdy, l: Hash) {
    let repo = r.read().repo_id().unwrap();
    let record = r.store().landing(&l).unwrap();
    let g = sign_record(
        l,
        StatementSource::Approval(&record),
        repo,
        person_signer(),
        TestKey::Person,
    );
    put_signed(r, &g);
}

/// The person's signature of tag `t` of `r`.
fn sign_tag(r: &mut Rdy, t: Hash) {
    let repo = r.read().repo_id().unwrap();
    let tag = r.store().read_tag(&t).unwrap();
    let g = sign_record(
        t,
        StatementSource::Tag { hash: t, tag: &tag },
        repo,
        person_signer(),
        TestKey::Person,
    );
    put_signed(r, &g);
}

/// The agent's signature of the relay of tag `t` of `r`: the tag and the ANSWER that relayed it.
fn sign_relay_tag(r: &mut Rdy, t: Hash) {
    let repo = r.read().repo_id().unwrap();
    let tag = r.store().read_tag(&t).unwrap();
    let (answer_hash, answer) = r
        .store()
        .answer_of(&format!("tag:{}", tag.name))
        .unwrap()
        .expect("the relay answer");
    let agent = Provenance {
        actor: "agent:relay".into(),
        run: Some("chat".into()),
        role: Role::Writer,
        at_ms: 2,
    };
    let g = sign_record(
        t,
        StatementSource::RelayTag {
            hash: t,
            tag: &tag,
            answer_hash,
            answer: &answer,
        },
        repo,
        agent,
        TestKey::Agent,
    );
    put_signed(r, &g);
}
