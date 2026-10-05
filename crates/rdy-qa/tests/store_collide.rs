//! Review Focus 2 through the agent's own upload path (plan M2a1, kind1): an asset uploaded with
//! the exact bytes of a future tag, proposal or landing does not hide that object.
use rdy_model::*;
use rdy_qa::fixtures::*;
use rdy_store::kinds;
use serde_json::json;

fn b64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Uploads `bytes` as an agent would and returns the state (the asset's chunk now holds them).
fn upload(st: &mut rdy_mcp::tools::McpState, bytes: &[u8]) {
    rdy_mcp::tools::rdy_asset(
        st,
        serde_json::from_value(json!({"data_base64": b64(bytes)})).unwrap(),
    )
    .unwrap();
    assert!(
        st.rdy
            .store()
            .has_kind(kinds::BLOB_CHUNK, &Hash::of(bytes))
            .unwrap()
    );
}

fn agent_state() -> (tempfile::TempDir, rdy_mcp::tools::McpState) {
    let (dir, r) = project();
    let mut st = rdy_mcp::tools::McpState::new(r);
    rdy_mcp::tools::rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer"})).unwrap(),
    )
    .unwrap();
    (dir, st)
}

fn by(role: Role) -> Provenance {
    Provenance {
        actor: "human:qa".into(),
        run: None,
        role,
        at_ms: 42,
    }
}

#[test]
fn kind1_an_asset_with_a_future_tags_bytes_does_not_hide_the_tag() {
    let (_d, mut st) = agent_state();
    write(
        &mut st.rdy,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"title": "T", "doc_type": "glossary"}}]),
    )
    .unwrap();
    let landing: Hash = land(&mut st.rdy, "work").unwrap().parse().unwrap();
    let tag = Tag {
        name: "v1.0.0".into(),
        landing,
        message: None,
        tagged_by: by(Role::Approver),
    };
    upload(&mut st, &postcard::to_allocvec(&tag).unwrap());
    let id = st.rdy.store_mut().put_tag(&tag).unwrap();
    // a write outside the engine must be followed by invalidate() (snap1 precondition)
    st.rdy.invalidate();
    assert_eq!(st.rdy.store().read_tag(&id).unwrap(), tag);
    assert_eq!(st.rdy.read().versions().unwrap().len(), 1);
}

#[test]
fn kind1_an_asset_with_a_future_proposals_bytes_does_not_hide_it() {
    let (_d, mut st) = agent_state();
    write(
        &mut st.rdy,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"title": "P", "doc_type": "glossary"}}]),
    )
    .unwrap();
    let changes = st.rdy.store().view("work").unwrap().changes;
    let predicted = Proposal {
        stable: None,
        prev: None,
        title: "p".into(),
        view: "work".into(),
        changes,
        alternative: None,
        provenance: by(Role::Writer),
        canon_at: st.rdy.store().canon_head().unwrap(),
    };
    upload(&mut st, &postcard::to_allocvec(&predicted).unwrap());
    let p = st
        .rdy
        .store_mut()
        .propose("work", "p", by(Role::Writer))
        .unwrap();
    // a write outside the engine must be followed by invalidate() (snap1 precondition)
    st.rdy.invalidate();
    assert_eq!(p, predicted.id());
    st.rdy.land(&approver(), &p.to_string()).unwrap();
}

#[test]
fn kind1_an_asset_with_a_future_landings_bytes_does_not_hide_it() {
    let (_d, mut st) = agent_state();
    write(
        &mut st.rdy,
        "work",
        json!([{"op": "create", "type": "doc", "fields": {"title": "L", "doc_type": "glossary"}}]),
    )
    .unwrap();
    let p = st
        .rdy
        .store_mut()
        .propose("work", "p", by(Role::Writer))
        .unwrap();
    let s = st.rdy.store();
    let pr = s.proposal(&p).unwrap();
    let changes: Vec<Change> = pr.changes.iter().map(|c| s.change(c).unwrap()).collect();
    let state = s
        .merge_onto(&s.canon_state().unwrap(), &changes)
        .unwrap()
        .state;
    let predicted = LandingRecord {
        proposal: p,
        prev: s.canon_head().unwrap(),
        state: state.id(),
        approved_by: by(Role::Approver),
        changes: pr.changes.clone(),
        chosen: None,
        checks: vec![],
        assurance: Assurance::Local,
        relayed_by: None,
        relay_evidence: None,
        approved_version: Some(p),
        rebased_from: vec![],
        evidence: None,
    };
    upload(&mut st, &postcard::to_allocvec(&predicted).unwrap());
    let l = st.rdy.store_mut().land(&p, by(Role::Approver)).unwrap();
    assert_eq!(l, predicted.id());
    st.rdy.invalidate();
    let scope = st
        .rdy
        .read()
        .resolve_scope(&format!("landing:{l}"))
        .unwrap();
    st.rdy.read().materialized(&scope).unwrap();
}
