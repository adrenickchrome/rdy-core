//! rpl9 (repo links, D-DAPR8, D-C94DV, D-WDKWQ): the cited side exports one tag's filtered view as
//! a link delivery stream through the adapter, and lists the deliveries a tag uses. The selection
//! is the publication's own, so the expectations here come from the profile read of the same
//! snapshot; the stream is checked by the store's own delivery check.

use rdy_adapter::links::{DeliveryReport, TagUse};
use rdy_adapter::publication::ProfileSummary;
use rdy_adapter::stream::StreamPurpose;
use rdy_adapter::{BlockId, Hash, RepoId, StoreHandle};
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::pending::{CARD_MAX_TOKENS, ItemKind, PendingQuery};
use rdy_engine::redaction::RedactInput;
use rdy_engine::{Rdy, Session, SessionKind, WriteRequest};
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::{Block, FieldValue, Role};
use rdy_store::sync_stream::{StreamReader, check_delivery};
use rdy_store::{codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;

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

fn write_in(r: &mut Rdy, view: &str, ops: Value) -> rdy_engine::WriteResult {
    r.write(
        &session(Role::Writer),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: None,
            message: "rpl9".into(),
        },
    )
    .unwrap()
}

fn write(r: &mut Rdy, ops: Value) -> rdy_engine::WriteResult {
    write_in(r, "work", ops)
}

/// Confirms each exception beside version `p`, as the person answering for themselves.
fn confirm_exceptions(r: &mut Rdy, p: &rdy_engine::ProposalInfo) {
    let s = session(Role::Approver);
    let q = r
        .read()
        .pending(&PendingQuery {
            person: s.actor.clone(),
            session_ref: None,
            max_tokens: CARD_MAX_TOKENS,
        })
        .unwrap();
    let refs: Vec<String> = q
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::Confirm && i.batch.as_deref() == Some(p.r#ref.as_str()))
        .map(|i| i.r#ref.clone())
        .collect();
    for item in refs {
        r.answer(
            &s,
            &s.actor,
            AnswerRequest {
                r#ref: item.clone(),
                option: "confirm".into(),
                card_hash: None,
                note: None,
                relay: RelayInput::default(),
                brief: None,
            },
        )
        .unwrap_or_else(|e| panic!("confirm {item}: {e}"));
    }
}

fn land(r: &mut Rdy) {
    let p = r
        .propose(&session(Role::Writer), "work", "rpl9", Some("test summary"))
        .unwrap();
    confirm_exceptions(r, &p);
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn tag(r: &mut Rdy, name: &str) {
    r.tag(&session(Role::Approver), name, None, None).unwrap();
}

fn bid(w: &rdy_engine::WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

/// Records one removal request of `targets` and lands it with a person's approval.
fn redact(r: &mut Rdy, targets: &[String]) {
    let s = session(Role::Writer);
    r.redact_in(
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
    let p = r
        .propose(&s, "redact", "Remove", Some("test summary"))
        .unwrap();
    r.land(&session(Role::Approver), &p.id).unwrap();
}

fn row(rows: &[ProfileSummary], name: &str) -> ProfileSummary {
    rows.iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("the {name} profile"))
        .clone()
}

fn export(snap: &rdy_adapter::Snapshot, tag: &str, profile: &str) -> (DeliveryReport, Vec<u8>) {
    let mut out = Vec::new();
    let report = snap
        .link_export(tag, profile, &mut out)
        .unwrap_or_else(|e| panic!("export {profile} at {tag}: {e}"));
    (report, out)
}

/// A source project, closed again.
struct Source {
    dir: tempfile::TempDir,
    /// The prose under doc a.
    ap: BlockId,
    /// The prose under doc b, which no profile but `all` selects.
    bp: BlockId,
    /// The decision that the second decision supersedes.
    old: BlockId,
    new: BlockId,
}

/// v1.0.0: doc `a` (prose `ap`, two decisions where `new` supersedes `old`), doc `b` (prose
/// `bp`), profile `site` (org, selects a) and `all` (org, selects a and b). Then `ap` changes and
/// doc `c` lands untagged.
fn source() -> Source {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$a", "fields": {"name": "a", "title": "A", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$ap", "parent": "$a", "body": "Text of a."},
            {"op": "create", "type": "doc", "id": "$b", "fields": {"name": "b", "title": "B", "doc_type": "page"}},
            {"op": "create", "type": "prose", "id": "$bp", "parent": "$b", "body": "Text of b."},
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
        ]),
    );
    let (a, b, admin) = (bid(&w, "$a"), bid(&w, "$b"), bid(&w, "$admin"));
    let w2 = write(
        &mut r,
        json!([
            {"op": "create", "type": "publication_profile", "parent": admin.to_string(),
                "fields": {"key": "site", "title": "Site", "audience": "org", "select": [a.to_string()]}},
            {"op": "create", "type": "publication_profile", "parent": admin.to_string(),
                "fields": {"key": "all", "title": "All", "audience": "org", "select": [a.to_string(), b.to_string()]}},
            {"op": "create", "type": "decision", "id": "$old", "parent": a.to_string(),
                "fields": {"statement": "Use the old way.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$new", "parent": a.to_string(),
                "fields": {"statement": "Use the new way.", "status": "proposed"},
                "links": {"supersedes": [{"target": "$old"}]}}
        ]),
    );
    let ap = bid(&w, "$ap");
    let bp = bid(&w, "$bp");
    let (old, new) = (bid(&w2, "$old"), bid(&w2, "$new"));
    land(&mut r);
    tag(&mut r, "v1.0.0");
    write(
        &mut r,
        json!([{"op": "update", "id": ap.to_string(), "body": "Text of a, later."}]),
    );
    land(&mut r);
    write(
        &mut r,
        json!([{"op": "create", "type": "doc", "fields": {"name": "c", "title": "C", "doc_type": "page"}}]),
    );
    land(&mut r);
    r.close().unwrap();
    Source {
        dir,
        ap,
        bp,
        old,
        new,
    }
}

#[test]
fn rpl9_link_export_matches_profile_at_tag() {
    let s = source();
    let handle = StoreHandle::open(s.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let rows = snap.profiles("v1.0.0").unwrap();
    let tag_hash = snap.tags().unwrap()[0].0;

    for name in ["site", "all"] {
        let profile = row(&rows, name);
        let (report, bytes) = export(&snap, "v1.0.0", &profile.selector);
        let ids: Vec<BlockId> = report
            .delivery
            .versions
            .iter()
            .map(|v| v.id.clone())
            .collect();
        let want: Vec<BlockId> = profile.selection.iter().map(|e| e.id.clone()).collect();
        assert_eq!(ids, want, "{name}: the profile's selection, in id order");
        for (v, e) in report.delivery.versions.iter().zip(&profile.selection) {
            assert_eq!(v.version, e.version, "{name}: the tag's version");
        }
        // one selection identity: the profile's own
        assert_eq!(report.delivery.selection, profile.selection_identity);
        let publication = &report.delivery.publication;
        assert_eq!(publication.tag, "v1.0.0");
        assert_eq!(publication.profile, profile.selector);
        assert_eq!(publication.profile_version, profile.version);

        // the stream is valid whole, names the tag for provenance, and holds only the blocks
        let checked = check_delivery(&bytes[..]).unwrap();
        assert_eq!(checked.header.purpose, StreamPurpose::LinkDelivery);
        assert_eq!(checked.header.tag, tag_hash);
        assert!(checked.header.prerequisites.is_empty());
        assert_eq!(checked.keys, report.keys);
        assert_eq!(checked.digest, report.digest);
        assert_eq!(report.records, 1 + want.len() as u64);
        assert_eq!(report.bytes + 85, bytes.len() as u64);
        assert!(report.keys.iter().all(|k| k.kind == kinds::BLOCK));
    }

    // the profile selects a, not b; b's block is not in the site delivery
    let site = export(&snap, "v1.0.0", &row(&rows, "site").selector).0;
    let all = export(&snap, "v1.0.0", &row(&rows, "all").selector).0;
    let has = |r: &DeliveryReport, id: &BlockId| r.delivery.versions.iter().any(|v| v.id == *id);
    assert!(has(&site, &s.ap) && !has(&site, &s.bp));
    assert!(has(&all, &s.ap) && has(&all, &s.bp));
    // the tag's text, not canon's: ap changed after the tag
    let ap = site
        .delivery
        .versions
        .iter()
        .find(|v| v.id == s.ap)
        .unwrap();
    let want = snap
        .profiles("v1.0.0")
        .unwrap()
        .iter()
        .find(|p| p.name == "site")
        .unwrap()
        .selection
        .iter()
        .find(|e| e.id == s.ap)
        .unwrap()
        .version;
    assert_eq!(ap.version, want);

    // a missing tag and a missing profile are refused, with nothing promised of the output
    let e = snap.link_export("v9.9.9", "site", Vec::new()).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    assert!(snap.link_export("v1.0.0", "nope", Vec::new()).is_err());
    handle.close().unwrap();
}

#[test]
fn rpl9_export_has_derived_facts_and_residue() {
    let s = source();
    // the removal of ap, after the tag: the tag's version travels as its residue
    let mut r = Rdy::open(s.dir.path()).unwrap();
    redact(&mut r, &[s.ap.to_string()]);
    r.close().unwrap();

    let handle = StoreHandle::open(s.dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let rows = snap.profiles("v1.0.0").unwrap();
    let site = row(&rows, "site");
    let (report, bytes) = export(&snap, "v1.0.0", &site.selector);

    // the derived facts come from the tag's blocks; the removed block has none
    let by_id: BTreeMap<_, _> = report
        .delivery
        .versions
        .iter()
        .map(|v| (v.id.clone(), v))
        .collect();
    let old = by_id[&s.old];
    let new = by_id[&s.new];
    assert_eq!(old.derived.superseded_by, vec![s.new.clone()]);
    assert!(new.derived.superseded_by.is_empty());
    assert_eq!(new.derived.status.as_deref(), Some("accepted"));
    assert_eq!(old.derived.status.as_deref(), Some("accepted"));
    assert_eq!(by_id[&s.ap].derived, ForeignDerived::default());
    assert_ne!(by_id[&s.ap].content, Hash([0; 32]));

    // the removed version keeps its recorded identity and travels as residue
    assert_eq!(report.delivery.selection, site.selection_identity);
    let checked = check_delivery(&bytes[..]).unwrap();
    assert_eq!(checked.keys, report.keys);
    let (mut reader, _) = StreamReader::open(&bytes[..]).unwrap();
    let mut dropped = Vec::new();
    let mut present = 0;
    while let Some(rec) = reader.next().unwrap() {
        match rec {
            rdy_store::sync_stream::StreamRecord::Dropped { key, .. } => dropped.push(key.hash),
            rdy_store::sync_stream::StreamRecord::Present { .. } => present += 1,
        }
    }
    assert_eq!(dropped, vec![by_id[&s.ap].version]);
    assert!(present > 0);
    // none of the removed text is in the stream
    assert!(!String::from_utf8_lossy(&bytes).contains("Text of a."));
    handle.close().unwrap();
}

const CITED: RepoId = RepoId([7; 16]);

fn cited_block(id: &str, body: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::from([(
            "title".to_string(),
            FieldValue::Str("Cited rule".to_string()),
        )]),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

/// A project that cites a delivered block of another repo from a claim, tagged. Returns the
/// directory, the cited block, the claim, and the source ref the delivery came from.
fn citing() -> (tempfile::TempDir, Block, BlockId, String) {
    let (dir, cited, claim, src, _) = citing_then(false);
    (dir, cited, claim, src)
}

/// As [`citing`], and with `advance`, a second delivery of the same source after the tag that
/// removes the cited block from its selection. Also returns the first delivery's entry.
fn citing_then(advance: bool) -> (tempfile::TempDir, Block, BlockId, String, Hash) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "repo_link", "id": "$l", "parent": "$d",
             "fields": {"alias": "core", "repo": CITED.to_string(), "profile": "pf-AGNT1"}}
        ]),
    );
    land(&mut r);
    let link = w.created["$l"].clone();
    let cited = cited_block("d-FRGNX", "Foreign words about the gateway.");
    let version = Hash::of(&codec::item_new(kinds::BLOCK, &cited).1);
    let v = ForeignVersion {
        id: cited.id.clone(),
        version,
        content: Hash::of(b"content of the cited rule"),
        encoding: 0,
        derived: ForeignDerived::default(),
    };
    let sel = BTreeMap::from([(cited.id.clone(), version)]);
    let entry = ForeignEntry {
        repo: CITED,
        source: ForeignSource::Link {
            link: link.clone(),
            site: "hub.example".into(),
            at_ms: 1_760_000_000_000,
        },
        event: ForeignEvent::Delivery {
            prev: None,
            publication: ForeignPublication {
                tag: "v1".into(),
                profile: "pf-AGNT1".into(),
                profile_version: None,
            },
            set: None,
            added: vec![v],
            changed: vec![],
            removed: vec![],
            shells: vec![],
            selection: selection_identity(sel.iter()).unwrap(),
        },
    };
    let first = r
        .store_mut()
        .put_foreign(&entry, std::slice::from_ref(&cited))
        .unwrap();
    r.publish_after_foreign_for_tests().unwrap();
    let w = write(
        &mut r,
        json!([{"op": "create", "type": "claim", "id": "$c", "parent": w.created["$s"].clone(),
            "body": "Follow {{core:d-FRGNX}}."}]),
    );
    let claim = bid(&w, "$c");
    land(&mut r);
    tag(&mut r, "v1.0.0");
    if advance {
        let mut next = entry.clone();
        if let ForeignEvent::Delivery {
            prev,
            added,
            removed,
            selection,
            ..
        } = &mut next.event
        {
            *prev = Some(first);
            *added = vec![];
            *removed = vec![cited.id.clone()];
            *selection = selection_identity(std::iter::empty()).unwrap();
        }
        r.store_mut().put_foreign(&next, &[]).unwrap();
        r.publish_after_foreign_for_tests().unwrap();
    }
    r.close().unwrap();
    (dir, cited, claim, format!("foreign/link/{link}"), first)
}

#[test]
fn rpl9_export_never_carries_transitive_foreign_content() {
    let (dir, cited, claim, _) = citing();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let (report, bytes) = export(&snap, "v1.0.0", "implicit:org");

    // the local blocks travel, the claim among them
    assert!(report.delivery.versions.iter().any(|v| v.id == claim));
    // the delivered block does not, by id, by version or by text
    let version = Hash::of(&codec::item_new(kinds::BLOCK, &cited).1);
    assert!(report.delivery.versions.iter().all(|v| v.id != cited.id));
    assert!(report.keys.iter().all(|k| k.hash != version));
    assert!(
        !String::from_utf8_lossy(&bytes).contains("Foreign words"),
        "the cited text is not in the stream"
    );
    // no foreign entry and no TAG object: only BLOCK records, and no prerequisite
    assert!(report.keys.iter().all(|k| k.kind == kinds::BLOCK));
    assert!(report.header.prerequisites.is_empty());
    let (mut reader, _) = StreamReader::open(&bytes[..]).unwrap();
    while let Some(rec) = reader.next().unwrap() {
        assert_eq!(rec.key().kind, kinds::BLOCK);
    }
    check_delivery(&bytes[..]).unwrap();
    handle.close().unwrap();
}

#[test]
fn rpl9_tag_deliveries_list_held_versions() {
    let (dir, cited, claim, src) = citing();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let uses: Vec<TagUse> = snap.tag_deliveries("v1.0.0").unwrap();
    assert_eq!(uses.len(), 1, "{uses:?}");
    let u = &uses[0];
    assert_eq!(u.citing, claim);
    assert_eq!((u.target.repo, &u.target.id), (CITED, &cited.id));
    let version = Hash::of(&codec::item_new(kinds::BLOCK, &cited).1);
    assert_eq!(u.held, Some(version));
    assert_eq!(u.source.as_deref(), Some(src.as_str()));
    assert!(u.entry.is_some());
    assert_eq!(u.pin.content, Hash::of(b"content of the cited rule"));
    // a tag with nothing cited lists nothing
    let s = source();
    let h2 = StoreHandle::open(s.dir.path()).unwrap();
    assert!(
        h2.snapshot()
            .unwrap()
            .tag_deliveries("v1.0.0")
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        h2.snapshot()
            .unwrap()
            .tag_deliveries("v9.0.0")
            .unwrap_err()
            .code,
        "E_NOT_FOUND"
    );
    h2.close().unwrap();
    handle.close().unwrap();
}

/// A public profile over `guide` (shown prose) and the planned doc `ghost` (one shipped note), so
/// the profile keeps `ghost` only as a shell.
fn shelled() -> (tempfile::TempDir, BlockId, BlockId) {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        json!([
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$ghost", "fields": {"name": "ghost", "title": "Ghost", "doc_type": "page", "planned": true}}
        ]),
    );
    let id = |k: &str| w.created[k].clone();
    let (admin, guide, ghost) = (id("$admin"), id("$guide"), id("$ghost"));
    write(
        &mut r,
        json!([
            {"op": "create", "type": "prose", "parent": guide, "body": "Shown words."},
            {"op": "create", "type": "note", "parent": ghost, "body": "Ghost words.", "fields": {"since": "1.0.0"}},
            {"op": "create", "type": "publication_profile", "parent": admin,
             "fields": {"key": "site", "title": "Site", "audience": "public", "select": [guide, ghost]}}
        ]),
    );
    land(&mut r);
    tag(&mut r, "v1.0.0");
    r.close().unwrap();
    (
        dir,
        BlockId::parse(&guide).unwrap(),
        BlockId::parse(&ghost).unwrap(),
    )
}

#[test]
fn rpl9_export_omits_a_shell_the_profile_hides() {
    let (dir, guide, ghost) = shelled();
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let site = row(&snap.profiles("v1.0.0").unwrap(), "site");
    // the profile keeps the planned doc as a shell
    let kept: Vec<&BlockId> = site.selection.iter().map(|e| &e.id).collect();
    assert!(kept.contains(&&ghost) && kept.contains(&&guide));

    let (report, bytes) = export(&snap, "v1.0.0", &site.selector);
    let ids: Vec<&BlockId> = report.delivery.versions.iter().map(|v| &v.id).collect();
    assert!(ids.contains(&&guide) && !ids.contains(&&ghost), "{ids:?}");
    let want: Vec<&BlockId> = kept.iter().copied().filter(|id| **id != ghost).collect();
    assert_eq!(ids, want, "the selection less its shell");
    // the delivery names the shell by id and version alone, and its identity is the
    // publication's own (D-2CG1N): a consumer compares the two directly
    assert!(!ids.contains(&&ghost));
    let shell = site.selection.iter().find(|e| e.id == ghost).unwrap();
    assert_eq!(report.delivery.shells.len(), 1);
    assert_eq!(report.delivery.shells[0].id, ghost);
    assert_eq!(report.delivery.shells[0].version, shell.version);
    assert!(report.keys.iter().all(|k| k.hash != shell.version));
    assert_eq!(report.delivery.selection, site.selection_identity);
    check_delivery(&bytes[..]).unwrap();
    handle.close().unwrap();
}

#[test]
fn rpl9_tag_deliveries_keep_provenance_after_the_source_advances() {
    let (dir, cited, claim, src, first) = citing_then(true);
    let handle = StoreHandle::open(dir.path()).unwrap();
    let snap = handle.snapshot().unwrap();
    let uses = snap.tag_deliveries("v1.0.0").unwrap();
    assert_eq!(uses.len(), 1, "{uses:?}");
    let u = &uses[0];
    assert_eq!(u.citing, claim);
    // the source no longer selects the version, yet the pin is held and its delivery is listed
    let version = Hash::of(&codec::item_new(kinds::BLOCK, &cited).1);
    assert_eq!(u.held, Some(version));
    assert_eq!(u.source.as_deref(), Some(src.as_str()));
    assert_eq!(u.entry, Some(first));
    handle.close().unwrap();
}
