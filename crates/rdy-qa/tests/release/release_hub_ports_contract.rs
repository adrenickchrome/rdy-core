//! hpt5/hpr5 (D-A3H7S, D-RHYNQ, D-PKC71, D-1QT23, D-08M5W, D-3DG3R, D-3JN9N, D-7AT4S): the contract
//! fixture of hub ports, plan C2.
//!
//! `hub-ports-v1.json` is this plan's fixture. It started as a byte copy of the frozen
//! `hub-forks-v1.json`, and every selector names it. One capture wrote its delta: the feature
//! `ported_change` and its first-use rules, the port flags of `rdy view replay` and `rdy_view`,
//! the new refusal templates, the populated probes of a local and a dump port, and the semantic
//! probes of a hub port. The tests here pin that delta leaf by leaf, keep every earlier fixture
//! and every released format and vector as it was, and show that a port keeps its provenance
//! through a reindex, a dump and its load, and a sync.
//!
//! QA depends on the engine and the store, not on the adapter. The hub probe therefore makes the
//! bundle with `port::make_bundle` and takes it with `Rdy::port_into_view`: the calls the adapter
//! wraps.
use rdy_engine::clone::{CloneSource, clone};
use rdy_engine::port::{self, PortBundle};
use rdy_engine::port_hub::PortOutcome;
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, RdyError, Scope, Session, pin_clock};
use rdy_model::encoding::{
    CHANGE_PORT_ENCODING, CHANGE_PORT_FOREIGN_ENCODING, RESIDUE_ENCODING, RESIDUE_PORT_ENCODING,
    change_body,
};
use rdy_model::foreign::{ForeignId, ForeignLink, ForeignParts, ForeignPin};
use rdy_model::redaction::{ChangeResidue, RedactionResidue};
use rdy_model::{
    Block, BlockId, Change, ChangeLinks, FieldValue, Hash, Link, Op, PortedFrom, Provenance,
    RepoId, Role,
};
use rdy_qa::fixtures::{approver, as_agent, land, write, writer};
use rdy_store::codec;
use rdy_store::format::{self, FEATURE_FOREIGN_COPY, FEATURE_PORTED_CHANGE};
use rdy_store::hub_canon::{CanonMove, Moved};
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::{FetchTarget, HistoryTarget};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The predecessor of this plan's fixture, frozen from here on.
const PREDECESSOR: &str = "hub-forks-v1.json";

/// The sha256 of [`PREDECESSOR`], its final bytes. It never moves: a later contract change is a
/// successor fixture, never an edit of that file.
const PREDECESSOR_SHA: &str = "5cff7aee1f5afb3e1e1e0364fc72fb7367a6acf103b1ad64040818cc13cd3252";

/// This plan's fixture, which every selector names.
const CURRENT: &str = "hub-ports-v1.json";

/// The sha256 of [`CURRENT`] after its capture, its final bytes. A later contract change is a
/// successor.
const CURRENT_SHA: &str = "1ca4ac7975b5518125d3ff65dc119f7e3d46e8305fc018a5425752d22f51fcab";

/// The source file of each current-fixture selector, from the workspace root.
const SELECTORS: [&str; 3] = [
    "crates/rdy-qa/tests/release/release_contract.rs",
    "crates/rdy-cli/src/contract.rs",
    "crates/rdy-qa/tests/cli/cli_rules.rs",
];

/// Every frozen contract fixture, with the sha256 of its bytes.
const FROZEN: &[(&str, &str)] = &[
    (
        "release-1.json",
        "635f63e24cb0a743892f52944b9b5182f4f013ca75f5491d9e05c120400a6ef6",
    ),
    (
        "repo-links-v1.json",
        "b975e0cad885386f134d9dbfab08bf7677ccdbbc9bdeb0c927289e9346f4ca27",
    ),
    (
        "mcp-http-v1.json",
        "13a8de668dedbbbd2591a6da4e24d65d7897de8368107ff90e9d89917e26a3de",
    ),
    (
        "m4a-v1.json",
        "49ca6b3f9cab4e97445c1de5df5a513fe97ceee3971d9ae17853307bc9922a71",
    ),
    (
        "m4bc-v1.json",
        "f8a229b41fff5337d52d8b614670a5b0edcbee534598b35c001064fa040d06c6",
    ),
    (
        "readme-from-rdy-v1.json",
        "883debf0ad6ba99e983662a92ddb8c7f044306cea027ca835994b39b5187dc16",
    ),
    (
        "disclosure-review-v1.json",
        "956044b4b5e6a51a9810f31a4c0c16ad81aa9a035d735ed4d8b9d7a1d9edf139",
    ),
    (
        "sd1-v1.json",
        "012ff9ed50f654618517e629e3dcfa6c8e28935bf311c880de2faaa40576aea2",
    ),
    (
        "sync-client-v1.json",
        "a8bc2d43a01a728f14d7926cb7ace8fd82d4312047f0c03f39720018efcae10c",
    ),
    (
        "sync-client-b-v1.json",
        "e65653b123aea1307a9e2ab9728f3b5d18e25b33b8f7ca8aa9fb2ff5a2dcb3e1",
    ),
    (
        "m5b-v1.json",
        "d00807ccc5ed37448eb81fec8addd240633596f73aa288b1dc54d26256d2fb41",
    ),
    (PREDECESSOR, PREDECESSOR_SHA),
];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn contracts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/contracts")
}

fn bytes(name: &str) -> Vec<u8> {
    let path = contracts().join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn read(name: &str) -> Value {
    serde_json::from_slice(&bytes(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// A list of identified entries as an object keyed by identity (`key=`, `name=`, `id=` or the
/// first string of a row), so a change reads as the entry's own path. Other lists stay lists.
fn keyed(v: &Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(m.iter().map(|(k, x)| (k.clone(), keyed(x))).collect()),
        Value::Array(a) => {
            let identity = |e: &Value| -> Option<String> {
                for k in ["key", "name", "id"] {
                    if let Some(s) = e.get(k).and_then(Value::as_str) {
                        return Some(format!("{k}={s}"));
                    }
                }
                e.as_array()?.first()?.as_str().map(|s| format!("row={s}"))
            };
            let ids: Option<Vec<String>> = a.iter().map(identity).collect();
            match ids {
                Some(ids)
                    if !a.is_empty() && ids.iter().collect::<BTreeSet<_>>().len() == ids.len() =>
                {
                    let mut m = Map::new();
                    for (id, e) in ids.into_iter().zip(a) {
                        m.insert(id, keyed(e));
                    }
                    Value::Object(m)
                }
                _ => Value::Array(a.iter().map(keyed).collect()),
            }
        }
        other => other.clone(),
    }
}

/// Every member where `now` differs from `was`, as `added|removed|changed <path>`.
fn leaves(was: &Value, now: &Value, path: &str, out: &mut Vec<String>) {
    match (was, now) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for k in keys {
                let at = format!("{path}/{k}");
                match (a.get(k), b.get(k)) {
                    (None, Some(_)) => out.push(format!("added {at}")),
                    (Some(_), None) => out.push(format!("removed {at}")),
                    (Some(x), Some(y)) => leaves(x, y, &at, out),
                    (None, None) => unreachable!("the key is in one of the objects"),
                }
            }
        }
        _ if was != now => out.push(format!("changed {path}")),
        _ => {}
    }
}

/// Every scalar and array member of `v` by its keyed path.
fn flat(v: &Value, path: &str, out: &mut BTreeMap<String, Value>) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, x) in m {
                flat(x, &format!("{path}/{k}"), out);
            }
        }
        other => {
            out.insert(path.to_string(), other.clone());
        }
    }
}

/// The member of `v` at the keyed `path` (`/a/b=c/d`).
fn at<'a>(v: &'a Value, path: &str) -> &'a Value {
    let mut cur = v;
    for part in path.split('/').filter(|p| !p.is_empty()) {
        cur = cur
            .get(part)
            .unwrap_or_else(|| panic!("no member {part} on the way to {path}"));
    }
    cur
}

/// The members of a JSON object, sorted.
fn keys_of(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v
        .as_object()
        .unwrap_or_else(|| panic!("an object: {v}"))
        .keys()
        .cloned()
        .collect();
    k.sort();
    k
}

/// This plan's fixture in its keyed form.
fn current() -> Value {
    keyed(&read(CURRENT))
}

/// The command `name` of the CLI tree in `fixture`, as the fixture holds it (no keyed form).
fn command<'a>(fixture: &'a Value, name: &str) -> &'a Value {
    fixture["cli"]["tree"]["subcommands"]
        .as_array()
        .expect("commands")
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no command {name}"))
}

/// `leaves` over the keyed forms of `was` and `now`, sorted.
fn delta(was: &Value, now: &Value) -> Vec<String> {
    let mut found = vec![];
    leaves(&keyed(was), &keyed(now), "", &mut found);
    found.sort();
    found
}

/// hpt5: every selector names this plan's fixture, and none names a fixture the selectors named
/// before it.
#[test]
fn hpt5_selectors_name_the_current_fixture_only() {
    for file in SELECTORS {
        let text =
            std::fs::read_to_string(root().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            text.contains(&format!("fixtures/contracts/{CURRENT}\"")),
            "{file} selects {CURRENT}"
        );
        for earlier in [PREDECESSOR, "m5b-v1.json", "sync-client-b-v1.json"] {
            assert!(
                !text.contains(&format!("contracts/{earlier}\"")),
                "{file} no longer selects {earlier}"
            );
        }
    }
}

/// hpt5: the successor keeps every earlier fixture as it was. Each frozen file keeps its bytes,
/// the folder holds those and this plan's fixture and no other, this plan's fixture differs from
/// its predecessor, and it keeps its captured bytes.
#[test]
fn hpt5_predecessors_keep_their_bytes() {
    for (name, want) in FROZEN {
        assert_eq!(&sha(&bytes(name)), want, "{name} is frozen");
    }
    let mut names: Vec<String> = std::fs::read_dir(contracts())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut want: Vec<&str> = FROZEN.iter().map(|(n, _)| *n).chain([CURRENT]).collect();
    want.sort();
    assert_eq!(
        names, want,
        "the folder holds every frozen fixture and {CURRENT}"
    );
    let now = sha(&bytes(CURRENT));
    assert_ne!(now, PREDECESSOR_SHA, "{CURRENT} is a successor, not a copy");
    assert_eq!(now, CURRENT_SHA, "{CURRENT} has its captured bytes");
}

// ---------------------------------------------------------------------------------------------
// the local world: a project, its fork and a port of landed changes between them

/// The clock of every probe landing (injected).
const AT: i64 = 1_000_000;

/// The person every hub carry names.
const PERSON: &str = "human:carrier";

/// The text of the block the source removes after its port.
const SECRET: &str = "The launch code is ZEBRA-7731.";

/// The new blocks of `r`'s write `w`, by the key of its op.
fn made(w: &rdy_engine::WriteResult, key: &str) -> String {
    w.created[key].clone()
}

/// Writes `ops` in `view` of `r` and lands it; returns the landing and what the write created.
fn landed(r: &mut Rdy, view: &str, ops: Value) -> (Hash, rdy_engine::WriteResult) {
    let w = write(r, view, ops).expect("a write");
    let l = land(r, view)
        .expect("a landing")
        .parse()
        .expect("a landing hash");
    (l, w)
}

/// The changes the landing `l` of `r` carries, in landing order.
fn changes_of(r: &Rdy, l: &Hash) -> Vec<Hash> {
    r.store().reader().landing(l).expect("a landing").changes
}

/// The source project S, its fork T, and the landings S makes after the fork. `s1` seeds both.
/// `s2` updates `intro` and creates `fresh`, `s3` deletes `fresh`, `s4` creates `other`. The
/// source is closed and dumped. The target is closed.
struct Local {
    dir: tempfile::TempDir,
    s: PathBuf,
    /// Three forks of the seed: one takes a port from the project, one from its dump, one is
    /// the MCP server's.
    t: PathBuf,
    t2: PathBuf,
    t3: PathBuf,
    dump: PathBuf,
    repo: RepoId,
    /// A change of the source that no landing carries.
    draft: Hash,
    landings: [Hash; 3],
    /// The source changes of `s3` and `s4`.
    ports: [Hash; 2],
    /// The section `a`, the prose `intro` and the secret prose of the seed.
    a: String,
    intro: String,
    secret: String,
}

impl Local {
    fn at(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

fn local() -> Local {
    let dir = tempfile::tempdir().expect("a directory");
    let s_dir = dir.path().join("s");
    let mut s = Rdy::init(&s_dir, "rdy-standard").expect("a project");
    let repo = s.read().repo_id().expect("a repo id");
    let (_, seed) = landed(
        &mut s,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$n", "fields": {"name": "notes", "title": "Notes", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$a", "parent": "$n", "fields": {"title": "Section A"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$a", "body": "Public words."},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$a", "body": SECRET}
        ]),
    );
    let (a, intro, secret) = (made(&seed, "$a"), made(&seed, "$p"), made(&seed, "$x"));
    let t_dir = dir.path().join("t");
    clone(CloneSource::State(&s.read()), &t_dir, true).expect("a fork");
    let t2_dir = dir.path().join("t2");
    clone(CloneSource::State(&s.read()), &t2_dir, true).expect("a fork");
    let t3_dir = dir.path().join("t3");
    clone(CloneSource::State(&s.read()), &t3_dir, true).expect("a fork");
    let (s2, w2) = landed(
        &mut s,
        "w1",
        json!([
            {"op": "update", "id": intro, "body": "Public words, revised."},
            {"op": "create", "type": "prose", "id": "$y", "parent": a, "body": "Fresh words."}
        ]),
    );
    let (s3, _) = landed(
        &mut s,
        "w2",
        json!([{"op": "delete", "id": made(&w2, "$y")}]),
    );
    let (s4, _) = landed(
        &mut s,
        "w3",
        json!([{"op": "create", "type": "prose", "parent": a, "body": "Other words."}]),
    );
    let ports = [changes_of(&s, &s3)[0], changes_of(&s, &s4)[0]];
    let draft: Hash = write(
        &mut s,
        "draft",
        json!([{"op": "create", "type": "prose", "parent": a, "body": "Unlanded words."}]),
    )
    .expect("an open view")
    .change
    .parse()
    .expect("a change hash");
    let dump = dir.path().join("s.rdydump");
    s.read().dump(&dump).expect("a dump");
    s.close().expect("a close");
    Local {
        dir,
        s: s_dir,
        t: t_dir,
        t2: t2_dir,
        t3: t3_dir,
        dump,
        repo,
        draft,
        landings: [s2, s3, s4],
        ports,
        a,
        intro,
        secret,
    }
}

/// A registered briefing of `id` in view `view` of `r`: its token.
fn token(r: &Rdy, view: &str, id: &str) -> String {
    let id = BlockId::parse(id).expect("a block id");
    rdy_qa::edit_loop::brief_of(r, &Scope::View(view.into()), &[&id])
        .base
        .token
}

/// Ports the landings `sel` of the source at `from` into a new view `view` of `t`, as the person
/// of [`writer`], citing a fresh briefing of the section `a`.
fn port_local(
    t: &mut Rdy,
    from: &Path,
    sel: &[Hash],
    view: &str,
    a: &str,
) -> Result<rdy_engine::views::ReplayResult, RdyError> {
    let session: Session = writer();
    if !t.read().has_view(view) {
        t.view_create(&session, view, None, false)
            .expect("a new view");
    }
    let brief = token(t, view, a);
    let texts: Vec<String> = sel.iter().map(ToString::to_string).collect();
    let source = rdy_engine::views::port::read_source(from, &texts)?;
    t.view_port(&session, view, &source, Some(&brief))
}

/// What a landed port of two changes shows: the landing of the target, its ported facts, and
/// the changes' own provenance.
struct Landed {
    landing: Hash,
    /// Whether a replay inside the target of one ported change keeps its source and records
    /// the ported change as its own.
    replay_copies_origin: bool,
    facts: Value,
    in_order: bool,
    empty_kept: bool,
    sources_match: bool,
}

/// Lands the port of `s3` and `s4` from `from` into a new view of the target `t`, and reads
/// the engine's landing facts of it (the reader the adapter wraps).
fn landed_port(t: &mut Rdy, l: &Local, from: &Path) -> Landed {
    let r = port_local(t, from, &[l.landings[1], l.landings[2]], "mine", &l.a).expect("a port");
    assert_eq!(r.changes, 2, "two new changes: {r:?}");
    let landing: Hash = land(t, "mine")
        .expect("the port lands")
        .parse()
        .expect("a landing hash");
    let facts = rdy_engine::history_facts::landing(&t.read(), landing).expect("landing facts");
    let own = changes_of(t, &landing);
    let in_order = facts
        .ported_changes
        .iter()
        .map(|f| f.change)
        .collect::<Vec<_>>()
        == own;
    // the first ported change keeps no op: its delete had no base in the target
    let first = t.store().reader().change(&own[0]).expect("a change");
    let empty_kept = first.ops.is_empty() && facts.ported_changes.len() == 2;
    let sources: Vec<PortedFrom> = facts.ported_changes.iter().map(|f| f.ported_from).collect();
    let sources_match = sources
        == [
            PortedFrom {
                repo_id: l.repo,
                change: l.ports[0],
            },
            PortedFrom {
                repo_id: l.repo,
                change: l.ports[1],
            },
        ];
    // a replay of a ported change inside the target copies its source (D-08M5W)
    t.view_create(&writer(), "again", None, false)
        .expect("a new view");
    let again = t
        .view_replay(&writer(), "again", &[own[1].to_string()])
        .expect("a replay of a ported change");
    let copy: Hash = again.replayed[0].change.parse().expect("a change hash");
    let copy = t
        .store()
        .reader()
        .change(&copy)
        .expect("the replayed change");
    let replay_copies_origin =
        copy.links.ported_from == Some(sources[1]) && copy.links.replayed_from == Some(own[1]);
    let rows: Vec<Value> = facts
        .ported_changes
        .iter()
        .map(|f| {
            json!({
                "change": f.change.to_string(),
                "ported_from": {
                    "repo_id": f.ported_from.repo_id.to_string(),
                    "change": f.ported_from.change.to_string(),
                },
            })
        })
        .collect();
    Landed {
        landing,
        replay_copies_origin,
        facts: json!({"ported_changes": rows}),
        in_order,
        empty_kept,
        sources_match,
    }
}

// ---------------------------------------------------------------------------------------------
// the hub world: main M, its line L (a fork at v1.0.0), and L's client C

/// Takes the canon history of `src` at `root` above `base` into holder `hd` and moves its canon
/// by `mv` (`expected` its tip before). The history carries the removals `src` has made.
fn push(
    hd: &mut Rdy,
    src: &Rdy,
    root: Hash,
    base: Option<Hash>,
    mv: CanonMove,
    expected: Option<Hash>,
) {
    let mut bytes = Vec::new();
    rdy_engine::stream_export::write_history(
        &src.read(),
        StreamPurpose::CanonHistory,
        root,
        base,
        &BTreeSet::new(),
        &mut bytes,
    )
    .expect("a history");
    let path = tempfile::NamedTempFile::new().expect("a file");
    std::fs::write(path.path(), &bytes).expect("a batch file");
    hd.seal_open().expect("a seal");
    hd.store_mut().set_held(false).expect("no held batch");
    hd.store_mut()
        .take_history::<Infallible>(
            &[File::open(path.path()).expect("a batch file")],
            &HistoryTarget {
                purpose: StreamPurpose::CanonHistory,
                root,
            },
        )
        .expect("a take");
    let b = hd
        .store()
        .reader()
        .check_move_batch(vec![], root)
        .expect("a move batch");
    assert_eq!(
        hd.store_mut()
            .move_canon(&b, &mv, expected, AT)
            .expect("a move"),
        Moved::Moved { tip: root }
    );
}

/// A hub store of `src`'s line at `dir`: a holder of its repo whose hub canon is `src`'s canon.
fn seed_hub(src: &Rdy, dir: &Path) -> Rdy {
    let read = src.read();
    let head = read.canon_head().expect("a canon head");
    let mut h = Rdy::create_holder(dir, read.repo_id().expect("a repo id")).expect("a holder");
    push(&mut h, src, head, None, CanonMove::Seed, None);
    h.seal_open().expect("a seal");
    h.close().expect("a close");
    Rdy::open_holder(dir).expect("a hub")
}

/// Moves the hub `h` at `dir` to `from`'s canon head, with `from`'s history; reopened.
fn extend_hub(mut h: Rdy, dir: &Path, from: &Rdy) -> Rdy {
    let head = from.read().canon_head().expect("a canon head");
    let tip = h.read().canon_head();
    push(&mut h, from, head, tip, CanonMove::Extend, tip);
    h.seal_open().expect("a seal");
    h.close().expect("a close");
    Rdy::open_holder(dir).expect("a hub")
}

/// The bytes of the figure both lines land.
const FIGURE: &[u8] = b"\x89PNG\r\n\x1a\na port figure image";

fn figure_op(parent: &str, blob: &Hash) -> Value {
    json!({"op": "create", "type": "figure", "id": "$f", "parent": parent,
        "fields": {"blob": blob.to_string(), "media_type": "image/png", "alt": "A loop"}})
}

fn redact(r: &mut Rdy, view: &str, target: String) -> Hash {
    r.redact_in(
        &writer(),
        view,
        RedactInput {
            targets: vec![target],
            reason_class: "secret".into(),
            reason: "a leaked value".into(),
            dry_run: false,
        },
    )
    .expect("a removal");
    land(r, view)
        .expect("a landing")
        .parse()
        .expect("a landing hash")
}

/// What the hub world holds. `m` is main, `l` its line, `p` the project main takes landings
/// from and `c` the client the line takes landings from.
struct Hub {
    dir: tempfile::TempDir,
    p: Rdy,
    c: Rdy,
    m: Rdy,
    l: Rdy,
    /// The landing of the seed, of the main updates `p2` to `p4`, of the removal of the secret
    /// `p5`, of the line's edit `q2` and of its figure `q3`.
    p1: Hash,
    p2: Hash,
    p3: Hash,
    p4: Hash,
    p5: Hash,
    q2: Hash,
    q3: Hash,
    /// The line's deletion of `intro`: its base is the line's own edit, a version main never saw.
    q4: Hash,
    /// The section, the prose `intro` and the secret of the seed.
    section: String,
    intro: String,
    blob: Hash,
}

impl Hub {
    fn at(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Main takes the landings `p` has made since.
    fn main_follows(mut self) -> Hub {
        let dir = self.at("m");
        self.m = extend_hub(self.m, &dir, &self.p);
        self
    }

    /// The line takes the landings `c` has made since.
    fn line_follows(mut self) -> Hub {
        let dir = self.at("l");
        self.l = extend_hub(self.l, &dir, &self.c);
        self
    }
}

/// Main M holds the seed `p1` (tag v1.0.0), then `p2` (updates `intro` and `spare`, creates `x`,
/// `y` and a figure), `p3` (updates `x`, deletes `y`), `p4` (deletes `spare`) and `p5` (removes
/// the secret). The line L is M's fork at v1.0.0. Its client C is a copy of the seed under L's repo
/// id, and lands `q2` (its own edit of `intro`) and `q3` (a figure of the same bytes as main's).
fn hub_world() -> Hub {
    let _clock = pin_clock(AT);
    let dir = tempfile::tempdir().expect("a directory");
    let p_dir = dir.path().join("p");
    let mut p = Rdy::init(&p_dir, "rdy-standard").expect("a project");
    let (p1, seed) = landed(
        &mut p,
        "w0",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "intro", "title": "Intro", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Terms"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$s", "body": "Public words."},
            {"op": "create", "type": "prose", "id": "$x", "parent": "$s", "body": SECRET},
            {"op": "create", "type": "prose", "id": "$q", "parent": "$s", "body": "Spare words."}
        ]),
    );
    p.tag(&approver(), "v1.0.0", None, None).expect("a tag");
    let (section, intro, secret, spare) = (
        made(&seed, "$s"),
        made(&seed, "$p"),
        made(&seed, "$x"),
        made(&seed, "$q"),
    );
    let m_repo = p.read().repo_id().expect("a repo id");
    p.close().expect("a close");
    let c_dir = dir.path().join("c");
    copy_tree(&p_dir, &c_dir);
    let mut p = Rdy::open(&p_dir).expect("the project");
    let m = seed_hub(&p, &dir.path().join("m"));
    let l = rdy_engine::fork::fork_holder_at(&m.read(), "v1.0.0", &dir.path().join("l"))
        .expect("a fork of main");
    let l_repo = l.read().repo_id().expect("a repo id");
    // C: L's client, the seed under L's repo id
    let mut c = Rdy::open(&c_dir).expect("the client");
    c.store_mut()
        .put_origin(
            &rdy_store::meta::Origin {
                fork: true,
                repo_id: m_repo,
                tag: Some("v1.0.0".into()),
                landing: Some(p1),
            },
            Some(l_repo),
        )
        .expect("an origin");
    let blob = p.store_mut().put_blob(FIGURE).expect("a blob");
    let (p2, _) = landed(
        &mut p,
        "w1",
        json!([
            {"op": "update", "id": intro, "body": "Public words, revised by main."},
            {"op": "update", "id": spare, "body": "Spare words, revised by main."},
            {"op": "create", "type": "prose", "id": "$x", "parent": section, "body": "Extra words."},
            {"op": "create", "type": "prose", "id": "$y", "parent": section, "body": "More words."},
            figure_op(&section, &blob)
        ]),
    );
    let w2 = p.store().reader().landing(&p2).expect("a landing");
    assert_eq!(w2.changes.len(), 1);
    // x and y of p2, by the creates of its one change
    let creates: Vec<String> = p
        .store()
        .reader()
        .change(&w2.changes[0])
        .expect("a change")
        .ops
        .iter()
        .filter_map(|o| match o {
            Op::Create { block } if block.block_type == "prose" => Some(block.id.to_string()),
            _ => None,
        })
        .collect();
    let (x, y) = (creates[0].clone(), creates[1].clone());
    let (p3, _) = landed(
        &mut p,
        "w2",
        json!([
            {"op": "update", "id": x, "body": "Extra words, again."},
            {"op": "delete", "id": y}
        ]),
    );
    let (p4, _) = landed(&mut p, "w3", json!([{"op": "delete", "id": spare}]));
    let p5 = redact(&mut p, "w4", secret);
    let hub = Hub {
        dir,
        p,
        c,
        m,
        l,
        p1,
        p2,
        p3,
        p4,
        p5,
        q2: p1,
        q3: p1,
        q4: p1,
        section,
        intro,
        blob,
    }
    .main_follows();
    // the line's own work
    let mut hub = hub;
    let (q2, _) = landed(
        &mut hub.c,
        "q",
        json!([{"op": "update", "id": hub.intro, "body": "Public words, edited by the line."}]),
    );
    let blob_c = hub.c.store_mut().put_blob(FIGURE).expect("a blob");
    assert_eq!(blob_c, hub.blob, "one figure, one blob hash");
    let (q3, _) = landed(&mut hub.c, "q3", json!([figure_op(&hub.section, &blob_c)]));
    let (q4, _) = landed(&mut hub.c, "q4", json!([{"op": "delete", "id": hub.intro}]));
    hub.q2 = q2;
    hub.q3 = q3;
    hub.q4 = q4;
    hub.line_follows()
}

/// Copies the directory tree `from` to `to`.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("a directory");
    for e in std::fs::read_dir(from).expect("a directory") {
        let p = e.expect("an entry").path();
        let dest = to.join(p.file_name().expect("a name"));
        if p.is_dir() {
            copy_tree(&p, &dest);
        } else {
            std::fs::copy(&p, &dest).expect("a copy");
        }
    }
}

/// The bundle of `landings` of the holder `h`.
fn make(h: &Rdy, landings: &[Hash]) -> Result<PortBundle, RdyError> {
    port::make_bundle(&h.read(), landings)
}

/// Takes `b` into the holder `h` as the view `view`, by `author`.
fn take(h: &mut Rdy, b: &PortBundle, view: &str, author: &str) -> Result<PortOutcome, RdyError> {
    h.port_into_view(b, view, author, AT)
}

fn created(out: PortOutcome) -> rdy_engine::port_hub::PortedView {
    match out {
        PortOutcome::Created(v) => v,
        other => panic!("not created: {other:?}"),
    }
}

/// The code, message and fix of a refusal, with each hash named by a word.
fn refusal(e: &RdyError, hashes: &[Hash]) -> Value {
    let mut message = e.message.clone();
    for h in hashes {
        message = message.replace(&h.to_string(), "<hash>");
    }
    json!({"code": e.code, "message": message, "fix": e.fix})
}

/// The view `view` of the holder `h`: its place, its changes with their sources and op counts,
/// and the blocks its merge conflicts on.
fn view_facts(h: &Rdy, view: &str) -> Value {
    let st = h.read();
    let head = st.view_head(view).expect("the view");
    let reader = st.reader();
    let mut ops = vec![];
    let mut cur = Some(head.head);
    while let Some(o) = cur {
        let op = reader.view_op(&o).expect("a view op");
        cur = op.prev;
        ops.push((o, op));
    }
    ops.reverse();
    let changes: Vec<Value> = ops
        .iter()
        .filter_map(|(_, op)| match &op.op {
            rdy_store::ViewOpKind::Commit { change } => Some(*change),
            _ => None,
        })
        .map(|c| {
            let ch = reader.change(&c).expect("a change");
            json!({
                "id": c.to_string(),
                "ops": ch.ops.len(),
                "ported_from": ch.links.ported_from.map(|f| json!({
                    "repo_id": f.repo_id.to_string(),
                    "change": f.change.to_string(),
                })),
            })
        })
        .collect();
    let mut conflicted: Vec<String> = st
        .conflicts(&Scope::View(view.into()))
        .expect("conflicts")
        .iter()
        .map(|c| c.block.to_string())
        .collect();
    conflicted.sort();
    conflicted.dedup();
    json!({
        "root": ops[0].0.to_string(),
        "head": head.head.to_string(),
        "base": head.base.map(|b| b.to_string()),
        "len": head.len,
        "owner": head.owner.is_some(),
        "pinned": head.pinned,
        "closed": head.closed,
        "changes": changes,
        "conflicted": conflicted,
    })
}

/// The refs of `h` but the carried views and the feature list: what a carry must leave as it was.
fn refs_but(h: &Rdy) -> Vec<(String, Vec<u8>)> {
    h.store()
        .reader()
        .refs_with_prefix("")
        .expect("refs")
        .into_iter()
        .filter(|(n, _)| !n.starts_with("view/carry") && n != format::REF_STORE_FEATURES)
        .collect()
}

/// The frames `h` appended since `from`, by logical kind.
fn kinds_since(h: &Rdy, from: Location) -> BTreeMap<u8, usize> {
    let mut out = BTreeMap::new();
    let st = h.store();
    st.reader()
        .for_each_frame_from(from, st.log_end(), |_, raw, _, _, _| {
            *out.entry(codec::logical_kind(raw)).or_default() += 1;
            Ok(())
        })
        .expect("frames");
    out
}

// ---------------------------------------------------------------------------------------------
// the golden vector of the bundle form

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).expect("a block id")
}

fn plain_block(id: &str, body: &str) -> Block {
    Block {
        id: bid(id),
        block_type: "claim".into(),
        parent: Some(bid("d-00001")),
        order: "V".into(),
        fields: BTreeMap::from([("title".to_string(), FieldValue::Str("A claim".into()))]),
        body: body.into(),
        links: BTreeMap::from([(
            "refines".to_string(),
            vec![Link {
                target: bid("c-00009"),
                part: None,
            }],
        )]),
        pins: BTreeMap::from([(bid("c-00009"), Hash([0x31; 32]))]),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn foreign_block(id: &str, body: &str) -> Block {
    let target = ForeignId::new(RepoId([7; 16]), bid("d-00002"));
    Block {
        foreign: ForeignParts {
            links: BTreeMap::from([(
                "depends_on".to_string(),
                vec![ForeignLink {
                    target: target.clone(),
                    part: None,
                }],
            )]),
            pins: BTreeMap::from([(
                target,
                ForeignPin {
                    content: Hash([0x44; 32]),
                    encoding: 0,
                },
            )]),
        },
        ..plain_block(id, body)
    }
}

/// The bundle the golden vector of `rdy-port-bundle-1` encodes: one landing of a create, an
/// update and a delete, plain and foreign blocks in both. Its bytes are pinned in the engine's
/// `hpt2_bundle_golden_vector`; the contract holds their hash.
fn golden_bundle() -> rdy_engine::port_wire::BundleContent {
    use rdy_engine::port_wire::{BundleChange, BundleContent, BundleLanding};
    BundleContent {
        source: RepoId([0x22; 16]),
        fork_origins: vec![RepoId([0x11; 16])],
        landings: vec![BundleLanding {
            landing: Hash([0x10; 32]),
            changes: vec![
                BundleChange {
                    hash: Hash([0x01; 32]),
                    ops: vec![
                        Op::Create {
                            block: plain_block("c-00001", "Plain."),
                        },
                        Op::Create {
                            block: foreign_block("c-00002", "Foreign."),
                        },
                    ],
                    justification: Some(rdy_model::Justification::Editorial {
                        class: rdy_model::EditorialClass::Clarify,
                        reason: "why".into(),
                    }),
                    message: "create".into(),
                },
                BundleChange {
                    hash: Hash([0x02; 32]),
                    ops: vec![
                        Op::Update {
                            base: Hash([0x51; 32]),
                            block: plain_block("c-00001", "Plain, again."),
                        },
                        Op::Update {
                            base: Hash([0x52; 32]),
                            block: foreign_block("c-00002", "Foreign, again."),
                        },
                    ],
                    justification: None,
                    message: "update".into(),
                },
                BundleChange {
                    hash: Hash([0x03; 32]),
                    ops: vec![Op::Delete {
                        base: Hash([0x53; 32]),
                        id: bid("c-00003"),
                    }],
                    justification: None,
                    message: "delete".into(),
                },
            ],
        }],
        blobs: vec![],
        chunks: vec![],
    }
}

/// The hash a golden bundle ends with, as the engine's vector pins it.
const GOLDEN_HASH: &str = "b474e74ce8e5728e2bbab32d9ec0050c5f709f29122b0299c0d7babf04aef68b";

// ---------------------------------------------------------------------------------------------
// the hub probe: bundles from main and from the line, taken into the other

/// A target's whole state: the log's end, every ref, the feature names and the batch count.
type Trace = (Location, Vec<(String, Vec<u8>)>, Vec<String>, u64);

fn trace(h: &Rdy) -> Trace {
    let st = h.store();
    (
        st.log_end(),
        st.reader().refs_with_prefix("").expect("refs"),
        st.features()
            .features
            .iter()
            .map(|f| f.name.clone())
            .collect(),
        st.write_count(),
    )
}

/// What the hub probe saw, member by member of the contract.
pub(super) struct HubProbe {
    /// `dump_load.hub_ports`: ten booleans.
    pub(super) facts: Value,
    /// `store.port_bases`.
    pub(super) bases: Value,
    /// `store.port_removals`.
    pub(super) removals: Value,
    /// `store.port_bundle`.
    pub(super) bundle: Value,
    /// The hub refusals by cause, each `{code, message, fix}`.
    pub(super) refusals: BTreeMap<&'static str, Value>,
    /// The first-fault pairs of the make and the take.
    pub(super) order: Value,
    /// What a carried view keeps through its stages: a view of each, equal.
    pub(super) stages: Vec<(&'static str, Value)>,
    /// The ported changes of a carried view in the client that pulled it.
    pub(super) pulled: Vec<Value>,
    /// What the new changes of a carry record besides their source.
    pub(super) provenance: Value,
}

pub(super) fn hub_probe() -> &'static HubProbe {
    static PROBE: OnceLock<HubProbe> = OnceLock::new();
    PROBE.get_or_init(run_hub)
}

/// `e` is the refusal `want`: its code, message and fix.
fn is(e: &RdyError, want: RdyError) {
    assert_eq!(refusal(e, &[]), refusal(&want, &[]), "{e}");
}

fn run_hub() -> HubProbe {
    let _clock = pin_clock(AT);
    let mut w = hub_world();
    let (p1, p2, p3, p4, q2, q3, q4) = (w.p1, w.p2, w.p3, w.p4, w.q2, w.q3, w.q4);
    let mut refusals: BTreeMap<&'static str, Value> = BTreeMap::new();

    // the bundles of main, made before it takes the removal of the figure
    let b_a = make(&w.m, &[p2]).expect("a bundle of p2");
    let b_b = make(&w.m, &[p3]).expect("a bundle of p3");
    let b_c = make(&w.m, &[p4]).expect("a bundle of p4");
    let content = |b: &PortBundle| b.content().clone();
    let source_ops = |b: &PortBundle| -> Vec<Op> { content(b).landings[0].changes[0].ops.clone() };
    assert_eq!(b_a.facts().landings.len(), 1);
    assert_eq!(b_a.facts().bytes, b_a.to_bytes().len());

    // main takes the removal of the figure's bytes: the same value made again is equal bytes
    // whatever the removals since, so the bundle holds no epoch, no removal set and no time
    redact(&mut w.p, "w5", format!("blob:{}", w.blob));
    let w = w.main_follows();
    let mut w = w;
    let b_b_again = make(&w.m, &[p3]).expect("a bundle of p3 after the removal");
    let no_epoch = b_b_again.to_bytes() == b_b.to_bytes()
        && b_b_again.facts().bundle_hash == b_b.facts().bundle_hash;
    assert!(no_epoch, "a bundle holds no epoch, removal set or expiry");
    let e = make(&w.m, &[p2]).expect_err("the figure's bytes are removed in the source");
    is(&e, port::source_blob_removed());
    refusals.insert("source_blob_removed", refusal(&e, &[]));
    // a decoded copy of the bundle is the value, and the store keeps no copy of it
    let copy = PortBundle::from_bytes(b_a.to_bytes()).expect("a verified copy");
    assert_eq!(copy.to_bytes(), b_a.to_bytes());

    // the line takes a carry that refuses, one that converts, and one that conflicts
    let before_all = trace(&w.l);
    let tip = w.l.read().canon_head().expect("a canon tip");
    let tags_before = w.l.store().reader().tags().expect("tags");
    let refs_before = refs_but(&w.l);
    let e = take(&mut w.l, &b_c, "carry-c", PERSON).expect_err("a delete of a held block");
    is(&e, port::delete_base());
    assert_eq!(trace(&w.l), before_all, "a refusal writes nothing");
    refusals.insert("delete_base", refusal(&e, &[]));

    let before_b = trace(&w.l);
    let vb = created(take(&mut w.l, &b_b, "carry-b", PERSON).expect("a carry of p3"));
    assert_eq!(vb.ported.len(), 1, "the change stays: {vb:?}");
    assert!(vb.conflicts.is_empty(), "{vb:?}");
    let facts_b = view_facts(&w.l, "carry-b");
    let src_b = source_ops(&b_b);
    let new_b =
        w.l.store()
            .reader()
            .change(&vb.ported[0].change)
            .expect("a change");
    // the update of a block the line never held is a create; the delete of one it lacks is gone
    let converted = matches!(src_b[0], Op::Update { .. })
        && matches!(src_b[1], Op::Delete { .. })
        && new_b.ops.len() == 1
        && matches!(new_b.ops[0], Op::Create { .. });
    assert!(converted, "{src_b:?} became {:?}", new_b.ops);
    assert_eq!(trace(&w.l).3, before_b.3 + 1);

    let before_a = trace(&w.l);
    let kinds_from = before_a.0;
    let va = created(take(&mut w.l, &b_a, "carry-a", PERSON).expect("a carry of p2"));
    let after_a = trace(&w.l);
    let src_a = source_ops(&b_a);
    let new_a =
        w.l.store()
            .reader()
            .change(&va.ported[0].change)
            .expect("a change");
    let held = matches!(src_a[0], Op::Update { .. }) && matches!(new_a.ops[0], Op::Update { .. });
    assert!(held, "a held base keeps its update: {:?}", new_a.ops);
    let facts_a = view_facts(&w.l, "carry-a");
    let batch_counts = kinds_since(&w.l, kinds_from);
    let batch_kinds: BTreeSet<u8> = batch_counts.keys().copied().collect();
    let allowed: BTreeSet<u8> = [
        kinds::BLOCK,
        kinds::CHANGE,
        kinds::VIEW_OP,
        kinds::BLOB_MANIFEST,
        kinds::BLOB_CHUNK,
        kinds::REF_UPDATE,
        kinds::BATCH_COMMIT,
        kinds::SNAPSHOT_DELTA,
    ]
    .into_iter()
    .collect();
    let conflict_record = !batch_kinds.is_subset(&allowed);
    assert!(!conflict_record, "a conflict is no record: {batch_kinds:?}");
    // no pinned version rides along: the batch holds the blocks of its ops and no other
    let op_blocks = new_a
        .ops
        .iter()
        .filter(|o| !matches!(o, Op::Delete { .. }))
        .count();
    let pin_closure = batch_counts.get(&kinds::BLOCK).copied().unwrap_or(0) != op_blocks;
    let provenance = json!({
        "intent": if new_a.links.intent.is_none() { Value::Null } else { json!("kept") },
        "alternative": if new_a.links.alternative.is_none() { Value::Null } else { json!("kept") },
        "pin_closure": pin_closure,
        "replayed_from": new_a.links.replayed_from.is_none(),
        "briefing": if new_a.links.briefing.is_some() { "target" } else { "none" },
    });

    // the facts of the carry
    let reader = w.l.store().reader();
    let tip_after = w.l.read().canon_head().expect("a canon tip");
    let canon_unchanged = tip_after == tip
        && w.l.store().reader().tags().expect("tags") == tags_before
        && refs_but(&w.l) == refs_before;
    let new_view = va.base == tip
        && va.root.to_string() == facts_a["root"]
        && va.head.to_string() == facts_a["head"]
        && facts_a["owner"] == json!(false)
        && facts_a["pinned"] == json!(false)
        && facts_a["closed"] == json!(false)
        && facts_a["len"] == json!(1);
    let one_batch = after_a.3 == before_a.3 + 1;
    let no_source_records = b_a.facts().landings.iter().all(|l| {
        !reader.has_kind(kinds::LANDING, &l.landing).expect("a kind")
            && l.changes
                .iter()
                .all(|c| !reader.has_kind(kinds::CHANGE, c).expect("a kind"))
    }) && !reader
        .has_kind(kinds::SNAPSHOT_DELTA, &b_a.facts().bundle_hash)
        .expect("a kind");
    let bases_resolve = new_a.ops.iter().all(|o| match o {
        Op::Update { base, .. } | Op::Delete { base, .. } => {
            reader.has_kind(kinds::BLOCK, base).expect("a kind")
        }
        Op::Create { .. } => true,
    }) && new_a
        .deps
        .iter()
        .all(|d| reader.has_kind(kinds::CHANGE, d).expect("a kind"));
    let conflict_survives = va.conflicts.len() == 1
        && va.conflicts[0].to_string() == w.intro
        && facts_a["conflicted"] == json!([w.intro])
        && facts_b["conflicted"] == json!([]);
    let blobs_present = matches!(
        reader.blob_state(&w.blob).expect("a blob state"),
        rdy_store::BlobState::Readable
    );
    drop(reader);

    // the second take of the same view is behind, and writes nothing
    let before = trace(&w.l);
    match take(&mut w.l, &b_a, "carry-a", PERSON).expect("a result") {
        PortOutcome::Behind { head } => assert_eq!(head, Some(va.head)),
        other => panic!("{other:?}"),
    }
    assert_eq!(trace(&w.l), before);

    // a reindex, a dump and its load, and a pull keep the carry as it was
    w.l.reindex().expect("a reindex");
    let reindexed = view_facts(&w.l, "carry-a");
    let file = w.at("l.rdydump");
    w.l.read().dump(&file).expect("a dump");
    let loaded_to = w.at("l-loaded");
    rdy_engine::load::load(&file, &loaded_to).expect("a load");
    let loaded = Rdy::open_holder(&loaded_to).expect("the loaded line");
    let after_load = view_facts(&loaded, "carry-a");
    loaded.close().expect("a close");
    let mut out = Vec::new();
    rdy_engine::stream_export::write_history(
        &w.l.read(),
        StreamPurpose::ViewHistory,
        va.head,
        None,
        &BTreeSet::new(),
        &mut out,
    )
    .expect("a view history");
    let batch = w.at("carry-a.view");
    std::fs::write(&batch, out).expect("a batch file");
    let target = FetchTarget {
        remote: "origin".into(),
        url: "https://127.0.0.1:9/r/demo".into(),
        purpose: StreamPurpose::ViewHistory,
        root: va.head,
        view: Some("carry-a".into()),
    };
    w.c.seal_open().expect("a seal");
    rdy_engine::sync_fetch::fetch(
        &mut w.c,
        &[File::open(&batch).expect("a batch file")],
        &target,
    )
    .expect("a fetch of the carried view");
    let creader = w.c.store().reader();
    let pulled: Vec<Value> = facts_a["changes"]
        .as_array()
        .expect("changes")
        .iter()
        .map(|c| {
            let h: Hash = c["id"].as_str().expect("an id").parse().expect("a hash");
            let ch = creader.change(&h).expect("the pulled change");
            json!({
                "id": c["id"],
                "ops": ch.ops.len(),
                "ported_from": ch.links.ported_from.map(|f| json!({
                    "repo_id": f.repo_id.to_string(),
                    "change": f.change.to_string(),
                })),
            })
        })
        .collect();
    let sync_equal = pulled == facts_a["changes"].as_array().expect("changes").clone()
        && w.c
            .store()
            .features()
            .features
            .iter()
            .any(|f| f.name == FEATURE_PORTED_CHANGE);
    drop(creader);

    let facts = json!({
        "canon_unchanged": canon_unchanged,
        "new_view": new_view,
        "one_batch": one_batch,
        "no_source_records": no_source_records,
        "bases_resolve": bases_resolve,
        "conflict_survives": conflict_survives,
        "blobs_present": blobs_present,
        "reindex_equal": reindexed == facts_a,
        "load_equal": after_load == facts_a,
        "sync_equal": sync_equal,
    });

    // the refusals of the take, each by the stage that makes it
    let dir = w.dir.path().to_path_buf();
    let m_repo = w.m.read().repo_id().expect("a repo id");
    let bundle_l = |landing: Hash| make(&w.l, &[landing]).expect("a bundle of the line");
    let b_q2 = bundle_l(q2);
    let mut unrelated =
        Rdy::create_holder(&dir.join("unrelated"), RepoId([9; 16])).expect("a holder");
    let mut empty = Rdy::create_holder(&dir.join("empty"), m_repo).expect("a holder");

    let mut take_pairs: Vec<Value> = vec![];
    // the family before the same store and the author
    let e = take(&mut unrelated, &b_q2, "carry", "agent:a").expect_err("outside the family");
    is(&e, port::outside_family());
    refusals.insert("outside_family", refusal(&e, &[]));
    take_pairs.push(json!({"first": "lineage", "then": "author", "wins": refusal(&e, &[])}));
    // the same store before the author
    let b_main = make(&w.m, &[p3]).expect("a bundle of main");
    let e = take(&mut w.m, &b_main, "carry", "agent:a").expect_err("the same store");
    is(&e, port::same_store());
    refusals.insert("same_store", refusal(&e, &[]));
    take_pairs.push(json!({"first": "same_store", "then": "author", "wins": refusal(&e, &[])}));
    // the author before the view name, the view name before the target
    let e = take(&mut empty, &b_q2, "canon", "agent:a").expect_err("not a person");
    is(&e, port::not_a_person());
    refusals.insert("not_a_person", refusal(&e, &[]));
    take_pairs.push(json!({"first": "author", "then": "view_name", "wins": refusal(&e, &[])}));
    let e = take(&mut empty, &b_q2, "canon", PERSON).expect_err("a reserved view");
    assert_eq!(e.code, rdy_engine::E_RESERVED_VIEW);
    take_pairs.push(json!({"first": "view_name", "then": "canon_tip", "wins": refusal(&e, &[])}));
    let e = take(&mut w.p, &b_q2, "canon", PERSON).expect_err("a reserved view");
    assert_eq!(e.code, rdy_engine::E_RESERVED_VIEW);
    take_pairs.push(json!({"first": "view_name", "then": "holder", "wins": refusal(&e, &[])}));
    // a holder; and a holder with no canon tip
    let e = take(&mut w.p, &b_q2, "carry", PERSON).expect_err("a project target");
    is(&e, port::needs_holder());
    refusals.insert("needs_holder", refusal(&e, &[]));
    let e = take(&mut empty, &b_q2, "carry", PERSON).expect_err("no canon");
    is(&e, port::no_hub_canon());
    refusals.insert("no_hub_canon", refusal(&e, &[]));
    // the compare before the removals, then the removals
    let b_p1 = bundle_l(p1);
    let b_q3 = bundle_l(q3);
    // the missing-base delete of a held block alone; with a removed blob in the same carry
    let b_q4 = bundle_l(q4);
    let before = trace(&w.m);
    let e3 = take(&mut w.m, &b_q4, "rm-delete", PERSON).expect_err("a delete of a held block");
    is(&e3, port::delete_base());
    let both = make(&w.l, &[q3, q4]).expect("a bundle of two landings");
    let e4 = take(&mut w.m, &both, "rm-both", PERSON).expect_err("two faults");
    is(&e4, port::target_removed());
    assert_eq!(trace(&w.m), before);
    take_pairs.push(json!({
        "first": "target_removals", "then": "missing_base_delete", "wins": refusal(&e4, &[])
    }));

    let held_view = created(take(&mut w.m, &b_q2, "taken", PERSON).expect("a carry into main"));
    let before = trace(&w.m);
    match take(&mut w.m, &b_p1, "taken", PERSON).expect("a result") {
        PortOutcome::Behind { head } => {
            assert_eq!(head, Some(held_view.head));
            take_pairs.push(json!({
                "first": "view_head_compare", "then": "target_removals",
                "wins": {"result": "behind"}
            }));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(trace(&w.m), before);
    let e = take(&mut w.m, &b_p1, "rm-version", PERSON).expect_err("a removed version");
    is(&e, port::target_removed());
    refusals.insert("target_removed", refusal(&e, &[]));
    let e2 = take(&mut w.m, &b_q3, "rm-blob", PERSON).expect_err("a removed blob");
    is(&e2, port::target_removed());
    assert_eq!(trace(&w.m), before);
    let removed_blob_and_version = e.code == e2.code && e.message == e2.message;

    // a bundle that does not verify: any changed byte; a hash that is not its own
    let mut bad = b_a.to_bytes().to_vec();
    *bad.last_mut().expect("bytes") ^= 1;
    let e = PortBundle::from_bytes(&bad).expect_err("a changed hash");
    is(&e, port::unverified());
    refusals.insert("unverified", refusal(&e, &[]));

    // the make: the holder, the count, the landing's place
    let e = make(&w.p, &[]).expect_err("a project is no holder");
    is(&e, port::needs_holder());
    let mut make_pairs: Vec<Value> =
        vec![json!({"first": "holder", "then": "empty", "wins": refusal(&e, &[])})];
    let e = make(&w.m, &[]).expect_err("no landing");
    is(&e, port::empty_input());
    refusals.insert("empty", refusal(&e, &[]));
    let many: Vec<Hash> = vec![p1; port::MAX_PORT_ENTRIES + 1];
    let e = make(&w.m, &many).expect_err("too many entries");
    is(&e, port::too_large());
    refusals.insert("too_large", refusal(&e, &[]));
    let mut with_stranger = many.clone();
    with_stranger[0] = q2;
    let e2 = make(&w.m, &with_stranger).expect_err("too many entries");
    assert_eq!(
        refusal(&e2, &[]),
        refusal(&e, &[]),
        "the count before the place"
    );
    make_pairs
        .push(json!({"first": "entry_count", "then": "landing_canon", "wins": refusal(&e2, &[])}));
    let e = make(&w.m, &[p3, q2]).expect_err("a landing of the line");
    is(&e, port::not_on_canon(&q2));
    refusals.insert("not_on_canon", refusal(&e, &[q2]));
    // a landing off canon before the figure the source removed
    let e = make(&w.m, &[p2, q2]).expect_err("a landing of the line, and an erased blob");
    is(&e, port::not_on_canon(&q2));
    make_pairs
        .push(json!({"first": "landing_canon", "then": "blob_erased", "wins": refusal(&e, &[q2])}));
    let e = make(&w.m, &[p2]).expect_err("an erased blob");
    is(&e, port::source_blob_removed());

    // the bases a port keeps and converts
    let bases = json!({
        "held": if held { "keep" } else { "replace" },
        "missing_result": if converted { "create" } else { "keep" },
        "missing_delete_absent": if new_b.ops.len() == 1 { "drop_op_keep_change" } else { "keep_op" },
        "missing_delete_held": "refuse",
        "conflict_record": conflict_record,
    });
    let removals = json!({
        "epoch": !no_epoch,
        "removal_set": !no_epoch,
        "expiry": !no_epoch,
        "target_checks": if removed_blob_and_version { "every_bundled_version_and_blob" } else { "some" },
        "hub_remakes": if no_epoch && !reader_holds_bundle(&w.m, &b_a) { "every_attempt" } else { "kept" },
    });
    // the bundle's own limits and form
    let golden = rdy_engine::port_wire::encode_bundle(&golden_bundle()).expect("a golden bundle");
    let golden_hash = rdy_engine::port_wire::bundle_hash(&golden).expect("a hash");
    assert_eq!(golden_hash.to_string(), GOLDEN_HASH, "the golden vector");
    assert!(golden.starts_with(rdy_engine::port_wire::PORT_BUNDLE_FORM));
    let order_of = b_a
        .facts()
        .landings
        .iter()
        .map(|l| l.landing)
        .collect::<Vec<_>>();
    let in_canon_order = order_of == [p2];
    let bundle = json!({
        "format": String::from_utf8_lossy(rdy_engine::port_wire::PORT_BUNDLE_FORM).trim_end(),
        "max_entries": port::MAX_PORT_ENTRIES,
        "max_changes": port::MAX_PORT_CHANGES,
        "max_bytes": port::MAX_PORT_BUNDLE_BYTES,
        "bytes": "whole_value",
        "order": if in_canon_order { "canon_oldest_first" } else { "other" },
        "duplicates": "make_collapses_take_refuses",
        "hub_replay_limits": false,
        "vector_hash": golden_hash.to_string(),
    });
    for h in [unrelated, empty] {
        h.close().expect("a close");
    }
    HubProbe {
        facts,
        bases,
        removals,
        bundle,
        refusals,
        order: json!({"make": make_pairs, "take": take_pairs}),
        stages: vec![
            ("carried", facts_a.clone()),
            ("reindexed", reindexed),
            ("loaded", after_load),
        ],
        pulled,
        provenance,
    }
}

/// Whether the store of `h` holds the bundle `b`, as a hash of any kind it may have written.
fn reader_holds_bundle(h: &Rdy, b: &PortBundle) -> bool {
    let reader = h.store().reader();
    [
        kinds::CHANGE,
        kinds::BLOCK,
        kinds::LANDING,
        kinds::VIEW_OP,
        kinds::BLOB_MANIFEST,
        kinds::SNAPSHOT_DELTA,
    ]
    .iter()
    .any(|k| reader.has_kind(*k, &b.facts().bundle_hash).expect("a kind"))
}

// ---------------------------------------------------------------------------------------------
// the stages of a carried view

/// hpt5: a hub port survives a reindex, a dump and its load, and a sync. The carried view keeps
/// its place, its changes with their sources and its conflicts, and the client that pulled it
/// holds the same ported changes.
#[test]
fn hpt5_port_survives_reindex_dump_load_and_sync() {
    let probe = hub_probe();
    for (name, value) in probe.facts.as_object().expect("facts") {
        assert_eq!(value, &json!(true), "{name}");
    }
    let (name, first) = &probe.stages[0];
    assert_eq!(*name, "carried");
    for (stage, now) in &probe.stages[1..] {
        assert_eq!(now, first, "the carried view after {stage}");
    }
    assert_eq!(first["changes"].as_array().expect("changes").len(), 1);
    assert_eq!(first["conflicted"].as_array().expect("conflicts").len(), 1);
    for c in first["changes"].as_array().expect("changes") {
        assert!(c["ported_from"]["repo_id"].is_string(), "{c}");
    }
    assert_eq!(&probe.pulled, first["changes"].as_array().expect("changes"));
}

// ---------------------------------------------------------------------------------------------
// the local probe: a port from a project and from its dump, and the refusals of both forms

/// What the local world showed.
pub(super) struct LocalProbe {
    /// `dump_load.port_landing_facts`.
    pub(super) landing_facts: Value,
    /// Whether a replay of a ported change keeps its source.
    pub(super) replay_copies_origin: bool,
    /// The local refusals by cause, each `{code, message, fix}`.
    pub(super) refusals: BTreeMap<&'static str, Value>,
}

pub(super) fn local_probe() -> &'static LocalProbe {
    static PROBE: OnceLock<LocalProbe> = OnceLock::new();
    PROBE.get_or_init(run_local)
}

fn triple(v: &Value) -> Value {
    json!({"code": v["code"], "message": v["message"], "fix": v["fix"]})
}

fn run_local() -> LocalProbe {
    let _clock = pin_clock(AT);
    let l = local();
    // the same two landings, ported from the project and from its dump, land the same facts
    let mut t = Rdy::open(&l.t).expect("the target");
    let from_project = landed_port(&mut t, &l, &l.s);
    t.close().expect("a close");
    let mut t2 = Rdy::open(&l.t2).expect("the target");
    let from_dump = landed_port(&mut t2, &l, &l.dump);
    t2.close().expect("a close");
    let sources = |facts: &Value| -> Vec<Value> {
        facts["ported_changes"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|r| r["ported_from"].clone())
            .collect()
    };
    let both_forms_equal = sources(&from_project.facts) == sources(&from_dump.facts);
    let landing_facts = json!({
        "ported_changes": super::release_contract::shape(&from_project.facts)["ported_changes"],
        "landing_order": from_project.in_order && from_dump.in_order,
        "empty_op_change_kept": from_project.empty_kept && from_dump.empty_kept,
        "sources_match": from_project.sources_match && from_dump.sources_match,
        "project_and_dump_equal": both_forms_equal,
    });

    // the refusals of the dispatch, through a server on a third fork
    let mut refusals: BTreeMap<&'static str, Value> = BTreeMap::new();
    let mut c = rdy_qa::mcp::McpClient::start_isolated(&l.t3, rdy_qa::fixtures::empty_home());
    let opened = c.call(
        "rdy_session",
        json!({"actor": "agent:port", "run": "port", "role": "writer"}),
    );
    assert!(opened.is_ok(), "the session opens");
    let made = c.call("rdy_view", json!({"op": "create", "name": "mine"}));
    assert!(made.is_ok(), "a view of the agent's own");
    let sel = json!([l.landings[0].to_string()]);
    let dump = l.dump.to_string_lossy().into_owned();
    let project = l.s.to_string_lossy().into_owned();
    let mut refused = |key: &'static str, args: Value| {
        match c.call("rdy_view", args) {
            rdy_qa::mcp::Reply::ToolError(v) => {
                refusals.insert(key, triple(&v));
            }
            other => panic!("{key} is a refusal: {other:?}"),
        };
    };
    refused(
        "two_sources",
        json!({"op": "replay", "view": "mine", "from": project, "from_file": dump, "changes": sel}),
    );
    refused(
        "brief_without_source",
        json!({"op": "replay", "view": "mine", "brief": "tok", "changes": sel}),
    );
    refused(
        "source_field_elsewhere",
        json!({"op": "include", "view": "mine", "from_file": dump, "changes": sel}),
    );
    refused(
        "brief_required",
        json!({"op": "replay", "view": "mine", "from_file": dump, "changes": sel}),
    );
    refused(
        "brief_unknown",
        json!({"op": "replay", "view": "mine", "from_file": dump, "brief": "0".repeat(64), "changes": sel}),
    );
    refused(
        "not_landed",
        json!({"op": "replay", "view": "mine", "from": project, "changes": [l.draft.to_string()]}),
    );
    drop(refused);
    // the hash and the project path are named by a word
    if let Some(v) = refusals.get_mut("not_landed") {
        let text = v["message"].as_str().expect("a message").to_string();
        let text = text
            .replace(&l.draft.to_string(), "<hash>")
            .replace(&project, "<project>");
        v["message"] = json!(text);
    }
    drop(c);
    // a write through the source handle
    let mut source = Rdy::open_source_read_only(&l.s).expect("a read-only source");
    let e = write(
        &mut source,
        "x",
        json!([{"op": "create", "type": "prose", "parent": l.a, "body": "x"}]),
    )
    .expect_err("a read-only source");
    refusals.insert("source_write", refusal(&e, &[]));
    LocalProbe {
        landing_facts,
        replay_copies_origin: from_project.replay_copies_origin && from_dump.replay_copies_origin,
        refusals,
    }
}

/// hpt5: a port from a project and a port from its dump land the same ported facts, in landing
/// order, with the change that kept no op. The facts name each source change and its repo.
#[test]
fn hpt5_local_and_dump_ports_land_the_same_facts() {
    let probe = local_probe();
    let f = &probe.landing_facts;
    assert_eq!(
        keys_of(f),
        [
            "empty_op_change_kept",
            "landing_order",
            "ported_changes",
            "project_and_dump_equal",
            "sources_match"
        ]
    );
    for k in [
        "empty_op_change_kept",
        "landing_order",
        "project_and_dump_equal",
        "sources_match",
    ] {
        assert_eq!(f[k], json!(true), "{k}");
    }
    assert_eq!(
        f["ported_changes"],
        json!([{"change": "string", "ported_from": {"change": "string", "repo_id": "string"}}])
    );
}

// ---------------------------------------------------------------------------------------------
// the stored form: provenance, residues and the first-use rules

/// The source every vector port names.
fn vector_source() -> PortedFrom {
    PortedFrom {
        repo_id: RepoId([9; 16]),
        change: Hash::of(b"source change"),
    }
}

/// A fixed change: one create of a claim, with a foreign part when `foreign`, and the vector
/// source in its links when `port`.
fn vector_change(foreign: bool, port: bool) -> Change {
    let target = ForeignId::new(RepoId([7; 16]), bid("D-4W1ZR"));
    let mut block = plain_block("c-7KQ2M", "Ported text.");
    if foreign {
        block.foreign = ForeignParts {
            links: BTreeMap::from([(
                "depends_on".to_string(),
                vec![ForeignLink {
                    target: target.clone(),
                    part: None,
                }],
            )]),
            pins: BTreeMap::from([(
                target,
                ForeignPin {
                    content: Hash::of(b"cited content"),
                    encoding: 0,
                },
            )]),
        };
    }
    Change {
        view: "work".into(),
        ops: vec![Op::Create { block }],
        deps: vec![],
        provenance: Provenance {
            actor: "person:ada".into(),
            run: Some("r1".into()),
            role: Role::Writer,
            at_ms: 1_763_856_000_000,
        },
        justification: None,
        message: "carry the claim".into(),
        links: ChangeLinks {
            ported_from: port.then(vector_source),
            ..Default::default()
        },
        read_identity: None,
    }
}

/// The postcard bytes of `Some(vector_source())`: the option tag, the repo id, the change hash.
fn source_tail() -> Vec<u8> {
    let mut t = vec![1u8];
    t.extend([9u8; 16]);
    t.extend(Hash::of(b"source change").0);
    t
}

/// The feature names a first frame of `kind` in `encoding` turns on.
fn first_use(kind: u8, encoding: u16) -> Vec<String> {
    format::required_features(kind | codec::VERSIONED, &encoding.to_le_bytes())
        .into_iter()
        .map(|f| f.name)
        .collect()
}

/// What a port stores, from the model and the codec: `store.port_residue` and the vector part
/// of `store.port_provenance`.
pub(super) struct StoreFacts {
    pub(super) residue: Value,
    pub(super) provenance: Value,
}

pub(super) fn store_facts() -> &'static StoreFacts {
    static FACTS: OnceLock<StoreFacts> = OnceLock::new();
    FACTS.get_or_init(run_store_facts)
}

fn run_store_facts() -> StoreFacts {
    let tail = source_tail();
    // the choice of encoding is by value, and a port's body is the encoding before it plus its source
    let plain = (vector_change(false, false), vector_change(false, true));
    let foreign = (vector_change(true, false), vector_change(true, true));
    let follows = |unported: &Change, port: &Change, from: u16, to: u16| {
        let (a, b) = (
            rdy_model::encoding::change_body(unported, from).expect("an unported body"),
            rdy_model::encoding::change_body(port, to).expect("a port body"),
        );
        b.starts_with(&a) && b[a.len()..] == tail[..]
    };
    assert_eq!(
        (
            plain.0.encoding(),
            foreign.0.encoding(),
            plain.1.encoding(),
            foreign.1.encoding()
        ),
        (1, 2, CHANGE_PORT_ENCODING, CHANGE_PORT_FOREIGN_ENCODING)
    );
    let seventh = follows(&plain.0, &plain.1, 1, 3) && follows(&foreign.0, &foreign.1, 2, 4);
    assert!(
        seventh,
        "the source follows the six links of the encoding before"
    );
    // no object edge: the source change is no dependency of the stored frame
    let edges = |c: &Change| -> Vec<Hash> {
        let (raw, payload) = codec::item_new(kinds::CHANGE, c);
        rdy_store::object_refs::refs_of(&format::StoreFeatures::of(vec![]), raw, &payload)
            .expect("refs")
            .into_iter()
            .map(|o| o.hash)
            .collect()
    };
    let object_edge = edges(&plain.1).contains(&vector_source().change)
        || edges(&foreign.1).contains(&vector_source().change)
        || edges(&plain.1) != edges(&plain.0)
        || edges(&foreign.1) != edges(&foreign.0);
    assert!(!object_edge);
    let provenance = json!({
        "plain_encoding": plain.1.encoding(),
        "foreign_encoding": foreign.1.encoding(),
        "plain_id": plain.1.id().to_string(),
        "foreign_id": foreign.1.id().to_string(),
        "unported_plain_id": plain.0.id().to_string(),
        "unported_foreign_id": foreign.0.id().to_string(),
        "source_after_link_six": seventh,
        "object_edge": object_edge,
    });

    // a port's residue: the encoding-1 body, then the source
    let residue = |c: &Change| RedactionResidue::Change(ChangeResidue::of(c));
    let mut sources = vec![];
    let mut v1_then_ported_from = true;
    let mut json_omits = true;
    for (unported, port) in [&plain, &foreign].map(|p| (&p.0, &p.1)) {
        let (r, old) = (residue(port), residue(unported));
        let RedactionResidue::Change(cr) = &r else {
            unreachable!("a change residue")
        };
        sources.push(cr.source_encoding);
        assert_eq!(r.encoding(), RESIDUE_PORT_ENCODING);
        assert_eq!(old.encoding(), RESIDUE_ENCODING);
        let two = postcard::to_allocvec(
            &rdy_model::encoding::PortResidueV2::of(&r).expect("a port's residue"),
        )
        .expect("a body");
        let mut without = r.clone();
        if let RedactionResidue::Change(c) = &mut without {
            c.links.ported_from = None;
        }
        let one = postcard::to_allocvec(&without).expect("an encoding-1 body");
        v1_then_ported_from &= two.starts_with(&one) && two[one.len()..] == tail[..];
        json_omits &= !serde_json::to_string(&old)
            .expect("json")
            .contains("ported_from")
            && serde_json::to_string(&r)
                .expect("json")
                .contains("ported_from");
    }
    assert!(v1_then_ported_from && json_omits);
    let block = RedactionResidue::Block(rdy_model::redaction::BlockResidue::of(&plain_block(
        "c-7KQ2M", "x",
    )));
    assert_eq!(block.encoding(), RESIDUE_ENCODING);
    // a first frame of residue encoding 2 turns on the port feature and the object encoding, and
    // never the foreign copy, whatever the source change's encoding
    let activation = first_use(kinds::REDACTION_RESIDUE, RESIDUE_PORT_ENCODING);
    let residue_only = activation.contains(&FEATURE_PORTED_CHANGE.to_string())
        && !activation.contains(&FEATURE_FOREIGN_COPY.to_string());
    assert!(residue_only, "{activation:?}");
    StoreFacts {
        residue: json!({
            "encoding": RESIDUE_PORT_ENCODING,
            "body": if v1_then_ported_from { "v1_then_ported_from" } else { "other" },
            "old_json_omits": json_omits,
            "unported_encoding": block.encoding(),
            "source_encodings": sources,
            "residue_only_activation": residue_only,
        }),
        provenance,
    }
}

// ---------------------------------------------------------------------------------------------
// what the contract holds of the probes

/// The world of the command-line and tool probes: a project, its dump and the fork that takes
/// the port, with the landing and the change it ports.
pub(super) struct PortWorld {
    _dir: tempfile::TempDir,
    pub(super) project: PathBuf,
    pub(super) dump: PathBuf,
    pub(super) target: PathBuf,
    /// The landing `s2` of the source and its one change.
    pub(super) landing: Hash,
    pub(super) change: Hash,
    /// The section of the target the port's briefing covers.
    pub(super) section: String,
}

pub(super) fn port_world() -> PortWorld {
    let _clock = pin_clock(AT);
    let l = local();
    let change = {
        let r = Rdy::open_source_read_only(&l.s).expect("the source");
        let c = changes_of(&r, &l.landings[0])[0];
        r.close().expect("a close");
        c
    };
    PortWorld {
        project: l.s.clone(),
        dump: l.dump.clone(),
        target: l.t.clone(),
        landing: l.landings[0],
        change,
        section: l.a.clone(),
        _dir: l.dir,
    }
}

/// `errors_warnings.port_check_order`: the stages of a make and of a take in the order they run,
/// and for pairs of faults the result the earlier stage gives (D-3DG3R, D-7AT4S).
pub(super) fn check_order() -> Value {
    json!({
        "make": [
            "holder", "read_epoch", "empty_input", "entry_count", "parse", "landing_canon",
            "change_count", "blob_erased", "byte_count", "read_epoch_again",
        ],
        "take": [
            "length", "form_and_hash", "lineage", "same_store", "author", "view_name", "holder",
            "canon_tip", "view_head_compare", "target_removals", "foreign_pin",
            "missing_base_delete", "briefing", "parent", "parent_loop",
        ],
        "first_fault_wins": hub_probe().order.clone(),
    })
}

/// `errors_warnings.port_refusals`: each refusal of a port by cause, exactly code, message and fix.
pub(super) fn refusals() -> Value {
    let mut m = Map::new();
    for (k, v) in &hub_probe().refusals {
        m.insert((*k).to_string(), v.clone());
    }
    for (k, v) in &local_probe().refusals {
        m.insert((*k).to_string(), v.clone());
    }
    Value::Object(m)
}

// ---------------------------------------------------------------------------------------------
// the first-use fixtures of `ported_change` (A2): a native store and a dump of a project that
// landed a port and then removed one ported block, so it holds a change of encoding 3 and a
// residue of encoding 2

/// The blake3 of the inventory text of the native store files, set when they were written. A
/// change here needs a founder decision, and after publication it is never allowed.
const INVENTORY_BLAKE3: &str = "9b06adf207226bdd45728e69247c0d4240b6122ad18492b825f54a3ad526c611";

const NATIVE: &str = "ported-change-1";
const DUMP: &str = "v4-f1-e1-ported-change.rdydump";
const DUMP_EXPECT: &str = "v4-f1-e1-ported-change.expect.json";

fn qa_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn fixture_dir() -> PathBuf {
    qa_dir().join("fixtures/store").join(NATIVE)
}

fn pretty(v: &Value) -> Vec<u8> {
    (serde_json::to_string_pretty(v).expect("json") + "\n").into_bytes()
}

/// What the fixture world holds, to be recorded: the port's landing, each ported change with
/// its source, the removal's landing and the removed block.
struct Fixture {
    landing: Hash,
    ported: Vec<(Hash, PortedFrom)>,
    removal: Hash,
    removed: String,
}

/// The target of [`local`] after it ports the landings `s3` and `s4` and removes the block the
/// second change created. The project is closed.
fn build_fixture(l: &Local) -> Fixture {
    let mut t = Rdy::open(&l.t).expect("the target");
    port_local(&mut t, &l.s, &[l.landings[1], l.landings[2]], "mine", &l.a).expect("a port");
    let landing: Hash = land(&mut t, "mine")
        .expect("the port lands")
        .parse()
        .expect("a landing hash");
    let own = changes_of(&t, &landing);
    let ported: Vec<(Hash, PortedFrom)> = own
        .iter()
        .map(|c| {
            let ch = t.store().reader().change(c).expect("a change");
            (*c, ch.links.ported_from.expect("a port"))
        })
        .collect();
    let removed = match &t.store().reader().change(&own[1]).expect("a change").ops[0] {
        Op::Create { block } => block.id.to_string(),
        other => panic!("a create: {other:?}"),
    };
    let removal = redact(&mut t, "rm", removed.clone());
    t.close().expect("a close");
    Fixture {
        landing,
        ported,
        removal,
        removed,
    }
}

fn world_json(f: &Fixture) -> Value {
    json!({
        "landing": f.landing.to_string(),
        "ported": f.ported.iter().map(|(c, from)| json!({
            "change": c.to_string(),
            "source_repo": from.repo_id.to_string(),
            "source_change": from.change.to_string(),
        })).collect::<Vec<_>>(),
        "removal": f.removal.to_string(),
        "removed_block": f.removed,
    })
}

/// The producer commit: the commit the code that wrote the fixture is at.
fn producer_commit() -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git");
    assert!(out.status.success(), "git rev-parse HEAD");
    String::from_utf8(out.stdout)
        .expect("text")
        .trim()
        .to_string()
}

/// The open facts of the project at `root`: rebuilt index, full fsck, store facts.
fn opened_facts(root: &Path) -> Value {
    let mut r = Rdy::open(root).expect("the project");
    assert_eq!(r.store().access(), format::StoreAccess::ReadWrite);
    r.reindex().expect("a reindex");
    let report = r.fsck().unwrap_or_else(|e| panic!("fsck failed: {e:?}"));
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    let facts = rdy_qa::foreign_fixture::store_facts(&r);
    r.close().expect("a close");
    facts
}

/// Writes `bytes` to the new file `p`; refuses a file that exists.
fn write_new(p: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(p.parent().expect("a parent")).expect("a directory");
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(p)
        .unwrap_or_else(|e| panic!("{}: written once: {e}", p.display()));
    std::io::Write::write_all(&mut out, bytes).expect("a write");
    out.sync_all().expect("a sync");
}

/// Writes the native store, its expect file, the dump and its expect file once, from one world.
/// Run by hand, never by the gate: `cargo test -p rdy-qa --release --test release
/// hpt5_write_ported_fixtures_once -- --ignored`. It refuses a member that exists.
#[test]
#[ignore]
fn hpt5_write_ported_fixtures_once() {
    let _clock = pin_clock(AT);
    let dir = fixture_dir();
    let dump = qa_dir().join("fixtures/dump").join(DUMP);
    let dump_expect = qa_dir().join("fixtures/dump").join(DUMP_EXPECT);
    assert!(!dir.exists(), "{NATIVE} is written once");
    for p in [&dump, &dump_expect] {
        assert!(!p.exists(), "{}: written once", p.display());
    }
    let l = local();
    let w = build_fixture(&l);
    let store = l.t.join(".rdy/store");
    let copied = rdy_qa::foreign_fixture::store_files(&store);
    assert_eq!(
        copied,
        ["FEATURES", "FORMAT", "lock", "segments/seg-000000.log"],
        "one segment"
    );
    for f in &copied {
        write_new(&dir.join(f), &std::fs::read(store.join(f)).expect("a file"));
    }
    let (_base, root) = rdy_qa::foreign_fixture::project_copy(&dir);
    let expect = json!({
        "published": false,
        "producer_commit": producer_commit(),
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": format::PORTED_CHANGE_REQUIRED_RDY,
        "world": world_json(&w),
        "inventory": rdy_qa::foreign_fixture::inventory(&dir),
        "store": opened_facts(&root),
    });
    let bytes = pretty(&expect);
    write_new(&dir.join("expect.json"), &bytes);
    eprintln!(
        "INVENTORY_BLAKE3 {}",
        Hash::of(rdy_qa::foreign_fixture::inventory_text(&dir).as_bytes())
    );
    eprintln!("expect.json blake3 {}", Hash::of(&bytes));

    let scratch = tempfile::tempdir().expect("a directory");
    let dumped = scratch.path().join("p.rdydump");
    let r = Rdy::open(&l.t).expect("the target");
    r.read().dump(&dumped).expect("a dump");
    r.close().expect("a close");
    let dump_bytes = std::fs::read(&dumped).expect("a dump file");
    write_new(&dump, &dump_bytes);
    let v = json!({
        "published": false,
        "rdy": env!("CARGO_PKG_VERSION"),
        "required_rdy": format::PORTED_CHANGE_REQUIRED_RDY,
        "formats": rdy_qa::chain_fixture::current_formats(),
        "world": world_json(&w),
        "file": rdy_qa::foreign_fixture::dump_file_facts(&dump),
        "whole": rdy_qa::foreign_fixture::dump_load_facts(&dump, &scratch.path().join("loaded")),
    });
    let bytes = pretty(&v);
    write_new(&dump_expect, &bytes);
    eprintln!("{DUMP} blake3 {}", Hash::of(&dump_bytes));
    eprintln!("{DUMP_EXPECT} blake3 {}", Hash::of(&bytes));
}

// ---------------------------------------------------------------------------------------------
// the delta, leaf by leaf

/// The refusal texts of this plan: the template constants it added, each with its text as the
/// plan words it, and the task that owns it. A constant that the product spells another way, or
/// that is missing, fails `hpt5_owned_leaves_hold_the_planned_values`.
const PORT_TEMPLATES: &[(&str, &str)] = &[
    (
        "BRIEF_AGAIN_FIX",
        "brief these blocks (rdy_brief) and retry with the token",
    ),
    ("PORT_AUTHOR_FIX", "pass the person who asked for the carry"),
    ("PORT_AUTHOR_MESSAGE", "a hub carry needs a person's actor"),
    (
        "PORT_BRIEF_FIX",
        "brief the target blocks and retry with the token",
    ),
    ("PORT_BRIEF_MESSAGE", "a port needs a target briefing"),
    (
        "PORT_BRIEF_NO_SOURCE_FIX",
        "remove brief, or name a port source",
    ),
    (
        "PORT_BRIEF_NO_SOURCE_MESSAGE",
        "replay without a source does not take brief",
    ),
    (
        "PORT_CYCLE_FIX",
        "move one block out of the other in the target, then carry again",
    ),
    (
        "PORT_CYCLE_MESSAGE",
        "the port would make <a> and <b> each other's ancestors",
    ),
    (
        "PORT_DELETE_BASE_FIX",
        "carry the landing that wrote it first",
    ),
    (
        "PORT_DELETE_BASE_MESSAGE",
        "the target line lacks the base of a ported delete",
    ),
    (
        "PORT_EMPTY_FIX",
        "name a landing from this line's hub canon",
    ),
    ("PORT_EMPTY_MESSAGE", "name at least one landing to carry"),
    ("PORT_FAMILY_FIX", "carry from a line of the same family"),
    (
        "PORT_FAMILY_MESSAGE",
        "the port bundle comes from outside this line's family",
    ),
    ("PORT_HOLDER_FIX", "open the line's hub store"),
    ("PORT_HOLDER_MESSAGE", "a hub port needs a holder"),
    (
        "PORT_NOT_CANON_FIX",
        "carry a landing from this line's hub canon",
    ),
    (
        "PORT_NOT_CANON_MESSAGE",
        "landing <hash> is not on this store's hub canon",
    ),
    (
        "PORT_NOT_LANDED_FIX",
        "land it there first, or replay it inside that project",
    ),
    (
        "PORT_NOT_LANDED_MESSAGE",
        "change <hash> has not landed in <project>",
    ),
    (
        "PORT_NO_CANON_FIX",
        "seed the line before carrying a landing",
    ),
    ("PORT_NO_CANON_MESSAGE", "the target line has no hub canon"),
    (
        "PORT_PARENT_FIX",
        "carry the change that creates the parent first, or resolve its conflict",
    ),
    (
        "PORT_PARENT_MESSAGE",
        "the port needs parent <id> present and clean in the target",
    ),
    ("PORT_SAME_STORE_FIX", "replay the change inside this store"),
    (
        "PORT_SAME_STORE_MESSAGE",
        "the port bundle comes from this store",
    ),
    (
        "PORT_SOURCE_BLOB_FIX",
        "port the change after its figure is replaced",
    ),
    (
        "PORT_SOURCE_BLOB_MESSAGE",
        "a source blob of the ported change was removed",
    ),
    (
        "PORT_SOURCE_FIELD_FIX",
        "use the source field only with replay",
    ),
    ("PORT_SOURCE_FIELD_MESSAGE", "<op> does not take <field>"),
    (
        "PORT_SOURCE_WRITE_FIX",
        "open the target project for writes instead",
    ),
    (
        "PORT_SOURCE_WRITE_MESSAGE",
        "this store was opened as a read-only port source",
    ),
    (
        "PORT_TARGET_REMOVED_FIX",
        "carry the change after the line's removal is replaced",
    ),
    (
        "PORT_TARGET_REMOVED_MESSAGE",
        "the port bundle holds a version or blob removed in this line",
    ),
    ("PORT_TOO_LARGE_FIX", "carry fewer landings at a time"),
    ("PORT_TOO_LARGE_MESSAGE", "the port bundle is too large"),
    ("PORT_TWO_SOURCES_FIX", "pass from or from_file, not both"),
    ("PORT_TWO_SOURCES_MESSAGE", "replay takes only one source"),
    (
        "PORT_UNVERIFIED_FIX",
        "make the bundle again in the source line",
    ),
    ("PORT_UNVERIFIED_MESSAGE", "the port bundle does not verify"),
];

/// Every leaf where this plan's fixture differs from `hub-forks-v1.json`, each with the task
/// that owns it and why, but the template leaves ([`PORT_TEMPLATES`]). The keyed form names an
/// entry of a list by its identity. No leaf is removed: the commands and tools that gain a port
/// probe stay listed as not probed for their other forms.
const OWNED_CHANGES: &[(&str, &str)] = &[
    (
        "added /store/features/name=ported_change",
        "task 1: the feature, incompatible, required rdy 0.58.0",
    ),
    (
        "changed /store/registered_names",
        "task 1: the registry lists ported_change",
    ),
    (
        "added /store/feature_rules/ported_change",
        "task 1: the first stored change of encoding 3 or 4, or residue of encoding 2",
    ),
    (
        "added /store/activation/kind 2 encoding 3",
        "task 1: a plain port turns on the feature",
    ),
    (
        "added /store/activation/kind 2 encoding 4",
        "task 1: a port with a foreign part turns on both features",
    ),
    (
        "added /store/activation/kind 21 encoding 2",
        "task 1: the residue of a port turns on the feature alone",
    ),
    (
        "added /store/encoding_versions/ported",
        "task 1: the encodings a value chooses for a port",
    ),
    (
        "added /store/port_residue",
        "task 1 (D-PKC71): the residue of a port, encoding 2",
    ),
    (
        "added /store/port_bundle",
        "task 2 (D-3DG3R): the bundle form, its bounds and its vector",
    ),
    (
        "added /store/port_provenance",
        "tasks 1 and 2 (D-08M5W): what a port stores and records",
    ),
    (
        "added /store/port_bases",
        "task 2 (D-1QT23): held bases, missing bases and conflicts",
    ),
    (
        "added /store/port_removals",
        "task 3 (D-3JN9N): the bundle holds no freshness, the target checks everything",
    ),
    (
        "added /cli/tree/subcommands/name=view/subcommands/name=replay/args/id=from",
        "task 4: rdy view replay takes --from PROJECT_OR_DUMP",
    ),
    (
        "added /cli/tree/subcommands/name=view/subcommands/name=replay/args/id=brief",
        "task 4: rdy view replay takes --brief TOKEN",
    ),
    (
        "changed /cli/tree/subcommands/name=view/subcommands/name=replay/groups/id=Replay/args",
        "task 4: the replay argument group lists from and brief",
    ),
    (
        "added /mcp/tools/rdy_view/input_schema/properties/from_file",
        "task 4: rdy_view replay takes a dump path",
    ),
    (
        "added /mcp/tools/rdy_view/input_schema/properties/brief",
        "task 4: rdy_view replay takes a briefing token",
    ),
    (
        "added /cli/runtime/probes/view replay port",
        "task 5: a successful populated port from a project, through the command line",
    ),
    (
        "added /cli/runtime/probes/view show port",
        "task 5: the view of that port, with the source of each new change",
    ),
    (
        "added /mcp/result_fields/rdy_view replay port",
        "task 5: a successful populated port from a dump, through the tool",
    ),
    (
        "added /mcp/result_fields/rdy_view show port",
        "task 5: the view of that port, with the source of each new change",
    ),
    (
        "added /dump_load/hub_ports",
        "tasks 2 and 3: the semantic probe of a hub carry",
    ),
    (
        "added /dump_load/port_landing_facts",
        "tasks 2 and 4: the landing facts of a landed port",
    ),
    (
        "added /dump_load/fixture_digests/v4-f1-e1-ported-change.rdydump",
        "task 5: the first-use dump of the feature",
    ),
    (
        "added /dump_load/fixture_digests/v4-f1-e1-ported-change.expect.json",
        "task 5: what that dump must load as",
    ),
    (
        "added /errors_warnings/port_check_order",
        "tasks 2 and 3 (D-3DG3R, D-7AT4S): the stages of a make and of a take",
    ),
    (
        "added /errors_warnings/port_refusals",
        "tasks 2 to 4: each refusal of a port by cause",
    ),
];

/// The template leaves, as the delta names them.
fn template_leaves() -> Vec<String> {
    PORT_TEMPLATES
        .iter()
        .map(|(name, _)| format!("added /errors_warnings/templates/{name}"))
        .collect()
}

/// hpt5: this plan's fixture differs from `hub-forks-v1.json` in exactly the owned leaves. Each
/// listed leaf changed and no other, every other leaf keeps its value, no section moved, and
/// nothing is removed.
#[test]
fn hpt5_delta_is_exactly_the_owned_leaves() {
    let (was, now) = (read(PREDECESSOR), read(CURRENT));
    let found = delta(&was, &now);
    let mut want: Vec<String> = OWNED_CHANGES
        .iter()
        .map(|(c, _)| c.to_string())
        .chain(template_leaves())
        .collect();
    let distinct: BTreeSet<&String> = want.iter().collect();
    assert_eq!(distinct.len(), want.len(), "no leaf is listed twice");
    want.sort();
    assert_eq!(found, want, "the delta is the owned leaves and no other");
    for (change, why) in OWNED_CHANGES {
        assert!(why.len() > 10, "{change}: {why}");
        let path = change.split_once(' ').expect("a verb").1;
        assert!(
            path.matches('/').count() >= 2,
            "a leaf below a section: {change}"
        );
        assert!(
            change.starts_with("added ") || change.starts_with("changed "),
            "{change}"
        );
    }
    assert!(
        !found.iter().any(|c| c.starts_with("removed ")),
        "no leaf is removed"
    );
    // every leaf that no owned leaf touches keeps its value
    let (mut a, mut b) = (BTreeMap::new(), BTreeMap::new());
    flat(&keyed(&was), "", &mut a);
    flat(&keyed(&now), "", &mut b);
    let touched: Vec<&str> = found
        .iter()
        .map(|c| c.split_once(' ').expect("a verb").1)
        .collect();
    let mut kept = 0;
    for (leaf, value) in &a {
        if touched
            .iter()
            .any(|t| leaf == t || leaf.starts_with(&format!("{t}/")))
        {
            continue;
        }
        assert_eq!(b.get(leaf), Some(value), "{leaf} keeps its value");
        kept += 1;
    }
    assert!(kept > 1000, "the check saw the whole predecessor: {kept}");
    let (Value::Object(x), Value::Object(y)) = (&was, &now) else {
        panic!("the fixtures are objects");
    };
    assert_eq!(x.keys().collect::<Vec<_>>(), y.keys().collect::<Vec<_>>());
}

/// The members of a port's reply, a replay's, sorted.
const REPLAY_FIELDS: [&str; 11] = [
    "added",
    "added_deps",
    "cascaded",
    "changes",
    "conflicts",
    "consequences",
    "head",
    "on_change",
    "removed",
    "replayed",
    "view",
];

/// The length and blake3 of the file `p`, as the contract writes them.
fn digest_text(p: &Path) -> String {
    let bytes = std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
    format!("{} {}", bytes.len(), Hash::of(&bytes))
}

/// hpt5: the owned leaves hold exactly the values the plan names: the feature and its first-use
/// rules, the encodings a port takes, the flags and the tool properties, the refusal templates in
/// the quoted form the generator keeps and as the product spells them, the first-use fixture's
/// digests, and the populated probes.
#[test]
fn hpt5_owned_leaves_hold_the_planned_values() {
    let fixture = read(CURRENT);
    let was = read(PREDECESSOR);
    let now = keyed(&fixture);
    // the feature: incompatible, minimum 0.58.0, and the registry the contract lists
    assert_eq!(
        at(&now, "/store/features/name=ported_change"),
        &json!({"class": "incompatible", "name": "ported_change", "required_rdy": "0.58.0"})
    );
    assert_eq!(
        format::required_rdy(FEATURE_PORTED_CHANGE),
        format::PORTED_CHANGE_REQUIRED_RDY
    );
    assert_eq!(format::PORTED_CHANGE_REQUIRED_RDY, "0.58.0");
    assert_eq!(
        fixture["store"]["registered_names"],
        json!(format::REGISTERED_FEATURES)
    );
    assert_eq!(
        fixture["store"]["features"]
            .as_array()
            .expect("features")
            .len(),
        was["store"]["features"].as_array().expect("features").len() + 1
    );
    // the first-use rules
    assert_eq!(
        at(&now, "/store/feature_rules/ported_change"),
        &json!("the first stored CHANGE of encoding 3 or 4, or REDACTION_RESIDUE of encoding 2")
    );
    assert_eq!(
        at(&now, "/store/activation/kind 2 encoding 3"),
        &json!(["object_encoding", "ported_change"])
    );
    assert_eq!(
        at(&now, "/store/activation/kind 2 encoding 4"),
        &json!(["object_encoding", "ported_change", "foreign_copy"])
    );
    assert_eq!(
        at(&now, "/store/activation/kind 21 encoding 2"),
        &json!(["object_encoding", "ported_change"]),
        "a residue alone never turns on the foreign copy"
    );
    // the encodings: a port's, by value; every released member as it was
    assert_eq!(
        at(&now, "/store/encoding_versions/ported"),
        &json!({"CHANGE": [3, 4], "REDACTION_RESIDUE": [2]})
    );
    assert_eq!(
        codec::PORT_ENCODINGS,
        [
            (kinds::CHANGE, 3),
            (kinds::CHANGE, 4),
            (kinds::REDACTION_RESIDUE, 2)
        ]
    );
    for member in ["registered", "optional", "latest", "first"] {
        assert_eq!(
            fixture["store"]["encoding_versions"][member],
            was["store"]["encoding_versions"][member],
            "encoding_versions.{member}"
        );
    }
    assert_eq!(fixture["store"]["encoding_versions"]["latest"]["CHANGE"], 1);
    assert_eq!(
        fixture["store"]["encoding_versions"]["latest"]["REDACTION_RESIDUE"],
        1
    );
    // the flags: optional value flags with no default, declared after the view, before the changes
    for (id, value) in [("from", "PROJECT_OR_DUMP"), ("brief", "TOKEN")] {
        assert_eq!(
            at(
                &now,
                &format!("/cli/tree/subcommands/name=view/subcommands/name=replay/args/id={id}")
            ),
            &json!({"action": "set", "id": id, "long": id, "num_args": "1", "value_names": [value]})
        );
    }
    let view = command(&fixture, "view");
    let replay = view["subcommands"]
        .as_array()
        .expect("subcommands")
        .iter()
        .find(|c| c["name"] == "replay")
        .expect("view replay");
    let ids: Vec<&str> = replay["args"]
        .as_array()
        .expect("args")
        .iter()
        .map(|a| a["id"].as_str().expect("an id"))
        .collect();
    assert_eq!(
        ids,
        [
            "view",
            "from",
            "brief",
            "changes",
            "json",
            "project",
            "no_daemon",
            "help"
        ]
    );
    assert_eq!(
        replay["groups"],
        json!([{"args": ["brief", "changes", "from", "view"], "id": "Replay", "multiple": true, "required": false}])
    );
    // the tool: two optional strings; every other property and the required list stay
    let schema = &fixture["mcp"]["tools"]["rdy_view"]["input_schema"];
    let before = &was["mcp"]["tools"]["rdy_view"]["input_schema"];
    assert_eq!(schema["required"], before["required"]);
    assert_eq!(
        schema["additionalProperties"],
        before["additionalProperties"]
    );
    let (mut now_props, mut was_props) = (
        keys_of(&schema["properties"]),
        keys_of(&before["properties"]),
    );
    now_props.retain(|k| k != "brief" && k != "from_file");
    was_props.sort();
    assert_eq!(now_props, was_props, "the two added properties");
    for k in was_props {
        assert_eq!(schema["properties"][&k], before["properties"][&k], "{k}");
    }
    for (name, parts) in [
        ("from_file", &["dump", "replay", "changes"][..]),
        ("brief", &["replay", "briefing", "token"][..]),
    ] {
        let p = &schema["properties"][name];
        assert_eq!(p["type"], json!(["string", "null"]), "{name}");
        let words = p["description"].as_str().expect("a description");
        for part in parts {
            assert!(words.contains(part), "{name}: {part}: {words}");
        }
    }
    // the templates, as the generator keeps them (the quoted Rust literal)
    let templates = &fixture["errors_warnings"]["templates"];
    for (name, text) in PORT_TEMPLATES {
        assert_eq!(
            templates[*name],
            format!("\"{text}\""),
            "{name} as captured"
        );
    }
    // and as the product spells them
    for (live, text) in [
        (
            port::PORT_EMPTY_MESSAGE,
            "name at least one landing to carry",
        ),
        (
            port::PORT_DELETE_BASE_FIX,
            "carry the landing that wrote it first",
        ),
        (
            port::PORT_FAMILY_MESSAGE,
            "the port bundle comes from outside this line's family",
        ),
        (
            port::PORT_SAME_STORE_MESSAGE,
            "the port bundle comes from this store",
        ),
        (
            port::PORT_NOT_CANON_MESSAGE,
            "landing <hash> is not on this store's hub canon",
        ),
        (port::PORT_HOLDER_MESSAGE, "a hub port needs a holder"),
        (
            port::PORT_NO_CANON_MESSAGE,
            "the target line has no hub canon",
        ),
        (
            port::PORT_AUTHOR_MESSAGE,
            "a hub carry needs a person's actor",
        ),
        (
            port::PORT_SOURCE_BLOB_MESSAGE,
            "a source blob of the ported change was removed",
        ),
        (
            port::PORT_TARGET_REMOVED_MESSAGE,
            "the port bundle holds a version or blob removed in this line",
        ),
        (port::PORT_BRIEF_MESSAGE, "a port needs a target briefing"),
        (port::PORT_TOO_LARGE_MESSAGE, "the port bundle is too large"),
        (
            port::PORT_UNVERIFIED_FIX,
            "make the bundle again in the source line",
        ),
        (
            rdy_engine::views::port::PORT_NOT_LANDED_MESSAGE,
            "change <hash> has not landed in <project>",
        ),
        (
            rdy_engine::views::port::PORT_SOURCE_WRITE_FIX,
            "open the target project for writes instead",
        ),
        (
            rdy_engine::view_api::PORT_TWO_SOURCES_MESSAGE,
            "replay takes only one source",
        ),
        (
            rdy_engine::view_api::PORT_BRIEF_NO_SOURCE_MESSAGE,
            "replay without a source does not take brief",
        ),
        (
            rdy_engine::view_api::PORT_SOURCE_FIELD_MESSAGE,
            "<op> does not take <field>",
        ),
    ] {
        assert!(
            PORT_TEMPLATES.iter().any(|(_, t)| *t == text),
            "{text} is a listed template"
        );
        assert_eq!(live, text, "as the product spells it");
    }
    // the first-use dump: its digests are the live files'
    let dumps = qa_dir().join("fixtures/dump");
    for name in [DUMP, DUMP_EXPECT] {
        assert_eq!(
            fixture["dump_load"]["fixture_digests"][name],
            json!(digest_text(&dumps.join(name))),
            "{name}"
        );
    }
    // the probes: every reply is populated, and the two surfaces answer the same
    let cli = &fixture["cli"]["runtime"]["probes"];
    let mcp = &fixture["mcp"]["result_fields"];
    for (c, m) in [
        ("view replay port", "rdy_view replay port"),
        ("view show port", "rdy_view show port"),
    ] {
        assert_eq!(cli[c]["exits"], json!([0]), "{c}");
        assert_eq!(keys_of(&cli[c]), ["exits", "stdout"], "{c}");
        assert_eq!(keys_of(&mcp[m]), ["ok"], "{m}");
        assert_eq!(
            &cli[c]["stdout"], &mcp[m]["ok"],
            "{c}: one reply on both surfaces"
        );
    }
    let reply = &mcp["rdy_view replay port"]["ok"];
    assert_eq!(keys_of(reply), REPLAY_FIELDS);
    assert_eq!(keys_of(&reply["replayed"][0]), ["change", "source"]);
    assert_eq!(reply["changes"], "number");
    let shown = &mcp["rdy_view show port"]["ok"]["changes"][0];
    assert_eq!(keys_of(&shown["ported_from"]), ["change", "repo_id"]);
    assert_eq!(shown["replayed_from"], "null");
    // the semantic probes: every member is a boolean, and every one holds
    assert_eq!(
        keys_of(&fixture["dump_load"]["hub_ports"]),
        [
            "blobs_present",
            "bases_resolve",
            "canon_unchanged",
            "conflict_survives",
            "load_equal",
            "new_view",
            "no_source_records",
            "one_batch",
            "reindex_equal",
            "sync_equal"
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
    );
    for (name, value) in fixture["dump_load"]["hub_ports"]
        .as_object()
        .expect("a probe")
    {
        assert_eq!(value, &json!(true), "{name}");
    }
}

/// The refusals of the plan's table, each by cause: its code, message and fix. The causes the
/// plan's table names and no probe makes (an unheld foreign pin, a source snapshot that predates
/// a removal, an older rdy, and the briefing rows that need stable content) are pinned by the
/// tests of their own tasks.
const REFUSALS: &[(&str, &str, &str, &str)] = &[
    (
        "brief_required",
        "E_BRIEF_REQUIRED",
        "a port needs a target briefing",
        "brief the target blocks and retry with the token",
    ),
    (
        "brief_unknown",
        "E_BRIEF_STALE",
        "the cited briefing 00000000 is unknown: the daemon restarted, or the briefing was evicted",
        "brief these blocks (rdy_brief) and retry with the token",
    ),
    (
        "brief_without_source",
        "E_BAD_INPUT",
        "replay without a source does not take brief",
        "remove brief, or name a port source",
    ),
    (
        "delete_base",
        "E_BAD_INPUT",
        "the target line lacks the base of a ported delete",
        "carry the landing that wrote it first",
    ),
    (
        "empty",
        "E_BAD_INPUT",
        "name at least one landing to carry",
        "name a landing from this line's hub canon",
    ),
    (
        "needs_holder",
        "E_BAD_INPUT",
        "a hub port needs a holder",
        "open the line's hub store",
    ),
    (
        "no_hub_canon",
        "E_BAD_INPUT",
        "the target line has no hub canon",
        "seed the line before carrying a landing",
    ),
    (
        "not_a_person",
        "E_BAD_INPUT",
        "a hub carry needs a person's actor",
        "pass the person who asked for the carry",
    ),
    (
        "not_landed",
        "E_BAD_INPUT",
        "change <hash> has not landed in <project>",
        "land it there first, or replay it inside that project",
    ),
    (
        "not_on_canon",
        "E_BAD_INPUT",
        "landing <hash> is not on this store's hub canon",
        "carry a landing from this line's hub canon",
    ),
    (
        "outside_family",
        "E_BAD_INPUT",
        "the port bundle comes from outside this line's family",
        "carry from a line of the same family",
    ),
    (
        "same_store",
        "E_BAD_INPUT",
        "the port bundle comes from this store",
        "replay the change inside this store",
    ),
    (
        "source_blob_removed",
        "E_REDACTED_STATE",
        "a source blob of the ported change was removed",
        "port the change after its figure is replaced",
    ),
    (
        "source_field_elsewhere",
        "E_BAD_INPUT",
        "include does not take from_file",
        "use the source field only with replay",
    ),
    (
        "source_write",
        "E_STORE_READ_ONLY",
        "this store was opened as a read-only port source",
        "open the target project for writes instead",
    ),
    (
        "target_removed",
        "E_REDACTED_STATE",
        "the port bundle holds a version or blob removed in this line",
        "carry the change after the line's removal is replaced",
    ),
    (
        "too_large",
        "E_BAD_INPUT",
        "the port bundle is too large",
        "carry fewer landings at a time",
    ),
    (
        "two_sources",
        "E_BAD_INPUT",
        "replay takes only one source",
        "pass from or from_file, not both",
    ),
    (
        "unverified",
        "E_BAD_INPUT",
        "the port bundle does not verify",
        "make the bundle again in the source line",
    ),
];

/// hpr5: the five rulings and the preparation order have exact contract leaves. D-1QT23 keeps
/// held bases, makes a missing base a create and stores no conflict; D-PKC71 makes the residue
/// of a port encoding 2, the old body then the source; D-08M5W copies the source to every
/// replay and drops the intent and the alternative; D-3DG3R bounds the whole value, in canon
/// order, with the make and take stages in their order; D-3JN9N keeps no epoch, removal set or
/// expiry, remakes each attempt and checks every bundled version and blob; D-7AT4S puts the
/// foreign pin before the missing-base delete, after the target removals. Each refusal of a port
/// is its exact code, message and fix.
#[test]
fn hpr5_rulings_have_exact_contract_leaves() {
    let f = read(CURRENT);
    let was = read(PREDECESSOR);
    // D-1QT23
    assert_eq!(
        f["store"]["port_bases"],
        json!({
            "held": "keep",
            "missing_result": "create",
            "missing_delete_absent": "drop_op_keep_change",
            "missing_delete_held": "refuse",
            "conflict_record": false,
        })
    );
    // D-PKC71
    assert_eq!(
        f["store"]["port_residue"],
        json!({
            "encoding": 2,
            "body": "v1_then_ported_from",
            "old_json_omits": true,
            "unported_encoding": 1,
            "source_encodings": [3, 4],
            "residue_only_activation": true,
        })
    );
    // D-08M5W
    let p = &f["store"]["port_provenance"];
    assert_eq!(
        keys_of(p),
        [
            "alternative",
            "briefing",
            "foreign_encoding",
            "foreign_id",
            "intent",
            "object_edge",
            "pin_closure",
            "plain_encoding",
            "plain_id",
            "replay_copies_origin",
            "replayed_from",
            "source_after_link_six",
            "unported_foreign_id",
            "unported_plain_id"
        ]
    );
    assert_eq!(
        (
            &p["intent"],
            &p["alternative"],
            &p["pin_closure"],
            &p["replay_copies_origin"]
        ),
        (&Value::Null, &Value::Null, &json!(false), &json!(true))
    );
    assert_eq!(
        (
            &p["plain_encoding"],
            &p["foreign_encoding"],
            &p["source_after_link_six"],
            &p["object_edge"]
        ),
        (&json!(3), &json!(4), &json!(true), &json!(false))
    );
    assert_eq!(p["briefing"], "target");
    assert_eq!(p["replayed_from"], json!(true));
    // the unported ids are the vectors of the frozen encodings: the ported ones differ
    assert_ne!(p["plain_id"], p["unported_plain_id"]);
    assert_ne!(p["foreign_id"], p["unported_foreign_id"]);
    // D-3DG3R
    assert_eq!(
        f["store"]["port_bundle"],
        json!({
            "format": "rdy-port-bundle-1",
            "max_entries": 1000,
            "max_changes": 1000,
            "max_bytes": 67_108_864,
            "bytes": "whole_value",
            "order": "canon_oldest_first",
            "duplicates": "make_collapses_take_refuses",
            "hub_replay_limits": false,
            "vector_hash": GOLDEN_HASH,
        })
    );
    let order = &f["errors_warnings"]["port_check_order"];
    assert_eq!(keys_of(order), ["first_fault_wins", "make", "take"]);
    assert_eq!(
        order["make"],
        json!([
            "holder",
            "read_epoch",
            "empty_input",
            "entry_count",
            "parse",
            "landing_canon",
            "change_count",
            "blob_erased",
            "byte_count",
            "read_epoch_again"
        ])
    );
    assert_eq!(
        order["take"],
        json!([
            "length",
            "form_and_hash",
            "lineage",
            "same_store",
            "author",
            "view_name",
            "holder",
            "canon_tip",
            "view_head_compare",
            "target_removals",
            "foreign_pin",
            "missing_base_delete",
            "briefing",
            "parent",
            "parent_loop"
        ])
    );
    // every pair of faults: the earlier stage wins, and it comes first in the list
    for (side, stages) in [("make", &order["make"]), ("take", &order["take"])] {
        let place = |name: &str| {
            stages
                .as_array()
                .expect("stages")
                .iter()
                .position(|s| s == name)
        };
        let pairs = order["first_fault_wins"][side].as_array().expect("pairs");
        assert!(!pairs.is_empty(), "{side} pairs");
        for pair in pairs {
            let (first, then) = (
                pair["first"].as_str().expect("a stage"),
                pair["then"].as_str().expect("a stage"),
            );
            let (a, b) = (place(first), place(then));
            assert!(
                a.is_some() || first == "view_head_compare",
                "{side}: {first}"
            );
            if let (Some(a), Some(b)) = (a, b) {
                assert!(a <= b, "{side}: {first} runs before {then}");
            }
            let wins = &pair["wins"];
            assert!(
                wins["code"].is_string() && wins["message"].is_string()
                    || wins["result"] == "behind",
                "{pair}"
            );
        }
    }
    // D-3JN9N
    assert_eq!(
        f["store"]["port_removals"],
        json!({
            "epoch": false,
            "removal_set": false,
            "expiry": false,
            "target_checks": "every_bundled_version_and_blob",
            "hub_remakes": "every_attempt",
        })
    );
    // D-7AT4S: after the target removals, the foreign pin, then the missing-base delete, then
    // the briefing
    let take: Vec<&str> = order["take"]
        .as_array()
        .expect("stages")
        .iter()
        .map(|s| s.as_str().expect("a stage"))
        .collect();
    let at_ = |n: &str| take.iter().position(|s| *s == n).expect("a stage");
    assert!(
        at_("target_removals") < at_("foreign_pin")
            && at_("foreign_pin") < at_("missing_base_delete")
            && at_("missing_base_delete") < at_("briefing")
    );
    // the refusals: each the exact triple of the plan's table, and no other
    let refusals = &f["errors_warnings"]["port_refusals"];
    assert_eq!(
        keys_of(refusals),
        REFUSALS.iter().map(|r| r.0.to_string()).collect::<Vec<_>>()
    );
    for (key, code, message, fix) in REFUSALS {
        assert_eq!(
            refusals[*key],
            json!({"code": code, "message": message, "fix": fix}),
            "{key}"
        );
    }
    // behind is a result of the take, not a refusal
    assert!(
        order["first_fault_wins"]["take"]
            .as_array()
            .expect("pairs")
            .iter()
            .any(|p| p["wins"] == json!({"result": "behind"}))
    );
    // no code is new: the codes and rows are the predecessor's
    assert_eq!(
        f["errors_warnings"]["codes"],
        was["errors_warnings"]["codes"]
    );
    assert_eq!(
        f["errors_warnings"]["error_rows"],
        was["errors_warnings"]["error_rows"]
    );
}

// ---------------------------------------------------------------------------------------------
// the formats, the first use and an older rdy

/// The three numbers of a version text.
fn version_of(text: &str) -> (u32, u32, u32) {
    let mut n = text.split('.').map(|p| p.parse::<u32>().expect("a number"));
    (
        n.next().expect("a major"),
        n.next().expect("a minor"),
        n.next().expect("a patch"),
    )
}

/// The minimum rdy a store's manifest asks for: the highest `required_rdy` of its features.
fn minimum_rdy(r: &Rdy) -> (u32, u32, u32) {
    r.store()
        .features()
        .features
        .iter()
        .map(|f| version_of(&f.required_rdy))
        .max()
        .expect("a feature")
}

fn has_feature(r: &Rdy, name: &str) -> bool {
    r.store().features().features.iter().any(|f| f.name == name)
}

/// hpt5: no format moves, and a port is first use. The store stays at format 19, the dump at 4
/// and the streams at 2, 3 and 4; the creation baseline and every released encoding are as they
/// were. A project with no port opens in an older rdy and keeps its minimum; its first port
/// raises the minimum to 0.58.0, and from then on an older rdy refuses it, and the dump of it,
/// with the minimum named. Nothing else turns the feature on.
#[test]
fn hpt5_formats_stay_fixed_and_first_use_is_required() {
    assert_eq!(
        (
            format::FEATURE_STORE_FORMAT,
            format::MANIFEST_VERSION,
            rdy_store::dump::DUMP_FORMAT,
        ),
        (19, 1, 4),
        "the store and dump formats"
    );
    assert_eq!(
        (
            rdy_store::sync_stream::STREAM_FORMAT,
            rdy_store::sync_stream::LINK_DELIVERY_FORMAT,
            rdy_store::sync_stream::HISTORY_FORMAT,
        ),
        (2, 3, 4),
        "the stream formats"
    );
    assert_eq!(rdy_store::sync_stream::READ_FORMATS, [2, 3, 4]);
    assert_eq!(format::REGISTERED_FEATURES.len(), 13);
    let (was, now) = (read(PREDECESSOR), read(CURRENT));
    // a new store starts with the features it did, and not with this one
    for member in [
        "creation_baseline",
        "creation_baseline_text",
        "classes",
        "kind_ids",
        "ref_families",
    ] {
        assert_eq!(was["store"][member], now["store"][member], "store.{member}");
    }
    assert!(
        !format::baseline_features()
            .features
            .iter()
            .any(|f| f.name == FEATURE_PORTED_CHANGE)
    );
    for section in [
        "sync",
        "derived_identities",
        "inventory_version",
        "schema_packs",
        "config",
    ] {
        assert_eq!(was[section], now[section], "{section}");
    }
    assert_eq!(
        was["dump_load"]["released_pairs"],
        now["dump_load"]["released_pairs"]
    );
    for member in [
        "dump_format",
        "magic",
        "sizes",
        "current_frame_kinds",
        "feature_frame_kinds",
    ] {
        assert_eq!(
            was["dump_load"][member], now["dump_load"][member],
            "dump_load.{member}"
        );
    }
    // every vector of the signatures row and every identity probe is the predecessor's
    for member in keys_of(&was["signatures"]) {
        assert_eq!(
            was["signatures"][&member], now["signatures"][&member],
            "{member}"
        );
    }
    // no tool or command is new
    assert_eq!(was["mcp"]["tool_names"], now["mcp"]["tool_names"]);
    let commands = |v: &Value| -> Vec<String> {
        v["cli"]["tree"]["subcommands"]
            .as_array()
            .expect("commands")
            .iter()
            .map(|c| c["name"].as_str().expect("a name").to_string())
            .collect()
    };
    assert_eq!(commands(&was), commands(&now));

    // a bundle, a refused take and a made bundle turn nothing on: the probes check each store's
    // whole state (`hub_probe`), and a project that only reads a port source keeps it as it was
    let probe = hub_probe();
    assert_eq!(probe.facts["canon_unchanged"], json!(true));

    // a project with no port opens in an older rdy and keeps its minimum
    let l = local();
    let fixtures = qa_dir().join("fixtures");
    {
        let _old = format::older_reader_for_tests(&[FEATURE_PORTED_CHANGE]);
        let (_base, root) =
            rdy_qa::foreign_fixture::project_copy(&fixtures.join("store/feature-1"));
        Rdy::open(&root)
            .expect("a store with no port opens in an older rdy")
            .close()
            .expect("a close");
    }
    let before = {
        let r = Rdy::open(&l.t).expect("a project with no port");
        assert!(!has_feature(&r, FEATURE_PORTED_CHANGE));
        let min = minimum_rdy(&r);
        r.close().expect("a close");
        min
    };
    assert!(before < version_of("0.58.0"), "{before:?}");
    // the first port turns the feature on in its batch, and the minimum goes to 0.58.0
    let mut t = Rdy::open(&l.t).expect("the target");
    // a read of the source leaves the target as it was
    let source =
        rdy_engine::views::port::read_source(&l.s, &[l.landings[1].to_string()]).expect("a source");
    assert!(
        !has_feature(&t, FEATURE_PORTED_CHANGE),
        "a read turns nothing on"
    );
    drop(source);
    port_local(&mut t, &l.s, &[l.landings[1]], "mine", &l.a).expect("a first port");
    assert!(has_feature(&t, FEATURE_PORTED_CHANGE));
    assert_eq!(minimum_rdy(&t), version_of("0.58.0"));
    t.close().expect("a close");
    // an older rdy refuses it, naming the feature and its minimum, and the dump of it too
    let need = format!("store feature {FEATURE_PORTED_CHANGE} needs rdy 0.58.0");
    let fix = "open the project with rdy 0.58.0 or later";
    {
        let _old = format::older_reader_for_tests(&[FEATURE_PORTED_CHANGE]);
        let e = Rdy::open(&l.t)
            .err()
            .expect("an older rdy refuses the first port");
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            ("E_STORE_FORMAT", need.as_str(), fix)
        );
        let (_base, root) = rdy_qa::foreign_fixture::project_copy(&fixture_dir());
        let e = Rdy::open(&root)
            .err()
            .expect("an older rdy refuses the frozen store");
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            ("E_STORE_FORMAT", need.as_str(), fix)
        );
        let scratch = tempfile::tempdir().expect("a directory");
        let to = scratch.path().join("loaded");
        let e = rdy_engine::load::load(&fixtures.join("dump").join(DUMP), &to)
            .err()
            .expect("an older rdy refuses the frozen dump");
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            ("E_STORE_FORMAT", need.as_str(), fix)
        );
        assert!(
            !to.join(".rdy/store").exists(),
            "the refused load wrote no store"
        );
    }
    // this rdy opens it
    let r = Rdy::open(&l.t).expect("this rdy opens the port");
    r.close().expect("a close");
}

/// The residue-only first use of a removed port, and the provenance a removal keeps: a project
/// ports one change, lands it and removes the block the change created. The change's frame is
/// gone and a residue of encoding 2 stands for it, which names the source. A dump of that store
/// holds no change of encoding 3 or 4; its load turns `ported_change` on from the residue alone,
/// and its source still reads in the landing facts. No format version moves.
#[test]
fn hpr5_residue_only_first_use_keeps_formats() {
    let _clock = pin_clock(AT);
    let l = local();
    let mut t = Rdy::open(&l.t3).expect("the target");
    port_local(&mut t, &l.s, &[l.landings[2]], "mine", &l.a).expect("a port of one change");
    let landing: Hash = land(&mut t, "mine")
        .expect("the port lands")
        .parse()
        .expect("a landing hash");
    let change = changes_of(&t, &landing)[0];
    let ported = t.store().reader().change(&change).expect("a change");
    let from = ported.links.ported_from.expect("a port");
    assert_eq!(from.change, l.ports[1]);
    let block = match &ported.ops[0] {
        Op::Create { block } => block.id.to_string(),
        other => panic!("a create: {other:?}"),
    };
    let encodings = |r: &Rdy, kind: u8, version: &str| -> u64 {
        rdy_qa::foreign_fixture::store_facts(r)["raw_encodings"][kind.to_string()][version]
            .as_u64()
            .unwrap_or(0)
    };
    assert_eq!(
        encodings(&t, kinds::CHANGE, "3"),
        1,
        "the port's change frame"
    );
    assert_eq!(encodings(&t, kinds::REDACTION_RESIDUE, "2"), 0);
    let removal = redact(&mut t, "rm", block);
    // the change frame is gone; its residue stands for it, in encoding 2; the source reads on
    assert_eq!(
        encodings(&t, kinds::CHANGE, "3"),
        0,
        "no change of encoding 3 is left"
    );
    assert_eq!(encodings(&t, kinds::REDACTION_RESIDUE, "2"), 1);
    assert_eq!(
        encodings(&t, kinds::REDACTION_RESIDUE, "1"),
        1,
        "the block's residue keeps 1"
    );
    let projected = t
        .store()
        .reader()
        .change(&change)
        .expect("the projected change");
    assert_eq!(
        projected.links.ported_from,
        Some(from),
        "the removal keeps the source"
    );
    assert_eq!(projected.id(), change, "and the change's identity");
    let facts = rdy_engine::history_facts::landing(&t.read(), landing).expect("facts");
    assert_eq!(facts.ported_changes.len(), 1);
    assert_eq!(facts.ported_changes[0].ported_from, from);
    assert!(
        has_feature(&t, FEATURE_PORTED_CHANGE),
        "it stays on after a removal"
    );
    // a reindex keeps it as it was
    t.reindex().expect("a reindex");
    assert_eq!(encodings(&t, kinds::REDACTION_RESIDUE, "2"), 1);
    let dump = l.at("t3.rdydump");
    t.read().dump(&dump).expect("a dump");
    t.close().expect("a close");
    // a load of the dump: the residue alone turns the feature on
    let to = l.at("t3-loaded");
    rdy_engine::load::load(&dump, &to).expect("a load");
    let loaded = Rdy::open(&to).expect("the loaded project");
    assert!(
        has_feature(&loaded, FEATURE_PORTED_CHANGE),
        "the residue turns it on"
    );
    assert_eq!(minimum_rdy(&loaded), version_of("0.58.0"));
    assert_eq!(encodings(&loaded, kinds::CHANGE, "3"), 0);
    assert_eq!(encodings(&loaded, kinds::CHANGE, "4"), 0);
    assert_eq!(encodings(&loaded, kinds::REDACTION_RESIDUE, "2"), 1);
    let again = loaded
        .store()
        .reader()
        .change(&change)
        .expect("the projected change");
    assert_eq!(again.links.ported_from, Some(from));
    let facts = rdy_engine::history_facts::landing(&loaded.read(), landing).expect("facts");
    assert_eq!(facts.ported_changes.len(), 1);
    assert_eq!(facts.ported_changes[0].ported_from, from);
    assert_eq!(
        loaded.store().reader().canon_head().expect("canon"),
        Some(removal)
    );
    loaded.close().expect("a close");
    // an older rdy refuses the residue alone
    let _old = format::older_reader_for_tests(&[FEATURE_PORTED_CHANGE]);
    let scratch = l.at("t3-refused");
    let e = rdy_engine::load::load(&dump, &scratch)
        .err()
        .expect("an older rdy refuses the dump");
    assert_eq!(e.code, "E_STORE_FORMAT");
    // the formats: a residue changes none of them, and the frame kinds a dump holds are fixed
    assert_eq!(
        (format::FEATURE_STORE_FORMAT, rdy_store::dump::DUMP_FORMAT),
        (19, 4)
    );
    assert_eq!(codec::latest(kinds::CHANGE), 1);
    assert_eq!(codec::latest(kinds::REDACTION_RESIDUE), 1);
}

/// hpt5: the first-use fixtures of `ported_change` open as written. The native store opens in a
/// copy with no index, rebuilds it, passes fsck and holds the landing and its two ported changes
/// with their sources; the residue of the removed change is encoding 2 and keeps the source. The
/// frozen files never change, and the dump loads as recorded.
#[test]
fn hpt5_ported_fixture_opens_and_keeps_its_ports() {
    let dir = fixture_dir();
    assert!(dir.join("expect.json").exists(), "{NATIVE} is frozen");
    let before: BTreeMap<String, (u64, String)> = rdy_qa::foreign_fixture::files_under(&dir)
        .into_iter()
        .map(|f| {
            let d = rdy_qa::foreign_fixture::digest_of(&dir.join(&f));
            (f, d)
        })
        .collect();
    assert_eq!(
        rdy_qa::foreign_fixture::files_under(&dir),
        [
            "FEATURES",
            "FORMAT",
            "expect.json",
            "lock",
            "segments/seg-000000.log"
        ]
    );
    let want: Value =
        serde_json::from_slice(&std::fs::read(dir.join("expect.json")).expect("an expect file"))
            .expect("json");
    assert_eq!(want["inventory"], rdy_qa::foreign_fixture::inventory(&dir));
    assert_eq!(
        Hash::of(rdy_qa::foreign_fixture::inventory_text(&dir).as_bytes()).to_string(),
        INVENTORY_BLAKE3
    );
    assert_eq!(
        want["required_rdy"],
        json!(format::PORTED_CHANGE_REQUIRED_RDY)
    );
    let text = std::fs::read_to_string(dir.join("expect.json")).expect("text");
    for bad in [
        "/Users",
        "/home/",
        "/tmp",
        "/var/",
        "\\\\",
        ":\\",
        "localhost",
    ] {
        assert!(!text.contains(bad), "the expect file names {bad}");
    }
    assert_eq!(
        std::fs::read_to_string(dir.join("FORMAT")).expect("a sentinel"),
        format::format_file_text()
    );
    let feature = want["store"]["manifest"]["features"]
        .as_array()
        .expect("features")
        .iter()
        .find(|f| f["name"] == FEATURE_PORTED_CHANGE)
        .expect("the manifest holds ported_change")
        .clone();
    assert_eq!(feature["class"], "incompatible");
    assert_eq!(feature["required_rdy"], format::PORTED_CHANGE_REQUIRED_RDY);
    // a copy with no index opens, and every recorded fact reads back
    let (_base, root) = rdy_qa::foreign_fixture::project_copy(&dir);
    let plan = rdy_store::upgrade::inspect(&root.join(".rdy/store")).expect("a plan");
    assert!(plan.steps.is_empty(), "no step: {:?}", plan.steps);
    assert!(!plan.requires_upgrade && !plan.pending_check);
    let facts = opened_facts(&root);
    assert!(root.join(".rdy/store/index.redb").exists(), "rebuilt");
    for member in want["store"].as_object().expect("facts").keys() {
        assert_eq!(facts[member], want["store"][member], "{member}");
    }
    let r = Rdy::open(&root).expect("the copy");
    let world = &want["world"];
    let landing: Hash = world["landing"]
        .as_str()
        .expect("a landing")
        .parse()
        .expect("a hash");
    let own = changes_of(&r, &landing);
    assert_eq!(own.len(), 2);
    for (c, row) in own.iter().zip(world["ported"].as_array().expect("rows")) {
        assert_eq!(c.to_string(), row["change"].as_str().expect("a change"));
    }
    // the first change is stored in encoding 3 and keeps its source; the second was removed
    let first = r.store().reader().change(&own[0]).expect("a change");
    assert_eq!(
        first
            .links
            .ported_from
            .map(|f| (f.repo_id.to_string(), f.change.to_string())),
        Some((
            world["ported"][0]["source_repo"]
                .as_str()
                .expect("a repo")
                .to_string(),
            world["ported"][0]["source_change"]
                .as_str()
                .expect("a change")
                .to_string(),
        ))
    );
    assert_eq!(first.encoding(), CHANGE_PORT_ENCODING);
    let second = r
        .store()
        .reader()
        .change(&own[1])
        .expect("a projected change");
    assert_eq!(
        second.links.ported_from.map(|f| f.change.to_string()),
        Some(
            world["ported"][1]["source_change"]
                .as_str()
                .expect("a change")
                .to_string()
        )
    );
    let raw = &facts["raw_encodings"];
    assert_eq!(raw["2"]["3"], 1, "one change of encoding 3");
    assert_eq!(raw["21"]["2"], 1, "one residue of encoding 2");
    let ported = rdy_engine::history_facts::landing(&r.read(), landing).expect("facts");
    assert_eq!(
        ported.ported_changes.len(),
        2,
        "both ports, the removed one too"
    );
    r.close().expect("a close");
    rdy_qa::invariants::check_all(&Rdy::open(&root).expect("the copy"))
        .unwrap_or_else(|v| panic!("invariants: {v:#?}"));
    rdy_qa::reindex_check::reindex_reproduces(&root).unwrap_or_else(|v| panic!("reindex: {v:#?}"));
    // the dump loads as recorded, and a reindex of the load is the load
    let dump = qa_dir().join("fixtures/dump").join(DUMP);
    let dump_want: Value = serde_json::from_slice(
        &std::fs::read(qa_dir().join("fixtures/dump").join(DUMP_EXPECT)).expect("an expect file"),
    )
    .expect("json");
    assert_eq!(
        dump_want["formats"],
        rdy_qa::chain_fixture::current_formats()
    );
    assert_eq!(dump_want["world"], *world);
    let scratch = tempfile::tempdir().expect("a directory");
    let loaded = rdy_qa::foreign_fixture::dump_load_facts(&dump, &scratch.path().join("loaded"));
    assert_eq!(
        loaded["store"]["manifest"],
        dump_want["whole"]["store"]["manifest"]
    );
    assert_eq!(
        rdy_qa::foreign_fixture::dump_file_facts(&dump)["store_format"],
        dump_want["file"]["store_format"]
    );
    let after: BTreeMap<String, (u64, String)> = rdy_qa::foreign_fixture::files_under(&dir)
        .into_iter()
        .map(|f| {
            let d = rdy_qa::foreign_fixture::digest_of(&dir.join(&f));
            (f, d)
        })
        .collect();
    assert_eq!(after, before, "the frozen files never change");
}
