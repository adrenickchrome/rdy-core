//! rpl23 (repo links, D-8VJAZ, D-MEMMQ, D-4W1ZR): organization and team locks.
//!
//! A cited instruction is locked here when its link's instruction baseline pins it at a version
//! whose `locked` is true, and the link's last adopt answer served an organization or team set:
//! the same foreign identity and adopted baseline that `rdy_instructions` reads. A no-set cited
//! lock, and a lock that a delivery only offers, have no force.
//!
//! - A decision that departs from a locked instruction is refused with `E_INSTRUCTION_LOCKED`,
//!   on the write and again at the landing.
//! - An adopt answer that would lock an instruction a live deviation departs from is refused with
//!   the revoke-first fix. It writes nothing, and the card stays open with its hash.
//! - The project, person and cited layers never take a lock's force away.
//! - `unadopted_locked` compares each delivery with the baseline: a first delivery's locks count
//!   against an empty baseline, with that delivery's time.
use rdy_engine::answer::AnswerRequest;
use rdy_engine::answer::relay::RelayInput;
use rdy_engine::foreign_delivery::{DeliveryTaken, take_delivery};
use rdy_engine::foreign_locks::{LockChange, LockEdit};
use rdy_engine::instructions::{
    self, BOUNDS, Caller, Contribution, Layer, LoadRequest, PersonHost, Reply,
};
use rdy_engine::instructions_config::{PersonPath, PersonSource};
use rdy_engine::pending::{Item, ItemKind, PlanCardBinding};
use rdy_engine::{DecisionInput, Rdy, RdyError, Scope, Session, WriteResult};
use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPin, ForeignPublication, ForeignSet, ForeignSource,
    ForeignVersion, selection_identity,
};
use rdy_model::{Block, BlockId, EvidenceKind, FieldValue, Hash, RepoId, Role};
use rdy_qa::fixtures::{approver, land, write, writer};
use rdy_qa::intake::{PERSON, pending};
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::segment::Location;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

const LOCKED: &str = "E_INSTRUCTION_LOCKED";
const LOCKED_FIX: &str = "add an instruction on top of it, or ask the set's owners to change it there: rdy_write a new instruction / rdy new instruction";
const AT_MS: i64 = 1_760_000_000_000;

fn repo(n: u8) -> RepoId {
    RepoId([n; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn created(w: &WriteResult, name: &str) -> BlockId {
    BlockId::parse(&w.created[name]).unwrap()
}

fn put(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    write(r, view, ops).unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

/// A cited instruction, as the cited side delivers it: selector `all` and `locked`.
fn rule(id_: &str, body: &str, locked: bool) -> Block {
    Block {
        id: id(id_),
        block_type: "instruction".into(),
        parent: None,
        order: "V".into(),
        fields: BTreeMap::from([
            (
                "applies_to".to_string(),
                FieldValue::List(vec![FieldValue::Str("all".into())]),
            ),
            ("locked".to_string(), FieldValue::Bool(locked)),
        ]),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn pin_of(b: &Block) -> ForeignPin {
    ForeignPin {
        content: b.content_hash(),
        encoding: 0,
    }
}

/// A link delivery stream of `from` under `profile` holding `selected`, written to `dir/name`.
fn stream(dir: &Path, name: &str, from: RepoId, profile: &str, selected: &[&Block]) -> File {
    let mut selected = selected.to_vec();
    selected.sort_by(|a, b| a.id.cmp(&b.id));
    let versions: Vec<ForeignVersion> = selected
        .iter()
        .map(|b| ForeignVersion {
            id: b.id.clone(),
            version: b.version_hash(),
            content: b.content_hash(),
            encoding: 0,
            derived: ForeignDerived::default(),
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
    for b in &selected {
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
/// profile).
struct Fx {
    _d: tempfile::TempDir,
    files: tempfile::TempDir,
    r: Rdy,
    s: BlockId,
    links: BTreeMap<String, (BlockId, RepoId, String)>,
}

fn fixture(links: &[(&str, RepoId, &str)]) -> Fx {
    let (d, mut r) = rdy_qa::fixtures::project();
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d",
               "fields": {"title": "Guide", "doc_type": "overview"}}),
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
    land(&mut r, "seed").unwrap();
    let links = links
        .iter()
        .map(|(a, from, p)| {
            (
                a.to_string(),
                (created(&w, &format!("${a}")), *from, p.to_string()),
            )
        })
        .collect();
    Fx {
        _d: d,
        files: tempfile::tempdir().unwrap(),
        r,
        s: created(&w, "$s"),
        links,
    }
}

impl Fx {
    /// Takes the delivery `selected` into link `alias`, of set `set`.
    fn deliver(
        &mut self,
        alias: &str,
        name: &str,
        set: Option<ForeignSet>,
        selected: &[&Block],
    ) -> DeliveryTaken {
        let (link, from, profile) = self.links[alias].clone();
        let source = ForeignSource::Link {
            link: link.to_string(),
            site: "hub.example".into(),
            at_ms: AT_MS,
        };
        let file = stream(self.files.path(), name, from, &profile, selected);
        take_delivery(&mut self.r, &file, &source, set)
            .unwrap_or_else(|e| panic!("take {name}: {e:?}"))
    }

    fn link(&self, alias: &str) -> BlockId {
        self.links[alias].0.clone()
    }

    /// The adopt card of link `alias`.
    fn card(&self, alias: &str) -> Item {
        let link = self.link(alias);
        let mut all: Vec<Item> = adopt_items(&self.r)
            .into_iter()
            .filter(|i| {
                matches!(&i.plan_binding, Some(PlanCardBinding::Adopt { link: l, .. }) if *l == link)
            })
            .collect();
        assert_eq!(all.len(), 1, "{alias}: {all:#?}");
        all.remove(0)
    }

    /// The person adopts the open card of link `alias`.
    fn adopt(&mut self, alias: &str) {
        let card = self.card(alias);
        answer(&mut self.r, &card, "adopt", None)
            .unwrap_or_else(|e| panic!("adopt {alias}: {e:?}"));
    }

    /// Writes into view `view` a decision that departs from `target`, as rdy_decide does.
    fn decide_deviation(&mut self, view: &str, target: &str) -> Result<WriteResult, RdyError> {
        self.r.decide(
            &writer(),
            view,
            DecisionInput {
                parent: self.s.to_string(),
                statement: format!("Here we depart from {target}."),
                title: Some(format!("Depart from {target}")),
                deviates_from: vec![target.into()],
                ..Default::default()
            },
        )
    }

    /// Writes into view `view` a decision that departs from `target`, as rdy_write does.
    fn write_deviation(&mut self, view: &str, target: &str) -> Result<WriteResult, RdyError> {
        write(
            &mut self.r,
            view,
            json!([{"op": "create", "type": "decision", "id": "$dec", "parent": self.s.to_string(),
                    "fields": {"statement": format!("Our rule departs from {target}."),
                               "status": "proposed", "needs_content": false},
                    "links": {"deviates_from": [target]}}]),
        )
    }

    fn pins(&self, b: &BlockId) -> BTreeMap<ForeignId, ForeignPin> {
        self.r.read().block(&Scope::Canon, b).unwrap().foreign.pins
    }
}

fn adopt_items(r: &Rdy) -> Vec<Item> {
    pending(r)
        .items
        .into_iter()
        .filter(|i| i.kind == ItemKind::Adopt)
        .collect()
}

fn answer_as(
    r: &mut Rdy,
    s: &Session,
    card: &Item,
    option: &str,
    note: Option<&str>,
    relay: RelayInput,
) -> Result<Value, RdyError> {
    r.answer(
        s,
        PERSON,
        AnswerRequest {
            r#ref: card.r#ref.clone(),
            option: option.into(),
            card_hash: Some(card.card_hash.clone()),
            note: note.map(str::to_string),
            relay,
            ..AnswerRequest::default()
        },
    )
    .map(|i| serde_json::to_value(i).unwrap())
}

fn answer(r: &mut Rdy, card: &Item, option: &str, note: Option<&str>) -> Result<Value, RdyError> {
    answer_as(r, &approver(), card, option, note, RelayInput::default())
}

fn relayer() -> Session {
    Session::agent("agent:relay".into(), Some("r1".into()), Role::Writer).unwrap()
}

fn relay() -> RelayInput {
    RelayInput {
        relayed_by: Some("agent:relay".into()),
        evidence: Some("Yes, adopt it.".into()),
        question: Some("Adopt the delivery?".into()),
        session_ref: Some("chat-1".into()),
        kind: Some(EvidenceKind::Card),
    }
}

fn log_end(r: &Rdy) -> Location {
    r.store().log_end()
}

/// Asserts that a refusal is the locked-set one: `<shown> is locked by its <set> set`, `fix`.
fn assert_locked(e: &RdyError, shown: &str, set: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            LOCKED,
            format!("{shown} is locked by its {set} set").as_str(),
            fix
        ),
        "{e:#?}"
    );
}

/// Asserts that nothing was written since `end` and the card of `alias` stands with `hash`.
fn nothing_written(fx: &Fx, alias: &str, end: Location, hash: &str, what: &str) {
    assert_eq!(log_end(&fx.r), end, "{what}: nothing written");
    let view = format!("view/adopt-{}", &hash[..12]);
    let views = fx.r.store().refs_with_prefix(&view).unwrap();
    assert!(views.is_empty(), "{what}: no adopt view: {views:?}");
    let c = fx.card(alias);
    assert_eq!(c.card_hash, hash, "{what}: the same card stays open");
    assert!(
        fx.r.read().reader().answer_of(&c.r#ref).unwrap().is_none(),
        "{what}: no answer"
    );
}

/// The decision id a deviation write created.
fn decision_of(w: &WriteResult) -> BlockId {
    w.created
        .get("$decision")
        .or_else(|| w.created.get("$dec"))
        .map(|s| BlockId::parse(s).unwrap())
        .expect("a decision")
}

#[test]
fn rpl23_locked_set_refuses_deviation() {
    let mut fx = fixture(&[
        ("org", repo(7), "pf-AGNT1"),
        ("ops", repo(8), "pf-AGNT1"),
        ("lib", repo(9), "pf-AGNT1"),
    ]);
    let lock = rule("ai-KPT01", "Every claim names its owner.", true);
    let open = rule("ai-FRE01", "Prefer short sections.", false);
    let team = rule("ai-TEAM1", "Deploys run on weekdays.", true);
    let cited = rule("ai-CTD01", "Name every queue.", true);
    fx.deliver("org", "o1", Some(ForeignSet::Organization), &[&lock, &open]);
    fx.deliver(
        "ops",
        "t1",
        Some(ForeignSet::Team("platform".into())),
        &[&team],
    );
    fx.deliver("lib", "c1", None, &[&cited]);
    // a lock that a delivery only offers has no force: the deviation is written, not landed yet
    let early = fx
        .decide_deviation("early", "org:ai-KPT01")
        .unwrap_or_else(|e| panic!("an unadopted lock has no force: {e:?}"));
    let early_dec = decision_of(&early);
    for alias in ["org", "ops", "lib"] {
        fx.adopt(alias);
    }
    // the organization lock is in force now: rdy_decide is refused, and writes nothing
    let end = log_end(&fx.r);
    let e = fx.decide_deviation("d1", "org:ai-KPT01").unwrap_err();
    assert_locked(&e, "org:ai-KPT01", "organization", LOCKED_FIX);
    assert_eq!(log_end(&fx.r), end, "a refused decision writes nothing");
    // rdy_write is refused the same way, for a team lock too
    let e = fx.write_deviation("d2", "ops:ai-TEAM1").unwrap_err();
    assert_locked(&e, "ops:ai-TEAM1", "team platform", LOCKED_FIX);
    let e = fx.write_deviation("d2", "org:ai-KPT01").unwrap_err();
    assert_locked(&e, "org:ai-KPT01", "organization", LOCKED_FIX);
    assert_eq!(log_end(&fx.r), end, "a refused write writes nothing");
    // an unlocked rule of the set, and a no-set cited lock, may be departed from
    fx.decide_deviation("d3", "org:ai-FRE01")
        .unwrap_or_else(|e| panic!("an unlocked rule: {e:?}"));
    fx.write_deviation("d4", "lib:ai-CTD01")
        .unwrap_or_else(|e| panic!("a no-set cited lock has no force: {e:?}"));
    // the deviation written before the lock was adopted is refused at its landing
    let head = fx.r.read().canon_head();
    let st = fx.r.read();
    let p_changes: Vec<Hash> = st.included("early").to_vec();
    // the landing's lock facts name the new deviation from the locked instruction
    let facts = st.lock_facts(&p_changes).unwrap();
    assert_eq!(facts.len(), 1, "{facts:#?}");
    assert_eq!(facts[0].decision, early_dec);
    assert_eq!(facts[0].target, ForeignId::new(repo(7), id("ai-KPT01")));
    assert_eq!(facts[0].alias, "org");
    assert_eq!(facts[0].set, ForeignSet::Organization);
    assert_eq!(facts[0].version, lock.version_hash());
    drop(st);
    let e = land(&mut fx.r, "early").unwrap_err();
    assert_locked(&e, "org:ai-KPT01", "organization", LOCKED_FIX);
    assert_eq!(fx.r.read().canon_head(), head, "the landing is refused");
    fx.r.fsck().unwrap();
}

#[test]
fn rpl23_new_lock_requires_revoking_existing_deviation() {
    let mut fx = fixture(&[("org", repo(7), "pf-AGNT1")]);
    let link = fx.link("org");
    let open = rule("ai-RGM01", "Every claim names its owner.", false);
    fx.deliver("org", "v1", Some(ForeignSet::Organization), &[&open]);
    fx.adopt("org");
    // an unlocked rule in force: a live deviation from it lands
    let w = fx.decide_deviation("dv", "org:ai-RGM01").unwrap();
    let dec = decision_of(&w);
    land(&mut fx.r, "dv").unwrap();
    // the set locks the rule; adopting that would lock the live deviation
    let locked = rule("ai-RGM01", "Every claim names its owner.", true);
    fx.deliver("org", "v2", Some(ForeignSet::Organization), &[&locked]);
    let card = fx.card("org");
    let end = log_end(&fx.r);
    let e = answer(&mut fx.r, &card, "adopt", None).unwrap_err();
    assert_locked(
        &e,
        "org:ai-RGM01",
        "organization",
        &format!("revoke the deviation {dec} first, then answer the adopt item again"),
    );
    nothing_written(&fx, "org", end, &card.card_hash, "a locked adopt");
    let rule_id = ForeignId::new(repo(7), id("ai-RGM01"));
    assert_eq!(
        fx.pins(&link)[&rule_id],
        pin_of(&open),
        "the baseline stays"
    );
    // revoke the deviation first, then the adopt answer lands the lock
    fx.r.revoke(
        &writer(),
        "rv",
        &dec.to_string(),
        "the set locks the rule",
        &fx.s.to_string(),
        None,
    )
    .unwrap_or_else(|e| panic!("revoke: {e:?}"));
    land(&mut fx.r, "rv").unwrap();
    let card = fx.card("org");
    answer(&mut fx.r, &card, "adopt", None).unwrap_or_else(|e| panic!("adopt: {e:?}"));
    assert_eq!(
        fx.pins(&link)[&rule_id],
        pin_of(&locked),
        "the lock is in force"
    );
    // and a new deviation is refused
    let e = fx.decide_deviation("dv2", "org:ai-RGM01").unwrap_err();
    assert_locked(&e, "org:ai-RGM01", "organization", LOCKED_FIX);
    fx.r.fsck().unwrap();
}

#[test]
fn rpl23_failed_locked_adopt_has_zero_writes() {
    let mut fx = fixture(&[("ops", repo(8), "pf-AGNT1")]);
    let link = fx.link("ops");
    // a no-set lock is in force with no force: a deviation from it lands
    let locked = rule("ai-RGM01", "Deploys run on weekdays.", true);
    fx.deliver("ops", "v1", None, &[&locked]);
    fx.adopt("ops");
    let w = fx.write_deviation("dv", "ops:ai-RGM01").unwrap();
    let dec = decision_of(&w);
    land(&mut fx.r, "dv").unwrap();
    // the same rule now comes as the team platform's: the set alone gives the lock force
    fx.deliver(
        "ops",
        "v2",
        Some(ForeignSet::Team("platform".into())),
        &[&locked],
    );
    let card = fx.card("ops");
    let fix = format!("revoke the deviation {dec} first, then answer the adopt item again");
    let head = fx.r.read().canon_head();
    let pins = fx.pins(&link);
    let end = log_end(&fx.r);
    // a local answer, a relayed one and a retry: each refused, none writes a byte
    for (what, s, relay_input) in [
        ("local", approver(), RelayInput::default()),
        ("relayed", relayer(), relay()),
        ("retry", approver(), RelayInput::default()),
    ] {
        let e = answer_as(&mut fx.r, &s, &card, "adopt", None, relay_input).unwrap_err();
        assert_locked(&e, "ops:ai-RGM01", "team platform", &fix);
        nothing_written(&fx, "ops", end, &card.card_hash, what);
        assert_eq!(fx.r.read().canon_head(), head, "{what}: canon stands");
        assert_eq!(fx.pins(&link), pins, "{what}: the baseline stands");
    }
    // keep is no adoption: it records the reason
    answer(
        &mut fx.r,
        &card,
        "keep",
        Some("The deviation stays for now."),
    )
    .unwrap_or_else(|e| panic!("keep: {e:?}"));
    fx.r.fsck().unwrap();
}

/// A fake person host: the meta project at `meta`, read through `Rdy::open`.
struct Host {
    path: PersonPath,
}

impl PersonHost for Host {
    fn locate(&self) -> Result<PersonSource, RdyError> {
        Ok(PersonSource {
            path: self.path.clone(),
            origin: None,
            warnings: vec![],
            config_file: None,
        })
    }

    fn fetch(
        &self,
        meta: &Path,
        load: &LoadRequest,
        _caller: &Caller,
    ) -> Result<Contribution, RdyError> {
        let r = Rdy::open(meta)?;
        let part = instructions::collect(&r.read(), Layer::Person, load);
        r.close()?;
        part
    }
}

/// Writes a landed ruleset `name` with one locked instruction `body` (selector `all`) into `r`,
/// with its reason; returns the instruction's id and the changes of its landing.
fn local_rule(r: &mut Rdy, name: &str, body: &str) -> (String, Vec<Hash>) {
    let w = put(
        r,
        name,
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"name": name, "title": name, "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Reasons"}}
        ]),
    );
    let d = r
        .decide(
            &writer(),
            name,
            DecisionInput {
                parent: w.created["$s"].clone(),
                statement: format!("{name} is decided"),
                ..Default::default()
            },
        )
        .unwrap()
        .created["$decision"]
        .clone();
    let i = put(
        r,
        name,
        json!([{"op": "create", "type": "instruction", "id": "$i", "parent": w.created["$d"],
                "fields": {"applies_to": ["all"], "locked": true}, "body": body,
                "links": {"decided_by": [{"target": d}]}}]),
    );
    let changes: Vec<Hash> = r.read().included(name).to_vec();
    land(r, name).unwrap();
    (i.created["$i"].clone(), changes)
}

#[test]
fn rpl23_project_person_and_cited_layers_cannot_override_lock() {
    // one repo under two profiles: `org` serves the organization set, `lib` serves no set
    let mut fx = fixture(&[("org", repo(7), "pf-AGNT1"), ("lib", repo(7), "pf-AGNT2")]);
    let lock = rule("ai-KPT01", "Every claim names its owner.", true);
    let loose = rule("ai-KPT01", "Claims may leave out their owner.", false);
    fx.deliver("org", "o1", Some(ForeignSet::Organization), &[&lock]);
    fx.deliver("lib", "c1", None, &[&loose]);
    fx.adopt("org");
    fx.adopt("lib");
    // the project layer: its own locked rule, and a decision that departs from the lock
    let (own, own_changes) = local_rule(&mut fx.r, "project-rules", "Claims leave out owners.");
    let e = fx.decide_deviation("p1", "org:ai-KPT01").unwrap_err();
    assert_locked(&e, "org:ai-KPT01", "organization", LOCKED_FIX);
    // the cited layer: the same rule through the no-set link is still locked by the set
    let e = fx.write_deviation("p2", "lib:ai-KPT01").unwrap_err();
    assert_locked(&e, "org:ai-KPT01", "organization", LOCKED_FIX);
    // the project's own lock is a set owner's lock change, which the hub reads
    let st = fx.r.read();
    let changes: Vec<LockChange> = st.lock_changes(&own_changes).unwrap();
    assert_eq!(changes.len(), 1, "{changes:#?}");
    assert_eq!(
        (
            changes[0].id.to_string(),
            changes[0].edit,
            changes[0].before
        ),
        (own.clone(), LockEdit::Locked, None)
    );
    drop(st);
    // the person layer: a meta project with its own rule
    let meta = tempfile::tempdir().unwrap();
    let mut mr = Rdy::init(&meta.path().join("meta"), "rdy-standard").unwrap();
    local_rule(&mut mr, "person-rules", "Claims never name owners.");
    let meta_path = mr.root().to_path_buf();
    mr.close().unwrap();
    let host = Host {
        path: PersonPath::Override(meta_path),
    };
    let reply: Reply = instructions::deliver_with(
        Some((&fx.r.read(), fx.r.root())),
        &LoadRequest::default(),
        &Caller {
            actor: "agent".into(),
            run: None,
        },
        Some(&host),
        &BOUNDS,
    )
    .unwrap_or_else(|e| panic!("deliver: {e:?}"));
    // the locked rule arrives once, at the organization layer, with the set's text and its lock
    let rows: Vec<_> = reply
        .instructions
        .iter()
        .filter(|r| r.id == "ai-KPT01")
        .collect();
    assert_eq!(rows.len(), 1, "{:#?}", reply.instructions);
    assert_eq!(rows[0].layer, Layer::Organization);
    assert!(rows[0].locked && rows[0].layer.locks_have_force());
    assert_eq!(rows[0].text, "Every claim names its owner.");
    // every later layer comes after it, and their locks have no force
    let at = |l: &Layer| {
        reply
            .instructions
            .iter()
            .position(|r| r.layer == *l)
            .unwrap_or_else(|| panic!("{l} in {:#?}", reply.instructions))
    };
    assert!(at(&Layer::Organization) < at(&Layer::Project));
    assert!(at(&Layer::Project) < at(&Layer::Person));
    for r in reply
        .instructions
        .iter()
        .filter(|r| matches!(r.layer, Layer::Cited(_) | Layer::Project | Layer::Person))
    {
        assert!(!r.layer.locks_have_force(), "{r:#?}");
    }
    let mine = reply.instructions.iter().find(|r| r.id == own).unwrap();
    assert!(mine.locked && mine.layer == Layer::Project, "{mine:#?}");
    fx.r.fsck().unwrap();
}

#[test]
fn rpl23_unadopted_locked_reports_a_first_delivery() {
    let mut fx = fixture(&[("org", repo(7), "pf-AGNT1")]);
    let lock = rule("ai-KPT01", "Every claim names its owner.", true);
    let open = rule("ai-FRE01", "Prefer short sections.", false);
    let first = fx.deliver("org", "v1", Some(ForeignSet::Organization), &[&lock, &open]);
    // the first delivery's lock counts against the empty baseline, with that delivery's time
    let rows = fx.r.read().unadopted_locked().unwrap();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    let row = &rows[0];
    assert_eq!(row.link, fx.link("org"));
    assert_eq!(row.alias, "org");
    assert_eq!(row.repo, repo(7));
    assert_eq!(row.id, id("ai-KPT01"));
    assert_eq!(row.change, LockEdit::Locked);
    assert_eq!(
        (row.adopted, row.delivered),
        (None, Some(lock.version_hash()))
    );
    assert_eq!(row.set, Some(ForeignSet::Organization));
    assert_eq!((row.entry, row.at_ms), (first.entry, AT_MS));
    // a keep leaves none of the rules in force: the lock stays unadopted
    let card = fx.card("org");
    answer(&mut fx.r, &card, "keep", Some("Not this quarter."))
        .unwrap_or_else(|e| panic!("keep: {e:?}"));
    assert_eq!(fx.r.read().unadopted_locked().unwrap(), rows);
    // a change of the lock is unadopted against the baseline too; adopting it clears the list
    let lock2 = rule("ai-KPT01", "Every claim names its owner and team.", true);
    let second = fx.deliver(
        "org",
        "v2",
        Some(ForeignSet::Organization),
        &[&lock2, &open],
    );
    let rows = fx.r.read().unadopted_locked().unwrap();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(
        (rows[0].delivered, rows[0].entry),
        (Some(lock2.version_hash()), second.entry)
    );
    fx.adopt("org");
    assert!(fx.r.read().unadopted_locked().unwrap().is_empty());
    // the set unlocks the rule: the cleared lock is unadopted until adopted
    let unlocked = rule("ai-KPT01", "Every claim names its owner and team.", false);
    let third = fx.deliver(
        "org",
        "v3",
        Some(ForeignSet::Organization),
        &[&unlocked, &open],
    );
    let rows = fx.r.read().unadopted_locked().unwrap();
    assert_eq!(rows.len(), 1, "{rows:#?}");
    assert_eq!(
        (
            rows[0].change,
            rows[0].adopted,
            rows[0].delivered,
            rows[0].entry
        ),
        (
            LockEdit::Unlocked,
            Some(lock2.version_hash()),
            Some(unlocked.version_hash()),
            third.entry
        )
    );
    fx.r.fsck().unwrap();
}

/// New defect (found by `rpl23_project_person_and_cited_layers_cannot_override_lock`): the adopt
/// card of one link read another link's instruction baseline pins (same repo, other profile) as
/// citations, so adopting it moved that link's baseline to its own delivered version and took
/// the set's lock away. A baseline moves only by its own link's adopt answer.
#[test]
fn rpl23_another_link_of_the_repo_never_moves_a_baseline() {
    let mut fx = fixture(&[("org", repo(7), "pf-AGNT1"), ("lib", repo(7), "pf-AGNT2")]);
    let lock = rule("ai-KPT01", "Every claim names its owner.", true);
    let loose = rule("ai-KPT01", "Claims may leave out their owner.", false);
    fx.deliver("org", "o1", Some(ForeignSet::Organization), &[&lock]);
    fx.adopt("org");
    let org = fx.link("org");
    let target = ForeignId::new(repo(7), id("ai-KPT01"));
    assert_eq!(fx.pins(&org)[&target], pin_of(&lock));
    fx.deliver("lib", "c1", None, &[&loose]);
    let card = fx.card("lib");
    let Some(PlanCardBinding::Adopt { pins, .. }) = &card.plan_binding else {
        panic!("{:#?}", card.plan_binding)
    };
    assert!(
        pins.iter().all(|p| !p.citers.contains(&org)),
        "the org baseline is no citation: {pins:#?}"
    );
    fx.adopt("lib");
    assert_eq!(
        fx.pins(&org)[&target],
        pin_of(&lock),
        "the org baseline stays"
    );
    assert_eq!(fx.pins(&fx.link("lib"))[&target], pin_of(&loose));
    fx.r.fsck().unwrap();
}
