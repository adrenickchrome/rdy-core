//! ab2 (D-WJ0SJ, D-YAY1G): rdy_read, rdy_context, rdy_brief and rdy_query show the same derived
//! as-built status and the same override warnings. Source tools see planned content whole and
//! take no profile.
use rdy_engine::RdyError;
use rdy_engine::as_built::AS_BUILT_FIX_NO_ITEM;
use rdy_mcp::tools::*;
use rdy_qa::fixtures::*;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// A view `work` with a plain section, a planned section, three decisions and claims that cover
/// each status, then `fillers` more claims for paging.
fn world(fillers: usize) -> (tempfile::TempDir, McpState, BTreeMap<String, String>) {
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": {"name": "guide", "title": "Guide", "doc_type": "page"}}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Shipped"}}),
        json!({"op": "create", "type": "section", "id": "$p", "parent": "$d", "fields": {"title": "Planned", "planned": true}}),
        json!({"op": "create", "type": "decision", "id": "$dv", "fields": {"statement": "Ship V.", "status": "proposed", "built": "v1.2.0"}}),
        json!({"op": "create", "type": "decision", "id": "$dp", "fields": {"statement": "Plan P.", "status": "proposed"}}),
        json!({"op": "create", "type": "decision", "id": "$d2", "fields": {"statement": "Ship two.", "status": "proposed", "built": "2.0.0"}}),
        json!({"op": "create", "type": "claim", "id": "$c1", "parent": "$s", "body": "Shipped in one two.", "links": {"decided_by": ["$dv"]}}),
        json!({"op": "create", "type": "claim", "id": "$c2", "parent": "$s", "body": "PLANNEDSENTINEL text stays visible.", "links": {"implements": ["$dp"]}}),
        json!({"op": "create", "type": "claim", "id": "$c3", "parent": "$p", "body": "Under the planned section.", "links": {"decided_by": ["$d2"]}}),
        json!({"op": "create", "type": "claim", "id": "$c4", "parent": "$s", "body": "Said to ship early.", "fields": {"since": "v1.0.0"}, "links": {"decided_by": ["$d2"]}}),
    ];
    for i in 0..fillers {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$f{i}"), "parent": "$s", "body": format!("Filler claim number {i} holds.")}));
    }
    let (d, mut r) = project();
    let res = write(&mut r, "work", Value::Array(ops)).unwrap();
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer", "view": "work"}))
            .unwrap(),
    )
    .unwrap();
    (d, st, res.created.clone())
}

fn read(st: &mut McpState, args: Value) -> Result<Value, RdyError> {
    rdy_read(st, serde_json::from_value(args).unwrap())
}

fn tokens(v: &Value) -> usize {
    serde_json::to_string(v).unwrap().len() / 4
}

/// The as-built warnings of a reply, as (block, message, fix).
fn warnings(v: &Value) -> BTreeSet<(String, String, String)> {
    v["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == "W_AS_BUILT_OVERRIDE")
        .map(|w| {
            (
                w["block"].as_str().unwrap().to_string(),
                w["message"].as_str().unwrap().to_string(),
                w["fix"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn warning(block: &str, eff: &str, der: &str) -> (String, String, String) {
    (
        block.to_string(),
        format!("Block {block} has override status {eff}, but its decisions derive {der}."),
        AS_BUILT_FIX_NO_ITEM.to_string(),
    )
}

#[test]
fn ab2_read_context_brief_and_query_agree() {
    let (_d, mut st, c) = world(0);
    let (c1, c2, c3, c4, p, s) = (
        &c["$c1"], &c["$c2"], &c["$c3"], &c["$c4"], &c["$p"], &c["$s"],
    );
    let want_status = json!({
        c1.as_str(): {"status": "since", "since": "1.2.0", "derived": {"status": "since", "since": "1.2.0"}, "override": null},
        c2.as_str(): {"status": "planned", "since": null, "derived": {"status": "planned", "since": null}, "override": null},
        c3.as_str(): {"status": "planned", "since": null, "derived": {"status": "since", "since": "2.0.0"}, "override": {"block": p, "field": "planned"}},
        c4.as_str(): {"status": "since", "since": "1.0.0", "derived": {"status": "since", "since": "2.0.0"}, "override": {"block": c4, "field": "since"}},
        p.as_str(): {"status": "planned", "since": null, "derived": {"status": "unversioned", "since": null}, "override": {"block": p, "field": "planned"}},
        s.as_str(): {"status": "unversioned", "since": null, "derived": {"status": "unversioned", "since": null}, "override": null},
    });
    let want_warnings: BTreeSet<_> = [
        warning(c3, "planned", "since 2.0.0"),
        warning(c4, "since 1.0.0", "since 2.0.0"),
    ]
    .into();

    // all three read formats carry the same metadata beside unchanged text
    for format in ["compact", "markdown", "yaml"] {
        let v = read(&mut st, json!({"id": c["$d"], "format": format})).unwrap();
        assert_eq!(v["as_built"], want_status, "{format}");
        assert_eq!(warnings(&v), want_warnings, "{format}");
        let text = v["text"].as_str().unwrap();
        // source access stays whole: planned text is there, and no status is written into it
        assert!(text.contains("PLANNEDSENTINEL"), "{format}: {text}");
        assert!(
            !text.contains("as_built") && !text.contains("unversioned"),
            "{text}"
        );
        if format == "yaml" {
            // the authored spelling round-trips; the normalized one is only in the metadata
            assert!(text.contains("since: v1.0.0"), "{text}");
        }
    }
    // a pattern read keeps only the matching block, and only its metadata
    let v = read(
        &mut st,
        json!({"id": c["$d"], "pattern": "plannedsentinel"}),
    )
    .unwrap();
    assert_eq!(v["matched"], 1);
    assert_eq!(
        v["as_built"],
        json!({c2.as_str(): want_status[c2.as_str()]})
    );
    assert!(v.get("warnings").is_none(), "{v}");
    let v = read(&mut st, json!({"id": c["$d"], "pattern": "/early/"})).unwrap();
    assert_eq!(v["as_built"].as_object().unwrap().len(), 1);
    assert_eq!(
        warnings(&v),
        [warning(c4, "since 1.0.0", "since 2.0.0")].into()
    );
    // a decision is no content block: no metadata
    let v = read(&mut st, json!({"id": c["$d2"]})).unwrap();
    assert!(v.get("as_built").is_none(), "{v}");

    // context: the same status and warning, and the retirement status unchanged beside it
    for id in [c1, c2, c3, c4, p] {
        let v = rdy_context(&mut st, serde_json::from_value(json!({"id": id})).unwrap()).unwrap();
        assert_eq!(v["as_built"], want_status[id.as_str()], "{id}");
        let want: BTreeSet<_> = want_warnings
            .iter()
            .filter(|w| w.0 == *id)
            .cloned()
            .collect();
        assert_eq!(warnings(&v), want, "{id}");
        assert!(v["status"].is_null(), "{v}");
    }
    let dec = rdy_context(
        &mut st,
        serde_json::from_value(json!({"id": c["$d2"]})).unwrap(),
    )
    .unwrap();
    assert_eq!(dec["status"]["status"], "proposed", "{dec}");
    assert!(dec.get("as_built").is_none(), "{dec}");

    // brief: the base blocks carry the status, except an unversioned one; the briefing carries
    // the warnings
    let b = rdy_brief(
        &mut st,
        serde_json::from_value(json!({"path": c["$d"], "max_tokens": 100000})).unwrap(),
    )
    .unwrap();
    let base: BTreeMap<String, Value> = b["base"]["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r.get("as_built").is_some())
        .map(|r| (r["id"].as_str().unwrap().to_string(), r["as_built"].clone()))
        .collect();
    let mut versioned = want_status.clone();
    versioned.as_object_mut().unwrap().remove(s.as_str());
    assert_eq!(json!(base), versioned);
    assert_eq!(warnings(&b), want_warnings);

    // query: the derived fields select planned content too; source reads hide nothing
    let q = |st: &mut McpState, filter: &str| -> BTreeSet<String> {
        let v = rdy_query(
            st,
            serde_json::from_value(json!({"filter": filter, "max_tokens": 100000})).unwrap(),
        )
        .unwrap();
        v["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect()
    };
    let set = |ids: &[&String]| ids.iter().map(|i| i.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(q(&mut st, "planned = true"), set(&[c2, c3, p]));
    assert_eq!(q(&mut st, "planned = false"), set(&[c1, c4, s]));
    assert_eq!(q(&mut st, "planned != true"), set(&[c1, c4, s]));
    assert_eq!(q(&mut st, "has since"), set(&[c1, c4]));
    assert_eq!(q(&mut st, "since >= 1.2.0"), set(&[c1]));
    assert_eq!(q(&mut st, "since < v1.2.0"), set(&[c4]));
    assert_eq!(q(&mut st, "since = v1.0.0"), set(&[c4]));
    // a missing since makes every comparison false, != too
    assert_eq!(q(&mut st, "since != 9.9.9"), set(&[c1, c4]));
    // the raw field keeps its authored spelling
    assert_eq!(q(&mut st, "fields.since = v1.0.0"), set(&[c4]));
    assert_eq!(q(&mut st, "fields.since = 1.0.0"), BTreeSet::new());
    assert_eq!(q(&mut st, "fields.planned = true"), set(&[p]));
    // a decision or a doc has neither derived field
    assert_eq!(
        q(
            &mut st,
            "type = decision and (planned = false or planned = true or has since)"
        ),
        BTreeSet::new()
    );
    let e = rdy_query(
        &mut st,
        serde_json::from_value(json!({"filter": "since > soon"})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            "E_BAD_INPUT",
            "filter: since takes a release version at column 9",
            "read the filter guide in rdy_schema, or run rdy q --help"
        )
    );
    // no source tool takes a profile
    for tool in ["rdy_read", "rdy_query"] {
        let args = json!({"id": c1, "filter": "planned = true", "profile": "public"});
        let e = match tool {
            "rdy_read" => serde_json::from_value::<ReadParams>(args).map(|_| ()),
            _ => serde_json::from_value::<QueryParams>(args).map(|_| ()),
        };
        assert!(e.is_err(), "{tool} takes no profile");
    }
}

#[test]
fn ab2_paged_reads_and_briefings_carry_only_their_own_metadata() {
    let (_d, mut st, c) = world(80);
    let full = read(&mut st, json!({"id": c["$d"], "max_tokens": 100000})).unwrap();
    let all_status = full["as_built"].as_object().unwrap().clone();
    let all_warnings = warnings(&full);
    assert_eq!(all_warnings.len(), 2);
    let ids: Vec<String> = all_status.keys().cloned().collect();
    for max in [300usize, 900] {
        let mut v = read(&mut st, json!({"id": c["$d"], "max_tokens": max})).unwrap();
        let (mut seen_status, mut seen_warnings) = (serde_json::Map::new(), BTreeSet::new());
        let mut pages = 0;
        loop {
            assert!(tokens(&v) <= max, "{max}: {v}");
            let text = v["text"].as_str().unwrap();
            let shown: BTreeSet<&String> =
                ids.iter().filter(|i| text.contains(i.as_str())).collect();
            let meta = v["as_built"].as_object().cloned().unwrap_or_default();
            // the page carries the status of exactly the blocks it shows
            assert_eq!(meta.keys().collect::<BTreeSet<_>>(), shown, "{v}");
            for (k, s) in meta {
                assert_eq!(all_status[&k], s);
                seen_status.insert(k, s);
            }
            for w in warnings(&v) {
                assert!(shown.contains(&w.0), "{w:?} is not on the page");
                seen_warnings.insert(w);
            }
            pages += 1;
            let Some(next) = v["next"].as_str().map(str::to_string) else {
                break;
            };
            v = read(&mut st, json!({"page": next, "max_tokens": max})).unwrap();
            assert!(pages < 500);
        }
        assert!(pages > 1, "{max}");
        assert_eq!(seen_status, all_status);
        assert_eq!(seen_warnings, all_warnings);
    }

    // a briefing page lists the warnings of its own base blocks
    let mut b = rdy_brief(
        &mut st,
        serde_json::from_value(json!({"path": c["$d"], "max_tokens": 1500})).unwrap(),
    )
    .unwrap();
    let mut seen = BTreeSet::new();
    let mut pages = 0;
    loop {
        assert!(tokens(&b) <= 1500, "{b}");
        let on_page: BTreeSet<String> = b["base"]["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap().to_string())
            .collect();
        for w in warnings(&b) {
            assert!(on_page.contains(&w.0), "{w:?}");
            seen.insert(w);
        }
        for r in b["base"]["blocks"].as_array().unwrap() {
            let id = r["id"].as_str().unwrap();
            let want = all_status.get(id).filter(|s| s["status"] != "unversioned");
            assert_eq!(r.get("as_built"), want, "{id}");
        }
        pages += 1;
        let Some(next) = b["next"].as_str().map(str::to_string) else {
            break;
        };
        b = rdy_brief(
            &mut st,
            serde_json::from_value(json!({"page": next, "max_tokens": 1500})).unwrap(),
        )
        .unwrap();
        assert!(pages < 500);
    }
    assert!(pages > 1);
    assert_eq!(seen, all_warnings);

    // a built-only change moves the read: a cursor of the old read is stale
    let first = read(&mut st, json!({"id": c["$d"], "max_tokens": 300})).unwrap();
    let next = first["next"].as_str().unwrap().to_string();
    let dv = c["$dv"].clone();
    st.engine
        .write(Box::new(move |r| {
            write(
                r,
                "work",
                json!([{"op": "update", "id": dv, "fields": {"built": "1.3.0"}}]),
            )?;
            Ok(Value::Null)
        }))
        .unwrap();
    let e = read(&mut st, json!({"page": next})).unwrap_err();
    assert_eq!(e.code, "E_PAGE_STALE", "{e}");
    let v = read(&mut st, json!({"id": c["$c1"]})).unwrap();
    assert_eq!(v["as_built"][c["$c1"].as_str()]["since"], "1.3.0");
}
