//! Places a change-plan version that no engine path writes yet (a bound or a sealed one) for a
//! read test: its PLAN frame and its `plan/<view>` ref, in one batch, through the store's raw
//! test hook (`Store::write_guarded_for_tests`), so no plan check runs. Production writes plans
//! only through the typed plan writer and the proposal and landing composition.
use rdy_model::Hash;
use rdy_model::plan::Plan;
use rdy_model::redaction::encode_into;
use rdy_store::Store;
use rdy_store::kinds::{self, RefUpdate};
use rdy_store::plan::plan_ref;

/// The canonical bytes of `v`.
fn enc<T: serde::Serialize>(v: &T) -> Vec<u8> {
    let mut buf = vec![0u8; rdy_model::plan::MAX_PLAN_BYTES];
    encode_into(v, &mut buf).unwrap().to_vec()
}

/// Writes `plan` and the ref naming it, with no plan check; returns its id.
pub fn put_plan_frames(s: &mut Store, plan: &Plan) -> Hash {
    let id = plan.id();
    let r = RefUpdate {
        name: plan_ref(&plan.view),
        value: id.0.to_vec(),
        at: s.log_end(),
    };
    s.write_guarded_for_tests(vec![(kinds::PLAN, enc(plan)), (kinds::REF_UPDATE, enc(&r))])
        .unwrap();
    id
}
