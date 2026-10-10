use rdy_engine::*;
use rdy_model::*;
use serde_json::json;

fn agent() -> Session {
    Session {
        actor: "agent:designer".into(),
        run: Some("run-1".into()),
        role: Role::Writer,
        kind: rdy_engine::SessionKind::Human,
    }
}

fn ops(v: serde_json::Value) -> Vec<OpInput> {
    serde_json::from_value(v).unwrap()
}

fn req(v: serde_json::Value) -> WriteRequest {
    WriteRequest {
        view: "work".into(),
        ops: ops(v),
        justification: None,
        message: "test".into(),
    }
}

fn project() -> (tempfile::TempDir, Rdy) {
    let dir = tempfile::tempdir().unwrap();
    let r = Rdy::init(dir.path(), "rdy-standard").unwrap();
    (dir, r)
}

fn skeleton(r: &mut Rdy) -> WriteResult {
    r.write(&agent(), req(json!([
        {"op": "create", "type": "doc", "id": "$doc", "fields": {"title": "Identity", "doc_type": "glossary"}},
        {"op": "create", "type": "section", "id": "$terms", "parent": "$doc", "fields": {"title": "Terms"}},
        {"op": "create", "type": "term", "id": "$ledger", "parent": "$terms", "fields": {"name": "Ledger"}, "body": "The record of balances."},
        {"op": "create", "type": "claim", "id": "$c1", "parent": "$terms", "body": "Balances live on the {{$ledger}}."}
    ]))).unwrap()
}

#[test]
fn placeholders_build_a_document_in_one_write() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let ledger = BlockId::parse(&res.created["$ledger"]).unwrap();
    let c1 = BlockId::parse(&res.created["$c1"]).unwrap();
    let claim = r.read().block(&Scope::View("work".into()), &c1).unwrap();
    assert_eq!(
        claim.body,
        format!("Balances live on the {{{{{ledger}}}}}.")
    );
    let term = r
        .read()
        .block(&Scope::View("work".into()), &ledger)
        .unwrap();
    assert_eq!(
        claim.pins[&ledger],
        term.content_hash(),
        "same-write targets are pinned to their stored content"
    );
    assert!(res.warnings.is_empty());
    assert!(
        r.read().materialized(&Scope::Canon).unwrap().is_empty(),
        "nothing in canon yet"
    );
}

#[test]
fn provenance_comes_from_the_session() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let change: Change = r.store().get(&res.change.parse().unwrap()).unwrap();
    assert_eq!(change.provenance.actor, "agent:designer");
    assert_eq!(change.provenance.run.as_deref(), Some("run-1"));
}

#[test]
fn references_to_existing_blocks_are_pinned() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let ledger_id = res.created["$ledger"].clone();
    let res2 = r.write(&agent(), req(json!([
        {"op": "create", "type": "claim", "parent": res.created["$terms"], "body": format!("See {{{{{ledger_id}}}}}.")}
    ]))).unwrap();
    let id = BlockId::parse(&res2.created["op0"]).unwrap();
    let scope = Scope::View("work".into());
    let ledger = r
        .read()
        .block(&scope, &BlockId::parse(&ledger_id).unwrap())
        .unwrap();
    let claim = r.read().block(&scope, &id).unwrap();
    assert_eq!(claim.pins[&ledger.id], ledger.content_hash());
}

#[test]
fn unknown_and_malformed_references_are_refused_and_nothing_is_written() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let before = r.store().view("work").unwrap().changes.len();
    let e = r.write(&agent(), req(json!([
        {"op": "create", "type": "claim", "parent": res.created["$terms"], "body": "See {{t-ZZZZZ}}."}
    ]))).unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_REF");
    assert!(e.message.contains("t-ZZZZZ"));
    let e = r.write(&agent(), req(json!([
        {"op": "create", "type": "claim", "parent": res.created["$terms"], "body": "See {{the ledger}}."}
    ]))).unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_REF");
    assert!(e.fix.contains("{{"), "{}", e.fix);
    assert_eq!(r.store().view("work").unwrap().changes.len(), before);
}

#[test]
fn validation_errors_carry_issues() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let e = r
        .write(
            &agent(),
            req(json!([
                {"op": "create", "type": "claim", "parent": res.created["$terms"], "body": ""}
            ])),
        )
        .unwrap_err();
    assert_eq!(e.code, "E_VALIDATION");
    assert_eq!(e.issues[0].code, "E_BODY_REQUIRED");
}

#[test]
fn stale_base_is_refused_with_the_current_hash() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let c1 = res.created["$c1"].clone();
    let scope = Scope::View("work".into());
    let v1 = r
        .read()
        .block(&scope, &BlockId::parse(&c1).unwrap())
        .unwrap()
        .version_hash();
    r.write(
        &agent(),
        req(json!([{"op": "update", "id": c1, "base": v1.to_string(), "body": "Edited once."}])),
    )
    .unwrap();
    let v2 = r
        .read()
        .block(&scope, &BlockId::parse(&c1).unwrap())
        .unwrap()
        .version_hash();
    let e = r.write(&agent(), req(json!([{"op": "update", "id": c1, "base": v1.to_string(), "body": "Edited twice."}]))).unwrap_err();
    assert_eq!(e.code, "E_STALE_BASE");
    assert!(e.fix.contains(&v2.to_string()));
}

#[test]
fn after_controls_sibling_order() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let terms = res.created["$terms"].clone();
    let ledger = res.created["$ledger"].clone();
    let res2 = r.write(&agent(), req(json!([
        {"op": "create", "type": "note", "parent": terms, "after": ledger, "body": "Between ledger and claim."}
    ]))).unwrap();
    let scope = Scope::View("work".into());
    let m = r.read().materialized(&scope).unwrap();
    let kids: Vec<String> = m
        .children(Some(&BlockId::parse(&terms).unwrap()))
        .iter()
        .map(|b| b.id.to_string())
        .collect();
    assert_eq!(
        kids,
        vec![
            ledger,
            res2.created["op0"].clone(),
            res.created["$c1"].clone()
        ]
    );
}

#[test]
fn permissions_and_structure_rules() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let reader = Session {
        role: Role::Reader,
        ..agent()
    };
    assert_eq!(
        r.write(
            &reader,
            req(json!([{"op": "delete", "id": res.created["$c1"]}]))
        )
        .unwrap_err()
        .code,
        "E_PERMISSION"
    );
    let e = r.write(&agent(), req(json!([
        {"op": "create", "type": "decision", "parent": res.created["$terms"], "fields": {"statement": "Use a ledger", "status": "accepted"}}
    ]))).unwrap_err();
    assert_eq!(e.code, "E_PERMISSION");
    assert_eq!(
        r.write(
            &agent(),
            req(json!([{"op": "delete", "id": res.created["$terms"]}]))
        )
        .unwrap_err()
        .code,
        "E_HAS_CHILDREN"
    );
    let canon_write = WriteRequest {
        view: "canon".into(),
        ops: ops(json!([{"op": "delete", "id": res.created["$c1"]}])),
        justification: None,
        message: String::new(),
    };
    assert_eq!(
        r.write(&agent(), canon_write).unwrap_err().code,
        "E_RESERVED_VIEW"
    );
    assert_eq!(
        r.write(
            &agent(),
            req(json!([{"op": "create", "type": "claim", "parent": "$nope", "body": "x"}]))
        )
        .unwrap_err()
        .code,
        "E_BAD_INPUT"
    );
}

#[test]
fn missing_skeleton_sections_are_warnings() {
    let (_d, mut r) = project();
    let res = r.write(&agent(), req(json!([
        {"op": "create", "type": "doc", "fields": {"title": "Overview", "doc_type": "overview"}}
    ]))).unwrap();
    let codes: Vec<_> = res.warnings.iter().map(|w| w.code).collect();
    assert_eq!(codes, vec!["W_SKELETON_MISSING", "W_SKELETON_MISSING"]); // Summary, Goals
}

fn approver() -> Session {
    Session {
        role: Role::Approver,
        ..agent()
    }
}

fn view_len(r: &Rdy) -> usize {
    r.store().view("work").unwrap().changes.len()
}

fn code(r: &mut Rdy, v: serde_json::Value) -> &'static str {
    r.write(&agent(), req(v)).unwrap_err().code
}

#[test]
fn move_into_own_subtree_is_refused() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let before = view_len(&r);
    let (doc, terms, ledger) = (
        res.created["$doc"].clone(),
        res.created["$terms"].clone(),
        res.created["$ledger"].clone(),
    );
    assert_eq!(
        code(
            &mut r,
            json!([{"op": "move", "id": terms, "parent": terms}])
        ),
        "E_BAD_INPUT"
    );
    assert_eq!(
        code(
            &mut r,
            json!([{"op": "move", "id": terms, "parent": ledger}])
        ),
        "E_BAD_INPUT"
    );
    assert_eq!(
        code(&mut r, json!([{"op": "move", "id": doc, "parent": terms}])),
        "E_BAD_INPUT"
    );
    assert_eq!(view_len(&r), before);
}

#[test]
fn successful_move_changes_parent_and_order() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let w = r.write(&agent(), req(json!([
        {"op": "create", "type": "section", "parent": res.created["$doc"], "fields": {"title": "More"}}
    ]))).unwrap();
    let more = w.created["op0"].clone();
    r.write(
        &agent(),
        req(json!([{"op": "move", "id": res.created["$c1"], "parent": more}])),
    )
    .unwrap();
    let scope = Scope::View("work".into());
    let c1 = r
        .read()
        .block(&scope, &BlockId::parse(&res.created["$c1"]).unwrap())
        .unwrap();
    assert_eq!(c1.parent.unwrap().to_string(), more);
    let m = r.read().materialized(&scope).unwrap();
    assert_eq!(m.children(Some(&BlockId::parse(&more).unwrap())).len(), 1);
}

#[test]
fn root_creates_keep_input_order_and_honour_after() {
    let (_d, mut r) = project();
    let res = r.write(&agent(), req(json!([
        {"op": "create", "type": "doc", "id": "$a", "fields": {"title": "A", "doc_type": "glossary"}},
        {"op": "create", "type": "doc", "id": "$b", "fields": {"title": "B", "doc_type": "glossary"}},
        {"op": "create", "type": "doc", "id": "$c", "after": "$a", "fields": {"title": "C", "doc_type": "glossary"}}
    ]))).unwrap();
    let m = r.read().materialized(&Scope::View("work".into())).unwrap();
    let roots: Vec<String> = m.children(None).iter().map(|b| b.id.to_string()).collect();
    assert_eq!(
        roots,
        vec![
            res.created["$a"].clone(),
            res.created["$c"].clone(),
            res.created["$b"].clone()
        ]
    );
}

#[test]
fn after_must_be_a_sibling_even_at_root() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let before = view_len(&r);
    let doc = res.created["$doc"].clone();
    assert_eq!(
        code(
            &mut r,
            json!([{"op": "create", "type": "doc", "after": res.created["$terms"], "fields": {"title": "X", "doc_type": "glossary"}}])
        ),
        "E_BAD_INPUT"
    );
    assert_eq!(
        code(
            &mut r,
            json!([{"op": "create", "type": "claim", "parent": doc, "after": res.created["$c1"], "body": "x"}])
        ),
        "E_BAD_INPUT"
    );
    assert_eq!(view_len(&r), before);
}

#[test]
fn placeholder_misuse_is_bad_input() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let terms = res.created["$terms"].clone();
    let e = r.write(&agent(), req(json!([{"op": "create", "type": "claim", "parent": terms, "body": "See {{$later}}."}]))).unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.message.contains("$later"));
    assert_eq!(
        code(
            &mut r,
            json!([
                {"op": "create", "type": "claim", "id": "$x", "parent": terms, "body": "one"},
                {"op": "create", "type": "claim", "id": "$x", "parent": terms, "body": "two"}
            ])
        ),
        "E_BAD_INPUT"
    );
}

#[test]
fn a_block_cannot_be_touched_twice_including_delete_then_create() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let c1 = res.created["$c1"].clone();
    assert_eq!(
        code(
            &mut r,
            json!([
                {"op": "delete", "id": c1},
                {"op": "create", "type": "claim", "id": c1, "parent": res.created["$terms"], "body": "again"}
            ])
        ),
        "E_BAD_INPUT"
    );
    assert_eq!(
        code(
            &mut r,
            json!([
                {"op": "update", "id": c1, "body": "a"},
                {"op": "update", "id": c1, "body": "b"}
            ])
        ),
        "E_BAD_INPUT"
    );
}

#[test]
fn updates_keep_existing_pins_and_moves_keep_all_pins() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let scope = Scope::View("work".into());
    let (terms, ledger, c1) = (
        res.created["$terms"].clone(),
        res.created["$ledger"].clone(),
        res.created["$c1"].clone(),
    );
    let w = r.write(&agent(), req(json!([
        {"op": "create", "type": "claim", "parent": terms, "body": format!("About {{{{{ledger}}}}}.")}
    ]))).unwrap();
    let pinned = w.created["op0"].clone();
    let lid = BlockId::parse(&ledger).unwrap();
    let pid = BlockId::parse(&pinned).unwrap();
    let v1 = r.read().block(&scope, &lid).unwrap().content_hash();
    r.write(
        &agent(),
        req(json!([{"op": "update", "id": ledger, "body": "A newer definition."}])),
    )
    .unwrap();
    let v2 = r.read().block(&scope, &lid).unwrap().content_hash();
    assert_ne!(v1, v2);
    r.write(&agent(), req(json!([{"op": "update", "id": pinned, "body": format!("Edited about {{{{{ledger}}}}}.")}]))).unwrap();
    assert_eq!(
        r.read().block(&scope, &pid).unwrap().pins[&lid],
        v1,
        "existing pin kept on edit"
    );
    // c1 was pinned when the skeleton was written in one write: an edit keeps that pin
    let c1_pin = r
        .read()
        .block(&scope, &BlockId::parse(&c1).unwrap())
        .unwrap()
        .pins[&lid];
    r.write(
        &agent(),
        req(json!([{"op": "update", "id": c1, "body": format!("Now {{{{{ledger}}}}}.")}])),
    )
    .unwrap();
    assert_eq!(
        r.read()
            .block(&scope, &BlockId::parse(&c1).unwrap())
            .unwrap()
            .pins[&lid],
        c1_pin
    );
    // a reference new in this edit gets the current version
    let fresh = r.write(&agent(), req(json!([{"op": "create", "type": "claim", "parent": terms, "body": "No references yet."}]))).unwrap().created["op0"].clone();
    r.write(
        &agent(),
        req(json!([{"op": "update", "id": fresh, "body": format!("Now {{{{{ledger}}}}}.")}])),
    )
    .unwrap();
    assert_eq!(
        r.read()
            .block(&scope, &BlockId::parse(&fresh).unwrap())
            .unwrap()
            .pins[&lid],
        v2
    );
    // moves keep pins unchanged
    let sec = r.write(&agent(), req(json!([{"op": "create", "type": "section", "parent": res.created["$doc"], "fields": {"title": "Other"}}]))).unwrap();
    r.write(
        &agent(),
        req(json!([{"op": "move", "id": pinned, "parent": sec.created["op0"]}])),
    )
    .unwrap();
    assert_eq!(r.read().block(&scope, &pid).unwrap().pins[&lid], v1);
}

#[test]
fn remaining_rules_have_codes() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let (terms, c1, ledger) = (
        res.created["$terms"].clone(),
        res.created["$c1"].clone(),
        res.created["$ledger"].clone(),
    );
    assert_eq!(code(&mut r, json!([])), "E_BAD_INPUT");
    assert_eq!(
        code(
            &mut r,
            json!([{"op": "update", "id": "b-ZZZZZ", "body": "x"}])
        ),
        "E_UNKNOWN_BLOCK"
    );
    assert_eq!(
        code(
            &mut r,
            json!([{"op": "create", "type": "claim", "id": c1, "parent": terms, "body": "x"}])
        ),
        "E_ALREADY_EXISTS"
    );
    assert_eq!(
        code(
            &mut r,
            json!([{"op": "update", "id": ledger, "fields": {"name": 1.5}}])
        ),
        "E_BAD_INPUT"
    );
    assert_eq!(
        code(
            &mut r,
            json!([{"op": "update", "id": c1, "base": "nothex", "body": "x"}])
        ),
        "E_BAD_INPUT"
    );
    assert_eq!(
        code(
            &mut r,
            json!([{"op": "create", "type": "claim", "parent": terms, "links": {"refines": [{"target": "t-ZZZZZ"}]}, "body": "x"}])
        ),
        "E_UNKNOWN_REF"
    );
}

#[test]
fn unset_removes_a_field_and_links_accept_placeholders() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let scope = Scope::View("work".into());
    let w0 = r.write(&agent(), req(json!([
        {"op": "create", "type": "requirement", "parent": res.created["$terms"], "fields": {"priority": "must"}, "body": "Must hold."}
    ]))).unwrap();
    let req_id = BlockId::parse(&w0.created["op0"]).unwrap();
    assert!(
        r.read()
            .block(&scope, &req_id)
            .unwrap()
            .fields
            .contains_key("priority")
    );
    r.write(
        &agent(),
        req(json!([{"op": "update", "id": req_id.to_string(), "unset": ["priority"]}])),
    )
    .unwrap();
    assert!(
        !r.read()
            .block(&scope, &req_id)
            .unwrap()
            .fields
            .contains_key("priority")
    );
    let w = r.write(&agent(), req(json!([
        {"op": "create", "type": "claim", "id": "$p", "parent": res.created["$terms"], "body": "Premise."},
        {"op": "create", "type": "claim", "id": "$q", "parent": res.created["$terms"], "body": "Conclusion.",
         "links": {"refines": [{"target": "$p"}]}}
    ]))).unwrap();
    let q = r
        .read()
        .block(&scope, &BlockId::parse(&w.created["$q"]).unwrap())
        .unwrap();
    let links: Vec<_> = q.links.values().flatten().collect();
    assert_eq!(links[0].target.to_string(), w.created["$p"]);
    let p = r.read().block(&scope, &links[0].target).unwrap();
    assert_eq!(q.pins[&p.id], p.content_hash());
}

#[test]
fn frozen_decisions_cannot_change() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let w = r.write(&approver(), req(json!([
        {"op": "create", "type": "decision", "parent": res.created["$terms"], "fields": {"statement": "Use a ledger", "status": "accepted"}}
    ]))).unwrap();
    let e = r
        .write(
            &agent(),
            req(json!([{"op": "update", "id": w.created["op0"], "body": "changed"}])),
        )
        .unwrap_err();
    assert_eq!(e.code, "E_FROZEN");
}

#[test]
fn a_failing_second_op_writes_nothing() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let before = view_len(&r);
    let blocks_before = r
        .read()
        .materialized(&Scope::View("work".into()))
        .unwrap()
        .len();
    let e = r.write(&agent(), req(json!([
        {"op": "create", "type": "claim", "id": "$ok", "parent": res.created["$terms"], "body": "Fine."},
        {"op": "update", "id": "b-ZZZZZ", "body": "x"}
    ]))).unwrap_err();
    assert_eq!(e.code, "E_UNKNOWN_BLOCK");
    assert_eq!(view_len(&r), before);
    assert_eq!(
        r.read()
            .materialized(&Scope::View("work".into()))
            .unwrap()
            .len(),
        blocks_before
    );
}

#[test]
fn same_write_references_are_pinned_in_dependency_order_and_later_edits_make_suspects() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let scope = Scope::View("work".into());
    let terms = res.created["$terms"].clone();
    // a chain built in one write: claim -> requirement -> term
    let w = r.write(&agent(), req(json!([
        {"op": "create", "type": "term", "id": "$t", "parent": terms, "fields": {"name": "Wallet"}, "body": "Holds a balance."},
        {"op": "create", "type": "requirement", "id": "$q", "parent": terms, "body": "Every member has one {{$t}}."},
        {"op": "create", "type": "claim", "id": "$c", "parent": terms, "body": "Per {{$q}}, wallets exist."}
    ]))).unwrap();
    let id = |k: &str| BlockId::parse(&w.created[k]).unwrap();
    let (t, q, c) = (
        r.read().block(&scope, &id("$t")).unwrap(),
        r.read().block(&scope, &id("$q")).unwrap(),
        r.read().block(&scope, &id("$c")).unwrap(),
    );
    assert_eq!(q.pins[&t.id], t.content_hash());
    assert_eq!(
        c.pins[&q.id],
        q.content_hash(),
        "pinned to the target's final content"
    );
    assert!(r.read().context(&scope, &c.id).unwrap().suspects.is_empty());

    r.write(
        &agent(),
        req(json!([{"op": "update", "id": t.id.to_string(), "body": "Holds a balance in pence."}])),
    )
    .unwrap();
    let sus = r.read().context(&scope, &q.id).unwrap().suspects;
    assert_eq!(sus.len(), 1);
    assert_eq!(
        (sus[0].target.clone(), sus[0].reason.as_str()),
        (t.id.clone(), "changed")
    );
}

#[test]
fn braces_in_code_and_diagrams_are_content_and_escapes_are_literal() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let scope = Scope::View("work".into());
    let (terms, ledger) = (
        res.created["$terms"].clone(),
        res.created["$ledger"].clone(),
    );
    let w = r.write(&agent(), req(json!([
        {"op": "create", "type": "term", "id": "$w", "parent": terms, "fields": {"name": "Wallet"}, "body": "Holds a balance."},
        {"op": "create", "type": "diagram", "id": "$dg", "parent": terms, "fields": {"lang": "mermaid", "alt": "flow"},
         "body": format!("graph TD\n  A{{{{Decide?}}}} --> B[{{{{{ledger}}}}}] --> C[{{{{$w}}}}]\n")},
        {"op": "create", "type": "code", "id": "$cd", "parent": terms, "fields": {"lang": "yaml"}, "body": "image: {{ .Values.image }}\nname: {{$w}}\n"},
        {"op": "create", "type": "claim", "id": "$c", "parent": terms, "body": r"Write \{{literal}} to show braces."}
    ]))).unwrap();
    let get = |r: &mut Rdy, k: &str| {
        r.read()
            .block(&scope, &BlockId::parse(&w.created[k]).unwrap())
            .unwrap()
    };
    let (wallet, dg, cd, c) = (
        get(&mut r, "$w"),
        get(&mut r, "$dg"),
        get(&mut r, "$cd"),
        get(&mut r, "$c"),
    );
    assert_eq!(
        dg.body,
        format!(
            "graph TD\n  A{{{{Decide?}}}} --> B[{{{{{ledger}}}}}] --> C[{{{{{}}}}}]\n",
            wallet.id
        ),
        "placeholders substitute in diagrams"
    );
    assert_eq!(dg.pins.len(), 2);
    assert_eq!(
        cd.body, "image: {{ .Values.image }}\nname: {{$w}}\n",
        "code bodies are left alone"
    );
    assert!(cd.pins.is_empty());
    assert_eq!(c.body, r"Write \{{literal}} to show braces.");
    assert!(c.pins.is_empty());
    // prose keeps strict checking
    assert_eq!(
        code(
            &mut r,
            json!([{"op": "create", "type": "claim", "parent": terms, "body": "{{not an id}}"}])
        ),
        "E_UNKNOWN_REF"
    );
}

#[test]
fn questions_cannot_be_deleted() {
    let (_d, mut r) = project();
    let res = skeleton(&mut r);
    let q = r.write(&agent(), req(json!([{"op": "create", "type": "question", "parent": res.created["$terms"], "fields": {"prompt": "Do wallets move?"}}]))).unwrap().created["op0"].clone();
    let e = r
        .write(&agent(), req(json!([{"op": "delete", "id": q}])))
        .unwrap_err();
    assert_eq!(e.code, "E_BAD_INPUT");
    assert!(e.message.contains("never deleted"), "{}", e.message);
}

#[test]
fn msw11_a_nonce_lookup_since_a_head_reads_only_the_changes_after_it() {
    let (_d, mut r) = project();
    let commit = |r: &mut Rdy, name: String, nonce: String| {
        let req = req(json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}
        ]));
        let links = ChangeLinks {
            client_nonce: Some(nonce),
            ..Default::default()
        };
        r.write_linked(&agent(), req, links, None).unwrap();
    };
    for i in 0..30 {
        commit(&mut r, format!("old{i}"), format!("old-{i}"));
    }
    let since = r.read().view_head("work").unwrap().head.to_string();
    commit(&mut r, "mine".into(), "mine".into());
    // many other commits land in the view before the probe
    let others = 20;
    for i in 0..others {
        commit(&mut r, format!("other{i}"), format!("other-{i}"));
    }
    let st = r.read();
    let mine = st
        .change_by_nonce_since("work", "mine", &since, 10_000)
        .unwrap();
    assert!(mine.change.is_some() && !mine.capped, "{mine:?}");
    // the reads grow with the commits after the head, not with the history before it
    let after = others + 1;
    let absent = st
        .change_by_nonce_since("work", "none", &since, 10_000)
        .unwrap();
    assert_eq!((absent.change, absent.capped), (None, false));
    assert!(
        absent.read >= after && absent.read <= 2 * after,
        "{absent:?}"
    );
    let late = st.view_head("work").unwrap().head.to_string();
    let none_after = st
        .change_by_nonce_since("work", "none", &late, 10_000)
        .unwrap();
    assert_eq!(none_after.read, 0, "{none_after:?}");
}

/// Final review M3: the walk after a head stops at its limit, so a probe never stalls the
/// writer; a walk that stops before the head says so (`capped`), and a match within the limit is
/// still found.
#[test]
fn msw11_a_nonce_lookup_since_a_head_stops_at_its_limit() {
    let (_d, mut r) = project();
    let commit = |r: &mut Rdy, name: String, nonce: String| {
        let req = req(json!([
            {"op": "create", "type": "doc", "id": "$d", "fields": {"name": name, "title": "D", "doc_type": "glossary"}}
        ]));
        let links = ChangeLinks {
            client_nonce: Some(nonce),
            ..Default::default()
        };
        r.write_linked(&agent(), req, links, None).unwrap();
    };
    commit(&mut r, "first".into(), "first".into());
    let since = r.read().view_head("work").unwrap().head.to_string();
    commit(&mut r, "mine".into(), "mine".into());
    for i in 0..10 {
        commit(&mut r, format!("other{i}"), format!("other-{i}"));
    }
    let st = r.read();
    let stopped = st.change_by_nonce_since("work", "mine", &since, 6).unwrap();
    assert_eq!(
        (stopped.change, stopped.capped),
        (None, true),
        "{stopped:?}"
    );
    assert!(stopped.read <= 6, "{stopped:?}");
    let newest = st
        .change_by_nonce_since("work", "other-9", &since, 6)
        .unwrap();
    assert!(newest.change.is_some() && !newest.capped, "{newest:?}");
    let all = st
        .change_by_nonce_since("work", "mine", &since, 10_000)
        .unwrap();
    assert!(all.change.is_some(), "{all:?}");
}

#[test]
fn hd3_the_op_tag_and_every_documented_field_still_parse() {
    let v = ops(json!([
        {"op": "create", "type": "claim", "id": "$c", "parent": "p", "after": "a", "fields": {"any": 1},
         "body": "b", "links": {"k": "t", "j": ["t"], "m": [{"target": "t", "part": "w"}, "u"]}},
        {"op": "create", "type": "claim", "before": "b"},
        {"op": "update", "id": "x", "base": "v", "fields": {}, "unset": ["f"], "body": "b",
         "links": {"k": [{"target": "t"}]}, "type": "note", "move_to": {"parent": "p", "after": "a"}},
        {"op": "move", "id": "x", "base": "v", "parent": "p", "before": "b"},
        {"op": "delete", "id": "x", "base": "v"},
        {"op": "rebalance"},
        {"op": "rebalance", "parent": "p"},
        {"op": "repin", "id": "x", "base": "v", "targets": ["t"]}
    ]));
    assert_eq!(v.len(), 8);
}

#[test]
fn hd3_parse_ops_names_the_op_and_the_nearest_field() {
    let e = parse_ops(vec![
        json!({"op": "rebalance"}),
        json!({"op": "delete", "id": "x", "bse": "v"}),
    ])
    .unwrap_err();
    assert_eq!(e.code, E_BAD_INPUT);
    assert_eq!(e.message, "op 1: unknown field \"bse\"");
    assert_eq!(e.fix, "did you mean \"base\"? the fields are id, base");
}

#[test]
fn hd3_nearest_is_the_closed_rule() {
    use rdy_engine::did_you_mean::nearest;
    assert_eq!(
        nearest("parent_id", &["id", "base", "parent", "after", "before"]),
        Some("parent")
    );
    assert_eq!(nearest("prent", &["id", "parent"]), Some("parent"));
    assert_eq!(nearest("arg", &["id", "op", "args"]), Some("args"));
    assert_eq!(nearest("zzz", &["id", "op", "args"]), None);
    assert_eq!(
        nearest("id_xyz", &["id"]),
        None,
        "a prefix under 3 characters never matches"
    );
}
