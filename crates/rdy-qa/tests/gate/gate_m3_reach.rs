//! mg9 (D-2GT0Z, D-JQD5X, D-DSPMW, D-AJ0XQ, D-ME2HD): the reach of a version's blocking rules
//! holds every input the rules read, so a competing landing between the version's assessment and
//! its approval that changes such an input stales the assessment, and the stale answer creates
//! no landing. Each test reproduces one freshness row of the known-issues log (160, 161, 162,
//! 166): a version is assessed ready, a competing version lands, then the first is approved.
//! Nothing reruns a rule at landing (D-JQD5X); the refusal must come from the stored reach.
//!
//! Independent evidence: the M3 audit (`rdy_qa::m3_audit`, Task 1) reads the stored landing
//! between held snapshots, and M3f's canon query evaluates the blocking rule on canon. A landing
//! that leaves stable canon failing a blocking rule is the defect, whatever the audit says.
use rdy_engine::*;
use rdy_qa::fixtures::*;
use rdy_qa::intake::{TEST_SUMMARY, answer_open_items, confirm_before_landing};
use rdy_qa::m3_audit::{ObservedLanding, audit_m3, check_m3};
use rdy_qa::tensions::{ag, j_typo, put_j};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The pack every test uses: claims and sections with an `ok` and a `checked` field, then
/// `rules`.
fn pack(rules: &str) -> String {
    format!(
        "\
name: mg9
version: \"1\"
extends: rdy-standard
block_types:
  section:
    prefix: s
    traits: [structural]
    fields:
      title: {{ kind: str, required: true }}
      lifecycle: {{ kind: enum, values: [draft, stable, deprecated] }}
      ok: {{ kind: bool }}
    children: [section, prose, claim, requirement, constraint, term, component, actor, interface, value, entity, flow, example, note, rationale, table, diagram, figure, code, decision, question]
  claim:
    prefix: c
    traits: [normative, describes]
    body: required
    fields:
      ok: {{ kind: bool }}
      checked: {{ kind: bool }}
rules:
{rules}"
    )
}

/// The gate config of every test: the coverage gate is off (row 161 is about pack rules, not
/// the no-lower gate), and a moved base refuses only inside the reach.
const EXTRA: &str = "coverage:\n  gate: off\nreviews:\n  require_up_to_date: false\n";

fn rule(name: &str, select: &str, require: &str) -> String {
    format!(
        "  - name: {name}\n    select: {select}\n    require: {require}\n    severity: blocking\n    message: {name} message\n    fix: {name} fix\n"
    )
}

fn mg9_project(rules: &str) -> (tempfile::TempDir, Rdy) {
    let (d, r) = project();
    drop(r);
    rdy_qa::packs::use_pack(d.path(), "mg9", &pack(rules), EXTRA);
    let r = Rdy::open(d.path()).unwrap();
    (d, r)
}

fn propose_as(r: &mut Rdy, view: &str) -> ProposalInfo {
    answer_open_items(r, &ag(view), view);
    r.propose(&ag(view), view, view, Some(TEST_SUMMARY))
        .unwrap_or_else(|e| panic!("propose {view}: {e:#?}"))
}

fn approve(r: &mut Rdy, p: &ProposalInfo) -> Result<LandInfo, RdyError> {
    confirm_before_landing(r, &approver(), p);
    r.land_report(&approver(), &p.id)
}

/// The stable blocks of canon that fail blocking rule `select` / `require`, by M3f's own query.
fn canon_failures(r: &Rdy, select: &str, require: &str) -> BTreeSet<String> {
    r.read()
        .query(
            &Scope::Canon,
            &format!("({select}) and not ({require}) and lifecycle = stable"),
        )
        .unwrap()
        .matches
        .into_iter()
        .map(|m| m.id)
        .collect()
}

/// The race of every test. `first` was assessed ready; `second`, assessed after it, lands; then
/// the person approves `first`. The stale approval must refuse with `E_PROPOSAL_MOVED` and leave
/// canon where `second` put it. `row` names the known-issues row; `select`/`require` is the
/// blocking rule the late landing would break.
fn race(
    r: &mut Rdy,
    row: u32,
    first: &ProposalInfo,
    second: &str,
    (select, require): (&str, &str),
) {
    assert!(first.ready, "row {row}: {:#?}", first.issues);
    // a tension query also reads the open views' changes as sides, so the in-flight pair of row
    // 166 shows before either lands; only the final canon, with no view left, is judged there
    let judge_now = !require.contains("tension");
    assert!(
        !judge_now || canon_failures(r, select, require).is_empty(),
        "row {row}: canon starts clean: {:?}",
        canon_failures(r, select, require)
    );
    let p2 = propose_as(r, second);
    assert!(p2.ready, "row {row}: {:#?}", p2.issues);
    let l2 = approve(r, &p2).unwrap_or_else(|e| panic!("row {row}: the competitor lands: {e:#?}"));
    assert!(
        !judge_now || canon_failures(r, select, require).is_empty(),
        "row {row}: the competitor alone leaves canon clean"
    );
    let before = r.read();
    let head = before.canon_head();
    match approve(r, first) {
        Err(e) => {
            assert_eq!(e.code, E_PROPOSAL_MOVED, "row {row}: {e:#?}");
            assert!(
                e.message.starts_with(&format!(
                    "proposal {} moved: landing {} changed ",
                    first.r#ref,
                    &l2.landing[..12]
                )),
                "row {row}: {}",
                e.message
            );
            assert_eq!(r.read().canon_head(), head, "row {row}: nothing landed");
            assert!(!judge_now || canon_failures(r, select, require).is_empty());
            assert_eq!(check_m3(r), vec![], "row {row}");
        }
        Ok(l) => {
            let after = r.read();
            let observed = ObservedLanding {
                landing: l.landing.parse().unwrap(),
                shown: None,
            };
            let audit = audit_m3(&before, &after, &observed);
            panic!(
                "KNOWN-ISSUES row {row} reproduces: the stale approval of {} landed as {} after \
                 competing landing {}; stable canon now fails the blocking rule on {:?}; the M3 \
                 audit of the landing (stored reach {:?}) reports {:#?}",
                first.r#ref,
                &l.landing[..12],
                &l2.landing[..12],
                canon_failures(r, select, require),
                stored_reach(r, first),
                audit
            );
        }
    }
}

fn stored_reach(r: &Rdy, p: &ProposalInfo) -> Vec<String> {
    let st = r.read();
    let h = st
        .reader()
        .assessment_of_proposal(&p.id.parse().unwrap(), st.seq())
        .unwrap()
        .unwrap();
    st.reader()
        .assessment(&h)
        .unwrap()
        .reach
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// A doc `notes` with section `s` and the claims `bodies`, landed; `stable` makes the doc
/// stable. The ids of the doc, the section and the claims.
fn seed(r: &mut Rdy, stable: bool, claims: &[(&str, Value)]) -> (String, String, Vec<String>) {
    let mut doc = json!({"name": "notes", "title": "Notes", "doc_type": "overview"});
    if stable {
        doc["lifecycle"] = json!("stable");
    }
    let mut ops = vec![
        json!({"op": "create", "type": "doc", "id": "$d", "fields": doc}),
        json!({"op": "create", "type": "section", "id": "$s", "parent": "$d", "fields": {"title": "Ideas", "ok": true}}),
    ];
    for (k, (body, fields)) in claims.iter().enumerate() {
        ops.push(json!({"op": "create", "type": "claim", "id": format!("$c{k}"), "parent": "$s", "body": body, "fields": fields}));
    }
    let w = put_j(r, "seed", Value::Array(ops), j_typo());
    land(r, "seed").unwrap();
    let ids = (0..claims.len())
        .map(|k| w.created[&format!("$c{k}")].clone())
        .collect();
    (w.created["$d"].clone(), w.created["$s"].clone(), ids)
}

#[test]
fn mg9_rule_reach_includes_lifecycle_ancestors() {
    // row 160: the blocking rule names no lifecycle field, but its severity reads the claim's
    // effective lifecycle, inherited from the doc. The claim fails the rule while the doc is a
    // draft (a warning, ready); a competing version makes the doc stable.
    let (select, require) = ("type = claim", "fields.ok = true");
    let (_d, mut r) = mg9_project(&rule("ok-claims", select, require));
    let (doc, _, c) = seed(&mut r, false, &[("C holds.", json!({"ok": true}))]);
    put_j(
        &mut r,
        "a1",
        json!([{"op": "update", "id": c[0], "fields": {"ok": false}}]),
        j_typo(),
    );
    let p1 = propose_as(&mut r, "a1");
    assert!(
        p1.warnings.iter().any(|w| w.code == W_RULE_CHECK),
        "{:#?}",
        p1.warnings
    );
    put_j(
        &mut r,
        "a2",
        json!([{"op": "update", "id": doc, "fields": {"name": "notes", "title": "Notes", "doc_type": "overview", "lifecycle": "stable"}}]),
        j_typo(),
    );
    race(&mut r, 160, &p1, "a2", (select, require));
}

/// The coverage score M3f's canon query gives block `id` (0 to 100).
fn coverage_of(r: &Rdy, id: &str) -> u32 {
    (0..=100u32)
        .find(|k| {
            r.read()
                .query(&Scope::Canon, &format!("id = {id} and coverage = {k}"))
                .unwrap()
                .matches
                .len()
                == 1
        })
        .unwrap_or_else(|| panic!("{id} has a coverage score"))
}

#[test]
fn mg9_rule_reach_includes_coverage_inputs() {
    // row 161: a section's coverage reads its children (a section with no content child is a
    // stub). Two versions each delete one of the two claims of a stable section; each alone
    // keeps the section's score, both together lower it under the blocking rule's floor.
    let claims = [
        ("C1 holds.", json!({"checked": true})),
        ("C2 holds.", json!({"checked": true})),
    ];
    // a probe of the same canon with no rule: the scores before and after both deletions
    let (_pd, mut probe) = mg9_project("  []\n");
    let (_, ps, pc) = seed(&mut probe, true, &claims);
    let full = coverage_of(&probe, &ps);
    put_j(
        &mut probe,
        "both",
        json!([{"op": "delete", "id": pc[0]}, {"op": "delete", "id": pc[1]}]),
        j_typo(),
    );
    land(&mut probe, "both").unwrap();
    let stub = coverage_of(&probe, &ps);
    assert!(stub < full, "a stub section scores lower: {stub} < {full}");
    let select = "type = section and fields.ok = true";
    let require = format!("coverage > {stub}");
    let (_d, mut r) = mg9_project(&rule("covered", select, &require));
    let (_, s, c) = seed(&mut r, true, &claims);
    assert_eq!(coverage_of(&r, &s), full);
    put_j(
        &mut r,
        "a1",
        json!([{"op": "delete", "id": c[0]}]),
        j_typo(),
    );
    let p1 = propose_as(&mut r, "a1");
    put_j(
        &mut r,
        "a2",
        json!([{"op": "delete", "id": c[1]}]),
        j_typo(),
    );
    race(&mut r, 161, &p1, "a2", (select, &require));
}

#[test]
fn mg9_rule_reach_detects_a_new_key_holder() {
    // row 162: a doc's key gets a `-2` suffix when another doc already holds it. A version adds a
    // stable doc whose key is free at its assessment; a competing version lands a doc with the
    // same key first, so the late landing would give the version's doc the `-2` key that a
    // blocking rule refuses.
    let (kd, mut kr) = mg9_project("  []\n");
    let mine = kr
        .new_doc(
            &ag("k2"),
            "k2",
            "product-handbook/onboarding/overview",
            None,
            Some("O"),
        )
        .unwrap();
    let (doc_id, key) = {
        let st = kr.read();
        let m = st.materialized(&Scope::View("k2".into())).unwrap();
        let d = m
            .iter()
            .find(|b| b.block_type == "doc" && b.field_str("title") == Some("O"))
            .unwrap_or_else(|| panic!("the new doc: {mine:?}"));
        (d.id.to_string(), d.field_str("key").unwrap().to_string())
    };
    put_j(
        &mut kr,
        "k2",
        json!([{"op": "update", "id": doc_id, "fields": {"lifecycle": "stable"}}]),
        j_typo(),
    );
    kr.new_doc(
        &ag("k1"),
        "k1",
        "engineering-handbook/onboarding/overview",
        None,
        Some("O"),
    )
    .unwrap();
    kr.close().unwrap();
    let select = format!("type = doc and key = \"{key}-2\"");
    let require = "fields.title = Nope";
    rdy_qa::packs::use_pack(
        kd.path(),
        "mg9",
        &pack(&rule("second-key", &select, require)),
        EXTRA,
    );
    let mut kr = Rdy::open(kd.path()).unwrap();
    let p1 = propose_as(&mut kr, "k2");
    assert_eq!(
        kr.read()
            .materialized(&Scope::View("k2".into()))
            .unwrap()
            .get(&rdy_model::BlockId::parse(&doc_id).unwrap())
            .unwrap()
            .field_str("lifecycle"),
        Some("stable")
    );
    race(&mut kr, 162, &p1, "k1", (&select, require));
}

#[test]
#[ignore = "KNOWN-ISSUES row 166 (L3/I6): below the fix bar; open M3 gap, exit checklist; reproduces a race, so it fails only some runs"]
fn mg9_rule_reach_includes_unlinked_duplicate_partners() {
    // row 166: a tension rule reads duplicate partners, which no link joins. Two versions each
    // give one of two unlinked stable claims, in two docs, the same text; each alone leaves no
    // duplicate. (Two claims of one section reach each other through their parent.)
    let (select, require) = ("type = claim", "tension = false");
    let (_d, mut r) = mg9_project(&rule("calm-claims", select, require));
    let w = put_j(
        &mut r,
        "seed",
        json!([
            {"op": "create", "type": "doc", "id": "$d1", "fields": {"name": "logos", "title": "Logos", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s1", "parent": "$d1", "fields": {"title": "Colors"}},
            {"op": "create", "type": "claim", "id": "$c0", "parent": "$s1", "body": "The logo is blue."},
            {"op": "create", "type": "doc", "id": "$d2", "fields": {"name": "pages", "title": "Pages", "doc_type": "overview", "lifecycle": "stable"}},
            {"op": "create", "type": "section", "id": "$s2", "parent": "$d2", "fields": {"title": "Output"}},
            {"op": "create", "type": "claim", "id": "$c1", "parent": "$s2", "body": "Pages render as HTML."}
        ]),
        j_typo(),
    );
    land(&mut r, "seed").unwrap();
    let c = [w.created["$c0"].clone(), w.created["$c1"].clone()];
    let same = "Every invoice is paid within thirty days.";
    put_j(
        &mut r,
        "a1",
        json!([{"op": "update", "id": c[0], "body": same}]),
        j_typo(),
    );
    let p1 = propose_as(&mut r, "a1");
    put_j(
        &mut r,
        "a2",
        json!([{"op": "update", "id": c[1], "body": same}]),
        j_typo(),
    );
    race(&mut r, 166, &p1, "a2", (select, require));
}

/// The card facts the stored assessment of `p` keeps, as `<name>: <value>` lines.
fn stored_facts(r: &Rdy, p: &ProposalInfo) -> Vec<String> {
    let st = r.read();
    let h = st
        .reader()
        .assessment_of_proposal(&p.id.parse().unwrap(), st.seq())
        .unwrap()
        .unwrap_or_else(|| panic!("{} stores an assessment", p.r#ref));
    st.reader().assessment(&h).unwrap().card.facts
}

/// The approve card of `p` that the pending queue shows: its fact names and its hash.
fn shown_card(r: &Rdy, p: &ProposalInfo) -> (Vec<String>, String) {
    let item = r
        .read()
        .pending(&rdy_engine::pending::PendingQuery {
            person: rdy_qa::intake::PERSON.into(),
            session_ref: None,
            max_tokens: rdy_engine::pending::CARD_MAX_TOKENS,
        })
        .unwrap()
        .items
        .into_iter()
        .find(|i| i.r#ref == p.r#ref)
        .unwrap_or_else(|| panic!("{} has an approve card", p.r#ref));
    (
        item.facts.iter().map(|f| f.name.to_string()).collect(),
        item.card_hash,
    )
}

#[test]
fn ab10_a_key_that_lands_as_written_adds_no_keys_fact() {
    // TASK-1277: the `Keys` fact is present only when the landing projection assigns a key the
    // version's own changes do not write. A doc whose key is free, and a doc whose clash propose
    // already re-keyed (its view writes the `-2` key), both land as written: no fact, the card's
    // facts are today's, and the landing compares against that and lands.
    let names = |facts: &[String]| -> Vec<String> {
        facts
            .iter()
            .map(|f| f.split_once(": ").map_or(f.clone(), |(n, _)| n.to_string()))
            .collect()
    };
    let today = [
        "Changes",
        "Justification",
        "Decisions",
        "Supersedes",
        "Re-affirms",
        "Reversibility",
        "Consequences",
        "Change plan",
        "Tensions",
        "Coverage",
        "Confidence",
        "Base",
        "Exceptions",
        "Policy",
    ];
    for rules in [
        rule("titled", "type = doc", "fields.title = O"),
        "  []\n".to_string(),
    ] {
        let (_d, mut r) = mg9_project(&rules);
        // k1's doc takes the key first, so k2's propose re-keys its doc to `-2`
        for (view, path) in [
            ("k1", "engineering-handbook/onboarding/overview"),
            ("k2", "product-handbook/onboarding/overview"),
        ] {
            r.new_doc(&ag(view), view, path, None, Some("O")).unwrap();
        }
        let p1 = propose_as(&mut r, "k1");
        approve(&mut r, &p1).unwrap_or_else(|e| panic!("{rules}: k1 lands: {e:#?}"));
        let p2 = propose_as(&mut r, "k2");
        assert!(p2.ready, "{rules}: {:#?}", p2.issues);
        assert_eq!(p2.keys.len(), 1, "{rules}: propose re-keys: {:?}", p2.keys);
        let facts = stored_facts(&r, &p2);
        assert_eq!(names(&facts), today, "{rules}: {facts:#?}");
        let (shown, hash) = shown_card(&r, &p2);
        assert_eq!(shown, today, "{rules}: the shown card is the stored one");
        let st = r.read();
        let h = st
            .reader()
            .assessment_of_proposal(&p2.id.parse().unwrap(), st.seq())
            .unwrap()
            .unwrap();
        assert_eq!(
            st.reader().assessment(&h).unwrap().card.hash.to_string(),
            hash,
            "{rules}: the shown hash is the stored one"
        );
        let l = approve(&mut r, &p2).unwrap_or_else(|e| panic!("{rules}: k2 lands: {e:#?}"));
        assert!(l.keys.is_empty(), "{rules}: lands as written: {:?}", l.keys);
    }
}

#[test]
fn ab10_a_key_a_competing_landing_shifts_refuses_once_with_a_keys_fact() {
    // TASK-1265, TASK-1277 with no pack rule: k2's doc key is free when it is proposed; k1 lands
    // a doc with the same key first. The approval of k2 refuses with the key in the message and
    // a fresh card whose `Keys` fact, right after Decisions, shows the `-2` key; the next
    // approval lands it.
    let (_d, mut r) = mg9_project("  []\n");
    let mut docs = vec![];
    for (view, path) in [
        ("k2", "product-handbook/onboarding/overview"),
        ("k1", "engineering-handbook/onboarding/overview"),
    ] {
        r.new_doc(&ag(view), view, path, None, Some("O")).unwrap();
        let st = r.read();
        let m = st.materialized(&Scope::View(view.into())).unwrap();
        let d = m
            .iter()
            .find(|b| b.block_type == "doc" && b.field_str("title") == Some("O"))
            .unwrap();
        docs.push((d.id.to_string(), d.field_str("key").unwrap().to_string()));
    }
    let (mine, key) = docs[0].clone();
    let p2 = propose_as(&mut r, "k2");
    assert!(p2.keys.is_empty(), "the key is free: {:?}", p2.keys);
    let p1 = propose_as(&mut r, "k1");
    let l1 = approve(&mut r, &p1).unwrap();
    let head = r.read().canon_head();
    let e = approve(&mut r, &p2).unwrap_err();
    assert_eq!(e.code, E_PROPOSAL_MOVED, "{e:#?}");
    assert_eq!(
        e.message,
        format!(
            "proposal {} moved: landing {} changed the key of {mine} ({key} to {key}-2) after its assessment",
            p2.r#ref,
            &l1.landing[..12]
        )
    );
    assert_eq!(
        e.fix,
        format!(
            "review the fresh card in corrected and approve {} again",
            p2.r#ref
        )
    );
    assert_eq!(r.read().canon_head(), head, "nothing landed");
    let facts = stored_facts(&r, &p2);
    let at = facts
        .iter()
        .position(|f| f.starts_with("Decisions: "))
        .unwrap();
    assert_eq!(facts[at + 1], format!("Keys: {mine} {key}-2"), "{facts:#?}");
    let l2 = approve(&mut r, &p2).unwrap_or_else(|e| panic!("the fresh card lands: {e:#?}"));
    assert_eq!(
        l2.keys.get(&mine),
        Some(&format!("{key}-2")),
        "{:?}",
        l2.keys
    );
}
