//! Strict readers for the existing actor records, used only at the boundary of the new editable
//! records. `Provenance`, `ActorRun` and `RelayEvidence` keep their permissive deserializers and
//! their postcard bytes: each reader here has the same fields in the same order, refuses unknown
//! keys, and converts into the existing type.

use crate::{ActorRun, Provenance, RelayEvidence, Role};
use serde::{Deserialize, Deserializer};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictProvenance {
    actor: String,
    run: Option<String>,
    role: Role,
    at_ms: i64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictActorRun {
    actor: String,
    run: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictRelayEvidence {
    quoted: String,
    session_ref: String,
    question: Option<String>,
}

impl From<StrictProvenance> for Provenance {
    fn from(p: StrictProvenance) -> Self {
        Provenance {
            actor: p.actor,
            run: p.run,
            role: p.role,
            at_ms: p.at_ms,
        }
    }
}

impl From<StrictActorRun> for ActorRun {
    fn from(a: StrictActorRun) -> Self {
        ActorRun {
            actor: a.actor,
            run: a.run,
        }
    }
}

impl From<StrictRelayEvidence> for RelayEvidence {
    fn from(r: StrictRelayEvidence) -> Self {
        RelayEvidence {
            quoted: r.quoted,
            session_ref: r.session_ref,
            question: r.question,
        }
    }
}

pub(crate) fn provenance<'de, D: Deserializer<'de>>(d: D) -> Result<Provenance, D::Error> {
    StrictProvenance::deserialize(d).map(Into::into)
}

pub(crate) fn actor_run_opt<'de, D: Deserializer<'de>>(d: D) -> Result<Option<ActorRun>, D::Error> {
    Option::<StrictActorRun>::deserialize(d).map(|o| o.map(Into::into))
}

pub(crate) fn relay_opt<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<RelayEvidence>, D::Error> {
    Option::<StrictRelayEvidence>::deserialize(d).map(|o| o.map(Into::into))
}
