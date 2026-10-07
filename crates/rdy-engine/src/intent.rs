//! Intents (plan M2d1 Task 6; intn1, iwr1). An intent says, for a while, that a session means to
//! work on an area or on some blocks, in a view. It is advisory: it refuses no write. Each change
//! to it (renew, withdraw) is a new version; expiry is computed when it is read, never written.
use crate::error::*;
use crate::views::clip;
use crate::write::{WriteRequest, WriteResult};
use crate::{Delta, Rdy, ReadState, Scope, Session, now_ms, rfc3339};
use rdy_model::{BlockId, ChangeLinks, Hash, Intent, IntentStatus};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub use crate::coord_graph::intent_label;

/// The time an intent lasts when no ttl is given: 2 hours.
pub const TTL_DEFAULT_MS: i64 = 7_200_000;
/// The longest ttl: 7 days.
pub const TTL_MAX_MS: i64 = 604_800_000;
/// The shortest ttl: 1 minute.
pub const TTL_MIN_MS: i64 = 60_000;
/// The most blocks one intent names.
pub const MAX_INTENT_BLOCKS: usize = 1000;
/// The longest angle, in characters.
pub const MAX_ANGLE: usize = 500;

/// An intent as a reader sees it: its latest version, with the time it was first posted and its
/// status at the time of the read (`open`, `withdrawn` or `expired`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IntentInfo {
    /// The display id: `in-` and the stable id's short form.
    pub id: String,
    /// The stable id (the first version's hash).
    pub intent: String,
    /// The latest version.
    pub version: String,
    /// The version the latest one replaced.
    pub prev: Option<String>,
    pub actor: String,
    pub run: Option<String>,
    pub view: String,
    pub area: Option<String>,
    pub blocks: Vec<String>,
    pub angle: String,
    /// When the first version was written.
    pub posted_at: String,
    pub posted_at_ms: i64,
    pub expires_at: String,
    pub expires_at_ms: i64,
    pub status: String,
}

/// Which intents `ReadState::intents` lists. Every filter given must match. `block` matches an
/// intent that covers the block in canon. Without `include_expired`, only open, unexpired intents
/// are listed; with it, expired and withdrawn ones are listed too.
#[derive(Clone, Debug, Default)]
pub struct IntentFilter {
    pub area: Option<String>,
    pub block: Option<BlockId>,
    pub actor: Option<String>,
    pub view: Option<String>,
    pub include_expired: bool,
}

/// Another session's open intent on blocks a write touched (iwr1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct IntentNote {
    pub id: String,
    pub actor: String,
    pub run: Option<String>,
    pub view: String,
    pub angle: String,
    pub expires_at: String,
    /// The touched blocks the intent covers, in id order.
    pub blocks: Vec<String>,
}

fn bad_ttl(text: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!(
            "ttl {} is not a duration like 30m, 2h or 7d (at most 7d)",
            clip(text)
        ),
        "pass a whole number of minutes, hours or days (30m, 2h, 7d), from 1m to 7d",
    )
}

/// The milliseconds of a ttl text: `<n>m`, `<n>h` or `<n>d`, `n` a whole number without a leading
/// zero, from 1m to 7d. Anything else is `E_BAD_INPUT`.
pub fn parse_ttl(text: &str) -> Result<i64, RdyError> {
    let Some(unit) = text.chars().last() else {
        return Err(bad_ttl(text));
    };
    let unit_ms = match unit {
        'm' => 60_000,
        'h' => 3_600_000,
        'd' => 86_400_000,
        _ => return Err(bad_ttl(text)),
    };
    let digits = &text[..text.len() - 1];
    if !digits.starts_with(|c: char| ('1'..='9').contains(&c))
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(bad_ttl(text));
    }
    let ms = digits
        .parse::<i64>()
        .ok()
        .and_then(|n| n.checked_mul(unit_ms))
        .ok_or_else(|| bad_ttl(text))?;
    if !(TTL_MIN_MS..=TTL_MAX_MS).contains(&ms) {
        return Err(bad_ttl(text));
    }
    Ok(ms)
}

/// A ttl given in milliseconds, checked against the bounds; `None` is the default.
fn ttl_or_default(ttl_ms: Option<i64>) -> Result<i64, RdyError> {
    match ttl_ms {
        None => Ok(TTL_DEFAULT_MS),
        Some(ms) if (TTL_MIN_MS..=TTL_MAX_MS).contains(&ms) => Ok(ms),
        Some(ms) => Err(RdyError::new(
            E_BAD_INPUT,
            format!("ttl {ms} ms is not between 1m and 7d"),
            "pass a ttl from 1m to 7d, or none for the default (2h)",
        )),
    }
}

/// The status of intent version `it` at `now`: withdrawn, else expired from `expires_at_ms` on.
fn status_at(it: &Intent, now: i64) -> &'static str {
    match it.status {
        IntentStatus::Withdrawn => "withdrawn",
        IntentStatus::Open if now >= it.expires_at_ms => "expired",
        IntentStatus::Open => "open",
    }
}

/// Whether intent version `it` is open (not withdrawn, not expired) at `now`.
pub(crate) fn status_open_at(it: &Intent, now: i64) -> bool {
    status_at(it, now) == "open"
}

fn info(stable: Hash, version: Hash, it: &Intent, posted_at_ms: i64, now: i64) -> IntentInfo {
    IntentInfo {
        id: intent_label(&stable),
        intent: stable.to_string(),
        version: version.to_string(),
        prev: it.prev.map(|p| p.to_string()),
        actor: it.actor.clone(),
        run: it.run.clone(),
        view: it.view.clone(),
        area: it.area.clone(),
        blocks: it.blocks.iter().map(ToString::to_string).collect(),
        angle: it.angle.clone(),
        posted_at: rfc3339(posted_at_ms),
        posted_at_ms,
        expires_at: rfc3339(it.expires_at_ms),
        expires_at_ms: it.expires_at_ms,
        status: status_at(it, now).to_string(),
    }
}

/// The `E_BAD_INPUT` fix that names the pack's areas.
fn area_fix(st: &ReadState) -> String {
    let areas = &st.pack().areas;
    let names = if areas.is_empty() {
        "the pack declares none".to_string()
    } else {
        areas.keys().cloned().collect::<Vec<_>>().join(", ")
    };
    format!("pass area (one of: {names}) or blocks")
}

fn check_writer(s: &Session, what: &str) -> Result<(), RdyError> {
    if s.role.can_write() {
        return Ok(());
    }
    Err(RdyError::new(
        E_PERMISSION,
        format!("role {:?} cannot {what} intents", s.role),
        "start the session as a writer",
    ))
}

impl ReadState {
    /// The first version's `at_ms` of the intent `stable`, whose latest version is `latest`.
    fn intent_posted_at(&self, stable: Hash, latest: Hash, it: &Intent) -> Result<i64, RdyError> {
        if stable == latest {
            return Ok(it.at_ms);
        }
        Ok(self.reader().intent(&stable)?.at_ms)
    }

    /// The intents `f` selects, as of `now_ms`: newest first by the time each was first posted,
    /// then by id.
    pub fn intents(&self, f: &IntentFilter, now_ms: i64) -> Result<Vec<IntentInfo>, RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let mut out = vec![];
        for (&stable, &latest) in &self.coord_heads.intents {
            let it = self.reader().intent(&latest)?;
            if !f.include_expired && status_at(&it, now_ms) != "open" {
                continue;
            }
            if f.area.as_ref().is_some_and(|a| it.area.as_ref() != Some(a))
                || f.actor.as_ref().is_some_and(|a| *a != it.actor)
                || f.view.as_ref().is_some_and(|v| *v != it.view)
            {
                continue;
            }
            if let Some(b) = &f.block
                && !self.intent_covers(&Scope::Canon, &it, b)?
            {
                continue;
            }
            let posted = self.intent_posted_at(stable, latest, &it)?;
            out.push(info(stable, latest, &it, posted, now_ms));
        }
        out.sort_by(|a, b| {
            b.posted_at_ms
                .cmp(&a.posted_at_ms)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(out)
    }

    /// The intent `text` names: `in-<hex>` or `<hex>`, the full stable id or a unique prefix of at
    /// least 6 hex digits. Returns its stable id, its latest version and that version. Absent or
    /// ambiguous is `E_NOT_FOUND`.
    pub fn resolve_intent(&self, text: &str) -> Result<(Hash, Hash, Intent), RdyError> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        let hex = text
            .strip_prefix("in-")
            .unwrap_or(text)
            .to_ascii_lowercase();
        let not_found = || {
            RdyError::new(
                E_NOT_FOUND,
                format!("no intent {}", clip(text)),
                "pass an intent id from rdy_intent list (in-<hex>, at least 6 hex digits)",
            )
        };
        if !(6..=64).contains(&hex.len()) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(not_found());
        }
        let heads = &self.coord_heads.intents;
        let found: Vec<(Hash, Hash)> = match hex.parse::<Hash>() {
            Ok(h) => heads.get(&h).map(|&l| (h, l)).into_iter().collect(),
            Err(_) => heads
                .iter()
                .filter(|(s, _)| s.to_string().starts_with(&hex))
                .map(|(&s, &l)| (s, l))
                .collect(),
        };
        match found.as_slice() {
            [] => Err(not_found()),
            [(stable, latest)] => Ok((*stable, *latest, self.reader().intent(latest)?)),
            many => Err(RdyError::new(
                E_NOT_FOUND,
                format!(
                    "intent {} is ambiguous ({} intents)",
                    clip(text),
                    many.len()
                ),
                format!(
                    "use more characters: it matches {}",
                    many.iter()
                        .take(5)
                        .map(|(s, _)| intent_label(s))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }

    /// Whether intent version `it` covers block `b` in `scope`: `b` is one of its blocks, or its
    /// area is one of `b`'s areas there. A block absent from `scope` is in no area.
    pub fn intent_covers(&self, scope: &Scope, it: &Intent, b: &BlockId) -> Result<bool, RdyError> {
        if it.blocks.contains(b) {
            return Ok(true);
        }
        let Some(area) = &it.area else {
            return Ok(false);
        };
        Ok(self.block_areas(scope, b)?.iter().any(|a| a == area))
    }

    /// The areas of `b` in `scope`; none for a block absent there.
    fn block_areas(&self, scope: &Scope, b: &BlockId) -> Result<Vec<String>, RdyError> {
        match self.areas_of(scope, b) {
            Err(e) if e.code == E_UNKNOWN_BLOCK => Ok(vec![]),
            r => r,
        }
    }

    /// iwr1: the open, unexpired intents (as of `now_ms`) of sessions other than `(actor, run)`
    /// that cover any of `touched` in `scope`, each with the touched blocks it covers; by id.
    pub fn intent_notes(
        &self,
        scope: &Scope,
        actor: &str,
        run: Option<&str>,
        touched: &BTreeSet<BlockId>,
        now_ms: i64,
    ) -> Result<Vec<IntentNote>, RdyError> {
        self.notes(None, scope, actor, run, touched, now_ms)
    }

    /// [`ReadState::intent_notes`] for a write that took `before` to this state: a touched block
    /// is in the areas it has in `scope` here or had there, so a block the write deleted, or moved
    /// out of an area, still hears of an intent on the area it left.
    pub fn intent_notes_since(
        &self,
        before: &ReadState,
        scope: &Scope,
        actor: &str,
        run: Option<&str>,
        touched: &BTreeSet<BlockId>,
        now_ms: i64,
    ) -> Result<Vec<IntentNote>, RdyError> {
        self.notes(Some(before), scope, actor, run, touched, now_ms)
    }

    fn notes(
        &self,
        before: Option<&ReadState>,
        scope: &Scope,
        actor: &str,
        run: Option<&str>,
        touched: &BTreeSet<BlockId>,
        now_ms: i64,
    ) -> Result<Vec<IntentNote>, RdyError> {
        if self.coord_heads.intents.is_empty() || touched.is_empty() {
            return Ok(vec![]);
        }
        // each touched block's areas (here, and `before` when given), read once, when an area
        // intent first needs them
        let mut areas: BTreeMap<&BlockId, Vec<String>> = BTreeMap::new();
        let mut out = vec![];
        for (&stable, latest) in &self.coord_heads.intents {
            let it = self.reader().intent(latest)?;
            if status_at(&it, now_ms) != "open"
                || (it.actor.as_str(), it.run.as_deref()) == (actor, run)
            {
                continue;
            }
            let mut blocks = vec![];
            for b in touched {
                let covered = it.blocks.contains(b)
                    || match &it.area {
                        None => false,
                        Some(area) => {
                            if !areas.contains_key(b) {
                                let mut found = self.block_areas(scope, b)?;
                                if let Some(before) = before {
                                    found.extend(before.block_areas(scope, b)?);
                                }
                                areas.insert(b, found);
                            }
                            areas[b].contains(area)
                        }
                    };
                if covered {
                    blocks.push(b.to_string());
                }
            }
            if !blocks.is_empty() {
                out.push(IntentNote {
                    id: intent_label(&stable),
                    actor: it.actor,
                    run: it.run,
                    view: it.view,
                    angle: it.angle,
                    expires_at: rfc3339(it.expires_at_ms),
                    blocks,
                });
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }
}

impl Rdy {
    /// Posts an intent: `s` means to work on `area` and/or `blocks` in `view` (the session's
    /// default view when `None`), for `ttl_ms` (2h when `None`). Blocks resolve in the view's
    /// scope (ids, keys or paths) and are stored as ids, deduplicated, in id order.
    pub fn intent_post(
        &mut self,
        s: &Session,
        view: Option<&str>,
        area: Option<&str>,
        blocks: &[String],
        angle: &str,
        ttl_ms: Option<i64>,
    ) -> Result<IntentInfo, RdyError> {
        check_writer(s, "post")?;
        let st = self.read();
        if area.is_none() && blocks.is_empty() {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "an intent needs an area or blocks",
                area_fix(&st),
            ));
        }
        if angle.is_empty() || angle.chars().count() > MAX_ANGLE {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "angle must be 1 to 500 characters",
                "say in a few words what you mean to do",
            ));
        }
        if blocks.len() > MAX_INTENT_BLOCKS {
            return Err(RdyError::new(
                E_LIMIT,
                format!(
                    "an intent names at most {MAX_INTENT_BLOCKS} blocks; {} were given",
                    blocks.len()
                ),
                "name the area instead, or post more than one intent",
            ));
        }
        if let Some(a) = area
            && !st.pack().areas.contains_key(a)
        {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("no area {} in pack {}", clip(a), st.pack().name),
                area_fix(&st),
            ));
        }
        let view = view.map_or_else(|| s.default_view(), str::to_string);
        let scope = Scope::View(view.clone());
        let mut ids = BTreeSet::new();
        if !blocks.is_empty() {
            let m = st.materialized(&scope)?;
            for text in blocks {
                let id = st.resolve_id(&scope, text)?;
                if m.get(&id).is_none() {
                    return Err(RdyError::new(
                        E_UNKNOWN_BLOCK,
                        format!("no block {id} in view {view}"),
                        "check the id with rdy_read",
                    ));
                }
                ids.insert(id);
            }
        }
        let ttl = ttl_or_default(ttl_ms)?;
        let now = now_ms();
        let it = Intent {
            stable: None,
            prev: None,
            actor: s.actor.clone(),
            run: s.run.clone(),
            view,
            area: area.map(str::to_string),
            blocks: ids.into_iter().collect(),
            angle: angle.to_string(),
            expires_at_ms: now.saturating_add(ttl),
            status: IntentStatus::Open,
            at_ms: now,
        };
        let version = self.store_mut().put_intent(&it)?;
        self.publish(Delta::Coord { closed: vec![] })?;
        Ok(info(version, version, &it, now, now))
    }

    /// Renews intent `id` until now plus `ttl_ms` (2h when `None`): a new version, same id.
    pub fn intent_renew(
        &mut self,
        s: &Session,
        id: &str,
        ttl_ms: Option<i64>,
    ) -> Result<IntentInfo, RdyError> {
        self.intent_next(s, id, |it, now| {
            it.expires_at_ms = now.saturating_add(ttl_or_default(ttl_ms)?);
            Ok(())
        })
    }

    /// Withdraws intent `id`: a new version with status withdrawn, same expiry.
    pub fn intent_withdraw(&mut self, s: &Session, id: &str) -> Result<IntentInfo, RdyError> {
        self.intent_next(s, id, |it, _| {
            it.status = IntentStatus::Withdrawn;
            Ok(())
        })
    }

    /// Writes the next version of intent `id`, as `change` makes it from the latest (at `now`).
    /// Checks, in order: the session may write, the intent resolves, the session may change it
    /// (the poster's actor, any run, or a human), it is not withdrawn, it has not expired.
    fn intent_next(
        &mut self,
        s: &Session,
        id: &str,
        change: impl FnOnce(&mut Intent, i64) -> Result<(), RdyError>,
    ) -> Result<IntentInfo, RdyError> {
        check_writer(s, "change")?;
        let st = self.read();
        let (stable, latest, it) = st.resolve_intent(id)?;
        let label = intent_label(&stable);
        if !s.is_human() && s.actor != it.actor {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("intent {label} belongs to {}", it.actor),
                "post your own intent",
            ));
        }
        if it.status == IntentStatus::Withdrawn {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("intent {label} was withdrawn"),
                "post a new one",
            ));
        }
        let now = now_ms();
        if now >= it.expires_at_ms {
            return Err(RdyError::new(
                E_INTENT_EXPIRED,
                format!("intent {label} expired at {}", rfc3339(it.expires_at_ms)),
                "post a new one",
            ));
        }
        let posted = st.intent_posted_at(stable, latest, &it)?;
        let mut next = it;
        next.stable = Some(stable);
        next.prev = Some(latest);
        next.at_ms = now;
        change(&mut next, now)?;
        let version = self.store_mut().put_intent(&next)?;
        self.publish(Delta::Coord { closed: vec![] })?;
        Ok(info(stable, version, &next, posted, now))
    }

    /// `write_briefed`, recording intent `intent` (any form `resolve_intent` takes) on the change.
    /// An unknown intent is `E_NOT_FOUND` and nothing is written.
    pub fn write_with_intent(
        &mut self,
        s: &Session,
        req: WriteRequest,
        brief: Option<String>,
        intent: Option<&str>,
    ) -> Result<WriteResult, RdyError> {
        let links = match intent {
            Some(text) => ChangeLinks {
                intent: Some(self.read().resolve_intent(text)?.0),
                ..Default::default()
            },
            None => ChangeLinks::default(),
        };
        self.write_linked(s, req, links, brief.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intn1_ttl_bounds_and_overflow() {
        assert_eq!(parse_ttl("1m").unwrap(), TTL_MIN_MS);
        assert_eq!(parse_ttl("168h").unwrap(), TTL_MAX_MS);
        for bad in ["169h", "99999999999999999999d", "1\u{e9}", "01h", "1H"] {
            assert_eq!(parse_ttl(bad).unwrap_err().code, E_BAD_INPUT, "{bad}");
        }
        assert_eq!(ttl_or_default(None).unwrap(), TTL_DEFAULT_MS);
        assert_eq!(ttl_or_default(Some(1)).unwrap_err().code, E_BAD_INPUT);
    }
}
