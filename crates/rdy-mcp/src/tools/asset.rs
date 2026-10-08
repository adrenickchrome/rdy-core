//! rdy_asset: add an asset to the project.

use super::*;
use base64::Engine as _;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AssetParams {
    pub data_base64: String,
}

pub fn rdy_asset(st: &mut McpState, p: AssetParams) -> Result<Value, RdyError> {
    let s = session(st)?;
    if !s.role.can_write() {
        return Err(RdyError::new(
            E_PERMISSION,
            "readers cannot upload assets",
            "start the session as a writer",
        ));
    }
    let too_big = || {
        limit(
            format!("the asset is over the limit of {MAX_ASSET_BYTES} bytes"),
            format!("upload assets of at most {MAX_ASSET_BYTES} bytes"),
        )
    };
    if p.data_base64.len() > MAX_ASSET_BYTES / 3 * 4 + 4 {
        return Err(too_big());
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&p.data_base64)
        .map_err(|e| {
            RdyError::new(
                E_BAD_INPUT,
                e.to_string(),
                "send the file as standard base64",
            )
        })?;
    if bytes.len() > MAX_ASSET_BYTES {
        return Err(too_big());
    }
    let n = bytes.len();
    let h = st.engine.write(Box::new(move |r| {
        Ok(json!(r.store_mut().put_blob(&bytes)?.to_string()))
    }))?;
    Ok(json!({"hash": h, "bytes": n, "use": "set a figure block's `blob` field to this hash"}))
}
