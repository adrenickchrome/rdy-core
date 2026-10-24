//! hpb17 (D-91DZW, D-D2NPN, D-BPYN8, D-KDFZ3, D-VRW0A, D-QYCRB, D-Z8Q4N, D-075QD, D-WDKWQ): the
//! publication take through the adapter. Core checks the upload, resolves the requested profile
//! in the root tag's own snapshot over the upload and the holder, and hands the host's verdict
//! those facts before anything is appended. Every fact is compared with a fresh
//! `Snapshot::profiles` row read after the take; every core refusal comes before the verdict, and
//! a verdict that refuses or panics leaves the holder as it was and ready for the next call.
//!
//! The source projects are built with core calls; the holders are opened through the adapter.

use rdy_adapter::publication::{Audience, CheckStatus, ProfileFacts, ProfileSummary};
use rdy_adapter::publication_take::{
    PublicationFacts, PublicationRefusal, PublicationTaken, PublicationTarget,
};
use rdy_adapter::take::UploadTarget;
use rdy_adapter::{BlockId, Hash, RdyError, RepoId, StoreHandle};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Scope, Session, SessionKind, WriteRequest, stream_export};
use rdy_model::Role;
use rdy_store::closure::{self, END_OF_LOG, StoreSource};
use rdy_store::kinds;
use rdy_store::object_refs::{ObjectKey, ObjectRef};
use rdy_store::sync_proof::ObjectSource;
use rdy_store::sync_stream::{SYNC_KINDS, StreamHeader};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const CANARY: &str = "ZEBRA-7731";
const IMPLICIT_ORG: &str = "implicit:org";

// ---------------------------------------------------------------------------------------------
// source projects (core)

fn session(role: Role) -> Session {
    Session {
        actor: if role == Role::Approver {
            "human:founder".into()
        } else {
            "agent:a".into()
        },
        run: None,
        role,
        kind: SessionKind::Human,
    }
}

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: "work".into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "hpb17".into(),
        },
    )
    .unwrap()
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(
            &session(Role::Writer),
            "work",
            "hpb17",
            Some("test summary"),
        )
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn tag_msg(r: &mut Rdy, name: &str, message: Option<&str>) -> Hash {
    r.tag(
        &session(Role::Approver),
        name,
        None,
        message.map(str::to_string),
    )
    .unwrap();
    r.store()
        .tags()
        .unwrap()
        .into_iter()
        .find(|(n, _, _)| n == name)
        .unwrap()
        .1
}

fn tag(r: &mut Rdy, name: &str) -> Hash {
    tag_msg(r, name, None)
}

fn landing_of(r: &Rdy, tag: Hash) -> Hash {
    r.store().read_tag(&tag).unwrap().landing
}

fn repo(r: &Rdy) -> RepoId {
    r.store().repo_id().unwrap().unwrap()
}

fn bid(w: &rdy_engine::WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

/// Records one removal request of `targets` and lands it with a person's approval; returns the
/// request.
fn redact(r: &mut Rdy, targets: &[String]) -> Hash {
    let s = session(Role::Writer);
    let res = r
        .redact_in(
            &s,
            "redact",
            RedactInput {
                targets: targets.to_vec(),
                reason_class: "secret".into(),
                reason: "a leaked code".into(),
                dry_run: false,
            },
        )
        .unwrap();
    let request = res.redaction.expect("a request is recorded");
    let p = r
        .propose(&s, "redact", "Remove", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
    request
}

/// The source of most cases, tagged `v1.0.0`: doc `a` (a prose block with the canary, a figure
/// and a prose block that references doc `b`), doc `b`, and under doc `admin` three profiles:
/// `site` (org, selects a: one excluded reference, a warning), `press` (public, selects b:
/// clear) and `leak` (public, selects a: blocked).
struct Src {
    r: Rdy,
    a: BlockId,
    ap: BlockId,
    b: BlockId,
    blob: Hash,
    site: BlockId,
    press: BlockId,
    leak: BlockId,
    admin: BlockId,
    v1: Hash,
}

fn src(dir: &Path) -> Src {
    std::fs::create_dir_all(dir).unwrap();
    let mut r = Rdy::init(dir, "rdy-standard").unwrap();
    let blob = r.store_mut().put_blob(b"figure bytes").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": format!("The launch code is {CANARY}.")},
            {"op": "create", "type": "figure", "id": "$fa", "parent": "$a", "fields": {"blob": blob.to_string(), "media_type": "application/octet-stream", "alt": "fig"}},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$bp", "parent": "$b", "body": "Text of b."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, ap, b, admin) = (
        bid(&w, "$a"),
        bid(&w, "$ap"),
        bid(&w, "$b"),
        bid(&w, "$admin"),
    );
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "prose", "parent": a.to_string(), "body": format!("See {{{{{b}}}}}.")},
            {"op": "create", "type": "publication_profile", "id": "$site", "parent": admin.to_string(),
                "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a.to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$press", "parent": admin.to_string(),
                "fields": {"key": "press", "title": "Press", "audience": "public", "select": [b.to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$leak", "parent": admin.to_string(),
                "fields": {"key": "leak", "title": "Leak", "audience": "public", "select": [a.to_string()]}}
        ]),
    );
    let (site, press, leak) = (bid(&w, "$site"), bid(&w, "$press"), bid(&w, "$leak"));
    land(&mut r);
    let v1 = tag(&mut r, "v1.0.0");
    Src {
        r,
        a,
        ap,
        b,
        blob,
        site,
        press,
        leak,
        admin,
        v1,
    }
}

// ---------------------------------------------------------------------------------------------
// uploads

/// The whole history of `tag` as one batch, as core's export writes it.
fn export(r: &Rdy, tag: Hash) -> Vec<u8> {
    let mut out = Vec::new();
    stream_export::write(&r.read(), tag, &BTreeSet::new(), &mut out).unwrap();
    out
}

/// The closure of TAG `tag` in `r` less the records of `held`, as `n` batch files: record `i`
/// of the closure order goes to file `i % n`. Each file's prerequisites are exactly the keys
/// its records need from the other files and the holder, plus the root when it lacks it.
fn split(r: &Rdy, tag: Hash, held: &BTreeSet<ObjectKey>, n: usize) -> Vec<Vec<u8>> {
    let reader = r.store().reader();
    let c = closure::closure(&reader, tag, held).unwrap();
    let all: BTreeSet<ObjectKey> = c.records.iter().chain(held).copied().collect();
    let root = ObjectKey {
        kind: kinds::TAG,
        hash: tag,
    };
    let mut source = StoreSource {
        reader: &reader,
        end: END_OF_LOG,
    };
    let resolve = |l: ObjectRef| match l.kind {
        Some(kind) => ObjectKey { kind, hash: l.hash },
        None => SYNC_KINDS
            .iter()
            .map(|&kind| ObjectKey { kind, hash: l.hash })
            .find(|k| all.contains(k))
            .unwrap(),
    };
    (0..n)
        .map(|part| {
            let records: Vec<ObjectKey> = c
                .records
                .iter()
                .enumerate()
                .filter(|(i, _)| i % n == part)
                .map(|(_, k)| *k)
                .collect();
            let mine: BTreeSet<ObjectKey> = records.iter().copied().collect();
            let mut needed = BTreeSet::new();
            for k in &records {
                let rec = source.record(*k).unwrap().unwrap();
                for l in rec.refs().unwrap() {
                    needed.insert(resolve(l));
                }
                needed.extend(rec.proof());
            }
            needed.insert(root);
            needed.retain(|k| !mine.contains(k));
            let header = StreamHeader::tag_history(repo(r), tag, needed.into_iter().collect());
            closure::write_batch(&mut source, &header, &records, Vec::new())
                .unwrap()
                .0
        })
        .collect()
}

/// The keys of the whole closure of `tag` in `r`.
fn closure_keys(r: &Rdy, tag: Hash) -> BTreeSet<ObjectKey> {
    closure::closure(&r.store().reader(), tag, &BTreeSet::new())
        .unwrap()
        .records
        .into_iter()
        .collect()
}

fn paths(dir: &Path, name: &str, batches: &[&Vec<u8>]) -> Vec<PathBuf> {
    batches
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let p = dir.join(format!("{name}-{i}.rdysync"));
            std::fs::write(&p, b).unwrap();
            p
        })
        .collect()
}

fn files(dir: &Path, name: &str, batches: &[&Vec<u8>]) -> Vec<File> {
    paths(dir, name, batches)
        .iter()
        .map(|p| File::open(p).unwrap())
        .collect()
}

fn upload(tag: &str, landing: Hash) -> UploadTarget {
    UploadTarget {
        tag: tag.into(),
        landing,
    }
}

fn target(tag: &str, landing: Hash, profile: &str) -> PublicationTarget {
    PublicationTarget {
        upload: upload(tag, landing),
        profile: profile.into(),
    }
}

// ---------------------------------------------------------------------------------------------
// holders and verdicts

fn segments(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(root.join(".rdy/store/segments")).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("seg-") && name.ends_with(".log") {
            out.insert(name, std::fs::read(e.path()).unwrap());
        }
    }
    out
}

fn holds_canary(root: &Path) -> bool {
    segments(root)
        .values()
        .any(|b| b.windows(CANARY.len()).any(|w| w == CANARY.as_bytes()))
}

/// The holder's log bytes and its tags (hash and name), as a fresh snapshot reads them.
type State = (BTreeMap<String, Vec<u8>>, Vec<(Hash, String)>);

fn state(root: &Path, h: &StoreHandle) -> State {
    let tags = h
        .snapshot()
        .unwrap()
        .tags()
        .unwrap()
        .into_iter()
        .map(|(hash, t)| (hash, t.name))
        .collect();
    (segments(root), tags)
}

/// A host's own refusal: typed, not `Clone`, never serialized.
#[derive(Debug, PartialEq, Eq)]
struct HostRefusal {
    reason: String,
}

/// What a verdict saw: how often it ran and the facts it was given last.
#[derive(Clone, Default)]
struct Seen {
    calls: Arc<AtomicUsize>,
    facts: Arc<Mutex<Option<PublicationFacts>>>,
}

impl Seen {
    /// A verdict that records its facts and answers `answer`.
    fn verdict(
        &self,
        answer: Result<(), HostRefusal>,
    ) -> impl FnOnce(&PublicationFacts) -> Result<(), HostRefusal> + Send + 'static + use<> {
        let seen = self.clone();
        move |f: &PublicationFacts| {
            seen.calls.fetch_add(1, Ordering::SeqCst);
            *seen.facts.lock().unwrap() = Some(f.clone());
            answer
        }
    }

    fn pass(
        &self,
    ) -> impl FnOnce(&PublicationFacts) -> Result<(), HostRefusal> + Send + 'static + use<> {
        self.verdict(Ok(()))
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn last(&self) -> PublicationFacts {
        self.facts.lock().unwrap().clone().expect("the verdict ran")
    }
}

fn taken(r: Result<PublicationTaken, PublicationRefusal<HostRefusal>>) -> PublicationTaken {
    match r {
        Ok(t) => t,
        Err(PublicationRefusal::Core(e)) => panic!("core refused: {e:?}"),
        Err(PublicationRefusal::Verdict(e)) => panic!("the verdict refused: {e:?}"),
    }
}

fn core_err(r: Result<PublicationTaken, PublicationRefusal<HostRefusal>>) -> RdyError {
    match r {
        Err(PublicationRefusal::Core(e)) => e,
        Ok(t) => panic!("taken: {:?}", t.facts),
        Err(PublicationRefusal::Verdict(e)) => panic!("the verdict refused: {e:?}"),
    }
}

/// The row of `profiles(tag)` whose selector is `selector`, read from a fresh snapshot.
fn row(h: &StoreHandle, tag: &str, selector: &str) -> ProfileSummary {
    h.snapshot()
        .unwrap()
        .profiles(tag)
        .unwrap()
        .into_iter()
        .find(|p| p.selector == selector)
        .unwrap_or_else(|| panic!("no profile {selector} at {tag}"))
}

/// Every profile fact equals the row's.
fn assert_row(f: &ProfileFacts, row: &ProfileSummary) {
    assert_eq!(f.selector, row.selector);
    assert_eq!(f.id, row.id, "{}", f.selector);
    assert_eq!(f.version, row.version, "{}", f.selector);
    assert_eq!(f.key, row.key, "{}", f.selector);
    assert_eq!(f.title, row.title, "{}", f.selector);
    assert_eq!(f.audience, row.audience, "{}", f.selector);
    assert_eq!(f.trail, row.trail, "{}", f.selector);
    assert_eq!(f.selection, row.selection, "{}", f.selector);
    assert_eq!(f.selection_hash, row.selection_hash, "{}", f.selector);
    assert_eq!(
        f.selection_identity, row.selection_identity,
        "{}",
        f.selector
    );
    assert_eq!(f.references, row.references, "{}", f.selector);
}

// ---------------------------------------------------------------------------------------------
// tests

#[test]
fn hpb17_empty_holder_resolves_an_upload_only_profile() {
    let dir = tempfile::tempdir().unwrap();
    let s = src(&dir.path().join("src"));
    let l1 = landing_of(&s.r, s.v1);
    let root = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&root, repo(&s.r)).unwrap();
    let seen = Seen::default();
    let press = s.press.to_string();
    let got = taken(h.take_publication(
        &files(dir.path(), "up", &[&export(&s.r, s.v1)]),
        target("v1.0.0", l1, &press),
        seen.pass(),
    ));
    assert_eq!(seen.calls(), 1);
    // the verdict saw the facts the take returns
    assert_eq!(seen.last(), got.facts);
    let f = &got.facts;
    assert_eq!(
        (f.repo_id, f.tag.as_str(), f.tag_hash, f.landing),
        (repo(&s.r), "v1.0.0", s.v1, l1)
    );
    assert_eq!(
        (
            got.outcome.tag.as_str(),
            got.outcome.tag_hash,
            got.outcome.landing,
            got.outcome.unchanged
        ),
        ("v1.0.0", s.v1, l1, false)
    );
    let p = f.profile.as_ref().expect("the profile");
    assert_row(p, &row(&h, "v1.0.0", &press));
    assert_eq!(
        (
            p.id.as_ref(),
            p.key.as_deref(),
            p.title.as_str(),
            p.audience,
            p.references
        ),
        (
            Some(&s.press),
            Some("press"),
            "Press",
            Audience::Public,
            CheckStatus::Clear
        )
    );
    // the selection is doc b and its prose, by id, at their source versions
    let src_m = s.r.read().materialized(&Scope::At(l1)).unwrap();
    let ids: Vec<BlockId> = p.selection.iter().map(|e| e.id.clone()).collect();
    assert!(ids.windows(2).all(|w| w[0] < w[1]), "id order");
    assert!(ids.contains(&s.b));
    for e in &p.selection {
        assert_eq!(Some(e.version), src_m.version_of(&e.id));
        assert_eq!(e.redaction, None);
    }
    assert_eq!(p.version, src_m.version_of(&s.press));
    h.close().unwrap();
}

#[test]
fn hpb17_mixed_holder_and_upload_resolve_the_pinned_revision() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = src(&dir.path().join("src"));
    let site = s.site.to_string();
    write(
        &mut s.r,
        json!([{"op": "update", "id": site, "fields": {"title": "Site 2", "select": [s.a.to_string(), s.b.to_string()]}}]),
    );
    land(&mut s.r);
    let v2 = tag(&mut s.r, "v2.0.0");
    write(
        &mut s.r,
        json!([{"op": "update", "id": site, "fields": {"title": "Site 3", "select": [s.b.to_string()]}}]),
    );
    land(&mut s.r);
    let v3 = tag(&mut s.r, "v3.0.0");
    let (l1, l2, l3) = (
        landing_of(&s.r, s.v1),
        landing_of(&s.r, v2),
        landing_of(&s.r, v3),
    );
    let root = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&root, repo(&s.r)).unwrap();
    h.take(
        &files(dir.path(), "v1", &[&export(&s.r, s.v1)]),
        upload("v1.0.0", l1),
    )
    .unwrap();

    // v2's dependencies: some held (v1's history), the rest split over two batch files
    let held = closure_keys(&s.r, s.v1);
    let parts = split(&s.r, v2, &held, 2);
    let up = paths(dir.path(), "v2", &[&parts[1], &parts[0]]);
    let open = || {
        up.iter()
            .map(|p| File::open(p).unwrap())
            .collect::<Vec<_>>()
    };
    let seen = Seen::default();
    let first = taken(h.take_publication(&open(), target("v2.0.0", l2, &site), seen.pass()));
    assert!(!first.outcome.unchanged);
    let p = first.facts.profile.clone().expect("the profile");
    assert_eq!(p.title, "Site 2");
    assert_row(&p, &row(&h, "v2.0.0", &site));
    let v2_m = s.r.read().materialized(&Scope::At(l2)).unwrap();
    assert_eq!(p.version, v2_m.version_of(&s.site));

    // a later revision of the profile, taken since, does not change what v2's tag selects
    h.take(
        &files(dir.path(), "v3", &[&export(&s.r, v3)]),
        upload("v3.0.0", l3),
    )
    .unwrap();
    assert_eq!(row(&h, "v3.0.0", &site).title, "Site 3");
    let again = taken(h.take_publication(&open(), target("v2.0.0", l2, &site), seen.pass()));
    assert!(again.outcome.unchanged);
    assert_eq!(again.facts, first.facts);
    assert_row(
        again.facts.profile.as_ref().unwrap(),
        &row(&h, "v2.0.0", &site),
    );
    assert_eq!(seen.calls(), 2);
    h.close().unwrap();
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let t = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &t);
        } else {
            std::fs::copy(e.path(), t).unwrap();
        }
    }
}

#[test]
fn hpb17_existing_equivalent_tag_supplies_surviving_facts() {
    let dir = tempfile::tempdir().unwrap();
    let s = src(&dir.path().join("a"));
    let site = s.site.to_string();
    s.r.close().unwrap();
    copy_dir(&dir.path().join("a"), &dir.path().join("b"));
    let mut a = Rdy::open(&dir.path().join("a")).unwrap();
    let mut b = Rdy::open(&dir.path().join("b")).unwrap();
    let ta = tag_msg(&mut a, "v1.2.0", Some("first"));
    let tb = tag_msg(&mut b, "1.2.0", Some("second"));
    assert_ne!(ta, tb);
    let l = landing_of(&a, ta);
    assert_eq!(landing_of(&b, tb), l);
    let root = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&root, repo(&a)).unwrap();
    h.take(
        &files(dir.path(), "a", &[&export(&a, ta)]),
        upload("v1.2.0", l),
    )
    .unwrap();

    // the incoming 1.2.0 names the same landing: the held v1.2.0 survives, by its name and hash
    let seen = Seen::default();
    let got = taken(h.take_publication(
        &files(dir.path(), "b", &[&export(&b, tb)]),
        target("1.2.0", l, &site),
        seen.pass(),
    ));
    assert_eq!(seen.last(), got.facts);
    assert_eq!(
        (
            got.facts.tag.as_str(),
            got.facts.tag_hash,
            got.facts.landing
        ),
        ("v1.2.0", ta, l)
    );
    assert_eq!(
        (got.outcome.tag.as_str(), got.outcome.tag_hash),
        ("v1.2.0", ta)
    );
    assert_row(
        got.facts.profile.as_ref().unwrap(),
        &row(&h, "v1.2.0", &site),
    );
    let tags: Vec<String> = h
        .snapshot()
        .unwrap()
        .tags()
        .unwrap()
        .into_iter()
        .map(|(_, t)| t.name)
        .collect();
    assert_eq!(tags, ["v1.2.0"]);
    h.close().unwrap();
}

#[test]
fn hpb17_explicit_and_implicit_facts_equal_profiles_after_take() {
    let dir = tempfile::tempdir().unwrap();
    let s = src(&dir.path().join("src"));
    let l1 = landing_of(&s.r, s.v1);
    let up = paths(dir.path(), "up", &[&export(&s.r, s.v1)]);
    let open = || {
        up.iter()
            .map(|p| File::open(p).unwrap())
            .collect::<Vec<_>>()
    };
    let root = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&root, repo(&s.r)).unwrap();
    let seen = Seen::default();
    let mut facts = Vec::new();
    for selector in [
        IMPLICIT_ORG.to_string(),
        s.site.to_string(),
        s.press.to_string(),
    ] {
        let got = taken(h.take_publication(&open(), target("v1.0.0", l1, &selector), seen.pass()));
        assert_eq!(seen.last(), got.facts);
        facts.push(got.facts.profile.expect("a profile"));
    }
    assert_eq!(seen.calls(), 3);
    let rows = h.snapshot().unwrap().profiles("v1.0.0").unwrap();
    for f in &facts {
        let r = rows.iter().find(|r| r.selector == f.selector).unwrap();
        assert_row(f, r);
    }
    // implicit:org: no id, version or key; org with a trail; the holder's own title
    let implicit = &facts[0];
    assert_eq!(
        (
            implicit.selector.as_str(),
            implicit.id.as_ref(),
            implicit.version,
            implicit.key.as_ref(),
            implicit.audience,
            implicit.trail
        ),
        (IMPLICIT_ORG, None, None, None, Audience::Org, true)
    );
    assert_eq!(implicit.title, rows[0].title);
    // the explicit org profile: its key, its id as the selector, and its reference warning
    let site = &facts[1];
    assert_eq!(
        (
            site.selector.clone(),
            site.key.as_deref(),
            site.audience,
            site.trail,
            site.references
        ),
        (
            s.site.to_string(),
            Some("site"),
            Audience::Org,
            true,
            CheckStatus::Warning
        )
    );
    h.close().unwrap();
}

#[test]
fn hpb17_missing_profile_is_a_host_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let s = src(&dir.path().join("src"));
    let l1 = landing_of(&s.r, s.v1);
    let up = paths(dir.path(), "up", &[&export(&s.r, s.v1)]);
    let open = || {
        up.iter()
            .map(|p| File::open(p).unwrap())
            .collect::<Vec<_>>()
    };
    // a well-formed full id that no block of the tag has
    let m = s.r.read().materialized(&Scope::At(l1)).unwrap();
    let missing = ["ZZZZZ", "ZZZZY", "ZZZZX"]
        .iter()
        .map(|body| format!("{}-{body}", s.site.prefix()))
        .find(|id| m.get(&BlockId::parse(id).unwrap()).is_none())
        .unwrap();
    let root = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&root, repo(&s.r)).unwrap();
    let before = state(&root, &h);

    // the verdict sees no profile, and its own typed refusal comes back unchanged
    let seen = Seen::default();
    let got = h.take_publication(
        &open(),
        target("v1.0.0", l1, &missing),
        seen.verdict(Err(HostRefusal {
            reason: "the site's profile is not in this version".into(),
        })),
    );
    match got {
        Err(PublicationRefusal::Verdict(e)) => assert_eq!(
            e,
            HostRefusal {
                reason: "the site's profile is not in this version".into()
            }
        ),
        Err(PublicationRefusal::Core(e)) => panic!("core refused: {e:?}"),
        Ok(t) => panic!("taken: {:?}", t.facts),
    }
    assert_eq!(seen.calls(), 1);
    let f = seen.last();
    assert_eq!(f.profile, None);
    assert_eq!(
        (f.tag.as_str(), f.tag_hash, f.landing),
        ("v1.0.0", s.v1, l1)
    );
    assert_eq!(state(&root, &h), before);

    // a host that accepts it: the upload is taken with no profile facts
    let got = taken(h.take_publication(&open(), target("v1.0.0", l1, &missing), seen.pass()));
    assert_eq!(got.facts.profile, None);
    assert_eq!(seen.calls(), 2);
    assert_eq!(state(&root, &h).1, vec![(s.v1, "v1.0.0".to_string())]);
    h.close().unwrap();
}

#[test]
fn hpb17_core_refusals_precede_the_verdict() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut s = src(&dir.path().join("src"));
    // a profile whose selector names a prose block, and a profile whose content is removed
    let w = write(
        &mut s.r,
        json!([
            {"op": "create", "type": "publication_profile", "id": "$bad", "parent": s.admin.to_string(),
                "fields": {"key": "bad", "title": "Bad", "audience": "org", "select": [s.ap.to_string()]}},
            {"op": "create", "type": "publication_profile", "id": "$gone", "parent": s.admin.to_string(),
                "fields": {"key": "gone", "title": "Gone", "audience": "org", "select": [s.b.to_string()]}}
        ]),
    );
    let (bad, gone) = (bid(&w, "$bad"), bid(&w, "$gone"));
    land(&mut s.r);
    redact(&mut s.r, &[gone.to_string()]);
    let v2 = tag(&mut s.r, "v2.0.0");
    let l2 = landing_of(&s.r, v2);
    let up = paths(dir.path(), "up", &[&export(&s.r, v2)]);
    let open = || {
        up.iter()
            .map(|p| File::open(p).unwrap())
            .collect::<Vec<_>>()
    };
    let root = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&root, repo(&s.r)).unwrap();
    let before = state(&root, &h);
    let seen = Seen::default();

    let unstable = (
        "E_BAD_INPUT",
        "A publication take names its profile by a full profile id or implicit:org.".to_string(),
        "Pass the site's profile id, or implicit:org.".to_string(),
    );
    let cases: Vec<(&str, String, (&str, String, String))> = vec![
        ("key", "site".into(), unstable.clone()),
        ("malformed", "not an id!".into(), unstable.clone()),
        ("too long", "x".repeat(129), unstable.clone()),
        (
            "implicit:public",
            "implicit:public".into(),
            (
                "E_BAD_INPUT",
                "the implicit profile is always org (FW-PB9a)".into(),
                "name an explicit publication profile whose audience is public".into(),
            ),
        ),
        (
            "unknown implicit",
            "implicit:nope".into(),
            (
                "E_BAD_INPUT",
                "Implicit profile \"implicit:nope\" is not known.".into(),
                "Use implicit:org.".into(),
            ),
        ),
        (
            "non-profile",
            s.a.to_string(),
            (
                "E_BAD_INPUT",
                format!("Block {} is not a publication profile.", s.a),
                "Use a publication profile id or key.".into(),
            ),
        ),
        (
            "invalid profile",
            bad.to_string(),
            (
                "E_BAD_INPUT",
                format!("Publication selector {} must name a doc or folder.", s.ap),
                "Select the containing doc or folder.".into(),
            ),
        ),
        (
            "redacted profile",
            gone.to_string(),
            (
                "E_BAD_INPUT",
                format!("Publication profile {gone} has been redacted."),
                "Choose a profile whose content is available.".into(),
            ),
        ),
        (
            "blocked public profile",
            s.leak.to_string(),
            (
                "E_PUBLICATION_REFERENCE",
                "Public profile has 1 references outside its selection.".into(),
                "Include the target docs or remove these references. Run the publication check again."
                    .into(),
            ),
        ),
    ];
    for (case, selector, (code, message, fix)) in cases {
        let e = core_err(h.take_publication(&open(), target("v2.0.0", l2, &selector), seen.pass()));
        assert_eq!(
            (e.code, e.message.as_str(), e.fix.as_str()),
            (code, message.as_str(), fix.as_str()),
            "{case}"
        );
        assert_eq!(seen.calls(), 0, "{case}: no verdict");
        assert_eq!(state(&root, &h), before, "{case}: nothing written");
    }

    // the take's own checks come before the selector's meaning: a wrong target with a key is
    // the take's refusal
    let e = core_err(h.take_publication(
        &open(),
        target(
            "v2.0.0",
            s.r.store().read_tag(&s.v1).unwrap().landing,
            "site",
        ),
        seen.pass(),
    ));
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            "E_BAD_RECORD",
            "upload target does not match the batch root tag"
        )
    );
    assert_eq!(seen.calls(), 0);

    // the same handle takes a valid publication next
    let got = taken(h.take_publication(
        &open(),
        target("v2.0.0", l2, &s.press.to_string()),
        seen.pass(),
    ));
    assert_eq!(seen.calls(), 1);
    assert_eq!(got.facts.tag_hash, v2);
    h.close().unwrap();
}

#[test]
fn hpb17_org_reference_warning_reaches_the_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let s = src(&dir.path().join("src"));
    let l1 = landing_of(&s.r, s.v1);
    let root = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&root, repo(&s.r)).unwrap();
    let seen = Seen::default();
    let site = s.site.to_string();
    let got = taken(h.take_publication(
        &files(dir.path(), "up", &[&export(&s.r, s.v1)]),
        target("v1.0.0", l1, &site),
        seen.pass(),
    ));
    let saw = seen.last().profile.expect("the profile");
    assert_eq!(saw.references, CheckStatus::Warning);
    assert_eq!(saw.audience, Audience::Org);
    assert_eq!(got.facts.profile.as_ref(), Some(&saw));
    assert!(!got.outcome.unchanged);
    assert_row(&saw, &row(&h, "v1.0.0", &site));
    h.close().unwrap();
}

#[test]
fn hpb17_holder_and_upload_removals_shape_the_same_facts() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut s = src(&dir.path().join("src"));
    let site = s.site.to_string();
    let l1 = landing_of(&s.r, s.v1);
    let before_v1 = export(&s.r, s.v1);
    let original =
        s.r.read()
            .materialized(&Scope::At(l1))
            .unwrap()
            .version_of(&s.ap)
            .unwrap();
    let removal = redact(&mut s.r, &[s.ap.to_string()]);
    let v2 = tag(&mut s.r, "v2.0.0");
    // made before the blob's removal, which a later export would carry too
    let v2_bytes = export(&s.r, v2);
    let blob_removal = redact(&mut s.r, &[format!("blob:{}", s.blob)]);
    let v3 = tag(&mut s.r, "v3.0.0");
    let (l2, l3) = (landing_of(&s.r, v2), landing_of(&s.r, v3));
    let entry = |f: &ProfileFacts| {
        f.selection
            .iter()
            .find(|e| e.id == s.ap)
            .cloned()
            .expect("the removed prose stays selected")
    };

    // a removal the holder already holds shapes an upload made before it
    let one = dir.path().join("one");
    let mut h = StoreHandle::create_holder(&one, repo(&s.r)).unwrap();
    h.take(&files(dir.path(), "v2", &[&v2_bytes]), upload("v2.0.0", l2))
        .unwrap();
    let seen = Seen::default();
    let got = taken(h.take_publication(
        &files(dir.path(), "v1", &[&before_v1]),
        target("v1.0.0", l1, &site),
        seen.pass(),
    ));
    let held = got.facts.profile.clone().unwrap();
    assert_row(&held, &row(&h, "v1.0.0", &site));
    let e = entry(&held);
    assert_eq!((e.version, e.redaction), (original, Some(removal)));
    assert!(!holds_canary(&one));
    h.close().unwrap();

    // approvals that arrive in the upload shape the facts the same way
    let two = dir.path().join("two");
    let mut h = StoreHandle::create_holder(&two, repo(&s.r)).unwrap();
    let got = taken(h.take_publication(
        &files(dir.path(), "v3", &[&export(&s.r, v3)]),
        target("v3.0.0", l3, &site),
        seen.pass(),
    ));
    let incoming = got.facts.profile.clone().unwrap();
    assert_row(&incoming, &row(&h, "v3.0.0", &site));
    let e = entry(&incoming);
    assert_eq!((e.version, e.redaction), (original, Some(removal)));
    // the identity binds ids and versions only; the hash moves with what was removed (the
    // figure's blob is unavailable now)
    assert_eq!(incoming.selection_identity, held.selection_identity);
    assert_ne!(incoming.selection_hash, held.selection_hash);
    // and now held, the same removals shape the older upload as in the first holder
    let got = taken(h.take_publication(
        &files(dir.path(), "v1b", &[&before_v1]),
        target("v1.0.0", l1, &site),
        seen.pass(),
    ));
    let older = got.facts.profile.unwrap();
    assert_row(&older, &row(&h, "v1.0.0", &site));
    assert_eq!(entry(&older), entry(&held));
    assert!(!holds_canary(&two));
    assert_ne!(blob_removal, removal);
    assert_eq!(seen.calls(), 3);
    h.close().unwrap();
}

/// Two figures whose blobs share their first chunk: one full store chunk (1 MiB) of the same
/// bytes, then one byte of their own.
fn shared_chunk_blobs() -> (Vec<u8>, Vec<u8>) {
    let head: Vec<u8> = (0..1u32 << 20).map(|i| (i % 251) as u8).collect();
    let (mut a, mut b) = (head.clone(), head);
    a.push(b'a');
    b.push(b'b');
    (a, b)
}

#[test]
fn hpb17_a_dropped_shared_chunk_shapes_the_facts() {
    // removing blob `a` drops its chunks, the shared one too: blob `b`'s manifest names no
    // removal, so only the chunk's drop makes it unavailable, by the holder's mask or by the
    // upload's approval
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let mut r = Rdy::init(&src, "rdy-standard").unwrap();
    let (a, b) = shared_chunk_blobs();
    let (ba, bb) = (
        r.store_mut().put_blob(&a).unwrap(),
        r.store_mut().put_blob(&b).unwrap(),
    );
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d", "title": "D", "doc_type": "page"}},
            {"op": "create", "type": "figure", "id": "$fa", "parent": "$d", "fields": {"blob": ba.to_string(), "media_type": "application/octet-stream", "alt": "a"}},
            {"op": "create", "type": "figure", "id": "$fb", "parent": "$d", "fields": {"blob": bb.to_string(), "media_type": "application/octet-stream", "alt": "b"}},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (d, admin) = (bid(&w, "$d"), bid(&w, "$admin"));
    let w = write(
        &mut r,
        json!([{"op": "create", "type": "publication_profile", "id": "$s", "parent": admin.to_string(),
            "fields": {"key": "site", "title": "Site", "audience": "org", "select": [d.to_string()]}}]),
    );
    let site = bid(&w, "$s").to_string();
    land(&mut r);
    let v1 = tag(&mut r, "v1.0.0");
    let before = export(&r, v1);
    redact(&mut r, &[format!("blob:{ba}")]);
    let v2 = tag(&mut r, "v2.0.0");
    let (l1, l2) = (landing_of(&r, v1), landing_of(&r, v2));

    // the approval arrives in the upload
    let one = dir.path().join("one");
    let mut h = StoreHandle::create_holder(&one, repo(&r)).unwrap();
    let seen = Seen::default();
    let got = taken(h.take_publication(
        &files(dir.path(), "v2", &[&export(&r, v2)]),
        target("v2.0.0", l2, &site),
        seen.pass(),
    ));
    let incoming = got.facts.profile.unwrap();
    assert_row(&incoming, &row(&h, "v2.0.0", &site));
    h.close().unwrap();

    // the holder holds the removal already, and the upload was made before it
    let two = dir.path().join("two");
    let mut h = StoreHandle::create_holder(&two, repo(&r)).unwrap();
    h.take(
        &files(dir.path(), "v2b", &[&export(&r, v2)]),
        upload("v2.0.0", l2),
    )
    .unwrap();
    let got = taken(h.take_publication(
        &files(dir.path(), "v1", &[&before]),
        target("v1.0.0", l1, &site),
        seen.pass(),
    ));
    let held = got.facts.profile.unwrap();
    assert_row(&held, &row(&h, "v1.0.0", &site));
    assert_eq!(held.selection_identity, incoming.selection_identity);
    assert_eq!(seen.calls(), 2);
    h.close().unwrap();
}

#[test]
fn hpb17_verdict_rejection_and_panic_leave_the_holder_reusable() {
    let _clock = rdy_engine::pin_clock(1_000_000);
    let dir = tempfile::tempdir().unwrap();
    let mut s = src(&dir.path().join("src"));
    let site = s.site.to_string();
    let l1 = landing_of(&s.r, s.v1);
    let before_v1 = export(&s.r, s.v1);
    redact(&mut s.r, &[s.ap.to_string()]);
    let v2 = tag(&mut s.r, "v2.0.0");
    let l2 = landing_of(&s.r, v2);
    let up = paths(dir.path(), "v2", &[&export(&s.r, v2)]);
    let open = || {
        up.iter()
            .map(|p| File::open(p).unwrap())
            .collect::<Vec<_>>()
    };
    let root = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&root, repo(&s.r)).unwrap();
    h.take(
        &files(dir.path(), "v1", &[&before_v1]),
        upload("v1.0.0", l1),
    )
    .unwrap();
    let old = h.snapshot().unwrap();
    let before = state(&root, &h);
    assert!(holds_canary(&root));
    // the removal epoch has not moved while an older snapshot still reads its content
    let unchanged = |h: &StoreHandle, case: &str| {
        assert_eq!(state(&root, h), before, "{case}");
        assert!(holds_canary(&root), "{case}");
        old.profiles("v1.0.0")
            .unwrap_or_else(|e| panic!("{case}: {e:?}"));
    };

    let seen = Seen::default();
    let got = h.take_publication(
        &open(),
        target("v2.0.0", l2, &site),
        seen.verdict(Err(HostRefusal {
            reason: "not today".into(),
        })),
    );
    match got {
        Err(PublicationRefusal::Verdict(e)) => assert_eq!(e.reason, "not today"),
        Err(PublicationRefusal::Core(e)) => panic!("core refused: {e:?}"),
        Ok(t) => panic!("taken: {:?}", t.facts),
    }
    unchanged(&h, "rejection");

    let panicked = h.take_publication(
        &open(),
        target("v2.0.0", l2, &site),
        |_: &PublicationFacts| -> Result<(), HostRefusal> { panic!("the host's verdict broke") },
    );
    let e = core_err(panicked);
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_INTERNAL",
            "the publication take's verdict panicked",
            "report this as a bug in the host"
        )
    );
    unchanged(&h, "panic");

    // the same handle takes it next, and only then does the removal apply
    let got = taken(h.take_publication(&open(), target("v2.0.0", l2, &site), seen.pass()));
    assert_eq!(seen.calls(), 2);
    assert!(!got.outcome.unchanged);
    assert!(!holds_canary(&root));
    assert_eq!(old.profiles("v1.0.0").unwrap_err().code, "E_REDACTED_STATE");
    h.fsck().unwrap();
    h.close().unwrap();
}

#[test]
fn hpb17_complete_retry_calls_the_verdict_once_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let s = src(&dir.path().join("src"));
    let l1 = landing_of(&s.r, s.v1);
    let site = s.site.to_string();
    let up = paths(dir.path(), "up", &[&export(&s.r, s.v1)]);
    let open = || {
        up.iter()
            .map(|p| File::open(p).unwrap())
            .collect::<Vec<_>>()
    };
    let root = dir.path().join("holder");
    let mut h = StoreHandle::create_holder(&root, repo(&s.r)).unwrap();
    let seen = Seen::default();
    let first = taken(h.take_publication(&open(), target("v1.0.0", l1, &site), seen.pass()));
    assert_eq!(seen.calls(), 1);
    assert!(!first.outcome.unchanged);
    let after_first = state(&root, &h);
    for round in 2..4 {
        let again = taken(h.take_publication(&open(), target("v1.0.0", l1, &site), seen.pass()));
        assert_eq!(seen.calls(), round, "one verdict per call");
        assert_eq!(again.facts, first.facts);
        assert_eq!(seen.last(), first.facts);
        assert_eq!(
            (
                again.outcome.unchanged,
                again.outcome.frames_written,
                again.outcome.records_taken
            ),
            (true, 0, 0)
        );
        assert_eq!(again.outcome.end, first.outcome.end, "the log end");
        assert_eq!(state(&root, &h), after_first);
    }
    h.close().unwrap();
}
