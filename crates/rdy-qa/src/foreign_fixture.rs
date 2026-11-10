//! The first-use `foreign_copy` fixtures of repo links (D-FK58A, D-8VJAZ, D-CN4QJ, D-DAPR8): one
//! cited project and one citing project, built only through the product's own engine calls. No
//! foreign record is written by hand.
//!
//! The world:
//!
//! 1. The cited project (`rdy-standard`) lands doc `rules`: a section with the claims `kept` and
//!    `removed`, and an accepted decision. Its publication profile `agents` (org) selects the
//!    doc. It tags [`FC_TAG1`] and writes the `agents` link delivery at that tag
//!    (`link_export::write`): the delivery is sync format 3, purpose `link_delivery`.
//! 2. The cited project changes `kept`, lands, tags [`FC_TAG2`] and writes the second delivery.
//! 3. The citing project lands doc `guide` with a section and two repo links to the cited repo,
//!    `core` and `old`. The first repo link turns `foreign_copy` on.
//! 4. `core` takes the first delivery (`take_delivery`). `old` takes it too, then the link is
//!    revoked (`revoke_link`): revoked history.
//! 5. The citing claim cites `kept` and `removed` in its body (pins) and names the cited
//!    decision in `decided_by` (a foreign link), and lands.
//! 6. `core` takes the second delivery: `kept` changes, and the person answers the adopt card
//!    with `adopt` (one adoption landing, the pin moves).
//! 7. The cited project removes `removed` with a person's approval. Its history at [`FC_TAG2`]
//!    carries the portable removal proof; the citing project takes it
//!    (`foreign_removal::take_foreign_removal`): one `Removal` entry erases the citing copy.
//!
//! The members are written once from this world; their readers never rebuild it. The facts
//! functions here read a store as the expect files record it, for the writer and the readers.
use crate::every_kind;
use crate::fixtures::{approver, land, write, writer};
use crate::intake::{PERSON, TEST_SUMMARY, pending};
use rdy_engine::answer::AnswerRequest;
use rdy_engine::foreign_delivery::{revoke_link, take_delivery};
use rdy_engine::foreign_removal::take_foreign_removal;
use rdy_engine::pending::ItemKind;
use rdy_engine::redaction::RedactInput;
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_model::foreign::ForeignSource;
use rdy_model::{Answer, AnswerAct, BlockId, Hash, RepoId};
use rdy_store::StoreError;
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::segment::Location;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

/// The cited tag of the first delivery.
pub const FC_TAG1: &str = "v1.0.0";
/// The cited tag of the second delivery and of the history that carries the removal.
pub const FC_TAG2: &str = "v1.1.0";
/// The key of the cited publication profile both links name (by its id).
pub const FC_PROFILE: &str = "agents";
/// The site and the time of every link source.
pub const FC_SITE: &str = "hub.example";
pub const FC_AT_MS: i64 = 1_760_000_000_000;
/// The live link's alias, and the revoked link's.
pub const FC_LIVE: &str = "core";
pub const FC_REVOKED: &str = "old";
/// The body of `kept` at each tag, and of `removed`.
pub const FC_KEPT_1: &str = "Every service logs in UTC.";
pub const FC_KEPT_2: &str = "Every service logs in UTC, with the offset.";
pub const FC_REMOVED: &str = "Queues hold at most ten thousand jobs.";
/// The statement of the cited decision.
pub const FC_DECISION: &str = "Logs keep thirty days.";

/// The built world: its directories, the ids, the records the members must hold, and the bytes
/// of the first delivery.
pub struct ForeignWorld {
    /// The cited project, the citing project and the stream files, under one directory.
    pub dir: tempfile::TempDir,
    pub cited_root: PathBuf,
    pub citing_root: PathBuf,
    /// The cited repo.
    pub repo: RepoId,
    /// The cited claims and decision.
    pub kept: BlockId,
    pub removed: BlockId,
    pub decision: BlockId,
    /// The citing repo links (`core`, live; `old`, revoked) and the citing claim.
    pub live: BlockId,
    pub revoked: BlockId,
    pub claim: BlockId,
    /// The person's Adopt answer and the foreign Removal entry.
    pub adopt_answer: Hash,
    pub removal_entry: Hash,
    /// The first delivery's stream (sync format 3), as the cited project wrote it.
    pub delivery: Vec<u8>,
}

fn bid(w: &rdy_engine::WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

fn landed(r: &mut Rdy, view: &str) {
    land(r, view).unwrap_or_else(|e| panic!("land {view}: {e:?}"));
}

/// The tag `name` of `r`: its hash and landing.
fn tag_of(r: &Rdy, name: &str) -> (Hash, Hash) {
    let (_, h, t) = r
        .store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, ..)| n == name)
        .unwrap_or_else(|| panic!("tag {name}"));
    (h, t.landing)
}

/// The `agents` link delivery of `r` at tag `name`.
fn delivery_at(r: &Rdy, name: &str) -> Vec<u8> {
    let (h, landing) = tag_of(r, name);
    let mut out = Vec::new();
    rdy_engine::link_export::write(&r.read(), name, h, landing, FC_PROFILE, &mut out)
        .unwrap_or_else(|e| panic!("delivery at {name}: {e:?}"));
    out
}

/// The history of `r` at tag `name`, as one sync batch.
fn history_at(r: &Rdy, name: &str) -> Vec<u8> {
    let (h, _) = tag_of(r, name);
    let mut out = Vec::new();
    rdy_engine::stream_export::write(&r.read(), h, &Default::default(), &mut out)
        .unwrap_or_else(|e| panic!("history at {name}: {e:?}"));
    out
}

/// `bytes`, written to `dir/name` and opened.
fn file(dir: &Path, name: &str, bytes: &[u8]) -> File {
    let p = dir.join(name);
    std::fs::write(&p, bytes).unwrap();
    File::open(p).unwrap()
}

fn source(link: &BlockId) -> ForeignSource {
    ForeignSource::Link {
        link: link.to_string(),
        site: FC_SITE.into(),
        at_ms: FC_AT_MS,
    }
}

/// The cited project after steps 1 and 2: its repo, its profile, `kept`, `removed`, the
/// decision and both deliveries.
struct Cited {
    repo: RepoId,
    profile: BlockId,
    kept: BlockId,
    removed: BlockId,
    decision: BlockId,
    first: Vec<u8>,
    second: Vec<u8>,
}

/// The cited project: steps 1 and 2.
fn cited(root: &Path) -> Cited {
    let mut c = Rdy::init(root, "rdy-standard").unwrap();
    let w = put(
        &mut c,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Service rules"}},
            {"op": "create", "type": "claim", "id": "$k", "parent": "$s", "body": FC_KEPT_1},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": FC_REMOVED},
            {"op": "create", "type": "doc", "id": "$admin",
             "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (doc, section, kept, removed, admin) = (
        bid(&w, "$d"),
        bid(&w, "$s"),
        bid(&w, "$k"),
        bid(&w, "$x"),
        bid(&w, "$admin"),
    );
    let p = put(
        &mut c,
        "work",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$p", "parent": admin.to_string(),
             "fields": {"key": FC_PROFILE, "title": "Agents", "audience": "org",
                        "select": [doc.to_string()]}}
        ]),
    );
    let profile = bid(&p, "$p");
    landed(&mut c, "work");
    let decided = c
        .decide(
            &writer(),
            "dec",
            DecisionInput {
                parent: section.to_string(),
                statement: FC_DECISION.into(),
                title: Some("Log retention".into()),
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("decide: {e:?}"));
    let decision = BlockId::parse(&decided.created["$decision"]).unwrap();
    landed(&mut c, "dec");
    c.tag(&approver(), FC_TAG1, None, None).unwrap();
    let first = delivery_at(&c, FC_TAG1);
    put(
        &mut c,
        "work2",
        json!([{"op": "update", "id": kept.to_string(), "body": FC_KEPT_2}]),
    );
    landed(&mut c, "work2");
    c.tag(&approver(), FC_TAG2, None, None).unwrap();
    let second = delivery_at(&c, FC_TAG2);
    let repo = c.store().reader().repo_id().unwrap().unwrap();
    c.close().unwrap();
    Cited {
        repo,
        profile,
        kept,
        removed,
        decision,
        first,
        second,
    }
}

/// Step 7 on the cited side: removes `removed` with a person's approval; returns the history at
/// [`FC_TAG2`], which carries the removal's portable proof.
fn cited_removal(root: &Path, removed: &BlockId) -> Vec<u8> {
    let mut c = Rdy::open(root).unwrap();
    c.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![removed.to_string()],
            reason_class: "secret".into(),
            reason: "an internal limit".into(),
            dry_run: false,
        },
    )
    .unwrap_or_else(|e| panic!("redact: {e:?}"));
    let p = c
        .propose(&writer(), "redact", "Remove the limit", Some(TEST_SUMMARY))
        .unwrap();
    c.land(&approver(), &p.id)
        .unwrap_or_else(|e| panic!("land the removal: {e:?}"));
    let history = history_at(&c, FC_TAG2);
    c.close().unwrap();
    history
}

/// Builds the world (see the module) in a new temporary directory. Both projects are closed.
pub fn build() -> ForeignWorld {
    let dir = tempfile::tempdir().unwrap();
    let cited_root = dir.path().join("cited");
    let citing_root = dir.path().join("citing");
    let files = dir.path().join("files");
    std::fs::create_dir_all(&files).unwrap();
    let Cited {
        repo,
        profile,
        kept,
        removed,
        decision,
        first,
        second,
    } = cited(&cited_root);

    // 3. the citing seed and its two repo links
    let mut r = Rdy::init(&citing_root, "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Rules we follow"}},
            {"op": "create", "type": "repo_link", "id": "$core", "parent": "$d",
             "fields": {"alias": FC_LIVE, "repo": repo.to_string(), "profile": profile.to_string()}},
            {"op": "create", "type": "repo_link", "id": "$old", "parent": "$d",
             "fields": {"alias": FC_REVOKED, "repo": repo.to_string(), "profile": profile.to_string()}}
        ]),
    );
    let (section, live, revoked) = (bid(&w, "$s"), bid(&w, "$core"), bid(&w, "$old"));
    landed(&mut r, "seed");

    // 4. both links take the first delivery; `old` is revoked
    let taken = take_delivery(&mut r, &file(&files, "d1", &first), &source(&live), None)
        .unwrap_or_else(|e| panic!("core takes v1: {e:?}"));
    assert!(!taken.unchanged && taken.added >= 3, "{taken:?}");
    take_delivery(
        &mut r,
        &file(&files, "d1-old", &first),
        &source(&revoked),
        None,
    )
    .unwrap_or_else(|e| panic!("old takes v1: {e:?}"));
    let gone = revoke_link(&mut r, &source(&revoked)).unwrap();
    assert!(!gone.unchanged);

    // 5. the citing claim: two pins and a foreign decided_by link
    let w = put(
        &mut r,
        "cite",
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": section.to_string(),
                "body": format!("Our jobs follow {{{{{FC_LIVE}:{kept}}}}} and {{{{{FC_LIVE}:{removed}}}}}."),
                "links": {"decided_by": [{"target": format!("{FC_LIVE}:{decision}")}]}}]),
    );
    let claim = bid(&w, "$c");
    landed(&mut r, "cite");

    // 6. the second delivery changes `kept`; the person adopts it
    let taken = take_delivery(&mut r, &file(&files, "d2", &second), &source(&live), None)
        .unwrap_or_else(|e| panic!("core takes v2: {e:?}"));
    assert_eq!(taken.changed, 1, "{taken:?}");
    let cards: Vec<_> = pending(&r)
        .items
        .into_iter()
        .filter(|i| i.kind == ItemKind::Adopt)
        .collect();
    assert_eq!(cards.len(), 1, "one adopt card: {cards:#?}");
    let card = &cards[0];
    let info = r
        .answer(
            &approver(),
            PERSON,
            AnswerRequest {
                r#ref: card.r#ref.clone(),
                option: "adopt".into(),
                card_hash: Some(card.card_hash.clone()),
                ..AnswerRequest::default()
            },
        )
        .unwrap_or_else(|e| panic!("adopt: {e:?}"));
    let adopt_answer: Hash = info.answer.expect("the adopt answer").parse().unwrap();

    // 7. the cited removal, taken
    let history = cited_removal(&cited_root, &removed);
    let removal = take_foreign_removal(&mut r, &file(&files, "history", &history), &repo)
        .unwrap_or_else(|e| panic!("take the removal: {e:?}"));
    let removal_entry = removal
        .removals
        .iter()
        .find_map(|t| t.entry)
        .expect("a Removal entry");
    r.seal_open().unwrap();
    r.close().unwrap();
    ForeignWorld {
        dir,
        cited_root,
        citing_root,
        repo,
        kept,
        removed,
        decision,
        live,
        revoked,
        claim,
        adopt_answer,
        removal_entry,
        delivery: first,
    }
}

// ---------------------------------------------------------------------------------------------
// the facts an expect file records

fn origin() -> Location {
    Location {
        segment: 0,
        offset: 0,
    }
}

/// The fsck report without its `unlogged_moves` count: analysis a later build changes, not damage.
fn without_moves(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(m) = v.as_object_mut() {
        m.remove("unlogged_moves");
    }
    v
}

/// What an open of a store must read (the native store fixture's facts, as `feature-1` records
/// them): the manifest, the raw encodings, the log end, the frame and batch counts, the objects
/// and their keys, the removed objects, the refs, the tags, the landings, the signatures and the
/// fsck report.
pub fn store_facts(r: &Rdy) -> Value {
    let end = r.store().log_end();
    let (mut frames, mut batches) = (0u64, 0u64);
    r.read()
        .reader()
        .for_each_record_from(origin(), end, |_, f, _| {
            match f {
                StoredFrame::Present { kind, .. } if kind == kinds::BATCH_COMMIT => batches += 1,
                _ => frames += 1,
            }
            Ok(())
        })
        .unwrap();
    let facts = every_kind::facts(r);
    let identity = every_kind::identity_facts(r, Some(end));
    let features = every_kind::feature_facts(r, end);
    json!({
        "manifest": features["manifest"],
        "raw_encodings": features["encodings"],
        "log_end": [end.segment, end.offset],
        "frames": frames,
        "batches": batches,
        "objects": facts["objects"],
        "object_keys": identity["keys"],
        "redacted": identity["redacted"],
        "refs": facts["refs"],
        "tags": identity["tags"],
        "landings": facts["landings"],
        "signatures": facts["signatures"],
        "fsck": without_moves(&facts["fsck"]),
    })
}

/// The foreign copy of `r`: every FOREIGN entry by hash, each source with its newest entry and
/// whether it is revoked, the canon repo links, the canon blocks with a foreign part, the Adopt
/// answers, and the BLOCK versions that read as removed.
pub fn foreign_facts(r: &Rdy) -> Value {
    let st = r.read();
    let reader = st.reader();
    let mut entries = vec![];
    reader
        .for_each_object_of(kinds::FOREIGN, |h| {
            let e = reader.foreign_entry(&h)?;
            entries.push(json!({"hash": h.to_string(), "entry": e}));
            Ok(())
        })
        .unwrap();
    let mut sources = vec![];
    for (name, head) in reader.foreign_sources().unwrap() {
        let view = reader.source_view(&name).unwrap().unwrap();
        sources.push(json!({
            "ref": name,
            "head": head.to_string(),
            "repo": view.repo.to_string(),
            "revoked": view.revoked,
            "publication": view.publication,
            "selection": view.selection.to_string(),
        }));
    }
    let canon = st.materialized(&Scope::Canon).unwrap();
    let mut links = BTreeMap::new();
    let mut parts = BTreeMap::new();
    for b in canon.iter() {
        if b.block_type == rdy_schema::repo_link::REPO_LINK {
            links.insert(
                b.id.to_string(),
                json!({
                    "alias": b.field_str("alias"),
                    "repo": b.field_str("repo"),
                    "profile": b.field_str("profile"),
                }),
            );
        }
        if !b.foreign.is_empty() {
            parts.insert(b.id.to_string(), json!(b.foreign));
        }
    }
    let mut adopts = vec![];
    reader
        .for_each_object_of(kinds::ANSWER, |h| {
            let a: Answer = reader.get_typed(kinds::ANSWER, &h)?;
            if a.act == AnswerAct::Adopt {
                adopts.push(json!({
                    "hash": h.to_string(),
                    "item": a.item,
                    "targets": a.targets.iter().map(ToString::to_string).collect::<Vec<_>>(),
                }));
            }
            Ok(())
        })
        .unwrap();
    let mut removed = vec![];
    reader
        .for_each_object_of(kinds::BLOCK, |h| {
            if matches!(
                reader.get_kind_raw(kinds::BLOCK, &h),
                Err(StoreError::Redacted { .. })
            ) {
                removed.push(h.to_string());
            }
            Ok(())
        })
        .unwrap();
    json!({
        "entries": entries,
        "sources": sources,
        "links": links,
        "parts": parts,
        "adopt_answers": adopts,
        "removed_blocks": removed,
    })
}

/// The names of each foreign event kind in `facts["entries"]`, sorted: what the fixture must
/// hold (deliveries, a revocation and a removal).
pub fn event_kinds(facts: &Value) -> Vec<String> {
    let mut out: Vec<String> = facts["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            e["entry"]["event"]
                .as_object()
                .and_then(|m| m.keys().next().cloned())
                .unwrap_or_default()
        })
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------------------------
// the members: the native store files, the dump and the delivery

/// The relative path of every file under `dir`, sorted, with `/` separators.
pub fn files_under(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if std::fs::symlink_metadata(&p).unwrap().is_dir() {
                walk(&p, base, out);
            } else {
                out.push(
                    p.strip_prefix(base)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    let mut out = vec![];
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// The store files under `dir`: every file but `expect.json` and the derived index.
pub fn store_files(dir: &Path) -> Vec<String> {
    files_under(dir)
        .into_iter()
        .filter(|f| f != "expect.json" && !f.starts_with("index.redb"))
        .collect()
}

/// The length and blake3 of the file `p`.
pub fn digest_of(p: &Path) -> (u64, String) {
    let bytes = std::fs::read(p).unwrap();
    (bytes.len() as u64, Hash::of(&bytes).to_string())
}

/// The store files under `dir` with their lengths and blake3, in path order.
pub fn inventory(dir: &Path) -> Value {
    Value::Array(
        store_files(dir)
            .iter()
            .map(|f| {
                let (len, blake3) = digest_of(&dir.join(f));
                json!({"path": f, "len": len, "blake3": blake3})
            })
            .collect(),
    )
}

/// The inventory text of the store files under `dir`: one line `<path> <len> <blake3>` each.
pub fn inventory_text(dir: &Path) -> String {
    store_files(dir)
        .iter()
        .map(|f| {
            let (len, blake3) = digest_of(&dir.join(f));
            format!("{f} {len} {blake3}\n")
        })
        .collect()
}

/// A new `rdy-standard` project whose store is a copy of the store files in `from`, with no
/// index. The checked-in files are never opened.
pub fn project_copy(from: &Path) -> (tempfile::TempDir, PathBuf) {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    Rdy::init(&root, "rdy-standard").unwrap().close().unwrap();
    let store = root.join(".rdy/store");
    std::fs::remove_dir_all(&store).unwrap();
    std::fs::create_dir_all(store.join("segments")).unwrap();
    for f in store_files(from) {
        std::fs::copy(from.join(&f), store.join(&f)).unwrap();
    }
    assert!(!store.join("index.redb").exists(), "no index before open");
    (base, root)
}

fn loc(l: Location) -> Value {
    json!([l.segment, l.offset])
}

/// What the dump file `p` holds, by its envelope and a whole read: its formats, the rdy that
/// wrote it, its log end and chain facts, its trailer, its length, its batch, frame and dropped
/// frame counts and its project file names.
pub fn dump_file_facts(p: &Path) -> Value {
    use rdy_store::dump::{DumpItem, DumpReader, read_envelope};
    let mut f = File::open(p).unwrap();
    let (format, header, trailer, len) = read_envelope(&mut f).unwrap();
    let (mut rd, _, _) = DumpReader::open(std::io::BufReader::new(File::open(p).unwrap())).unwrap();
    let (mut batches, mut frames, mut dropped, mut files) = (0u64, 0u64, 0u64, vec![]);
    while let Some(item) = rd.next().unwrap() {
        match item {
            DumpItem::File { path, .. } => files.push(path),
            DumpItem::Batch(b) => {
                batches += 1;
                frames += b.frames.len() as u64;
                dropped += b
                    .frames
                    .iter()
                    .filter(|f| f.present_payload().is_none())
                    .count() as u64;
            }
        }
    }
    let store = match &header.store {
        rdy_store::format::DumpStore::Legacy(n) => json!(n),
        rdy_store::format::DumpStore::Features(m) => json!({ "features": m }),
    };
    json!({
        "dump_format": format,
        "store_format": store,
        "rdy": header.rdy,
        "log_end": loc(header.log_end),
        "chain": header.chain,
        "trailer": {
            "batches": trailer.batches,
            "frames": trailer.frames,
            "files": trailer.files,
            "removals": trailer.removals,
            "first_removed": trailer.first_removed.map(loc),
            "digest": trailer.digest.to_string(),
        },
        "len": len,
        "batches": batches,
        "frames": frames,
        "dropped": dropped,
        "files": files,
    })
}

/// A load of the dump `p` into the new project `to`: the summary's counts, then the loaded
/// store's facts and its foreign copy, after a reindex. fsck must pass.
pub fn dump_load_facts(p: &Path, to: &Path) -> Value {
    let s = rdy_engine::load::load(p, to).unwrap_or_else(|e| panic!("load: {e:?}"));
    let mut r = Rdy::open(to).unwrap();
    r.reindex().unwrap();
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let v = json!({
        "summary": {
            "dump_format": s.dump_format,
            "batches": s.batches,
            "frames": s.frames,
            "landings": s.landings,
            "tags": s.tags,
            "views": s.views,
        },
        "store": store_facts(&r),
        "foreign": foreign_facts(&r),
    });
    r.close().unwrap();
    v
}

/// What the link delivery `bytes` states: its frames as the released sync expect files state
/// them (`stream`: the header, each object record and the trailer's totals), its delivery check
/// (`delivery`: `check_delivery`'s report), its selection identity (D-C94DV) recomputed from the
/// delivered versions and shells, and the same-delivery digest of D-MEMMQ step 5.
pub fn delivery_facts(bytes: &[u8]) -> Value {
    use rdy_store::sync_stream::{StreamPurpose, StreamReader, StreamRecord, check_delivery};
    let report = check_delivery(bytes).unwrap_or_else(|e| panic!("check_delivery: {e}"));
    let (mut rd, header) = StreamReader::open_for(bytes, StreamPurpose::LinkDelivery).unwrap();
    let mut records = vec![];
    while let Some(r) = rd.next().unwrap() {
        records.push(match &r {
            StreamRecord::Present { key, payload, .. } => json!({
                "kind": key.kind,
                "hash": key.hash.to_string(),
                "raw_kind": r.raw_kind(),
                "form": "present",
                "len": payload.len(),
            }),
            StreamRecord::Dropped {
                key, original_len, ..
            } => json!({
                "kind": key.kind,
                "hash": key.hash.to_string(),
                "raw_kind": r.raw_kind(),
                "form": "dropped",
                "len": original_len,
            }),
        });
    }
    let totals = rd.finish().unwrap();
    let d = &report.delivery;
    let selection = rdy_model::foreign::delivery_selection(
        d.versions.iter().map(|v| (&v.id, &v.version)),
        &d.shells,
    )
    .unwrap();
    let digest = rdy_engine::foreign_delivery::same_delivery_digest(
        &d.publication.profile,
        None,
        &d.versions,
    );
    json!({
        "stream": {
            "header": header,
            "records": records,
            "totals": totals,
        },
        "delivery": report,
        "selection": selection.to_string(),
        "same_delivery_digest": digest.to_string(),
    })
}

/// A take of the link delivery `bytes` into a new project with one repo link to its repo and
/// profile: what the take reports and the foreign copy after it.
pub fn delivery_take_facts(bytes: &[u8]) -> Value {
    let report = rdy_store::sync_stream::check_delivery(bytes).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&dir.path().join("p"), "rdy-standard").unwrap();
    let w = put(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "repo_link", "id": "$l", "parent": "$d",
             "fields": {"alias": FC_LIVE, "repo": report.header.repo_id.to_string(),
                        "profile": report.delivery.publication.profile}}
        ]),
    );
    landed(&mut r, "seed");
    let stream = file(dir.path(), "delivery.rdysync", bytes);
    let taken = take_delivery(&mut r, &stream, &source(&bid(&w, "$l")), None)
        .unwrap_or_else(|e| panic!("take: {e:?}"));
    let again = take_delivery(&mut r, &stream, &source(&bid(&w, "$l")), None).unwrap();
    let st = r.read();
    let held: Vec<String> = report
        .delivery
        .versions
        .iter()
        .filter(|v| st.reader().has_kind(kinds::BLOCK, &v.version).unwrap())
        .map(|v| v.id.to_string())
        .collect();
    drop(st);
    r.close().unwrap();
    json!({
        "added": taken.added,
        "changed": taken.changed,
        "removed": taken.removed,
        "blocks_taken": taken.blocks_taken,
        "selection": taken.selection.to_string(),
        "unchanged": taken.unchanged,
        "again_unchanged": again.unchanged,
        "held": held,
    })
}
