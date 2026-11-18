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
        ("rdy_pull", json!({"dump": "x"}), "dump", ""),
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
