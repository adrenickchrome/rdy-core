//! The every-kind project: one project that holds an object of every kind of dump format 2 (store
//! format 13) and a ref of every family, and one read of it ("facts") that a later rdy can
//! repeat. The project is a clone, so it holds `meta/origin` (O41). Its source's last landing is a
//! person's approval of a content removal, so its log holds dropped payloads (D-3M81W). Shared by
//! the sg6 store test and the dump, load and clone tests (D-69AY3, D-EV4F1).
use crate::fixtures::*;
use crate::inbox::on;
use crate::intake::{PERSON, confirm_exceptions, decide_in};
use crate::packs::qa_project;
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
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::tensions::{TensionKind, tension_label};
use rdy_engine::{DecisionInput, Engine, LandOptions, Local, Rdy, Scope, Session};
use rdy_model::redaction::{Redaction, RedactionApply};
use rdy_model::{
    ActorRun, BlockId, Finding, Hash, PairItem, Provenance, RelayEvidence, Review, ReviewSubject,
    Role, SemanticTension, Verdict, pair_key,
};
use rdy_store::Relay;
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::{Location, SegmentLog};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

/// The ref families of an every-kind store (store format 13): the ten of an M2 store, `canon`,
/// the open proposals (`proposal/`), the answers (`answer/`), the snoozes (`pending/`), the repo
/// id (`meta/repo_id`) and the clone's origin (`meta/origin`). Store format 13 adds kinds and no
/// family: a redaction has no ref. A store format that adds a family adds it here; each frozen
/// fixture test keeps its own list (`V1_FAMILIES` in `io_dump_fixture.rs`, `V11_FAMILIES` in
/// `io_dump_fixture_s11.rs`, `V12_FAMILIES` in `io_dump_fixture_s12.rs`, `V13_FAMILIES` in
/// `io_dump_fixture_s13.rs`).
pub const FAMILIES: [&str; 16] = [
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
        reason: String::new(),
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
    let first = SemanticTension {
        key,
        items,
        review: Hash::of(&postcard::to_allocvec(&review).unwrap()),
        advice: Some(advice.into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: review.provenance.clone(),
    };
    r.record_verdict(&review, Some(&first)).unwrap();
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
/// blob shares. The project returned is a clone of the one built (O41): it keeps the built
/// project's log byte for byte, then holds its origin batch (`meta/origin`), which names the
/// removal's landing.
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
/// `{"refs": {<name>: <hex value>}, "objects": {<kind>: <count>}, "landings": [<hash>], "fsck": <FsckReport>}`.
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
    })
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
