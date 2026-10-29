//! Review Focus 2 through the agent's own upload path (plan M2a1, kind1): an asset uploaded with
//! the exact bytes of a future tag, proposal or landing does not hide that object.
use rdy_model::*;
use rdy_qa::fixtures::*;
use rdy_qa::intake::TEST_SUMMARY;
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
        st.engine
            .read()
            .reader()
            .has_kind(kinds::BLOB_CHUNK, &Hash::of(bytes))
            .unwrap()
    );
}

fn agent_state() -> (tempfile::TempDir, rdy_mcp::tools::McpState) {
    let (dir, r) = project();
    let mut st =
        rdy_mcp::tools::McpState::new(std::sync::Arc::new(rdy_engine::engine::Local::new(r)));
    rdy_mcp::tools::rdy_session(
        &mut st,
        serde_json::from_value(json!({"actor": "agent:qa", "role": "writer"})).unwrap(),
    )
    .unwrap();
    (dir, st)
}

/// Runs `f` on the engine's writer.
fn on_writer(
    st: &rdy_mcp::tools::McpState,
    f: impl FnOnce(&mut rdy_engine::Rdy) -> Result<serde_json::Value, rdy_engine::RdyError>
    + Send
    + 'static,
) -> serde_json::Value {
    st.engine.write(Box::new(f)).unwrap()
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
    let landing: Hash = on_writer(&st, |r| {
        write(
            r,
            "work",
            json!([{"op": "create", "type": "doc", "fields": {"title": "T", "doc_type": "page"}}]),
        )
        .unwrap();
        Ok(json!(land(r, "work").unwrap()))
    })
    .as_str()
    .unwrap()
    .parse()
    .unwrap();
    let tag = Tag {
        name: "v1.0.0".into(),
        landing,
        message: None,
        tagged_by: by(Role::Approver),
    };
    upload(&mut st, &postcard::to_allocvec(&tag).unwrap());
    let t = tag.clone();
    let id: Hash = on_writer(&st, move |r| {
        let id = r.store_mut().put_tag(&t).unwrap();
        // a write outside the engine must be followed by invalidate() (snap1 precondition)
        r.invalidate();
        Ok(json!(id.to_string()))
    })
    .as_str()
    .unwrap()
    .parse()
    .unwrap();
    let read = st.engine.read();
    assert_eq!(read.reader().read_tag(&id).unwrap(), tag);
    assert_eq!(read.versions().unwrap().len(), 1);
}

#[test]
fn kind1_an_asset_with_a_future_proposals_bytes_does_not_hide_it() {
    let (_d, mut st) = agent_state();
    on_writer(&st, |r| {
        write(
            r,
            "work",
            json!([{"op": "create", "type": "doc", "fields": {"title": "P", "doc_type": "page"}}]),
        )
        .unwrap();
        Ok(json!(null))
    });
    let changes = st.engine.read().reader().view("work").unwrap().changes;
    let predicted = Proposal {
        stable: None,
        prev: None,
        title: "p".into(),
        view: "work".into(),
        changes,
        alternative: None,
        provenance: by(Role::Writer),
        canon_at: st.engine.read().canon_head(),
        summary: Some(TEST_SUMMARY.into()),
    };
    upload(&mut st, &postcard::to_allocvec(&predicted).unwrap());
    let p: Hash = on_writer(&st, |r| {
        let changes = r.store_mut().included("work").unwrap().as_ref().clone();
        let p = r
            .store_mut()
            .propose_changes(
                "work",
                "p",
                Some(TEST_SUMMARY),
                by(Role::Writer),
                changes,
                None,
            )
            .unwrap();
        // a write outside the engine must be followed by invalidate() (snap1 precondition)
        r.invalidate();
        // O10: the assessment propose writes, so the version is ready
        let pref = r.resolve_proposal(&p.to_string())?;
        let st = r.read();
        let v = st.reader().proposal(&p)?;
        let c = rdy_engine::readiness::Candidate::stored(&st, pref, v)?;
        let got = rdy_engine::readiness::assess(&st, &c, st.require_up_to_date())?;
        let a = got
            .assessment
            .unwrap_or_else(|| panic!("not ready: {:#?}", got.readiness.issues));
        drop(st);
        r.store_mut().put_assessment(&a).unwrap();
        r.invalidate();
        Ok(json!(p.to_string()))
    })
    .as_str()
    .unwrap()
    .parse()
    .unwrap();
    assert_eq!(p, predicted.id());
    on_writer(&st, move |r| {
        r.land(&approver(), &p.to_string()).unwrap();
        Ok(json!(null))
    });
}

#[test]
fn kind1_an_asset_with_a_future_landings_bytes_does_not_hide_it() {
    let (_d, mut st) = agent_state();
    let p: Hash = on_writer(&st, |r| {
        write(
            r,
            "work",
            json!([{"op": "create", "type": "doc", "fields": {"title": "L", "doc_type": "page"}}]),
        )
        .unwrap();
        let p = r
            .store_mut()
            .propose("work", "p", by(Role::Writer))
            .unwrap();
        Ok(json!(p.to_string()))
    })
    .as_str()
    .unwrap()
    .parse()
    .unwrap();
    let read = st.engine.read();
    let s = read.reader();
    let pr = s.proposal(&p).unwrap();
    let changes: Vec<Change> = pr.changes.iter().map(|c| s.change(c).unwrap()).collect();
    let state = s
        .merge_onto(s.canon_head().unwrap(), &s.canon_state().unwrap(), &changes)
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
        assessment: None,
        policy: None,
        read_identity: None,
    };
    upload(&mut st, &postcard::to_allocvec(&predicted).unwrap());
    drop(read);
    let l: Hash = on_writer(&st, move |r| {
        let l = r.store_mut().land(&p, by(Role::Approver)).unwrap();
        r.invalidate();
        Ok(json!(l.to_string()))
    })
    .as_str()
    .unwrap()
    .parse()
    .unwrap();
    assert_eq!(l, predicted.id());
    let read = st.engine.read();
    let scope = read.resolve_scope(&format!("landing:{l}")).unwrap();
    read.materialized(&scope).unwrap();
}
