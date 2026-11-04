//! The instructions op (ain4, D-A8N0E): `{load?}` is one public instructions call, this project
//! and the person layer merged, as `rdy_instructions` answers it. `{load?, part: true}` is the
//! private form a host's reader of the person layer sends ([`crate::instructions`]): this
//! store's own canon as the person layer, never another source, and it marks no call complete.
use super::*;
use rdy_engine::instructions::{self, Caller, Layer, LoadRequest};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct InstructionsArgs {
    load: Option<Vec<String>>,
    #[serde(default)]
    part: bool,
}

pub(super) fn instructions(cx: &OpContext, a: InstructionsArgs) -> Result<Value, RdyError> {
    let load = LoadRequest::parse(&a.load.unwrap_or_default())?;
    let state = cx.engine.read();
    if a.part {
        let part = instructions::collect(&state, Layer::Person, &load)?;
        return Ok(json!({ "contribution": part }));
    }
    let caller = Caller {
        actor: cx.session.actor.clone(),
        run: cx.session.run.clone(),
    };
    let root = cx.engine.root();
    let host = cx.engine.person_host();
    let reply = instructions::deliver(Some((&state, &root)), &load, &caller, host.as_deref())?;
    drop(state);
    let bytes = reply.to_json()?;
    serde_json::from_slice(&bytes).map_err(|e| {
        RdyError::new(
            rdy_engine::E_INTERNAL,
            format!("an instructions reply did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}
