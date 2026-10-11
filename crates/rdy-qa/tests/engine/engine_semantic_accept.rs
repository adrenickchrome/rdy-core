//! se8: a person's acceptance of a reviewer's advice, relayed from chat or given at the CLI;
//! the acceptance holds until a side changes; listings and proposals show the advice.
use rdy_engine::tensions::api::AcceptRequest;
use rdy_engine::tensions::*;
use rdy_engine::{
    E_BAD_INPUT, E_PERMISSION, E_PROPOSAL_MOVED, ProposalInfo, Rdy, Scope, Session, proposal_text,
};
use rdy_model::*;
use rdy_qa::edit_loop::{Spec, spec_doc};
use rdy_qa::fixtures::*;
use rdy_qa::tensions::*;

/// Records `actor`'s `tension` verdict on blocks `x` and `y` at their contents in `scope`,
/// advising blocking when `advise` (recorded as `submit_verdict` records it).
fn verdict_on(r: &mut Rdy, scope: &Scope, x: &BlockId, y: &BlockId, actor: &str, advise: bool) {
    let m = r.read().materialized(scope).unwrap();
    let content = |id: &BlockId| m.get(id).unwrap().content_hash_with(r.store().tracking());
    let mut items = vec![
        PairItem {
            block: x.clone(),
            content: content(x),
        },
        PairItem {
            block: y.clone(),
            content: content(y),
        },
    ];
    items.sort();
    let key = pair_key(&items);
    let review = Review {
        subject: ReviewSubject::Pair {
            key,
            items: items.clone(),
        },
        case_hash: Some(Hash([9; 32])),
        verdict: Verdict::Tension,
        reason: String::new(),
        findings: vec![Finding {
            block: x.clone(),
            note: "B and C cannot both hold".into(),
        }],
        provenance: Provenance {
            actor: actor.into(),
            run: Some("r".into()),
            role: Role::Reviewer,
            at_ms: 1,
        },
    };
    let first = SemanticTension {
        key,
        items,
        review: Hash::of(&postcard::to_allocvec(&review).unwrap()),
        advice: advise.then(|| "it contradicts the gate rule".into()),
        blocking: false,
        relay: None,
        relayed_by: None,
        prev: None,
        provenance: review.provenance.clone(),
    };
    r.record_verdict(&review, Some(&first)).unwrap();
}

/// C edited in view a; agent:r judged the pair (B, C) a tension, advising blocking when
/// `advise` (recorded as `submit_verdict` records it); view a proposed. Returns the tension's
/// label and the proposal.
fn judged(advise: bool) -> (tempfile::TempDir, Rdy, Spec, String, ProposalInfo) {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts B."));
    verdict_on(&mut r, &Scope::Inflight, &s.b, &s.c, "agent:r", advise);
    let tn = tension_label(&one(&r, TensionKind::Semantic).id);
    let p = r.propose(&writer(), "a", "a").unwrap();
    (d, r, s, tn, p)
}
/// An acceptance relayed from chat, for the advised tensions `p` showed.
fn chat(p: &ProposalInfo) -> AcceptRequest {
    AcceptRequest {
        evidence: Some("yes, accept the advice".into()),
        question: Some("Accept the reviewer's advice to block?".into()),
        session_ref: Some("chat-7".into()),
        ..direct(p)
    }
}
/// An acceptance a person gives at the CLI, for the advised tensions `p` showed.
fn direct(p: &ProposalInfo) -> AcceptRequest {
    AcceptRequest {
        proposal: p.r#ref.clone(),
        advised_digest: p.advised_digest.clone().unwrap_or_default(),
        ..Default::default()
    }
}

#[test]
fn se8_an_agent_accepts_only_by_relaying_a_persons_words() {
    let (_d, mut r, _s, tn, p) = judged(true);
    let e = r.accept_advice(&ag("w"), direct(&p)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_PERMISSION,
            "an agent accepts advice only by relaying a person's words"
        )
    );
    let mut by = chat(&p);
    by.relayed_by = Some("agent:w".into());
    assert_eq!(r.accept_advice(&ag("w"), by).unwrap_err().code, E_BAD_INPUT);
    assert_eq!(
        one(&r, TensionKind::Semantic).severity,
        TensionSeverity::Advisory,
        "nothing written"
    );
    let a = r.accept_advice(&ag("w"), chat(&p)).unwrap();
    assert_eq!(a.accepted.len(), 1);
    assert_eq!(
        (a.accepted[0].was.as_str(), a.severity),
        (tn.as_str(), TensionSeverity::Blocking)
    );
    let t = one(&r, TensionKind::Semantic);
    assert_eq!(
        (tension_label(&t.id), t.severity),
        (a.accepted[0].id.clone(), TensionSeverity::Blocking)
    );
    let j = r.read().tension_info(&t).judged.unwrap();
    assert_eq!(
        (j.actor.as_str(), j.advice.as_deref()),
        ("agent:r", Some("it contradicts the gate rule"))
    );
    assert_eq!(j.findings[0].note, "B and C cannot both hold");
    let acc = j.accepted.unwrap();
    assert_eq!(acc.actor, "agent:w");
    assert_eq!(
        (acc.evidence.as_deref(), acc.session_ref.as_deref()),
        (Some("yes, accept the advice"), Some("chat-7"))
    );
}

#[test]
fn se8_a_person_at_the_cli_accepts_without_a_relay() {
    let (_d, mut r, _s, _tn, p) = judged(true);
    let mut half = direct(&p);
    half.evidence = Some("yes".into());
    assert_eq!(
        r.accept_advice(&writer(), half).unwrap_err().message,
        "relayed_by and evidence go together"
    );
    let mut q = direct(&p);
    q.question = Some("Accept?".into());
    assert_eq!(
        r.accept_advice(&writer(), q).unwrap_err().message,
        "question needs evidence"
    );
    r.accept_advice(&writer(), direct(&p)).unwrap();
    let t = one(&r, TensionKind::Semantic);
    let acc = r.read().tension_info(&t).judged.unwrap().accepted.unwrap();
    assert_eq!((acc.actor.as_str(), acc.evidence), ("human:qa", None));
}

#[test]
fn se8_only_advised_tensions_of_the_named_version_are_accepted() {
    let (_d, mut r, s, tn, p) = judged(false);
    let e = r.accept_advice(&writer(), direct(&p)).unwrap_err();
    assert_eq!(
        e.message,
        format!("{} has no advised semantic tension to accept", p.r#ref)
    );
    let mut named = direct(&p);
    named.tensions = vec![tn.clone()];
    let e = r.accept_advice(&writer(), named).unwrap_err();
    assert_eq!(
        e.message,
        format!("{tn} is not an advised semantic tension of {}", p.r#ref)
    );
    let stable = p.r#ref.split('@').next().unwrap().to_string();
    let e = r
        .accept_advice(
            &writer(),
            AcceptRequest {
                proposal: stable,
                ..direct(&p)
            },
        )
        .unwrap_err();
    assert_eq!(
        e.message,
        "an acceptance names the exact proposal version the person was shown"
    );
    put(&mut r, "a", upd(&s.a, "A changed."));
    r.propose(&writer(), "a", "a").unwrap();
    assert_eq!(
        r.accept_advice(&writer(), direct(&p)).unwrap_err().code,
        E_PROPOSAL_MOVED
    );
}

#[test]
fn se8_accepted_and_dismissed_tensions_are_not_accepted_again() {
    let (_d, mut r, _s, _tn, p) = judged(true);
    let a = r.accept_advice(&writer(), direct(&p)).unwrap();
    let id = a.accepted[0].id.clone();
    let mut again = direct(&p);
    again.tensions = vec![id.clone()];
    assert_eq!(
        r.accept_advice(&writer(), again).unwrap_err().message,
        format!("{id} is accepted already")
    );
    // FR1 finding 5: a retry of a committed acceptance says so
    let e = r.accept_advice(&writer(), direct(&p)).unwrap_err();
    assert_eq!(
        e.message,
        format!("{}: the advice is accepted already", p.r#ref)
    );
    // an advised tension someone dismissed is out of the set
    let (_d2, mut r2, _s2, tn2, p2) = judged(true);
    r2.dismiss_tension(&ag("x"), &tn2, "they agree after all")
        .unwrap();
    let e = r2.accept_advice(&writer(), direct(&p2)).unwrap_err();
    assert_eq!(
        e.message,
        format!("{} has no advised semantic tension to accept", p2.r#ref)
    );
}

#[test]
fn se8_the_acceptance_holds_until_a_side_changes() {
    let (_d, mut r, s, _tn, p) = judged(true);
    r.accept_advice(&writer(), direct(&p)).unwrap();
    put(&mut r, "a", upd(&s.c, "C agrees now."));
    assert!(of_kind(&r, TensionKind::Semantic).is_empty());
}

#[test]
fn se8_a_reviewer_of_another_actor_may_dismiss_an_accepted_tension() {
    let (_d, mut r, _s, _tn, p) = judged(true);
    let a = r.accept_advice(&writer(), direct(&p)).unwrap();
    let rv = Session::agent("agent:z".into(), Some("z".into()), Role::Reviewer).unwrap();
    r.dismiss_tension(&rv, &a.accepted[0].id, "checked: they do agree")
        .unwrap();
}

#[test]
fn se8_propose_and_show_carry_the_advice() {
    let (_d, r, _s, tn, p) = judged(true);
    let listed = p.tensions.iter().find(|t| t.id == tn).unwrap();
    assert_eq!(
        listed.judged.as_ref().unwrap().advice.as_deref(),
        Some("it contradicts the gate rule")
    );
    let show = r.read().proposal_show(&p.r#ref).unwrap();
    assert_eq!(show.advised.len(), 1);
    let row = &show.advised[0];
    assert_eq!(
        (row.tension.as_str(), row.reviewer.as_str(), row.accepted),
        (tn.as_str(), "agent:r", false)
    );
    assert_eq!(row.findings[0].note, "B and C cannot both hold");
    let text = proposal_text(&serde_json::to_value(&show).unwrap());
    assert!(
        text.contains(&format!(
            "advised: {tn} by agent:r: it contradicts the gate rule"
        )),
        "{text}"
    );
}

/// FW-UX1: the chat path. `rdy_resolve {accept}` goes alone; the MCP session is the relay, so it
/// needs the person's words and takes no relayed_by; the session reference defaults to mcp.
#[test]
fn se8_rdy_resolve_relays_an_acceptance_from_chat() {
    use rdy_mcp::params::parse_params;
    use rdy_mcp::tools::{McpState, ResolveParams, rdy_resolve, rdy_session};
    let (_d, r, _s, tn, p) = judged(true);
    let mut st = McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_session(
        &mut st,
        serde_json::from_value(
            serde_json::json!({"actor": "agent:w", "run": "w", "role": "writer"}),
        )
        .unwrap(),
    )
    .unwrap();
    let params = |v: serde_json::Value| {
        parse_params::<ResolveParams>("rdy_resolve", v.as_object().unwrap().clone())
    };
    let digest = p.advised_digest.clone().unwrap();
    assert!(
        params(serde_json::json!({"accept": {"proposal": p.r#ref, "advised_digest": digest, "relayed_by": "agent:w"}}))
            .is_err()
    );
    // FR1 (ruling 1): the digest of the advised tensions the person was shown is required
    assert!(
        params(serde_json::json!({"accept": {"proposal": p.r#ref, "evidence": "yes"}})).is_err()
    );
    let e = rdy_resolve(
        &mut st,
        params(
            serde_json::json!({"tension": tn, "accept": {"proposal": p.r#ref, "advised_digest": digest, "evidence": "yes"}}),
        )
        .unwrap(),
    )
    .unwrap_err();
    assert_eq!(
        e.message,
        "accept names its proposal and tensions itself; pass it alone"
    );
    let e = rdy_resolve(
        &mut st,
        params(serde_json::json!({"accept": {"proposal": p.r#ref, "advised_digest": digest}}))
            .unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
    let v = rdy_resolve(
        &mut st,
        params(serde_json::json!({"accept": {"proposal": p.r#ref, "advised_digest": digest, "evidence": " yes, block it ", "question": "Block on it?"}})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        (v["proposal"].as_str(), v["severity"].as_str()),
        (Some(p.r#ref.as_str()), Some("blocking"))
    );
    assert_eq!(v["accepted"][0]["was"], tn.as_str());
    let st = st.engine.read();
    let t = st
        .tensions()
        .unwrap()
        .iter()
        .find(|t| t.kind == TensionKind::Semantic)
        .cloned()
        .unwrap();
    let acc = st.tension_info(&t).judged.unwrap().accepted.unwrap();
    assert_eq!(
        (
            acc.actor.as_str(),
            acc.evidence.as_deref(),
            acc.question.as_deref(),
            acc.session_ref.as_deref(),
            acc.relayed_by
        ),
        (
            "agent:w",
            Some("yes, block it"),
            Some("Block on it?"),
            Some("mcp"),
            None
        )
    );
}

/// FR1 (ruling 1, D-WMCFW: the person answers once for the batch shown): the acceptance answers
/// for the advised tensions the person was shown. A verdict recorded after the propose changes
/// the set, so the acceptance refuses and writes nothing; shown again, it takes the set listed now.
#[test]
fn se8_a_verdict_recorded_after_the_propose_makes_the_acceptance_refuse() {
    let (_d, mut r, s, tn, p) = judged(true);
    assert!(p.advised_digest.is_some());
    verdict_on(&mut r, &Scope::Inflight, &s.a, &s.c, "agent:q", true);
    let e = r.accept_advice(&writer(), direct(&p)).unwrap_err();
    assert_eq!(
        (e.code, e.message.clone(), e.fix.clone()),
        (
            E_PROPOSAL_MOVED,
            format!(
                "the advised tensions of {} changed since they were shown",
                p.r#ref
            ),
            format!(
                "show {} again and answer for the tensions it lists now",
                p.r#ref
            )
        )
    );
    let mut named = direct(&p);
    named.tensions = vec![tn];
    assert_eq!(
        r.accept_advice(&writer(), named).unwrap_err().code,
        E_PROPOSAL_MOVED
    );
    assert!(
        of_kind(&r, TensionKind::Semantic)
            .iter()
            .all(|t| t.severity == TensionSeverity::Advisory),
        "nothing written"
    );
    let show = r.read().proposal_show(&p.r#ref).unwrap();
    assert_eq!(show.advised.len(), 2);
    assert_ne!(show.advised_digest, p.advised_digest);
    let a = r
        .accept_advice(
            &writer(),
            AcceptRequest {
                advised_digest: show.advised_digest.clone().unwrap(),
                ..direct(&p)
            },
        )
        .unwrap();
    assert_eq!(a.accepted.len(), 2);
}

/// FR1 (ruling 1): past PROPOSAL_TENSIONS_MAX the propose report lists 50 tensions and counts
/// the advised ones it leaves out; the digest covers every advised tension, as `rdy show` lists
/// them, and the acceptance takes them all.
#[test]
fn se8_past_the_proposal_cut_the_digest_covers_every_advised_tension() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    let ops: Vec<serde_json::Value> = (0..55)
        .map(|i| {
            serde_json::json!({"op": "create", "type": "claim", "id": format!("$x{i}"),
                "parent": s.section.to_string(), "body": format!("Rule {i} holds for item {i}.")})
        })
        .collect();
    let w = write(&mut r, "work", serde_json::Value::Array(ops)).unwrap();
    land(&mut r, "work").unwrap();
    put(&mut r, "a", upd(&s.c, "C contradicts B."));
    for i in 0..55 {
        let x = created(&w, &format!("$x{i}"));
        verdict_on(&mut r, &Scope::Inflight, &x, &s.c, "agent:r", true);
    }
    let p = r.propose(&writer(), "a", "a").unwrap();
    assert_eq!(p.tensions.len(), rdy_engine::PROPOSAL_TENSIONS_MAX);
    let listed = p
        .tensions
        .iter()
        .filter(|t| t.judged.as_ref().is_some_and(|j| j.advice.is_some()))
        .count();
    assert!(p.advised_omitted > 0);
    assert_eq!(listed + p.advised_omitted, 55);
    let show = r.read().proposal_show(&p.r#ref).unwrap();
    assert_eq!(show.advised.len(), 55);
    assert_eq!(show.advised_digest, p.advised_digest);
    let a = r.accept_advice(&writer(), direct(&p)).unwrap();
    assert_eq!(a.accepted.len(), 55);
}

/// FR1 (ruling 2, founder: "Same texts, same answer"): the acceptance binds to the pair's two
/// texts. Views a and b write the same text to C; accepting on P(a) makes view b's form block
/// too, and the reply lists every tension that now blocks, with its view and proposal.
#[test]
fn se8_an_acceptance_binds_to_the_two_texts_in_every_view() {
    let (_d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts B."));
    put(&mut r, "b", upd(&s.c, "C contradicts B."));
    verdict_on(
        &mut r,
        &Scope::View("a".into()),
        &s.b,
        &s.c,
        "agent:r",
        true,
    );
    assert_eq!(of_kind(&r, TensionKind::Semantic).len(), 2);
    let p = r.propose(&writer(), "a", "a").unwrap();
    let a = r.accept_advice(&writer(), direct(&p)).unwrap();
    assert_eq!(a.accepted.len(), 1);
    let rows: Vec<(&str, Option<&str>)> = a
        .now_blocking
        .iter()
        .map(|b| (b.view.as_str(), b.proposal.as_deref()))
        .collect();
    assert_eq!(rows, vec![("a", Some(p.r#ref.as_str())), ("b", None)]);
    let now: std::collections::BTreeSet<String> = of_kind(&r, TensionKind::Semantic)
        .iter()
        .map(|t| tension_label(&t.id))
        .collect();
    let listed: std::collections::BTreeSet<String> =
        a.now_blocking.iter().map(|b| b.tension.clone()).collect();
    assert_eq!(listed, now);
    let pb = r.propose(&writer(), "b", "b").unwrap();
    assert!(
        pb.tensions
            .iter()
            .any(|t| t.kind == TensionKind::Semantic && t.refuses == Some(true)),
        "{:#?}",
        pb.tensions
    );
}

/// FR1 (finding 4): session_ref and relayed_by take at most 256 characters, as a view name does.
#[test]
fn se8_session_ref_and_relayed_by_take_at_most_256_characters() {
    let (_d, mut r, _s, _tn, p) = judged(true);
    let mut long = chat(&p);
    long.session_ref = Some("s".repeat(257));
    let e = r.accept_advice(&ag("w"), long).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            "session_ref takes at most 256 characters",
            "use a short session id, not the conversation"
        )
    );
    let mut by = direct(&p);
    by.evidence = Some("yes".into());
    by.relayed_by = Some(format!("agent:{}", "w".repeat(251)));
    let e = r.accept_advice(&writer(), by).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (E_BAD_INPUT, "relayed_by takes at most 256 characters")
    );
    let mut ok = chat(&p);
    ok.session_ref = Some("s".repeat(256));
    r.accept_advice(&ag("w"), ok).unwrap();
    let (_d2, mut r2, _s2, _tn2, p2) = judged(true);
    let mut by = direct(&p2);
    by.evidence = Some("yes".into());
    by.relayed_by = Some(format!("agent:{}", "w".repeat(250)));
    r2.accept_advice(&writer(), by).unwrap();
}

/// FR1 (finding 5): an acceptance changes the tension's id (the severity is in it). A retry of a
/// committed acceptance, whose reply was lost, says the advice is accepted already, by the old
/// label or for the whole version; the shown set's digest does not change.
#[test]
fn se8_a_retry_after_an_acceptance_says_it_is_accepted_already() {
    let (_d, mut r, _s, tn, p) = judged(true);
    let mut named = direct(&p);
    named.tensions = vec![tn.clone()];
    r.accept_advice(&writer(), named.clone()).unwrap();
    let e = r.accept_advice(&writer(), named).unwrap_err();
    assert_eq!(
        (e.code, e.message.clone(), e.fix.as_str()),
        (
            E_BAD_INPUT,
            format!("{tn} is accepted already"),
            "leave it out"
        )
    );
    let e = r.accept_advice(&writer(), direct(&p)).unwrap_err();
    assert_eq!(
        e.message,
        format!("{}: the advice is accepted already", p.r#ref)
    );
    assert_eq!(
        r.read().proposal_show(&p.r#ref).unwrap().advised_digest,
        p.advised_digest
    );
}
