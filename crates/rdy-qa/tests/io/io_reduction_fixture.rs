//! r5906 (D-3PNRK, D-4JMPH, D-WWAK7): the store feature `plan_delta` (incompatible, minimum rdy
//! [`PLAN_DELTA_REQUIRED_RDY`]) and its frozen fixtures. A PLAN of encoding 1 is a whole version
//! or the difference from the version before it; older PLAN 0 records keep their bytes and ids.
//!
//! The fourteen frozen members, each written once by [`r5906_write_reduction_fixtures_once`] and
//! never rewritten:
//!
//! - the native store `fixtures/store/reduction-1/`: `FEATURES`, `FORMAT`, `SPANMAP`, `lock` and
//!   one segment, byte for byte, and no derived index, with its `expect.json`. Its history is a
//!   load of the released dump `v4-f1-e1-ported-change` (PLAN 0 records and ported CHANGEs),
//!   then an ordinary current CHANGE, a bound proposal (a whole PLAN 1), answer-only versions
//!   (deltas, one of them a replacement), the landing and a seal that removes the later answers
//!   (the approved binding's removal), a new chain after the seal, and a growth chain of
//!   [`GROWTH`] versions that each add one answer. It was compacted before its own map was
//!   frozen.
//! - the dump `fixtures/dump/v4-f1-reduction.rdydump` of the same project and its expect file;
//! - the dump chain `v4-f1-reduction-chain-full`, `-chain-1` and `-chain-2` and its expect file;
//! - the sync stream `fixtures/sync/sync4-reduction.bin` of the canon history at the sealed
//!   landing, and its expect file.
//!
//! `io_feature_fixture` registers the dump and sync members in `FIRST_USE`.
use rdy_engine::Rdy;
use rdy_engine::stream_export;
use rdy_engine::sync_fetch;
use rdy_model::plan::{PlanAct, PlanAnswer, PlanKind, PlanSeal, item_hash};
use rdy_model::plan_encoding::{PlanRecordV1, v0_body};
use rdy_model::{BlockId, Hash, Plan, Provenance, Role};
use rdy_qa::foreign_fixture;
use rdy_store::dedup::{MapOpen, SpanMap};
use rdy_store::format::{FEATURE_PLAN_DELTA, FeatureClass, PLAN_DELTA_REQUIRED_RDY};
use rdy_store::kinds;
use rdy_store::redaction::StoredFrame;
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::FetchTarget;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The blake3 of the inventory text of the native store files, set when they were written. A
/// change here needs a founder decision, and after publication it is never allowed.
const INVENTORY_BLAKE3: &str = "4762c21d5faa49fb0acea01203732c0588cb3058be387453a694bae2ea72e356";
/// The blake3 of the native store's `expect.json`, set when it was written.
const EXPECT_BLAKE3: &str = "4db3dea6b9485bec2e27c32d32fbbbe05a020b678e26aa48237e876d2b31498d";

/// The released dump the world starts from.
pub(crate) const SOURCE: &str = "v4-f1-e1-ported-change.rdydump";
pub(crate) const DUMP: &str = "v4-f1-reduction.rdydump";
pub(crate) const DUMP_EXPECT: &str = "v4-f1-reduction.expect.json";
pub(crate) const CHAIN_FULL: &str = "v4-f1-reduction-chain-full.rdydump";
pub(crate) const CHAIN_1: &str = "v4-f1-reduction-chain-1.rdydump";
pub(crate) const CHAIN_2: &str = "v4-f1-reduction-chain-2.rdydump";
pub(crate) const CHAIN_EXPECT: &str = "v4-f1-reduction-chain.expect.json";
pub(crate) const SYNC: &str = "sync4-reduction.bin";
pub(crate) const SYNC_EXPECT: &str = "sync4-reduction.expect.json";

/// The view of the bound plan, its seal and the chain after it.
const VIEW: &str = "red";
/// The view of the growth chain.
const GROW: &str = "grow";
/// The versions of the growth chain, each with one more answer.
const GROWTH: usize = 100;
/// The length of each growth answer's reason.
const REASON: usize = 256;
/// The answer-only answers the bound plan's chain adds before the landing.
const LATER: usize = 24;
/// The answers of the bound version.
const BOUND: usize = 6;
// test-hygiene: allow port the sync state records this URL; nothing connects to it
const URL: &str = "https://127.0.0.1:9/r/reduction";

fn qa_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fixture_dir() -> PathBuf {
    qa_dir().join("fixtures/store/reduction-1")
}

fn dump_path(name: &str) -> PathBuf {
    qa_dir().join("fixtures/dump").join(name)
}

fn sync_path(name: &str) -> PathBuf {
    qa_dir().join("fixtures/sync").join(name)
}

/// The fourteen members the writer creates, in the order it creates them.
fn members() -> Vec<PathBuf> {
    let d = fixture_dir();
    vec![
        d.join("FEATURES"),
        d.join("FORMAT"),
        d.join("SPANMAP"),
        d.join("lock"),
        d.join("segments/seg-000000.log"),
        d.join("expect.json"),
        dump_path(DUMP),
        dump_path(DUMP_EXPECT),
        dump_path(CHAIN_FULL),
        dump_path(CHAIN_1),
        dump_path(CHAIN_2),
        dump_path(CHAIN_EXPECT),
        sync_path(SYNC),
        sync_path(SYNC_EXPECT),
    ]
}

/// None when no member exists, else the exact refusal of the writer.
fn written_once(paths: &[PathBuf]) -> Result<(), String> {
    match paths.iter().find(|p| p.exists()) {
        Some(p) => Err(format!(
            "reduction fixtures are written once; output exists: {}",
            p.display()
        )),
        None => Ok(()),
    }
}

pub(crate) fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap())
        .unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn pretty(v: &Value) -> Vec<u8> {
    (serde_json::to_string_pretty(v).unwrap() + "\n").into_bytes()
}

/// Writes `bytes` to the new file `p` (create_new); refuses a file that exists.
fn write_new(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .unwrap_or_else(|e| {
            panic!(
                "reduction fixtures are written once; output exists: {}: {e}",
                p.display()
            )
        });
    std::io::Write::write_all(&mut out, bytes).unwrap();
    out.sync_all().unwrap();
}

/// `n` lowercase letters from `seed`.
fn text(n: usize, seed: u64) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            char::from(b'a' + (x % 26) as u8)
        })
        .collect()
}

fn agent() -> Provenance {
    Provenance {
        actor: "agent:plan".into(),
        run: Some("plan".into()),
        role: Role::Writer,
        at_ms: 2,
    }
}

fn person() -> Provenance {
    Provenance {
        actor: "human:qa".into(),
        run: None,
        role: Role::Approver,
        at_ms: 3,
    }
}

/// The answer `n` of the scope `scope`, with act `act` and a reason of `REASON` letters (none
/// for Done).
fn answer(scope: &str, n: usize, act: PlanAct) -> PlanAnswer {
    let cause = BlockId::parse("b-00001").unwrap();
    PlanAnswer {
        item: item_hash(
            &cause,
            &format!("app:src/{scope}{n:03}.rs"),
            PlanKind::Confirm,
        ),
        act,
        reason: (act != PlanAct::Done).then(|| text(REASON, (n as u64) << 8 | scope.len() as u64)),
        subject: Some(Hash([0x61; 32])),
        cause: Hash([0x62; 32]),
        by: agent(),
    }
}

fn sorted(mut a: Vec<PlanAnswer>) -> Vec<PlanAnswer> {
    a.sort_by_key(|x| x.item);
    a
}

/// The plan ref of `view` set to `id`, as the plan writers compose it.
fn ref_item(view: &str, id: Hash) -> (u8, Vec<u8>) {
    (
        kinds::REF_UPDATE,
        postcard::to_allocvec(&kinds::RefUpdate {
            name: rdy_store::plan::plan_ref(view),
            value: id.0.to_vec(),
            at: rdy_store::segment::Location {
                segment: 0,
                offset: 0,
            },
        })
        .unwrap(),
    )
}

/// The world the members hold, with the scratch files the writer copies.
struct World {
    _base: tempfile::TempDir,
    root: PathBuf,
    doc: String,
    proposal: Hash,
    landing: Hash,
    bound: Hash,
    later: Vec<Hash>,
    seal: Hash,
    after_seal: Vec<Hash>,
    growth: Vec<Hash>,
    chain: [PathBuf; 3],
    sync: Vec<u8>,
    sync_report: Value,
    base: Hash,
    fetch: Value,
    receiver_features: Value,
}

fn build_world() -> World {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("p");
    let recv = base.path().join("recv");
    rdy_engine::load::load(&dump_path(SOURCE), &root).unwrap();
    rdy_engine::load::load(&dump_path(SOURCE), &recv).unwrap();
    // the load maps nothing: only this store's own compact builds its map
    if let Ok(map) = SpanMap::load(&root.join(".rdy/store"), MapOpen::ReadOnly) {
        assert_eq!(map.mappings().count(), 0, "a load maps nothing");
    }
    let mut r = Rdy::open(&root).unwrap();
    // an ordinary current CHANGE, proposed with a bound whole PLAN 1
    let w = rdy_qa::fixtures::write(
        &mut r,
        VIEW,
        json!([{"op": "create", "type": "doc", "id": "$d",
                "fields": {"title": "Reduction", "doc_type": "page"}}]),
    )
    .unwrap();
    let doc = w.created["$d"].clone();
    r.invalidate();
    let proposal = r
        .store_mut()
        .propose(VIEW, "Plans the reduction", agent())
        .unwrap();
    let bound_answers = sorted(
        (0..BOUND)
            .map(|n| {
                answer(
                    "red",
                    n,
                    if n % 2 == 0 {
                        PlanAct::Defer
                    } else {
                        PlanAct::Confirm
                    },
                )
            })
            .collect(),
    );
    let bound = Plan {
        stable: None,
        prev: None,
        view: VIEW.into(),
        proposal: Some(proposal),
        answers: bound_answers.clone(),
        heads: vec![],
        provenance: agent(),
        seal: None,
        read_identity: None,
    };
    r.store_mut()
        .write_guarded_for_tests(rdy_qa::every_kind::plan_frames(&bound))
        .unwrap();
    r.invalidate();
    let b = bound.id();
    let full = base.path().join(CHAIN_FULL);
    r.read().dump(&full).unwrap();

    // answer-only versions: new answers, a replacement, then a Done answer
    let mut answers = bound_answers.clone();
    answers.extend((BOUND..BOUND + LATER).map(|n| answer("red", n, PlanAct::Confirm)));
    let mut answers = sorted(answers);
    let mut later = vec![];
    let put = |r: &mut Rdy, prev: Hash, answers: &[PlanAnswer]| {
        r.store_mut()
            .put_plan(&Plan {
                stable: Some(b),
                prev: Some(prev),
                proposal: None,
                answers: answers.to_vec(),
                ..bound.clone()
            })
            .unwrap()
    };
    later.push(put(&mut r, b, &answers));
    let replaced = answer("red", BOUND + 1, PlanAct::Confirm).item;
    for a in answers.iter_mut().filter(|a| a.item == replaced) {
        a.reason = Some(text(REASON, 0xA11));
    }
    later.push(put(&mut r, later[0], &answers));
    answers.push(answer("red", BOUND + LATER, PlanAct::Done));
    let answers = sorted(answers);
    later.push(put(&mut r, later[1], &answers));
    r.invalidate();

    // the landing, and its seal: the approved binding's answers, so the later ones are removed
    let landing = r.store_mut().land(&proposal, person()).unwrap();
    let version = r.store().landing(&landing).unwrap().proposal;
    let seal = Plan {
        stable: Some(b),
        prev: Some(*later.last().unwrap()),
        proposal: Some(version),
        provenance: person(),
        seal: Some(PlanSeal {
            landing,
            version,
            decisions: vec![],
            items: vec![],
        }),
        ..bound.clone()
    };
    let frame = r.store().select_plan_frame(&seal, &[]).unwrap();
    let seal_id = frame.hash();
    r.store_mut()
        .write_guarded_for_tests(vec![frame.into_item(), ref_item(VIEW, seal_id)])
        .unwrap();
    r.invalidate();
    let inc1 = base.path().join(CHAIN_1);
    r.read().dump_after_file(&full, &inc1).unwrap();

    // a new chain after the seal
    let first = Plan {
        stable: None,
        prev: None,
        view: VIEW.into(),
        proposal: None,
        answers: vec![answer("next", 0, PlanAct::Confirm)],
        heads: vec![],
        provenance: agent(),
        seal: None,
        read_identity: None,
    };
    let n1 = r.store_mut().put_plan(&first).unwrap();
    let n2 = r
        .store_mut()
        .put_plan(&Plan {
            stable: Some(n1),
            prev: Some(n1),
            answers: sorted(vec![
                answer("next", 0, PlanAct::Confirm),
                answer("next", 1, PlanAct::Defer),
            ]),
            ..first.clone()
        })
        .unwrap();

    // the growth chain: each version adds one distinct answer
    let mut growth: Vec<Hash> = vec![];
    let mut grown: Vec<PlanAnswer> = vec![];
    for n in 0..GROWTH {
        grown.push(answer("grow", n, PlanAct::Confirm));
        grown.sort_by_key(|a| a.item);
        let id = r
            .store_mut()
            .put_plan(&Plan {
                stable: growth.first().copied(),
                prev: growth.last().copied(),
                view: GROW.into(),
                proposal: None,
                answers: grown.clone(),
                heads: vec![],
                provenance: agent(),
                seal: None,
                read_identity: None,
            })
            .unwrap();
        growth.push(id);
    }
    r.invalidate();
    let inc2 = base.path().join(CHAIN_2);
    r.read().dump_after_file(&inc1, &inc2).unwrap();

    // the canon history at the sealed landing, and its fetch into a matching receiver
    // the receiver holds the released history: the stream omits the closure of its canon
    let mut dst = Rdy::open(&recv).unwrap();
    let canon = dst.store().reader().canon_head().unwrap();
    let mut sync = Vec::new();
    let report = stream_export::write_history(
        &r.read(),
        StreamPurpose::CanonHistory,
        landing,
        canon,
        &BTreeSet::new(),
        &mut sync,
    )
    .unwrap();
    let checked = rdy_engine::stream_check::check_batch(&sync[..], None).unwrap();
    assert_eq!(checked, report);
    let report_json = serde_json::to_value(&report).unwrap();
    let file = base.path().join(SYNC);
    std::fs::write(&file, &sync).unwrap();
    let t = FetchTarget {
        remote: "origin".into(),
        url: URL.into(),
        purpose: StreamPurpose::CanonHistory,
        root: landing,
        view: None,
    };
    let got = sync_fetch::fetch(&mut dst, &[std::fs::File::open(&file).unwrap()], &t).unwrap();
    let plans_taken = [b, seal_id]
        .iter()
        .chain(&later)
        .filter(|h| dst.store().reader().has_kind(kinds::PLAN, h).unwrap())
        .count();
    let fetch = json!({
        "records_taken": got.records_taken,
        "frames_written": got.frames_written,
        "tags_taken": got.tags_taken,
        "tags_kept": got.tags_kept,
        "min_rdy_before": got.min_rdy_before,
        "min_rdy_after": got.min_rdy_after,
        "canon_moved": dst.store().reader().canon_head().unwrap() != canon,
        "plans_taken": plans_taken,
    });
    let receiver_features = serde_json::to_value(dst.store().features()).unwrap();
    dst.close().unwrap();

    let report = r.compact(false).unwrap();
    eprintln!("compact: {report:?}");
    r.close().unwrap();
    World {
        _base: base,
        root,
        doc,
        proposal,
        landing,
        bound: b,
        later,
        seal: seal_id,
        after_seal: vec![n1, n2],
        growth,
        chain: [full, inc1, inc2],
        sync,
        sync_report: report_json,
        base: canon.unwrap(),
        fetch,
        receiver_features,
    }
}

fn world_json(w: &World) -> Value {
    json!({
        "doc": w.doc,
        "proposal": w.proposal.to_string(),
        "landing": w.landing.to_string(),
        "bound": w.bound.to_string(),
        "later": w.later.iter().map(Hash::to_string).collect::<Vec<_>>(),
        "seal": w.seal.to_string(),
        "after_seal": w.after_seal.iter().map(Hash::to_string).collect::<Vec<_>>(),
        "growth_first": w.growth[0].to_string(),
        "growth_last": w.growth[GROWTH - 1].to_string(),
    })
}

/// Each stored PLAN frame of the store at `root` by its id: its raw encoding and, for encoding
/// 1, its form and the lengths of a delta's lists; and its payload length.
pub(crate) fn plan_frames(root: &Path) -> BTreeMap<Hash, (u16, Value, u64)> {
    let r = Rdy::open(root).unwrap();
    let mut out = BTreeMap::new();
    let end = r.store().log_end();
    r.read()
        .reader()
        .for_each_record_from(
            rdy_store::segment::Location {
                segment: 0,
                offset: 0,
            },
            end,
            |_, f, _| {
                if let StoredFrame::Present {
                    kind,
                    hash,
                    payload,
                } = f
                    && kind & !rdy_store::codec::VERSIONED == kinds::PLAN
                {
                    let o = rdy_store::codec::split(kind, payload).unwrap();
                    let form = match o.version {
                        0 => json!("v0"),
                        _ => match rdy_store::codec::decode_bounded_split::<PlanRecordV1>(&o)
                            .unwrap()
                        {
                            PlanRecordV1::Full(p) => json!({"full": p.answers.len()}),
                            PlanRecordV1::Delta(d) => json!({"delta": {
                                "set": d.answers_set.len(),
                                "remove": d.answers_remove.len(),
                            }}),
                        },
                    };
                    out.insert(hash, (o.version, form, payload.len() as u64));
                }
                Ok(())
            },
        )
        .unwrap();
    out
}

/// The new PLAN bytes of the growth chain `ids` in the store at `root`, and the bytes of a
/// version-0 whole copy of each of its logical versions.
pub(crate) fn growth_bytes(root: &Path, ids: &[Hash]) -> Value {
    let frames = plan_frames(root);
    let stored: u64 = ids.iter().map(|h| frames[h].2).sum();
    let r = Rdy::open(root).unwrap();
    let whole: u64 = ids
        .iter()
        .map(|h| v0_body(&r.store().reader().plan(h).unwrap()).len() as u64)
        .sum();
    json!({"versions": ids.len(), "plan_bytes": stored, "v0_full_bytes": whole})
}

/// The growth chain of the store at `root`: its versions, from the last back to the first.
fn growth_chain(root: &Path, last: Hash) -> Vec<Hash> {
    let r = Rdy::open(root).unwrap();
    let mut out = vec![last];
    while let Some(p) = r.store().reader().plan(out.last().unwrap()).unwrap().prev {
        out.push(p);
    }
    out.reverse();
    out
}

/// The mapped target bytes of the map in `store` by class, and its row count.
fn span_facts(store: &Path) -> Value {
    let map = SpanMap::load(store, MapOpen::ReadOnly).unwrap();
    let mut by: BTreeMap<String, u64> = BTreeMap::new();
    for m in map.mappings() {
        let k = m.target.raw_kind & !rdy_store::codec::VERSIONED;
        *by.entry(k.to_string()).or_default() += u64::from(m.target.len);
    }
    json!({
        "rows": map.rows(),
        "through": map.through().map(|l| json!([l.segment, l.offset])),
        "mapped": by,
    })
}

/// The producer commit: the commit the code that wrote the fixture is at.
fn producer_commit() -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "git rev-parse HEAD");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// The open facts of the project at `root`: rebuilt index, full fsck and store facts.
fn opened_facts(root: &Path) -> Value {
    let mut r = Rdy::open(root).unwrap();
    r.reindex().unwrap();
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let facts = foreign_fixture::store_facts(&r);
    r.close().unwrap();
    facts
}

/// The plan frames of the world's ids, by name, as [`plan_frames`] reads them.
fn world_frames(root: &Path, w: &World) -> Value {
    let frames = plan_frames(root);
    let one = |h: &Hash| {
        let (version, form, len) = &frames[h];
        json!({"id": h.to_string(), "encoding": version, "form": form, "len": len})
    };
    let v0 = frames.values().filter(|(v, _, _)| *v == 0).count();
    json!({
        "v0_records": v0,
        "bound": one(&w.bound),
        "later": w.later.iter().map(one).collect::<Vec<_>>(),
        "seal": one(&w.seal),
        "after_seal": w.after_seal.iter().map(one).collect::<Vec<_>>(),
    })
}

/// The facts of a whole load of the dump chain `files` into `to`: the summary's counts and the
/// store's facts after a reindex; fsck must pass.
pub(crate) fn chain_load_facts(files: &[PathBuf], to: &Path) -> Value {
    let s = rdy_engine::load::load_chain(files, to, None).unwrap_or_else(|e| panic!("{e:?}"));
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
        "store": foreign_fixture::store_facts(&r),
    });
    r.close().unwrap();
    v
}

/// Writes the fourteen members once, from one world. Run by hand, never by the gate, after the
/// readers fail: `cargo test -p rdy-qa --test io r5906_write_reduction_fixtures_once --
/// --ignored`. It refuses when any member exists.
#[test]
#[ignore]
fn r5906_write_reduction_fixtures_once() {
    if let Err(e) = written_once(&members()) {
        panic!("{e}");
    }
    let dir = fixture_dir();
    let w = build_world();
    let store = w.root.join(".rdy/store");
    let copied = foreign_fixture::store_files(&store);
    assert_eq!(
        copied,
        [
            "FEATURES",
            "FORMAT",
            "SPANMAP",
            "lock",
            "segments/seg-000000.log"
        ],
        "one segment"
    );
    // the forms the world is about: a whole bound version, deltas, a removal by the seal
    let frames = world_frames(&w.root, &w);
    assert_eq!(frames["bound"]["form"], json!({"full": BOUND}), "{frames}");
    assert!(
        frames["later"][1]["form"]["delta"]["set"] == json!(1),
        "{frames}"
    );
    assert!(
        frames["seal"]["form"]["delta"]["remove"].as_u64() == Some((LATER + 1) as u64),
        "{frames}"
    );
    assert!(frames["v0_records"].as_u64().unwrap() > 0, "{frames}");
    let growth = growth_bytes(&w.root, &w.growth);
    assert!(
        growth["plan_bytes"].as_u64() < growth["v0_full_bytes"].as_u64(),
        "{growth}"
    );
    assert_eq!(
        w.fetch["plans_taken"],
        json!(2 + w.later.len()),
        "{}",
        w.fetch
    );

    for f in &copied {
        write_new(&dir.join(f), &std::fs::read(store.join(f)).unwrap());
    }
    let (_base, root) = foreign_fixture::project_copy(&dir);
    let expect = json!({
        "published": false,
        "producer_commit": producer_commit(),
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": PLAN_DELTA_REQUIRED_RDY,
        "source": SOURCE,
        "world": world_json(&w),
        "plans": frames,
        "growth": growth,
        "inventory": foreign_fixture::inventory(&dir),
        "spans": span_facts(&dir),
        "store": opened_facts(&root),
    });
    let bytes = pretty(&expect);
    write_new(&dir.join("expect.json"), &bytes);
    eprintln!(
        "INVENTORY_BLAKE3 {}",
        Hash::of(foreign_fixture::inventory_text(&dir).as_bytes())
    );
    eprintln!("expect.json blake3 {}", Hash::of(&bytes));

    let scratch = tempfile::tempdir().unwrap();
    // the whole dump
    let dumped = scratch.path().join("p.rdydump");
    let r = Rdy::open(&w.root).unwrap();
    r.read().dump(&dumped).unwrap();
    r.close().unwrap();
    let dump_bytes = std::fs::read(&dumped).unwrap();
    write_new(&dump_path(DUMP), &dump_bytes);
    let v = json!({
        "published": false,
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": PLAN_DELTA_REQUIRED_RDY,
        "formats": rdy_qa::chain_fixture::current_formats(),
        "world": world_json(&w),
        "file": foreign_fixture::dump_file_facts(&dump_path(DUMP)),
        "whole": foreign_fixture::dump_load_facts(&dump_path(DUMP), &scratch.path().join("loaded")),
    });
    let bytes = pretty(&v);
    write_new(&dump_path(DUMP_EXPECT), &bytes);
    eprintln!("{DUMP} blake3 {}", Hash::of(&dump_bytes));
    eprintln!("{DUMP_EXPECT} blake3 {}", Hash::of(&bytes));

    // the dump chain
    let names = [CHAIN_FULL, CHAIN_1, CHAIN_2];
    for (name, p) in names.iter().zip(&w.chain) {
        let bytes = std::fs::read(p).unwrap();
        write_new(&dump_path(name), &bytes);
        eprintln!("{name} blake3 {}", Hash::of(&bytes));
    }
    let files: Vec<PathBuf> = names.iter().map(|n| dump_path(n)).collect();
    let v = json!({
        "published": false,
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": PLAN_DELTA_REQUIRED_RDY,
        "formats": rdy_qa::chain_fixture::current_formats(),
        "files": names.iter().zip(&files)
            .map(|(n, p)| json!({"name": n, "facts": foreign_fixture::dump_file_facts(p)}))
            .collect::<Vec<_>>(),
        "whole": chain_load_facts(&files, &scratch.path().join("chain")),
    });
    let bytes = pretty(&v);
    write_new(&dump_path(CHAIN_EXPECT), &bytes);
    eprintln!("{CHAIN_EXPECT} blake3 {}", Hash::of(&bytes));

    // the sync stream
    write_new(&sync_path(SYNC), &w.sync);
    let v = json!({
        "published": false,
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": PLAN_DELTA_REQUIRED_RDY,
        "purpose": StreamPurpose::CanonHistory,
        "root": w.landing.to_string(),
        "base": w.base.to_string(),
        "stream": w.sync_report,
        "fetch": w.fetch,
        "receiver_features": w.receiver_features,
    });
    let bytes = pretty(&v);
    write_new(&sync_path(SYNC_EXPECT), &bytes);
    eprintln!("{SYNC} blake3 {}", Hash::of(&w.sync));
    eprintln!("{SYNC_EXPECT} blake3 {}", Hash::of(&bytes));
}

/// The length and blake3 of each native fixture file, by relative path.
fn source_digests() -> BTreeMap<String, (u64, String)> {
    foreign_fixture::files_under(&fixture_dir())
        .into_iter()
        .map(|f| {
            let d = foreign_fixture::digest_of(&fixture_dir().join(&f));
            (f, d)
        })
        .collect()
}

/// r5906: the feature is registered as incompatible at 0.59.0 and frozen in `FIRST_USE`; the
/// native store reopens with no upgrade step, reindexes and passes fsck with its recorded facts
/// and plan forms, and keeps its files; an older reader refuses it; the writer refuses to write
/// any member again.
#[test]
fn r5906_reduction_fixture_is_immutable() {
    let reg = rdy_store::format::registered_features()
        .into_iter()
        .find(|f| f.name == FEATURE_PLAN_DELTA)
        .unwrap();
    assert_eq!(reg.class, FeatureClass::Incompatible);
    assert_eq!(reg.required_rdy, PLAN_DELTA_REQUIRED_RDY);
    assert_eq!(PLAN_DELTA_REQUIRED_RDY, "0.59.0");
    let first_use = crate::io_feature_fixture::dump_members();
    for m in [
        DUMP,
        DUMP_EXPECT,
        CHAIN_FULL,
        CHAIN_1,
        CHAIN_2,
        CHAIN_EXPECT,
    ] {
        assert!(first_use.contains(&m), "{m}");
    }

    let dir = fixture_dir();
    let before = source_digests();
    let want = read_json(&dir.join("expect.json"));
    assert_eq!(
        foreign_fixture::files_under(&dir),
        [
            "FEATURES",
            "FORMAT",
            "SPANMAP",
            "expect.json",
            "lock",
            "segments/seg-000000.log"
        ],
        "one segment and no derived index"
    );
    assert_eq!(want["inventory"], foreign_fixture::inventory(&dir));
    assert_eq!(
        Hash::of(foreign_fixture::inventory_text(&dir).as_bytes()).to_string(),
        INVENTORY_BLAKE3
    );
    assert_eq!(
        foreign_fixture::digest_of(&dir.join("expect.json")).1,
        EXPECT_BLAKE3
    );
    let written = std::fs::read_to_string(dir.join("expect.json")).unwrap();
    for bad in ["/Users", "/home/", "/tmp", "/var/", "\\\\", ":\\"] {
        assert!(!written.contains(bad), "the expect file names {bad}");
    }
    assert_eq!(span_facts(&dir), want["spans"]);

    let (_base, root) = foreign_fixture::project_copy(&dir);
    let plan = rdy_store::upgrade::inspect(&root.join(".rdy/store")).unwrap();
    assert!(plan.steps.is_empty(), "no step: {:?}", plan.steps);
    let store = opened_facts(&root);
    for member in want["store"].as_object().unwrap().keys() {
        assert_eq!(store[member], want["store"][member], "{member}");
    }
    assert!(
        want["store"]["manifest"]["features"]
            .as_array()
            .unwrap()
            .contains(&serde_json::to_value(&reg).unwrap()),
        "the manifest holds plan_delta"
    );
    // every plan version reads, and each frame keeps its recorded form
    let frames = plan_frames(&root);
    let world = &want["world"];
    let id = |v: &Value| -> Hash { v.as_str().unwrap().parse().unwrap() };
    let named = &want["plans"];
    for v in std::iter::once(&named["bound"])
        .chain(named["later"].as_array().unwrap())
        .chain(std::iter::once(&named["seal"]))
        .chain(named["after_seal"].as_array().unwrap())
    {
        let (version, form, len) = &frames[&id(&v["id"])];
        assert_eq!(
            (json!(version), form, json!(len)),
            (v["encoding"].clone(), &v["form"], v["len"].clone())
        );
    }
    assert_eq!(
        json!(frames.values().filter(|(v, _, _)| *v == 0).count()),
        named["v0_records"]
    );
    let r = Rdy::open(&root).unwrap();
    for h in frames.keys() {
        r.store().reader().plan(h).unwrap();
    }
    let seal = r.store().reader().plan(&id(&world["seal"])).unwrap();
    let bound = r.store().reader().plan(&id(&world["bound"])).unwrap();
    assert_eq!(
        seal.answers, bound.answers,
        "the seal keeps the approved binding"
    );
    drop(r);

    // an older rdy, which does not know the feature, refuses the store
    {
        let (_b, older) = foreign_fixture::project_copy(&dir);
        let _old = rdy_store::format::older_reader_for_tests(&[FEATURE_PLAN_DELTA]);
        let e = Rdy::open(&older).err().expect("an older rdy refuses it");
        assert_eq!(e.code, "E_STORE_FORMAT", "{e:?}");
        assert!(
            e.message
                .contains("store feature plan_delta needs rdy 0.59.0"),
            "{e:?}"
        );
    }

    // the writer refuses every member, with its exact text
    let refused = written_once(&members()).unwrap_err();
    assert_eq!(
        refused,
        format!(
            "reduction fixtures are written once; output exists: {}",
            members()[0].display()
        )
    );
    assert_eq!(
        source_digests(),
        before,
        "the checked-in files never change"
    );
}

/// r5906: a hundred successive distinct answers, each with a 256-byte reason, write fewer new
/// PLAN bytes than a hundred version-0 whole copies of the same logical versions; both totals
/// are pinned in the native fixture's expect file.
#[test]
fn r5906_new_answer_growth_avoids_full_copies() {
    let want = read_json(&fixture_dir().join("expect.json"));
    let (_base, root) = foreign_fixture::project_copy(&fixture_dir());
    let last: Hash = want["world"]["growth_last"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let ids = growth_chain(&root, last);
    assert_eq!(ids.len(), GROWTH);
    assert_eq!(ids[0].to_string(), want["world"]["growth_first"]);
    let r = Rdy::open(&root).unwrap();
    for (n, h) in ids.iter().enumerate() {
        let p = r.store().reader().plan(h).unwrap();
        assert_eq!(p.answers.len(), n + 1);
        assert!(
            p.answers
                .iter()
                .all(|a| a.reason.as_ref().is_some_and(|t| t.len() == REASON))
        );
    }
    drop(r);
    let got = growth_bytes(&root, &ids);
    assert_eq!(got, want["growth"]);
    assert!(
        got["plan_bytes"].as_u64().unwrap() < got["v0_full_bytes"].as_u64().unwrap(),
        "{got}"
    );
}
