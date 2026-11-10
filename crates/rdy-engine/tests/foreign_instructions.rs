//! rpl22 (repo links, D-DAPR8, D-MEMMQ, D-8X04F): the instruction layers a repo link adds.
//!
//! `rdy_instructions` delivers a link's adopted rules only: the instructions its `repo_link`
//! block pins, at the pinned versions. A link whose adopt answer served an organization or team
//! set delivers them as that layer; any other link delivers them as `cited:<alias>`, with no
//! lock force. Every test builds its own projects under a temporary directory, and the person
//! layer comes from a fake host over one more temporary project.
mod common;

use rdy_engine::foreign_delivery::{DeliveryTaken, take_delivery};
use rdy_engine::instructions::{
    self, BOUNDS, Caller, Contribution, Layer, LoadRequest, PersonHost, Reply,
};
use rdy_engine::instructions_config::{PersonPath, PersonSource};
use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignPublication, ForeignSet, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::*;
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

fn repo(n: u8) -> RepoId {
    RepoId([n; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn writer() -> Session {
    Session::human("human:founder".into(), Role::Writer).unwrap()
}

fn founder() -> Session {
    Session::human("human:founder".into(), Role::Approver).unwrap()
}

fn write(r: &mut Rdy, view: &str, ops: Value) -> WriteResult {
    r.write(
        &writer(),
        WriteRequest {
            view: view.into(),
            ops: parse_ops(ops.as_array().unwrap().clone()).unwrap(),
            justification: None,
            message: format!("write to {view}"),
        },
    )
    .unwrap_or_else(|e| panic!("write to {view}: {e:?}"))
}

fn land(r: &mut Rdy, view: &str) {
    let p = r
        .propose(&writer(), view, view, Some(common::TEST_SUMMARY))
        .unwrap();
    common::confirm_exceptions(r, &founder(), &p);
    r.land(&founder(), &p.id)
        .unwrap_or_else(|e| panic!("land {view}: {e:?}"));
}

/// A cited block, as the cited side delivers it.
fn blk(
    id_: &str,
    ty: &str,
    parent: Option<&str>,
    body: &str,
    fields: Vec<(&str, FieldValue)>,
) -> Block {
    Block {
        id: id(id_),
        block_type: ty.into(),
        parent: parent.map(id),
        order: "V".into(),
        fields: fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn s(v: &str) -> FieldValue {
    FieldValue::Str(v.into())
}

fn doc(id_: &str, title: &str, required: bool) -> Block {
    let mut f = vec![("title", s(title))];
    if required {
        f.push(("required", FieldValue::Bool(true)));
    }
    blk(id_, "doc", None, "", f)
}

fn why(id_: &str, statement: &str) -> Block {
    blk(id_, "decision", None, "", vec![("statement", s(statement))])
}

/// An instruction of doc `parent` with selectors `sels`, `locked`, and reason `reason`.
fn rule(id_: &str, parent: &str, body: &str, sels: &[&str], locked: bool, reason: &str) -> Block {
    let mut b = blk(
        id_,
        "instruction",
        Some(parent),
        body,
        vec![
            (
                "applies_to",
                FieldValue::List(sels.iter().map(|x| s(x)).collect()),
            ),
            ("locked", FieldValue::Bool(locked)),
        ],
    );
    b.links.insert(
        "decided_by".into(),
        vec![Link {
            target: id(reason),
            part: None,
        }],
    );
    b
}

fn live() -> ForeignDerived {
    ForeignDerived {
        status: None,
        lifecycle: None,
        superseded_by: vec![],
    }
}

/// A link delivery stream of repo `from` holding `selected`, written to `dir/name`.
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
            derived: live(),
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

/// A project that links the repos of `links` (alias, repo, profile) under one doc.
struct Fx {
    _d: tempfile::TempDir,
    files: tempfile::TempDir,
    r: Rdy,
    ids: BTreeMap<String, BlockId>,
    profiles: BTreeMap<String, String>,
    section: String,
}

fn fixture(links: &[(&str, RepoId, &str)]) -> Fx {
    let d = tempfile::tempdir().unwrap();
    let mut r = Rdy::init(&d.path().join("p"), "rdy-standard").unwrap();
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
    let w = write(&mut r, "seed", Value::Array(ops));
    land(&mut r, "seed");
    let ids = links
        .iter()
        .map(|(a, _, _)| (a.to_string(), id(&w.created[&format!("${a}")])))
        .collect();
    let profiles = links
        .iter()
        .map(|(a, _, p)| (a.to_string(), p.to_string()))
        .collect();
    Fx {
        _d: d,
        profiles,
        files: tempfile::tempdir().unwrap(),
        r,
        ids,
        section: w.created["$s"].clone(),
    }
}

impl Fx {
    /// Takes the delivery `selected` of repo `from` into link `alias`, of set `set`.
    fn deliver(
        &mut self,
        alias: &str,
        name: &str,
        from: RepoId,
        set: Option<ForeignSet>,
        selected: &[&Block],
    ) -> DeliveryTaken {
        let source = ForeignSource::Link {
            link: self.ids[alias].to_string(),
            site: "hub.example".into(),
            at_ms: 1_760_000_000_000,
        };
        let file = stream(
            self.files.path(),
            name,
            from,
            &self.profiles[alias],
            selected,
        );
        take_delivery(&mut self.r, &file, &source, set)
            .unwrap_or_else(|e| panic!("take {name}: {e:?}"))
    }

    /// The link `alias` pins the instructions `cited`, as an adoption leaves it.
    fn pin(&mut self, alias: &str, cited: &[&str]) {
        let text: Vec<String> = cited
            .iter()
            .map(|c| format!("{{{{{alias}:{c}}}}}"))
            .collect();
        let view = format!("pin-{alias}-{}", cited.join("-").to_lowercase());
        write(
            &mut self.r,
            &view,
            json!([{"op": "update", "id": self.ids[alias].to_string(),
                    "fields": {"title": format!("In force: {}", text.join(" "))}}]),
        );
        land(&mut self.r, &view);
    }

    /// The person's adopt answer to delivery `entry` of repo `from`, written straight to the store.
    fn adopt(&mut self, from: RepoId, entry: &Hash) {
        let a = Answer {
            item: format!("adopt:{from}@{}", entry.short()),
            act: AnswerAct::Adopt,
            option: None,
            label: None,
            options: vec![],
            card_hash: Some(Hash::of(b"card")),
            targets: vec![],
            person: writer().provenance(),
            relayed_by: None,
            relay_evidence: None,
            evidence_kind: Some(EvidenceKind::Elicited),
            note: None,
        };
        self.r.store_mut().put_answer(&a).expect("put_answer");
        self.r.invalidate();
        self.r.seal_open().unwrap();
    }

    fn reply(&self, load: &[&str]) -> Reply {
        let load =
            LoadRequest::parse(&load.iter().map(|x| x.to_string()).collect::<Vec<_>>()).unwrap();
        let host = Host::none(self._d.path());
        instructions::deliver_with(
            Some((&self.r.read(), self.r.root())),
            &load,
            &caller(),
            Some(&host),
            &BOUNDS,
        )
        .unwrap_or_else(|e| panic!("deliver: {e:?}"))
    }
}

fn caller() -> Caller {
    Caller {
        actor: "agent".into(),
        run: None,
    }
}

/// A fake person host: the meta project at `meta`, read through `Rdy::open`, or none at all.
struct Host {
    path: PersonPath,
}

impl Host {
    fn none(under: &Path) -> Host {
        Host {
            path: PersonPath::Override(under.join("no-meta-project")),
        }
    }

    fn at(meta: &Path) -> Host {
        Host {
            path: PersonPath::Override(meta.to_path_buf()),
        }
    }
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

/// Writes a landed ruleset `name` with one instruction `body` (selector `all`) into `r`; its
/// reason is a decision that the same write creates. Returns the instruction's id.
fn local_rule(r: &mut Rdy, name: &str, body: &str) -> String {
    let w = write(
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
    let w = write(
        r,
        name,
        json!([{"op": "create", "type": "instruction", "id": "$i", "parent": w.created["$d"],
                "fields": {"applies_to": ["all"]}, "body": body,
                "links": {"decided_by": [{"target": d}]}}]),
    );
    land(r, name);
    w.created["$i"].clone()
}

fn layers(reply: &Reply) -> Vec<String> {
    reply
        .instructions
        .iter()
        .map(|i| i.layer.to_string())
        .collect()
}

fn ids(reply: &Reply) -> Vec<&str> {
    reply.instructions.iter().map(|i| i.id.as_str()).collect()
}

/// The cited content of one repo: a doc, a reason and the rules, all selector `all`.
fn content(prefix: &str, rules: &[(&str, &str, bool)], required: bool) -> Vec<Block> {
    let d = format!("d-{prefix}DC");
    let why_id = format!("D-{prefix}WH");
    let mut out = vec![
        doc(&d, &format!("{prefix} rules"), required),
        why(&why_id, "Because."),
    ];
    for (id_, body, locked) in rules {
        out.push(rule(id_, &d, body, &["all"], *locked, &why_id));
    }
    out
}

fn refs(v: &[Block]) -> Vec<&Block> {
    v.iter().collect()
}

#[test]
fn rpl22_layers_are_rdy_org_team_cited_project_person() {
    let org = repo(7);
    let team = repo(8);
    let lib = repo(9);
    let mut fx = fixture(&[
        ("org", org, "pf-AGNT1"),
        ("ops", team, "pf-AGNT1"),
        ("lib", lib, "pf-AGNT1"),
    ]);
    let o = content("GRP", &[("ai-GRP01", "Organization rule.", true)], false);
    let t = content("TEA", &[("ai-TEA01", "Team rule.", true)], false);
    let c = content("KBX", &[("ai-KBX01", "Cited rule.", true)], false);
    let eo = fx.deliver("org", "o1", org, Some(ForeignSet::Organization), &refs(&o));
    let et = fx.deliver(
        "ops",
        "t1",
        team,
        Some(ForeignSet::Team("platform".into())),
        &refs(&t),
    );
    fx.deliver("lib", "c1", lib, None, &refs(&c));
    fx.pin("org", &["ai-GRP01"]);
    fx.pin("ops", &["ai-TEA01"]);
    fx.pin("lib", &["ai-KBX01"]);
    fx.adopt(org, &eo.entry);
    fx.adopt(team, &et.entry);
    local_rule(&mut fx.r, "project-rules", "Project rule.");
    let meta = tempfile::tempdir().unwrap();
    let mut mr = Rdy::init(&meta.path().join("meta"), "rdy-standard").unwrap();
    local_rule(&mut mr, "person-rules", "Person rule.");
    let meta_path = mr.root().to_path_buf();
    mr.close().unwrap();
    let host = Host::at(&meta_path);
    let reply = instructions::deliver_with(
        Some((&fx.r.read(), fx.r.root())),
        &LoadRequest::default(),
        &caller(),
        Some(&host),
        &BOUNDS,
    )
    .unwrap();
    let seen = layers(&reply);
    let first = |l: &str| {
        seen.iter()
            .position(|x| x == l)
            .unwrap_or_else(|| panic!("{l} in {seen:?}"))
    };
    let order = [
        first("rdy"),
        first("organization"),
        first("team:platform"),
        first("cited:lib"),
        first("project"),
        first("person"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{seen:?}");
    // the locks of the cited project show as stored; the layer says which of them has force
    let row = |i: &str| {
        reply
            .instructions
            .iter()
            .find(|r| r.id == i)
            .unwrap()
            .clone()
    };
    assert!(row("ai-GRP01").locked && row("ai-GRP01").layer.locks_have_force());
    assert!(row("ai-TEA01").locked && row("ai-TEA01").layer.locks_have_force());
    assert!(row("ai-KBX01").locked && !row("ai-KBX01").layer.locks_have_force());
    assert_eq!(row("ai-KBX01").layer, Layer::Cited("lib".into()));
    assert_eq!(row("ai-GRP01").text, "Organization rule.");
    assert_eq!(row("ai-GRP01").reason[0].id, "D-GRPWH");
    assert_eq!(row("ai-GRP01").reason[0].statement, "Because.");
}

#[test]
fn rpl22_team_names_sort_the_display_and_share_one_rank() {
    let a = repo(7);
    let b = repo(8);
    let mut fx = fixture(&[("zed", a, "pf-AGNT1"), ("amy", b, "pf-AGNT1")]);
    let za = content("ZED", &[("ai-ZED01", "Zed rule.", false)], false);
    let ab = content("AMY", &[("ai-AMY01", "Amy rule.", false)], false);
    let e1 = fx.deliver(
        "zed",
        "z1",
        a,
        Some(ForeignSet::Team("alpha".into())),
        &refs(&za),
    );
    let e2 = fx.deliver(
        "amy",
        "a1",
        b,
        Some(ForeignSet::Team("beta".into())),
        &refs(&ab),
    );
    fx.pin("zed", &["ai-ZED01"]);
    fx.pin("amy", &["ai-AMY01"]);
    fx.adopt(a, &e1.entry);
    fx.adopt(b, &e2.entry);
    let reply = fx.reply(&[]);
    let foreign: Vec<&str> = reply
        .instructions
        .iter()
        .filter(|i| i.layer != Layer::Rdy)
        .map(|i| i.id.as_str())
        .collect();
    assert_eq!(foreign, ["ai-ZED01", "ai-AMY01"], "alpha sorts before beta");
    assert_eq!(
        Layer::Team("alpha".into()).precedence(),
        Layer::Team("beta".into()).precedence()
    );
}

#[test]
fn rpl22_required_rulesets_arrive_and_stay_in_menu() {
    let org = repo(7);
    let mut fx = fixture(&[("org", org, "pf-AGNT1")]);
    let mut c = content(
        "REQ",
        &[
            ("ai-REQ01", "First required rule.", false),
            ("ai-REQ02", "Second required rule.", false),
        ],
        true,
    );
    // an optional ruleset: a hint selector never delivers it, an `all` selector always does
    c.push(doc("d-APTDC", "Optional rules", false));
    c.push(rule(
        "ai-APT01",
        "d-APTDC",
        "Only when loaded.",
        &["area:billing"],
        false,
        "D-REQWH",
    ));
    c.push(rule(
        "ai-APT02",
        "d-APTDC",
        "Always shown.",
        &["all"],
        false,
        "D-REQWH",
    ));
    let e = fx.deliver("org", "o1", org, Some(ForeignSet::Organization), &refs(&c));
    fx.pin("org", &["ai-REQ01", "ai-REQ02", "ai-APT01", "ai-APT02"]);
    fx.adopt(org, &e.entry);
    let reply = fx.reply(&[]);
    assert_eq!(
        ids(&reply)
            .into_iter()
            .filter(|i| i.starts_with("ai-R") || i.starts_with("ai-A"))
            .collect::<Vec<_>>(),
        ["ai-APT02", "ai-REQ01", "ai-REQ02"],
        "the required ruleset in full, the `all` rule, no hint-selected rule"
    );
    let menu: BTreeMap<&str, bool> = reply
        .menu
        .iter()
        .filter(|m| m.layer == Layer::Organization)
        .map(|m| (m.id.as_str(), m.required))
        .collect();
    assert_eq!(
        menu,
        BTreeMap::from([
            ("organization:org:d-APTDC", false),
            ("organization:org:d-REQDC", true)
        ]),
        "every ruleset is on the menu, the required one marked"
    );
    let req = reply
        .menu
        .iter()
        .find(|m| m.id == "organization:org:d-REQDC")
        .unwrap();
    assert_eq!(req.name, "REQ rules");
    assert!(req.tokens > 0);
    let opt = reply
        .menu
        .iter()
        .find(|m| m.id == "organization:org:d-APTDC")
        .unwrap();
    assert_eq!(opt.hints, ["area:billing", "all"]);
    // loading the optional ruleset brings its other rule
    let loaded = fx.reply(&["organization:org:d-APTDC"]);
    assert!(ids(&loaded).contains(&"ai-APT01"), "{:?}", ids(&loaded));
}

#[test]
fn rpl22_handles_distinguish_repo_profile_and_layer() {
    let org = repo(7);
    let other = repo(8);
    // two links of one repo with two profiles, and a team link of another repo that holds a
    // doc of the same id
    let mut fx = fixture(&[
        ("org", org, "pf-AGNT1"),
        ("org2", org, "pf-AGNT2"),
        ("ops", other, "pf-AGNT1"),
    ]);
    let mut c = content("SAM", &[], false);
    c.push(rule(
        "ai-SAM01",
        "d-SAMDC",
        "Shared rule.",
        &["area:x"],
        false,
        "D-SAMWH",
    ));
    let e1 = fx.deliver("org", "o1", org, Some(ForeignSet::Organization), &refs(&c));
    let e2 = fx.deliver("org2", "o2", org, Some(ForeignSet::Organization), &refs(&c));
    let e3 = fx.deliver(
        "ops",
        "t1",
        other,
        Some(ForeignSet::Team("platform".into())),
        &refs(&c),
    );
    fx.pin("org", &["ai-SAM01"]);
    fx.pin("org2", &["ai-SAM01"]);
    fx.pin("ops", &["ai-SAM01"]);
    fx.adopt(org, &e1.entry);
    fx.adopt(org, &e2.entry);
    fx.adopt(other, &e3.entry);
    let reply = fx.reply(&[]);
    let menu: Vec<&str> = reply
        .menu
        .iter()
        .filter(|m| m.layer != Layer::Rdy)
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(
        menu,
        [
            "organization:org:d-SAMDC",
            "organization:org2:d-SAMDC",
            "team:platform:ops:d-SAMDC"
        ],
        "one handle per link and layer"
    );
    // a selector is a hint: nothing is delivered until a ruleset is loaded
    assert!(
        reply.instructions.iter().all(|i| i.layer == Layer::Rdy),
        "{:?}",
        ids(&reply)
    );
    // a handle names one link: loading org2 delivers the rule of repo 7 once, under that link
    let one = fx.reply(&["organization:org2:d-SAMDC"]);
    let sam: Vec<_> = one
        .instructions
        .iter()
        .filter(|i| i.id == "ai-SAM01")
        .collect();
    assert_eq!(sam.len(), 1, "{:?}", ids(&one));
    assert_eq!(sam[0].layer, Layer::Organization);
    assert_eq!(sam[0].ruleset, "organization:org2:d-SAMDC");
    // loading both links of one repo delivers the one rule once, from the first link
    let both = fx.reply(&["organization:org2:d-SAMDC", "organization:org:d-SAMDC"]);
    let sam: Vec<_> = both
        .instructions
        .iter()
        .filter(|i| i.id == "ai-SAM01")
        .collect();
    assert_eq!(sam.len(), 1, "{:?}", ids(&both));
    assert_eq!(sam[0].ruleset, "organization:org:d-SAMDC");
    // the team link is another repo: its rule has the same id and is another rule
    let team = fx.reply(&["team:platform:ops:d-SAMDC"]);
    let sam: Vec<_> = team
        .instructions
        .iter()
        .filter(|i| i.id == "ai-SAM01")
        .collect();
    assert_eq!(sam.len(), 1, "{:?}", ids(&team));
    assert_eq!(sam[0].layer, Layer::Team("platform".into()));
    // a bare doc id that two layers hold is ambiguous, a qualified one is not
    let bare = LoadRequest::parse(&["d-SAMDC".to_string()]).unwrap();
    let err = instructions::deliver_with(
        Some((&fx.r.read(), fx.r.root())),
        &bare,
        &caller(),
        Some(&Host::none(fx._d.path())),
        &BOUNDS,
    )
    .unwrap_err();
    assert_eq!(err.code, "E_BAD_INPUT", "{err:?}");
    assert!(
        err.message.contains("organization") && err.message.contains("team:platform"),
        "{err:?}"
    );
    // an unknown link in a handle is not found
    let missing = LoadRequest::parse(&["organization:nope:d-SAMDC".to_string()]).unwrap();
    let err = instructions::deliver_with(
        Some((&fx.r.read(), fx.r.root())),
        &missing,
        &caller(),
        Some(&Host::none(fx._d.path())),
        &BOUNDS,
    )
    .unwrap_err();
    assert_eq!(err.code, "E_NOT_FOUND", "{err:?}");
}

#[test]
fn rpl22_meta_project_is_not_loaded_twice() {
    let lib = repo(9);
    let mut fx = fixture(&[("lib", lib, "pf-AGNT1")]);
    let c = content("KBX", &[("ai-KBX01", "Cited rule.", false)], false);
    fx.deliver("lib", "c1", lib, None, &refs(&c));
    fx.pin("lib", &["ai-KBX01"]);
    let mine = local_rule(&mut fx.r, "my-rules", "My rule.");
    // the project is the person's meta project: it is the person layer, once, and its cited
    // layer is read from the same state, once
    let host = Host::at(fx.r.root());
    let reply = instructions::deliver_with(
        Some((&fx.r.read(), fx.r.root())),
        &LoadRequest::default(),
        &caller(),
        Some(&host),
        &BOUNDS,
    )
    .unwrap();
    assert_eq!(
        reply.instructions.iter().filter(|i| i.id == mine).count(),
        1,
        "{:?}",
        ids(&reply)
    );
    assert!(reply.instructions.iter().all(|i| i.layer != Layer::Project));
    assert_eq!(
        reply
            .instructions
            .iter()
            .filter(|i| i.id == "ai-KBX01")
            .count(),
        1
    );
    assert_eq!(
        reply
            .menu
            .iter()
            .filter(|m| m.id == "cited:lib:d-KBXDC")
            .count(),
        1
    );
}

#[test]
fn rpl22_unadopted_rules_are_pending_not_delivered() {
    let lib = repo(9);
    let mut fx = fixture(&[("lib", lib, "pf-AGNT1")]);
    let v1 = content(
        "KBX",
        &[
            ("ai-KBX01", "Old text.", false),
            ("ai-KBX02", "Never adopted.", false),
        ],
        false,
    );
    fx.deliver("lib", "c1", lib, None, &refs(&v1));
    // a first delivery that nothing pinned: none of its rules is in force
    let before = fx.reply(&[]);
    assert!(
        before.instructions.iter().all(|i| i.layer == Layer::Rdy),
        "{:?}",
        layers(&before)
    );
    assert!(before.menu.iter().all(|m| m.layer == Layer::Rdy));
    // the person adopts one rule at its delivered version
    fx.pin("lib", &["ai-KBX01"]);
    // the cited project changes both rules and locks the first
    let v2 = content(
        "KBX",
        &[
            ("ai-KBX01", "New text.", true),
            ("ai-KBX02", "Still not adopted.", false),
        ],
        false,
    );
    fx.deliver("lib", "c2", lib, None, &refs(&v2));
    let reply = fx.reply(&[]);
    let cited: Vec<_> = reply
        .instructions
        .iter()
        .filter(|i| i.layer == Layer::Cited("lib".into()))
        .collect();
    assert_eq!(cited.len(), 1, "{:?}", ids(&reply));
    assert_eq!(cited[0].id, "ai-KBX01");
    assert_eq!(
        cited[0].text, "Old text.",
        "the pinned version, not the delivered one"
    );
    assert!(!cited[0].locked, "a lock counts only at an adopted version");
    let entry = reply
        .menu
        .iter()
        .find(|m| m.layer == Layer::Cited("lib".into()))
        .unwrap();
    assert_eq!(
        entry.tokens,
        instructions::estimate_tokens("Old text.".len() as u64),
        "the menu counts adopted bodies only"
    );
    let all = serde_json::to_string(&reply).unwrap();
    assert!(
        !all.contains("Never adopted") && !all.contains("Still not adopted"),
        "{all}"
    );
}

#[test]
fn rpl22_a_revoked_link_delivers_nothing() {
    let lib = repo(9);
    let mut fx = fixture(&[("lib", lib, "pf-AGNT1")]);
    let c = content("KBX", &[("ai-KBX01", "Cited rule.", false)], false);
    fx.deliver("lib", "c1", lib, None, &refs(&c));
    fx.pin("lib", &["ai-KBX01"]);
    assert!(ids(&fx.reply(&[])).contains(&"ai-KBX01"));
    let source = ForeignSource::Link {
        link: fx.ids["lib"].to_string(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    };
    rdy_engine::foreign_delivery::revoke_link(&mut fx.r, &source).unwrap();
    assert!(!ids(&fx.reply(&[])).contains(&"ai-KBX01"));
    let _ = &fx.section;
}
