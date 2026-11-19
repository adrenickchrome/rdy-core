//! rpl27 (repo links; D-DAPR8, D-Y49PX, D-4W1ZR, D-0DNKT, D-MEMMQ; hub rows 72 to 83): the citing
//! side's pure inspection reads on one captured snapshot. Each read matches the engine's own
//! facts, names the cited repo and the delivering entry, and keeps the foreign cut it was taken
//! with when a later delivery arrives. `link_adoption` gives each repo link's newest delivery and
//! the delivery its last adopt answer named.
use rdy_adapter::foreign_facts::{
    AdoptedDelivery, LastDelivery, LinkState, LockEdit, SelectionEdit,
};
use rdy_adapter::{BlockId, DeliverySource, ForeignSet, Hash, RepoId, Snapshot, StoreHandle};
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::foreign_delivery::{revoke_link, take_delivery};
use rdy_engine::foreign_facts::foreign_tension_gate;
use rdy_engine::pending::{CARD_MAX_TOKENS, Item, ItemKind, PendingQuery, PlanCardBinding};
use rdy_engine::plan::api::{ItemState, PlanAnswerInput, PlanRow};
use rdy_engine::plan::compute::allowed_acts;
use rdy_engine::readiness::{Candidate, assess};
use rdy_engine::tensions::TensionKind;
use rdy_engine::{DecisionInput, Rdy, Session, WriteRequest, WriteResult};
use rdy_model::assessment::AssessmentPlanAnswerRef;
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::plan::{MAX_PLAN_ANSWERS, PlanAct, PlanKind};
use rdy_model::{Block, FieldValue, Role};
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

const AT_MS: i64 = 1_760_000_000_000;
const SUMMARY: &str = "test summary";
const PERSON: &str = "human:founder";

fn repo(n: u8) -> RepoId {
    RepoId([n; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn writer() -> Session {
    Session::human(PERSON.into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human(PERSON.into(), Role::Approver).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: rdy_engine::parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

/// Answers each open tension item of view `view` with `confirm`, and each open item on stable
/// content by its first allowed act, as the person.
fn answer_open_items(r: &mut Rdy, view: &str) {
    let Ok(reply) = r.read().view_plan(view) else {
        return;
    };
    let inputs: Vec<PlanAnswerInput> = reply
        .items
        .iter()
        .filter_map(|row| {
            let PlanRow::Live(i) = row else { return None };
            if i.state != ItemState::Open {
                return None;
            }
            let act = match i.kind {
                PlanKind::Tension => PlanAct::Confirm,
                PlanKind::UpdateImplementation => return None,
                kind if i.stable => *allowed_acts(kind).first()?,
                _ => return None,
            };
            Some(PlanAnswerInput {
                item: i.id.clone(),
                act,
                reason: Some("test: checked".into()),
            })
        })
        .collect();
    for chunk in inputs.chunks(MAX_PLAN_ANSWERS) {
        r.answer_plan(&writer(), view, chunk).unwrap();
    }
}

/// Proposes view `view` with its open items answered; returns the version and its id.
fn propose(r: &mut Rdy, view: &str) -> (Hash, rdy_engine::ProposalInfo) {
    answer_open_items(r, view);
    let p = r
        .propose(&writer(), view, view, Some(SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e} {:?}", e.issues));
    (p.id.parse().unwrap(), p)
}

/// Confirms each exception beside version `p`, then lands it.
fn land_version(r: &mut Rdy, p: &rdy_engine::ProposalInfo) {
    let q = r
        .read()
        .pending(&PendingQuery {
            person: PERSON.into(),
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
            &founder(),
            PERSON,
            AnswerRequest {
                r#ref: item,
                option: "confirm".into(),
                ..AnswerRequest::default()
            },
        )
        .unwrap();
    }
    r.land(&founder(), &p.id).unwrap();
}

fn land(r: &mut Rdy, view: &str) -> Hash {
    let (v, p) = propose(r, view);
    land_version(r, &p);
    v
}

/// A cited claim, as the cited side delivers it.
fn claim(id_: &str, body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: "claim".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::new(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

/// A cited instruction, selector `all`, with `locked` as given.
fn rule(id_: &str, body: &str, locked: bool) -> Block {
    let mut b = claim(id_, body);
    b.block_type = "instruction".into();
    b.fields = BTreeMap::from([
        (
            "applies_to".to_string(),
            FieldValue::List(vec![FieldValue::Str("all".into())]),
        ),
        ("locked".to_string(), FieldValue::Bool(locked)),
    ]);
    b
}

fn live() -> ForeignDerived {
    ForeignDerived::default()
}

fn deprecated() -> ForeignDerived {
    ForeignDerived {
        status: None,
        lifecycle: Some("deprecated".into()),
        superseded_by: vec![],
    }
}

/// A link delivery stream of `from` under `profile` holding `selected`, written to `dir/name`.
fn stream(
    dir: &Path,
    name: &str,
    from: RepoId,
    profile: &str,
    selected: &[(&Block, ForeignDerived)],
) -> File {
    let mut selected = selected.to_vec();
    selected.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    let versions: Vec<ForeignVersion> = selected
        .iter()
        .map(|(b, d)| ForeignVersion {
            id: b.id.clone(),
            version: b.version_hash(),
            content: b.content_hash(),
            encoding: 0,
            derived: d.clone(),
        })
        .collect();
    let delivery = DeliveryRecord {
        publication: ForeignPublication {
            tag: name.into(),
            profile: profile.into(),
            profile_version: None,
        },
        selection: selection_identity(versions.iter().map(|v| (&v.id, &v.version))).unwrap(),
        versions,
        shells: vec![],
    };
    let header = StreamHeader::link_delivery(baseline_features(), from, Hash::of(name.as_bytes()));
    let mut w = StreamWriter::new_delivery(Vec::new(), &header, &delivery).unwrap();
    for (b, _) in &selected {
        let o = codec::encode_new(kinds::BLOCK, *b);
        w.record(&StreamRecord::Present {
            key: ObjectKey {
                kind: kinds::BLOCK,
                hash: Hash::of(&o.payload),
            },
            encoding: o.version,
            payload: o.payload,
        })
        .unwrap();
    }
    let path = dir.join(name);
    std::fs::write(&path, w.finish().unwrap().0).unwrap();
    File::open(path).unwrap()
}

/// A project whose landed doc holds the section `s` and the repo links of `links` (alias, repo,
/// profile). `r` is open between steps; `view` closes it, reads a snapshot and reopens nothing.
struct Fx {
    dir: tempfile::TempDir,
    files: tempfile::TempDir,
    r: Option<Rdy>,
    s: BlockId,
    links: BTreeMap<String, (BlockId, RepoId, String)>,
}

fn fixture(links: &[(&str, RepoId, &str)]) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d",
               "fields": {"name": "guide", "title": "Guide", "doc_type": "overview"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d",
               "fields": {"title": "Rules"}}),
    ];
    for (alias, from, profile) in links {
        ops.push(
            json!({"op": "create", "type": "repo_link", "id": format!("${alias}"),
                        "parent": "$d",
                        "fields": {"alias": alias, "repo": from.to_string(), "profile": profile}}),
        );
    }
    let w = put(&mut r, "seed", Value::Array(ops));
    land(&mut r, "seed");
    let links = links
        .iter()
        .map(|(a, from, p)| {
            (
                a.to_string(),
                (id(&w.created[&format!("${a}")]), *from, p.to_string()),
            )
        })
        .collect();
    Fx {
        dir,
        files: tempfile::tempdir().unwrap(),
        r: Some(r),
        s: id(&w.created["$s"]),
        links,
    }
}

impl Fx {
    /// The open engine handle, reopened after a `view`.
    fn rdy(&mut self) -> &mut Rdy {
        if self.r.is_none() {
            self.r = Some(Rdy::open(self.dir.path()).unwrap());
        }
        self.r.as_mut().unwrap()
    }

    /// Runs `f` on a snapshot of the store, closing the engine handle first and the store after.
    fn view<T>(&mut self, f: impl FnOnce(&Snapshot) -> T) -> T {
        if let Some(r) = self.r.take() {
            r.close().unwrap();
        }
        let handle = StoreHandle::open(self.dir.path()).unwrap();
        let out = {
            let snap = handle.snapshot().unwrap();
            f(&snap)
        };
        handle.close().unwrap();
        out
    }

    fn link(&self, alias: &str) -> BlockId {
        self.links[alias].0.clone()
    }

    fn source(&self, alias: &str, at_ms: i64) -> ForeignSource {
        ForeignSource::Link {
            link: self.link(alias).to_string(),
            site: "hub.example".into(),
            at_ms,
        }
    }

    /// Takes the delivery `selected` into link `alias`, sent at `at_ms`, of set `set`.
    fn deliver_at(
        &mut self,
        alias: &str,
        name: &str,
        at_ms: i64,
        set: Option<ForeignSet>,
        selected: &[(&Block, ForeignDerived)],
    ) -> Hash {
        let (_, from, profile) = self.links[alias].clone();
        let source = self.source(alias, at_ms);
        let file = stream(self.files.path(), name, from, &profile, selected);
        take_delivery(self.rdy(), &file, &source, set)
            .unwrap_or_else(|e| panic!("take {name}: {e:?}"))
            .entry
    }

    fn deliver(
        &mut self,
        alias: &str,
        name: &str,
        set: Option<ForeignSet>,
        selected: &[&Block],
    ) -> Hash {
        let pairs: Vec<(&Block, ForeignDerived)> = selected.iter().map(|b| (*b, live())).collect();
        self.deliver_at(alias, name, AT_MS, set, &pairs)
    }

    /// The open adopt card of link `alias`.
    fn card(&mut self, alias: &str) -> Item {
        let link = self.link(alias);
        let q = self
            .rdy()
            .read()
            .pending(&PendingQuery {
                person: PERSON.into(),
                session_ref: None,
                max_tokens: CARD_MAX_TOKENS,
            })
            .unwrap();
        let mut all: Vec<Item> = q
            .items
            .into_iter()
            .filter(|i| i.kind == ItemKind::Adopt)
            .filter(|i| {
                matches!(&i.plan_binding, Some(PlanCardBinding::Adopt { link: l, .. }) if *l == link)
            })
            .collect();
        assert_eq!(all.len(), 1, "{alias}: {all:#?}");
        all.remove(0)
    }

    /// The person answers the open adopt card of `alias` with `option`.
    fn answer(&mut self, alias: &str, option: &str) {
        let card = self.card(alias);
        self.rdy()
            .answer(
                &founder(),
                PERSON,
                AnswerRequest {
                    r#ref: card.r#ref.clone(),
                    option: option.into(),
                    card_hash: Some(card.card_hash.clone()),
                    note: (option == "keep").then(|| "Not now.".to_string()),
                    relay: RelayInput::default(),
                    ..AnswerRequest::default()
                },
            )
            .unwrap_or_else(|e| panic!("{option} {alias}: {e:?}"));
    }
}

/// A project that cites a deprecated claim and a live one, with a write in view `p` that follows
/// the deprecated one and builds on the live one, proposed with its tension item answered. The
/// version, its landing and a second delivery that changes the live claim follow.
#[test]
fn rpl27_adapter_reads_match_readiness_facts() {
    let mut fx = fixture(&[("core", repo(7), "pf-AGNT1")]);
    let old = claim("c-FRGND", "Old services answer on port eighty.");
    let retry = claim("c-FRGNP", "Retries back off.");
    let first = fx.deliver_at(
        "core",
        "v1",
        AT_MS,
        None,
        &[(&old, deprecated()), (&retry, live())],
    );
    let s = fx.s.to_string();
    put(
        fx.rdy(),
        "p",
        json!([
            {"op": "create", "type": "claim", "id": "$o", "parent": s,
             "body": "Follow {{core:c-FRGND}}."},
            {"op": "create", "type": "claim", "id": "$b", "parent": s,
             "body": "Builds on {{core:c-FRGNP}}."}
        ]),
    );
    let (version, p) = propose(fx.rdy(), "p");
    // the engine's own facts for the version, with the answers its stored assessment holds
    let expected = {
        let r = fx.rdy();
        let st = r.read();
        let pref = r.resolve_proposal(&p.id).unwrap();
        let rec = st.reader().proposal(&version).unwrap();
        let c = Candidate::stored(&st, pref, rec.clone()).unwrap();
        let a = assess(&st, &c, &st.gate_config())
            .unwrap()
            .assessment
            .expect("ready");
        let answers: BTreeMap<Hash, AssessmentPlanAnswerRef> = a
            .tensions
            .iter()
            .filter_map(|t| Some((t.id, t.answer.clone()?)))
            .collect();
        let rows: Vec<_> = st
            .tensions_of_changes(&rec.changes)
            .unwrap()
            .into_iter()
            .map(|t| {
                let status = st.tension_status(&t);
                (t, status)
            })
            .collect();
        foreign_tension_gate(&rows, &answers)
    };
    assert_eq!(expected.len(), 1, "{expected:#?}");
    let gate = fx.view(|snap| snap.foreign_tension_gate(&version).unwrap());
    assert_eq!(gate, expected);
    let f = &gate[0];
    assert_eq!(f.kind, TensionKind::DeprecatedReference);
    assert_eq!((f.repo, f.entry), (repo(7), first));
    assert_eq!(f.target, ForeignId::new(repo(7), id("c-FRGND")));
    assert!(f.answer.is_some(), "the author's answer rides on the fact");
    // the version writes no instruction and no deviation
    fx.view(|snap| {
        assert_eq!(snap.lock_facts(&version).unwrap(), vec![]);
        assert_eq!(snap.lock_changes(&version).unwrap(), vec![]);
        let e = snap.foreign_tension_gate(&Hash::of(b"none")).unwrap_err();
        assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    });

    // landed, the citations are listed by cited repo, with their pins and the delivering entry
    let (_, p) = (version, p);
    land_version(fx.rdy(), &p);
    let source = format!("foreign/link/{}", fx.link("core"));
    let refs = fx.view(|snap| snap.foreign_refs().unwrap());
    assert_eq!(refs.len(), 1, "{refs:#?}");
    assert_eq!(
        (refs[0].repo, refs[0].aliases.clone()),
        (repo(7), vec!["core".to_string()])
    );
    assert_eq!(refs[0].refs.len(), 2, "{refs:#?}");
    for r in &refs[0].refs {
        assert!(
            r.pin.is_some() && !r.baseline && r.links.is_empty(),
            "{r:#?}"
        );
        assert_eq!(r.entry, Some(first));
        assert_eq!(r.source.as_deref(), Some(source.as_str()));
        assert_eq!(r.held, r.current);
    }
    assert!(refs[0].refs.windows(2).all(|w| w[0].citer < w[1].citer));
    let cites_retry = refs[0]
        .refs
        .iter()
        .find(|r| r.target.id == id("c-FRGNP"))
        .unwrap();
    assert_eq!(cites_retry.held, Some(retry.version_hash()));

    // a second delivery changes the live claim: its suspect names the new entry, the selection
    // change names both versions, and the pin still names the first
    let retry2 = claim("c-FRGNP", "Retries back off, then stop.");
    let second = fx.deliver_at(
        "core",
        "v2",
        AT_MS + 1,
        None,
        &[(&old, deprecated()), (&retry2, live())],
    );
    let link = fx.link("core");
    fx.view(|snap| {
        let signals = snap.foreign_signals().unwrap();
        let changed: Vec<_> = signals
            .suspects
            .iter()
            .filter(|s| s.target.id == id("c-FRGNP"))
            .collect();
        assert_eq!(changed.len(), 1, "{signals:#?}");
        assert_eq!(
            (changed[0].reason.as_str(), changed[0].entry),
            ("changed", Some(second))
        );
        assert!(signals.tensions.iter().all(|t| t.repo == repo(7)));
        let rows = snap.selection_changes(&link, Some(first)).unwrap();
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].edit, rows[0].before, rows[0].after, rows[0].entry),
            (
                SelectionEdit::Changed,
                Some(retry.version_hash()),
                Some(retry2.version_hash()),
                second
            )
        );
        assert_eq!(
            (rows[0].repo, &rows[0].link, &rows[0].id),
            (repo(7), &link, &id("c-FRGNP"))
        );
        // from before the first delivery, both are added, each at the entry that touched it last
        let all = snap.selection_changes(&link, None).unwrap();
        assert_eq!(
            all.iter().map(|r| (r.edit, r.entry)).collect::<Vec<_>>(),
            vec![
                (SelectionEdit::Added, first),
                (SelectionEdit::Added, second)
            ]
        );
        // from the newest entry, nothing changed; a stranger is refused
        assert_eq!(snap.selection_changes(&link, Some(second)).unwrap(), vec![]);
        let e = snap
            .selection_changes(&link, Some(Hash::of(b"x")))
            .unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{e}");
        assert_eq!(
            snap.selection_changes(&id("rl-FRGNZ"), None).unwrap(),
            vec![]
        );
    });
    // a later delivery that removes the claim reads as a removal from the second entry on
    let third = fx.deliver_at("core", "v3", AT_MS + 2, None, &[(&old, deprecated())]);
    fx.view(|snap| {
        let rows = snap.selection_changes(&link, Some(second)).unwrap();
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert_eq!(
            (rows[0].edit, rows[0].before, rows[0].after, rows[0].entry),
            (
                SelectionEdit::Removed,
                Some(retry2.version_hash()),
                None,
                third
            )
        );
    });
}

/// The blocks that cite a changed cited block, with the owners the live policy names for each.
#[test]
fn rpl27_blast_radius_uses_policy_owners() {
    let mut fx = fixture(&[("core", repo(7), "pf-AGNT1")]);
    let retry = claim("c-FRGNP", "Retries back off.");
    let other = claim("c-FRGNQ", "Timeouts are short.");
    fx.deliver("core", "v1", None, &[&retry, &other]);
    // two docs cite the claim; a third cites another; the policy gives one doc an owner
    let w = put(
        fx.rdy(),
        "cites",
        json!([
            {"op": "create", "type": "doc", "id": "$pay", "fields": {"name": "pay", "title": "Pay", "doc_type": "page"}},
            {"op": "create", "type": "claim", "id": "$pc", "parent": "$pay", "body": "Pay {{core:c-FRGNP}}."},
            {"op": "create", "type": "doc", "id": "$misc", "fields": {"name": "misc", "title": "Misc", "doc_type": "page"}},
            {"op": "create", "type": "claim", "id": "$mc", "parent": "$misc", "body": "Misc {{core:c-FRGNP}}."},
            {"op": "create", "type": "doc", "id": "$apart", "fields": {"name": "apart", "title": "Apart", "doc_type": "page"}},
            {"op": "create", "type": "claim", "id": "$ac", "parent": "$apart", "body": "Apart {{core:c-FRGNQ}}."}
        ]),
    );
    land(fx.rdy(), "cites");
    let (pc, mc) = (id(&w.created["$pc"]), id(&w.created["$mc"]));
    // no policy: no owners
    let plain = fx.view(|snap| {
        snap.foreign_blast_radius(&repo(7), &[id("c-FRGNP")])
            .unwrap()
    });
    assert_eq!(plain.len(), 2, "{plain:#?}");
    assert!(
        plain
            .iter()
            .all(|b| b.owners.is_empty() && b.owner_path.is_none())
    );
    // the live policy owns doc `pay`
    let d = put(
        fx.rdy(),
        "policy",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "rules", "title": "Rules", "doc_type": "page"}},
            {"op": "create", "type": "decision", "id": "$dec", "parent": "$d",
             "fields": {"statement": "Adopt the policy", "status": "proposed", "needs_content": false}}
        ]),
    );
    let (doc, dec) = (d.created["$d"].clone(), id(&d.created["$dec"]));
    fx.rdy()
        .write(
            &writer(),
            WriteRequest {
                view: "policy".into(),
                ops: rdy_engine::parse_ops(
                    json!([{"op": "create", "type": "authority_policy", "parent": doc,
                        "fields": {"key": "authority-policy", "agent_may_accept": [],
                                   "roles": {"approvers": ["human:ann"]}, "require": [],
                                   "owners": [{"path": "pay", "owners": ["human:ann"]}]}}])
                    .as_array()
                    .unwrap()
                    .clone(),
                )
                .unwrap(),
                justification: Some(rdy_model::Justification::Implements(vec![dec])),
                message: "write the policy".into(),
            },
        )
        .unwrap();
    land(fx.rdy(), "policy");
    let rows = fx.view(|snap| {
        snap.foreign_blast_radius(&repo(7), &[id("c-FRGNP")])
            .unwrap()
    });
    assert_eq!(rows.len(), 2, "{rows:#?}");
    assert!(rows.windows(2).all(|w| w[0].block < w[1].block), "id order");
    let by = |b: &BlockId| rows.iter().find(|r| r.block == *b).unwrap();
    assert_eq!(by(&pc).owners, vec!["human:ann".to_string()]);
    assert_eq!(by(&pc).owner_path.as_deref(), Some("pay"));
    assert!(by(&mc).owners.is_empty() && by(&mc).owner_path.is_none());
    assert_eq!(by(&pc).cites, vec![ForeignId::new(repo(7), id("c-FRGNP"))]);
    // an unchanged or foreign-repo target lists nothing; two changed targets list each citer once
    fx.view(|snap| {
        assert_eq!(
            snap.foreign_blast_radius(&repo(7), &[id("c-FRGNZ")])
                .unwrap(),
            vec![]
        );
        assert_eq!(
            snap.foreign_blast_radius(&repo(8), &[id("c-FRGNP")])
                .unwrap(),
            vec![]
        );
        let both = snap
            .foreign_blast_radius(&repo(7), &[id("c-FRGNP"), id("c-FRGNQ")])
            .unwrap();
        assert_eq!(both.len(), 3, "{both:#?}");
    });
}

/// The facts use the foreign cut the snapshot captured: a later delivery changes the live read
/// and not an earlier snapshot's.
#[test]
fn rpl27_lock_facts_use_captured_foreign_cut() {
    let mut fx = fixture(&[("org", repo(7), "pf-AGNT1")]);
    let lock = rule("ai-KPT01", "Every claim names its owner.", true);
    let first = fx.deliver("org", "v1", Some(ForeignSet::Organization), &[&lock]);
    // a deviation written before the lock has force, and a local locked rule of the project
    let s = fx.s.to_string();
    fx.rdy()
        .decide(
            &writer(),
            "dev",
            DecisionInput {
                parent: s,
                statement: "Here we depart from the owner rule.".into(),
                title: Some("Depart".into()),
                deviates_from: vec!["org:ai-KPT01".into()],
                ..Default::default()
            },
        )
        .unwrap();
    let (version, _) = propose(fx.rdy(), "dev");
    // the person adopts the organization's lock: it now has force
    fx.answer("org", "adopt");
    let w = put(
        fx.rdy(),
        "own",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "own", "title": "Own", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Reasons"}}
        ]),
    );
    let reason = fx
        .rdy()
        .decide(
            &writer(),
            "own",
            DecisionInput {
                parent: w.created["$s"].clone(),
                statement: "own is decided".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let i = put(
        fx.rdy(),
        "own",
        json!([{"op": "create", "type": "instruction", "id": "$i", "parent": w.created["$d"],
                "fields": {"applies_to": ["all"], "locked": true}, "body": "Claims leave out owners.",
                "links": {"decided_by": [{"target": reason}]}}]),
    );
    let (own_version, _) = propose(fx.rdy(), "own");
    fx.r.take().unwrap().close().unwrap();

    // one handle: a snapshot, then a later delivery through the handle's own writer
    let handle = StoreHandle::open(fx.dir.path()).unwrap();
    let before = handle.snapshot().unwrap();
    let facts = before.lock_facts(&version).unwrap();
    assert_eq!(facts.len(), 1, "{facts:#?}");
    assert_eq!(
        (facts[0].alias.as_str(), facts[0].version, facts[0].entry),
        ("org", lock.version_hash(), Some(first))
    );
    assert_eq!(facts[0].set, ForeignSet::Organization);
    assert_eq!(facts[0].target, ForeignId::new(repo(7), id("ai-KPT01")));
    assert_eq!(before.unadopted_locked().unwrap(), vec![]);
    // the project's own locked rule is a set owner's lock change
    let changes = before.lock_changes(&own_version).unwrap();
    assert_eq!(changes.len(), 1, "{changes:#?}");
    assert_eq!(
        (
            changes[0].id.to_string(),
            changes[0].edit,
            changes[0].before
        ),
        (i.created["$i"].clone(), LockEdit::Locked, None)
    );
    // the set unlocks the rule in a later delivery
    let unlocked = rule("ai-KPT01", "Every claim names its owner.", false);
    let (link, from, profile) = fx.links["org"].clone();
    let file = stream(
        fx.files.path(),
        "v2",
        from,
        &profile,
        &[(&unlocked, live())],
    );
    let second = handle
        .take_delivery(
            &file,
            DeliverySource {
                link,
                site: "hub.example".into(),
                at_ms: AT_MS + 5,
                set: Some(ForeignSet::Organization),
            },
        )
        .unwrap();
    let after = handle.snapshot().unwrap();
    // the earlier snapshot keeps its cut; the new one lists the delivered change with its time
    assert_eq!(before.unadopted_locked().unwrap(), vec![]);
    let rows = after.unadopted_locked().unwrap();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(
        (rows[0].change, rows[0].entry, rows[0].at_ms),
        (LockEdit::Unlocked, second.entry, AT_MS + 5)
    );
    // the facts of the version read the adopted baseline, which both cuts hold alike
    assert_eq!(after.lock_facts(&version).unwrap(), facts);
    assert_eq!(before.lock_facts(&version).unwrap(), facts);
    drop((before, after));
    handle.close().unwrap();
}

#[test]
fn rpl27_link_adoption_keep_on_first_delivery_is_absent() {
    let mut fx = fixture(&[("org", repo(7), "pf-AGNT1"), ("lib", repo(8), "pf-AGNT2")]);
    // before any delivery: one row per repo link, in alias order, with nothing yet
    let rows = fx.view(|snap| snap.link_adoption().unwrap());
    assert_eq!(
        rows.iter().map(|r| r.alias.as_str()).collect::<Vec<_>>(),
        ["lib", "org"]
    );
    for r in &rows {
        assert_eq!(
            (r.state, &r.last, &r.adopted),
            (LinkState::None, &None, &None)
        );
    }
    assert_eq!(
        (rows[1].link.clone(), rows[1].repo),
        (fx.link("org"), repo(7))
    );
    let lock = rule("ai-KPT01", "Every claim names its owner.", true);
    let first = fx.deliver("org", "v1", Some(ForeignSet::Organization), &[&lock]);
    let delivered = |first: Hash| LastDelivery {
        entry: first,
        at_ms: AT_MS,
        set: Some(ForeignSet::Organization),
        instructions: 1,
    };
    let rows = fx.view(|snap| snap.link_adoption().unwrap());
    let org = &rows[1];
    assert_eq!(org.state, LinkState::Delivered);
    assert_eq!(org.last, Some(delivered(first)));
    assert_eq!(org.adopted, None, "a delivery alone adopts nothing");
    // the other link is untouched
    assert_eq!((rows[0].state, &rows[0].last), (LinkState::None, &None));
    // a keep on the first delivery leaves none of its rules in force: still no adopted delivery
    fx.answer("org", "keep");
    let rows = fx.view(|snap| snap.link_adoption().unwrap());
    assert_eq!(rows[1].last, Some(delivered(first)));
    assert_eq!(rows[1].adopted, None);
}

#[test]
fn rpl27_link_adoption_names_the_last_adopt_answer() {
    let mut fx = fixture(&[("org", repo(7), "pf-AGNT1")]);
    let (a, b) = (
        rule("ai-KPT0A", "Every claim names its owner.", true),
        rule("ai-KPT0B", "Prefer short sections.", false),
    );
    let org = Some(ForeignSet::Organization);
    let first = fx.deliver_at(
        "org",
        "v1",
        AT_MS,
        org.clone(),
        &[(&a, live()), (&b, live())],
    );
    fx.answer("org", "adopt");
    let adopted = |entry: Hash, at_ms: i64| {
        Some(AdoptedDelivery {
            entry,
            at_ms,
            set: Some(ForeignSet::Organization),
        })
    };
    let row = || |snap: &Snapshot| snap.link_adoption().unwrap().remove(0);
    let r = fx.view(row());
    assert_eq!(r.adopted, adopted(first, AT_MS));
    assert_eq!(
        r.last.as_ref().map(|l| (l.entry, l.instructions)),
        Some((first, 2))
    );
    // a changed rule is delivered; the adopted delivery stays the first until a person answers
    let a2 = rule("ai-KPT0A", "Every claim names its owner and team.", true);
    let second = fx.deliver_at(
        "org",
        "v2",
        AT_MS + 1,
        org.clone(),
        &[(&a2, live()), (&b, live())],
    );
    let r = fx.view(row());
    assert_eq!(
        r.last.as_ref().map(|l| (l.entry, l.at_ms)),
        Some((second, AT_MS + 1))
    );
    assert_eq!(r.adopted, adopted(first, AT_MS));
    // a keep answer leaves it as it was
    fx.answer("org", "keep");
    let r = fx.view(row());
    assert_eq!(r.adopted, adopted(first, AT_MS));
    // a delivery of derived facts only leaves it too
    let third = fx.deliver_at(
        "org",
        "v3",
        AT_MS + 2,
        org.clone(),
        &[
            (
                &a2,
                ForeignDerived {
                    status: Some("accepted".into()),
                    lifecycle: None,
                    superseded_by: vec![],
                },
            ),
            (&b, live()),
        ],
    );
    let r = fx.view(row());
    assert_eq!(r.last.as_ref().map(|l| l.entry), Some(third));
    assert_eq!(r.adopted, adopted(first, AT_MS));
    // another change is delivered and adopted: the last adopt answer names it
    let b2 = rule("ai-KPT0B", "Prefer short sections and lists.", false);
    let fourth = fx.deliver_at("org", "v4", AT_MS + 3, org, &[(&a2, live()), (&b2, live())]);
    fx.answer("org", "adopt");
    let r = fx.view(row());
    assert_eq!(r.adopted, adopted(fourth, AT_MS + 3));
    assert_eq!(r.last.as_ref().map(|l| l.entry), Some(fourth));
    // a team set moves the adopted set with the answer
    let team = Some(ForeignSet::Team("docs".into()));
    let fifth = fx.deliver_at(
        "org",
        "v5",
        AT_MS + 4,
        team.clone(),
        &[(&a2, live()), (&b2, live())],
    );
    fx.answer("org", "adopt");
    let r = fx.view(row());
    assert_eq!(
        r.adopted,
        Some(AdoptedDelivery {
            entry: fifth,
            at_ms: AT_MS + 4,
            set: team.clone()
        })
    );
    assert_eq!(r.last.as_ref().map(|l| l.set.clone()), Some(team));
}

#[test]
fn rpl27_link_adoption_counts_an_empty_set_delivery() {
    let mut fx = fixture(&[("org", repo(7), "pf-AGNT1")]);
    let org = Some(ForeignSet::Organization);
    // a first delivery for a set that selects no instruction still counts: zero instructions
    let note = claim("c-FRGNP", "Retries back off.");
    let first = fx.deliver("org", "v1", org.clone(), &[&note]);
    let r = fx.view(|snap| snap.link_adoption().unwrap().remove(0));
    assert_eq!(r.state, LinkState::Delivered);
    assert_eq!(
        r.last,
        Some(LastDelivery {
            entry: first,
            at_ms: AT_MS,
            set: org.clone(),
            instructions: 0
        })
    );
    assert_eq!(r.adopted, None, "no instruction, so no adopt item");
    // a delivery with a rule is adopted
    let lock = rule("ai-KPT01", "Every claim names its owner.", true);
    let second = fx.deliver_at(
        "org",
        "v2",
        AT_MS + 1,
        org.clone(),
        &[(&note, live()), (&lock, live())],
    );
    fx.answer("org", "adopt");
    let r = fx.view(|snap| snap.link_adoption().unwrap().remove(0));
    assert_eq!(
        r.last.as_ref().map(|l| (l.entry, l.instructions)),
        Some((second, 1))
    );
    assert_eq!(r.adopted.as_ref().map(|a| a.entry), Some(second));
    // a delivery that removes every rule is adopted too: its baseline is empty
    let third = fx.deliver_at("org", "v3", AT_MS + 2, org.clone(), &[(&note, live())]);
    let r = fx.view(|snap| snap.link_adoption().unwrap().remove(0));
    assert_eq!(
        r.last.as_ref().map(|l| (l.entry, l.instructions)),
        Some((third, 0))
    );
    assert_eq!(r.adopted.as_ref().map(|a| a.entry), Some(second));
    fx.answer("org", "adopt");
    let r = fx.view(|snap| snap.link_adoption().unwrap().remove(0));
    assert_eq!(
        r.adopted,
        Some(AdoptedDelivery {
            entry: third,
            at_ms: AT_MS + 2,
            set: org.clone()
        })
    );
    // a revoked link keeps what it last delivered and what it adopted
    let source = fx.source("org", AT_MS + 3);
    revoke_link(fx.rdy(), &source).unwrap();
    let r = fx.view(|snap| snap.link_adoption().unwrap().remove(0));
    assert_eq!(r.state, LinkState::Revoked);
    assert_eq!(
        r.last.as_ref().map(|l| (l.entry, l.instructions)),
        Some((third, 0))
    );
    assert_eq!(r.adopted.as_ref().map(|a| a.entry), Some(third));
}
