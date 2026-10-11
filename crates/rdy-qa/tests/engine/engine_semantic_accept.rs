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

fn content(r: &Rdy, id: &BlockId) -> Hash {
    let m = r.read().materialized(&Scope::Inflight).unwrap();
    m.get(id).unwrap().content_hash_with(r.store().tracking())
}

/// C edited in view a; agent:r judged the pair (B, C) a tension, advising blocking when
/// `advise` (recorded as `submit_verdict` records it); view a proposed. Returns the tension's
/// label and the proposal.
fn judged(advise: bool) -> (tempfile::TempDir, Rdy, Spec, String, ProposalInfo) {
    let (d, mut r) = project();
    let s = spec_doc(&mut r);
    put(&mut r, "a", upd(&s.c, "C contradicts B."));
    let mut items = vec![
        PairItem {
            block: s.b.clone(),
            content: content(&r, &s.b),
        },
        PairItem {
            block: s.c.clone(),
            content: content(&r, &s.c),
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
            block: s.b.clone(),
            note: "B and C cannot both hold".into(),
        }],
        provenance: Provenance {
            actor: "agent:r".into(),
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
    let tn = tension_label(&one(&r, TensionKind::Semantic).id);
    let p = r.propose(&writer(), "a", "a").unwrap();
    (d, r, s, tn, p)
}
/// An acceptance relayed from chat.
fn chat(proposal: &str) -> AcceptRequest {
    AcceptRequest {
        proposal: proposal.into(),
        evidence: Some("yes, accept the advice".into()),
        question: Some("Accept the reviewer's advice to block?".into()),
        session_ref: Some("chat-7".into()),
        ..Default::default()
    }
}
/// An acceptance a person gives at the CLI.
fn direct(proposal: &str) -> AcceptRequest {
    AcceptRequest {
        proposal: proposal.into(),
        ..Default::default()
    }
}

#[test]
fn se8_an_agent_accepts_only_by_relaying_a_persons_words() {
    let (_d, mut r, _s, tn, p) = judged(true);
    let e = r.accept_advice(&ag("w"), direct(&p.r#ref)).unwrap_err();
    assert_eq!(
        (e.code, e.message.as_str()),
        (
            E_PERMISSION,
            "an agent accepts advice only by relaying a person's words"
        )
    );
    let mut by = chat(&p.r#ref);
    by.relayed_by = Some("agent:w".into());
    assert_eq!(r.accept_advice(&ag("w"), by).unwrap_err().code, E_BAD_INPUT);
    assert_eq!(
        one(&r, TensionKind::Semantic).severity,
        TensionSeverity::Advisory,
        "nothing written"
    );
    let a = r.accept_advice(&ag("w"), chat(&p.r#ref)).unwrap();
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
    let mut half = direct(&p.r#ref);
    half.evidence = Some("yes".into());
    assert_eq!(
        r.accept_advice(&writer(), half).unwrap_err().message,
        "relayed_by and evidence go together"
    );
    let mut q = direct(&p.r#ref);
    q.question = Some("Accept?".into());
    assert_eq!(
        r.accept_advice(&writer(), q).unwrap_err().message,
        "question needs evidence"
    );
    r.accept_advice(&writer(), direct(&p.r#ref)).unwrap();
    let t = one(&r, TensionKind::Semantic);
    let acc = r.read().tension_info(&t).judged.unwrap().accepted.unwrap();
    assert_eq!((acc.actor.as_str(), acc.evidence), ("human:qa", None));
}

#[test]
fn se8_only_advised_tensions_of_the_named_version_are_accepted() {
    let (_d, mut r, s, tn, p) = judged(false);
    let e = r.accept_advice(&writer(), direct(&p.r#ref)).unwrap_err();
    assert_eq!(
        e.message,
        format!("{} has no advised semantic tension to accept", p.r#ref)
    );
    let mut named = direct(&p.r#ref);
    named.tensions = vec![tn.clone()];
    let e = r.accept_advice(&writer(), named).unwrap_err();
    assert_eq!(
        e.message,
        format!("{tn} is not an advised semantic tension of {}", p.r#ref)
    );
    let stable = p.r#ref.split('@').next().unwrap().to_string();
    let e = r.accept_advice(&writer(), direct(&stable)).unwrap_err();
    assert_eq!(
        e.message,
        "an acceptance names the exact proposal version the person was shown"
    );
    put(&mut r, "a", upd(&s.a, "A changed."));
    r.propose(&writer(), "a", "a").unwrap();
    assert_eq!(
        r.accept_advice(&writer(), direct(&p.r#ref))
            .unwrap_err()
            .code,
        E_PROPOSAL_MOVED
    );
}

#[test]
fn se8_accepted_and_dismissed_tensions_are_not_accepted_again() {
    let (_d, mut r, _s, _tn, p) = judged(true);
    let a = r.accept_advice(&writer(), direct(&p.r#ref)).unwrap();
    let id = a.accepted[0].id.clone();
    let mut again = direct(&p.r#ref);
    again.tensions = vec![id.clone()];
    assert_eq!(
        r.accept_advice(&writer(), again).unwrap_err().message,
        format!("{id} is accepted already")
    );
    let e = r.accept_advice(&writer(), direct(&p.r#ref)).unwrap_err();
    assert_eq!(
        e.message,
        format!("{} has no advised semantic tension to accept", p.r#ref)
    );
    // an advised tension someone dismissed is out of the set
    let (_d2, mut r2, _s2, tn2, p2) = judged(true);
    r2.dismiss_tension(&ag("x"), &tn2, "they agree after all")
        .unwrap();
    let e = r2.accept_advice(&writer(), direct(&p2.r#ref)).unwrap_err();
    assert_eq!(
        e.message,
        format!("{} has no advised semantic tension to accept", p2.r#ref)
    );
}

#[test]
fn se8_the_acceptance_holds_until_a_side_changes() {
    let (_d, mut r, s, _tn, p) = judged(true);
    r.accept_advice(&writer(), direct(&p.r#ref)).unwrap();
    put(&mut r, "a", upd(&s.c, "C agrees now."));
    assert!(of_kind(&r, TensionKind::Semantic).is_empty());
}

#[test]
fn se8_a_reviewer_of_another_actor_may_dismiss_an_accepted_tension() {
    let (_d, mut r, _s, _tn, p) = judged(true);
    let a = r.accept_advice(&writer(), direct(&p.r#ref)).unwrap();
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
    assert!(
        params(serde_json::json!({"accept": {"proposal": p.r#ref, "relayed_by": "agent:w"}}))
            .is_err()
    );
    let e = rdy_resolve(
        &mut st,
        params(
            serde_json::json!({"tension": tn, "accept": {"proposal": p.r#ref, "evidence": "yes"}}),
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
        params(serde_json::json!({"accept": {"proposal": p.r#ref}})).unwrap(),
    )
    .unwrap_err();
    assert_eq!(e.code, E_PERMISSION);
    let v = rdy_resolve(
        &mut st,
        params(serde_json::json!({"accept": {"proposal": p.r#ref, "evidence": " yes, block it ", "question": "Block on it?"}})).unwrap(),
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
