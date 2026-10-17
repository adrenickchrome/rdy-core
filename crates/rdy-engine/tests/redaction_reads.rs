//! rdx6: source reads over removed content (D-3M81W, D-CC90E, D-EPK1Z). A removed version reads
//! as a stub that names its redaction in every scope; history keeps the original change, version
//! and landing hashes; tags keep their records and show a marker; event and inbox scans keep
//! their cursors over dropped frames; export skips a blob that lost a shared chunk.
//!
//! The store holds Task 2's raw authorized batch (intact payloads, active read masks), written
//! by the shared fixture while the project is closed.
#[path = "../../rdy-store/tests/support/redaction.rs"]
mod redaction_fixture;

use rdy_engine::events::replay;
use rdy_engine::inbox::read::InboxPage;
use rdy_engine::*;
use rdy_model::redaction::{CONTENT_REMOVED, RedactTarget};
use rdy_model::*;
use rdy_store::inbox::{InboxCursor, InboxSub};
use rdy_store::kinds::{self, BlobManifest};
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use redaction_fixture::{FixtureOptions, ORIGIN, Proof, RequestSpec, authorized_batch_fixture};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// The text every removed version held; no source read may show it after the removal.
const CANARY: &str = "CANARY";

/// The scopes the tests read, by their scope text (set once the proposal exists).
const SCOPES: [&str; 5] = ["canon", "view:edit", "view:gone", "tag:v1.0.0", "landing:"];

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn writer() -> Session {
    session(Role::Writer)
}

fn founder() -> Session {
    session(Role::Approver)
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy, view: &str, title: &str) -> String {
    let p = r.propose(&writer(), view, title, None).unwrap();
    r.land(&founder(), &p.id).unwrap()
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// What a scenario built: the ids, hashes and records the tests compare.
struct World {
    dir: tempfile::TempDir,
    doc: BlockId,
    section: BlockId,
    removed: BlockId,
    sibling: BlockId,
    shared_fig: BlockId,
    l0: String,
    l1: String,
    /// The change that wrote the doc, the section, both prose blocks and the figures.
    c1: Hash,
    proposal: String,
    /// The blob that loses a chunk it shares with the removed blob.
    shared_blob: Hash,
    /// The blob no request touches.
    plain_blob: Hash,
    request: Hash,
    /// Before the removal: each scope's version map.
    snapshots: BTreeMap<String, BTreeMap<BlockId, Hash>>,
    /// Before the removal: every tag's raw bytes, by name.
    tag_bytes: BTreeMap<String, (Hash, Vec<u8>)>,
    versions: Vec<VersionInfo>,
    /// Before the removal: the projection-independent change c1.
    change: Change,
    sibling_history: History,
    doc_history: History,
    /// Before the removal: where the log ended, its events and the inbox page from its start.
    end: Location,
    events: Vec<Event>,
    inbox: InboxPage,
    /// Before the removal: the batch marker locations of the log.
    markers: Vec<Location>,
}

impl World {
    fn scopes(&self) -> Vec<String> {
        let mut out: Vec<String> = SCOPES
            .iter()
            .map(|s| {
                if *s == "landing:" {
                    format!("landing:{}", self.l1)
                } else {
                    s.to_string()
                }
            })
            .collect();
        out.push(format!("proposal:{}", self.proposal));
        out
    }

    fn open(&self) -> Rdy {
        Rdy::open(self.dir.path()).unwrap()
    }
}

fn subscription() -> InboxSub {
    InboxSub {
        actor: "agent:a".into(),
        run: None,
        cursor: InboxCursor {
            at: ORIGIN,
            skip: 0,
        },
        areas: vec![],
        role: "writer".into(),
    }
}

fn markers(r: &Rdy, end: Location) -> Vec<Location> {
    let mut out = vec![];
    r.read()
        .reader()
        .for_each_record_from(ORIGIN, end, |loc, frame, _| {
            if let StoredFrame::Present { kind, .. } = frame
                && kind == kinds::BATCH_COMMIT
            {
                out.push(loc);
            }
            Ok(())
        })
        .unwrap();
    out
}

/// One project: an early landing tagged v0.1.0; a landing tagged v1.0.0 whose one change writes
/// a doc, a section with a canary title, a prose block with a canary body, a readable sibling and
/// two figures; a closed view that rewrote the canary block; an open, proposed view that edits the
/// sibling. Then one request removes every version of the section and the canary block, and a
/// blob that shares its only chunk with the first figure's blob.
fn world() -> World {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"name": "other", "title": "Other", "doc_type": "page"}}]),
    );
    let l0 = land(&mut r, "work", "other");
    r.tag(&founder(), "v0.1.0", None, Some("first".into()))
        .unwrap();
    let removed_bytes = vec![7u8; 1 << 20];
    let mut shared_bytes = removed_bytes.clone();
    shared_bytes.extend_from_slice(b"tail");
    let removed_blob = r.store_mut().put_blob(&removed_bytes).unwrap();
    let shared_blob = r.store_mut().put_blob(&shared_bytes).unwrap();
    let plain_blob = r.store_mut().put_blob(b"plain bytes").unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "CANARY title"}},
            {"op": "create", "type": "prose", "id": "$p1", "parent": "$s", "body": "CANARY body one."},
            {"op": "create", "type": "prose", "id": "$p2", "parent": "$s", "body": "Readable sibling."},
            {"op": "create", "type": "figure", "id": "$f1", "parent": "$d", "fields": {"blob": shared_blob.to_string(), "media_type": "application/octet-stream", "alt": "shared"}},
            {"op": "create", "type": "figure", "id": "$f2", "parent": "$d", "fields": {"blob": plain_blob.to_string(), "media_type": "application/octet-stream", "alt": "plain"}}
        ]),
    );
    let c1: Hash = w.change.parse().unwrap();
    let doc = bid(&w.created["$d"]);
    let section = bid(&w.created["$s"]);
    let removed = bid(&w.created["$p1"]);
    let sibling = bid(&w.created["$p2"]);
    let shared_fig = bid(&w.created["$f1"]);
    let l1 = land(&mut r, "work", "plan");
    r.tag(&founder(), "v1.0.0", None, Some("plan".into()))
        .unwrap();
    // a closed view that rewrote the canary block
    write(
        &mut r,
        "gone",
        json!([{"op": "update", "id": removed.to_string(), "body": "CANARY body two."}]),
    );
    r.view_close(&writer(), &["gone".to_string()], "abandoned")
        .unwrap();
    // an open view, proposed, that edits the sibling
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": sibling.to_string(), "body": "Edited sibling."}]),
    );
    let proposal = r.propose(&writer(), "edit", "edit", None).unwrap().r#ref;
    let mut world = World {
        dir,
        doc,
        section,
        removed,
        sibling,
        shared_fig,
        l0,
        l1,
        c1,
        proposal,
        shared_blob,
        plain_blob,
        request: Hash([0; 32]),
        snapshots: BTreeMap::new(),
        tag_bytes: BTreeMap::new(),
        versions: vec![],
        change: r.store().change(&c1).unwrap(),
        sibling_history: History {
            entries: vec![],
            more: false,
        },
        doc_history: History {
            entries: vec![],
            more: false,
        },
        end: ORIGIN,
        events: vec![],
        inbox: InboxPage {
            entries: vec![],
            cursor: String::new(),
            more: false,
            next: InboxCursor {
                at: ORIGIN,
                skip: 0,
            },
        },
        markers: vec![],
    };
    {
        let st = r.read();
        for text in world.scopes() {
            let scope = st.resolve_scope(&text).unwrap();
            let snap = st.materialized(&scope).unwrap().snapshot().blocks;
            assert!(snap.contains_key(&world.removed), "{text}");
            world.snapshots.insert(text, snap);
        }
        world.versions = st.versions().unwrap();
        for v in &world.versions {
            let h: Hash = v.tag.parse().unwrap();
            world
                .tag_bytes
                .insert(v.name.clone(), (h, st.reader().get_raw(&h).unwrap().1));
        }
        world.sibling_history = st.history(&Scope::Canon, &world.sibling, 1000).unwrap();
        world.doc_history = st.history(&Scope::Canon, &world.doc, 1000).unwrap();
        world.end = st.seq();
        world.events = replay(st.reader(), ORIGIN, world.end).unwrap();
        world.inbox = st.inbox_page(&subscription(), None, 1_000_000).unwrap();
    }
    world.markers = markers(&r, world.end);
    r.close().unwrap();
    let fixture = authorized_batch_fixture(
        &world.dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![
                RedactTarget::AllVersions(world.section.clone()),
                RedactTarget::AllVersions(world.removed.clone()),
                RedactTarget::Blob(removed_blob),
            ],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    world.request = fixture.requests[0].request;
    let shared = &fixture.requests[0].draft.request.manifest.shared_blobs;
    assert_eq!(
        shared,
        &vec![world.shared_blob],
        "the figure's blob shares the chunk"
    );
    world
}

fn json_of<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v).unwrap()
}

fn assert_no_canary(what: &str, text: &str) {
    assert!(!text.contains(CANARY), "{what} holds removed text: {text}");
}

#[test]
fn rdx6_every_scope_keeps_identity_and_returns_a_stub() {
    let w = world();
    let r = w.open();
    let st = r.read();
    for text in w.scopes() {
        let scope = st.resolve_scope(&text).unwrap();
        // every original version hash stays, the stub's included
        let snap = st.materialized(&scope).unwrap().snapshot().blocks;
        assert_eq!(
            &snap, &w.snapshots[&text],
            "{text}: the version map changed"
        );
        for id in [&w.section, &w.removed] {
            let b = st.block(&scope, id).unwrap();
            assert!(b.is_redacted(), "{text}: {id} is not a stub");
            assert_eq!(b.redaction(), Some(w.request), "{text}: {id}");
            assert_eq!(b.version_hash(), w.snapshots[&text][id], "{text}: {id}");
            assert_eq!(b.body, CONTENT_REMOVED, "{text}: {id}");
            assert!(b.fields.is_empty() && b.links.is_empty() && b.pins.is_empty());
            let ctx = serde_json::to_value(st.context(&scope, id).unwrap()).unwrap();
            assert_eq!(
                ctx["redaction"],
                json!({"id": w.request.to_string()}),
                "{text}: {id}"
            );
            assert_eq!(ctx["body"], json!(CONTENT_REMOVED), "{text}: {id}");
            assert_eq!(ctx["version"], json!(w.snapshots[&text][id].to_string()));
            assert_no_canary(&format!("{text} context of {id}"), &ctx.to_string());
        }
        // the readable blocks stay readable and name no redaction
        for id in [&w.sibling, &w.doc] {
            let b = st.block(&scope, id).unwrap();
            assert!(!b.is_redacted(), "{text}: {id}");
            let ctx = serde_json::to_value(st.context(&scope, id).unwrap()).unwrap();
            assert!(ctx.get("redaction").is_none(), "{text}: {id}: {ctx}");
        }
        let sibling = st.block(&scope, &w.sibling).unwrap().body;
        let want = if text == "view:edit" || text.starts_with("proposal:") {
            "Edited sibling."
        } else {
            "Readable sibling."
        };
        assert_eq!(sibling, want, "{text}");
        // the whole tree of the scope holds no removed text
        assert_no_canary(
            &format!("{text} tree"),
            &json_of(&st.tree(&scope, None, None).unwrap()),
        );
        let m = st.materialized(&scope).unwrap();
        for b in m.iter() {
            assert_no_canary(&format!("{text} block {}", b.id), &json_of(b));
        }
    }
}

#[test]
fn rdx6_tags_keep_their_records_and_show_a_marker() {
    let w = world();
    let r = w.open();
    let st = r.read();
    // TAG bytes, names, messages and landings stay as they were
    for (name, (h, bytes)) in &w.tag_bytes {
        assert_eq!(&st.reader().get_raw(h).unwrap().1, bytes, "{name}");
    }
    let after = st.versions().unwrap();
    assert_eq!(after.len(), 2);
    for (a, b) in after.iter().zip(&w.versions) {
        assert_eq!(
            (
                &a.name,
                &a.tag,
                &a.landing,
                &a.message,
                &a.tagged_by,
                a.at_ms
            ),
            (
                &b.name,
                &b.tag,
                &b.landing,
                &b.message,
                &b.tagged_by,
                b.at_ms
            )
        );
    }
    // v0.1.0 holds nothing removed: no marker members at all
    let early = serde_json::to_value(&after[0]).unwrap();
    assert_eq!(early["name"], "v0.1.0");
    assert_eq!(early["landing"], json!(w.l0));
    assert!(early.get("redacted").is_none(), "{early}");
    assert!(early.get("redactions").is_none(), "{early}");
    // v1.0.0's closure holds the removed versions
    let late = serde_json::to_value(&after[1]).unwrap();
    assert_eq!(late["name"], "v1.0.0");
    assert_eq!(late["landing"], json!(w.l1));
    assert_eq!(late["redacted"], json!(true), "{late}");
    assert_eq!(late["redactions"], json!([w.request.to_string()]), "{late}");
    assert_eq!(late["message"], json!("plan"), "the message is audit text");
    // the tag still names its landing as a scope
    assert_eq!(
        st.resolve_scope("tag:v1.0.0").unwrap(),
        Scope::At(w.l1.parse().unwrap())
    );
    // a version tagged after the removal shows the marker in the tag reply too
    drop(st);
    let mut r = r;
    let v = r.tag(&founder(), "v1.1.0", None, None).unwrap();
    assert!(v.redacted, "{v:?}");
    assert_eq!(v.redactions, vec![w.request.to_string()]);
    assert_eq!(r.read().versions().unwrap()[2], v);
}

#[test]
fn rdx6_source_reads_name_the_redaction_without_source_text() {
    let w = world();
    let r = w.open();
    let st = r.read();
    let note = format!("Redaction: {}.", w.request);
    let stub = st.block(&Scope::Canon, &w.removed).unwrap();
    let section = st.block(&Scope::Canon, &w.section).unwrap();
    let sibling = st.block(&Scope::Canon, &w.sibling).unwrap();
    // full: the source text is the notice, then the line that names the redaction
    assert_eq!(source_body(&stub), format!("{CONTENT_REMOVED}\n\n{note}"));
    assert_eq!(
        source_body(&section),
        format!("{CONTENT_REMOVED}\n\n{note}")
    );
    assert_eq!(source_body(&sibling), "Readable sibling.");
    // compact: one line
    assert_eq!(compact_summary(&stub), format!("{CONTENT_REMOVED} {note}"));
    assert_eq!(
        compact_summary(&section),
        format!("{CONTENT_REMOVED} {note}")
    );
    assert_eq!(compact_summary(&sibling), "Readable sibling.");
    // renderer summaries carry no source-only line
    assert_eq!(summary(&stub), CONTENT_REMOVED);
    // context: the stub's structural facts and its redaction, nothing else
    let ctx = serde_json::to_value(st.context(&Scope::Canon, &w.removed).unwrap()).unwrap();
    assert_eq!(ctx["id"], json!(w.removed.to_string()));
    assert_eq!(ctx["block_type"], "prose");
    assert_eq!(ctx["parent"], json!(w.section.to_string()));
    assert_eq!(ctx["fields"], json!({}));
    assert_eq!(ctx["body"], json!(CONTENT_REMOVED));
    assert_eq!(ctx["redaction"], json!({"id": w.request.to_string()}));
    assert_no_canary("context", &ctx.to_string());
    // history: the original change, version, author, message and landing, and the redaction
    let h = st.history(&Scope::Canon, &w.removed, 1000).unwrap();
    assert_eq!(h.entries.len(), 1);
    let e = serde_json::to_value(&h.entries[0]).unwrap();
    assert_eq!(
        e["version"],
        json!(w.snapshots["canon"][&w.removed].to_string())
    );
    assert_eq!(e["op"], "create");
    assert_eq!(e["change"], json!(w.c1.to_string()));
    assert_eq!(e["author"]["actor"], "agent:a");
    assert_eq!(e["message"], "write to work");
    assert_eq!(e["landing"]["id"], json!(w.l1));
    assert_eq!(e["redaction"], json!(w.request.to_string()));
    assert_no_canary("history", &e.to_string());
    let s = serde_json::to_value(st.history(&Scope::Canon, &w.sibling, 1000).unwrap()).unwrap();
    assert!(s["entries"][0].get("redaction").is_none(), "{s}");
    // the closed view's rewrite keeps its own version, change and the redaction
    let gone = Scope::View("gone".into());
    let h = serde_json::to_value(st.history(&gone, &w.removed, 1000).unwrap()).unwrap();
    assert_eq!(h["entries"][0]["op"], "update");
    assert_eq!(
        h["entries"][0]["version"],
        json!(w.snapshots["view:gone"][&w.removed].to_string())
    );
    assert_eq!(h["entries"][0]["redaction"], json!(w.request.to_string()));
    assert_eq!(h["entries"][1]["op"], "create");
    assert_eq!(h["entries"][1]["redaction"], json!(w.request.to_string()));
    assert_no_canary("closed view history", &h.to_string());
    // diff metadata: original ids and version hashes, the notice as the only body text
    let l0 = st.resolve_scope(&format!("landing:{}", w.l0)).unwrap();
    let diff = serde_json::to_value(st.diff(&l0, &Scope::Canon).unwrap()).unwrap();
    assert_no_canary("diff", &diff.to_string());
    let of = |d: &Value, id: &BlockId| -> Value {
        d["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == json!(id.to_string()))
            .unwrap_or_else(|| panic!("{id} in {d}"))
            .clone()
    };
    let hash = |h: &Hash| serde_json::to_value(h).unwrap();
    for id in [&w.removed, &w.section] {
        let e = of(&diff, id);
        assert_eq!(e["change"], "added", "{e}");
        assert_eq!(e["from_version"], Value::Null);
        assert_eq!(e["to_version"], hash(&w.snapshots["canon"][id]), "{e}");
        assert_eq!(
            e["body"],
            json!([{"op": "add", "text": CONTENT_REMOVED}]),
            "{e}"
        );
        assert_eq!(e["fields"], json!([]), "{e}");
        assert_eq!(e["links"], Value::Null, "{e}");
    }
    let e = of(&diff, &w.sibling);
    assert_eq!(
        e["body"],
        json!([{"op": "add", "text": "Readable sibling."}])
    );
    assert_eq!(e["to_version"], hash(&w.snapshots["canon"][&w.sibling]));
    // the closed view's rewrite: one update between the two original versions, no body text
    let gone_diff = serde_json::to_value(st.diff(&Scope::Canon, &gone).unwrap()).unwrap();
    assert_no_canary("diff of the closed view", &gone_diff.to_string());
    assert_eq!(gone_diff["counts"]["updated"], 1, "{gone_diff}");
    let e = of(&gone_diff, &w.removed);
    assert_eq!(e["change"], "updated");
    assert_eq!(e["from_version"], hash(&w.snapshots["canon"][&w.removed]));
    assert_eq!(e["to_version"], hash(&w.snapshots["view:gone"][&w.removed]));
    assert_eq!(e["body"], Value::Null, "both sides read the notice: {e}");
    assert_eq!(e["fields"], json!([]));
    // the compare lists the same blocks, by id and type only
    let cmp = serde_json::to_value(st.compare(&Scope::Canon, &l0).unwrap()).unwrap();
    assert_no_canary("compare", &cmp.to_string());
    assert_eq!(cmp["added"].as_array().unwrap().len(), 6, "{cmp}");
    let cmp = serde_json::to_value(st.compare(&gone, &Scope::Canon).unwrap()).unwrap();
    assert_no_canary("compare of the closed view", &cmp.to_string());
    assert_eq!(
        cmp["changed"],
        json!([{"block_type": "prose", "conflict": null, "fields": [], "from_parent": null, "id": w.removed.to_string(), "path": null, "to_parent": null}])
    );
}

#[test]
fn rdx6_events_and_inbox_keep_original_cursors() {
    let w = world();
    let r = w.open();
    let st = r.read();
    // the log holds dropped frames, and every batch marker stays where it was
    let mut dropped = 0;
    st.reader()
        .for_each_record_from(ORIGIN, w.end, |_, frame, _| {
            if let StoredFrame::Dropped { .. } = frame {
                dropped += 1;
            }
            Ok(())
        })
        .unwrap();
    assert!(dropped > 0, "the masks drop frames before the old end");
    assert_eq!(markers(&r, w.end), w.markers);
    // the events of the old log are the same events, at the same seqs
    let events = replay(st.reader(), ORIGIN, w.end).unwrap();
    assert!(!w.events.is_empty());
    assert_eq!(events, w.events);
    assert_no_canary("events", &json_of(&events));
    // the whole log replays, the request's carrier and its landing included
    let all = replay(st.reader(), ORIGIN, st.seq()).unwrap();
    assert_eq!(&all[..events.len()], &events[..]);
    let carrier = st.reader().redaction_request(&w.request).unwrap().carrier;
    assert!(
        all.iter().any(|e| matches!(
            &e.kind,
            EventKind::ChangeCommitted { change, view, .. } if *change == carrier && view == "redact"
        )),
        "the zero-op carrier is committed: {all:?}"
    );
    assert!(
        all[events.len()..]
            .iter()
            .any(|e| matches!(e.kind, EventKind::Landed { .. })),
        "the landing that applied the request"
    );
    // the inbox from its start gives the same entries, then the newer ones
    assert!(!w.inbox.entries.is_empty(), "the session has entries");
    let page = st.inbox_page(&subscription(), None, 1_000_000).unwrap();
    assert_eq!(&page.entries[..w.inbox.entries.len()], &w.inbox.entries[..]);
    assert_no_canary("inbox", &json_of(&page));
    // a cursor from before the removal still reads, from exactly where it was
    let old = st
        .inbox_page(&subscription(), Some(w.inbox.next), 1_000_000)
        .unwrap();
    assert_eq!(old.entries, page.entries[w.inbox.entries.len()..].to_vec());
}

#[test]
fn rdx6_history_preserves_unaffected_sibling_ops() {
    let w = world();
    let r = w.open();
    let st = r.read();
    let c = st.reader().change(&w.c1).unwrap();
    assert!(c.is_projection());
    assert_eq!(c.id(), w.c1, "the change keeps its original id");
    assert_eq!(c.deps, w.change.deps);
    assert_eq!(c.provenance, w.change.provenance);
    assert_eq!(c.justification, w.change.justification);
    assert_eq!(c.message, w.change.message);
    assert_eq!(c.links, w.change.links);
    assert_eq!(c.ops.len(), w.change.ops.len());
    for (now, was) in c.ops.iter().zip(&w.change.ops) {
        let (Op::Create { block: now }, Op::Create { block: was }) = (now, was) else {
            panic!("c1 creates only");
        };
        assert_eq!(now.id, was.id, "op order");
        assert_eq!(now.version_hash(), was.version_hash(), "{}", was.id);
        if was.id == w.section || was.id == w.removed {
            assert!(now.is_redacted(), "{}", was.id);
            assert_eq!(now.redaction(), Some(w.request));
        } else {
            assert_eq!(now, was, "{} is unaffected", was.id);
        }
    }
    // the siblings' history is what it was: same change, version, author and landing
    let sibling = st.history(&Scope::Canon, &w.sibling, 1000).unwrap();
    assert_eq!(sibling, w.sibling_history);
    assert_eq!(
        st.history(&Scope::Canon, &w.doc, 1000).unwrap(),
        w.doc_history
    );
    assert_eq!(
        st.history(&Scope::Canon, &w.shared_fig, 1000)
            .unwrap()
            .entries[0]
            .change,
        w.c1.to_string()
    );
    // the erased block's entry is the same entry, naming its redaction
    let removed = st.history(&Scope::Canon, &w.removed, 1000).unwrap();
    assert_eq!(removed.entries[0].change, sibling.entries[0].change);
    assert_eq!(removed.entries[0].landing, sibling.entries[0].landing);
    assert_eq!(removed.entries[0].author, sibling.entries[0].author);
    assert_eq!(
        removed.entries[0].redaction,
        Some(w.request.to_string()),
        "the erased block's entry names its redaction"
    );
    assert_eq!(sibling.entries[0].redaction, None);
    // one call over every block reads each last change through the projection
    let m = st.materialized(&Scope::Canon).unwrap();
    let ids: Vec<BlockId> = m.iter().map(|b| b.id.clone()).collect();
    let last = st.last_changes(&Scope::Canon, &ids).unwrap();
    assert_eq!(last.len(), ids.len());
    assert_no_canary("last changes", &json_of(&last));
}

/// Writes a blob manifest whose only chunk the store does not hold, and a figure in view `view`
/// that names it; returns the manifest's hash and the figure's id.
fn broken_blob(r: &mut Rdy, view: &str, parent: &BlockId) -> (Hash, String) {
    let manifest = BlobManifest {
        len: 4,
        chunks: vec![Hash::of(b"a chunk the store never held")],
    };
    let h = r
        .store_mut()
        .write(vec![(
            kinds::BLOB_MANIFEST,
            redaction_fixture::enc(&manifest),
        )])
        .unwrap()[0];
    let res = write(
        r,
        view,
        json!([{"op": "create", "type": "figure", "id": "$f", "parent": parent.to_string(), "fields": {"blob": h.to_string(), "media_type": "application/octet-stream", "alt": "broken"}}]),
    );
    (h, res.created["$f"].clone())
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

#[test]
fn rdx6_export_and_checkout_skip_an_unreadable_blob() {
    let w = world();
    let mut r = w.open();
    // the figure keeps its blob field; the blob itself no longer reads
    let fig = r.read().block(&Scope::Canon, &w.shared_fig).unwrap();
    assert_eq!(
        fig.field_str("blob"),
        Some(w.shared_blob.to_string().as_str())
    );
    assert!(matches!(
        r.read().reader().get_blob(&w.shared_blob),
        Err(rdy_store::StoreError::Redacted { request }) if request == w.request
    ));
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("export");
    let summary = r.read().export(&Scope::Canon, &dest).unwrap();
    assert_eq!(summary.blobs, 1, "the unreadable blob is not counted");
    assert_eq!(
        files_in(&dest.join("blobs")),
        vec![w.plain_blob.to_string()]
    );
    assert_eq!(
        std::fs::read(dest.join("blobs").join(w.plain_blob.to_string())).unwrap(),
        b"plain bytes"
    );
    assert_eq!(summary.docs, 2);
    assert_eq!(
        summary.blocks,
        r.read().materialized(&Scope::Canon).unwrap().len()
    );
    assert!(dest.join("rdy-export.yaml").is_file());
    for entry in walk(&dest) {
        assert_no_canary(
            &entry.display().to_string(),
            &String::from_utf8_lossy(&std::fs::read(&entry).unwrap()),
        );
    }
    // an unrelated blob error keeps its text
    let (h, figure) = broken_blob(&mut r, "broken", &w.doc);
    let e = r
        .read()
        .export(&Scope::View("broken".into()), &out.path().join("broken"))
        .unwrap_err();
    assert_eq!(e.code, E_NOT_FOUND);
    assert_eq!(
        e.message,
        format!("{figure} names blob {h}, which is not a blob in the store")
    );
    assert_eq!(
        e.fix,
        format!("attach the asset again (rdy_asset) and point {figure} at it, or delete {figure}")
    );
    assert!(
        !out.path().join("broken").exists(),
        "a failed export leaves nothing"
    );
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// A project with an early landing tagged v0.1.0 (a doc that nothing removes), then one write in
/// view `work` of `ops`, landed and tagged v1.0.0. Returns the open project and the write.
fn tagged_world(dir: &Path, ops: impl FnOnce(&mut Rdy) -> Value) -> (Rdy, WriteResult) {
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"name": "other", "title": "Other", "doc_type": "page"}}]),
    );
    land(&mut r, "work", "other");
    r.tag(&founder(), "v0.1.0", None, Some("first".into()))
        .unwrap();
    let ops = ops(&mut r);
    let w = write(&mut r, "work", ops);
    land(&mut r, "work", "plan");
    r.tag(&founder(), "v1.0.0", None, Some("plan".into()))
        .unwrap();
    (r, w)
}

/// Authorizes one request of `targets`, recorded in view `view` at `at_ms`, against the closed
/// project at `dir`; returns the request's hash.
fn authorize(dir: &Path, view: &str, at_ms: i64, targets: Vec<RedactTarget>) -> Hash {
    let fixture = authorized_batch_fixture(
        &dir.join(".rdy/store"),
        &[RequestSpec::new(view, targets).at(at_ms)],
        &FixtureOptions::approved(Proof::local_human()),
    );
    fixture.requests[0].request
}

/// The versions' marker members, by name: (redacted, redactions) as JSON.
fn markers_of(st: &ReadState) -> BTreeMap<String, (Value, Value)> {
    st.versions()
        .unwrap()
        .iter()
        .map(|v| {
            let j = serde_json::to_value(v).unwrap();
            (
                v.name.clone(),
                (
                    j.get("redacted").cloned().unwrap_or(Value::Null),
                    j.get("redactions").cloned().unwrap_or(Value::Null),
                ),
            )
        })
        .collect()
}

#[test]
fn rdx6_tags_mark_a_blob_only_removal() {
    let dir = tempfile::tempdir().unwrap();
    let mut blob = Hash([0; 32]);
    let (r, w) = tagged_world(dir.path(), |r| {
        blob = r.store_mut().put_blob(b"secret figure bytes").unwrap();
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$d", "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "figure"}}
        ])
    });
    let fig = bid(&w.created["$f"]);
    r.close().unwrap();
    // the request removes the blob only: no block version and no change
    let request = authorize(dir.path(), "redact", 40, vec![RedactTarget::Blob(blob)]);
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    assert!(matches!(
        st.reader().get_blob(&blob),
        Err(rdy_store::StoreError::Redacted { request: q }) if q == request
    ));
    let c: Hash = w.change.parse().unwrap();
    assert_eq!(st.reader().redaction_of(kinds::CHANGE, &c).unwrap(), None);
    assert!(!st.block(&Scope::Canon, &fig).unwrap().is_redacted());
    let marks = markers_of(&st);
    assert_eq!(marks["v0.1.0"], (Value::Null, Value::Null));
    assert_eq!(
        marks["v1.0.0"],
        (json!(true), json!([request.to_string()])),
        "the tag's figure names a removed blob"
    );
}

#[test]
fn rdx6_tags_mark_a_shared_chunk_removal() {
    let dir = tempfile::tempdir().unwrap();
    let (mut removed, mut shared) = (Hash([0; 32]), Hash([0; 32]));
    let (r, _) = tagged_world(dir.path(), |r| {
        let removed_bytes = vec![7u8; 1 << 20];
        let mut shared_bytes = removed_bytes.clone();
        shared_bytes.extend_from_slice(b"tail");
        // the removed blob is in the store, but no block names it
        removed = r.store_mut().put_blob(&removed_bytes).unwrap();
        shared = r.store_mut().put_blob(&shared_bytes).unwrap();
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "figure", "id": "$f", "parent": "$d", "fields": {"blob": shared.to_string(), "media_type": "application/octet-stream", "alt": "shared"}}
        ])
    });
    r.close().unwrap();
    let fixture = authorized_batch_fixture(
        &dir.path().join(".rdy/store"),
        &[RequestSpec::new(
            "redact",
            vec![RedactTarget::Blob(removed)],
        )],
        &FixtureOptions::approved(Proof::local_human()),
    );
    let request = fixture.requests[0].request;
    assert_eq!(
        fixture.requests[0].draft.request.manifest.shared_blobs,
        vec![shared],
        "the figure's blob shares the removed chunk"
    );
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    assert_eq!(
        st.reader()
            .redaction_of(kinds::BLOB_MANIFEST, &shared)
            .unwrap(),
        None,
        "the figure's own manifest is not selected"
    );
    assert!(matches!(
        st.reader().get_blob(&shared),
        Err(rdy_store::StoreError::Redacted { request: q }) if q == request
    ));
    let marks = markers_of(&st);
    assert_eq!(marks["v0.1.0"], (Value::Null, Value::Null));
    assert_eq!(
        marks["v1.0.0"],
        (json!(true), json!([request.to_string()])),
        "the tag's figure names a blob that lost a shared chunk"
    );
}

#[test]
fn rdx6_tags_list_every_sibling_removal() {
    let dir = tempfile::tempdir().unwrap();
    let (r, w) = tagged_world(dir.path(), |_| {
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "plan", "title": "Plan", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$a", "parent": "$d", "body": "CANARY a."},
            {"op": "create", "type": "prose", "id": "$b", "parent": "$d", "body": "CANARY b."}
        ])
    });
    let (a, b) = (bid(&w.created["$a"]), bid(&w.created["$b"]));
    r.close().unwrap();
    // two removals, one after the other, of two blocks one change wrote
    let r1 = authorize(
        dir.path(),
        "redact-a",
        40,
        vec![RedactTarget::AllVersions(a.clone())],
    );
    let r2 = authorize(
        dir.path(),
        "redact-b",
        50,
        vec![RedactTarget::AllVersions(b.clone())],
    );
    let r = Rdy::open(dir.path()).unwrap();
    let st = r.read();
    assert_eq!(st.block(&Scope::Canon, &a).unwrap().redaction(), Some(r1));
    assert_eq!(st.block(&Scope::Canon, &b).unwrap().redaction(), Some(r2));
    // the change's own mask names one request only
    let c: Hash = w.change.parse().unwrap();
    assert_eq!(
        st.reader().redaction_of(kinds::CHANGE, &c).unwrap(),
        Some(r1)
    );
    let mut want = vec![r1.to_string(), r2.to_string()];
    want.sort();
    let marks = markers_of(&st);
    assert_eq!(marks["v0.1.0"], (Value::Null, Value::Null));
    assert_eq!(
        marks["v1.0.0"],
        (json!(true), json!(want)),
        "both requests that removed the change's blocks"
    );
}
