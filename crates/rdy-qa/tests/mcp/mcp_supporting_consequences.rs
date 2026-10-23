//! rf10 (D-4WC6R, D-ZWT4H): over MCP, a rewrite whose `Reflects` names a source the view no
//! longer holds is an unjustified change, and a source edit gives the blocks that reflect it
//! their follow-up. The same tension crosses rdy_write, rdy_view and rdy_propose, and the same
//! follow-up crosses rdy_write, rdy_propose and rdy_plan. The rewrite is a typed store commit
//! with the normalized model `Reflects` (no raw reflects input).
use rdy_engine::engine::{Local, write_typed};
use rdy_engine::*;
use rdy_mcp::tools::*;
use rdy_mcp::view_tools::rdy_view;
use rdy_model::{BlockId, Change, Justification, Link, Op, Provenance, Role};
use rdy_qa::edit_loop::brief_of;
use rdy_qa::fixtures::*;
use serde_json::{Value, from_value, json};
use std::sync::Arc;

fn id(s: &str) -> BlockId {
    BlockId::parse(s).unwrap()
}

/// Rewrites `b` in `view` with `body`, justified by `j`, with `deps`; `reflects` (if any) becomes
/// its one `reflects` link, pinned at the target's content in `view`.
fn rewrite(
    r: &mut Rdy,
    view: &str,
    b: &BlockId,
    body: &str,
    j: Option<Justification>,
    reflects: Option<&BlockId>,
    deps: Vec<rdy_model::Hash>,
) -> rdy_model::Hash {
    let scope = Scope::View(view.into());
    let st = r.read();
    let mut next = st.block(&scope, b).unwrap();
    let base = next.version_hash();
    next.body = body.into();
    if let Some(t) = reflects {
        let pin = st
            .block(&scope, t)
            .unwrap()
            .content_hash_with(st.reader().tracking());
        next.links.insert(
            "reflects".into(),
            vec![Link {
                target: t.clone(),
                part: None,
            }],
        );
        next.pins.insert(t.clone(), pin);
    }
    drop(st);
    let h = r
        .store_mut()
        .commit_to_view(
            view,
            Change {
                view: view.into(),
                ops: vec![Op::Update { base, block: next }],
                deps,
                provenance: Provenance {
                    actor: "agent:qa".into(),
                    run: None,
                    role: Role::Writer,
                    at_ms: 7,
                },
                justification: j,
                message: String::new(),
                links: Default::default(),
                read_identity: None,
            },
        )
        .unwrap();
    r.invalidate();
    h
}

fn call<P: serde::de::DeserializeOwned>(
    st: &mut McpState,
    f: fn(&mut McpState, P) -> Result<Value, RdyError>,
    v: Value,
) -> Value {
    f(st, from_value(v.clone()).unwrap()).unwrap_or_else(|e| panic!("{v}: {e}"))
}

/// The unjustified-change tensions a reply lists under `key` (a list of tension rows): their full
/// ids and blocks.
fn unjustified(rows: &Value) -> Vec<(String, Vec<String>)> {
    rows.as_array()
        .unwrap_or_else(|| panic!("no rows: {rows}"))
        .iter()
        .filter(|t| t["kind"] == "unjustified_change")
        .map(|t| {
            (
                t["tension"].as_str().unwrap().to_string(),
                t["blocks"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|b| b.as_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect()
}

/// The (block, kind, edge, follow_up) of each consequence entry for `block` in a reply.
fn followups(reply: &Value, block: &BlockId) -> Vec<(String, String, String, String)> {
    reply["consequences"]
        .as_array()
        .unwrap_or_else(|| panic!("no consequences: {reply}"))
        .iter()
        .filter(|c| c["block"] == block.to_string())
        .map(|c| {
            (
                c["block"].as_str().unwrap().to_string(),
                c["kind"].as_str().unwrap().to_string(),
                c["because"]["edge"].as_str().unwrap().to_string(),
                c["follow_up"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

/// The (kind, subject, cause, edge) of each plan item in a reply.
fn items(reply: &Value) -> Vec<(String, String, String, String)> {
    let mut out: Vec<_> = reply["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items: {reply}"))
        .iter()
        .map(|i| {
            let s = |k: &str| i[k].as_str().unwrap().to_string();
            (s("kind"), s("subject"), s("cause"), s("edge"))
        })
        .collect();
    out.sort();
    out
}

#[test]
fn rf10_mcp_reports_missing_source_tension_and_followups() {
    let (_d, mut r) = project();
    // canon: stable claims x and y; y states s1's value and reflects it
    let w = write(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "spec", "title": "Spec", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Rules"}},
            {"op": "create", "type": "claim", "id": "$x", "parent": "$s", "body": "X holds."},
            {"op": "create", "type": "claim", "id": "$y", "parent": "$s", "body": "Keep 30 days."},
            {"op": "create", "type": "doc", "id": "$r", "fields": {"name": "refs", "title": "Refs", "doc_type": "overview"}},
            {"op": "create", "type": "section", "id": "$rs", "parent": "$r", "fields": {"title": "Sources"}},
            {"op": "create", "type": "source", "id": "$s1", "parent": "$rs",
                "fields": {"title": "Release 1", "kind": "release", "locator": "v1"},
                "body": "Retention is 30 days."}
        ]),
    )
    .unwrap();
    let g = |k: &str| id(&w.created[k]);
    let (sec, x, y, refs, s1) = (g("$s"), g("$x"), g("$y"), g("$rs"), g("$s1"));
    let deps = r.read().included("seed").to_vec();
    rewrite(&mut r, "seed", &y, "Keep 30 days.", None, Some(&s1), deps);
    land(&mut r, "seed").unwrap();
    let t = brief_of(&r, &Scope::Canon, &[&sec]).base.token;
    rdy_qa::edit_loop::write_cited(
        &mut r,
        "st",
        json!([{"op": "update", "id": sec.to_string(), "fields": {"lifecycle": "stable"}}]),
        Some(&t),
    )
    .unwrap();
    land(&mut r, "st").unwrap();

    let mut st = McpState::new(Arc::new(Local::new(r)));
    call(
        &mut st,
        rdy_session,
        json!({"actor": "agent:qa", "role": "writer", "view": "rf"}),
    );
    // C0 creates s3; C1 (a typed commit, depending on nothing) rewrites x reflecting s3
    let c0 = call(
        &mut st,
        rdy_write,
        json!({"ops": [{"op": "create", "type": "source", "id": "$s3", "parent": refs.to_string(),
            "fields": {"title": "Release 3", "kind": "release", "locator": "v3"}}]}),
    );
    let s3 = id(c0["created"]["$s3"].as_str().unwrap());
    let c0 = c0["change"].as_str().unwrap().to_string();
    let (x2, s32) = (x.clone(), s3.clone());
    let c1 = write_typed(&*st.engine, move |r| {
        Ok(rewrite(
            r,
            "rf",
            &x2,
            "X follows release 3.",
            Some(Justification::Reflects {
                sources: vec![s32],
                code: false,
            }),
            None,
            vec![],
        )
        .to_string())
    })
    .unwrap();
    let c1 = c1.as_str().unwrap().to_string();

    // rdy_write: the source edit gives y its follow-up through reflects
    let edit = call(
        &mut st,
        rdy_write,
        json!({"ops": [{"op": "update", "id": s1.to_string(), "body": "Retention is 7 days."}]}),
    );
    let want_y = vec![(
        y.to_string(),
        "suspect".to_string(),
        "reflects".to_string(),
        "update".to_string(),
    )];
    assert_eq!(followups(&edit, &y), want_y, "{edit:#}");
    assert!(unjustified(&edit["on_change"]["tensions"]).is_empty());

    // rdy_write: deleting s3 makes C1 unjustified in the view
    let del = call(
        &mut st,
        rdy_write,
        json!({"ops": [{"op": "delete", "id": s3.to_string()}]}),
    );
    let opened = unjustified(&del["on_change"]["tensions"]);
    assert_eq!(opened.len(), 1, "{del:#}");
    let (tension, blocks) = opened[0].clone();
    assert_eq!(blocks, vec![x.to_string()]);
    let row = del["on_change"]["tensions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["tension"] == tension.as_str())
        .unwrap();
    assert_eq!(
        row["summary"],
        format!(
            "change {} edits stable normative block {x} without a justification",
            &c1[..12]
        )
    );
    assert_eq!(row["severity"], "advisory");

    // rdy_view: undoing the delete resolves it; excluding C0 opens it again
    let undo = call(&mut st, rdy_view, json!({"op": "undo"}));
    let resolved: Vec<&str> = undo["on_change"]["resolved"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["tension"].as_str())
        .collect();
    assert_eq!(resolved, vec![tension.as_str()], "{undo:#}");
    let excl = call(&mut st, rdy_view, json!({"op": "exclude", "changes": [c0]}));
    assert_eq!(
        unjustified(&excl["on_change"]["tensions"]),
        vec![(tension.clone(), vec![x.to_string()])],
        "{excl:#}"
    );

    // rdy_propose: the landing form lists it, and y's follow-up is the same
    let p = call(
        &mut st,
        rdy_propose,
        json!({"title": "Release 7", "summary": "Follow release 7."}),
    );
    assert_eq!(
        unjustified(&p["tensions"]),
        vec![(tension.clone(), vec![x.to_string()])],
        "{p:#}"
    );
    assert_eq!(followups(&p, &y), want_y, "{p:#}");

    // rdy_plan: the same follow-up, read for the view and for the version
    let want_items = vec![(
        "update".to_string(),
        y.to_string(),
        s1.to_string(),
        "reflects".to_string(),
    )];
    let pv = call(&mut st, rdy_plan, json!({}));
    assert_eq!(items(&pv), want_items, "{pv:#}");
    let pp = call(
        &mut st,
        rdy_plan,
        json!({"proposal": p["ref"].as_str().unwrap()}),
    );
    assert_eq!(items(&pp), want_items, "{pp:#}");
}
