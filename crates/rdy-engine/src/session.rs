use crate::error::{E_PERMISSION, RdyError};
use rdy_model::{Provenance, Role};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// The run a human session records: the CLI is the human's one run.
pub const HUMAN_RUN: &str = "cli";
/// The scope text of the in-flight state (insc1): canon plus every open view's changes. A
/// reserved name: no view can take it.
pub const INFLIGHT_SCOPE: &str = "inflight";
/// The prefix of a human actor: an agent session never claims one.
const HUMAN_PREFIX: &str = "human:";

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

thread_local! {
    /// The pinned clock of this thread: the next time [`now_ms`] reads, if pinned.
    static PINNED: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// The time in milliseconds since the epoch: the wall clock, or this thread's pinned clock.
pub fn now_ms() -> i64 {
    if let Some(t) = PINNED.with(|p| {
        let t = p.get()?;
        p.set(Some(t + 1));
        Some(t)
    }) {
        return t;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Pins this thread's clock at `start_ms` until the guard drops, so fixtures stamp the same
/// dates on every run. Each read advances it by a millisecond, keeping stamps ordered.
pub fn pin_clock(start_ms: i64) -> PinnedClock {
    PinnedClock(PINNED.with(|p| p.replace(Some(start_ms))))
}

/// Restores the clock that [`pin_clock`] replaced when dropped.
#[must_use = "the clock is pinned only while the guard lives"]
pub struct PinnedClock(Option<i64>);

impl Drop for PinnedClock {
    fn drop(&mut self) {
        PINNED.with(|p| p.set(self.0));
    }
}

impl Session {
    /// The human's session: its run is `cli`. The actor must be in the actor form (D-ME2C0):
    /// else `E_BAD_INPUT`.
    pub fn human(actor: String, role: Role) -> Result<Session, RdyError> {
        crate::identity::validate_actor(&actor)?;
        Ok(Session {
            actor,
            run: Some(HUMAN_RUN.to_string()),
            role,
            kind: SessionKind::Human,
        })
    }

    /// An agent's session (D-ME2C0): the actor in the actor form, then the run (if any) in the
    /// run form (`E_BAD_INPUT`), then no `human:` actor and never the approver role
    /// (`E_PERMISSION`): approvals are the human's.
    pub fn agent(actor: String, run: Option<String>, role: Role) -> Result<Session, RdyError> {
        crate::identity::validate_actor(&actor)?;
        if let Some(r) = &run {
            crate::identity::validate_run(r)?;
        }
        if actor.starts_with(HUMAN_PREFIX) {
            return Err(RdyError::new(
                E_PERMISSION,
                Self::HUMAN_CLAIM,
                "unset RDY_ACTOR to act as the person, or relay their words",
            ));
        }
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

    /// The message of an agent session that claims a human actor (D-ME2C0).
    pub const HUMAN_CLAIM: &'static str = "an agent session cannot claim a human actor";

    /// Whether `e` is the constructor's refusal of an agent session claiming a human actor (and
    /// not the approver-role refusal, which shares its code): MCP gives it its own fix.
    pub fn is_human_claim(e: &RdyError) -> bool {
        e.code == E_PERMISSION && e.message == Self::HUMAN_CLAIM
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

/// The human actor of the user config alone: `actor` in `<xdg>/rdy/config.yaml` (else
/// `<home>/.config/rdy/config.yaml`), read within the config bounds, else the bounded fallback
/// of `user` (D-Q8NFG). A file that does not read, a wrong type or an actor outside the form is
/// its error, never the fallback.
pub fn human_actor(
    xdg_config_home: Option<&Path>,
    home: Option<&Path>,
    user: Option<&str>,
) -> Result<String, RdyError> {
    crate::config_levels::user_actor(
        crate::config_levels::user_file_of(xdg_config_home, home).as_deref(),
        user,
    )
}

/// The human of a project: its `.rdy/local.yaml` actor first, then the user config's.
pub use crate::config_levels::{human_actor_for, human_actor_for_env};

/// The session for a client that sent `actor` and `run`: the human if it sent no actor or the
/// human's own (its run is `cli`, whatever it sent), else an agent. The configured `human`, then
/// each supplied actor and run, are checked against their forms first (D-ME2C0); another
/// `human:` actor is an agent claiming a human, refused.
pub fn classify(
    human: &str,
    actor: Option<&str>,
    run: Option<&str>,
    role: Role,
) -> Result<Session, RdyError> {
    crate::identity::validate_actor(human)?;
    if let Some(a) = actor {
        crate::identity::validate_actor(a)?;
    }
    if let Some(r) = run {
        crate::identity::validate_run(r)?;
    }
    match actor {
        None => Session::human(human.to_string(), role),
        Some(a) if a == human => Session::human(human.to_string(), role),
        Some(a) => Session::agent(a.to_string(), run.map(str::to_string), role),
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Canon,
    View(String),
    /// Canon as of a landing in canon's chain (a version or a landing id). Read-only.
    At(rdy_model::Hash),
    /// insc1: canon plus the distinct changes of every open view, merged. An analysis scope:
    /// read-only, with no head and no owner.
    Inflight,
    /// ls8: a proposal version (its hash): canon at the version's base plus its changes, in
    /// their stored order. Read-only, and it never moves: a version never changes.
    Proposal(rdy_model::Hash),
}

/// UTC RFC 3339 timestamp with second precision, e.g. 2026-10-03T19:35:01Z.
pub fn rfc3339_now() -> String {
    rfc3339(now_ms())
}

/// UTC RFC 3339 timestamp with second precision for `ms` milliseconds since the epoch.
pub fn rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
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
