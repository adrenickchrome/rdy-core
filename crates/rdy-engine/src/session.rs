use rdy_model::{Provenance, Role};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub actor: String,
    pub run: Option<String>,
    pub role: Role,
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
    pub fn provenance(&self) -> Provenance {
        Provenance {
            actor: self.actor.clone(),
            run: self.run.clone(),
            role: self.role,
            at_ms: now_ms(),
        }
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
