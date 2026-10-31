//! nb9 (D-YXBXH, t-F3KR2): a write that creates blocks returns `briefing: {token, covers}`. The
//! token covers the created blocks and records what they depend on; a follow-up write on a
//! created block cites it with no new `rdy_brief`. It is registered only after the write is
//! published, and a write past its bounds is refused before anything is appended.
use rdy_engine::accept::AcceptItem;
use rdy_engine::brief::{BriefRecord, CreatedBriefing, MAX_CREATED_BRIEFING, W_BRIEF_STALE};
use rdy_engine::engine::{Engine, Local};
use rdy_engine::*;
use rdy_model::BlockId;
use rdy_qa::daemon::Served;
use rdy_qa::edit_loop::*;
use rdy_qa::fixtures::*;
use rdy_qa::mcp::McpClient;
use rdy_qa::wc;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn view(v: &str) -> Scope {
    Scope::View(v.into())
}

fn ids_of(w: &WriteResult) -> BTreeSet<BlockId> {
    w.created
        .values()
        .map(|v| BlockId::parse(v).unwrap())
        .collect()
}

fn held(r: &Rdy, b: &CreatedBriefing) -> std::sync::Arc<BriefRecord> {
    r.briefs()
        .get(&b.token)
        .expect("the receipt's token is registered")
}

fn is_token(t: &str) -> bool {
    t.len() == 64 && t.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[test]
fn nb9_created_token_matches_the_full_brief_record() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let w = write(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "claim", "id": "$n", "parent": s.section.to_string(),
             "body": format!("New leans on {{{{{}}}}}.", s.a)},
            {"op": "create", "type": "claim", "parent": s.section.to_string(), "body": "Unnamed one."},
            {"op": "create", "type": "claim", "parent": s.section.to_string(), "body": "Unnamed two."}
        ]),
    )
    .unwrap();
    let receipt = w
        .briefing
        .clone()
        .expect("a write that creates returns a briefing");
    let ids = ids_of(&w);
    assert_eq!(receipt.covers, 3);
    assert_eq!(ids.len(), 3);
    let full = r.read().brief_ids(&view("w"), &ids, None).unwrap();
    assert_eq!(receipt.token, full.base.token);
    assert_eq!(full.covers, receipt.covers);
    // the presentation differs (the full briefing lists the section neighbors), the record not
    assert!(!full.neighbors.is_empty());
    let rec = held(&r, &receipt);
    assert_eq!(*rec, *full.record);
    assert_eq!(rec.covers, ids);
    assert!(
        rec.recorded.contains_key(&s.a),
        "the dependency is recorded"
    );
    assert!(!rec.covers(&s.a), "and never covered");
    let v = serde_json::to_value(&w).unwrap();
    assert_eq!(
        v["briefing"],
        json!({"token": receipt.token, "covers": 3}),
        "{v}"
    );
}

#[test]
fn nb9_followup_write_uses_receipt_without_a_brief_call() {
    let (_d, mut r) = project();
    let s = stable_spec(&mut r);
    let g = write(&mut r, "w", json!([
        {"op": "create", "type": "doc", "id": "$g", "fields": {"name": "glossary", "title": "Glossary", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$gs", "parent": "$g", "fields": {"title": "Terms"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$gs", "fields": {"name": "Ledger"}}
    ]))
    .unwrap();
    let entity = g.created["$e"].clone();
    // the parent's valid token covers the create under the stable section
    let ts = brief_of(&r, &view("w"), &[&s.section]).base.token;
    let e = Local::new(r);
    let section = s.section.to_string();
    let v = rdy_engine::consequence::write_and_report(&e, "w", None, move |r| {
        write_cited(
            r,
            "w",
            json!([{"op": "create", "type": "claim", "id": "$c", "parent": section,
                    "body": "The Ledger keeps every entry."}]),
            Some(&ts),
        )
    })
    .unwrap();
    let claim = v["created"]["$c"].as_str().unwrap().to_string();
    assert_eq!(v["briefing"]["covers"], 1, "{v}");
    let token = v["briefing"]["token"].as_str().unwrap().to_string();
    let suggestion = v["suggestions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["kind"] == "link" && x["block"] == claim && x["target"] == entity)
        .unwrap_or_else(|| panic!("a describes suggestion: {v}"))
        .clone();
    let item: AcceptItem = serde_json::from_value(suggestion).unwrap();
    let briefs = e.briefs();
    let before = (briefs.len(), briefs.entries());
    let a = e
        .write(Box::new(move |r| {
            let a = r.accept(&writer(), "w", vec![item], None, Some(token), None)?;
            Ok(serde_json::to_value(a).unwrap())
        }))
        .unwrap();
    assert_eq!(a["accepted"]["links"], 1, "{a}");
    assert!(
        a.get("briefing").is_none(),
        "an update-only write returns none: {a}"
    );
    assert_eq!(
        (briefs.len(), briefs.entries()),
        before,
        "no briefing was made for the follow-up"
    );
    let b = e
        .read()
        .block(&view("w"), &BlockId::parse(&claim).unwrap())
        .unwrap();
    assert_eq!(
        b.links["describes"][0].target.to_string(),
        entity,
        "the accepted link landed on the stable claim"
    );
    e.close().unwrap();
}

#[test]
fn nb9_receipt_covers_creates_not_dependencies_or_updates() {
    let (d, mut r) = project();
    let s = stable_spec(&mut r);
    // a draft doc with D, which a created block will depend on
    let dw = write(&mut r, "w", json!([
        {"op": "create", "type": "doc", "id": "$dd", "fields": {"name": "deps", "title": "Deps", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$ds", "parent": "$dd", "fields": {"title": "D"}},
        {"op": "create", "type": "claim", "id": "$dep", "parent": "$ds", "body": "D holds."}
    ]))
    .unwrap();
    let dep = dw.created["$dep"].clone();
    let ts = brief_of(&r, &view("w"), &[&s.section, &s.c]).base.token;
    // a mixed batch: a named create, an unnamed one, a create deleted again, an update
    let w = write_cited(
        &mut r,
        "w",
        json!([
            {"op": "create", "type": "claim", "id": "$a", "parent": s.section.to_string(),
             "body": format!("A new claim leans on {{{{{dep}}}}}.")},
            {"op": "create", "type": "claim", "parent": s.section.to_string(), "body": "Unnamed."},
            {"op": "create", "type": "claim", "id": "$x", "parent": s.section.to_string(), "body": "Gone again."},
            {"op": "delete", "id": "$x"},
            {"op": "update", "id": s.c.to_string(), "body": "C stands alone, edited."}
        ]),
        Some(&ts),
    )
    .unwrap();
    let receipt = w.briefing.clone().unwrap();
    assert_eq!(receipt.covers, 2, "the surviving creates only");
    let rec = held(&r, &receipt);
    let a = BlockId::parse(&w.created["$a"]).unwrap();
    let unnamed = BlockId::parse(&w.created["op1"]).unwrap();
    assert_eq!(rec.covers, BTreeSet::from([a.clone(), unnamed]));
    assert!(!rec.covers(&s.c), "an update is not covered");
    let dep_id = BlockId::parse(&dep).unwrap();
    assert!(rec.recorded.contains_key(&dep_id) && !rec.covers(&dep_id));

    // a write with no create returns no briefing
    let ts = brief_of(&r, &view("w"), &[&s.c]).base.token;
    let none = write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": s.c.to_string(), "body": "C, once more."}]),
        Some(&ts),
    )
    .unwrap();
    assert!(none.briefing.is_none());
    assert!(
        serde_json::to_value(&none)
            .unwrap()
            .get("briefing")
            .is_none()
    );

    // a direct write to a dependency needs its own coverage: make D stable, then cite the receipt
    let td = brief_of(&r, &view("w"), &[&dep_id]).base.token;
    write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": dep, "fields": {"lifecycle": "stable"}}]),
        Some(&td),
    )
    .unwrap();
    let e = write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": dep, "body": "D holds, edited."}]),
        Some(&receipt.token),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BRIEF_REQUIRED, "{e}");
    // the dependency changed (its lifecycle): the receipt is stale for the created block
    let e = write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": a.to_string(), "body": "A new claim, edited."}]),
        Some(&receipt.token),
    )
    .unwrap_err();
    assert_eq!(e.code, E_BRIEF_STALE, "{e}");
    assert!(e.message.contains(&dep), "{e}");

    // an import covers the blocks of every author change, in one token
    let out = d.path().join("export");
    r.read().export(&view("w"), &out).unwrap();
    let (_d2, mut r2) = project();
    let imported = r2.import(&writer(), "work", &out).unwrap();
    let receipt = imported.briefing.clone().unwrap();
    let blocks: BTreeSet<BlockId> = r2
        .read()
        .materialized(&view("work"))
        .unwrap()
        .ids()
        .cloned()
        .collect();
    assert_eq!(receipt.covers, blocks.len());
    assert_eq!(held(&r2, &receipt).covers, blocks);
    let full = r2
        .read()
        .brief_ids(&view("work"), &blocks, Some(1))
        .unwrap();
    assert_eq!(full.base.token, receipt.token);
}

fn create_one(r: &mut Rdy, parent: &BlockId) -> Result<WriteResult, RdyError> {
    write(
        r,
        "w",
        json!([{"op": "create", "type": "claim", "parent": parent.to_string(), "body": "Created."}]),
    )
}

#[test]
fn nb9_receipt_is_published_atomically_and_forgotten_on_restart() {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    let children = |r: &Rdy| {
        r.read()
            .materialized(&view("w"))
            .unwrap()
            .children(Some(&s.section))
            .len()
    };
    let n0 = children(&r);
    let reg = |r: &Rdy| (r.briefs().len(), r.briefs().entries());
    let before = reg(&r);
    // a torn append: nothing is published, nothing registered
    r.store_mut().fail_next_append_after(10);
    assert!(create_one(&mut r, &s.section).is_err());
    assert_eq!(reg(&r), before, "no usable token after a failed append");
    assert_eq!(children(&r), n0);
    drop(r);

    // a failed seal: the publication fails, nothing is registered
    let mut r = Rdy::open(d.path()).unwrap();
    let before = reg(&r);
    r.store_mut().fail_next_sync();
    assert!(create_one(&mut r, &s.section).is_err());
    assert_eq!(reg(&r), before, "no usable token after a failed seal");
    drop(r);

    // a successful write: the token is registered when the reply comes back
    let mut r = Rdy::open(d.path()).unwrap();
    let w = create_one(&mut r, &s.section).unwrap();
    let receipt = w.briefing.clone().unwrap();
    assert!(r.briefs().get(&receipt.token).is_some());
    let created = w.created["op0"].clone();
    r.close().unwrap();

    // a restart forgets it: a draft write citing it warns as for any unknown briefing
    let mut r = Rdy::open(d.path()).unwrap();
    assert!(r.briefs().get(&receipt.token).is_none());
    let w = write_cited(
        &mut r,
        "w",
        json!([{"op": "update", "id": created, "body": "Created, edited."}]),
        Some(&receipt.token),
    )
    .unwrap();
    let warn = w
        .warnings
        .iter()
        .find(|x| x.code == W_BRIEF_STALE)
        .expect("the unknown receipt warns");
    assert_eq!(
        warn.message,
        format!(
            "the cited briefing {} is unknown; this write touches only drafts, so it went ahead",
            &receipt.token[..8]
        )
    );
}

/// The state the bounds test compares: the log's end, the view and the registry.
fn state(r: &Rdy) -> (rdy_store::segment::Location, Hash, usize, usize) {
    (
        r.store().log_end(),
        r.read().materialized(&view("w")).unwrap().snapshot_id(),
        r.briefs().len(),
        r.briefs().entries(),
    )
}

use rdy_model::Hash;

/// `n` claims under `parent`; the first `on_e` describe `e` and the first `on_f` describe `f`.
fn claims(parent: &str, n: usize, e: &str, on_e: usize, f: &str, on_f: usize) -> Value {
    Value::Array(
        (0..n)
            .map(|i| {
                let mut targets = vec![];
                if i < on_e {
                    targets.push(json!({"target": e}));
                }
                if i < on_f {
                    targets.push(json!({"target": f}));
                }
                let mut op = json!({"op": "create", "type": "claim", "parent": parent, "body": format!("C{i}.")});
                if !targets.is_empty() {
                    op["links"] = json!({"describes": targets});
                }
                op
            })
            .collect(),
    )
}

#[test]
fn nb9_created_briefing_bounds_fail_before_append() {
    assert_eq!(MAX_CREATED_BRIEFING, 100_000);
    assert_eq!(rdy_engine::brief::BRIEF_REGISTRY_ENTRIES, 1_000_000);
    let (_d, mut r) = project();
    // a draft section; entity E, which eight decisions affect, and entity F, which one affects
    let w = write(&mut r, "w", json!([
        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "big", "title": "Big", "doc_type": "overview"}},
        {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "S"}},
        {"op": "create", "type": "entity", "id": "$e", "parent": "$s", "fields": {"name": "Ledger"}},
        {"op": "create", "type": "entity", "id": "$f", "parent": "$s", "fields": {"name": "Queue"}}
    ]))
    .unwrap();
    let (section, e, f) = (
        w.created["$s"].clone(),
        w.created["$e"].clone(),
        w.created["$f"].clone(),
    );
    for i in 0..9 {
        r.decide(
            &writer(),
            "w",
            DecisionInput {
                parent: section.clone(),
                statement: format!("Rule {i} holds"),
                affects: vec![if i < 8 { e.clone() } else { f.clone() }],
                ..Default::default()
            },
        )
        .unwrap();
    }
    // 100,000 creates: covers 100,000; recorded 100,000 + E, F and nine decisions; routes
    // 8 x 99,998 + 5: exactly 1,000,000 entries. One more route is one past.
    let past_entries = claims(&section, 100_000, &e, 99_998, &f, 6);
    let before = state(&r);
    let err = write(&mut r, "w", past_entries).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            E_LIMIT,
            "briefing exceeds 1000000 entries",
            "brief fewer blocks, or split the write into smaller changes"
        )
    );
    assert_eq!(state(&r), before, "refused before the append");

    let past_creates = claims(&section, 100_001, &e, 0, &f, 0);
    let err = write(&mut r, "w", past_creates).unwrap_err();
    assert_eq!(
        (err.code, err.message.as_str(), err.fix.as_str()),
        (
            E_LIMIT,
            "write briefing exceeds 100000 created blocks",
            "split the write into smaller changes"
        )
    );
    assert_eq!(state(&r), before, "refused before the append");

    let at = claims(&section, 100_000, &e, 99_998, &f, 5);
    let w = write(&mut r, "w", at).unwrap();
    let receipt = w.briefing.clone().unwrap();
    assert_eq!(receipt.covers, 100_000);
    assert_ne!(r.store().log_end(), before.0);
    // the registry keeps the briefing inserted last, at exactly the entry budget
    assert_eq!((r.briefs().len(), r.briefs().entries()), (1, 1_000_000));
    assert!(r.briefs().get(&receipt.token).is_some());
}

/// A served project with [`stable_spec`] in canon.
fn served_stable() -> (Served, Spec) {
    let s = Served::new();
    let mut r = Rdy::open(s.dir.path()).unwrap();
    let spec = stable_spec(&mut r);
    r.close().unwrap();
    (s, spec)
}

/// Through `c`: a create under the stable section citing its briefing returns a receipt, a
/// follow-up write on the created claim citing the receipt goes through, and a decide under the
/// section returns one too. Every reply keeps on_change.
fn mcp_receipts(c: &mut McpClient, sp: &Spec, run: &str) {
    let reply = c.call(
        "rdy_session",
        json!({"actor": format!("agent:{run}"), "run": run, "role": "writer"}),
    );
    assert!(reply.code().is_none());
    let b = c
        .call("rdy_brief", json!({"ids": [sp.section.to_string()]}))
        .ok();
    let ts = b["base"]["token"].as_str().unwrap().to_string();
    let w = c
        .call(
            "rdy_write",
            json!({"brief": ts, "ops": [{"op": "create", "type": "claim", "id": "$n", "parent": sp.section.to_string(), "body": "New and stable."}]}),
        )
        .ok();
    assert!(w["on_change"].is_object(), "{w}");
    assert_eq!(w["briefing"]["covers"], 1, "{w}");
    let token = w["briefing"]["token"].as_str().unwrap().to_string();
    assert!(is_token(&token), "{w}");
    let n = w["created"]["$n"].as_str().unwrap().to_string();
    let f = c
        .call(
            "rdy_write",
            json!({"brief": token, "ops": [{"op": "update", "id": n, "body": "New and stable, edited."}]}),
        )
        .ok();
    assert!(f.get("briefing").is_none(), "{f}");
    assert!(f["on_change"].is_object(), "{f}");
    let b = c
        .call("rdy_brief", json!({"ids": [sp.section.to_string()]}))
        .ok();
    let ts = b["base"]["token"].as_str().unwrap().to_string();
    let d = c
        .call(
            "rdy_decide",
            json!({"brief": ts, "decision": {"parent": sp.section.to_string(), "statement": "New rule holds"}}),
        )
        .ok();
    assert_eq!(d["briefing"]["covers"], 1, "{d}");
    assert!(d["on_change"].is_object(), "{d}");
}

#[test]
fn nb9_mcp_daemon_and_local_created_receipts_agree() {
    // in-process MCP (no daemon)
    let (_d, mut r) = project();
    let sp = stable_spec(&mut r);
    let dir = r.root().to_path_buf();
    r.close().unwrap();
    let mut c = McpClient::start(&dir);
    mcp_receipts(&mut c, &sp, "local");
    drop(c);

    // MCP through the daemon
    let (s, sp) = served_stable();
    let mut c = McpClient::start_proxy(&s);
    mcp_receipts(&mut c, &sp, "daemon");
    drop(c);

    // the CLI wrappers through the daemon: rdy new (a create), rdy import (every author's blocks)
    let (ok, v) = s.json(&["--json", "new", "doc", "fresh", "--title", "Fresh"]);
    assert!(ok, "{v}");
    assert_eq!(v["briefing"]["covers"], 1, "{v}");
    assert!(is_token(v["briefing"]["token"].as_str().unwrap()), "{v}");
    s.stop();
    let out = s.dir.path().join("export");
    let r = Rdy::open(s.dir.path()).unwrap();
    r.read().export(&Scope::Canon, &out).unwrap();
    let n = r.read().materialized(&Scope::Canon).unwrap().len();
    r.close().unwrap();
    let t = Served::new();
    let (ok, v) = t.json(&["--json", "import", out.to_str().unwrap()]);
    assert!(ok, "{v}");
    assert_eq!(v["briefing"]["covers"], n, "{v}");
    assert!(is_token(v["briefing"]["token"].as_str().unwrap()), "{v}");
    t.stop();
}

/// The lines of `text` that are a created-block briefing line.
fn briefing_lines(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| l.starts_with("briefing: "))
        .collect()
}

#[test]
fn nb9_commit_and_edit_print_one_briefing_line() {
    let (s, seed) = wc::seeded();
    let agent = |args: &[&str]| s.rdy_as("agent:ed", Some("r1"), args);
    let agent_json = |args: &[&str]| {
        let mut a = vec!["--json"];
        a.extend_from_slice(args);
        s.json_as("agent:ed", Some("r1"), &a)
    };
    assert!(agent_json(&["checkout"]).0);
    let add_prose = |body: &str| {
        let orig = wc::read(&s, "notes.yaml");
        wc::put(
            &s,
            "notes.yaml",
            &format!(
                "{orig}- type: prose\n  parent: {}\n  body: {body}\n",
                seed.notes
            ),
        );
    };
    // JSON: the receipt object
    add_prose("Fresh prose.");
    assert!(agent_json(&["add", "notes.yaml"]).0);
    let (ok, v) = agent_json(&["commit", "-m", "create"]);
    assert!(ok, "{v}");
    assert_eq!(v["briefing"]["covers"], 1, "{v}");
    assert!(is_token(v["briefing"]["token"].as_str().unwrap()), "{v}");
    // text: exactly one line, after the result and before any warning
    add_prose("More fresh prose.");
    assert!(agent_json(&["add", "notes.yaml"]).0);
    let o = agent(&["commit", "-m", "create again"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    let lines = briefing_lines(&text);
    assert_eq!(lines.len(), 1, "{text}");
    assert!(
        is_token(lines[0].trim_start_matches("briefing: ")),
        "{text}"
    );
    let all: Vec<&str> = text.lines().collect();
    let at = all
        .iter()
        .position(|l| l.starts_with("briefing: "))
        .unwrap();
    let committed = all
        .iter()
        .position(|l| l.starts_with("committed "))
        .unwrap();
    assert!(committed < at, "{text}");
    if let Some(w) = all.iter().position(|l| l.starts_with("warning ")) {
        assert!(at < w, "{text}");
    }
    // no create, no line
    wc::set_body(&s, "notes.yaml", &seed.note, "Note, edited.");
    assert!(agent_json(&["add", "notes.yaml"]).0);
    let o = agent(&["commit", "-m", "edit"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(briefing_lines(&String::from_utf8_lossy(&o.stdout)).is_empty());

    // rdy new prints the line; its JSON carries the object
    let (ok, v) = s.json(&["--json", "new", "doc", "made", "--title", "Made"]);
    assert!(ok, "{v}");
    assert!(is_token(v["briefing"]["token"].as_str().unwrap()), "{v}");
    let o = s.rdy(&["new", "doc", "made-too", "--title", "Made too"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    assert_eq!(briefing_lines(&text).len(), 1, "{text}");
    assert!(text.starts_with("created doc "), "{text}");
}
