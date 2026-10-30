//! dq4: `rdy show` resolves a landing (D-HZYK0, O13): a proposal ref, `landing:`, `tag:`, a full
//! hash or a hash prefix names its target; anything else is a block, read as before.
use rdy_engine::show::ShowTarget;
use rdy_engine::{ProposalInfo, Rdy};
use rdy_model::Hash;
use rdy_qa::fixtures::*;
use rdy_store::kinds;
use serde_json::json;

fn claim(parent: &str, body: &str) -> serde_json::Value {
    json!([{"op": "create", "type": "claim", "parent": parent, "body": body}])
}

struct Fx {
    _d: tempfile::TempDir,
    r: Rdy,
    /// The doc, its section and its claim.
    ids: (String, String, String),
    /// The three landings, oldest first.
    landings: Vec<Hash>,
}

/// A project with three landings, tagged `v1.0.0` at the second.
fn three_landings() -> Fx {
    let (d, mut r) = project();
    let ids = seed_doc(&mut r, "seed");
    let mut landings = vec![land(&mut r, "seed").unwrap().parse().unwrap()];
    for (i, view) in ["one", "two"].into_iter().enumerate() {
        write(&mut r, view, claim(&ids.1, &format!("claim {view}"))).unwrap();
        landings.push(land(&mut r, view).unwrap().parse().unwrap());
        if i == 0 {
            r.tag(&approver(), "v1.0.0", None, None).unwrap();
        }
    }
    Fx {
        _d: d,
        r,
        ids,
        landings,
    }
}

/// A proposal that is open, and one that has landed (its ref stays).
fn proposed(fx: &mut Fx) -> ProposalInfo {
    write(&mut fx.r, "open", claim(&fx.ids.1, "open claim")).unwrap();
    fx.r.propose(
        &writer(),
        "open",
        "open",
        rdy_qa::intake::summary_for(&fx.r, "open"),
    )
    .unwrap()
}

#[test]
fn dq4_a_full_proposal_hash_still_shows_the_proposal() {
    let mut fx = three_landings();
    let p = proposed(&mut fx);
    assert_eq!(
        fx.r.read().show_target(&p.id).unwrap(),
        ShowTarget::Proposal(p.id.clone())
    );
    // a version prefix, and the same text in upper case
    assert_eq!(
        fx.r.read().show_target(&p.id[..12]).unwrap(),
        ShowTarget::Proposal(p.id.clone())
    );
    assert_eq!(
        fx.r.read().show_target(&p.id[..12].to_uppercase()).unwrap(),
        ShowTarget::Proposal(p.id.clone())
    );
}

#[test]
fn dq4_a_full_landing_hash_is_a_landing() {
    let fx = three_landings();
    for l in &fx.landings {
        assert_eq!(
            fx.r.read().show_target(&l.to_string()).unwrap(),
            ShowTarget::Landing(*l)
        );
        assert_eq!(
            fx.r.read().show_target(&l.to_string()[..10]).unwrap(),
            ShowTarget::Landing(*l)
        );
    }
}

#[test]
fn dq4_a_full_hash_of_another_kind_is_neither() {
    let mut fx = three_landings();
    let chunk =
        fx.r.store_mut()
            .write(vec![(kinds::BLOB_CHUNK, b"not a proposal".to_vec())])
            .unwrap()[0];
    for h in [chunk, Hash::of(b"a hash the store lacks")] {
        let e = fx.r.read().show_target(&h.to_string()).unwrap_err();
        assert_eq!(e.code, "E_BAD_INPUT", "{e}");
        assert_eq!(
            e.message,
            format!("{h} is not a proposal version or a landing")
        );
        assert_eq!(e.fix, "list landings with rdy log, or pass P-<id>@<n>");
    }
}

#[test]
fn dq4_landing_and_tag_forms_name_the_landing() {
    let fx = three_landings();
    let read = fx.r.read();
    let second = fx.landings[1];
    assert_eq!(
        read.show_target(&format!("landing:{second}")).unwrap(),
        ShowTarget::Landing(second)
    );
    assert_eq!(
        read.show_target(&format!("landing:{}", &second.to_string()[..8]))
            .unwrap(),
        ShowTarget::Landing(second)
    );
    assert_eq!(
        read.show_target("tag:v1.0.0").unwrap(),
        ShowTarget::Landing(second)
    );
    let e = read.show_target("tag:v9.9.9").unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
}

#[test]
fn dq4_a_proposal_ref_is_a_proposal() {
    let mut fx = three_landings();
    let p = proposed(&mut fx);
    let read = fx.r.read();
    assert_eq!(
        read.show_target(&p.r#ref).unwrap(),
        ShowTarget::Proposal(p.r#ref.clone())
    );
    let stable = p.r#ref.split('@').next().unwrap();
    assert_eq!(
        read.show_target(stable).unwrap(),
        ShowTarget::Proposal(stable.to_string())
    );
}

#[test]
fn dq4_ids_keys_and_paths_are_blocks() {
    let fx = three_landings();
    let read = fx.r.read();
    for text in [
        fx.ids.0.as_str(),
        fx.ids.1.as_str(),
        fx.ids.2.as_str(),
        "seed",
        "seed/s",
        // six hex letters that start no hash
        "facade",
        // too short to name a hash
        "abcde",
    ] {
        assert_eq!(read.show_target(text).unwrap(), ShowTarget::Block, "{text}");
    }
}

#[test]
fn dq4_log_row_equals_the_row_log_page_gives() {
    let fx = three_landings();
    let read = fx.r.read();
    let rows = read.log_rows().unwrap();
    assert_eq!(rows.len(), 3);
    for row in &rows {
        let id: Hash = row.landing.parse().unwrap();
        let one = read.log_row(&id).unwrap();
        assert_eq!(
            serde_json::to_value(&one).unwrap(),
            serde_json::to_value(row).unwrap()
        );
    }
    // the tag rides on its landing
    let tagged = read.log_row(&fx.landings[1]).unwrap();
    assert_eq!(tagged.tags, vec!["v1.0.0".to_string()]);
}

#[test]
fn dq4_log_row_of_a_hash_off_the_chain_is_refused() {
    let fx = three_landings();
    let off = Hash::of(b"off the chain");
    let e = fx.r.read().log_row(&off).unwrap_err();
    assert_eq!(e.code, "E_NOT_FOUND", "{e}");
    assert_eq!(
        e.message,
        format!(
            "no landing in canon's history matches {:?}",
            off.to_string()
        )
    );
    assert_eq!(e.fix, "list landings with rdy log");
}
