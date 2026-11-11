//! ab4 (D-WJ0SJ, D-69YCN, D-P3YD5, D-7M0YF): a publication profile's `planned` setting filters
//! planned content once, inside the resolution, before any reader runs. Status comes from the
//! full source scope; the selection, its graph, entries, attachments, digests, reference rows and
//! disclosure facts all read the one filtered view. A hidden planned doc or section that still
//! holds a shown block stays as a bare title-and-route shell; an empty one goes.
mod common;

use rdy_engine::as_built::Status;
use rdy_engine::publication::{
    self, Audience, CheckStatus, Classification, ReadPublicationSource, ReferenceRow, Resolved,
    ShownStatus,
};
use rdy_engine::*;
use rdy_model::redaction::RedactedBlockIdentity;
use rdy_model::*;
use rdy_schema::publication::{PlannedSetting, SinceSetting};
use rdy_schema::{Pack, PackSet};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------------------------------------
// helpers

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

fn write_why(r: &mut Rdy, view: &str, ops: Value, reason: Option<&str>) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: serde_json::from_value(ops).unwrap(),
            justification: reason.map(|w| Justification::Editorial {
                class: EditorialClass::Clarify,
                reason: w.into(),
            }),
            message: format!("write to {view}"),
        },
    )
    .unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    write_why(r, view, ops, None)
}

fn propose(r: &mut Rdy, view: &str) -> ProposalInfo {
    common::answer_open_items(r, &writer(), view);
    r.propose(&writer(), view, "as-built view", Some(common::TEST_SUMMARY))
        .unwrap()
}

fn land(r: &mut Rdy, view: &str) -> String {
    let p = propose(r, view);
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id).unwrap()
}

fn bid(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn ids(res: &Resolved) -> BTreeSet<String> {
    res.entries.iter().map(|e| e.id.to_string()).collect()
}

fn set<S: AsRef<str>>(items: &[S]) -> BTreeSet<String> {
    items.iter().map(|s| s.as_ref().to_string()).collect()
}

fn shown(status: Status, since: Option<&str>, pill: bool) -> ShownStatus {
    ShownStatus {
        status,
        since: since.map(str::to_string),
        pill,
    }
}

fn status_of(res: &Resolved, id: &str) -> Option<ShownStatus> {
    res.as_built.get(&bid(id)).cloned()
}

fn blk(id: &str, ty: &str, parent: Option<&str>, order: &str) -> Block {
    Block {
        id: bid(id),
        block_type: ty.into(),
        parent: parent.map(bid),
        order: order.into(),
        fields: BTreeMap::new(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn with(mut b: Block, k: &str, v: FieldValue) -> Block {
    b.fields.insert(k.into(), v);
    b
}

fn body(mut b: Block, text: &str) -> Block {
    b.body = text.into();
    b
}

fn link(mut b: Block, kind: &str, target: &str) -> Block {
    b.links.entry(kind.into()).or_default().push(Link {
        target: bid(target),
        part: None,
    });
    b
}

fn doc(id: &str, name: &str, doc_type: &str, order: &str) -> Block {
    with(
        with(
            with(blk(id, "doc", None, order), "name", s(name)),
            "title",
            s(&format!("Doc {name}")),
        ),
        "doc_type",
        s(doc_type),
    )
}

fn section(id: &str, parent: &str, title: &str, order: &str) -> Block {
    with(blk(id, "section", Some(parent), order), "title", s(title))
}

fn decision(id: &str, parent: Option<&str>, built: Option<&str>, order: &str) -> Block {
    let b = with(
        with(blk(id, "decision", parent, order), "status", s("accepted")),
        "statement",
        s(&format!("Ruling {id}")),
    );
    match built {
        Some(v) => with(b, "built", s(v)),
        None => b,
    }
}

fn planned(b: Block) -> Block {
    with(b, "planned", FieldValue::Bool(true))
}

fn since(b: Block, v: &str) -> Block {
    with(b, "since", s(v))
}

fn profile(id: &str, key: &str, audience: &str, select: &[&str], extra: &[(&str, &str)]) -> Block {
    let mut b = blk(id, "publication_profile", Some("d-ADM1N"), "z");
    b.fields.insert("key".into(), s(key));
    b.fields.insert("title".into(), s("Profile title"));
    b.fields.insert("audience".into(), s(audience));
    b.fields.insert(
        "select".into(),
        FieldValue::List(select.iter().map(|x| s(x)).collect()),
    );
    for (k, v) in extra {
        b.fields.insert((*k).into(), s(v));
    }
    b
}

/// A state of `blocks` alone, built in memory.
fn synth(blocks: Vec<Block>) -> Materialized {
    Materialized::build(
        StateSnapshot {
            blocks: BTreeMap::new(),
        },
        blocks.into_iter().map(|b| (b.id.clone(), b)).collect(),
    )
}

fn stub(id: &str, ty: &str, parent: Option<&str>, request: Hash) -> Block {
    Block::redacted(
        bid(id),
        ty.into(),
        parent.map(bid),
        "m".into(),
        RedactedBlockIdentity {
            version: Hash::of(format!("version of {id}").as_bytes()),
            content_hash: Hash::of(format!("content of {id}").as_bytes()),
            frozen: false,
            request,
        },
    )
}

fn row(source: &str, location: &str, target: &str, c: Classification, n: u64) -> ReferenceRow {
    ReferenceRow {
        source: bid(source),
        location: location.into(),
        target: target.into(),
        classification: c,
        occurrences: n,
    }
}

/// The standard pack with one more content type, `memo`.
fn with_memo() -> Pack {
    let mut set = PackSet::with_builtin();
    let name = set
        .add_yaml(
            "name: team\nversion: \"1\"\nextends: rdy-standard\nblock_types:\n  memo:\n    prefix: mm\n    traits: [informative]\n    body: required\n",
        )
        .unwrap();
    set.resolve(&name).unwrap()
}

/// `profile` resolved in the in-memory state `m` through `st`'s store, read with `pack`.
fn res_with(st: &ReadState, pack: &Pack, m: &Materialized, profile: &str) -> Resolved {
    let source = ReadPublicationSource::with_settings(st, pack, "Project", None);
    publication::resolve_source(&source, m, profile).unwrap()
}

fn res(st: &ReadState, m: &Materialized, profile: &str) -> Resolved {
    publication::resolve_in_for_tests(st, m, profile).unwrap()
}

/// Every retained block keeps its source parent, its parent is retained (or it sits at the top
/// or is kept outside a doc under D-P3YD5), and every entry carries the source version.
fn assert_tree_and_versions(res: &Resolved, m: &Materialized) {
    for e in &res.entries {
        let src = m.get(&e.id).unwrap();
        let kept = res.m.get(&e.id).unwrap();
        assert_eq!(kept.parent, src.parent, "{} keeps its parent", e.id);
        assert_eq!(kept.order, src.order, "{} keeps its place", e.id);
        assert_eq!(
            e.version,
            m.version_of(&e.id).unwrap_or_else(|| src.version_hash()),
            "{} keeps its source version",
            e.id
        );
        if let Some(p) = &kept.parent {
            assert!(
                res.m.get(p).is_some() || rdy_model::tree::may_sit_outside_a_doc(&kept.block_type),
                "{}'s parent {p} is retained",
                e.id
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// tests

/// Public hides planned content by default, org marks it, and an explicit setting wins. Status
/// comes from the full source scope: a governing decision outside the selection still makes its
/// block planned, an unrelated decision that reaches a block through an entity does not, and
/// each tag, view and proposal reads its own profile revision and its own decisions. A source
/// read holds every block; implicit:org marks.
#[test]
fn ab4_profile_defaults_filter_from_full_scoped_status() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$rules", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dp", "parent": "$rules", "fields": {"statement": "Ship the planned part.", "status": "proposed"}},
            {"op": "create", "type": "decision", "id": "$dv", "parent": "$rules", "fields": {"statement": "Ship the built part.", "status": "proposed", "built": "v1.2.0"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$guide", "fields": {"title": "Usage"}},
            {"op": "create", "type": "note", "id": "$np", "parent": "$s", "body": "Planned note.", "links": {"decided_by": ["$dp"]}},
            {"op": "create", "type": "note", "id": "$nv", "parent": "$s", "body": "Built note.", "links": {"decided_by": ["$dv"]}},
            {"op": "create", "type": "prose", "id": "$pu", "parent": "$s", "body": "Plain prose."},
            {"op": "create", "type": "entity", "id": "$en", "parent": "$s", "fields": {"name": "Ledger"}},
            {"op": "create", "type": "prose", "id": "$pr", "parent": "$s", "body": "The ledger keeps rows.", "links": {"describes": ["$en"]}},
            {"op": "create", "type": "decision", "id": "$de", "parent": "$rules", "fields": {"statement": "Plan the ledger.", "status": "proposed"}, "links": {"affects": [{"target": "$en"}]}}
        ]),
    );
    let c = |k: &str| w.created[k].clone();
    let (admin, guide) = (c("$admin"), c("$guide"));
    let (dp, np, nv, pu, en, pr, sec) = (
        c("$dp"),
        c("$np"),
        c("$nv"),
        c("$pu"),
        c("$en"),
        c("$pr"),
        c("$s"),
    );
    let pf = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "publication_profile", "id": "$ph", "parent": admin,
             "fields": {"key": "pubhide", "title": "Public", "audience": "public", "select": [guide]}},
            {"op": "create", "type": "publication_profile", "id": "$pm", "parent": admin,
             "fields": {"key": "pubmark", "title": "Public marked", "audience": "public", "planned": "mark", "select": [guide]}},
            {"op": "create", "type": "publication_profile", "id": "$om", "parent": admin,
             "fields": {"key": "orgmark", "title": "Org", "audience": "org", "select": [guide]}},
            {"op": "create", "type": "publication_profile", "id": "$oh", "parent": admin,
             "fields": {"key": "orghide", "title": "Org hidden", "audience": "org", "planned": "hide", "select": [guide]}}
        ]),
    );
    let (ph, om) = (pf.created["$ph"].clone(), pf.created["$om"].clone());
    land(&mut r, "work");
    r.tag(&founder(), "v1.0.0", None, None).unwrap();

    let all_kept = set(&[&guide, &sec, &np, &nv, &pu, &en, &pr]);
    let without_planned = set(&[&guide, &sec, &nv, &pu, &en, &pr]);
    let st = r.read();
    let at = |scope: &Scope, profile: &str| {
        publication::resolve_inspection(&st, scope, profile).unwrap()
    };

    // public hides by default; org marks by default; an explicit setting wins either way
    let hide = at(&Scope::Canon, "pubhide");
    assert_eq!(hide.profile.planned, PlannedSetting::Hide);
    assert_eq!(hide.profile.since, None);
    assert_eq!(ids(&hide), without_planned);
    let mark = at(&Scope::Canon, "pubmark");
    assert_eq!(mark.profile.planned, PlannedSetting::Mark);
    assert_eq!(ids(&mark), all_kept);
    let org = at(&Scope::Canon, "orgmark");
    assert_eq!(org.profile.planned, PlannedSetting::Mark);
    assert_eq!(ids(&org), all_kept);
    let org_hide = at(&Scope::Canon, "orghide");
    assert_eq!(org_hide.profile.planned, PlannedSetting::Hide);
    assert_eq!(ids(&org_hide), without_planned);
    for res in [&hide, &mark, &org, &org_hide] {
        assert!(res.shells.is_empty(), "no planned container here");
    }

    // the governing decision lies outside every selection, and still sets the status; the
    // status data a resolution carries holds no decision and no derivation
    for res in [&hide, &mark, &org] {
        assert!(!ids(res).contains(&dp), "{dp} is outside the selection");
    }
    assert_eq!(
        status_of(&mark, &np),
        Some(shown(Status::Planned, None, false))
    );
    assert_eq!(status_of(&hide, &np), None, "a hidden block has no status");
    assert_eq!(
        status_of(&hide, &nv),
        Some(shown(Status::Since, Some("1.2.0"), false)),
        "since shows in reference docs only when the profile omits it"
    );
    // an unrelated planned decision that reaches the entity gives no status: only direct links
    for b in [&en, &pr, &pu, &sec] {
        assert_eq!(
            status_of(&hide, b),
            Some(shown(Status::Unversioned, None, false)),
            "{b}"
        );
    }
    // every kept content block has a status, and only kept blocks do
    for res in [&hide, &mark, &org, &org_hide] {
        let with_status: BTreeSet<String> = res.as_built.keys().map(|b| b.to_string()).collect();
        let mut content = ids(res);
        content.remove(&guide);
        assert_eq!(with_status, content);
    }
    // a hidden block's link is no link outside the selection: its source is gone too
    assert!(
        !hide
            .references
            .links_outside
            .iter()
            .any(|l| l.source.as_str() == np),
        "{:?}",
        hide.references.links_outside
    );
    assert!(
        mark.references
            .links_outside
            .iter()
            .any(|l| l.source.as_str() == np)
    );

    // implicit:org marks and omits since; a source read holds every block and reads planned
    let implicit = at(&Scope::Canon, publication::IMPLICIT_ORG);
    assert_eq!(implicit.profile.planned, PlannedSetting::Mark);
    assert_eq!(implicit.profile.since, None);
    assert!(ids(&implicit).contains(&np));
    assert_eq!(
        status_of(&implicit, &np),
        Some(shown(Status::Planned, None, false))
    );
    assert_eq!(
        st.as_built(&Scope::Canon, [&bid(&np)]).unwrap()[0]
            .1
            .label(),
        "planned"
    );
    assert!(st.block(&Scope::Canon, &bid(&np)).is_ok());

    // a new profile revision in canon: pubhide now marks; the tag keeps the revision it held
    write(
        &mut r,
        "policy",
        json!([{"op": "update", "id": ph, "fields": {"planned": "mark"}}]),
    );
    land(&mut r, "policy");
    // a view and its proposal: orgmark hides there
    write(
        &mut r,
        "edit",
        json!([{"op": "update", "id": om, "fields": {"planned": "hide"}}]),
    );
    let pe = propose(&mut r, "edit");
    // a built-only change in another view and its proposal: the planned decision ships in 2.0.0
    write_why(
        &mut r,
        "later",
        json!([{"op": "update", "id": dp, "fields": {"built": "2.0.0"}}]),
        Some("it shipped in 2.0.0"),
    );
    let pl = propose(&mut r, "later");
    let st = r.read();
    let at = |scope: &Scope, profile: &str| {
        publication::resolve_inspection(&st, scope, profile).unwrap()
    };
    let tag = st.resolve_scope("tag:v1.0.0").unwrap();
    let edit = Scope::View("edit".into());
    let later = Scope::View("later".into());
    let p_edit = st.resolve_scope(&format!("proposal:{}", pe.r#ref)).unwrap();
    let p_later = st.resolve_scope(&format!("proposal:{}", pl.r#ref)).unwrap();

    let canon = at(&Scope::Canon, "pubhide");
    assert_eq!(canon.profile.planned, PlannedSetting::Mark);
    assert_eq!(ids(&canon), all_kept);
    let then = at(&tag, "pubhide");
    assert_eq!(then.profile.planned, PlannedSetting::Hide);
    assert_eq!(ids(&then), without_planned);
    // durable output through the tag reads the same revision
    assert_eq!(
        ids(&publication::resolve(&st, &tag, "pubhide").unwrap()),
        without_planned
    );
    for scope in [&edit, &p_edit] {
        let res = at(scope, "orgmark");
        assert_eq!(res.profile.planned, PlannedSetting::Hide, "{scope:?}");
        assert_eq!(ids(&res), without_planned, "{scope:?}");
    }
    assert_eq!(ids(&at(&Scope::Canon, "orgmark")), all_kept);
    // the built-only change: the note's version is the same, and it shows where the decision
    // shipped, with that version
    let m_canon = st.materialized(&Scope::Canon).unwrap();
    for scope in [&later, &p_later] {
        let res = at(scope, "orghide");
        assert_eq!(ids(&res), all_kept, "{scope:?}");
        assert_eq!(
            status_of(&res, &np),
            Some(shown(Status::Since, Some("2.0.0"), false)),
            "{scope:?}"
        );
        let e = res.entries.iter().find(|e| e.id.as_str() == np).unwrap();
        assert_eq!(Some(e.version), m_canon.version_of(&bid(&np)), "{scope:?}");
    }
    assert_eq!(ids(&at(&Scope::Canon, "orghide")), without_planned);
}

/// The 0.37.0 reference rules hold over the filtered view: a retained reference to hidden
/// content is excluded, wherever it sits and however often; a hidden source gives no row; a link
/// to hidden content is a link outside the selection and never blocks; an id reference to an
/// outside decision is dropped; a missing id stays missing; code source is never scanned; the
/// chosen profile's metadata counts; the root-decision closure is made before the filter.
#[test]
fn ab4_hidden_targets_keep_pb2_reference_rules() {
    let (_dir, r) = project();
    let st = r.read();
    let blocks = vec![
        doc("d-ADM1N", "admin", "page", "a"),
        with(
            profile("pf-PBC01", "pubsite", "public", &["d-KEEP1"], &[]),
            "title",
            s("Site of {{n-HDN01}}"),
        ),
        profile("pf-PBC02", "orgsite", "org", &["d-KEEP1"], &[]),
        profile(
            "pf-PBC03",
            "orghide",
            "org",
            &["d-KEEP1"],
            &[("planned", "hide")],
        ),
        doc("d-KEEP1", "keep", "page", "b"),
        link(
            with(
                body(
                    planned(blk("n-HDN01", "note", Some("d-KEEP1"), "a")),
                    "Hidden {{d-0THR1}} and {{D-M1SS2}}.",
                ),
                "key",
                s("hidden-note"),
            ),
            "decided_by",
            "D-R00T2",
        ),
        link(
            body(
                blk("p-SHW01", "prose", Some("d-KEEP1"), "b"),
                "See {{n-HDN01}}, {{n-HDN01|the note}} and {{n-HDN01}} again; by key \
                 {{hidden-note}}; ruling ({{D-DT001|@key}}) and {{D-DT001|the ruling}}; gone \
                 {{D-M1SS1}}; root {{D-R00T1}}.",
            ),
            "decided_by",
            "D-R00T1",
        ),
        with(
            blk("tb-TAB01", "table", Some("d-KEEP1"), "c"),
            "rows",
            FieldValue::List(vec![FieldValue::List(vec![s(
                "Cell {{n-HDN01}} ({{D-DT001}})",
            )])]),
        ),
        with(
            body(
                blk("dg-DGM01", "diagram", Some("d-KEEP1"), "d"),
                "graph TD; A[{{n-HDN01}}]",
            ),
            "lang",
            s("mermaid"),
        ),
        body(
            blk("cd-CDE01", "code", Some("d-KEEP1"), "e"),
            "let x = \"{{n-HDN01}}\";",
        ),
        link(
            body(blk("p-W1RE1", "prose", Some("d-KEEP1"), "f"), "Describes."),
            "describes",
            "n-HDN01",
        ),
        doc("d-0THR1", "other", "page", "c"),
        decision("D-DT001", Some("d-0THR1"), Some("1.0.0"), "a"),
        decision("D-R00T1", None, Some("1.0.0"), "r1"),
        decision("D-R00T2", None, Some("planned"), "r2"),
    ];
    let m = synth(blocks);
    use Classification::{Excluded, Missing};

    let public = res(&st, &m, "pubsite");
    // the planned note is hidden; both root decisions stay: the closure reads the selection
    // before the filter (D-P3YD5), whatever their own built
    assert_eq!(
        ids(&public),
        set(&[
            "D-R00T1", "D-R00T2", "cd-CDE01", "d-KEEP1", "dg-DGM01", "p-SHW01", "p-W1RE1",
            "tb-TAB01"
        ])
    );
    assert_eq!(
        status_of(&public, "p-SHW01"),
        Some(shown(Status::Since, Some("1.0.0"), false))
    );
    let refs = &public.references;
    assert_eq!(
        refs.rows,
        vec![
            row("dg-DGM01", "body", "n-HDN01", Excluded, 1),
            row("p-SHW01", "body", "D-M1SS1", Missing, 1),
            row("p-SHW01", "body", "hidden-note", Excluded, 1),
            row("p-SHW01", "body", "n-HDN01", Excluded, 3),
            row("pf-PBC01", "/fields/title", "n-HDN01", Excluded, 1),
            row("tb-TAB01", "/fields/rows/0/0", "n-HDN01", Excluded, 1),
        ]
    );
    assert_eq!(
        (refs.excluded_count, refs.missing_count, refs.blocked_count),
        (7, 1, 8)
    );
    assert_eq!(
        refs.links_outside,
        vec![row("p-W1RE1", "/links/describes/0", "n-HDN01", Excluded, 1)]
    );
    assert_eq!(refs.links_outside_count, 1);
    assert_eq!(refs.dropped_decisions, BTreeSet::from([bid("D-DT001")]));
    assert_eq!(refs.status, CheckStatus::Blocked);
    let e = public.require_output().unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_PUBLICATION_REFERENCE,
            "Public profile has 8 references outside its selection.",
            "Include the target docs or remove these references. Run the publication check again."
        )
    );

    // org marks: the note is shown, so references to it are no rows, and its own count
    let org = res(&st, &m, "orgsite");
    assert!(ids(&org).contains("n-HDN01"));
    assert_eq!(
        org.references.rows,
        vec![
            row("n-HDN01", "body", "D-M1SS2", Missing, 1),
            row("n-HDN01", "body", "d-0THR1", Excluded, 1),
            row("p-SHW01", "body", "D-M1SS1", Missing, 1),
        ]
    );
    assert!(org.references.links_outside.is_empty());
    let w = org.references.warning().unwrap();
    assert_eq!(
        (w.code, w.message.as_str(), w.fix.as_str()),
        (
            W_PUBLICATION_REFERENCE,
            "Org profile has 3 references outside its selection.",
            "Review these references before you share the output."
        )
    );

    // org with hide: the public rows less the public profile's own title, as a warning
    let org_hide = res(&st, &m, "orghide");
    assert_eq!(ids(&org_hide), ids(&public));
    assert_eq!(
        org_hide.references.rows,
        public
            .references
            .rows
            .iter()
            .filter(|r| r.source.as_str() != "pf-PBC01")
            .cloned()
            .collect::<Vec<_>>()
    );
    assert_eq!(org_hide.references.status, CheckStatus::Warning);
    assert_eq!(
        org_hide.references.warning().unwrap().message,
        "Org profile has 7 references outside its selection."
    );
    assert!(org_hide.require_output().is_ok());
    // the graph holds no edge to or from the hidden note
    for res in [&public, &org_hide] {
        assert!(res.graph.incoming(&bid("n-HDN01")).is_empty());
        assert!(res.graph.outgoing(&bid("n-HDN01")).is_empty());
        assert!(res.m.get(&bid("n-HDN01")).is_none());
    }
}

/// D-69YCN: a hidden planned doc or section that still holds a shown block keeps only its title
/// and route; an empty one goes. Own since beats inherited planned; supporting blocks are not
/// filtered by status; a custom content type filters like any other. Each retained block keeps
/// its source parent, place and version.
#[test]
fn ab4_containers_and_surviving_children_follow_o2() {
    let (_dir, r) = project();
    let st = r.read();
    let pack = with_memo();
    let request = Hash::of(b"a removal request");
    let select = ["d-PRP01", "d-EMPT1", "d-SHW01"];
    let blocks = vec![
        doc("d-ADM1N", "admin", "page", "a"),
        profile("pf-PBC01", "pubsite", "public", &select, &[]),
        profile("pf-PBC02", "nothing", "public", &["d-EMPT1"], &[]),
        profile("pf-PBC03", "orgsite", "org", &select, &[]),
        profile(
            "pf-PBC04",
            "showall",
            "public",
            &select,
            &[("since", "show")],
        ),
        profile("pf-PBC05", "quiet", "public", &select, &[("since", "off")]),
        // a planned reference doc with a shipped block deep inside it
        link(
            body(
                with(
                    planned(doc("d-PRP01", "plan", "reference", "b")),
                    "lifecycle",
                    s("draft"),
                ),
                "Secret doc body.",
            ),
            "decided_by",
            "D-DT001",
        ),
        link(
            body(
                with(
                    section("s-SEC01", "d-PRP01", "Sec one", "a"),
                    "lifecycle",
                    s("draft"),
                ),
                "Secret section text.",
            ),
            "decided_by",
            "D-DT001",
        ),
        body(
            since(blk("n-SHP01", "note", Some("s-SEC01"), "a"), "v1.2.0"),
            "Shipped one.",
        ),
        section("s-SEC02", "s-SEC01", "Sec two", "b"),
        body(blk("p-G0NE2", "prose", Some("s-SEC02"), "a"), "Gone two."),
        section("s-SEC03", "s-SEC01", "Sec three", "c"),
        decision("D-1N001", Some("s-SEC03"), Some("planned"), "a"),
        body(blk("p-G0NE1", "prose", Some("d-PRP01"), "b"), "Gone one."),
        // a removed section under the planned doc, with a shipped block under it
        stub("s-STB01", "section", Some("d-PRP01"), request),
        body(
            since(blk("n-SHP03", "note", Some("s-STB01"), "a"), "1.0.0"),
            "Shipped three.",
        ),
        // a planned doc with nothing shown
        planned(doc("d-EMPT1", "all", "page", "c")),
        body(blk("p-G0NE3", "prose", Some("d-EMPT1"), "a"), "Gone three."),
        // a shown doc with a planned section and custom content
        doc("d-SHW01", "shown", "page", "d"),
        planned(section("s-SEC04", "d-SHW01", "Sec four", "a")),
        body(
            since(blk("n-SHP02", "note", Some("s-SEC04"), "a"), "2.0.0"),
            "Shipped two.",
        ),
        body(
            planned(blk("mm-MEM01", "memo", Some("d-SHW01"), "b")),
            "Planned memo.",
        ),
        body(
            since(blk("mm-MEM02", "memo", Some("d-SHW01"), "c"), "3.0.0-rc.1"),
            "Shipped memo.",
        ),
        doc("d-0THR1", "other", "page", "e"),
        decision("D-DT001", Some("d-0THR1"), Some("1.0.0"), "a"),
    ];
    let m = synth(blocks);

    let public = res_with(&st, &pack, &m, "pubsite");
    assert_eq!(
        ids(&public),
        set(&[
            "D-1N001", "d-PRP01", "d-SHW01", "mm-MEM02", "n-SHP01", "n-SHP02", "n-SHP03",
            "s-SEC01", "s-SEC03", "s-SEC04", "s-STB01"
        ])
    );
    assert_eq!(
        public.shells,
        BTreeSet::from([
            bid("d-PRP01"),
            bid("s-SEC01"),
            bid("s-SEC03"),
            bid("s-SEC04"),
            bid("s-STB01")
        ])
    );
    // a removed container stays its stub, with its removal
    assert!(public.m.get(&bid("s-STB01")).unwrap().is_redacted());
    let stub_entry = public
        .entries
        .iter()
        .find(|e| e.id.as_str() == "s-STB01")
        .unwrap();
    assert_eq!(stub_entry.redaction, Some(request));
    assert_eq!(public.redactions, vec![request]);
    assert_tree_and_versions(&public, &m);
    // a shell is its title and its route: no body, no link, no pin, no other field
    for (id, fields) in [
        ("d-PRP01", vec!["name", "title"]),
        ("s-SEC01", vec!["title"]),
        ("s-SEC03", vec!["title"]),
        ("s-SEC04", vec!["title"]),
    ] {
        let b = public.m.get(&bid(id)).unwrap();
        let src = m.get(&bid(id)).unwrap();
        assert_eq!(
            b.fields.keys().map(String::as_str).collect::<Vec<_>>(),
            fields,
            "{id}"
        );
        assert_eq!(b.field_str("title"), src.field_str("title"), "{id}");
        assert!(b.body.is_empty(), "{id}");
        assert!(b.links.is_empty() && b.pins.is_empty(), "{id}");
        assert_eq!(status_of(&public, id), None, "a shell shows no marker");
    }
    // nav and route: the shell doc keeps its path, its digest is listed, the empty doc is gone
    assert_eq!(
        public
            .docs
            .iter()
            .map(|d| (d.id.to_string(), d.path.clone()))
            .collect::<Vec<_>>(),
        vec![
            ("d-PRP01".to_string(), "plan".to_string()),
            ("d-SHW01".to_string(), "shown".to_string())
        ]
    );
    assert_eq!(
        public.m.path_of(&bid("d-PRP01")),
        m.path_of(&bid("d-PRP01"))
    );
    // the graph holds no edge of a shell's links
    assert!(
        public
            .graph
            .outgoing(&bid("s-SEC01"))
            .iter()
            .all(|e| !matches!(&e.kind, rdy_graph::EdgeKind::Link(_)))
    );
    assert_eq!(public.references.status, CheckStatus::Clear);
    assert!(public.references.links_outside.is_empty());
    // statuses: own since beats the inherited planned; since shows in a reference doc only
    assert_eq!(
        status_of(&public, "n-SHP01"),
        Some(shown(Status::Since, Some("1.2.0"), true))
    );
    assert_eq!(
        status_of(&public, "n-SHP02"),
        Some(shown(Status::Since, Some("2.0.0"), false))
    );
    assert_eq!(
        status_of(&public, "mm-MEM02"),
        Some(shown(Status::Since, Some("3.0.0-rc.1"), false))
    );
    assert_eq!(
        status_of(&public, "D-1N001"),
        None,
        "a decision is no content"
    );
    // explicit show and off apply to every block
    let all = res_with(&st, &pack, &m, "showall");
    assert_eq!(all.profile.since, Some(SinceSetting::Show));
    assert!(status_of(&all, "n-SHP01").unwrap().pill);
    assert!(status_of(&all, "n-SHP02").unwrap().pill);
    assert!(status_of(&all, "mm-MEM02").unwrap().pill);
    let quiet = res_with(&st, &pack, &m, "quiet");
    assert_eq!(quiet.profile.since, Some(SinceSetting::Off));
    assert!(!status_of(&quiet, "n-SHP01").unwrap().pill);
    assert_eq!(ids(&quiet), ids(&public));

    // a profile whose one doc is all planned selects nothing
    let nothing = res_with(&st, &pack, &m, "nothing");
    assert!(nothing.entries.is_empty() && nothing.docs.is_empty() && nothing.shells.is_empty());
    assert!(nothing.as_built.is_empty());

    // org marks: everything is kept whole, with the planned statuses
    let org = res_with(&st, &pack, &m, "orgsite");
    assert!(org.shells.is_empty());
    assert_tree_and_versions(&org, &m);
    for e in &org.entries {
        assert_eq!(
            org.m.get(&e.id),
            m.get(&e.id),
            "{} is the source block",
            e.id
        );
    }
    assert_eq!(
        status_of(&org, "mm-MEM01"),
        Some(shown(Status::Planned, None, false))
    );
    assert_eq!(
        status_of(&org, "s-SEC01"),
        Some(shown(Status::Planned, None, false))
    );
    // the standard pack knows no memo: no status, so nothing hides it
    let standard = res(&st, &m, "pubsite");
    assert!(ids(&standard).contains("mm-MEM01"));
    assert_eq!(status_of(&standard, "mm-MEM01"), None);
}

/// The bytes of one selection entry row.
fn entry_row(out: &mut Vec<u8>, id: &BlockId, version: &Hash, redaction: Option<Hash>) {
    for part in [
        id.to_string(),
        version.to_string(),
        redaction.map_or("-".into(), |h| h.to_string()),
    ] {
        out.extend_from_slice(part.as_bytes());
        out.push(0);
    }
}

/// The disclosure comparison and the selection digests read the filtered view: a built-only
/// change of an excluded decision shows a block, a shell that becomes whole and a status that
/// changes are disclosed though no entry version moved, a profile's policy change adds and
/// removes attachments, removals keep their requests, and a self-comparison is empty. The
/// selection identity hashes entries only, so a status or shell change keeps it.
#[test]
fn ab4_disclosure_and_identity_follow_visibility() {
    let (_dir, r) = project();
    let st = r.read();
    let request = Hash::of(b"a removal request");
    let blob = Hash::of(b"a figure");
    let base = |ds: &str, dp: &str, policy: Option<&str>| {
        let extra: Vec<(&str, &str)> = policy.map(|p| ("planned", p)).into_iter().collect();
        synth(vec![
            doc("d-ADM1N", "admin", "page", "a"),
            profile("pf-PBC01", "pubsite", "public", &["d-KEEP1"], &extra),
            profile("pf-PBC02", "orgsite", "org", &["d-KEEP1"], &[]),
            doc("d-KEEP1", "keep", "page", "b"),
            link(
                section("s-SEC01", "d-KEEP1", "Sec one", "a"),
                "decided_by",
                "D-DS001",
            ),
            body(
                since(blk("n-SHP01", "note", Some("s-SEC01"), "a"), "1.0.0"),
                "Shipped.",
            ),
            link(
                body(blk("n-PNN01", "note", Some("d-KEEP1"), "b"), "Planned."),
                "decided_by",
                "D-DP001",
            ),
            with(
                with(
                    with(
                        planned(blk("fg-F1G01", "figure", Some("d-KEEP1"), "c")),
                        "blob",
                        s(&blob.to_string()),
                    ),
                    "media_type",
                    s("image/png"),
                ),
                "alt",
                s("A figure"),
            ),
            stub("p-STB01", "prose", Some("d-KEEP1"), request),
            doc("d-0THR1", "other", "page", "c"),
            decision("D-DS001", Some("d-0THR1"), Some(ds), "a"),
            decision("D-DP001", Some("d-0THR1"), Some(dp), "b"),
        ])
    };
    let a = base("planned", "planned", None);
    let b = base("1.0.0", "1.0.0", None);
    let c = base("1.0.0", "planned", None);
    let marked = base("planned", "planned", Some("mark"));

    let pa = res(&st, &a, "pubsite");
    let pb = res(&st, &b, "pubsite");
    let pc = res(&st, &c, "pubsite");
    assert_eq!(pa.shells, BTreeSet::from([bid("s-SEC01")]));
    assert_eq!(ids(&pa), set(&["d-KEEP1", "n-SHP01", "p-STB01", "s-SEC01"]));
    assert!(
        pa.attachments.is_empty(),
        "a hidden figure has no attachment row"
    );
    assert_eq!(pa.redactions, vec![request]);

    // a built-only change of an excluded decision shows its block, at its source version
    assert_eq!(
        ids(&pb),
        set(&["d-KEEP1", "n-PNN01", "n-SHP01", "p-STB01", "s-SEC01"])
    );
    assert!(pb.shells.is_empty());
    let d = publication::disclosure_diff(&pa, &pb).unwrap();
    assert_eq!(
        d.changed_docs
            .iter()
            .map(|c| c.id.to_string())
            .collect::<Vec<_>>(),
        vec!["d-KEEP1"]
    );
    assert!(!d.profile_changed);
    assert_ne!(d.before_selection_identity, d.after_selection_identity);
    assert_eq!(d.redactions, vec![request]);
    let shown_version = pb
        .entries
        .iter()
        .find(|e| e.id.as_str() == "n-PNN01")
        .unwrap()
        .version;
    assert_eq!(
        shown_version,
        a.get(&bid("n-PNN01")).unwrap().version_hash()
    );
    // the excluded decision is not added to explain it
    assert!(!ids(&pb).contains("D-DP001") && !ids(&pb).contains("D-DS001"));

    // a shell that becomes whole: the same entries and identity, a new digest and hash
    assert_eq!(pa.entries, pc.entries);
    assert!(pc.shells.is_empty());
    let d = publication::disclosure_diff(&pa, &pc).unwrap();
    assert_eq!(d.before_selection_identity, d.after_selection_identity);
    assert_ne!(d.before_selection_hash, d.after_selection_hash);
    assert_eq!(
        d.changed_docs
            .iter()
            .map(|c| c.id.to_string())
            .collect::<Vec<_>>(),
        vec!["d-KEEP1"]
    );
    let d = publication::disclosure_diff(&pc, &pa).unwrap();
    assert_eq!(d.changed_docs.len(), 1, "and back");

    // org marks: a status change alone is disclosed, the identity stays
    let oa = res(&st, &a, "orgsite");
    let oc = res(&st, &c, "orgsite");
    assert_eq!(oa.entries, oc.entries);
    assert_eq!(
        status_of(&oa, "s-SEC01"),
        Some(shown(Status::Planned, None, false))
    );
    assert_eq!(
        status_of(&oc, "s-SEC01"),
        Some(shown(Status::Since, Some("1.0.0"), false))
    );
    let d = publication::disclosure_diff(&oa, &oc).unwrap();
    assert_eq!(d.before_selection_identity, d.after_selection_identity);
    assert_ne!(d.before_selection_hash, d.after_selection_hash);
    assert_eq!(d.changed_docs.len(), 1);
    assert_eq!(oa.attachments.len(), 1, "a marked figure keeps its row");

    // a changed profile policy: the figure's attachment comes and goes with it
    let pm = res(&st, &marked, "pubsite");
    let d = publication::disclosure_diff(&pa, &pm).unwrap();
    assert_eq!(
        d.added_attachments
            .iter()
            .map(|x| x.block.to_string())
            .collect::<Vec<_>>(),
        vec!["fg-F1G01"]
    );
    assert!(d.removed_attachments.is_empty());
    let d = publication::disclosure_diff(&pm, &pa).unwrap();
    assert_eq!(
        d.removed_attachments
            .iter()
            .map(|x| x.block.to_string())
            .collect::<Vec<_>>(),
        vec!["fg-F1G01"]
    );

    // deterministic: a second resolution is equal; a self-comparison is empty
    for (x, m) in [(&pa, &a), (&pb, &b), (&oa, &a)] {
        let again = res(
            &st,
            m,
            if x.profile.audience == Audience::Org {
                "orgsite"
            } else {
                "pubsite"
            },
        );
        assert_eq!(again.entries, x.entries);
        assert_eq!(again.shells, x.shells);
        assert_eq!(again.as_built, x.as_built);
        assert_eq!(again.selection_hash, x.selection_hash);
        assert_eq!(again.docs, x.docs);
        let d = publication::disclosure_diff(x, x).unwrap();
        assert!(d.added_docs.is_empty() && d.removed_docs.is_empty() && d.changed_docs.is_empty());
        assert!(d.added_attachments.is_empty() && d.removed_attachments.is_empty());
        assert!(d.new_references.is_empty() && !d.profile_changed);
    }

    // a selection with no shell and only unversioned content hashes by the existing recipe
    let old = synth(vec![
        doc("d-ADM1N", "admin", "page", "a"),
        profile("pf-PBC01", "pubsite", "public", &["d-KEEP1"], &[]),
        doc("d-KEEP1", "keep", "page", "b"),
        link(
            body(blk("n-PNN01", "note", Some("d-KEEP1"), "b"), "Old."),
            "decided_by",
            "D-DP001",
        ),
        doc("d-0THR1", "other", "page", "c"),
        decision("D-DP001", Some("d-0THR1"), None, "b"),
    ]);
    let po = res(&st, &old, "pubsite");
    let mut bytes = b"rdy-publication-selection-1\0".to_vec();
    let version = po
        .profile
        .version
        .map_or_else(|| "-".to_string(), |h| h.to_string());
    for part in ["pf-PBC01", &version] {
        bytes.extend_from_slice(part.as_bytes());
        bytes.push(0);
    }
    for e in &po.entries {
        entry_row(&mut bytes, &e.id, &e.version, e.redaction);
    }
    assert_eq!(po.selection_hash, Hash::of(&bytes));

    tag_baseline_discloses_a_shipped_block();
}

/// A tag baseline against canon after a built-only landing: the shown block is disclosed, at
/// the version the tag held.
fn tag_baseline_discloses_a_shipped_block() {
    let (_dir, mut r) = project();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}},
            {"op": "create", "type": "doc", "id": "$rules", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dp", "parent": "$rules", "fields": {"statement": "Ship the planned part.", "status": "proposed"}},
            {"op": "create", "type": "doc", "id": "$guide", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "note", "id": "$np", "parent": "$guide", "body": "Planned note.", "links": {"decided_by": ["$dp"]}},
            {"op": "create", "type": "prose", "id": "$pu", "parent": "$guide", "body": "Plain prose."}
        ]),
    );
    let c = |k: &str| w.created[k].clone();
    write(
        &mut r,
        "work",
        json!([{"op": "create", "type": "publication_profile", "id": "$ph", "parent": c("$admin"),
                "fields": {"key": "pubhide", "title": "Public", "audience": "public", "select": [c("$guide")]}}]),
    );
    land(&mut r, "work");
    r.tag(&founder(), "v1.0.0", None, None).unwrap();
    write_why(
        &mut r,
        "ship",
        json!([{"op": "update", "id": c("$dp"), "fields": {"built": "1.1.0"}}]),
        Some("it shipped in 1.1.0"),
    );
    land(&mut r, "ship");
    let st = r.read();
    let tag = st.resolve_scope("tag:v1.0.0").unwrap();
    let before = publication::resolve(&st, &tag, "pubhide").unwrap();
    let after = publication::resolve(&st, &Scope::Canon, "pubhide").unwrap();
    assert!(!ids(&before).contains(&c("$np")));
    assert!(ids(&after).contains(&c("$np")));
    let d = publication::disclosure_diff(&before, &after).unwrap();
    assert_eq!(
        d.changed_docs
            .iter()
            .map(|x| x.id.to_string())
            .collect::<Vec<_>>(),
        vec![c("$guide")]
    );
    assert!(!d.profile_changed);
    let held = st.materialized(&tag).unwrap().version_of(&bid(&c("$np")));
    let e = after
        .entries
        .iter()
        .find(|e| e.id.as_str() == c("$np"))
        .unwrap();
    assert_eq!(Some(e.version), held);
}
