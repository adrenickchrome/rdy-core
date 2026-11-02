//! The error details of a rendered refusal (D-N507H): the message in plain words, and beside it
//! a native `details` control whose popover holds the error code and its fix. It needs no script
//! and no click handler: the browser opens it from the keyboard or the pointer. Every text is
//! escaped by the template, never written as markup, and the control carries no request id.
use crate::RdyError;
use serde::Serialize;

/// The error code and the fix a refusal shows in its popover.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Detail {
    pub code: String,
    pub fix: String,
}

/// The refusal block of `message` and `detail`: the same partial the not-found page includes.
pub fn html(message: &str, detail: &Detail) -> Result<String, RdyError> {
    let env = crate::env()?;
    let t = env
        .get_template("partials/refusal.html")
        .map_err(crate::tpl)?;
    t.render(minijinja::context! {
        refusal_message => message,
        detail => minijinja::Value::from_serialize(detail),
    })
    .map_err(crate::tpl)
}
