//! The every-kind project: one project that holds an object of every kind of dump format 1 and a
//! ref of every family, and one read of it ("facts") that a later rdy can repeat. Shared by the
//! sg6 store test and the dump and load tests (D-69AY3, D-EV4F1).
use crate::fixtures::*;
use crate::inbox::on;
use crate::packs::qa_project;
use crate::tensions::{ag, created, one_involving, put, upd};
use rdy_engine::alt::AltMemberInput;
use rdy_engine::inbox::api::{InboxRequest, run_inbox};
use rdy_engine::inbox::sub::subscribe;
use rdy_engine::mention_review::MentionVerdict;
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::tensions::{TensionKind, tension_label};
use rdy_engine::{Engine, LandOptions, Local, Rdy, Scope, Session};
use rdy_model::{
    ActorRun, BlockId, Finding, Hash, PairItem, Provenance, RelayEvidence, Review, ReviewSubject,
    Role, SemanticTension, Verdict, pair_key,
};
use rdy_store::Relay;
use rdy_store::kinds;
use rdy_store::segment::Location;
use serde_json::json;
use std::collections::BTreeMap;
use std::path::Path;

/// The ref families of an every-kind store (store format 10): the ten of an M2 store, `canon`
/// and the open proposals (`proposal/`).
pub const FAMILIES: [&str; 12] = [
    "meta/store_format",
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
];

/// The object kinds of dump format 1 that the project holds: every kind but the batch marker.
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
        case_hash: Some(Hash([9; 32])),
        verdict: Verdict::Tension,
        reason: String::new(),
        findings: vec![Finding {
            block: y.clone(),
            note: "three and four cannot both hold".into(),
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
        advice: Some("four contradicts three".into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: review.provenance.clone(),
    };
    r.record_verdict(&review, Some(&first)).unwrap();
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
/// does), that holds at least one object of every kind of `V1_KINDS` and one ref of every family
/// of `FAMILIES`: an imported blob, the open proposal `sa` (`proposal/`), a `.rdyignore` with the
/// line `*.draft.yaml`, and a `.rdy/local.yaml` with `actor: human:qa` (a dump leaves it out).
/// Closed.
pub fn project() -> tempfile::TempDir {
    let (dir, r) = qa_project();
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
        r.propose(&ag("sa"), "sa", "sa").unwrap()
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
        let a = r.accept_advice(&ag("relay"), req).unwrap();
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
        let p = r.propose(&ag("rel"), "rel", "rel").unwrap();
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
    // the inbox ack moves the persisted cursor
    let before = e.read().reader().inbox("agent:a", Some("a")).unwrap();
    let req: InboxRequest = serde_json::from_value(json!({"ack": true})).unwrap();
    run_inbox(&e, &ag("a"), req).unwrap();
    let after = e.read().reader().inbox("agent:a", Some("a")).unwrap();
    assert!(
        after.as_ref().unwrap().cursor > before.unwrap().cursor,
        "the ack moved the cursor"
    );
    e.close().unwrap();
    std::fs::write(dir.path().join(".rdyignore"), "*.draft.yaml\n").unwrap();
    std::fs::write(dir.path().join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    dir
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What a later rdy must read the same from this project, after a dump and a load:
/// `{"refs": {<name>: <hex value>}, "objects": {<kind>: <count>}, "landings": [<hash>], "fsck": <FsckReport>}`.
/// `refs` is every ref. `objects` counts the data frames by kind from the log start to `seq()`
/// (batch markers left out). `landings` is canon's landing chain, oldest first.
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
        .for_each_frame_from(origin, st.seq(), |_, kind, _, _, _| {
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
