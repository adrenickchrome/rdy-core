//! pub5 (D-KDFZ3, D-KQ4CA): `rdy new profile` is a thin wrapper over the ordinary write. It
//! builds the same block `rdy_write` does for a chat, with nothing the write path does not know.
use rdy_engine::{Rdy, Scope};
use rdy_model::{Block, BlockId, FieldValue};
use rdy_qa::daemon::Served;
use rdy_qa::mcp::McpClient;
use rdy_schema::publication::{self, ProfileFields};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// The ids the two sides select and sit under, by the role they play.
struct Tree {
    guide: String,
    other: String,
    folder: String,
}

fn id_of(v: &Value, placeholder: &str) -> String {
    v["created"][placeholder]
        .as_str()
        .unwrap_or_else(|| panic!("{placeholder} in {v}"))
        .to_string()
}

/// The CLI side: the tree `guide`, `other` and `notes/`, in view `work`.
fn cli_tree(s: &Served) -> Tree {
    let (ok, g) = s.json(&["--json", "new", "doc", "guide"]);
    assert!(ok, "{g}");
    let (ok, o) = s.json(&["--json", "new", "doc", "other"]);
    assert!(ok, "{o}");
    let (ok, f) = s.json(&["--json", "new", "folder", "notes"]);
    assert!(ok, "{f}");
    let folder = f["created"]
        .as_object()
        .unwrap()
        .values()
        .filter_map(Value::as_str)
        .find(|id| id.starts_with("fd-"))
        .unwrap()
        .to_string();
    Tree {
        guide: id_of(&g, "$doc"),
        other: id_of(&o, "$doc"),
        folder,
    }
}

/// The chat side: the same tree, written by `rdy_write` in the agent's own view.
fn chat_tree(c: &mut McpClient) -> Tree {
    let w = c
        .call(
            "rdy_write",
            json!({"ops": [
                {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}},
                {"op": "create", "type": "doc", "id": "$o", "fields": {"name": "other", "title": "Other", "doc_type": "page"}},
                {"op": "create", "type": "folder", "id": "$f", "fields": {"name": "notes"}}
            ]}),
        )
        .ok();
    Tree {
        guide: id_of(&w, "$g"),
        other: id_of(&w, "$o"),
        folder: id_of(&w, "$f"),
    }
}

/// The block `id` in `view`, and its parent.
fn read_block(r: &Rdy, view: &str, id: &str) -> (Block, Block) {
    let st = r.read();
    let scope = Scope::View(view.into());
    let b = st.block(&scope, &BlockId::parse(id).unwrap()).unwrap();
    let parent = st.block(&scope, b.parent.as_ref().unwrap()).unwrap();
    (b, parent)
}

/// A profile block with generated ids replaced by the role they play, so a block made from the
/// command line and one made from chat compare equal: type, parent, every field but the key.
fn normalized(b: &Block, parent: &Block, tree: &Tree) -> Value {
    let roles: BTreeMap<&str, &str> = BTreeMap::from([
        (tree.guide.as_str(), "guide"),
        (tree.other.as_str(), "other"),
        (tree.folder.as_str(), "folder"),
    ]);
    let role = |id: &str| roles.get(id).copied().unwrap_or(id).to_string();
    let fields: BTreeMap<String, Value> = b
        .fields
        .iter()
        .filter(|(k, _)| k.as_str() != "key")
        .map(|(k, v)| {
            let v = match v {
                FieldValue::List(items) => {
                    json!(
                        items
                            .iter()
                            .filter_map(|i| i.as_str())
                            .map(role)
                            .collect::<Vec<_>>()
                    )
                }
                other => serde_json::to_value(other).unwrap(),
            };
            (k.clone(), v)
        })
        .collect();
    json!({
        "type": b.block_type,
        "parent": role(parent.id.as_str()),
        "parent_type": parent.block_type,
        "fields": fields,
        "body": b.body,
        "links": b.links.len(),
        "pins": b.pins.len(),
    })
}

fn parsed(b: &Block, parent: &Block) -> ProfileFields {
    let pack = rdy_schema::PackSet::with_builtin()
        .resolve("rdy-standard")
        .unwrap();
    publication::parse(&pack, b, Some(parent)).unwrap_or_else(|e| panic!("{e:?}"))
}

#[test]
fn pub5_cli_and_mcp_create_the_same_profile_shape() {
    let s = Served::new();
    let cli = cli_tree(&s);
    let mut c = McpClient::start_proxy(&s);
    let r = c.call("rdy_session", json!({"actor": "agent:p", "role": "writer"}));
    assert!(r.code().is_none(), "{:?}", r.ok());
    let chat = chat_tree(&mut c);

    // a public profile with the least: no trail, no description
    let (ok, a) = s.json(&[
        "--json",
        "new",
        "profile",
        "site",
        "--parent",
        &cli.guide,
        "--title",
        "Site",
        "--audience",
        "public",
        "--select",
        &cli.other,
        "--select",
        &cli.folder,
    ]);
    assert!(ok, "{a}");
    let a_id = id_of(&a, "$profile");
    assert_eq!(a["view"], "work");
    // an org profile with every optional field, its trail set against the default
    let out = s.rdy(&[
        "new",
        "profile",
        "handbook",
        "--parent",
        &cli.guide,
        "--title",
        "Handbook",
        "--audience",
        "org",
        "--select",
        &cli.other,
        "--description",
        "For the team",
        "--trail",
        "false",
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let line = text.lines().next().unwrap();
    let b_id = line
        .strip_prefix("created publication profile handbook (")
        .and_then(|r| r.strip_suffix(')'))
        .unwrap_or_else(|| panic!("{text:?}"))
        .to_string();
    assert!(b_id.starts_with("pf-"), "{text:?}");

    let w = c
        .call(
            "rdy_write",
            json!({"ops": [
                {"op": "create", "type": "publication_profile", "id": "$a", "parent": chat.guide,
                 "fields": {"key": "site-chat", "title": "Site", "audience": "public",
                            "select": [chat.other, chat.folder]}},
                {"op": "create", "type": "publication_profile", "id": "$b", "parent": chat.guide,
                 "fields": {"key": "handbook-chat", "title": "Handbook", "audience": "org",
                            "select": [chat.other], "description": "For the team", "trail": false}}
            ]}),
        )
        .ok();
    let (ca_id, cb_id) = (id_of(&w, "$a"), id_of(&w, "$b"));
    drop(c);
    s.stop();

    let r = Rdy::open(s.dir.path()).unwrap();
    let (ba, pa) = read_block(&r, "work", &a_id);
    let (bb, pb) = read_block(&r, "work", &b_id);
    let (ca, qa) = read_block(&r, "work-agent-p", &ca_id);
    let (cb, qb) = read_block(&r, "work-agent-p", &cb_id);
    // the same type, parent, fields and empty body and links
    assert_eq!(normalized(&ba, &pa, &cli), normalized(&ca, &qa, &chat));
    assert_eq!(normalized(&bb, &pb, &cli), normalized(&cb, &qb, &chat));
    assert_eq!(ba.block_type, "publication_profile");
    assert_eq!(pa.block_type, "doc");
    // the keys are the ones given
    assert_eq!(ba.field_str("key"), Some("site"));
    assert_eq!(bb.field_str("key"), Some("handbook"));
    assert_eq!(ca.field_str("key"), Some("site-chat"));
    // an absent trail is not stored; it reads as the audience's default
    assert!(!ba.fields.contains_key("trail") && !ca.fields.contains_key("trail"));
    assert!(!parsed(&ba, &pa).trail && !parsed(&ca, &qa).trail);
    assert_eq!(bb.fields.get("trail"), Some(&FieldValue::Bool(false)));
    assert!(!parsed(&bb, &pb).trail && !parsed(&cb, &qb).trail);
    assert_eq!(
        parsed(&bb, &pb).description.as_deref(),
        Some("For the team")
    );
    assert_eq!(parsed(&ba, &pa).select.len(), 2);
    r.close().unwrap();
}

#[test]
fn pub5_new_help_and_required_options() {
    let s = Served::new();
    let cli = cli_tree(&s);
    let help = String::from_utf8_lossy(&s.rdy(&["--help"]).stdout).into_owned();
    assert!(
        help.lines().any(|l| l.trim_start().starts_with("new")
            && l.contains("Create a folder, doc, or publication profile.")),
        "{help}"
    );
    // each required option is required; a refusal writes nothing
    let full = [
        "new",
        "profile",
        "site",
        "--parent",
        &cli.guide,
        "--title",
        "Site",
        "--audience",
        "public",
        "--select",
        &cli.other,
    ];
    for (drop_at, name) in [
        (3, "--parent"),
        (5, "--title"),
        (7, "--audience"),
        (9, "--select"),
    ] {
        let mut args: Vec<&str> = full.to_vec();
        args.drain(drop_at..drop_at + 2);
        let out = s.rdy(&args);
        assert!(!out.status.success(), "{name}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains(name), "{name}: {err}");
    }
    let (ok, v) = s.json(&["--json", "ls"]);
    assert!(ok);
    assert!(!v.to_string().contains("publication_profile"), "{v}");
}
