//! rpl29 (repo links, D-DAPR8, D-FK58A): the working copy shows a cited block by the alias of a
//! live repo link and stores it as `{{<repo>:<id>}}`. A checkout, update, edit and commit round
//! trip keeps the stored bytes, the `@key` mark and every held pin; a rename of an alias moves no
//! stored byte; two aliases of one repo read their own profile's selection and never widen a
//! write.
use rdy_engine::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignEntry, ForeignEvent, ForeignPublication, ForeignSource, ForeignVersion,
    selection_identity,
};
use rdy_model::{Block, BlockId, FieldValue, Hash, RepoId};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{land, write, writer};
use rdy_qa::wc::*;
use rdy_store::{codec, kinds};
use serde_json::json;
use std::collections::BTreeMap;

const P1: &str = "pf-AGNT1";
const P2: &str = "pf-AGNT2";
const AT_MS: i64 = 1_760_000_000_000;

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn decision(id: &str, statement: &str) -> Block {
    Block {
        id: BlockId::parse(id).unwrap(),
        block_type: "decision".into(),
        parent: None,
        order: "V".into(),
        fields: [("statement", statement), ("status", "accepted")]
            .iter()
            .map(|(k, v)| (k.to_string(), FieldValue::Str((*v).into())))
            .collect(),
        body: String::new(),
        links: BTreeMap::new(),
        pins: BTreeMap::new(),
        foreign: Default::default(),
        read_identity: None,
    }
}

fn version_of(b: &Block) -> Hash {
    Hash::of(&codec::item_new(kinds::BLOCK, b).1)
}

/// The chain of one source of the link block `link`, delivering under `profile`.
struct Source {
    link: String,
    profile: &'static str,
    head: Option<Hash>,
    sel: BTreeMap<BlockId, Hash>,
}

impl Source {
    fn new(link: &str, profile: &'static str) -> Source {
        Source {
            link: link.to_string(),
            profile,
            head: None,
            sel: BTreeMap::new(),
        }
    }

    /// Writes a delivery straight to the store and publishes the state after it.
    fn deliver(&mut self, r: &mut Rdy, tag: &str, added: &[&Block], removed: &[&str]) {
        for b in added {
            self.sel.insert(b.id.clone(), version_of(b));
        }
        let removed: Vec<BlockId> = removed.iter().map(|i| BlockId::parse(i).unwrap()).collect();
        for id in &removed {
            self.sel.remove(id);
        }
        let entry = ForeignEntry {
            repo: cited(),
            source: ForeignSource::Link {
                link: self.link.clone(),
                site: "hub.example".into(),
                at_ms: AT_MS,
            },
            event: ForeignEvent::Delivery {
                prev: self.head,
                publication: ForeignPublication {
                    tag: tag.into(),
                    profile: self.profile.into(),
                    profile_version: None,
                },
                set: None,
                added: added
                    .iter()
                    .map(|b| ForeignVersion {
                        id: b.id.clone(),
                        version: version_of(b),
                        content: Hash::of(format!("content {} {:?}", b.id, b.fields).as_bytes()),
                        encoding: 0,
                        derived: ForeignDerived::default(),
                    })
                    .collect(),
                changed: vec![],
                removed,
                shells: vec![],
                selection: selection_identity(self.sel.iter()).unwrap(),
            },
        };
        let blocks: Vec<Block> = added.iter().map(|b| (*b).clone()).collect();
        self.head = Some(r.store_mut().put_foreign(&entry, &blocks).unwrap());
        r.publish_after_foreign_for_tests().unwrap();
    }
}

struct Cited {
    link: String,
    source: Source,
    /// A prose block that cites Y through the alias `alt`.
    alt_prose: String,
}

fn repo_text() -> String {
    cited().to_string()
}

/// The seeded project plus two repo links of one repo (`core` reads P1 and holds X, `alt` reads P2
/// and holds Y), a claim that cites X in its body and by `decided_by`, and a prose that cites Y.
fn world() -> (Served, Seed, Cited) {
    let (s, seed) = seeded();
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "create", "type": "repo_link", "id": "$core", "parent": seed.intro,
             "fields": {"alias": "core", "repo": repo_text(), "profile": P1}},
            {"op": "create", "type": "repo_link", "id": "$alt", "parent": seed.intro,
             "fields": {"alias": "alt", "repo": repo_text(), "profile": P2}}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let (link, alt) = (w.created["$core"].clone(), w.created["$alt"].clone());
    let mut source = Source::new(&link, P1);
    let mut other = Source::new(&alt, P2);
    source.deliver(
        &mut r,
        "v1",
        &[
            &decision("d-FRGNX", "Use X."),
            &decision("D-FRGNZ", "Use Z."),
        ],
        &[],
    );
    other.deliver(&mut r, "v1", &[&decision("d-FRGNY", "Use Y.")], &[]);
    let w = write(
        &mut r,
        "work",
        json!([
            {"op": "update", "id": seed.claim, "body": "Claims hold under {{core:d-FRGNX}}.",
             "links": {"decided_by": [{"target": seed.decision}, {"target": "core:d-FRGNX"}],
                       "describes": [{"target": seed.prose}, {"target": "core:d-FRGNX"},
                                     {"target": "core:D-FRGNZ"}]}},
            {"op": "create", "type": "prose", "id": "$p", "parent": seed.section, "after": seed.claim,
             "body": "Also see {{alt:d-FRGNY}}."}
        ]),
    )
    .unwrap();
    land(&mut r, "work").unwrap();
    let alt_prose = w.created["$p"].clone();
    r.close().unwrap();
    (
        s,
        seed,
        Cited {
            link,
            source,
            alt_prose,
        },
    )
}

fn stored_body(token: &str) -> String {
    format!("{{{{{}:{token}}}}}", repo_text())
}

#[test]
fn rpl29_alias_rename_roundtrip_keeps_stored_bytes() {
    let (s, seed, c) = world();
    let before = store_block(&s, "work", &seed.claim);
    assert_eq!(
        before.body,
        format!("Claims hold under {}.", stored_body("d-FRGNX")),
        "the store keeps the repo form"
    );
    let pins = before.foreign.pins.clone();
    assert_eq!(pins.len(), 2, "{pins:?}");

    // checkout shows the alias, never the repo's hex digits
    ok(&s, &["checkout"]);
    let file = read(&s, "guides/intro.yaml");
    assert!(!file.contains(&repo_text()), "{file}");
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.claim)["body"].as_str(),
        Some("Claims hold under {{core:d-FRGNX}}.")
    );

    // the alias is renamed in the store; update shows the new alias and moves no stored byte
    store_write(
        &s,
        "work",
        json!([{"op": "update", "id": c.link, "fields": {"alias": "lib"}}]),
    );
    store_land(&s, "work");
    ok(&s, &["update"]);
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.claim)["body"].as_str(),
        Some("Claims hold under {{lib:d-FRGNX}}.")
    );
    let renamed = store_block(&s, "work", &seed.claim);
    assert_eq!(renamed.version_hash(), before.version_hash());

    // an edit of the same entry commits under the new alias and keeps the stored repo form,
    // the foreign link and the held pin
    set_body(
        &s,
        "guides/intro.yaml",
        &seed.claim,
        "Claims hold under {{lib:d-FRGNX}}, firmly.",
    );
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "edit a cited claim"]);
    assert!(okk, "{v}");
    let after = store_block(&s, "work", &seed.claim);
    assert_eq!(
        after.body,
        format!("Claims hold under {}, firmly.", stored_body("d-FRGNX"))
    );
    assert_eq!(after.foreign.pins, pins);
    assert_eq!(after.foreign.links, before.foreign.links);
    assert!(
        entry(&s, "guides/intro.yaml", &seed.claim)["body"]
            .as_str()
            .is_some_and(|b| b == "Claims hold under {{lib:d-FRGNX}}, firmly."),
        "written back as the alias"
    );
}

#[test]
fn rpl29_two_profiles_cannot_widen_a_write() {
    let (s, seed, c) = world();
    ok(&s, &["checkout"]);
    // each block shows the alias whose profile selection holds its target, though `alt` sorts
    // before `core`
    assert_eq!(
        entry(&s, "guides/intro.yaml", &seed.claim)["body"].as_str(),
        Some("Claims hold under {{core:d-FRGNX}}.")
    );
    assert_eq!(
        entry(&s, "guides/intro.yaml", &c.alt_prose)["body"].as_str(),
        Some("Also see {{alt:d-FRGNY}}.")
    );

    // citing X under the alias whose profile does not deliver it is refused, and nothing lands
    let kept = store_block(&s, "work", &c.alt_prose);
    set_body(
        &s,
        "guides/intro.yaml",
        &c.alt_prose,
        "Also see {{alt:d-FRGNX}}.",
    );
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "widen"]);
    assert!(!okk, "{v}");
    assert!(
        v["message"]
            .as_str()
            .unwrap()
            .contains("alt:d-FRGNX is not in the content delivered from alt"),
        "{v}"
    );
    assert_eq!(store_block(&s, "work", &c.alt_prose), kept);

    // the alias that does deliver it takes the citation and pins X
    set_body(
        &s,
        "guides/intro.yaml",
        &c.alt_prose,
        "Also see {{core:d-FRGNX}} and {{alt:d-FRGNY}}.",
    );
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "cite both"]);
    assert!(okk, "{v}");
    let now = store_block(&s, "work", &c.alt_prose);
    assert_eq!(
        now.body,
        format!(
            "Also see {} and {}.",
            stored_body("d-FRGNX"),
            stored_body("d-FRGNY")
        )
    );
    assert_eq!(now.foreign.pins.len(), 2, "{:?}", now.foreign.pins);
}

#[test]
fn rpl29_a_held_pin_that_no_alias_selects_stays_in_the_stored_form() {
    let (s, seed, mut c) = world();
    // the source drops X: no alias selects it now, but the claim still holds its pin
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    c.source.deliver(&mut r, "v2", &[], &["d-FRGNX"]);
    r.close().unwrap();
    let before = store_block(&s, "work", &seed.claim);
    ok(&s, &["checkout"]);
    let shown = entry(&s, "guides/intro.yaml", &seed.claim)["body"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        shown,
        format!("Claims hold under {}.", stored_body("d-FRGNX")),
        "no alias resolves it, so the file keeps the stored form"
    );
    // an edit around it commits and keeps the pin
    set_body(
        &s,
        "guides/intro.yaml",
        &seed.claim,
        &format!("{shown} Still holds."),
    );
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "edit around a held pin"]);
    assert!(okk, "{v}");
    let after = store_block(&s, "work", &seed.claim);
    assert_eq!(after.body, format!("{shown} Still holds."));
    assert_eq!(after.foreign.pins, before.foreign.pins);
}

#[test]
fn rpl29_a_link_edit_keeps_the_foreign_targets_of_its_kind() {
    let (s, seed, _c) = world();
    let before = store_block(&s, "work", &seed.claim);
    assert_eq!(before.foreign.links["describes"].len(), 2);
    ok(&s, &["checkout"]);
    // the file shows the cited block by its alias; a cited decision is a store-only type, so it
    // stays out of the file, as a local one does
    let shown = entry(&s, "guides/intro.yaml", &seed.claim);
    let targets: Vec<&str> = shown["links"]["describes"]
        .as_sequence()
        .unwrap()
        .iter()
        .map(|l| l["target"].as_str().unwrap())
        .collect();
    assert_eq!(targets, [seed.prose.as_str(), "core:d-FRGNX"], "{shown:?}");
    // a second local target changes the kind; both cited targets stay
    edit(&s, "guides/intro.yaml", |list| {
        let e = list
            .iter_mut()
            .find(|e| e["id"].as_str() == Some(seed.claim.as_str()))
            .unwrap();
        let d = e["links"]["describes"].as_sequence_mut().unwrap();
        d.push(serde_yaml_ng::from_str(&format!("target: {}", seed.note)).unwrap());
    });
    ok(&s, &["add", "guides/intro.yaml"]);
    let (okk, v) = json(&s, &["commit", "-m", "another description"]);
    assert!(okk, "{v}");
    let after = store_block(&s, "work", &seed.claim);
    assert_eq!(after.links["describes"].len(), 2);
    assert_eq!(after.foreign.links["describes"].len(), 2);
    assert_eq!(
        after.foreign.links["describes"],
        before.foreign.links["describes"]
    );
    assert_eq!(after.foreign.pins, before.foreign.pins);
}

#[test]
fn rpl29_an_edit_buffer_shows_aliases_and_keeps_stored_bytes_and_pins() {
    let (s, seed, _c) = world();
    s.stop();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let scope = Scope::View("work".into());
    let id = BlockId::parse(&seed.claim).unwrap();
    let before = r.read().block(&scope, &id).unwrap();
    let yaml = r.read().block_yaml(&scope, &before).unwrap();
    assert!(!yaml.contains(&repo_text()), "{yaml}");
    assert!(
        yaml.contains("Claims hold under {{core:d-FRGNX}}.")
            && yaml.contains("core:d-FRGNX")
            && yaml.contains("core:D-FRGNZ"),
        "{yaml}"
    );

    // a buffer nobody touched changes nothing
    let e = r
        .edit_yaml(&writer(), "work", &yaml, None, None)
        .unwrap_err();
    assert_eq!(
        e.message,
        format!("nothing changed in {}", seed.claim),
        "{e:?}"
    );

    // a changed body keeps the stored repo form of its reference, the cited links and every pin
    let edited = yaml.replace("Claims hold under", "Claims still hold under");
    r.edit_yaml(&writer(), "work", &edited, None, None).unwrap();
    let after = r.read().block(&scope, &id).unwrap();
    assert_eq!(
        after.body,
        format!("Claims still hold under {}.", stored_body("d-FRGNX"))
    );
    assert_eq!(after.foreign.links, before.foreign.links);
    assert_eq!(after.foreign.pins, before.foreign.pins);

    // a cited link the buffer drops is removed; the others stay
    let dropped: String = r
        .read()
        .block_yaml(&scope, &after)
        .unwrap()
        .lines()
        .filter(|l| !l.contains("core:D-FRGNZ"))
        .map(|l| format!("{l}\n"))
        .collect();
    r.edit_yaml(&writer(), "work", &dropped, None, None)
        .unwrap();
    let last = r.read().block(&scope, &id).unwrap();
    assert_eq!(last.foreign.links["describes"].len(), 1);
    assert_eq!(
        last.foreign.links["decided_by"],
        before.foreign.links["decided_by"]
    );
    r.close().unwrap();
}
