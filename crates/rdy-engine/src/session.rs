use rdy_model::{Provenance, Role};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub actor: String,
    pub run: Option<String>,
    pub role: Role,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
}

impl Scope {
    pub fn parse(s: &str) -> Scope {
        if s == "canon" {
            Scope::Canon
        } else {
            Scope::View(s.to_string())
        }
    }
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
