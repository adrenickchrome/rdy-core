use rdy_model::{Provenance, Role};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub actor: String,
    pub run: Option<String>,
    pub role: Role,
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Session {
    pub fn provenance(&self) -> Provenance {
        Provenance { actor: self.actor.clone(), run: self.run.clone(), role: self.role, at_ms: now_ms() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Canon,
    View(String),
}

impl Scope {
    pub fn parse(s: &str) -> Scope {
        if s == "canon" { Scope::Canon } else { Scope::View(s.to_string()) }
    }
}
