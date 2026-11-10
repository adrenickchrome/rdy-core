//! rpl31 (repo links, D-DAPR8, D-8VJAZ, D-KQ4CA): the command line's parity with the repo-link
//! tools. `rdy new repo-link`, `rdy links` (with `show` and `refs`), `rdy show <alias>:<id>`,
//! `rdy adopt-upstream` and the per-link lines of `rdy status` run through the project's daemon,
//! read what the tools read, and write only through the ordinary write path. The released
//! commands keep their JSON fields and exit values; the CLI has no Later and no exchange command.
use rdy_engine::foreign_delivery::{DeliveryTaken, take_delivery};
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_model::foreign::{
    ForeignDerived, ForeignPublication, ForeignSource, ForeignVersion, selection_identity,
};
use rdy_model::{Block, BlockId, FieldValue, Hash, RepoId};
use rdy_qa::daemon::Served;
use rdy_qa::fixtures::{land, write, writer};
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

const PROFILE: &str = "pf-AGNT1";

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// A cited block, as its home project delivers it.
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

/// The cited decision that is the upstream copy of the local one.
fn upstream() -> Block {
    block(
        "D-PSTRM",
        "decision",
        &[
            ("statement", "Every job logs in UTC."),
            ("status", "accepted"),
        ],
        "",
    )
}

fn claim(body: &str) -> Block {
    block("c-FRGNC", "claim", &[], body)
}

/// A link delivery stream of the cited repo holding `selected`, written to `dir/name`.
fn stream(dir: &Path, name: &str, selected: &[&Block]) -> File {
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
            profile: PROFILE.into(),
            profile_version: None,
        },
        selection: selection_identity(versions.iter().map(|v| (&v.id, &v.version))).unwrap(),
        versions,
        shells: vec![],
    };
    let header =
        StreamHeader::link_delivery(baseline_features(), cited(), Hash::of(name.as_bytes()));
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

fn deliver(r: &mut Rdy, dir: &Path, name: &str, link: &BlockId, selected: &[&Block]) {
    let source = ForeignSource::Link {
        link: link.to_string(),
        site: "hub.example".into(),
        at_ms: 1_760_000_000_000,
    };
    let _: DeliveryTaken = take_delivery(r, &stream(dir, name, selected), &source, None)
        .unwrap_or_else(|e| panic!("take {name}: {e:?}"));
}

/// A served project with the repo link `core`, its first delivery, the landed accepted decision
/// `old`, and the landed claim that cites the delivered claim. `behind` adds a second delivery
/// that changes the cited claim: the pin is behind and an adopt card stands.
struct World {
    s: Served,
    doc: String,
    old: String,
}

fn world(behind: bool) -> World {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let files = tempfile::tempdir().unwrap();
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d",
             "fields": {"title": "Guide", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d",
             "fields": {"title": "Rules"}},
            {"op": "create", "type": "repo_link", "id": "$l", "parent": "$d",
             "fields": {"alias": "core", "repo": cited().to_string(), "profile": PROFILE}}
        ]),
    )
    .unwrap();
    land(&mut r, "seed").unwrap();
    let (doc, section, link) = (
        w.created["$d"].clone(),
        id(&w.created["$s"]),
        id(&w.created["$l"]),
    );
    deliver(
        &mut r,
        files.path(),
        "v1",
        &link,
        &[&upstream(), &claim("Every service logs in UTC.")],
    );
    let decided = r
        .decide(
            &writer(),
            "dec",
            DecisionInput {
                parent: section.to_string(),
                statement: "Every job logs in UTC.".into(),
                title: Some("Log time".into()),
                ..Default::default()
            },
        )
        .unwrap();
    let old = decided.created["$decision"].clone();
    land(&mut r, "dec").unwrap();
    write(
        &mut r,
        "cite",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": section.to_string(),
                "body": "Our billing jobs follow {{core:c-FRGNC}}."}]),
    )
    .unwrap();
    land(&mut r, "cite").unwrap();
    if behind {
        deliver(
            &mut r,
            files.path(),
            "v2",
            &link,
            &[&upstream(), &claim("Every service logs in local time.")],
        );
    }
    r.close().unwrap();
    World { s, doc, old }
}

/// Whether the command succeeded and what it printed: stdout on success, stderr on failure.
fn text(o: &std::process::Output) -> (bool, String) {
    let ok = o.status.success();
    let t = String::from_utf8_lossy(if ok { &o.stdout } else { &o.stderr });
    (ok, t.trim_end().to_string())
}

fn json_ok(s: &Served, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let (ok, v) = s.json(&all);
    assert!(ok, "{args:?}: {v}");
    v
}

fn json_err(s: &Served, args: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    let (ok, v) = s.json(&all);
    assert!(!ok, "{args:?} should fail: {v}");
    v
}

#[test]
fn rpl31_cli_links_new_show_and_adopt_upstream() {
    let w = world(true);
    let s = &w.s;
    // rdy links: one line per link, then the JSON the tool gives
    let (ok, out) = text(&s.rdy(&["links"]));
    assert!(ok, "{out}");
    assert!(
        out.starts_with("link core: delivered, tag v2, repo "),
        "{out}"
    );
    assert!(out.contains("1 pin behind"), "{out}");
    let v = json_ok(s, &["links"]);
    assert_eq!(v["links"][0]["alias"], "core", "{v}");
    assert_eq!(v["links"][0]["state"], "delivered", "{v}");
    assert_eq!(v["links"][0]["tag"], "v2", "{v}");
    assert_eq!(v["links"][0]["pins_behind"], 1, "{v}");
    assert_eq!(v["unlinked"], json!([]), "{v}");
    // rdy links show: what the alias delivers, with each block's pin state
    let shown = json_ok(s, &["links", "show", "core"]);
    let rows = shown["blocks"].as_array().unwrap();
    let claim_row = rows.iter().find(|b| b["id"] == "c-FRGNC").expect("claim");
    assert_eq!(claim_row["pin"], "behind", "{shown}");
    let (ok, out) = text(&s.rdy(&["links", "show", "core"]));
    assert!(ok, "{out}");
    assert!(out.starts_with("link core: delivered"), "{out}");
    assert!(
        out.contains("c-FRGNC claim") && out.contains("behind"),
        "{out}"
    );
    // rdy links refs: the pins of canon to cited content, for all links and for one
    for args in [vec!["links", "refs"], vec!["links", "refs", "core"]] {
        let refs = json_ok(s, &args);
        assert_eq!(refs["repos"][0]["alias"], "core", "{refs}");
        assert_eq!(refs["repos"][0]["pins"][0]["target"], "c-FRGNC", "{refs}");
        assert_eq!(refs["repos"][0]["pins"][0]["behind"], true, "{refs}");
    }
    let (ok, out) = text(&s.rdy(&["links", "refs"]));
    assert!(
        ok && out.contains("core") && out.contains("c-FRGNC"),
        "{out}"
    );
    // an unknown alias is E_NOT_FOUND, with the decided message and fix
    for args in [vec!["links", "show", "nope"], vec!["links", "refs", "nope"]] {
        let e = json_err(s, &args);
        assert_eq!(e["code"], "E_NOT_FOUND", "{e}");
        assert_eq!(e["message"], "no repo link has the alias nope", "{e}");
        assert_eq!(e["fix"], "list the links: rdy_links / rdy links", "{e}");
    }
    // rdy show reads a cited block read-only, with its provenance
    let (ok, out) = text(&s.rdy(&["show", "core:c-FRGNC"]));
    assert!(ok, "{out}");
    assert!(out.contains("delivered by the hub (link "), "{out}");
    assert!(out.contains("local time"), "{out}");
    let e = json_err(s, &["show", "core:c-FRGNC", "--blame"]);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    let e = json_err(s, &["show", "nope:c-FRGNC"]);
    assert_eq!(e["code"], "E_UNKNOWN_REF", "{e}");
    // rdy new repo-link: one change, in the person's view; the alias is unique
    let other = RepoId([8; 16]).to_string();
    let (ok, out) = text(&s.rdy(&[
        "new",
        "repo-link",
        "peer",
        "--parent",
        &w.doc,
        "--repo",
        &other,
        "--profile",
        "pf-OTHER",
        "--title",
        "The peer project",
    ]));
    assert!(ok, "{out}");
    assert!(out.starts_with("created repo_link rl-"), "{out}");
    let created = json_ok(
        s,
        &[
            "new",
            "repo-link",
            "second",
            "--parent",
            &w.doc,
            "--repo",
            &other,
            "--profile",
            "pf-THIRD",
        ],
    );
    let link_id = created["created"]["$repo_link"].as_str().unwrap();
    let block = json_ok(s, &["show", link_id, "--scope", "view:work"]);
    assert_eq!(block["fields"]["alias"], "second", "{block}");
    assert_eq!(block["fields"]["repo"], other, "{block}");
    let e = json_err(
        s,
        &[
            "new",
            "repo-link",
            "core",
            "--parent",
            &w.doc,
            "--repo",
            &other,
            "--profile",
            "pf-OTHER",
        ],
    );
    assert_eq!(e["code"], "E_VALIDATION", "{e}");
    assert_eq!(
        e["message"], "repo link core: alias is already in use",
        "{e}"
    );
    // rdy adopt-upstream: a cited claim is no decision; the cited decision supersedes the old one
    let e = json_err(s, &["adopt-upstream", &w.old, "core:c-FRGNC"]);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(
        e["message"].as_str().unwrap().contains("not a decision"),
        "{e}"
    );
    let e = json_err(s, &["adopt-upstream", &w.old, "nope:D-PSTRM"]);
    assert_eq!(e["code"], "E_UNKNOWN_REF", "{e}");
    assert_eq!(e["message"], "no repo link has the alias nope", "{e}");
    let (ok, out) = text(&s.rdy(&["adopt-upstream", &w.old, "core:D-PSTRM"]));
    assert!(ok, "{out}");
    assert!(out.starts_with("change "), "{out}");
    let old = json_ok(s, &["show", &w.old, "--scope", "view:work"]);
    assert!(
        old["status_words"]
            .as_str()
            .unwrap()
            .starts_with("superseded"),
        "{old}"
    );
    // canon still holds the old decision in force: nothing landed
    let canon = json_ok(s, &["show", &w.old]);
    assert_eq!(canon["status_words"], "live", "{canon}");
}

#[test]
fn rpl31_cli_json_preserves_existing_fields_and_exits() {
    // a project with no link: status has no `links` member, and every released member stays
    let bare = Served::new();
    let v = json_ok(&bare, &["status"]);
    for key in [
        "pack",
        "daemon",
        "landings",
        "suspects",
        "version_position",
        "views",
        "warnings",
        "seq",
        "open_report",
        "quarantine",
    ] {
        assert!(v.get(key).is_some(), "{key} in {v}");
    }
    assert!(v.get("links").is_none(), "{v}");
    let (ok, out) = text(&bare.rdy(&["links"]));
    assert!(ok && out == "no repo links", "{out}");
    let v = json_ok(&bare, &["links"]);
    assert_eq!(v, json!({"links": [], "unlinked": []}));

    // a project with a link: the same members, plus the links
    let w = world(true);
    let s = &w.s;
    let linked = json_ok(s, &["status"]);
    for key in [
        "pack",
        "daemon",
        "landings",
        "suspects",
        "version_position",
        "views",
        "warnings",
        "seq",
        "open_report",
        "quarantine",
    ] {
        assert!(linked.get(key).is_some(), "{key} in {linked}");
    }
    assert_eq!(linked["links"]["links"][0]["alias"], "core", "{linked}");
    assert_eq!(
        linked["links"]["links"][0]["state"], "delivered",
        "{linked}"
    );
    assert_eq!(linked["links"]["links"][0]["pins_behind"], 1, "{linked}");
    assert_eq!(linked["links"]["unlinked"], 0, "{linked}");
    let (ok, out) = text(&s.rdy(&["status"]));
    assert!(ok, "{out}");
    assert!(
        out.contains("\nlink core: delivered, tag v2, 1 pin behind"),
        "{out}"
    );
    // a released read keeps its shape: show of a local decision has no cited members
    let shown = json_ok(s, &["show", &w.old]);
    assert!(
        shown.get("cited").is_none() && shown["id"] == w.old,
        "{shown}"
    );

    // exits: a refusal exits 1, as every refusal does; a usage error exits 2, as clap's
    let refused = s.rdy(&["links", "show", "nope"]);
    let released = s.rdy(&["show", "no-such-block"]);
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(refused.status.code(), released.status.code());
    for args in [
        vec!["links", "bogus"],
        vec!["links", "show"],
        vec!["adopt-upstream", "D-AAAAA"],
        vec!["new", "repo-link", "x"],
    ] {
        assert_eq!(s.rdy(&args).status.code(), Some(2), "{args:?}");
    }
    // no exchange between two projects, and no hub verb, is a command
    for args in [
        vec!["links", "take"],
        vec!["links", "export"],
        vec!["links", "revoke"],
        vec!["take-delivery"],
        vec!["link-export"],
    ] {
        assert_eq!(s.rdy(&args).status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn rpl31_cli_adopt_has_no_later() {
    let w = world(true);
    let s = &w.s;
    let q = json_ok(s, &["pending"]);
    let card = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "adopt")
        .unwrap_or_else(|| panic!("an adopt card in {q}"));
    let r#ref = card["ref"].as_str().unwrap().to_string();
    assert!(
        r#ref.starts_with(&format!("adopt:{}@", cited())),
        "{}",
        r#ref
    );
    // the CLI has no Later: the refusal is the queue's own, and the card stays
    let e = json_err(s, &["answer", &r#ref, "later"]);
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert_eq!(e["message"], "the CLI has no Later", "{e}");
    let again = json_ok(s, &["pending"]);
    assert!(
        again["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["ref"] == r#ref.as_str()),
        "{again}"
    );
    assert_eq!(again["snoozed"], 0, "{again}");
    // no command takes the person's Later: neither links nor adopt-upstream has the word
    for args in [
        vec!["links", "later"],
        vec!["adopt-upstream", "--later"],
        vec!["links", "--later"],
    ] {
        assert_eq!(s.rdy(&args).status.code(), Some(2), "{args:?}");
    }
    // adopt repins in one landing; the link then has nothing behind
    let (ok, out) = text(&s.rdy(&["answer", &r#ref, "adopt"]));
    assert!(ok, "{out}");
    let v = json_ok(s, &["links"]);
    assert_eq!(v["links"][0]["pins_behind"], 0, "{v}");
}

#[test]
fn rpl31_daemon_reads_delivery_without_canon_change() {
    // the project before its second delivery: canon's landings and position
    let w = world(false);
    let s = &w.s;
    let before = json_ok(s, &["status"]);
    let one = json_ok(s, &["links"]);
    assert_eq!(one["links"][0]["tag"], "v1", "{one}");
    assert_eq!(one["links"][0]["pins_behind"], 0, "{one}");
    s.stop();
    // a delivery arrives while no daemon runs, as a pull brings it
    {
        let mut r = Rdy::open(s.dir.path()).unwrap();
        let files = tempfile::tempdir().unwrap();
        let link = {
            let st = r.read();
            let m = st.materialized(&Scope::Canon).unwrap();
            m.iter()
                .find(|b| b.block_type == "repo_link")
                .expect("the link")
                .id
                .clone()
        };
        deliver(
            &mut r,
            files.path(),
            "v2",
            &link,
            &[&upstream(), &claim("Every service logs in local time.")],
        );
        r.close().unwrap();
    }
    // the daemon, started fresh, reads the delivery and moves nothing in canon
    let after = json_ok(s, &["status"]);
    assert_eq!(after["landings"], before["landings"], "{after}");
    assert_eq!(
        after["version_position"], before["version_position"],
        "{after}"
    );
    assert_eq!(after["suspects"], before["suspects"], "{after}");
    let two = json_ok(s, &["links"]);
    assert_eq!(two["links"][0]["tag"], "v2", "{two}");
    assert_eq!(two["links"][0]["pins_behind"], 1, "{two}");
    // the pin did not move: the cited claim reads at the old text through canon's pin
    let log = json_ok(s, &["log"]);
    assert_eq!(
        log.as_array().unwrap().len() as u64,
        before["landings"].as_u64().unwrap(),
        "{log}"
    );
}
