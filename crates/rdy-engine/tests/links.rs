//! rpl28 (repo links, D-DAPR8, D-MEMMQ): the linked reads. `links`, `link_show` and `link_refs`
//! read the project's repo links: per link its state (`none`, `delivered`, `revoked`), the last
//! delivery's tag, profile version and time, and the pins that are behind; `show` lists what one
//! alias delivers (its profile's selection alone) and the instructions it has not adopted; `refs`
//! lists the pins to cited content. `rdy_read` takes `<alias>:<id>` and returns the cited block
//! read-only, with its provenance and the versions the project's pins hold.
mod common;

use rdy_engine::tool_reads::{read_entries, read_page};
use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::*;
use rdy_store::{codec, kinds};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

const AGENTS: &str = "pf-AGNT1";
const PEOPLE: &str = "pf-HMN22";
const AT_MS: i64 = 1_760_000_000_000;

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

fn repo(n: u8) -> RepoId {
    RepoId([n; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn block(id_: &str, ty: &str, fields: &[(&str, &str)], body: &str) -> Block {
    Block {
        id: id(id_),
        block_type: ty.into(),
        parent: None,
        order: "V".into(),
        fields: fields
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str((*v).into())))
            .collect(),
        body: body.into(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn decision(id_: &str, statement: &str, key: Option<&str>) -> Block {
    let mut f = vec![("statement", statement), ("status", "accepted")];
    if let Some(k) = key {
        f.push(("key", k));
    }
    block(id_, "decision", &f, "")
}

fn version_of(b: &Block) -> Hash {
    Hash::of(&codec::item_new(kinds::BLOCK, b).1)
}

fn content_of(b: &Block) -> Hash {
    Hash::of(format!("content of {} {} {:?}", b.id, b.body, b.fields).as_bytes())
}

fn fv(b: &Block) -> ForeignVersion {
    ForeignVersion {
        id: b.id.clone(),
        version: version_of(b),
        content: content_of(b),
        encoding: 0,
        derived: ForeignDerived::default(),
    }
}

/// One source's chain, as the citing side builds it.
struct Source {
    repo: RepoId,
    link: String,
    profile: &'static str,
    tag: &'static str,
    head: Option<Hash>,
    sel: BTreeMap<BlockId, Hash>,
}

impl Source {
    fn new(repo: RepoId, link: &str, profile: &'static str) -> Source {
        Source {
            repo,
            link: link.into(),
            profile,
            tag: "v1",
            head: None,
            sel: BTreeMap::new(),
        }
    }

    fn source(&self) -> ForeignSource {
        ForeignSource::Link {
            link: self.link.clone(),
            site: "hub.example".into(),
            at_ms: AT_MS,
        }
    }

    /// Writes a delivery straight to the store and publishes the state after it.
    fn deliver(&mut self, r: &mut Rdy, added: &[&Block], changed: &[&Block], removed: &[&str]) {
        for b in added.iter().chain(changed) {
            self.sel.insert(b.id.clone(), version_of(b));
        }
        for x in removed {
            self.sel.remove(&id(x));
        }
        let entry = ForeignEntry {
            repo: self.repo,
            source: self.source(),
            event: ForeignEvent::Delivery {
                prev: self.head,
                publication: ForeignPublication {
                    tag: self.tag.into(),
                    profile: self.profile.into(),
                    profile_version: None,
                },
                set: None,
                added: added.iter().map(|b| fv(b)).collect(),
                changed: changed.iter().map(|b| fv(b)).collect(),
                removed: removed.iter().map(|x| id(x)).collect(),
                shells: vec![],
                selection: selection_identity(self.sel.iter()).unwrap(),
            },
        };
        let blocks: Vec<Block> = added.iter().chain(changed).map(|b| (*b).clone()).collect();
        self.head = Some(r.store_mut().put_foreign(&entry, &blocks).unwrap());
        r.publish_after_foreign_for_tests().unwrap();
    }

    /// Revokes the source.
    fn revoke(&mut self, r: &mut Rdy) {
        let entry = ForeignEntry {
            repo: self.repo,
            source: self.source(),
            event: ForeignEvent::Revoked {
                prev: self.head.unwrap(),
            },
        };
        self.head = Some(r.store_mut().put_foreign(&entry, &[]).unwrap());
        r.publish_after_foreign_for_tests().unwrap();
    }
}

struct World {
    r: Rdy,
    sec: String,
    link: BTreeMap<String, String>,
    agents: Source,
    x: Block,
    y: Block,
    z: Block,
}

/// A landed project with links `core` and `core-people` to one repo under two profiles, `later`
/// (no delivery yet) and `old` (delivered, then revoked); and a delivery of a repo that no live
/// link names.
fn world(base: &Path) -> World {
    let mut r = Rdy::init(&base.join("p"), "rdy-standard").unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"title": "Guide", "doc_type": "page"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "repo_link", "id": "$core", "parent": "$d",
             "fields": {"alias": "core", "repo": repo(7).to_string(), "profile": AGENTS}},
            {"op": "create", "type": "repo_link", "id": "$people", "parent": "$d",
             "fields": {"alias": "core-people", "repo": repo(7).to_string(), "profile": PEOPLE}},
            {"op": "create", "type": "repo_link", "id": "$later", "parent": "$d",
             "fields": {"alias": "later", "repo": repo(5).to_string(), "profile": AGENTS, "title": "Not yet"}},
            {"op": "create", "type": "repo_link", "id": "$old", "parent": "$d",
             "fields": {"alias": "old", "repo": repo(4).to_string(), "profile": AGENTS}}
        ]),
    );
    land(&mut r, "seed");
    let x = decision("d-FRGNX", "Use X.", None);
    let y = decision("d-FRGNY", "Use Y.", None);
    let z = decision("d-FRGNZ", "Use Z.", Some("CORE-RULE"));
    let mut agents = Source::new(repo(7), &w.created["$core"], AGENTS);
    agents.deliver(&mut r, &[&x, &z], &[], &[]);
    let mut people = Source::new(repo(7), &w.created["$people"], PEOPLE);
    people.deliver(&mut r, &[&y], &[], &[]);
    let mut old = Source::new(repo(4), &w.created["$old"], AGENTS);
    old.deliver(&mut r, &[&decision("d-ANCNT", "Old rule.", None)], &[], &[]);
    old.revoke(&mut r);
    // a delivery whose link block no longer exists: its repo is unlinked
    let mut gone = Source::new(repo(9), "rl-GONE1", AGENTS);
    gone.tag = "v9";
    gone.deliver(&mut r, &[&decision("d-GNNE1", "Gone.", None)], &[], &[]);
    World {
        r,
        sec: w.created["$s"].clone(),
        link: w.created.clone(),
        agents,
        x,
        y,
        z,
    }
}

fn expect_err(e: RdyError, code: &str, message: &str, fix: &str) {
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (code, message, fix),
        "{e:?}"
    );
}

fn cite(r: &mut Rdy, sec: &str, body: &str) {
    write(
        r,
        "cite",
        json!([{"op": "create", "type": "claim", "parent": sec, "body": body}]),
    );
    land(r, "cite");
}

fn ids_of(v: &Value) -> Vec<String> {
    v["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn rpl28_links_list_show_refs_are_profile_aware() {
    let d = tempfile::tempdir().unwrap();
    let World {
        mut r,
        sec,
        link,
        mut agents,
        x,
        z,
        ..
    } = world(d.path());
    let at = rfc3339(AT_MS);

    // list: one row per live repo_link in alias order, then the delivered repos no link names
    let list = serde_json::to_value(r.read().links().unwrap()).unwrap();
    let rows = list["links"].as_array().unwrap();
    let aliases: Vec<&str> = rows.iter().map(|l| l["alias"].as_str().unwrap()).collect();
    assert_eq!(aliases, ["core", "core-people", "later", "old"], "{list}");
    assert_eq!(
        rows[0],
        json!({"alias": "core", "link": link["$core"], "repo": repo(7).to_string(),
               "profile": AGENTS, "state": "delivered", "tag": "v1", "delivered_at": at,
               "pins_behind": 0})
    );
    assert_eq!(rows[1]["profile"], PEOPLE);
    assert_eq!(rows[1]["state"], "delivered");
    assert_eq!(
        rows[2],
        json!({"alias": "later", "link": link["$later"], "repo": repo(5).to_string(),
               "profile": AGENTS, "title": "Not yet", "state": "none", "pins_behind": 0})
    );
    assert_eq!(rows[3]["state"], "revoked", "{list}");
    assert_eq!(rows[3]["tag"], "v1");
    assert_eq!(
        list["unlinked"],
        json!([{"repo": repo(9).to_string(), "state": "delivered", "tag": "v9",
                "delivered_at": at, "pins_behind": 0}])
    );

    // show: what one alias delivers, its profile's selection alone
    let st = r.read();
    let core = serde_json::to_value(st.link_show("core").unwrap()).unwrap();
    assert_eq!(core["link"]["alias"], "core");
    assert_eq!(ids_of(&core), ["d-FRGNX", "d-FRGNZ"], "{core}");
    assert_eq!(core["blocks"][0]["block_type"], "decision");
    assert_eq!(core["blocks"][0]["summary"], "Use X.");
    assert_eq!(core["blocks"][0]["version"], version_of(&x).to_string());
    let people = serde_json::to_value(st.link_show("core-people").unwrap()).unwrap();
    assert_eq!(ids_of(&people), ["d-FRGNY"], "{people}");
    // a revoked link shows its frozen last view; a link with no delivery shows none
    let old = serde_json::to_value(st.link_show("old").unwrap()).unwrap();
    assert_eq!(ids_of(&old), ["d-ANCNT"]);
    assert!(
        serde_json::to_value(st.link_show("later").unwrap()).unwrap()["blocks"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    expect_err(
        st.link_show("nope").unwrap_err(),
        "E_NOT_FOUND",
        "no repo link has the alias nope",
        "list the links: rdy_links / rdy links",
    );
    expect_err(
        st.link_refs(Some("nope")).unwrap_err(),
        "E_NOT_FOUND",
        "no repo link has the alias nope",
        "list the links: rdy_links / rdy links",
    );
    drop(st);

    // refs: the pins of the project's blocks to cited content, by repo; none before a citation
    assert!(
        serde_json::to_value(r.read().link_refs(None).unwrap()).unwrap()["repos"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    cite(
        &mut r,
        &sec,
        "Follow {{core:d-FRGNX}} and {{core:CORE-RULE}}.",
    );
    let refs = serde_json::to_value(r.read().link_refs(None).unwrap()).unwrap();
    assert_eq!(refs["repos"].as_array().unwrap().len(), 1, "{refs}");
    let repo_row = &refs["repos"][0];
    assert_eq!(repo_row["alias"], "core");
    assert_eq!(repo_row["repo"], repo(7).to_string());
    let pins: Vec<(&str, bool)> = repo_row["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["target"].as_str().unwrap(),
                p["behind"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(pins, [("d-FRGNX", false), ("d-FRGNZ", false)]);
    assert_eq!(repo_row["pins"][0]["held"], version_of(&x).to_string());
    assert_eq!(repo_row["pins"][0]["delivered"], version_of(&x).to_string());
    assert_eq!(repo_row["pins"][0]["cited_by"].as_array().unwrap().len(), 1);
    let one = serde_json::to_value(r.read().link_refs(Some("core-people")).unwrap()).unwrap();
    assert_eq!(
        one["repos"].as_array().unwrap().len(),
        1,
        "the alias names the repo it reads: {one}"
    );
    assert_eq!(one["repos"][0]["alias"], "core-people");

    // a delivery that changes X and removes Z leaves two pins behind; none moves
    let x2 = decision("d-FRGNX", "Use X, now.", None);
    agents.tag = "v2";
    agents.deliver(&mut r, &[], &[&x2], &["d-FRGNZ"]);
    let list = serde_json::to_value(r.read().links().unwrap()).unwrap();
    assert_eq!(list["links"][0]["tag"], "v2", "{list}");
    assert_eq!(list["links"][0]["pins_behind"], 2, "{list}");
    assert_eq!(
        list["links"][1]["pins_behind"], 2,
        "pins are the repo's: {list}"
    );
    let refs = serde_json::to_value(r.read().link_refs(Some("core")).unwrap()).unwrap();
    let rows = refs["repos"][0]["pins"].as_array().unwrap();
    assert_eq!(rows[0]["target"], "d-FRGNX");
    assert_eq!(rows[0]["behind"], true);
    assert_eq!(rows[0]["held"], version_of(&x).to_string());
    assert_eq!(rows[0]["delivered"], version_of(&x2).to_string());
    assert_eq!(rows[1]["target"], "d-FRGNZ");
    assert_eq!(rows[1]["behind"], true);
    assert_eq!(rows[1]["held"], version_of(&z).to_string());
    assert!(rows[1].get("delivered").is_none(), "removed: {}", rows[1]);
    let core = serde_json::to_value(r.read().link_show("core").unwrap()).unwrap();
    assert_eq!(ids_of(&core), ["d-FRGNX"]);
    assert_eq!(core["blocks"][0]["pin"], "behind");
    assert_eq!(core["blocks"][0]["summary"], "Use X, now.");
}

#[test]
fn rpl28_foreign_read_has_provenance_and_held_versions() {
    let d = tempfile::tempdir().unwrap();
    let World {
        mut r,
        sec,
        link,
        mut agents,
        x,
        y,
        ..
    } = world(d.path());
    cite(&mut r, &sec, "Follow {{core:d-FRGNX}}.");
    let x2 = decision("d-FRGNX", "Use X, now.", None);
    agents.tag = "v2";
    agents.deliver(&mut r, &[], &[&x2], &[]);

    let read = |r: &Rdy, target: &str, format: &str| -> Result<Value, RdyError> {
        let st = r.read();
        let e = read_entries(&st, target, "canon", &None, format, None)?;
        Ok(read_page(&e, "t", 0, None))
    };
    let short = |h: &Hash| h.to_string()[..12].to_string();

    // the current delivered version, its provenance, and the version the pin holds
    let v = read(&r, "core:d-FRGNX", "compact").unwrap();
    let text = v["text"].as_str().unwrap();
    assert_eq!(v["id"], "d-FRGNX");
    assert!(
        text.starts_with("core:d-FRGNX decision: Use X, now."),
        "{text}"
    );
    assert!(
        text.contains(&format!("delivered by the hub (link {})", link["$core"])),
        "{text}"
    );
    assert!(
        text.contains(&format!("delivered: {} in v2", short(&version_of(&x2)))),
        "{text}"
    );
    assert!(
        text.contains(&format!("pinned: {}", short(&version_of(&x)))),
        "{text}"
    );
    // a key and the stored form read the same block
    assert_eq!(
        read(&r, "core:d-FRGNX", "compact").unwrap(),
        read(&r, &format!("{}:d-FRGNX", repo(7)), "compact").unwrap()
    );
    let by_key = read(&r, "core:CORE-RULE", "compact").unwrap();
    assert_eq!(by_key["id"], "d-FRGNZ");
    // the other formats carry the same facts
    let yaml = read(&r, "core:d-FRGNX", "yaml").unwrap();
    assert!(yaml["text"].as_str().unwrap().contains("Use X, now."));
    assert!(
        yaml["text"]
            .as_str()
            .unwrap()
            .contains("delivered by the hub")
    );
    let md = read(&r, "core:d-FRGNX", "markdown").unwrap();
    assert!(md["text"].as_str().unwrap().contains("Use X, now."));
    assert!(
        md["text"]
            .as_str()
            .unwrap()
            .contains(&short(&version_of(&x)))
    );

    // the profile's selection decides: Y is the people profile's, not core's
    expect_err(
        read(&r, "core:d-FRGNY", "compact").unwrap_err(),
        "E_UNKNOWN_REF",
        "core:d-FRGNY is not in the content delivered from core",
        "cite a block that core delivers: rdy_links {op: \"show\", alias} / rdy links show core",
    );
    let y_text = read(&r, "core-people:d-FRGNY", "compact").unwrap();
    let y_text = y_text["text"].as_str().unwrap();
    assert!(y_text.contains(&format!("delivered: {} in v1", short(&version_of(&y)))));
    assert!(!y_text.contains("pinned:"), "{y_text}");
    expect_err(
        read(&r, "nope:d-FRGNX", "compact").unwrap_err(),
        "E_UNKNOWN_REF",
        "no repo link has the alias nope",
        "use the alias of a linked project: rdy_links / rdy links",
    );

    // a block the selection left stays readable through the pin that holds it
    agents.tag = "v3";
    agents.deliver(&mut r, &[], &[], &["d-FRGNX"]);
    let held = read(&r, "core:d-FRGNX", "compact").unwrap();
    let held = held["text"].as_str().unwrap();
    assert!(held.contains("Use X."), "the held version reads: {held}");
    assert!(held.contains("delivered: no longer delivered"), "{held}");
    assert!(
        held.contains(&format!("pinned: {}", short(&version_of(&x)))),
        "{held}"
    );
    expect_err(
        read(&r, "core:d-FRGNY", "compact").unwrap_err(),
        "E_UNKNOWN_REF",
        "core:d-FRGNY is not in the content delivered from core",
        "cite a block that core delivers: rdy_links {op: \"show\", alias} / rdy links show core",
    );
    // a local read never changes
    let local = read(&r, &sec, "compact").unwrap();
    assert_eq!(local["id"], sec);
}

#[test]
fn rpl28_links_show_lists_pending_instructions() {
    let d = tempfile::tempdir().unwrap();
    let World {
        mut r,
        link,
        mut agents,
        ..
    } = world(d.path());
    let rule = |i: &str, body: &str| block(i, "instruction", &[("applies_to", "all")], body);
    let (a1, a2) = (rule("ai-ARX01", "Always X."), rule("ai-ARX02", "Never Y."));
    agents.tag = "v2";
    agents.deliver(&mut r, &[&a1, &a2], &[], &[]);

    // no adoption yet: every delivered instruction is pending, none is in force
    let show = serde_json::to_value(r.read().link_show("core").unwrap()).unwrap();
    let pending = show["pending"].as_array().unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|p| (p["id"].as_str().unwrap(), p["change"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        [("ai-ARX01", "added"), ("ai-ARX02", "added")],
        "{show}"
    );
    assert_eq!(pending[0]["version"], version_of(&a1).to_string());
    assert_eq!(pending[0]["summary"], "Always X.");
    // the other alias of the repo and the other links have none
    for alias in ["core-people", "later", "old"] {
        let s = serde_json::to_value(r.read().link_show(alias).unwrap()).unwrap();
        assert!(s.get("pending").is_none(), "{alias}: {s}");
    }

    // the link pins RULE1, as an adoption leaves it: RULE2 stays pending
    write(
        &mut r,
        "pin",
        json!([{"op": "update", "id": link["$core"],
                "fields": {"title": "In force: {{core:ai-ARX01}}"}}]),
    );
    land(&mut r, "pin");
    let show = serde_json::to_value(r.read().link_show("core").unwrap()).unwrap();
    let pending = show["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1, "{show}");
    assert_eq!(pending[0]["id"], "ai-ARX02");
    // the baseline pin cites nothing: it is no ref and leaves no pin behind
    assert!(
        serde_json::to_value(r.read().link_refs(None).unwrap()).unwrap()["repos"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // a changed instruction is pending as changed; a removed one as removed
    let a1b = rule("ai-ARX01", "Always X, twice.");
    agents.tag = "v3";
    agents.deliver(&mut r, &[], &[&a1b], &["ai-ARX02"]);
    let show = serde_json::to_value(r.read().link_show("core").unwrap()).unwrap();
    let rows: Vec<(String, String)> = show["pending"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_string(),
                p["change"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [("ai-ARX01".to_string(), "changed".to_string())],
        "RULE2 was never in force, so its removal offers nothing: {show}"
    );
}
