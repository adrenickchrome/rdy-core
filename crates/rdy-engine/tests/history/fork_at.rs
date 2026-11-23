//! hfp3 (D-99CWC, D-HX6BX): a project fork at a tag. The fork is a new project with a new repo
//! id, the source's project files, its counted origins oldest first and its own origin last. Its
//! canon is the tag's landing, and it names only the tags of that landing's chain. It holds no
//! source view and a fresh default view. Later removal and dispute records travel as proof
//! records, and their changes are evidence only: no include or replay takes one as a source, by
//! name or through a dependency. A replay of a change that only a later record carries is
//! `E_NOT_FOUND`.
use crate::common;

use ed25519_dalek::Signer as _;
use rdy_engine::clone::{CloneSource, CloneSummary, clone, clone_at};
use rdy_engine::redaction::RedactInput;
use rdy_engine::signature_check::{read_check_context, signature_checks};
use rdy_engine::signing_crypto::blob_fingerprint;
use rdy_engine::*;
use rdy_model::dispute::{Dispute, DisputeOutcome, DisputeResolution};
use rdy_model::signature::{Signature, StatementSource, parse_statement, statement};
use rdy_model::*;
use rdy_store::kinds;
use rdy_store::meta::Origin;
use serde_json::{Value, json};
use ssh_encoding::Encode;
use ssh_key::public::{Ed25519PublicKey, KeyData};
use ssh_key::{Algorithm, HashAlg, SshSig};
use std::path::{Path, PathBuf};

const AT: i64 = 1_000_000;

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn person(at_ms: i64) -> Provenance {
    Provenance {
        actor: "human:founder".into(),
        run: None,
        role: Role::Approver,
        at_ms,
    }
}

fn write_in(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "hfp3".into(),
        },
    )
    .unwrap()
}

/// Writes a new doc `title` with one prose block in `view`; returns the write.
fn doc(r: &mut Rdy, view: &str, title: &str, body: &str) -> WriteResult {
    write_in(
        r,
        view,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": title.to_lowercase(), "title": title, "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$p", "parent": "$d", "body": body}
        ]),
    )
}

/// Proposes and lands `view`; returns the landing.
fn land(r: &mut Rdy, view: &str) -> Hash {
    let p = r
        .propose(&writer(), view, "hfp3", Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap().parse().unwrap()
}

fn tag(r: &mut Rdy, name: &str) {
    r.tag(&founder(), name, None, None).unwrap();
}

/// The canon chain of `r`, oldest first.
fn chain(r: &Rdy) -> Vec<Hash> {
    let reader = r.store().reader();
    let mut out = vec![];
    let mut cur = reader.canon_head().unwrap();
    while let Some(l) = cur {
        out.push(l);
        cur = reader.landing(&l).unwrap().prev;
    }
    out.reverse();
    out
}

fn ref_names(r: &Rdy, prefix: &str) -> Vec<String> {
    r.store()
        .reader()
        .refs_with_prefix(prefix)
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

/// Every ref of `r` with its value.
fn all_refs(r: &Rdy) -> Vec<(String, Vec<u8>)> {
    r.store().reader().refs_with_prefix("").unwrap()
}

/// Whether some file under `dir` holds `needle`.
fn bytes_hold(dir: &Path, needle: &[u8]) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if std::fs::read(&p)
                .unwrap()
                .windows(needle.len())
                .any(|w| w == needle)
            {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------------------------
// the source project

const SECRET: &[u8] = b"ZEBRA-7731";

/// Project P: `p1` (a doc with a secret block) tagged v1.0.0, `p2` tagged v2.0.0, an open view
/// `draft`, the removal of the secret `p3` tagged v3.0.0, the correction `p4`, and an upheld
/// dispute of `p1` that `p4` corrects. P is closed.
struct Source {
    dir: tempfile::TempDir,
    repo: RepoId,
    p1: Hash,
    p2: Hash,
    p3: Hash,
    p4: Hash,
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
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    std::fs::create_dir_all(&root).unwrap();
    let mut p = Rdy::init(&root, "rdy-standard").unwrap();
    let repo = p.read().repo_id().unwrap();
    let w = doc(&mut p, "work", "Intro", "Public words.");
    let secret = write_in(
        &mut p,
        "work",
        json!([{"op": "create", "type": "prose", "id": "$x", "parent": w.created["$d"], "body": "The launch code is ZEBRA-7731."}]),
    )
    .created["$x"]
        .clone();
    let p1 = land(&mut p, "work");
    tag(&mut p, "v1.0.0");
    doc(&mut p, "w1", "Later", "Words after the first version.");
    let p2 = land(&mut p, "w1");
    tag(&mut p, "v2.0.0");
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
    .unwrap();
    let p3 = land(&mut p, "redact");
    tag(&mut p, "v3.0.0");
    doc(&mut p, "w4", "Fix", "The corrected words.");
    let p4 = land(&mut p, "w4");
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
        .unwrap();
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
        .unwrap();
    p.seal_open().unwrap();
    p.close().unwrap();
    Source {
        dir,
        repo,
        p1,
        p2,
        p3,
        p4,
    }
}

/// Forks the closed project at `root` at tag `tag` into `dir`, through the open project's state.
fn fork_at(root: &Path, tag: &str, dir: &Path) -> Result<CloneSummary, RdyError> {
    let r = Rdy::open(root).unwrap();
    let st = r.read();
    let out = clone_at(CloneSource::State(&st), dir, true, Some(tag));
    drop(st);
    r.close().unwrap();
    out
}

/// The changes of the source's later landings that the fork `f` stores: evidence only.
fn evidence(s: &Source, f: &Rdy) -> Vec<Hash> {
    let reader = f.store().reader();
    let mut out = vec![];
    for l in [s.p2, s.p3, s.p4] {
        if !reader.has_kind(kinds::LANDING, &l).unwrap() {
            continue;
        }
        for c in reader.landing(&l).unwrap().changes {
            if reader.has_kind(kinds::CHANGE, &c).unwrap() {
                out.push(c);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// the project form

/// D-99CWC: the fork holds canon at the tag's landing and only that chain's tags. The later
/// landings travel as proof records and are no read root. The secret stays removed. The fork
/// has a new repo id, its origin names the source, the tag and its landing, and it holds no
/// source view, held chain, replaced mark or sync state. Its default view is fresh.
#[test]
fn hfp3_project_fork_keeps_only_tag_history() {
    let _clock = pin_clock(AT);
    let s = source();
    let before = {
        let r = Rdy::open(&s.root()).unwrap();
        let out = (r.store().log_end(), all_refs(&r));
        r.close().unwrap();
        out
    };
    let dir = s.at("f");
    let out = fork_at(&s.root(), "v1.0.0", &dir).unwrap();

    // the reply: a fork, from the source at the selected tag and its landing
    assert!(out.fork);
    assert!(out.origin.is_none());
    let from = out.forked_from.as_ref().expect("forked_from");
    assert_eq!(from.repo_id, s.repo.to_string());
    assert_eq!(from.tag.as_deref(), Some("v1.0.0"));
    assert_eq!(from.landing, Some(s.p1.to_string()));
    assert_ne!(out.repo_id, s.repo.to_string());
    assert_eq!(out.load.project, dir.display().to_string());
    assert_eq!(
        (out.load.landings, out.load.tags, out.load.views),
        (1, 1, 0)
    );
    assert!(out.load.files_written.contains(&".rdy.yaml".to_string()));

    // the source is unchanged
    let r = Rdy::open(&s.root()).unwrap();
    assert_eq!((r.store().log_end(), all_refs(&r)), before);
    r.close().unwrap();

    // the fork's shape holds after a reopen, after a reindex, and in a dump's load
    let mut f = Rdy::open(&dir).unwrap();
    fork_shape(&s, &f, &out.repo_id);
    f.reindex().unwrap();
    fork_shape(&s, &f, &out.repo_id);
    let file = s.at("f.rdydump");
    f.read().dump(&file).unwrap();
    f.close().unwrap();
    let loaded = s.at("loaded");
    rdy_engine::load::load(&file, &loaded).unwrap();
    let l = Rdy::open(&loaded).unwrap();
    fork_shape(&s, &l, &out.repo_id);
    l.close().unwrap();
    assert!(!bytes_hold(&loaded.join(".rdy"), SECRET));

    // the default view is fresh: the first write makes it, on the fork's canon
    let mut f = Rdy::open(&dir).unwrap();
    doc(&mut f, "work", "New", "Fresh work.");
    let head = f.read().view_head("work").cloned().unwrap();
    assert_eq!(head.base, Some(s.p1));
    assert_eq!(f.read().included("work").len(), 1);
    f.close().unwrap();
}

/// The fork `f` of `s` at v1.0.0, with repo id `repo`: see `hfp3_project_fork_keeps_only_tag_history`.
fn fork_shape(s: &Source, f: &Rdy, repo: &str) {
    let reader = f.store().reader();
    assert_eq!(reader.repo_id().unwrap().unwrap().to_string(), repo);
    assert_eq!(
        reader.origin().unwrap(),
        Some(Origin {
            fork: true,
            repo_id: s.repo,
            tag: Some("v1.0.0".into()),
            landing: Some(s.p1),
        })
    );
    assert_eq!(chain(f), [s.p1]);
    assert_eq!(ref_names(f, "tag/"), ["tag/v1.0.0"]);
    for prefix in ["view/", "held/", "replaced/", "meta/sync/"] {
        assert!(ref_names(f, prefix).is_empty(), "{prefix}");
    }
    // the later landings are stored, and none is on canon or tagged
    for l in [s.p2, s.p3, s.p4] {
        assert!(reader.has_kind(kinds::LANDING, &l).unwrap(), "{l}");
    }
    let ev = evidence(s, f);
    assert!(!ev.is_empty());
    for c in &ev {
        assert_eq!(reader.landing_of(c).unwrap(), None, "{c}");
    }
    let names: Vec<String> = f
        .read()
        .versions()
        .unwrap()
        .into_iter()
        .map(|v| v.name)
        .collect();
    assert_eq!(names, ["v1.0.0"]);
    // the dispute of v1.0.0 comes with it, and the secret stays removed
    assert_eq!(
        reader
            .disputes_of(&s.p1, f.store().log_end())
            .unwrap()
            .len(),
        1
    );
    drop(reader);
    f.fsck().unwrap();
    assert!(!bytes_hold(&f.root().join(".rdy"), SECRET));
}

// ---------------------------------------------------------------------------------------------
// signatures (a software ed25519 signer from a fixed seed, test only)

fn signing_key() -> ed25519_dalek::SigningKey {
    ed25519_dalek::SigningKey::from_bytes(&[0x62; 32])
}

fn key_data() -> KeyData {
    KeyData::Ed25519(Ed25519PublicKey(signing_key().verifying_key().to_bytes()))
}

fn key_blob() -> Vec<u8> {
    let mut blob = vec![];
    key_data().encode(&mut blob).unwrap();
    blob
}

fn sshsig(ns: &str, msg: &[u8]) -> Vec<u8> {
    let data = SshSig::signed_data(ns, HashAlg::Sha512, msg).unwrap();
    let sig = ssh_key::Signature::new(
        Algorithm::Ed25519,
        signing_key().sign(&data).to_bytes().to_vec(),
    )
    .unwrap();
    let mut out = vec![];
    SshSig::new(key_data(), ns, HashAlg::Sha512, sig)
        .unwrap()
        .encode(&mut out)
        .unwrap();
    out
}

/// The signature of landing `l` (record `rec`) as made in repo `repo`.
fn signed(l: Hash, rec: &LandingRecord, repo: RepoId) -> Signature {
    let stmt = statement(
        StatementSource::Approval(rec),
        Some(repo),
        &blob_fingerprint(&key_blob()),
    )
    .unwrap();
    let ns = parse_statement(&stmt).unwrap().namespace().to_string();
    Signature {
        subject: l,
        namespace: ns.clone(),
        sig: sshsig(&ns, &stmt),
        statement: stmt,
        signed_by: person(7),
    }
}

/// Each signature of `l` in `r`: the repo its statement names, and its statement verdict.
fn verdicts(r: &Rdy, l: &Hash) -> Vec<(String, String)> {
    let st = r.read();
    let mut out: Vec<(String, String)> = signature_checks(&st, l, None)
        .unwrap()
        .iter()
        .zip(r.store().reader().signatures_of(l).unwrap())
        .map(|(c, id)| {
            let g = r.store().reader().signature(&id).unwrap();
            let p = parse_statement(&g.statement).unwrap();
            (p.field("repo").unwrap().to_string(), c.statement.clone())
        })
        .collect();
    out.sort();
    out
}

fn sorted(mut ids: Vec<RepoId>) -> Vec<RepoId> {
    ids.sort();
    ids
}

/// D-99CWC, D-HX6BX: a project fork carries `.rdy.yaml`, `.rdyignore` and packs, never
/// `.rdy/local.yaml`. A fork of a project fork counts every ancestor origin, oldest first, and
/// its own last; a landing signed in any counted ancestor checks as a match.
#[test]
fn hfp3_fork_preserves_files_and_ancestor_origins() {
    let _clock = pin_clock(AT);
    let dir = tempfile::tempdir().unwrap();
    let p0 = dir.path().join("p0");
    std::fs::create_dir_all(&p0).unwrap();
    let mut r = Rdy::init(&p0, "rdy-standard").unwrap();
    let a = r.read().repo_id().unwrap();
    doc(&mut r, "work", "Intro", "Public words.");
    let l1 = land(&mut r, "work");
    tag(&mut r, "v1.0.0");
    let rec = r.store().reader().landing(&l1).unwrap();
    r.store_mut().put_signature(&signed(l1, &rec, a)).unwrap();
    r.seal_open().unwrap();
    // an ordinary fork of p0: a log copy with a new repo id
    let p = dir.path().join("p");
    let st = r.read();
    clone(CloneSource::State(&st), &p, true).unwrap();
    drop(st);
    r.close().unwrap();
    let mut r = Rdy::open(&p).unwrap();
    let b = r.read().repo_id().unwrap();
    r.store_mut().put_signature(&signed(l1, &rec, b)).unwrap();
    r.seal_open().unwrap();
    r.close().unwrap();
    std::fs::write(p.join(".rdyignore"), "drafts/\n").unwrap();
    std::fs::write(p.join(".rdy/local.yaml"), "{}\n").unwrap();
    let source_files = rdy_engine::dump::project_files(&p).unwrap();

    // the fork of p, and the fork of that fork
    let f1 = dir.path().join("f1");
    let out1 = fork_at(&p, "v1.0.0", &f1).unwrap();
    let f2 = dir.path().join("f2");
    let out2 = fork_at(&f1, "v1.0.0", &f2).unwrap();
    let c = out1.repo_id.parse::<RepoId>().unwrap();
    let from = out2.forked_from.as_ref().unwrap();
    assert_eq!(
        (
            from.repo_id.as_str(),
            from.tag.as_deref(),
            from.landing.clone()
        ),
        (out1.repo_id.as_str(), Some("v1.0.0"), Some(l1.to_string()))
    );

    let origin = |repo_id: RepoId| Origin {
        fork: true,
        repo_id,
        tag: Some("v1.0.0".into()),
        landing: Some(l1),
    };
    for (root, want) in [
        (&f1, vec![origin(a), origin(b)]),
        (&f2, vec![origin(a), origin(b), origin(c)]),
    ] {
        // the files, and never the local settings
        assert_eq!(rdy_engine::dump::project_files(root).unwrap(), source_files);
        assert!(!root.join(".rdy/local.yaml").exists());
        let r = Rdy::open(root).unwrap();
        let end = r.store().log_end();
        assert_eq!(
            r.store().reader().counted_fork_origins_before(end).unwrap(),
            want
        );
        // every counted ancestor's signature checks as a match
        let g = r.store().reader().signatures_of(&l1).unwrap();
        let g = r.store().reader().signature(&g[0]).unwrap();
        let cx = read_check_context(&r.read(), &g).unwrap();
        let lineage: Vec<RepoId> = want.iter().map(|o| o.repo_id).collect();
        assert_eq!(cx.source_repos, sorted(lineage));
        let mut matched = vec![
            (a.to_string(), "match".to_string()),
            (b.to_string(), "match".to_string()),
        ];
        matched.sort();
        assert_eq!(verdicts(&r, &l1), matched);
        r.fsck().unwrap();
        r.close().unwrap();
    }
}

// ---------------------------------------------------------------------------------------------
// evidence-only changes (D-99CWC): no include or replay takes one as a source

/// The fork of a new source at v1.0.0, open, with the source.
fn forked() -> (Source, Rdy) {
    let s = source();
    let dir = s.at("f");
    fork_at(&s.root(), "v1.0.0", &dir).unwrap();
    let f = Rdy::open(&dir).unwrap();
    (s, f)
}

/// What a refusal must leave as it was: the log's end and every ref.
fn snapshot(r: &Rdy) -> (rdy_store::segment::Location, Vec<(String, Vec<u8>)>) {
    (r.store().log_end(), all_refs(r))
}

/// D-99CWC: the refusal of evidence-only change `c`: the replay refusal's code, words and fix.
fn assert_no_change(e: &RdyError, c: &Hash) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_NOT_FOUND",
            format!("no change {}", c.short()).as_str(),
            "check the id with rdy view show <view>"
        ),
        "{e:?}"
    );
}

/// An owned view `name` with one write of its own, so it is open and owned.
fn owned(r: &mut Rdy, name: &str) {
    if !r.read().has_view(name) {
        doc(r, name, &format!("Own-{name}"), "Own words.");
    }
}

/// Each evidence change of `s` that `f` stores: a replay of it is `E_NOT_FOUND` and writes
/// nothing.
fn replay_refused(s: &Source, f: &mut Rdy) {
    let ev = evidence(s, f);
    assert!(!ev.is_empty());
    owned(f, "mine");
    let head = f.read().view_head("mine").unwrap().head;
    for c in &ev {
        // the bytes are here, as later evidence only
        assert!(f.store().reader().has_kind(kinds::CHANGE, c).unwrap());
        let before = snapshot(f);
        let e = f
            .view_replay(&writer(), "mine", &[c.to_string()])
            .unwrap_err();
        assert_no_change(&e, c);
        assert_eq!(snapshot(f), before, "nothing written");
        assert_eq!(f.read().view_head("mine").unwrap().head, head);
    }
}

/// D-99CWC: a change that the fork stores only through a later removal or dispute correction
/// cannot be replayed: `E_NOT_FOUND`, and nothing is written. The same holds after a reindex and
/// in a dump's load.
#[test]
fn hfp3_evidence_only_change_cannot_be_replayed() {
    let _clock = pin_clock(AT);
    let (s, mut f) = forked();
    replay_refused(&s, &mut f);
    f.reindex().unwrap();
    replay_refused(&s, &mut f);
    let file = s.at("f.rdydump");
    f.read().dump(&file).unwrap();
    f.close().unwrap();
    let mut f = Rdy::open(&s.at("f")).unwrap();
    replay_refused(&s, &mut f);
    f.close().unwrap();
    let loaded = s.at("loaded");
    rdy_engine::load::load(&file, &loaded).unwrap();
    let mut l = Rdy::open(&loaded).unwrap();
    replay_refused(&s, &mut l);
    l.close().unwrap();
}

/// Each evidence change of `s` that `f` stores: an include of it into an existing view, and
/// into a view that does not exist yet, is `E_NOT_FOUND` and writes nothing. A replay of it
/// after the refused include is still refused.
fn include_refused(s: &Source, f: &mut Rdy) {
    let ev = evidence(s, f);
    assert!(!ev.is_empty());
    owned(f, "into");
    for c in &ev {
        for view in ["into", "fresh"] {
            let before = snapshot(f);
            let e = f
                .view_include(&writer(), view, &[c.to_string()])
                .unwrap_err();
            assert_no_change(&e, c);
            assert_eq!(snapshot(f), before, "{view}: nothing written");
        }
        assert!(!f.read().has_view("fresh"));
        let e = f
            .view_replay(&writer(), "into", &[c.to_string()])
            .unwrap_err();
        assert_no_change(&e, c);
    }
}

/// D-99CWC: a named evidence-only change cannot be included, into an existing view or a new one.
/// Nothing is written, the new view is not made, and a replay still refuses the change. The same
/// holds after a reindex and in a dump's load.
#[test]
fn hfp3_evidence_only_change_cannot_be_included() {
    let _clock = pin_clock(AT);
    let (s, mut f) = forked();
    include_refused(&s, &mut f);
    f.reindex().unwrap();
    include_refused(&s, &mut f);
    let file = s.at("f.rdydump");
    f.read().dump(&file).unwrap();
    f.close().unwrap();
    let loaded = s.at("loaded");
    rdy_engine::load::load(&file, &loaded).unwrap();
    let mut l = Rdy::open(&loaded).unwrap();
    include_refused(&s, &mut l);
    l.close().unwrap();
}

/// Commits `ops` as one raw change to view `view`, with `current` the versions its ops read
/// (the store derives its deps from them); returns its id.
fn commit_raw(r: &mut Rdy, view: &str, ops: Vec<Op>, current: Vec<(BlockId, Hash)>) -> Hash {
    let change = Change {
        view: view.into(),
        ops,
        deps: vec![],
        provenance: writer().provenance(),
        justification: None,
        message: format!("hfp3 {view}"),
        links: ChangeLinks::default(),
        read_identity: None,
    };
    let at = |id: &BlockId| current.iter().find(|(b, _)| b == id).map(|(_, v)| *v);
    let ids = r
        .store_mut()
        .commit_many(view, vec![change], &at, &[], None, &writer().provenance())
        .unwrap();
    r.invalidate();
    ids[0]
}

/// The block of the first op of change `c` that creates one of type `ty`.
fn created(r: &Rdy, c: &Hash, ty: &str) -> Option<Block> {
    r.store()
        .reader()
        .change(c)
        .unwrap()
        .ops
        .into_iter()
        .find_map(|op| match op {
            Op::Create { block } if block.block_type == ty => Some(block),
            _ => None,
        })
}

/// An update of `block` to title `title`, on the version it has.
fn retitle(block: &Block, title: &str) -> (Op, Block) {
    let mut next = block.clone();
    next.read_identity = None;
    next.fields
        .insert("title".into(), FieldValue::Str(title.into()));
    (
        Op::Update {
            base: block.version_hash(),
            block: next.clone(),
        },
        next,
    )
}

/// D-99CWC: an include of an eligible change whose closure reaches an evidence-only change is
/// refused, naming that change: through a direct dep, through a transitive dep, and through the
/// change that creates a parent the include needs. Discovery grants nothing. Nothing is
/// written, into an existing view or a new one.
#[test]
fn hfp3_include_closure_refuses_evidence_only_change() {
    let _clock = pin_clock(AT);
    let (s, mut f) = forked();
    // `e`: an evidence-only change that creates a doc
    let (e, eblock) = evidence(&s, &f)
        .into_iter()
        .find_map(|c| created(&f, &c, "doc").map(|b| (c, b)))
        .expect("a later landing creates a doc");
    let (update, _) = retitle(&eblock, "Direct");
    let current = vec![(eblock.id.clone(), eblock.version_hash())];
    // direct: `a` updates the doc `e` made, so it needs `e`
    let a = commit_raw(&mut f, "a", vec![update], current.clone());
    assert_eq!(f.store().reader().change(&a).unwrap().deps, [e]);
    // transitive: `t1` updates `t2`'s version, and `t2` updates `e`'s
    let (update, t2block) = retitle(&eblock, "Second");
    let t2 = commit_raw(&mut f, "t2", vec![update], current.clone());
    let (update, _) = retitle(&t2block, "First");
    let t1 = commit_raw(
        &mut f,
        "t1",
        vec![update],
        vec![(t2block.id.clone(), t2block.version_hash())],
    );
    assert_eq!(f.store().reader().change(&t1).unwrap().deps, [t2]);
    // parent creator: `k` creates doc P and updates `e`'s doc; `c` puts prose under P alone
    let scratch = doc(&mut f, "scratch", "P", "Under P.");
    let id: Hash = scratch.change.parse().unwrap();
    let pdoc = created(&f, &id, "doc").unwrap();
    let prose = created(&f, &id, "prose").unwrap();
    f.view_exclude(
        &writer(),
        "scratch",
        std::slice::from_ref(&scratch.change),
        false,
    )
    .unwrap();
    let (update, _) = retitle(&eblock, "Creator");
    let k = commit_raw(
        &mut f,
        "k",
        vec![Op::Create { block: pdoc }, update],
        current,
    );
    assert_eq!(f.store().reader().change(&k).unwrap().deps, [e]);
    let c = commit_raw(&mut f, "c", vec![Op::Create { block: prose }], vec![]);
    assert!(f.store().reader().change(&c).unwrap().deps.is_empty());
    owned(&mut f, "into");
    for (name, source) in [("direct", a), ("transitive", t1), ("parent", c)] {
        for view in ["into", "fresh"] {
            let before = snapshot(&f);
            let err = f
                .view_include(&writer(), view, &[source.to_string()])
                .unwrap_err();
            assert_no_change(&err, &e);
            assert_eq!(snapshot(&f), before, "{name} into {view}: nothing written");
        }
        assert!(!f.read().has_view("fresh"));
    }
    // an eligible change whose closure is eligible is included as before
    let ok = doc(&mut f, "other", "Other", "Other words.").change;
    let out = f
        .view_include(&writer(), "into", std::slice::from_ref(&ok))
        .unwrap();
    assert_eq!(out.added, [ok]);
    f.close().unwrap();
}

/// D-99CWC: the evidence guard narrows nothing else. A replay takes a change from another open
/// view, an excluded change, a change of a closed view and a landed change; an include takes the
/// unlanded ones. In a fork, the tag's own changes replay as before.
#[test]
fn hfp3_replay_keeps_normal_view_sources() {
    let _clock = pin_clock(AT);
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("p");
    std::fs::create_dir_all(&root).unwrap();
    let mut r = Rdy::init(&root, "rdy-standard").unwrap();
    let landed = doc(&mut r, "work", "Landed", "Landed words.").change;
    land(&mut r, "work");
    let open = doc(&mut r, "va", "Open", "Open words.").change;
    let excluded = doc(&mut r, "vb", "Excluded", "Excluded words.").change;
    doc(&mut r, "vb", "Kept", "Kept words.");
    r.view_exclude(&writer(), "vb", std::slice::from_ref(&excluded), false)
        .unwrap();
    let closed = doc(&mut r, "vc", "Closed", "Closed words.").change;
    r.view_close(&writer(), &["vc".to_string()], "").unwrap();
    owned(&mut r, "mine");
    for c in [&open, &excluded, &closed, &landed] {
        let out = r
            .view_replay(&writer(), "mine", std::slice::from_ref(c))
            .unwrap();
        assert_eq!(out.replayed[0].source, *c);
    }
    for c in [&open, &excluded, &closed] {
        let out = r
            .view_include(&writer(), "t", std::slice::from_ref(c))
            .unwrap();
        assert!(out.added.contains(c));
    }
    r.close().unwrap();

    // a fork's tag changes are sources
    let (s, mut f) = forked();
    owned(&mut f, "mine");
    let reader = f.store().reader();
    let tagged: Vec<Hash> = reader
        .landing(&s.p1)
        .unwrap()
        .changes
        .into_iter()
        .filter(|c| {
            // the change that held the removed secret is a removed projection: not a source
            let change = reader.change(c).unwrap();
            reader.refuse_copy_of(&change).is_ok()
        })
        .collect();
    drop(reader);
    assert!(!tagged.is_empty());
    let out = f.view_replay(&writer(), "mine", &strings(&tagged)).unwrap();
    assert_eq!(out.replayed.len(), tagged.len());
    f.close().unwrap();
}

fn strings(hs: &[Hash]) -> Vec<String> {
    hs.iter().map(Hash::to_string).collect()
}
