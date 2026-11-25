//! hfp5 (D-TXFHR; D-99CWC, D-HX6BX, D-B2AC3, D-RDH1C): the contract fixture of hub forks, Part 1.
//!
//! `hub-forks-v1.json` is this plan's fixture. It started as a byte copy of the frozen
//! `m5b-v1.json`, and every selector named it until its successor `hub-ports-v1.json`. It is
//! frozen since, and so are `hub-ports-v1.json` and `m5a-v1.json`, each since its own successor;
//! the selectors name `rdy-0590-v1.json` now. One capture wrote its delta: the fork flag of
//! `rdy clone`, its populated probes, the hidden-list form, and the semantic probes of a fork
//! and its lineage. The tests here pin that delta leaf by leaf, keep every earlier fixture and
//! every released format and vector as it was, and show that a fork keeps its identities
//! through a reindex and a dump.
use rdy_engine::clone::{CloneSource, CloneSummary, clone_at};
use rdy_engine::publication::{self, IMPLICIT_ORG};
use rdy_engine::redaction::RedactInput;
use rdy_engine::signature_check::{read_check_context, signature_checks};
use rdy_engine::{Rdy, RdyError, pin_clock};
use rdy_model::dispute::{Dispute, DisputeOutcome, DisputeResolution};
use rdy_model::signature::{Signature, StatementSource, parse_statement};
use rdy_model::{Hash, Provenance, RepoId, Role};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::signing::{TestKey, sign_record};
use rdy_store::hub_canon::{CanonMove, Moved};
use rdy_store::kinds;
use rdy_store::meta::Origin;
use rdy_store::sync_stream::StreamPurpose;
use rdy_store::take_batch::HistoryTarget;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The predecessor of this plan's fixture, frozen from here on.
const PREDECESSOR: &str = "m5b-v1.json";

/// The sha256 of [`PREDECESSOR`], its final bytes. It never moves: a later contract change is a
/// successor fixture, never an edit of that file.
const PREDECESSOR_SHA: &str = "d00807ccc5ed37448eb81fec8addd240633596f73aa288b1dc54d26256d2fb41";

/// This plan's fixture, which every selector named until its successor [`SELECTED`].
const CURRENT: &str = "hub-forks-v1.json";

/// The sha256 of [`CURRENT`] after its capture, its final bytes. A later contract change is a
/// successor.
const CURRENT_SHA: &str = "5cff7aee1f5afb3e1e1e0364fc72fb7367a6acf103b1ad64040818cc13cd3252";

/// The successors of [`CURRENT`] before [`SELECTED`], in order, each frozen since its own
/// successor, with the sha256 that its successor copied.
const LATER: [(&str, &str); 2] = [
    (
        "hub-ports-v1.json",
        "1ca4ac7975b5518125d3ff65dc119f7e3d46e8305fc018a5425752d22f51fcab",
    ),
    (
        "m5a-v1.json",
        "b18e1992465210f6af99eabafb0290fb9b12f2288db5d5ac83bfa4f1a44bc4ec",
    ),
];

/// The latest successor of [`CURRENT`], which every selector names now; [`CURRENT`] is frozen
/// since its first successor.
const SELECTED: &str = "rdy-0590-v1.json";

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

/// hfp5: every selector names the latest successor of this plan's fixture, and none names this
/// plan's fixture, its earlier successors, `m5b-v1.json` or `sync-client-b-v1.json`, the fixtures
/// the selectors named before it.
#[test]
fn hfp5_selectors_name_the_current_fixture_only() {
    for file in SELECTORS {
        let text =
            std::fs::read_to_string(root().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"));
        assert!(
            text.contains(&format!("fixtures/contracts/{SELECTED}\"")),
            "{file} selects {SELECTED}"
        );
        for earlier in [PREDECESSOR, "sync-client-b-v1.json", CURRENT]
            .into_iter()
            .chain(LATER.map(|(n, _)| n))
        {
            assert!(
                !text.contains(&format!("contracts/{earlier}\"")),
                "{file} no longer selects {earlier}"
            );
        }
    }
}

/// hfp5: the successor keeps every earlier fixture as it was. Each frozen file keeps its bytes,
/// the folder holds those, this plan's fixture and its successors and no other, each earlier
/// successor keeps its bytes, and this plan's fixture differs from its predecessor and keeps its
/// bytes.
#[test]
fn hfp5_successor_preserves_every_frozen_fixture() {
    for (name, want) in FROZEN {
        assert_eq!(&sha(&bytes(name)), want, "{name} is frozen");
    }
    let mut names: Vec<String> = std::fs::read_dir(contracts())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for (name, want) in LATER {
        assert_eq!(sha(&bytes(name)), want, "{name} is frozen");
    }
    let mut want: Vec<&str> = FROZEN
        .iter()
        .chain(LATER.iter())
        .map(|(n, _)| *n)
        .chain([CURRENT, SELECTED])
        .collect();
    want.sort();
    assert_eq!(
        names, want,
        "the folder holds every frozen fixture, {CURRENT}, its successors and {SELECTED}"
    );
    let now = sha(&bytes(CURRENT));
    assert_ne!(now, PREDECESSOR_SHA, "{CURRENT} is a successor, not a copy");
    assert_eq!(now, CURRENT_SHA, "{CURRENT} has its captured bytes");
}

/// Every leaf where this plan's fixture differs from `m5b-v1.json`, each with the task that
/// owns it and why. The keyed form names an entry of a list by its identity.
const OWNED_CHANGES: &[(&str, &str)] = &[
    (
        "added /cli/tree/subcommands/name=clone/args/id=at",
        "task 3: rdy clone takes --at TAG",
    ),
    (
        "changed /cli/tree/subcommands/name=clone/groups/id=CloneArgs/args",
        "task 3: the clone argument group lists at",
    ),
    (
        "added /mcp/tools/rdy_clone/input_schema/properties/at",
        "task 3: rdy_clone takes an optional at, which needs fork",
    ),
    (
        "added /cli/runtime/probes/clone at",
        "task 3: a successful populated fork at a tag, through the command line",
    ),
    (
        "removed /cli/runtime/not_probed_commands/clone",
        "task 3: clone has a probe now",
    ),
    (
        "added /mcp/result_fields/rdy_clone",
        "task 3: a successful populated fork at a tag, through the tool",
    ),
    (
        "removed /mcp/result_fields_not_captured/rdy_clone",
        "task 3: rdy_clone has a recorded reply now",
    ),
    (
        "added /dump_load/fork_at",
        "tasks 2 and 3: the semantic probe of a project fork at a tag",
    ),
    (
        "added /signatures/hub_fork_lineage",
        "tasks 1 and 2: the lineage probe of a hub fork",
    ),
    (
        "added /mcp/result_fields/rdy_publication_check hidden form",
        "task 4: the hidden-list form of a first publication check",
    ),
    (
        "added /errors_warnings/fork_at",
        "task 3: the refusals of a fork at a tag",
    ),
    (
        "added /errors_warnings/templates/CLONE_AT_NEEDS_FORK_MESSAGE",
        "task 3: the message of at without fork",
    ),
    (
        "added /errors_warnings/templates/CLONE_AT_NEEDS_FORK_FIX",
        "task 3: the fix of at without fork",
    ),
    (
        "added /errors_warnings/templates/CLONE_AT_NEEDS_PROJECT_MESSAGE",
        "task 3: the message of at with a dump",
    ),
    (
        "added /errors_warnings/templates/CLONE_AT_NEEDS_PROJECT_FIX",
        "task 3: the fix of at with a dump",
    ),
    (
        "added /errors_warnings/templates/FORK_DESTINATION_FIX",
        "tasks 2 and 3: the fix of a fork into a directory that is not empty",
    ),
];

/// hfp5: this plan's fixture differs from `m5b-v1.json` in exactly the owned leaves. Each listed
/// leaf changed and no other, every other leaf keeps its value, no section moved, and the
/// removals are the two commands the probes now cover.
#[test]
fn hfp5_contract_delta_is_exactly_the_owned_leaves() {
    let (was, now) = (read(PREDECESSOR), read(CURRENT));
    let mut found = vec![];
    leaves(&keyed(&was), &keyed(&now), "", &mut found);
    found.sort();
    let mut want: Vec<String> = OWNED_CHANGES.iter().map(|(c, _)| c.to_string()).collect();
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
        if change.starts_with("removed ") {
            assert!(
                [
                    "/cli/runtime/not_probed_commands/clone",
                    "/mcp/result_fields_not_captured/rdy_clone"
                ]
                .contains(&path),
                "{change}"
            );
        } else {
            assert!(
                change.starts_with("added ") || change.starts_with("changed "),
                "{change}"
            );
        }
    }
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

// ---------------------------------------------------------------------------------------------
// the project fork probe (dump_load.fork_at, errors_warnings.fork_at)

/// The clock of every probe landing (injected).
const AT: i64 = 1_000_000;

/// The text the source removes after its first version.
const SECRET: &[u8] = b"ZEBRA-7731";

/// A person's provenance at `at_ms`.
fn person(at_ms: i64) -> Provenance {
    Provenance {
        actor: "human:qa".into(),
        run: None,
        role: Role::Approver,
        at_ms,
    }
}

/// A new doc `title` with one prose block in `view`.
fn doc(r: &mut Rdy, view: &str, title: &str, body: &str) -> rdy_engine::WriteResult {
    write(
        r,
        view,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": title.to_lowercase(), "title": title, "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
        ]),
    )
    .expect("a write")
}

/// Proposes and lands `view`; the landing.
fn landed(r: &mut Rdy, view: &str) -> Hash {
    land(r, view)
        .expect("a landing")
        .parse()
        .expect("a landing hash")
}

fn tagged(r: &mut Rdy, name: &str) {
    r.tag(&approver(), name, None, None).expect("a tag");
}

/// The canon chain of `r`, oldest first.
fn chain(r: &Rdy) -> Vec<Hash> {
    let reader = r.store().reader();
    let mut out = vec![];
    let mut cur = reader.canon_head().expect("a canon head");
    while let Some(l) = cur {
        out.push(l);
        cur = reader.landing(&l).expect("a landing").prev;
    }
    out.reverse();
    out
}

fn ref_names(r: &Rdy, prefix: &str) -> Vec<String> {
    r.store()
        .reader()
        .refs_with_prefix(prefix)
        .expect("refs")
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

/// Whether some file under `dir` holds `needle`. Call it only on a closed store: an open store
/// holds byte-range locks on its lock file and its index, and on Windows a read of a locked
/// file fails (OS error 33), so a scan of an open store cannot read every file.
fn bytes_hold(dir: &Path, needle: &[u8]) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).expect("a directory") {
            let p = e.expect("an entry").path();
            if p.is_dir() {
                stack.push(p);
            } else if std::fs::read(&p)
                .expect("a file")
                .windows(needle.len())
                .any(|w| w == needle)
            {
                return true;
            }
        }
    }
    false
}

/// Project S: `p1` (a doc with a secret block) tagged v1.0.0, `p2` tagged v2.0.0, an open view
/// `draft`, the removal of the secret `p3` tagged v3.0.0, the correction `p4`, and an upheld
/// dispute of `p1` that `p4` corrects. S is closed.
struct Source {
    dir: tempfile::TempDir,
    repo: RepoId,
    p1: Hash,
    later: [Hash; 3],
}

impl Source {
    fn root(&self) -> PathBuf {
        self.dir.path().join("p")
    }

    fn at(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

fn source() -> Source {
    let dir = tempfile::tempdir().expect("a directory");
    let root = dir.path().join("p");
    std::fs::create_dir_all(&root).expect("a root");
    let mut p = Rdy::init(&root, "rdy-standard").expect("a project");
    let repo = p.read().repo_id().expect("a repo id");
    let w = doc(&mut p, "work", "Intro", "Public words.");
    let secret = write(
        &mut p,
        "work",
        json!([{"op": "create", "type": "prose", "id": "$x", "parent": w.created["$d"], "body": "The launch code is ZEBRA-7731."}]),
    )
    .expect("a secret")
    .created["$x"]
        .clone();
    let p1 = landed(&mut p, "work");
    tagged(&mut p, "v1.0.0");
    doc(&mut p, "w1", "Later", "Words after the first version.");
    let p2 = landed(&mut p, "w1");
    tagged(&mut p, "v2.0.0");
    doc(&mut p, "draft", "Draft", "Work in flight.");
    p.redact_in(
        &writer(),
        "redact",
        RedactInput {
            targets: vec![secret],
            reason_class: "secret".into(),
            reason: "a leaked code".into(),
            dry_run: false,
        },
    )
    .expect("a removal");
    let p3 = landed(&mut p, "redact");
    tagged(&mut p, "v3.0.0");
    doc(&mut p, "w4", "Fix", "The corrected words.");
    let p4 = landed(&mut p, "w4");
    let (dispute, _) = p
        .store_mut()
        .put_dispute(
            &Dispute {
                landing: p1,
                opened_by: person(10),
                reason: "the first version leaked a code".into(),
                at_ms: 10,
            },
            None,
        )
        .expect("a dispute");
    p.store_mut()
        .put_dispute_resolution(
            &DisputeResolution {
                dispute,
                outcome: DisputeOutcome::Upheld,
                by: person(20),
                correction: Some(p4),
            },
            None,
        )
        .expect("a resolution");
    p.seal_open().expect("a seal");
    p.close().expect("a close");
    Source {
        dir,
        repo,
        p1,
        later: [p2, p3, p4],
    }
}

/// Forks the closed project at `root` at tag `tag` into `dir`, through the open project's state.
fn fork_at(root: &Path, tag: &str, dir: &Path) -> Result<CloneSummary, RdyError> {
    let r = Rdy::open(root).expect("a project");
    let st = r.read();
    let out = clone_at(CloneSource::State(&st), dir, true, Some(tag));
    drop(st);
    r.close().expect("a close");
    out
}

/// The changes of the source's later landings that the fork `f` stores: evidence only.
fn evidence(s: &Source, f: &Rdy) -> Vec<Hash> {
    let reader = f.store().reader();
    let mut out = vec![];
    for l in s.later {
        if !reader.has_kind(kinds::LANDING, &l).expect("a kind") {
            continue;
        }
        for c in reader.landing(&l).expect("a landing").changes {
            if reader.has_kind(kinds::CHANGE, &c).expect("a kind") {
                out.push(c);
            }
        }
    }
    out
}

/// What a fork keeps through a reindex and through a dump and its load.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Kept {
    repo: String,
    origin: Option<Origin>,
    chain: Vec<Hash>,
    tags: Vec<String>,
    tag_ids: Vec<Hash>,
    views: Vec<String>,
    other_refs: Vec<String>,
}

fn kept(r: &Rdy) -> Kept {
    let reader = r.store().reader();
    let mut other = vec![];
    for prefix in ["held/", "replaced/", "meta/sync/"] {
        other.extend(ref_names(r, prefix));
    }
    Kept {
        repo: reader
            .repo_id()
            .expect("a repo id")
            .expect("a repo id")
            .to_string(),
        origin: reader.origin().expect("an origin"),
        chain: chain(r),
        tags: ref_names(r, "tag/"),
        tag_ids: reader
            .tags()
            .expect("tags")
            .into_iter()
            .map(|(_, h, _)| h)
            .collect(),
        views: ref_names(r, "view/"),
        other_refs: other,
    }
}

/// The code, message and fix of a refusal, with `paths` and the short `hashes` named by a word.
fn refusal(e: &RdyError, paths: &[&Path], hashes: &[Hash]) -> Value {
    let mut message = super::release_contract::normalize(&e.message, paths);
    for h in hashes {
        message = message.replace(&h.short().to_string(), "<short hash>");
    }
    json!({"code": e.code, "message": message, "fix": e.fix})
}

/// What the project fork probe saw.
pub(super) struct ForkProbe {
    /// The ten facts of `dump_load.fork_at`, each a boolean.
    pub(super) facts: Value,
    /// The four refusals of `errors_warnings.fork_at`.
    pub(super) errors: Value,
    /// The fork's identities at each stage: the fork's, after a reindex and after a dump and its
    /// load.
    pub(super) stages: Vec<(&'static str, Value)>,
    /// The `chain` and `stopped_at` of an ordinary clone of the fork's dump: the load's own
    /// representation, which a fork at a tag does not fill.
    pub(super) plain_chain: (Value, Value),
}

/// The project fork probe, run once: S forked at v1.0.0, then reopened, reindexed, dumped and
/// loaded, and the four refusals.
pub(super) fn fork_probe() -> &'static ForkProbe {
    static PROBE: OnceLock<ForkProbe> = OnceLock::new();
    PROBE.get_or_init(run_fork_probe)
}

fn identities(k: &Kept) -> Value {
    json!({
        "repo": k.repo,
        "chain": k.chain.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "tags": k.tags,
        "tag_ids": k.tag_ids.iter().map(ToString::to_string).collect::<Vec<_>>(),
    })
}

fn run_fork_probe() -> ForkProbe {
    let _clock = pin_clock(AT);
    let s = source();
    let to = s.at("f");
    let out = fork_at(&s.root(), "v1.0.0", &to).expect("a fork at v1.0.0");
    // each secret scan runs on the closed store (see `bytes_hold`)
    let mut secret_stored = bytes_hold(&to.join(".rdy"), SECRET);
    let mut f = Rdy::open(&to).expect("the fork");
    let first = kept(&f);

    let from = out.forked_from.as_ref();
    let new_repo_id = first.repo != s.repo.to_string()
        && first.repo == out.repo_id
        && out.fork
        && out.origin.is_none()
        && from.is_some_and(|o| o.repo_id == s.repo.to_string());
    let origin_matches = first.origin
        == Some(Origin {
            fork: true,
            repo_id: s.repo,
            tag: Some("v1.0.0".into()),
            landing: Some(s.p1),
        });
    let reader = f.store().reader();
    let evidence_changes = evidence(&s, &f);
    let proof_landing_unreachable = !evidence_changes.is_empty()
        && s.later.iter().all(|l| {
            reader.has_kind(kinds::LANDING, l).expect("a kind") && !first.chain.contains(l)
        })
        && evidence_changes
            .iter()
            .all(|c| reader.landing_of(c).expect("a landing").is_none());
    drop(reader);

    // a reindex, then a dump and its load, keep what the fork holds
    f.reindex().expect("a reindex");
    let reindexed = kept(&f);
    let file = s.at("f.rdydump");
    f.read().dump(&file).expect("a dump");
    f.close().expect("a close");
    secret_stored |= bytes_hold(&to.join(".rdy"), SECRET);
    let loaded_to = s.at("loaded");
    rdy_engine::load::load(&file, &loaded_to).expect("a load");
    let l = Rdy::open(&loaded_to).expect("the loaded fork");
    let loaded = kept(&l);
    l.close().expect("a close");
    secret_stored |= bytes_hold(&loaded_to.join(".rdy"), SECRET);
    let removal_applied = !secret_stored;

    // the refusals, each through the engine's own entry
    let r = Rdy::open(&s.root()).expect("a project");
    let st = r.read();
    let needs_fork = clone_at(CloneSource::State(&st), &s.at("nf"), false, Some("v1.0.0"))
        .expect_err("at needs fork");
    let needs_project = clone_at(CloneSource::File(&file), &s.at("np"), true, Some("v1.0.0"))
        .expect_err("at needs a project");
    let busy = s.at("busy");
    std::fs::create_dir_all(&busy).expect("a directory");
    std::fs::write(busy.join("x"), b"x").expect("a file");
    let not_empty = clone_at(CloneSource::State(&st), &busy, true, Some("v1.0.0"))
        .expect_err("a directory that is not empty");
    drop(st);
    r.close().expect("a close");

    // a change the fork stores only as later evidence cannot be replayed, and nothing is written
    let mut f = Rdy::open(&to).expect("the fork");
    doc(&mut f, "mine", "Own-mine", "Own words.");
    let head = f.read().view_head("mine").expect("a view").head;
    let before = (
        f.store().log_end(),
        f.store().reader().refs_with_prefix("").expect("refs"),
    );
    let mut replay = None;
    let mut replay_refused = !evidence_changes.is_empty();
    for c in &evidence_changes {
        match f.view_replay(&writer(), "mine", &[c.to_string()]) {
            Err(e)
                if e.code == "E_NOT_FOUND" && e.message == format!("no change {}", c.short()) =>
            {
                replay.get_or_insert((e, *c));
            }
            _ => replay_refused = false,
        }
    }
    let after = (
        f.store().log_end(),
        f.store().reader().refs_with_prefix("").expect("refs"),
    );
    replay_refused &= before == after && f.read().view_head("mine").expect("a view").head == head;
    let (replay_error, replayed) = replay.expect("a refused replay");
    f.close().expect("a close");

    let facts = json!({
        "canon_at_tag": first.chain == [s.p1],
        "new_repo_id": new_repo_id,
        "origin_matches": origin_matches,
        "only_chain_tag_refs": first.tags == ["tag/v1.0.0"],
        "no_source_views": first.views.is_empty() && first.other_refs.is_empty(),
        "removal_applied": removal_applied,
        "proof_landing_unreachable": proof_landing_unreachable,
        "replay_refused": replay_refused,
        "reindex_equal": reindexed == first,
        "load_equal": loaded == first,
    });
    // an ordinary clone of the dump file holds the load's chain
    let plain = clone_at(CloneSource::File(&file), &s.at("plain"), false, None)
        .expect("a clone of the fork's dump");
    let plain_chain = (
        serde_json::to_value(&plain.load.chain).expect("a chain"),
        serde_json::to_value(&plain.load.stopped_at).expect("a stop"),
    );
    let errors = json!({
        "needs_fork": refusal(&needs_fork, &[], &[]),
        "needs_project": refusal(&needs_project, &[], &[]),
        "destination_not_empty": refusal(&not_empty, &[&busy], &[]),
        "evidence_replay": refusal(&replay_error, &[], &[replayed]),
    });
    ForkProbe {
        facts,
        errors,
        stages: vec![
            ("fork", identities(&first)),
            ("reindexed", identities(&reindexed)),
            ("loaded", identities(&loaded)),
        ],
        plain_chain,
    }
}

// ---------------------------------------------------------------------------------------------
// the hub fork probe (signatures.hub_fork_lineage)

/// Lands two docs in `r`, each tagged: `v1.0.0` and `v2.0.0`. Returns their landings.
pub(super) fn populate_tagged(r: &mut Rdy) -> (Hash, Hash) {
    doc(r, "work", "First", "First words.");
    let p1 = landed(r, "work");
    tagged(r, "v1.0.0");
    doc(r, "w2", "Second", "Second words.");
    let p2 = landed(r, "w2");
    tagged(r, "v2.0.0");
    (p1, p2)
}

/// A project with two tagged landings ([`populate_tagged`]), open, with them.
fn lineage_project(root: &Path) -> (Rdy, Hash, Hash) {
    std::fs::create_dir_all(root).expect("a root");
    let mut r = Rdy::init(root, "rdy-standard").expect("a project");
    let (p1, p2) = populate_tagged(&mut r);
    (r, p1, p2)
}

/// Takes the canon history of `src` at `root` above `base` into holder `hd` and moves its canon
/// by `mv` (`expected` its tip before).
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

/// The hub of project `src` (landings `p1`, `p2`) at `dir`, seeded with `p1` and extended to `p2`,
/// with the project's two version tags named, closed and opened again.
fn hub_of(src: &Rdy, p1: Hash, p2: Hash, dir: &Path) -> Rdy {
    let repo = src.read().repo_id().expect("a repo id");
    let mut hub = Rdy::create_holder(dir, repo).expect("a holder");
    push(&mut hub, src, p1, None, CanonMove::Seed, None);
    push(&mut hub, src, p2, Some(p1), CanonMove::Extend, Some(p1));
    let refs = ["v1.0.0", "v2.0.0"]
        .iter()
        .map(|t| {
            let name = format!("tag/{t}");
            let value = src.store().get_ref(&name).expect("a ref").expect("a tag");
            (name, value)
        })
        .collect();
    hub.store_mut().set_refs(refs).expect("the tag refs");
    hub.seal_open().expect("a seal");
    hub.close().expect("a close");
    Rdy::open_holder(dir).expect("the hub")
}

/// The verdict word of each signature of `subject` in `h`, by the repo its statement names.
fn verdicts(h: &Rdy, subject: &Hash) -> BTreeMap<String, String> {
    let st = h.read();
    signature_checks(&st, subject, None)
        .expect("signature checks")
        .iter()
        .zip(
            h.store()
                .reader()
                .signatures_of(subject)
                .expect("signatures"),
        )
        .map(|(c, id)| {
            let g = h.store().reader().signature(&id).expect("a signature");
            let repo = parse_statement(&g.statement)
                .expect("a statement")
                .field("repo")
                .expect("a repo")
                .to_string();
            (repo, c.statement.clone())
        })
        .collect()
}

/// A later `meta/origin` into the closed holder at `root`, through the store's own origin batch.
fn later_origin(h: Rdy, root: &Path, o: &Origin) -> Rdy {
    h.close().expect("a close");
    let mut s = rdy_store::Store::open(&root.join(".rdy/store")).expect("a store");
    s.put_origin(o, None).expect("an origin");
    drop(s);
    Rdy::open_holder(root).expect("a holder")
}

/// The lineage probe, run once.
pub(super) fn lineage_probe() -> &'static Value {
    static PROBE: OnceLock<Value> = OnceLock::new();
    PROBE.get_or_init(run_lineage_probe)
}

fn run_lineage_probe() -> Value {
    let _clock = pin_clock(AT);
    let dir = tempfile::tempdir().expect("a directory");
    let (src, p1, p2) = lineage_project(&dir.path().join("src"));
    let main_repo = src.read().repo_id().expect("a repo id");
    // main M, its line L (a fork of M at v1.0.0) and L's line LL (a fork of L)
    let m = hub_of(&src, p1, p2, &dir.path().join("m"));
    let l = rdy_engine::fork::fork_holder_at(&m.read(), "v1.0.0", &dir.path().join("l"))
        .expect("a fork of main");
    let ll_root = dir.path().join("ll");
    let ll = rdy_engine::fork::fork_holder_at(&l.read(), "v1.0.0", &ll_root)
        .expect("a fork of the line");
    let (l_repo, ll_repo) = (
        l.read().repo_id().expect("a repo id"),
        ll.read().repo_id().expect("a repo id"),
    );
    // an unrelated project and its baseline
    let (other, _, _) = lineage_project(&dir.path().join("other"));
    let other_repo = other.read().repo_id().expect("a repo id");
    let outside = publication::make_baseline(&other.read(), "tag:v1.0.0", IMPLICIT_ORG)
        .expect("an unrelated baseline");

    // the landing p1 signed in every repo of the line and in the unrelated one
    let rec = ll.store().reader().landing(&p1).expect("a landing");
    let signed = |repo: RepoId| -> Signature {
        sign_record(
            p1,
            StatementSource::Approval(&rec),
            repo,
            person(7),
            TestKey::Person,
        )
    };
    let mut ll = ll;
    for repo in [main_repo, l_repo, ll_repo, other_repo] {
        ll.store_mut()
            .put_signature(&signed(repo))
            .expect("a signature");
    }
    ll.seal_open().expect("a seal");
    ll.close().expect("a close");
    let mut ll = Rdy::open_holder(&ll_root).expect("the line's line");

    let word = |repo: RepoId, v: &BTreeMap<String, String>| v.get(&repo.to_string()).cloned();
    let seen = verdicts(&ll, &p1);
    let ancestor_verified = [main_repo, l_repo, ll_repo]
        .iter()
        .all(|r| word(*r, &seen).as_deref() == Some("match"));

    // baselines cross the three hubs both ways
    let baseline = |h: &Rdy, tag: &str, landing: Hash| {
        publication::make_baseline_at(&h.read(), tag, landing, IMPLICIT_ORG).expect("a baseline")
    };
    let (m1, m2) = (baseline(&m, "v1.0.0", p1), baseline(&m, "v2.0.0", p2));
    let (l1, ll1) = (baseline(&l, "v1.0.0", p1), baseline(&ll, "v1.0.0", p1));
    let accepts = |h: &Rdy, b: &publication::Baseline| {
        publication::check_baseline_lineage(&h.read(), b).is_ok()
    };
    let source_to_fork = accepts(&l, &m1) && accepts(&l, &m2);
    let fork_to_source = accepts(&m, &l1);
    let main_to_grandchild = accepts(&ll, &m1) && accepts(&ll, &m2);
    let grandchild_to_main = accepts(&m, &ll1);

    // the unrelated repo shares no lineage and stays foreign
    let refused = |h: &Rdy| {
        publication::check_baseline_lineage(&h.read(), &outside)
            .is_err_and(|e| e.code == "E_PUBLICATION_BASELINE")
    };
    let unrelated_foreign = word(other_repo, &seen).as_deref() == Some("foreign_repo")
        && [&m, &l, &ll].iter().all(|h| refused(h));

    // a late origin that names the unrelated repo widens nothing
    let lineage = |h: &Rdy| {
        let g = h
            .store()
            .reader()
            .signatures_of(&p1)
            .expect("signatures")
            .into_iter()
            .next()
            .expect("a signature");
        let g = h.store().reader().signature(&g).expect("a signature");
        read_check_context(&h.read(), &g)
            .expect("a check context")
            .source_repos
    };
    let before = lineage(&ll);
    ll = later_origin(
        ll,
        &ll_root,
        &Origin {
            fork: true,
            repo_id: other_repo,
            tag: Some("v1.0.0".into()),
            landing: Some(p1),
        },
    );
    let after = verdicts(&ll, &p1);
    let late_origin_ignored = lineage(&ll) == before
        && word(other_repo, &after).as_deref() == Some("foreign_repo")
        && refused(&ll)
        && accepts(&ll, &m1);
    for h in [m, l, ll] {
        h.close().expect("a close");
    }
    src.close().expect("a close");
    other.close().expect("a close");
    json!({
        "source_to_fork": source_to_fork,
        "fork_to_source": fork_to_source,
        "main_to_grandchild": main_to_grandchild,
        "grandchild_to_main": grandchild_to_main,
        "ancestor_verified": ancestor_verified,
        "unrelated_foreign": unrelated_foreign,
        "late_origin_ignored": late_origin_ignored,
    })
}

// ---------------------------------------------------------------------------------------------
// the leaves' values, the reply fields, the identities and the formats

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

/// hfp5: the owned leaves hold exactly the values the plan names: the flag and its place in the
/// declared order, the group, the tool property, the five template constants in the quoted form
/// the generator keeps and as the product spells them, and the refusal cases.
#[test]
fn hfp5_owned_leaves_hold_the_planned_values() {
    let fixture = read(CURRENT);
    let now = keyed(&fixture);
    // the flag: an optional value flag with no default, declared after --fork
    assert_eq!(
        at(&now, "/cli/tree/subcommands/name=clone/args/id=at"),
        &json!({
            "action": "set",
            "id": "at",
            "long": "at",
            "num_args": "1",
            "value_names": ["TAG"],
        })
    );
    let clone = command(&fixture, "clone");
    let ids: Vec<&str> = clone["args"]
        .as_array()
        .expect("args")
        .iter()
        .map(|a| a["id"].as_str().expect("an id"))
        .collect();
    assert_eq!(
        ids,
        [
            "source",
            "dir",
            "fork",
            "at",
            "json",
            "project",
            "no_daemon",
            "help"
        ]
    );
    assert_eq!(
        clone["groups"],
        json!([{"args": ["at", "dir", "fork", "source"], "id": "CloneArgs", "multiple": true, "required": false}])
    );
    // the tool: an optional string or null; the other properties and the required list stay
    let schema = &fixture["mcp"]["tools"]["rdy_clone"]["input_schema"];
    assert_eq!(schema["required"], json!(["dir"]));
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(
        keys_of(&schema["properties"]),
        ["at", "dir", "file", "fork", "url"]
    );
    let at_text = &schema["properties"]["at"];
    assert_eq!(at_text["type"], json!(["string", "null"]));
    let words = at_text["description"].as_str().expect("a description");
    for part in ["tag", "fork true", "file", "url"] {
        assert!(words.contains(part), "{part}: {words}");
    }
    // the five template constants, as the generator keeps them (the quoted Rust literal) and as
    // the product spells them
    let templates = &fixture["errors_warnings"]["templates"];
    for (name, text, live) in [
        (
            "CLONE_AT_NEEDS_FORK_MESSAGE",
            "a clone keeps the whole history; --at needs --fork",
            rdy_engine::CLONE_AT_NEEDS_FORK_MESSAGE,
        ),
        (
            "CLONE_AT_NEEDS_FORK_FIX",
            "add --fork, or remove --at",
            rdy_engine::CLONE_AT_NEEDS_FORK_FIX,
        ),
        (
            "CLONE_AT_NEEDS_PROJECT_MESSAGE",
            "--at needs a project",
            rdy_engine::CLONE_AT_NEEDS_PROJECT_MESSAGE,
        ),
        (
            "CLONE_AT_NEEDS_PROJECT_FIX",
            "load the dump, then fork the loaded project",
            rdy_engine::CLONE_AT_NEEDS_PROJECT_FIX,
        ),
        (
            "FORK_DESTINATION_FIX",
            "fork into a new directory, or an empty one",
            rdy_engine::FORK_DESTINATION_FIX,
        ),
    ] {
        assert_eq!(live, text, "{name} as the product spells it");
        assert_eq!(templates[name], format!("\"{text}\""), "{name} as captured");
    }
    // the refusal cases: exactly code, message and fix of the plan's table
    let cases = &fixture["errors_warnings"]["fork_at"];
    assert_eq!(
        keys_of(cases),
        [
            "destination_not_empty",
            "evidence_replay",
            "needs_fork",
            "needs_project"
        ]
    );
    for (case, code, message, fix) in [
        (
            "needs_fork",
            "E_BAD_INPUT",
            "a clone keeps the whole history; --at needs --fork",
            "add --fork, or remove --at",
        ),
        (
            "needs_project",
            "E_BAD_INPUT",
            "--at needs a project",
            "load the dump, then fork the loaded project",
        ),
        (
            "destination_not_empty",
            "E_ALREADY_EXISTS",
            "<path> is not empty",
            "fork into a new directory, or an empty one",
        ),
        (
            "evidence_replay",
            "E_NOT_FOUND",
            "no change <short hash>",
            "check the id with rdy view show <view>",
        ),
    ] {
        assert_eq!(
            cases[case],
            json!({"code": code, "message": message, "fix": fix}),
            "{case}"
        );
    }
    // the semantic probes: every member is a boolean, and every one holds
    assert_eq!(
        keys_of(&fixture["dump_load"]["fork_at"]),
        [
            "canon_at_tag",
            "load_equal",
            "new_repo_id",
            "no_source_views",
            "only_chain_tag_refs",
            "origin_matches",
            "proof_landing_unreachable",
            "reindex_equal",
            "removal_applied",
            "replay_refused"
        ]
    );
    assert_eq!(
        keys_of(&fixture["signatures"]["hub_fork_lineage"]),
        [
            "ancestor_verified",
            "fork_to_source",
            "grandchild_to_main",
            "late_origin_ignored",
            "main_to_grandchild",
            "source_to_fork",
            "unrelated_foreign"
        ]
    );
    for probe in [
        &fixture["dump_load"]["fork_at"],
        &fixture["signatures"]["hub_fork_lineage"],
    ] {
        for (name, value) in probe.as_object().expect("a probe") {
            assert_eq!(value, &json!(true), "{name}");
        }
    }
}

/// The members of a successful fork's reply (the load's summary, the new repo id and where the
/// fork comes from), sorted.
const CLONE_FIELDS: [&str; 17] = [
    "batches",
    "chain",
    "dump_format",
    "files_kept",
    "files_written",
    "fork",
    "forked_from",
    "frames",
    "fsck",
    "landings",
    "next",
    "project",
    "repo_id",
    "stopped_at",
    "store_format",
    "tags",
    "views",
];

/// The members of the fork's `fsck` report, sorted.
const FSCK_FIELDS: [&str; 10] = [
    "alt_sets",
    "frames",
    "intents",
    "landed_changes",
    "landings",
    "reviews",
    "transitions",
    "unlogged_moves",
    "versions",
    "views",
];

/// hfp5: the command line and the tool each record a successful, populated fork at a tag, and
/// the two replies are one shape. The reply has the clone's fields and `forked_from` with the
/// repo, the tag and the landing, and no `origin` or `warnings`. The load chain keeps its own
/// representation: a fork has no input file, so `chain` is empty and `stopped_at` is null; a
/// clone of a dump file fills them as `load` does.
#[test]
fn hfp5_clone_probes_pin_the_reply_fields() {
    let fixture = read(CURRENT);
    let cli = &fixture["cli"]["runtime"]["probes"]["clone at"];
    let mcp = &fixture["mcp"]["result_fields"]["rdy_clone"];
    assert_eq!(cli["exits"], json!([0]));
    assert_eq!(keys_of(cli), ["exits", "stdout"]);
    assert_eq!(keys_of(mcp), ["ok"]);
    assert_eq!(&cli["stdout"], &mcp["ok"], "one reply on both surfaces");
    let reply = &mcp["ok"];
    assert_eq!(keys_of(reply), CLONE_FIELDS);
    assert_eq!(
        keys_of(&reply["forked_from"]),
        ["landing", "repo_id", "tag"]
    );
    assert_eq!(keys_of(&reply["fsck"]), FSCK_FIELDS);
    assert_eq!(
        keys_of(&reply["store_format"]["features"]),
        ["features", "manifest_version"]
    );
    assert_eq!(
        keys_of(&reply["store_format"]["features"]["features"][0]),
        ["class", "name", "required_rdy"]
    );
    assert_eq!(reply["fork"], "bool");
    // the load's representation, not a fork's second name for the tag
    assert_eq!(reply["chain"], json!([]));
    assert_eq!(reply["stopped_at"], "null");
    let (chain, stop) = &fork_probe().plain_chain;
    let rows = chain.as_array().expect("a chain");
    assert_eq!(rows.len(), 1, "an ordinary clone of a dump file: {chain}");
    assert_eq!(keys_of(&rows[0]), ["base", "end", "file", "kind"]);
    assert_eq!(stop, &Value::Null);
    let at_stop = rdy_engine::StoppedAt {
        position: rdy_store::segment::Location {
            segment: 0,
            offset: 0,
        },
        landing: None,
        tag: None,
    };
    assert_eq!(
        keys_of(&serde_json::to_value(&at_stop).expect("a stop")),
        ["landing", "position", "tag"]
    );
}

/// The digest of the released dump fixtures and of the native store fixtures: the file count and
/// the sha256 of one line per file in byte order of its `/` path (`<path> <sha256 of its bytes>`).
const DUMP_FIXTURES_DIGEST: (usize, &str) = (
    86,
    "232ec8e9db5e8f4b582e82c3a4543c42743cdd59d82a36b3d39f75f7fe6a5d8e",
);
const STORE_FIXTURES_DIGEST: (usize, &str) = (
    85,
    "e5c48b33394612a9d4acc0f8325eaaa880953eb9dfa3ae723527b89853443fc8",
);

fn tree_digest(dir: &Path) -> (usize, String) {
    fn walk(dir: &Path, at: &str, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let e = e.unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            let rel = if at.is_empty() {
                name
            } else {
                format!("{at}/{name}")
            };
            if e.file_type().unwrap().is_dir() {
                walk(&e.path(), &rel, out);
            } else {
                out.push((rel, std::fs::read(e.path()).unwrap()));
            }
        }
    }
    let mut files = vec![];
    walk(dir, "", &mut files);
    // the first-use fixtures of `ported_change` (hub ports) and `dedup_spans` (span maps) came
    // after this plan's release
    files.retain(|(path, _)| {
        ![
            "v4-f1-e1-ported-change",
            "ported-change-1",
            "v4-f1-dedup-spans",
            "dedup-spans-1",
        ]
        .iter()
        .any(|later| path.starts_with(later))
    });
    files.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let lines: String = files
        .iter()
        .map(|(path, bytes)| format!("{path} {}\n", sha(bytes)))
        .collect();
    (files.len(), sha(lines.as_bytes()))
}

/// hfp5: a fork keeps its identities. Through a reindex and through a dump and its load, the
/// fork's repo id, canon chain, tag names and tag hashes are the ones it started with. Every
/// released dump, stream, object-hash and signature vector keeps its bytes: the released dump
/// and store fixtures, and every member of those rows in the successor.
#[test]
fn hfp5_reindex_dump_load_and_fork_keep_identities() {
    let probe = fork_probe();
    let (name, first) = &probe.stages[0];
    assert_eq!(*name, "fork");
    for (stage, now) in &probe.stages[1..] {
        assert_eq!(now, first, "the fork after {stage}");
    }
    assert_eq!(first["chain"].as_array().expect("a chain").len(), 1);
    assert_eq!(first["tags"], json!(["tag/v1.0.0"]));
    for (key, value) in probe.facts.as_object().expect("facts") {
        assert_eq!(value, &json!(true), "{key}");
    }
    assert!(
        lineage_probe()
            .as_object()
            .is_some_and(|m| m.values().all(|v| v == &json!(true)))
    );

    // the released fixtures keep their bytes
    let qa = Path::new(env!("CARGO_MANIFEST_DIR"));
    let dump = tree_digest(&qa.join("fixtures/dump"));
    assert_eq!(
        (dump.0, dump.1.as_str()),
        DUMP_FIXTURES_DIGEST,
        "dump fixtures"
    );
    let store = tree_digest(&qa.join("fixtures/store"));
    assert_eq!(
        (store.0, store.1.as_str()),
        STORE_FIXTURES_DIGEST,
        "store fixtures"
    );
    // the rows that hold vectors and identities are the predecessor's, member by member
    let (was, now) = (read(PREDECESSOR), read(CURRENT));
    for section in ["store", "sync", "derived_identities", "inventory_version"] {
        assert_eq!(was[section], now[section], "{section}");
    }
    for member in keys_of(&was["dump_load"]) {
        assert_eq!(
            was["dump_load"][&member], now["dump_load"][&member],
            "dump_load.{member}"
        );
    }
    for member in keys_of(&was["signatures"]) {
        assert_eq!(
            was["signatures"][&member], now["signatures"][&member],
            "signatures.{member}"
        );
    }
}

/// hfp5: no format or feature moves. The store stays at format 19, the dump at 4 and the streams
/// at 2, 3 and 4, and the feature registry is the 12 features of the predecessor, by name and
/// class. A fork adds no feature: it uses `hub_canon`.
#[test]
fn hfp5_no_format_or_feature_moves() {
    assert_eq!(
        (
            rdy_store::format::FEATURE_STORE_FORMAT,
            rdy_store::format::MANIFEST_VERSION,
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
    // the registry less the features later plans registered (hub ports, `ported_change`; span
    // maps, `dedup_spans`) is the 12 features of this plan, and those are the only additions
    let live: Vec<&str> = rdy_store::format::REGISTERED_FEATURES
        .iter()
        .copied()
        .filter(|n| *n != "ported_change" && *n != "dedup_spans")
        .collect();
    assert_eq!(live.len(), 12);
    assert_eq!(
        rdy_store::format::REGISTERED_FEATURES.len(),
        live.len() + 2,
        "ported_change and dedup_spans are the only additions"
    );
    assert!(rdy_store::format::REGISTERED_FEATURES.contains(&"hub_canon"));
    let (was, now) = (read(PREDECESSOR), read(CURRENT));
    assert_eq!(
        now["store"]["registered_names"],
        json!(live),
        "the store registers what the contract lists, and the later additions"
    );
    assert_eq!(
        was["store"]["registered_names"],
        now["store"]["registered_names"]
    );
    assert_eq!(was["store"]["features"], now["store"]["features"]);
    assert_eq!(
        now["dump_load"]["dump_format"],
        rdy_store::dump::DUMP_FORMAT
    );
    assert_eq!(
        was["dump_load"]["released_pairs"],
        now["dump_load"]["released_pairs"]
    );
    // no error code, tool or command is new: the codes and rows are the predecessor's
    assert_eq!(
        was["errors_warnings"]["codes"],
        now["errors_warnings"]["codes"]
    );
    assert_eq!(
        was["errors_warnings"]["error_rows"],
        now["errors_warnings"]["error_rows"]
    );
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
}

/// hfp5: the fork refusals are their own member. `errors_warnings.fork_at` holds the four
/// cases; the registered `error_rows` stay as they were and gain no `fork_at` row.
#[test]
fn hfp5_fork_refusals_are_their_own_member() {
    let (was, now) = (keyed(&read(PREDECESSOR)), current());
    let rows = at(&now, "/errors_warnings/error_rows");
    assert_eq!(rows, at(&was, "/errors_warnings/error_rows"));
    assert!(rows.get("row=fork_at").is_none(), "no registry row");
    assert!(now["errors_warnings"]["fork_at"].is_object());
    assert!(
        OWNED_CHANGES
            .iter()
            .all(|(c, _)| !c.contains("error_rows/fork_at")),
        "the allowlist names the member beside the rows"
    );
}
