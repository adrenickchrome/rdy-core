//! rpl30 (repo links, D-DAPR8, D-8VJAZ, D-Y18E1): the MCP face of repo links.
//!
//! - `rdy_links {op: list|show|refs, alias?}` reads the links, what one alias delivers and the
//!   pins of canon to cited content. It is a read of the `rdy:read` ruleset, in warn mode. The
//!   hub's `link_export`, `take_delivery` and `revoke_link` are no tools.
//! - `rdy_read` and `rdy_context` take `<alias>:<id>`; `rdy_decide` takes `deviates_from`;
//!   `rdy_supersede` takes `adopted_as` in place of `decision`; `rdy_answer` answers an adopt card.
//! - Unknown keys and the two supersede branches together are refused with nothing written.
use rdy_engine::engine::Local;
use rdy_engine::foreign_delivery::{DeliveryTaken, take_delivery};
use rdy_engine::{DecisionInput, Rdy, Scope};
use rdy_graph::DerivedStatus;
use rdy_mcp::params::parse_params;
use rdy_mcp::tools::*;
use rdy_model::foreign::{
    ForeignDerived, ForeignPublication, ForeignSource, ForeignVersion, selection_identity,
};
use rdy_model::{Block, BlockId, FieldValue, Hash, RepoId};
use rdy_qa::fixtures::{land, write, writer};
use rdy_qa::mcp::{McpClient, Reply, supply_kungfu};
use rdy_store::codec;
use rdy_store::format::baseline_features;
use rdy_store::kinds;
use rdy_store::object_refs::ObjectKey;
use rdy_store::sync_stream::{DeliveryRecord, StreamHeader, StreamRecord, StreamWriter};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

const PROFILE: &str = "pf-AGNT1";

fn cited() -> RepoId {
    RepoId([7; 16])
}

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

fn p<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).unwrap()
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

/// A project with the repo link `core`, its first delivery (the upstream decision and a claim),
/// the landed accepted decision `old`, and the landed claim `x` that cites the delivered claim.
struct World {
    _d: tempfile::TempDir,
    dir: std::path::PathBuf,
    st: McpState,
    section: BlockId,
    old: BlockId,
}

/// The project before any server holds it.
struct Seeded {
    d: tempfile::TempDir,
    r: Rdy,
    section: BlockId,
    old: BlockId,
}

fn seeded() -> Seeded {
    let (d, mut r) = rdy_qa::fixtures::project();
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
    let (section, link) = (id(&w.created["$s"]), id(&w.created["$l"]));
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
    let old = id(&decided.created["$decision"]);
    land(&mut r, "dec").unwrap();
    write(
        &mut r,
        "cite",
        json!([{"op": "create", "type": "claim", "id": "$x", "parent": section.to_string(),
                "body": "Our billing jobs follow {{core:c-FRGNC}}."}]),
    )
    .unwrap();
    land(&mut r, "cite").unwrap();
    // a later delivery changes the cited claim: the pin is behind, and an adopt card stands
    deliver(
        &mut r,
        files.path(),
        "v2",
        &link,
        &[&upstream(), &claim("Every service logs in local time.")],
    );
    Seeded { d, r, section, old }
}

fn world() -> World {
    let s = seeded();
    let dir = s.d.path().to_path_buf();
    // the person of the project: the one an answer is recorded for
    std::fs::write(dir.join(".rdy/local.yaml"), "actor: human:qa\n").unwrap();
    let mut st = McpState::new(Arc::new(Local::new(s.r)));
    rdy_session(
        &mut st,
        p(json!({"actor": "agent:qa", "run": "qa", "role": "writer"})),
    )
    .unwrap();
    World {
        _d: s.d,
        dir,
        st,
        section: s.section,
        old: s.old,
    }
}

fn tool_names(c: &mut McpClient) -> (Vec<String>, Vec<Value>) {
    let tools = c.tools();
    (
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect(),
        tools,
    )
}

#[test]
fn rpl30_links_schema_and_lists_agree() {
    let s = seeded();
    let dir = s.d.path().to_path_buf();
    s.r.close().unwrap();
    let mut c = McpClient::start(&dir);
    let (names, tools) = tool_names(&mut c);
    let by = |n: &str| -> &Value { &tools[names.iter().position(|x| x == n).expect(n)] };
    // rdy_links: a closed input with op and alias
    let links = by("rdy_links");
    assert_eq!(links["inputSchema"]["additionalProperties"], json!(false));
    let mut keys: Vec<&str> = links["inputSchema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(keys, ["alias", "op"]);
    let desc = links["description"].as_str().unwrap();
    for word in ["list", "show", "refs"] {
        assert!(desc.contains(word), "{desc}");
    }
    // the hub's tools are not here
    for hub in ["rdy_link_export", "rdy_take_delivery", "rdy_revoke_link"] {
        assert!(!names.iter().any(|n| n == hub), "{hub}");
    }
    assert!(
        !names
            .iter()
            .any(|n| n.contains("take_delivery") || n.contains("link_export")),
        "{names:?}"
    );
    // every tool of the kungfu table is listed, and the table lists no other
    for n in &names {
        assert!(
            rdy_engine::kungfu::policy(n).is_some(),
            "no kungfu row: {n}"
        );
    }
    // the added parameters
    let decide = by("rdy_decide");
    assert!(
        decide["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("deviates_from")
    );
    let supersede = by("rdy_supersede");
    let props = supersede["inputSchema"]["properties"].as_object().unwrap();
    for key in ["old", "part", "decision", "adopted_as", "intent"] {
        assert!(props.contains_key(key), "{key}");
    }
    // neither branch is required alone: the call names one of them
    assert_eq!(supersede["inputSchema"]["required"], json!(["old"]));
    // the redaction targets name the cited form
    let redact = by("rdy_redact")["inputSchema"]["properties"]["targets"]["description"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        redact.contains("<alias>:<id>@<64-hex version hash>"),
        "{redact}"
    );
}

#[test]
fn rpl30_kungfu_row_is_required_and_warns() {
    let row = rdy_engine::kungfu::policy("rdy_links").expect("a table row for rdy_links");
    assert_eq!(row.ruleset, Some("rdy:read"));
    assert_eq!(row.response, rdy_engine::kungfu::Response::Warn);
    let owner = rdy_engine::kungfu::owner("read").unwrap();
    assert!(owner.value.starts_with("kf-"));
    // the other hub names have no row
    for gone in ["rdy_link_export", "rdy_take_delivery", "rdy_revoke_link"] {
        assert!(rdy_engine::kungfu::policy(gone).is_none(), "{gone}");
    }
    // served: a call with no value warns once, and the right value does not
    let s = seeded();
    let dir = s.d.path().to_path_buf();
    s.r.close().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut c = McpClient::start_isolated(&dir, home.path()).raw();
    let r = c.call(
        "rdy_session",
        json!({"actor": "agent:k", "run": "k1", "role": "reader"}),
    );
    assert!(r.is_ok(), "{r:?}");
    let body = |r: Reply| match r {
        Reply::Ok(v) | Reply::ToolError(v) => v,
        Reply::RpcError(e) => panic!("a protocol error: {e}"),
    };
    let miss = |v: &Value| -> Vec<Value> {
        v["warnings"]
            .as_array()
            .map(|all| {
                all.iter()
                    .filter(|w| w["code"] == "W_KUNGFU")
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    let v = body(c.call("rdy_links", json!({})));
    assert!(v["links"].is_array(), "{v}");
    let warned = miss(&v);
    assert_eq!(warned.len(), 1, "{v}");
    assert_eq!(warned[0]["ruleset"], "rdy:read");
    // the other tool of the owner shares the one mark: no second warning
    let v = body(c.call("rdy_tags", json!({})));
    assert!(miss(&v).is_empty(), "{v}");
    let r = c.call("rdy_instructions", json!({"load": ["rdy:read"]}));
    assert!(r.is_ok(), "{r:?}");
    let mut args = json!({});
    supply_kungfu("rdy_links", &mut args);
    let v = body(c.call("rdy_links", args));
    assert!(miss(&v).is_empty(), "{v}");
}

#[test]
fn rpl30_foreign_reads_decide_supersede_and_answer_work() {
    let mut w = world();
    // list: the link, delivered
    let v = rdy_links(&mut w.st, p(json!({}))).unwrap();
    assert_eq!(v["links"][0]["alias"], "core", "{v}");
    assert_eq!(v["links"][0]["state"], "delivered", "{v}");
    // show: what core delivers, with the claim the project pins
    let v = rdy_links(&mut w.st, p(json!({"op": "show", "alias": "core"}))).unwrap();
    let shown: Vec<&str> = v["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap())
        .collect();
    assert_eq!(shown, ["D-PSTRM", "c-FRGNC"], "{v}");
    // refs: the pin of canon to the claim, with no alias and with one
    let v = rdy_links(&mut w.st, p(json!({"op": "refs"}))).unwrap();
    assert_eq!(v["repos"][0]["alias"], "core", "{v}");
    let v = rdy_links(&mut w.st, p(json!({"op": "refs", "alias": "core"}))).unwrap();
    assert_eq!(v["repos"][0]["pins"][0]["target"], "c-FRGNC", "{v}");
    // a bare call lists, an unknown alias is not found
    let e = rdy_links(&mut w.st, p(json!({"op": "show", "alias": "nope"}))).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_NOT_FOUND",
            "no repo link has the alias nope",
            "list the links: rdy_links / rdy links"
        )
    );
    // read and context take the cited form
    let v = rdy_read(
        &mut w.st,
        p(json!({"id": "core:D-PSTRM", "scope": "canon"})),
    )
    .unwrap();
    assert!(
        v["text"]
            .as_str()
            .unwrap()
            .contains("provenance: delivered by the hub"),
        "{v}"
    );
    let v = rdy_context(
        &mut w.st,
        p(json!({"id": "core:D-PSTRM", "scope": "canon"})),
    )
    .unwrap();
    assert_eq!(v["cited"], true, "{v}");
    assert!(
        v["text"]
            .as_str()
            .unwrap()
            .contains("provenance: delivered by the hub"),
        "{v}"
    );
    let e = rdy_context(
        &mut w.st,
        p(json!({"id": "nope:D-PSTRM", "scope": "canon"})),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_REF", "{e:?}");
    // decide: a deviation from the cited decision is written; a local target is refused
    let d = rdy_decide(
        &mut w.st,
        p(
            json!({"statement": "Billing logs in local time.", "title": "Billing time",
                 "parent": w.section.to_string(), "deviates_from": ["core:D-PSTRM"]}),
        ),
    )
    .unwrap();
    assert!(d["created"]["$decision"].is_string(), "{d}");
    let e = rdy_decide(
        &mut w.st,
        p(
            json!({"statement": "Reports log in local time.", "title": "Report time",
                 "parent": w.section.to_string(), "deviates_from": [w.old.to_string()]}),
        ),
    )
    .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION", "{e:?}");
    assert!(e.message.contains("not cited or external"), "{e:?}");
    // answer: the delivery that changed the cited claim made an adopt card, and keep answers it
    let q = rdy_pending(&mut w.st, p(json!({}))).unwrap();
    let card = q["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "adopt")
        .unwrap_or_else(|| panic!("an adopt card in {q}"))
        .clone();
    let options: Vec<&str> = card["options"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    assert_eq!(options, ["adopt", "keep", "later"], "{card}");
    let a = rdy_answer(
        &mut w.st,
        p(
            json!({"ref": card["ref"], "option": "keep", "card_hash": card["card_hash"],
                 "note": "Billing stays on UTC until the audit.",
                 "evidence": "Keep it.", "question": "Adopt the change from core?",
                 "session_ref": "chat-1"}),
        ),
    )
    .unwrap();
    assert_eq!(a["act"], "keep_pin", "{a}");
    let q = rdy_pending(&mut w.st, p(json!({}))).unwrap();
    assert!(
        q["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i["kind"] != "adopt"),
        "the card is settled: {q}"
    );
}

#[test]
fn rpl30_unknown_keys_and_mixed_supersede_refuse() {
    let mut w = world();
    let _ = &w.dir;
    let map = |v: Value| v.as_object().unwrap().clone();
    // an unknown key of rdy_links, with the near name, and nothing run
    let e = parse_params::<LinksParams>("rdy_links", map(json!({"op": "show", "alais": "core"})))
        .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert_eq!(e.message, "rdy_links: unknown parameter \"alais\"");
    assert!(e.fix.contains("did you mean \"alias\"?"), "{}", e.fix);
    let e = parse_params::<DecideParams>(
        "rdy_decide",
        map(json!({"statement": "x", "deviates_frm": ["core:D-PSTRM"]})),
    )
    .unwrap_err();
    assert!(
        e.fix.contains("did you mean \"deviates_from\"?"),
        "{}",
        e.fix
    );
    let e = parse_params::<SupersedeParams>(
        "rdy_supersede",
        map(json!({"old": "D-AAAAA", "adopted_a": "core:D-PSTRM"})),
    )
    .unwrap_err();
    assert!(e.fix.contains("did you mean \"adopted_as\"?"), "{}", e.fix);
    // the op and alias rules
    let e = rdy_links(&mut w.st, p(json!({"op": "export"}))).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.fix.contains("list, show, refs"), "{}", e.fix);
    let e = rdy_links(&mut w.st, p(json!({"op": "show"}))).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    let e = rdy_links(&mut w.st, p(json!({"op": "list", "alias": "core"}))).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT", "{e:?}");
    // both supersede branches, neither, or adopted_as with part: refused, nothing written
    let end = |w: &World| w.st.engine.read().canon_head();
    let before = end(&w);
    let decision = json!({"statement": "Every job logs in UTC, always.", "title": "Log time"});
    for args in [
        json!({"old": w.old.to_string(), "adopted_as": "core:D-PSTRM", "decision": decision}),
        json!({"old": w.old.to_string()}),
        json!({"old": w.old.to_string(), "adopted_as": "core:D-PSTRM", "part": "every job"}),
        json!({"old": w.old.to_string(), "decision": decision, "brief": "bt-x"}),
    ] {
        let e = rdy_supersede(&mut w.st, p(args.clone())).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{args}: {e:?}");
    }
    assert_eq!(end(&w), before);
    let views = w.st.engine.read().views_with_changes();
    assert!(
        views.iter().all(|(_, n)| *n == 0),
        "nothing was written: {views:?}"
    );
    // the existing shape still works, and so does adopted_as alone
    let v = rdy_supersede(
        &mut w.st,
        p(json!({"old": w.old.to_string(), "adopted_as": "core:D-PSTRM"})),
    )
    .unwrap();
    assert!(v.is_object(), "{v}");
    let view = w.st.view.clone();
    let st = w.st.engine.read();
    let status = st.graph(&Scope::View(view)).unwrap().status(&w.old);
    assert_eq!(
        status,
        Some(DerivedStatus::Superseded {
            by: vec![id("D-PSTRM")]
        })
    );
}

/// TASK-2069 (D-Y49PX): `rdy_suspects` lists the suspect on the cited claim that the second
/// delivery changed, the target by its alias, as the CLI's `rdy suspects` does.
#[test]
fn fsus_rdy_suspects_lists_the_foreign_row() {
    let mut w = world();
    let v = rdy_suspects(&mut w.st, p(json!({"scope": "canon"}))).unwrap();
    let items = v["items"].as_array().unwrap();
    let rows: Vec<&Value> = items
        .iter()
        .filter(|i| i["target"] == "core:c-FRGNC")
        .collect();
    assert_eq!(rows.len(), 1, "{v}");
    assert_eq!(rows[0]["reason"], "changed", "{v}");
    let block = rows[0]["block"].as_str().unwrap();
    assert!(block.starts_with("c-"), "{v}");
    // the citing claim is the one canon holds under the section
    let st = w.st.engine.read();
    let m = st.materialized(&Scope::Canon).unwrap();
    let b = m.get(&id(block)).expect("the citing block");
    assert_eq!(b.parent.as_ref(), Some(&w.section), "{v}");
    // the store holds the cited ref in its stored form, the repo by its hex digits
    assert!(
        b.body.contains(&format!("{}:c-FRGNC", cited())),
        "{}",
        b.body
    );
}
