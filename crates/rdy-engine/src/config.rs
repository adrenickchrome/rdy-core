//! The project config (`.rdy/config.yaml`) as rdy reads it.

use crate::config_levels;
use crate::{E_CONFIG, RdyError};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Config {
    pub(crate) pack: String,
    /// The project's name as a reader of its site sees it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// One sentence about the project, shown on the site's home page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    /// How proposals are reviewed (FW-HB1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reviews: Option<ReviewsConfig>,
    /// How views are listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) views: Option<ViewsConfig>,
    /// Where decisions are recorded (FW-PR2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) decisions: Option<DecisionsConfig>,
    #[serde(default, skip_serializing_if = "ServeSection::is_empty")]
    pub(crate) serve: ServeSection,
}

/// The `serve:` section of `.rdy/config.yaml`, in seconds.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct ServeSection {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    idle_timeout: Option<u64>,
}

impl ServeSection {
    fn is_empty(&self) -> bool {
        self.start_timeout.is_none() && self.idle_timeout.is_none()
    }

    pub(crate) fn resolve(&self) -> ServeConfig {
        ServeConfig {
            start_timeout: self
                .start_timeout
                .map_or(ServeConfig::DEFAULT.start_timeout, Duration::from_secs),
            idle_timeout: self
                .idle_timeout
                .map_or(ServeConfig::DEFAULT.idle_timeout, Duration::from_secs),
        }
    }
}

/// How long a client waits for a daemon to start and how long an idle daemon stays up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServeConfig {
    pub start_timeout: Duration,
    pub idle_timeout: Duration,
}

impl ServeConfig {
    pub const DEFAULT: ServeConfig = ServeConfig {
        start_timeout: Duration::from_secs(30),
        idle_timeout: Duration::from_secs(600),
    };
}

/// The project's `serve:` settings, read without opening the store. A missing file, a missing
/// section or an unreadable value gives the default for what is missing and never an error: a
/// client must be able to start a daemon on a project whose config is broken, and the daemon
/// then reports the config error.
pub fn serve_config(project: &Path) -> ServeConfig {
    let merged =
        config_levels::effective_lenient(project, config_levels::user_file_from_env().as_deref())
            .merged;
    let section = merged.get("serve");
    let secs = |key: &str| {
        section
            .and_then(|s| s.get(key))
            .and_then(serde_yaml_ng::Value::as_u64)
    };
    ServeSection {
        start_timeout: secs("start_timeout"),
        idle_timeout: secs("idle_timeout"),
    }
    .resolve()
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ViewsConfig {
    /// Days without a write after which `rdy view ls` flags an open view as idle.
    idle_days: u32,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct DecisionsConfig {
    /// The doc (path, key or id) a decision given no parent is recorded in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    home: Option<String>,
}

/// `views.idle_days` when the config does not set it.
const DEFAULT_IDLE_DAYS: u32 = 14;

#[derive(serde::Serialize, serde::Deserialize, Default)]
pub(crate) struct ReviewsConfig {
    /// A proposal lands only on the canon head it was proposed on.
    #[serde(default)]
    require_up_to_date: bool,
}

impl Config {
    pub(crate) fn require_up_to_date(&self) -> bool {
        self.reviews.as_ref().is_some_and(|r| r.require_up_to_date)
    }

    pub(crate) fn idle_days(&self) -> u32 {
        self.views
            .as_ref()
            .map_or(DEFAULT_IDLE_DAYS, |v| v.idle_days)
    }

    /// `decisions.home`, trimmed; an empty text counts as unset.
    pub(crate) fn decisions_home(&self) -> Option<Arc<str>> {
        self.decisions
            .as_ref()
            .and_then(|d| d.home.as_deref())
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(Arc::from)
    }
}

pub(crate) fn read_config(root: &Path) -> Result<Config, RdyError> {
    let effective = config_levels::effective(root, config_levels::user_file_from_env().as_deref())?;
    config_of(effective)
}

/// The config from the bytes `read_sources` read (the project and local files; the user config
/// is read from the environment), so what is loaded is what the fingerprint hashed.
pub(crate) fn parse_config(
    root: &Path,
    config: &[u8],
    local: Option<&[u8]>,
) -> Result<Config, RdyError> {
    let effective = config_levels::effective_from(
        root,
        config_levels::user_file_from_env().as_deref(),
        config_levels::GivenFiles {
            project: config,
            local,
        },
    )?;
    config_of(effective)
}

fn config_of(effective: config_levels::Effective) -> Result<Config, RdyError> {
    serde_yaml_ng::from_value(serde_yaml_ng::Value::Mapping(effective.merged)).map_err(|e| {
        RdyError::new(
            E_CONFIG,
            e.to_string(),
            format!("fix {}", config_levels::PROJECT_FILE),
        )
    })
}

/// The project's name and description as a site shows them, parsed once when a state is
/// published (a state is a snapshot). The name is the config's `name`, else the project
/// directory's name (`-` and `_` turned into spaces, the first letter upper-cased), else
/// `Design docs`; the description is the config's non-empty `description`.
pub(crate) fn site_identity(root: &Path, cfg: &Config) -> (Arc<str>, Option<Arc<str>>) {
    let named = cfg.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let name = match named {
        Some(n) => n.to_string(),
        None => {
            let dir = root
                .file_name()
                .map(|n| n.to_string_lossy().replace(['-', '_'], " "))
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty());
            match dir {
                Some(d) => {
                    let mut chars = d.chars();
                    let first = chars.next().expect("non-empty").to_uppercase();
                    first.chain(chars).collect()
                }
                None => "Design docs".to_string(),
            }
        }
    };
    let description = cfg
        .description
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map(Arc::from);
    (Arc::from(name), description)
}
