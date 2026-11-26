//! scc9 (D-47HVF, D-3RSCW, D-KQ4CA, D-VRH3Z, D-2VM48): `rdy_push` and `rdy_pull`, the chat path
//! of the sync client, run against a loopback TLS hub that speaks the sync protocol. Each test
//! drives the real binary: an MCP client of the project's daemon (`rdy mcp`, the proxy), with a
//! private user config home that trusts the hub's generated CA and holds a fresh sign-in.
#[allow(clippy::duplicate_mod)]
#[path = "../support/sync_hub.rs"]
mod sync_hub;

use rdy_model::Hash;
use rdy_qa::mcp::McpClient;
use serde_json::{Value, json};
use sync_hub::*;

fn hub_only() -> Hash {
    Hash::of(b"scc9 hub-only landing")
}

fn writes(k: &Kit) -> Vec<Req> {
    k.hub.of("/write")
}

fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
    k.sort();
    k
}

/// A chat client of `k` with a writer session.
fn writer(k: &Kit) -> McpClient {
    chat(k, Some("writer"))
}

/// scc9: every CLI act of push and pull has its chat form: the same acts give replies of the
/// same shape, and the chat push carries the capturing client's `rdy` in the sent inputs.
#[test]
fn scc9_chat_has_every_cli_act() {
    // push: an empty hub repo is seeded; the write carries this client's rdy
    let k = kit(&["a", "b"]);
    let mut c = writer(&k);
    let v = mcp_ok(&mut c, "rdy_push", json!({}));
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(k.hub.canon(), Some(k.proj.landings[1].to_string()));
    let sent = writes(&k)[0].json();
    assert_eq!(
        sent["render_inputs"]["rdy"],
        rdy_engine::formats::formats().rdy
    );
    let cli = kit(&["a", "b"]);
    let cv = cli_ok(&cli, &["push"]);
    assert_eq!(keys(&v), keys(&cv), "push: the chat and CLI replies match");
    assert_eq!(keys(&v["canon"]), keys(&cv["canon"]));
    // force that is not needed: no card, and the note is exact
    let k2 = kit(&["a", "b"]);
    k2.hub.set_canon(Some(k2.proj.landings[0]), None);
    let mut c2 = writer(&k2);
    let v = mcp_ok(&mut c2, "rdy_push", json!({"force": true}));
    assert_eq!(v["notes"], json!(["force not needed"]), "{v}");
    assert!(v.get("card").is_none(), "{v}");
    // force that moves hub canon: the card, then the answer
    let k3 = kit(&["a", "b"]);
    k3.hub.set_canon(Some(hub_only()), None);
    let mut c3 = writer(&k3);
    let card = mcp_ok(&mut c3, "rdy_push", json!({"force": true}));
    assert_eq!(card["status"], "confirmation_required", "{card}");
    let cli_card = cli_ok(&k3, &["push", "--force"]);
    assert_eq!(
        keys(&card),
        keys(&cli_card),
        "the card is the same on both paths"
    );
    // a restore takes its dump path
    let dump = k.proj.path().join("p.rdydump");
    let o = run(&k, &["dump", "--out", dump.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let k4 = kit(&["a"]);
    let dump4 = k4.proj.path().join("r.rdydump");
    let o = run(&k4, &["dump", "--out", dump4.to_str().unwrap()]);
    assert!(o.status.success());
    let mut c4 = writer(&k4);
    let v = mcp_ok(&mut c4, "rdy_push", json!({"restore": "r.rdydump"}));
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(writes(&k4)[0].json()["kind"], "restore");
    // a view the project lacks is the present refusal
    let e = mcp_err(&mut c, "rdy_push", json!({"views": ["nope"]}));
    assert_eq!(e["code"], "E_NOT_FOUND", "{e}");
    // pull: hub canon, take, strict, and the remote by its name
    let k5 = kit(&["a"]);
    let (_d, mut store) = k5.proj.copy(&k5.home);
    let l2 = sync_hub::land_doc(&mut store, "hub");
    k5.hub.serve(store.read());
    k5.hub.set_canon(Some(l2), Some(k5.proj.repo_id()));
    let mut c5 = writer(&k5);
    let v = mcp_ok(
        &mut c5,
        "rdy_pull",
        json!({"remote": "origin", "strict": true}),
    );
    assert_eq!(v["canon"]["status"], "pulled", "{v}");
    let v = mcp_ok(&mut c5, "rdy_pull", json!({"take": true}));
    assert!(
        v.get("card").is_none(),
        "a take that removes nothing needs no card: {v}"
    );
    let cv = cli_ok(&k5, &["pull", "--remote", "origin", "--strict"]);
    assert_eq!(keys(&v), keys(&cv), "pull: the chat and CLI replies match");
    drop(store);
}

/// scc9: the closed inputs. 256 view names pass the checks and 257 refuse; a repeated name,
/// `restore` with `views` or `force`, `take` with `strict` and no views, and relay-only fields
/// without `force`, `restore` or `take` are each `E_BAD_INPUT` with the ruled words; unknown keys
/// keep their suggestion.
#[test]
fn scc9_sync_inputs_refuse_unknown_keys_with_suggestions() {
    use rdy_mcp::tools::{PullInputs, PushInputs};
    let names = |n: usize| (0..n).map(|i| format!("view-{i:03}")).collect::<Vec<_>>();
    // 256 names pass, 257 do not
    let ok = PushInputs {
        views: names(256),
        ..PushInputs::default()
    };
    assert!(ok.check().is_ok());
    assert!(
        PullInputs {
            views: names(256),
            ..PullInputs::default()
        }
        .check()
        .is_ok()
    );
    let k = kit(&["a"]);
    let mut c = writer(&k);
    for tool in ["rdy_push", "rdy_pull"] {
        let e = mcp_err(&mut c, tool, json!({"views": names(257)}));
        assert_eq!(e["code"], "E_LIMIT", "{tool}: {e}");
        // 256 reach the engine, whose own refusal names the first view the project lacks
        let e = mcp_err(&mut c, tool, json!({"views": names(256)}));
        assert_ne!(e["code"], "E_LIMIT", "{tool}: {e}");
        // a name given twice
        let e = mcp_err(&mut c, tool, json!({"views": ["main", "main"]}));
        assert_eq!(e["code"], "E_BAD_INPUT", "{tool}: {e}");
        assert_eq!(e["message"], "view main is named twice");
        assert_eq!(e["fix"], "name each view once");
    }
    let refuse = |c: &mut McpClient, tool: &str, args: Value, message: &str, fix: &str| {
        let e = mcp_err(c, tool, args.clone());
        assert_eq!(e["code"], "E_BAD_INPUT", "{tool} {args}: {e}");
        assert_eq!(e["message"], message, "{tool} {args}");
        assert_eq!(e["fix"], fix, "{tool} {args}");
    };
    refuse(
        &mut c,
        "rdy_push",
        json!({"restore": "d.rdydump", "views": ["a"]}),
        "views is not used by push with restore",
        "remove views",
    );
    refuse(
        &mut c,
        "rdy_push",
        json!({"restore": "d.rdydump", "force": true}),
        "force is not used by push with restore",
        "remove force",
    );
    refuse(
        &mut c,
        "rdy_pull",
        json!({"take": true, "strict": true}),
        "strict is not used by pull with take and no views",
        "remove strict, or name the views to take",
    );
    // relay-only fields need force, restore or take
    for field in ["card_hash", "answer", "evidence", "question", "session_ref"] {
        let value = if field == "answer" {
            json!("confirm")
        } else {
            json!("x")
        };
        refuse(
            &mut c,
            "rdy_push",
            json!({field: value}),
            &format!("{field} is not used by push without force or restore"),
            &format!("remove {field}"),
        );
        refuse(
            &mut c,
            "rdy_pull",
            json!({field: value}),
            &format!("{field} is not used by pull without take"),
            &format!("remove {field}"),
        );
    }
    // an answer needs its card, and a question needs its words
    refuse(
        &mut c,
        "rdy_push",
        json!({"force": true, "answer": "confirm"}),
        "push needs card_hash",
        "pass card_hash",
    );
    refuse(
        &mut c,
        "rdy_pull",
        json!({"take": true, "card_hash": "h"}),
        "pull needs answer",
        "pass answer",
    );
    refuse(
        &mut c,
        "rdy_push",
        json!({"force": true, "card_hash": "h", "answer": "confirm", "question": "q"}),
        "push needs evidence",
        "pass evidence",
    );
    // unknown keys keep the present suggestion
    for (tool, args, key, want) in [
        ("rdy_push", json!({"remtoe": "origin"}), "remtoe", "remote"),
        ("rdy_push", json!({"forse": true}), "forse", "force"),
        ("rdy_pull", json!({"tkae": true}), "tkae", "take"),
        ("rdy_pull", json!({"strcit": true}), "strcit", "strict"),
    ] {
        let e = mcp_err(&mut c, tool, args);
        assert_eq!(e["code"], "E_BAD_INPUT");
        assert_eq!(
            e["message"],
            format!("{tool}: unknown parameter \"{key}\""),
            "{e}"
        );
        if !want.is_empty() {
            assert!(
                e["fix"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("did you mean \"{want}\"")),
                "{e}"
            );
        }
    }
}

/// scc9: push, ordinary pull and a view take need the writer role; a reader is refused before
/// anything is sent. An agent never answers a card as the person: with no relayed quote the relay
/// check refuses, and a session never claims a human actor.
#[test]
fn scc9_writer_and_reader_roles_are_explicit() {
    let k = kit(&["a"]);
    let mut r = chat(&k, Some("reader"));
    for (tool, args) in [
        ("rdy_push", json!({})),
        ("rdy_pull", json!({})),
        ("rdy_pull", json!({"take": true, "views": ["main"]})),
    ] {
        let e = mcp_err(&mut r, tool, args);
        assert_eq!(e["code"], "E_PERMISSION", "{tool}: {e}");
        assert_eq!(e["fix"], "start the session as a writer");
    }
    assert!(
        k.hub.requests().is_empty(),
        "a reader's call reaches no hub"
    );
    // a writer's calls pass the guard
    let mut w = writer(&k);
    assert_eq!(
        mcp_ok(&mut w, "rdy_push", json!({}))["canon"]["status"],
        "taken"
    );
    // an agent answers a card only by relaying a person's words
    let kf = kit(&["a"]);
    kf.hub.set_canon(Some(hub_only()), None);
    let mut c = writer(&kf);
    let card = mcp_ok(&mut c, "rdy_push", json!({"force": true}));
    let hash = card["card_hash"].as_str().unwrap();
    let e = mcp_err(
        &mut c,
        "rdy_push",
        json!({"force": true, "card_hash": hash, "answer": "confirm", "evidence": " "}),
    );
    assert_eq!(e["code"], "E_PERMISSION", "{e}");
    assert_eq!(
        e["message"],
        "an agent answers only by relaying a person's words"
    );
    assert!(writes(&kf).is_empty());
    // a session never claims a human actor
    let mut h = McpClient::start_proxy_capturing(kf.proj.path(), &kf.home.env());
    let r = h.call(
        "rdy_session",
        json!({"actor": "human:qa", "role": "writer"}),
    );
    assert!(
        !r.is_ok(),
        "an MCP session never claims a human actor: {r:?}"
    );
}

/// scc9: a pull that takes some views and refuses others lists each, with the tags taken and
/// kept and the canon replay's conflicts. The hub holds a view the project lacks and one whose
/// root is not the project's; the project has no sync state, so its local landing does not
/// replay.
#[test]
fn scc9_partial_pull_lists_views_tags_and_conflicts() {
    use rdy_qa::fixtures::{approver, write};
    let hub = Hub::start();
    let home = Home::new(&hub);
    home.signed_in(hub.origin());
    let mut proj = Proj::new(&["a"]);
    proj.remote(&home, &format!("{}{REMOTE_PATH}", hub.origin()));
    let (_d, mut store) = proj.copy(&home);
    // the project's own view `alpha`, and a local landing the hub's chain lacks
    {
        let mut r = rdy_engine::Rdy::open(proj.path()).unwrap();
        write(
            &mut r,
            "alpha",
            json!([{"op": "create", "type": "doc", "fields": {"name": "mine", "title": "mine", "doc_type": "page"}}]),
        )
        .unwrap();
        r.close().unwrap();
    }
    proj.land_more(&["local"]);
    // the hub's views `alpha` (another root) and `beta`, a tag, and one more landing
    for view in ["alpha", "beta"] {
        write(
            &mut store,
            view,
            json!([{"op": "create", "type": "doc", "fields": {"name": view, "title": view, "doc_type": "page"}}]),
        )
        .unwrap();
    }
    let l3 = sync_hub::land_doc(&mut store, "hub");
    store.tag(&approver(), "v1.0.0", None, None).unwrap();
    hub.serve(store.read());
    hub.set_canon(Some(l3), Some(proj.repo_id()));
    let k = Kit { hub, home, proj };
    let mut c = writer(&k);
    let v = mcp_ok(&mut c, "rdy_pull", json!({"views": ["alpha", "beta"]}));
    // canon: the replay's conflicts and the view that holds what did not replay
    assert_eq!(v["canon"]["status"], "pulled with conflicts", "{v}");
    assert!(
        v["canon"]["not_replayed"]
            .as_array()
            .is_some_and(|n| !n.is_empty()),
        "{v}"
    );
    assert!(v["canon"]["view"].is_string(), "{v}");
    // the tag the hub holds is taken
    assert_eq!(v["fetched"]["tags_taken"], json!(["v1.0.0"]), "{v}");
    assert_eq!(v["fetched"]["tags_kept"], json!([]));
    // each view is listed, in the order named: alpha refused, beta taken
    let views = v["views"].as_array().unwrap();
    assert_eq!(views.len(), 2, "{v}");
    assert_eq!(views[0]["view"], "alpha");
    assert_eq!(views[0]["status"], "refused", "{v}");
    assert!(views[0]["refusal"]["message"].is_string());
    assert_eq!(views[1]["view"], "beta");
    assert_eq!(views[1]["status"], "new", "{v}");
    assert_eq!(v["views_fetched"].as_array().unwrap().len(), 2, "{v}");
    drop(store);
}

/// scc9: a card for each canon move that removes landings, over MCP: a forced push, a restore
/// and a take. The card names the remote, the tips before and after and each landing that leaves
/// canon; the agent relays the person's quoted answer with the card's `card_hash`; a tip that
/// moved after the card changes the hash, and the answer is refused as any changed card is.
#[test]
fn scc9_force_restore_and_canon_take_require_current_person_card() {
    let relay = |card: &Value, base: Value| {
        let mut a = base;
        a["card_hash"] = card["card_hash"].clone();
        a["answer"] = json!("confirm");
        a["evidence"] = json!("yes, do it");
        a["question"] = json!("Move canon?");
        a["session_ref"] = json!("chat-9");
        a
    };
    // a forced push
    let k = kit(&["a", "b"]);
    let tip = hub_only();
    k.hub.set_canon(Some(tip), None);
    let mut c = writer(&k);
    let card = mcp_ok(&mut c, "rdy_push", json!({"force": true}));
    assert_eq!(card["status"], "confirmation_required", "{card}");
    assert_eq!(card["remote"], "origin");
    assert_eq!(card["before"], tip.to_string());
    assert_eq!(card["after"], k.proj.landings[1].to_string());
    assert!(card["card"].as_str().unwrap().contains(&tip.to_string()));
    assert!(writes(&k).is_empty(), "a card writes nothing");
    // the tip moves after the card: the old answer is refused with the new card
    let moved = Hash::of(b"scc9 moved");
    k.hub.set_canon(Some(moved), None);
    let e = mcp_err(&mut c, "rdy_push", relay(&card, json!({"force": true})));
    assert_eq!(e["code"], "E_ITEM_MOVED", "{e}");
    assert_eq!(e["corrected"]["before"], moved.to_string());
    assert_ne!(e["corrected"]["card_hash"], card["card_hash"]);
    assert!(writes(&k).is_empty());
    // the current card, relayed with the person's words, sends the force move
    let fresh = e["corrected"].clone();
    let v = mcp_ok(&mut c, "rdy_push", relay(&fresh, json!({"force": true})));
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(writes(&k).pop().unwrap().json()["kind"], "force");
    // a cancel is a decision, not a write
    let kc = kit(&["a"]);
    kc.hub.set_canon(Some(tip), None);
    let mut cc = writer(&kc);
    let card = mcp_ok(&mut cc, "rdy_push", json!({"force": true}));
    let mut cancel = relay(&card, json!({"force": true}));
    cancel["answer"] = json!("cancel");
    let v = mcp_ok(&mut cc, "rdy_push", cancel);
    assert_eq!(v["status"], "cancelled", "{v}");
    assert!(writes(&kc).is_empty());
    // a restore into a hub repo that holds canon
    let kr = kit(&["a"]);
    let dump = kr.proj.path().join("r.rdydump");
    let o = run(&kr, &["dump", "--out", dump.to_str().unwrap()]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    kr.hub.set_canon(Some(tip), None);
    let mut cr = writer(&kr);
    let card = mcp_ok(&mut cr, "rdy_push", json!({"restore": "r.rdydump"}));
    assert_eq!(card["status"], "confirmation_required", "{card}");
    assert_eq!(card["move"], "restore");
    assert_eq!(card["before"], tip.to_string());
    assert_eq!(card["after"], kr.proj.landings[0].to_string());
    let v = mcp_ok(
        &mut cr,
        "rdy_push",
        relay(&card, json!({"restore": "r.rdydump"})),
    );
    assert_eq!(v["canon"]["status"], "taken", "{v}");
    assert_eq!(writes(&kr).pop().unwrap().json()["kind"], "restore");
    // a take of hub canon
    let hub = Hub::start();
    let home = Home::new(&hub);
    home.signed_in(hub.origin());
    let mut proj = Proj::new(&["a"]);
    proj.remote(&home, &format!("{}{REMOTE_PATH}", hub.origin()));
    let (_d, mut store) = proj.copy(&home);
    proj.land_more(&["local"]);
    let l3 = sync_hub::land_doc(&mut store, "hub");
    hub.serve(store.read());
    hub.set_canon(Some(l3), Some(proj.repo_id()));
    let local_head = proj.landings[1];
    let kt = Kit { hub, home, proj };
    let mut ct = writer(&kt);
    let card = mcp_ok(&mut ct, "rdy_pull", json!({"take": true}));
    assert_eq!(card["status"], "confirmation_required", "{card}");
    assert_eq!(card["move"], "take");
    assert_eq!(card["before"], local_head.to_string());
    assert_eq!(card["after"], l3.to_string());
    assert!(
        card["leaving"]
            .as_array()
            .unwrap()
            .contains(&json!(local_head.to_string()))
    );
    let v = mcp_ok(&mut ct, "rdy_pull", relay(&card, json!({"take": true})));
    assert!(v.get("card").is_none(), "{v}");
    let status = cli_ok(&kt, &["status"]);
    assert_eq!(status["sync"][0]["applied"], l3.to_string(), "{status}");
    drop(store);
}

/// scc9: `force not needed` exactly and no card when no landing leaves canon; a view take needs
/// no canon-take card and leaves canon on the ordinary pull path.
#[test]
fn scc9_force_not_needed_and_view_take_keep_canon_intent() {
    let k = kit(&["a", "b"]);
    k.hub.set_canon(Some(k.proj.landings[0]), None);
    let mut c = writer(&k);
    let v = mcp_ok(&mut c, "rdy_push", json!({"force": true}));
    assert!(
        v.get("card").is_none() && v.get("card_hash").is_none(),
        "{v}"
    );
    assert_eq!(v["notes"], json!(["force not needed"]), "{v}");
    assert_eq!(v["force_not_needed"], true);
    assert_eq!(writes(&k).pop().unwrap().json()["kind"], "landings");
    // a view take: canon pulls as usual, the named view is taken, and no card comes first
    use rdy_qa::fixtures::write;
    let hub = Hub::start();
    let home = Home::new(&hub);
    home.signed_in(hub.origin());
    let proj = Proj::new(&["a"]);
    proj.remote(&home, &format!("{}{REMOTE_PATH}", hub.origin()));
    let (_d, mut store) = proj.copy(&home);
    write(
        &mut store,
        "main",
        json!([{"op": "create", "type": "doc", "fields": {"name": "m", "title": "m", "doc_type": "page"}}]),
    )
    .unwrap();
    let l2 = sync_hub::land_doc(&mut store, "hub");
    hub.serve(store.read());
    hub.set_canon(Some(l2), Some(proj.repo_id()));
    let kv = Kit { hub, home, proj };
    let mut cv = writer(&kv);
    let v = mcp_ok(
        &mut cv,
        "rdy_pull",
        json!({"take": true, "views": ["main"]}),
    );
    assert!(
        v.get("card").is_none() && v.get("card_hash").is_none(),
        "{v}"
    );
    assert_eq!(v["canon"]["status"], "pulled", "canon pulls as usual: {v}");
    assert_eq!(v["canon"]["canon"], l2.to_string());
    assert_eq!(v["views"][0]["view"], "main", "{v}");
    assert_eq!(v["views"][0]["status"], "new", "{v}");
    drop(store);
}

/// scc9: both sync tools are `rdy:publish` with the response `warn`, and `rdy_clone` stays in
/// `rdy:project-ops` (D-2VM48).
#[test]
fn scc9_sync_rulesets_are_publish_warn_and_clone_stays_project_ops() {
    let table = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../rdy-engine/kungfu-tools.json"),
    )
    .unwrap();
    for row in [
        r#"{"tool": "rdy_pull", "ruleset": "rdy:publish", "response": "warn"}"#,
        r#"{"tool": "rdy_push", "ruleset": "rdy:publish", "response": "warn"}"#,
        r#"{"tool": "rdy_clone", "ruleset": "rdy:project-ops", "response": "warn"}"#,
    ] {
        assert!(table.contains(row), "{row}");
    }
    let rows: Vec<Value> = serde_json::from_str(&table).unwrap();
    let of = |t: &str| rows.iter().find(|r| r["tool"] == t).cloned().unwrap();
    for t in ["rdy_pull", "rdy_push"] {
        assert_eq!(
            of(t),
            json!({"tool": t, "ruleset": "rdy:publish", "response": "warn"})
        );
        let compiled = rdy_engine::kungfu::TOOLS
            .iter()
            .find(|p| p.tool == t)
            .unwrap();
        assert_eq!(compiled.ruleset, Some("rdy:publish"));
    }
    assert_eq!(of("rdy_clone")["ruleset"], "rdy:project-ops");
}

// ---------------------------------------------------------------------------------------------
// rdy_clone {dir, url} (D-D5J18, D-7PYYS, D-47HVF)
// ---------------------------------------------------------------------------------------------

/// A parent directory for a clone, and the clone's absolute path in it (absent).
fn target() -> (tempfile::TempDir, std::path::PathBuf) {
    let parent = tempfile::tempdir().unwrap();
    let dir = parent.path().join("clone");
    (parent, dir)
}

fn entries(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

fn words(v: &Value) -> (String, String, String) {
    (
        v["code"].as_str().unwrap_or_default().to_string(),
        v["message"].as_str().unwrap_or_default().to_string(),
        v["fix"].as_str().unwrap_or_default().to_string(),
    )
}

/// `rdy_clone {dir, url}` by a reader.
fn clone_args(url: &str, dir: &std::path::Path) -> Value {
    json!({"dir": dir.to_str().unwrap(), "url": url})
}

/// A kit whose hub serves a copy of its project, and a reader's chat client.
fn served_kit(docs: &[&str]) -> (Kit, tempfile::TempDir, rdy_engine::Rdy, McpClient) {
    let k = kit(docs);
    let (copy, store) = serve_copy(&k, &[]);
    let c = chat(&k, Some("reader"));
    (k, copy, store, c)
}

/// scc9: `url` with `file` refuses with the field rule's words; `url` with `fork` gets D-47HVF's
/// refusal; with neither, the call clones this project, and a dump file still clones. A URL
/// clone by a reader session sends hub read steps only.
#[test]
fn scc9_clone_selectors_preserve_old_inputs() {
    let (k, _copy, _store, mut c) = served_kit(&["a"]);
    let url = hub_url(&k);
    // the daemon holds the project: its repo id is the one the hub names
    let repo_id = k.hub.shared.repo.lock().unwrap().repo_id.clone().unwrap();
    let (parent, dir) = target();
    let e = mcp_err(
        &mut c,
        "rdy_clone",
        json!({"dir": dir.to_str().unwrap(), "url": url, "file": "x.rdydump"}),
    );
    assert_eq!(
        words(&e),
        (
            "E_BAD_INPUT".into(),
            "file is not used by clone with url".into(),
            "remove file or url".into()
        )
    );
    let e = mcp_err(
        &mut c,
        "rdy_clone",
        json!({"dir": dir.to_str().unwrap(), "url": url, "fork": true}),
    );
    assert_eq!(
        words(&e),
        (
            "E_BAD_INPUT".into(),
            "a fork starts from a project or a dump, not from a hub URL".into(),
            "clone the hub repo, then run rdy clone --fork <that project> <dir>".into()
        )
    );
    assert!(k.hub.requests().is_empty());
    assert_eq!(entries(parent.path()), 0);
    // the default source: this project
    let (_p2, local) = target();
    let v = mcp_ok(&mut c, "rdy_clone", json!({"dir": local.to_str().unwrap()}));
    assert_eq!(v["repo_id"], repo_id, "{v}");
    assert!(v["origin"].is_object());
    // a dump file, and a fork of it
    let dump = k.proj.path().join("s.rdydump");
    mcp_ok(&mut c, "rdy_dump", json!({"out": dump.to_str().unwrap()}));
    let (_p3, from_file) = target();
    let v = mcp_ok(
        &mut c,
        "rdy_clone",
        json!({"dir": from_file.to_str().unwrap(), "file": "s.rdydump", "fork": true}),
    );
    assert_eq!(v["fork"], true, "{v}");
    assert_ne!(v["repo_id"], repo_id);
    // a URL clone by a reader: hub read steps only
    let v = mcp_ok(&mut c, "rdy_clone", clone_args(&url, &dir));
    assert_eq!(v["repo_id"], repo_id, "{v}");
    assert_eq!(v["remote"], "hub");
    let routes = k.hub.routes();
    assert!(
        routes.iter().all(|r| READ_ROUTES.contains(&r.as_str())),
        "{routes:?}"
    );
}

/// The settings reply of `base` with `member` removed; a reply with no pack names its reason.
fn without(base: &Value, member: &str) -> Value {
    let mut b = base.clone();
    b.as_object_mut().unwrap().remove(member);
    if member == "pack" {
        b["pack_reason"] = json!("none");
    }
    b
}

/// The bundle a clone writes in full, with a pack of its own named `team-pack`.
fn full_bundle(rdy: Option<&str>) -> Value {
    let mut pack = builtin_pack();
    pack["name"] = json!("team-pack");
    let mut b = pack_settings(pack, rdy);
    b["coverage_gate"] = json!("no_lower");
    b["coverage_target"] = json!(70);
    b["decisions_home"] = json!("decisions");
    b["propose"] = json!({"cadence": "task", "changes": 5});
    b["title"] = json!("Team docs");
    b["description"] = json!("The team's docs.");
    b
}

const MEMBERS: [&str; 7] = [
    "pack",
    "coverage_gate",
    "coverage_target",
    "decisions_home",
    "propose",
    "title",
    "description",
];

/// scc9: fixture v2's three settings cases, each absent member, the bounds, the edited-file
/// checks, the digest check, the hub/default report, the mismatch warning, the pack's
/// `tensions_across` exception, a readable bundle from a newer capturing rdy, and a later local
/// capture that names this rdy.
#[test]
fn scc9_clone_settings_bundle_bounds_defaults_and_pack_digest() {
    let (k, _copy, _store, mut c) = served_kit(&["a"]);
    let url = hub_url(&k);
    let clone_with = |c: &mut McpClient, body: Value| {
        k.hub.settings(body);
        let (parent, dir) = target();
        let v = mcp_ok(c, "rdy_clone", clone_args(&url, &dir));
        (v, parent, dir)
    };
    // step 5: every member null and no warning
    let (v, _p5, d5) = clone_with(&mut c, settings_step(5));
    assert_eq!(v["settings"]["hub"], json!([]), "{v}");
    assert_eq!(v["settings"]["defaults"], json!(MEMBERS));
    assert_eq!(v["warnings"], json!([]));
    let r = rdy_engine::Rdy::open(&d5).unwrap();
    assert_eq!(
        serde_json::to_value(r.read().pack()).unwrap(),
        builtin_pack()
    );
    r.close().unwrap();
    // step 12: the gate and title from the hub, the default pack, and the mismatch warning
    let (v, _p12, d12) = clone_with(&mut c, settings_step(12));
    assert_eq!(
        v["settings"]["hub"],
        json!(["coverage_gate", "title"]),
        "{v}"
    );
    assert_eq!(
        v["settings"]["defaults"],
        json!([
            "pack",
            "coverage_target",
            "decisions_home",
            "propose",
            "description"
        ])
    );
    assert_eq!(
        v["warnings"],
        json!([{
            "code": "W_UPSTREAM_PACK_MISMATCH",
            "message": format!("hub repo {url} holds gate settings for another pack, so this clone uses rdy's default pack"),
            "fix": "push the project to the hub repo again from a machine that has its pack, then clone again",
        }])
    );
    let cfg: Value =
        serde_yaml_ng::from_slice(&std::fs::read(d12.join(".rdy.yaml")).unwrap()).unwrap();
    assert_eq!(cfg["pack"], "rdy-standard");
    assert_eq!(cfg["name"], "Publish");
    assert_eq!(cfg["coverage"], json!({"gate": "off"}));
    // step 19: a pack in full named as the built-in pack. The test makes the pack differ from
    // this rdy's effective built-in pack, so its digest differs: the clone refuses in D-7PYYS's
    // words for its capturing rdy and writes no pack file under the built-in name
    let mut step19 = settings_step(19);
    assert_eq!(step19["pack"]["name"], "rdy-standard");
    step19["pack"] = other_engine_pack("rdy-standard");
    assert_ne!(step19["pack"], builtin_pack());
    k.hub.settings(step19.clone());
    let (p19, d19) = target();
    let e = mcp_err(&mut c, "rdy_clone", clone_args(&url, &d19));
    let (message, fix) = unfit_words("pack", &url, step19["rdy"].as_str());
    assert_eq!(words(&e), ("E_BAD_INPUT".into(), message, fix));
    assert_eq!(entries(p19.path()), 0);
    // step 19 with this rdy's built-in pack in full: the pack from the hub, no pack file, and
    // the effective pack is it
    step19["pack"] = builtin_pack();
    let (v, _p19, d19) = clone_with(&mut c, step19.clone());
    assert_eq!(
        v["settings"]["hub"],
        json!(["pack", "coverage_gate", "title"]),
        "{v}"
    );
    assert_eq!(v["warnings"], json!([]));
    let cfg: Value =
        serde_yaml_ng::from_slice(&std::fs::read(d19.join(".rdy.yaml")).unwrap()).unwrap();
    assert_eq!(cfg["pack"], "rdy-standard");
    assert_eq!(entries(&d19.join(".rdy/packs")), 0);
    let r = rdy_engine::Rdy::open(&d19).unwrap();
    assert_eq!(
        serde_json::to_value(r.read().pack()).unwrap(),
        step19["pack"]
    );
    r.close().unwrap();
    // each absent member stays unset: listed among the defaults, absent from .rdy.yaml
    let full = full_bundle(Some(&this_rdy()));
    for member in MEMBERS {
        let (v, _p, dir) = clone_with(&mut c, without(&full, member));
        let hub: Vec<&str> = MEMBERS.iter().copied().filter(|m| *m != member).collect();
        assert_eq!(v["settings"]["hub"], json!(hub), "{member}: {v}");
        assert_eq!(v["settings"]["defaults"], json!([member]), "{member}");
        let cfg: Value =
            serde_yaml_ng::from_slice(&std::fs::read(dir.join(".rdy.yaml")).unwrap()).unwrap();
        let key = match member {
            "pack" => None,
            "coverage_gate" => Some(cfg["coverage"]["gate"].clone()),
            "coverage_target" => Some(cfg["coverage"]["target"].clone()),
            "decisions_home" => Some(cfg["decisions"].clone()),
            "propose" => Some(cfg["propose"].clone()),
            "title" => Some(cfg["name"].clone()),
            _ => Some(cfg["description"].clone()),
        };
        match key {
            Some(k) => assert!(k.is_null(), "{member}: {cfg}"),
            None => assert_eq!(cfg["pack"], "rdy-standard", "{cfg}"),
        }
        // the written files pass the checks of an edited settings or pack file
        let r = rdy_engine::Rdy::open(&dir).unwrap();
        assert!(r.fsck().is_ok());
        r.close().unwrap();
    }
    // a readable bundle from a newer capturing rdy clones; its pack is the effective pack
    let newer = full_bundle(Some(&newer_rdy()));
    let (v, _pn, dn) = clone_with(&mut c, newer.clone());
    assert_eq!(v["warnings"], json!([]), "{v}");
    let r = rdy_engine::Rdy::open(&dn).unwrap();
    assert_eq!(
        serde_json::to_value(r.read().pack()).unwrap(),
        newer["pack"]
    );
    r.close().unwrap();
    // the pack's tensions_across exception: an unknown kind warns and is dropped
    let mut odd = full_bundle(Some(&this_rdy()));
    odd["pack"]["tensions_across"] = json!({"no_such_kind": "advisory"});
    let (v, _po, _do) = clone_with(&mut c, odd);
    assert_eq!(
        v["warnings"],
        json!([{
            "code": "W_CONFIG_UNKNOWN",
            "message": "pack team-pack: tensions_across has no kind \"no_such_kind\"; rdy ignores it",
            "fix": "use one of: duplicate_definition, value_disagreement, restatement_drift, deprecated_reference, premise_shift",
        }]),
        "{v}"
    );
    // the bounds: a reply over 16,777,216 bytes, and members over the render-input bounds
    let mut big = settings_step(5);
    big["description"] = json!("d".repeat(16_777_216));
    k.hub.settings(big);
    let (parent, dir) = target();
    let e = mcp_err(&mut c, "rdy_clone", clone_args(&url, &dir));
    assert_eq!(e["code"], "E_UPSTREAM_PROTOCOL", "{}", e["message"]);
    assert_eq!(entries(parent.path()), 0);
    let mut wide = full_bundle(Some(&this_rdy()));
    wide["pack"]["link_types"] = json!((0..200_001).map(|i| format!("l{i}")).collect::<Vec<_>>());
    let mut long = full_bundle(Some(&this_rdy()));
    long["title"] = json!("t".repeat(4097));
    for (body, member) in [(wide, "pack"), (long, "title")] {
        k.hub.settings(body);
        let e = mcp_err(&mut c, "rdy_clone", clone_args(&url, &dir));
        let (message, fix) = unfit_words(member, &url, Some(&this_rdy()));
        assert_eq!(words(&e), ("E_BAD_INPUT".into(), message, fix), "{member}");
        assert_eq!(entries(parent.path()), 0);
    }
    // the digest: a readable pack named as the built-in one, but not it
    let mut other = builtin_pack();
    other["version"] = json!("0.0.1-other");
    k.hub.settings(pack_settings(other, Some(&this_rdy())));
    let e = mcp_err(&mut c, "rdy_clone", clone_args(&url, &dir));
    let (message, fix) = unfit_words("pack", &url, Some(&this_rdy()));
    assert_eq!(words(&e), ("E_BAD_INPUT".into(), message, fix));
    assert_eq!(entries(parent.path()), 0);
    // a later local capture, from a push of the clone, names this client's rdy
    k.hub.settings(full_bundle(Some(&newer_rdy())));
    let (_pl, later) = target();
    mcp_ok(&mut c, "rdy_clone", clone_args(&url, &later));
    {
        let mut r = rdy_engine::Rdy::open(&later).unwrap();
        land_doc(&mut r, "later");
        r.close().unwrap();
    }
    let o = command(&later, &k.home, &["--json", "push"])
        .output()
        .unwrap();
    let (ok, v) = parsed(&o);
    let _ = command(&later, &k.home, &["serve", "--stop"]).output();
    assert!(ok, "{v}");
    let sent = writes(&k).pop().unwrap().json();
    assert_eq!(sent["render_inputs"]["rdy"], this_rdy(), "{sent}");
    assert_eq!(sent["render_inputs"]["pack"]["name"], "team-pack");
}

/// Runs each refusal case of `pick` through the chat path: D-7PYYS's words exactly, no fetch,
/// and the target as it was, absent or an existing empty directory.
fn refuse_each(pick: impl Fn(&str, &str, Option<&str>) -> bool) -> usize {
    let (k, _copy, _store, mut c) = served_kit(&["a"]);
    let url = hub_url(&k);
    let mut n = 0;
    for (label, body, member, captured) in refusal_cases() {
        if !pick(&label, member, captured.as_deref()) {
            continue;
        }
        n += 1;
        k.hub.settings(body);
        for existing in [false, true] {
            let (parent, dir) = target();
            if existing {
                std::fs::create_dir(&dir).unwrap();
            }
            let from = k.hub.requests().len();
            let e = mcp_err(&mut c, "rdy_clone", clone_args(&url, &dir));
            let (message, fix) = unfit_words(member, &url, captured.as_deref());
            assert_eq!(
                words(&e),
                ("E_BAD_INPUT".into(), message.clone(), fix.clone()),
                "{label}"
            );
            for s in SENTINELS {
                assert!(!message.contains(s) && !fix.contains(s), "{label}: {s}");
            }
            assert_eq!(
                &k.hub.routes()[from..],
                ["discovery", "state", "settings"],
                "{label}"
            );
            assert_eq!(dir.exists(), existing, "{label}");
            if existing {
                assert_eq!(entries(&dir), 0, "{label}");
            }
            assert_eq!(entries(parent.path()), usize::from(existing), "{label}");
        }
    }
    assert_eq!(fetches(&k), 0);
    n
}

/// scc9: an unreadable pack from a newer capturing rdy (an unknown key, an unreadable value,
/// both in a nested part too) refuses with the newer-version words.
#[test]
fn scc9_clone_unreadable_pack_from_newer_rdy_refuses() {
    let newer = newer_rdy();
    let n = refuse_each(|_, m, c| m == "pack" && c == Some(newer.as_str()));
    assert_eq!(n, 4);
}

/// scc9: an unreadable setting from an older capturing rdy, and from this rdy's own version,
/// refuses with the setting's name and the next-push words.
#[test]
fn scc9_clone_unreadable_setting_from_older_rdy_refuses() {
    let (older, this) = (older_rdy(), this_rdy());
    let n = refuse_each(|_, m, c| {
        m != "pack" && (c == Some(older.as_str()) || c == Some(this.as_str()))
    });
    assert_eq!(n, 8);
}

/// scc9: with no `rdy` member, an unreadable pack and an unreadable setting refuse with the
/// words that name no captured version.
#[test]
fn scc9_clone_unreadable_bundle_without_rdy_refuses() {
    let n = refuse_each(|_, _, c| c.is_none());
    assert_eq!(n, 8);
}

/// scc9: a readable pack whose digest is not the pack the clone would use refuses: with the
/// newer-version words when the bundle's rdy is newer, else with D-D5J18's refusal in the
/// words of the capturing version (older or this one) or of none. Two packs: one named as the
/// built-in pack but not it, and a pack of its own whose source form resolves to another pack
/// here (a pack as another engine would capture it, made by the test).
#[test]
fn scc9_clone_pack_digest_mismatch_from_newer_rdy_refuses() {
    let (k, _copy, _store, mut c) = served_kit(&["a"]);
    let url = hub_url(&k);
    let mut other = builtin_pack();
    other["version"] = json!("0.0.1-other");
    let foreign = other_engine_pack("team-pack");
    for pack in [other, foreign] {
        for captured in [Some(newer_rdy()), Some(older_rdy()), Some(this_rdy()), None] {
            k.hub
                .settings(pack_settings(pack.clone(), captured.as_deref()));
            let (parent, dir) = target();
            let e = mcp_err(&mut c, "rdy_clone", clone_args(&url, &dir));
            let (message, fix) = unfit_words("pack", &url, captured.as_deref());
            assert_eq!(
                words(&e),
                ("E_BAD_INPUT".into(), message, fix),
                "{}: {captured:?}",
                pack["name"]
            );
            assert_eq!(entries(parent.path()), 0);
        }
    }
    assert_eq!(fetches(&k), 0);
}

/// r59014: the chat path keeps each refusal of a clone that checks on the handle that applied
/// canon: a pack whose digest is not the clone's, a hub that leaves out the snapshot deltas, a
/// settings reply that the clone cannot read, and a user config with a share remote already. Each
/// leaves the destination, absent or empty, as it was.
#[test]
fn r59014_clone_keeps_bad_record_and_settings_refusals() {
    let (k, _copy, _store, mut c) = served_kit(&["a", "b"]);
    let url = hub_url(&k);
    let refused = |c: &mut McpClient, label: &str| {
        let mut first = None;
        for existing in [false, true] {
            let (parent, dir) = target();
            if existing {
                std::fs::create_dir(&dir).unwrap();
            }
            let before = tree(&dir);
            let e = mcp_err(c, "rdy_clone", clone_args(&url, &dir));
            assert_eq!(tree(&dir), before, "{label}");
            assert_eq!(entries(parent.path()), usize::from(existing), "{label}");
            first.get_or_insert(e);
        }
        first.unwrap()
    };
    // a pack whose digest is not the clone's
    let mut other = builtin_pack();
    other["version"] = json!("0.0.1-other");
    k.hub
        .settings(pack_settings(other, Some(this_rdy().as_str())));
    let e = refused(&mut c, "pack digest");
    let (message, fix) = unfit_words("pack", &url, Some(this_rdy().as_str()));
    assert_eq!(
        words(&e),
        ("E_BAD_INPUT".into(), message, fix),
        "pack digest"
    );
    // a settings member the clone cannot read
    let (label, body, member, captured) = refusal_cases().remove(0);
    k.hub.settings(body);
    let e = refused(&mut c, &label);
    let (message, fix) = unfit_words(member, &url, captured.as_deref());
    assert_eq!(words(&e), ("E_BAD_INPUT".into(), message, fix), "{label}");
    // a hub that leaves out the snapshot deltas
    k.hub.settings(settings_step(5));
    k.hub.get_omits(rdy_store::kinds::SNAPSHOT_DELTA);
    let e = refused(&mut c, "missing snapshot delta");
    assert_eq!(e["code"], "E_BAD_RECORD", "{e}");
    assert_eq!(
        e["fix"],
        "request this stream again; run rdy fsck at its source"
    );
    let message = e["message"].as_str().unwrap();
    assert!(
        message.starts_with("stream record ") && message.contains("unlisted dependency"),
        "{e}"
    );
    // a user config that already holds a share remote
    let (k2, _copy2, _store2, mut c2) = served_kit(&["a"]);
    let mut config = std::fs::read_to_string(k2.home.config()).unwrap();
    config.push_str(&format!(
        "remotes:\n  team:\n    url: {}\n    mode: share\n",
        hub_url(&k2)
    ));
    std::fs::write(k2.home.config(), &config).unwrap();
    let (parent, dir) = target();
    let e = mcp_err(&mut c2, "rdy_clone", clone_args(&hub_url(&k2), &dir));
    assert_eq!(
        words(&e),
        (
            "E_REMOTE_EXISTS".into(),
            rdy_engine::REMOTE_EXISTS_MESSAGE.into(),
            rdy_engine::REMOTE_EXISTS_FIX.into()
        )
    );
    assert_eq!(entries(parent.path()), 0);
}

/// scc9: no refusal quotes a sentinel key or value of an unreadable pack or setting, or the
/// parser's error, for a newer, older, equal or absent capturing rdy: each message and fix is
/// the template filled with the member, the URL and the versions alone.
#[test]
fn scc9_clone_settings_refusal_never_quotes_unknown_keys_or_values() {
    let (k, _copy, _store, mut c) = served_kit(&["a"]);
    let url = hub_url(&k);
    for (label, body, member, captured) in refusal_cases() {
        k.hub.settings(body);
        let (_parent, dir) = target();
        let e = mcp_err(&mut c, "rdy_clone", clone_args(&url, &dir));
        let all = e.to_string();
        for s in SENTINELS {
            assert!(!all.contains(s), "{label}: {s} in {all}");
        }
        for parser in [
            "unknown field",
            "unknown variant",
            "invalid type",
            "expected",
        ] {
            assert!(!all.contains(parser), "{label}: {parser} in {all}");
        }
        let (message, fix) = unfit_words(member, &url, captured.as_deref());
        assert_eq!(e["message"], message, "{label}");
        assert_eq!(e["fix"], fix, "{label}");
    }
}

/// D-H67EN: a pull whose lookup of the held push the hub refuses keeps `held`, goes on and warns
/// `W_UPSTREAM_HELD_LOOKUP` with the hub's code and fix.
fn not_found_body(push: &str) -> String {
    json!({"code": "E_HUB_NOT_FOUND", "message": "No push of that id.",
        "fix": format!("check the push id, and sign in with the account that opened push {push}"),
        "issues": [], "request_id": "req_00000000000000000000000009"})
    .to_string()
}

#[test]
fn scc8_held_lookup_refusal_warns_and_the_pull_goes_on() {
    let k = kit(&["a", "b"]);
    let mut c = writer(&k);
    k.hub.fault(Fault::HoldWrite(4));
    let v = mcp_ok(&mut c, "rdy_push", json!({}));
    assert_eq!(v["canon"]["status"], "held", "{v}");
    let push = v["canon"]["push"].as_str().unwrap().to_string();
    k.hub
        .fault(Fault::RefuseOutcome(404, not_found_body(&push)));
    let v = mcp_ok(&mut c, "rdy_pull", json!({}));
    assert_eq!(
        v["warnings"],
        json!([{
            "code": "W_UPSTREAM_HELD_LOOKUP",
            "message": format!("the outcome of held push {push} could not be read: E_HUB_NOT_FOUND"),
            "fix": format!("check the push id, and sign in with the account that opened push {push}"),
        }]),
        "{v}"
    );
    // held stays: the next push sends no write and reports the open request
    k.hub.fault(Fault::None);
    let before = writes(&k).len();
    let v = mcp_ok(&mut c, "rdy_push", json!({}));
    assert_eq!(v["canon"]["status"], "held_open", "{v}");
    assert_eq!(v["canon"]["request"], 4, "{v}");
    assert_eq!(writes(&k).len(), before);
}

// ---------------------------------------------------------------------------------------------
// The fetch of a hub dump (D-7CDEQ)
// ---------------------------------------------------------------------------------------------

const DUMP_ID: &str = "dmp_01J9";
const DUMP_NEXT: &str = "rdy push --restore <file>, or rdy load <file> <dir> for a copy";

/// A kit whose hub serves a dump of the kit's own project as [`DUMP_ID`], with the project's
/// repo id in its state step; and that dump's `rdy dump` reply.
fn dump_kit() -> (Kit, Value) {
    let k = kit(&["a", "b"]);
    let src = k.proj.path().join("src.rdydump");
    let made = cli_ok(&k, &["dump", "--out", src.to_str().unwrap()]);
    k.hub.dump(DUMP_ID, std::fs::read(&src).unwrap());
    std::fs::remove_file(&src).unwrap();
    // the daemon holds the store: the repo id comes from the dump's reply
    k.hub
        .set_canon(None, made["repo_id"].as_str().map(str::to_string));
    (k, made)
}

/// scc9: `rdy_pull {dump, out, remote?, url?}` is closed. With `dump`, every other pull input is
/// refused, a false bool, an empty list and each relay field included; `out` or `url` without
/// `dump`, `dump` without `out`, and `remote` with `url` get D-7CDEQ's exact words; an id of 0 or
/// 257 bytes, or with a byte outside printable ASCII, is refused; an unknown key keeps the
/// present suggestion. Nothing reaches the hub.
#[test]
fn scc9_dump_inputs_and_cli_refusals_are_exact() {
    let (k, _) = dump_kit();
    let url = hub_url(&k);
    let mut c = chat(&k, Some("reader"));
    let only = (
        "a dump fetch takes only out and remote or url",
        "run the dump fetch on its own",
    );
    let base = json!({"dump": DUMP_ID, "out": "f.rdydump"});
    let with = |k: &str, v: Value| {
        let mut a = base.clone();
        a[k] = v;
        a
    };
    let mut cases = vec![
        (
            json!({"out": "f"}),
            ("out goes only with dump", "add --dump <id>, or remove out"),
        ),
        (
            json!({"url": url}),
            ("url goes only with dump", "add --dump <id>, or remove url"),
        ),
        (
            json!({"dump": DUMP_ID}),
            ("a dump fetch needs out", "add --out <file>"),
        ),
        (
            with("url", json!(url))
                .as_object()
                .cloned()
                .map(|mut m| {
                    m.insert("remote".into(), json!("origin"));
                    Value::Object(m)
                })
                .unwrap(),
            ("name remote or url, not both", "keep one of them"),
        ),
    ];
    for (key, v) in [
        ("take", json!(false)),
        ("strict", json!(false)),
        ("views", json!([])),
        ("card_hash", json!("x")),
        ("answer", json!("confirm")),
        ("evidence", json!("yes")),
        ("question", json!("q")),
        ("session_ref", json!("s")),
    ] {
        cases.push((with(key, v), only));
    }
    let id = (
        "the dump id is not 1 to 256 bytes of printable ASCII",
        "pass the id that the hub's dump job gave",
    );
    for bad in [
        String::new(),
        "x".repeat(257),
        "d\u{e9}".into(),
        "a\nb".into(),
    ] {
        cases.push((with("dump", json!(bad)), id));
    }
    for (args, (message, fix)) in cases {
        let e = mcp_err(&mut c, "rdy_pull", args.clone());
        assert_eq!(
            (&e["code"], &e["message"], &e["fix"]),
            (&json!("E_BAD_INPUT"), &json!(message), &json!(fix)),
            "{args}"
        );
    }
    let e = mcp_err(&mut c, "rdy_pull", with("dumps", json!("x")));
    assert_eq!(e["code"], "E_BAD_INPUT", "{e}");
    assert!(e.to_string().contains("dump"), "a suggestion: {e}");
    assert!(k.hub.requests().is_empty(), "no refusal reaches the hub");
    // 1 and 256 bytes pass the checks: the hub refuses ids it never made
    for id in ["x".to_string(), "x".repeat(256)] {
        let e = mcp_err(&mut c, "rdy_pull", with("dump", json!(id)));
        assert_eq!(e["code"], "E_HUB_NOT_FOUND", "{e}");
    }
}

/// scc9: a reader session fetches by the default remote and by `url`: the fetch needs no project
/// role, runs discovery, state and dump with the machine's sign-in, and moves no store or sync
/// state. An ordinary pull still needs the writer role.
#[test]
fn scc9_dump_remote_and_url_use_sign_in_without_project_role() {
    let (k, made) = dump_kit();
    let mut c = chat(&k, Some("reader"));
    let before = cli_ok(&k, &["status"]);
    for (out, extra) in [
        ("r.rdydump", json!({})),
        ("u.rdydump", json!({"url": hub_url(&k)})),
    ] {
        let mut args = json!({"dump": DUMP_ID, "out": out});
        if let Some(u) = extra.get("url") {
            args["url"] = u.clone();
        }
        let n = k.hub.requests().len();
        let v = mcp_ok(&mut c, "rdy_pull", args);
        assert_eq!(v["digest"], made["digest"], "{v}");
        assert_eq!(k.hub.routes()[n..], ["discovery", "state", "dump"]);
        assert!(k.proj.path().join(out).is_file());
        for r in &k.hub.requests()[n..] {
            assert!(
                r.headers
                    .iter()
                    .any(|(h, v)| h == "authorization" && v == &format!("Bearer {ACCESS}")),
                "the machine's sign-in"
            );
        }
    }
    assert_eq!(before["sync"], cli_ok(&k, &["status"])["sync"]);
    let e = mcp_err(&mut c, "rdy_pull", json!({}));
    assert_eq!(e["code"], "E_PERMISSION", "{e}");
}

/// scc9: the chat reply names the checked file as `rdy dump` names it (size, dump format, kind,
/// repo id, trailer digest), the file under the project root, and the next step exactly.
#[test]
fn scc9_dump_reply_names_checked_file_and_next() {
    let (k, made) = dump_kit();
    let mut c = writer(&k);
    let v = mcp_ok(
        &mut c,
        "rdy_pull",
        json!({"dump": DUMP_ID, "out": "fetched.rdydump"}),
    );
    for m in ["bytes", "dump_format", "kind", "repo_id", "digest"] {
        assert_eq!(v[m], made[m], "{m}: {v}");
    }
    let out = k.proj.path().join("fetched.rdydump");
    assert_eq!(v["out"], out.display().to_string());
    assert_eq!(v["next"], DUMP_NEXT);
    assert_eq!(
        std::fs::metadata(&out).unwrap().len(),
        made["bytes"].as_u64().unwrap()
    );
}

/// The nightly tier (`cargo qa --tier nightly`); the release tier does not build these tests.
#[cfg(feature = "nightly-tests")]
mod nightly {
    use super::*;

    /// scc9: every settings refusal follows discovery, state and the `min_rdy` check, sends no
    /// fetch, and leaves the target as it was; the `min_rdy` stop comes before the settings read.
    #[test]
    fn scc9_clone_settings_refusal_fetches_nothing_and_leaves_dir() {
        let n = refuse_each(|_, _, _| true);
        assert_eq!(n, 32);
        let (k, _copy, _store, mut c) = served_kit(&["a"]);
        k.hub.min_rdy("99.0.0");
        let (parent, dir) = target();
        let e = mcp_err(&mut c, "rdy_clone", clone_args(&hub_url(&k), &dir));
        assert_eq!(e["code"], "E_STORE_FORMAT", "{e}");
        assert_eq!(k.hub.routes(), ["discovery", "state"]);
        assert_eq!(entries(parent.path()), 0);
    }
}
