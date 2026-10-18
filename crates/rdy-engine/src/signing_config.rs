//! The signing settings (D-QVH4N): `signing.key`, `signing.agent_key`, `signing.sign` and
//! `signing.allowed_signers`. All four are valid in the user config and in `.rdy/local.yaml`
//! only. This module reads them from [`Effective`]; the levels, the warnings and the origins are
//! the work of `config_levels`. It contacts no agent and reads no file.

use crate::RdyError;
use crate::config_levels::{
    Effective, Level, SIGNING_AGENT_KEY, SIGNING_ALLOWED_SIGNERS, SIGNING_KEY, SIGNING_SIGN,
};
use serde_yaml_ng::Value;
use std::path::{Path, PathBuf};

/// When rdy signs an act (`signing.sign`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SignMode {
    /// Never sign.
    Never,
    /// Sign when a key is set and the agent can sign: the default.
    #[default]
    Auto,
    /// Sign every act, and refuse the act when signing fails.
    Always,
}

/// The signing settings of one project for one person.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SigningConfig {
    /// `signing.key`: the public-key line or SHA256 fingerprint of the signing key, as written.
    pub key: Option<String>,
    /// `signing.agent_key`: the same for the key an agent signs with, as written.
    pub agent_key: Option<String>,
    /// `signing.sign`.
    pub sign: SignMode,
    /// `signing.allowed_signers`: the allowed-signers file. A relative path of the local file is
    /// resolved from the project root; any other path is absolute.
    pub allowed_signers: Option<PathBuf>,
}

impl SigningConfig {
    /// The signing settings of `effective`, for the project at `project`. The levels, the
    /// warnings and the equal-key rule are already applied in `effective`: this reads its merged
    /// settings and the origin of the allowed-signers path. Never fails today; it returns
    /// `Result` so that a later check can refuse without a signature change.
    pub fn from_effective(
        project: &Path,
        effective: &Effective,
    ) -> Result<SigningConfig, RdyError> {
        let text = |key: &str| match get(effective, key) {
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let sign = match get(effective, SIGNING_SIGN).and_then(Value::as_str) {
            Some("never") => SignMode::Never,
            Some("always") => SignMode::Always,
            _ => SignMode::Auto,
        };
        // a relative path of the local file is the project's; the user level holds absolute
        // paths only, and an empty string stays empty for the path error
        let allowed_signers = text(SIGNING_ALLOWED_SIGNERS).map(|s| {
            let from_local = effective
                .origins
                .iter()
                .any(|o| o.key == SIGNING_ALLOWED_SIGNERS && o.level == Level::Local);
            if from_local && !s.is_empty() && !Path::new(&s).is_absolute() {
                project.join(s)
            } else {
                PathBuf::from(s)
            }
        });
        Ok(SigningConfig {
            key: text(SIGNING_KEY),
            agent_key: text(SIGNING_AGENT_KEY),
            sign,
            allowed_signers,
        })
    }
}

/// The merged value at dotted `path`.
fn get<'a>(effective: &'a Effective, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let mut cur = effective.merged.get(parts.next()?)?;
    for part in parts {
        cur = cur.as_mapping()?.get(part)?;
    }
    Some(cur)
}
