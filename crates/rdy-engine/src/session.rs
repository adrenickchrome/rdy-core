use crate::error::{E_PERMISSION, RdyError};
use rdy_model::{Provenance, Role};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// The run a human session records: the CLI is the human's one run.
pub const HUMAN_RUN: &str = "cli";

/// Who is on the other end: the configured human, or anything else (an agent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionKind {
    Human,
    Agent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub actor: String,
    pub run: Option<String>,
    pub role: Role,
    pub kind: SessionKind,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Session {
    /// The human's session: its run is `cli`.
    pub fn human(actor: String, role: Role) -> Session {
        Session {
            actor,
            run: Some(HUMAN_RUN.to_string()),
            role,
            kind: SessionKind::Human,
        }
    }

    /// An agent's session. Agents never hold the approver role: approvals are the human's.
    pub fn agent(actor: String, run: Option<String>, role: Role) -> Result<Session, RdyError> {
        if role == Role::Approver {
            return Err(RdyError::new(
                E_PERMISSION,
                "agent sessions cannot hold the approver role",
                "approvals are made by the human actor: unset RDY_ACTOR, or set actor: in the user's rdy config",
            ));
        }
        Ok(Session {
            actor,
            run,
            role,
            kind: SessionKind::Agent,
        })
    }

    pub fn is_human(&self) -> bool {
        self.kind == SessionKind::Human
    }

    /// Where this session writes when it names no view: the human's `work`; an agent's run
    /// (`:` becomes `-`), else `work-<actor>`.
    pub fn default_view(&self) -> String {
        match self.kind {
            SessionKind::Human => "work".to_string(),
            SessionKind::Agent => match &self.run {
                Some(r) => r.replace(':', "-"),
                None => format!("work-{}", self.actor.replace(':', "-")),
            },
        }
    }

    pub fn provenance(&self) -> Provenance {
        Provenance {
            actor: self.actor.clone(),
            run: self.run.clone(),
            role: self.role,
            at_ms: now_ms(),
        }
    }
}

/// The human actor: `actor` in `<xdg>/rdy/config.yaml` (else `<home>/.config/rdy/config.yaml`)
/// when that file parses and names a non-empty string, else `human:<user>`. Never an error.
pub fn human_actor(
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
    user: Option<&str>,
) -> String {
    let file = match (xdg_config_home, home) {
        (Some(x), _) => Some(x.join("rdy/config.yaml")),
        (None, Some(h)) => Some(h.join(".config/rdy/config.yaml")),
        (None, None) => None,
    };
    let named = file
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|t| serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&t).ok())
        .and_then(|v| v.get("actor").and_then(|a| a.as_str()).map(str::to_string))
        .filter(|a| !a.is_empty());
    named.unwrap_or_else(|| format!("human:{}", user.unwrap_or("unknown")))
}

/// `human_actor` over `XDG_CONFIG_HOME`, `HOME` and `USER`: the one place product code reads them.
pub fn human_actor_from_env() -> String {
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let home = std::env::var_os("HOME");
    let user = std::env::var("USER").ok();
    human_actor(
        xdg.as_deref().map(Path::new),
        home.as_deref().map(Path::new),
        user.as_deref(),
    )
}

/// The session for a client that sent `actor` and `run`: the human if it sent no actor or the
/// human's own (its run is `cli`, whatever it sent), else an agent.
pub fn classify(
    human: &str,
    actor: Option<&str>,
    run: Option<&str>,
    role: Role,
) -> Result<Session, RdyError> {
    match actor {
        None => Ok(Session::human(human.to_string(), role)),
        Some(a) if a == human => Ok(Session::human(human.to_string(), role)),
        Some(a) => Session::agent(a.to_string(), run.map(str::to_string), role),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Canon,
    View(String),
    /// Canon as of a landing in canon's chain (a version or a landing id). Read-only.
    At(rdy_model::Hash),
}

/// UTC RFC 3339 timestamp with second precision, e.g. 2026-10-03T19:35:01Z.
pub fn rfc3339_now() -> String {
    let secs = now_ms().div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}
