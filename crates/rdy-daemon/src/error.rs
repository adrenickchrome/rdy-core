//! The error shape every reply carries: `{code, message, fix}` plus optional extra fields,
//! the same shape as the M1 tool errors.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const E_DAEMON_START: &str = "E_DAEMON_START";
pub const E_DAEMON_VERSION: &str = "E_DAEMON_VERSION";
pub const E_DAEMON_GONE: &str = "E_DAEMON_GONE";
pub const E_BAD_INPUT: &str = "E_BAD_INPUT";
pub const E_LIMIT: &str = "E_LIMIT";
/// An op that needs a session ran before the `session` op.
pub const E_NO_SESSION: &str = "E_NO_SESSION";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorReply {
    pub code: String,
    pub message: String,
    pub fix: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl ErrorReply {
    pub fn new(code: &str, message: impl Into<String>, fix: impl Into<String>) -> Self {
        ErrorReply {
            code: code.to_string(),
            message: message.into(),
            fix: fix.into(),
            extra: Map::new(),
        }
    }
}

impl std::fmt::Display for ErrorReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {} (fix: {})", self.code, self.message, self.fix)
    }
}

impl std::error::Error for ErrorReply {}

impl From<rdy_engine::RdyError> for ErrorReply {
    fn from(e: rdy_engine::RdyError) -> Self {
        let mut r = ErrorReply::new(e.code, e.message, e.fix);
        if !e.issues.is_empty() {
            r.extra.insert(
                "issues".into(),
                serde_json::to_value(&e.issues).unwrap_or(Value::Null),
            );
        }
        if let Some(c) = e.corrected {
            r.extra.insert("corrected".into(), c);
        }
        if let Some(b) = e.briefing {
            r.extra.insert("briefing".into(), b);
        }
        r
    }
}
