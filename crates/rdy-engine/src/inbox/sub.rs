//! Subscriptions: a session's inbox ref is created at the head of the log, and `rdy_session`
//! declares its areas and role (spec §10.2; D-TPZ0W, D-V4NZX, D-Z0EMP).
use crate::engine::write_typed;
use crate::{E_BAD_INPUT, E_INTERNAL, Engine, Rdy, RdyError, Session};
use rdy_model::Role;
use rdy_store::StoreError;
use rdy_store::inbox::{InboxCursor, InboxSub};

/// The role text a subscription stores. An approver session reads the human's inbox as a writer.
pub fn role_name(r: Role) -> &'static str {
    match r {
        Role::Reader => "reader",
        Role::Writer | Role::Approver => "writer",
        Role::Reviewer => "reviewer",
    }
}

impl Rdy {
    /// Writes one session's inbox ref. Only subscribe and ack call it outside tests.
    pub fn put_subscription(&mut self, sub: &InboxSub) -> Result<(), RdyError> {
        // a refusal means a caller built a subscription the store's rules reject: a bug in rdy,
        // not bad input; any other store error maps as it does everywhere (damage stays `E_IO`)
        self.store_mut().put_inbox(sub).map_err(|e| match e {
            StoreError::Refused(message) => {
                RdyError::new(E_INTERNAL, message, "report this as a bug in rdy")
            }
            e => e.into(),
        })?;
        self.publish(crate::Delta::Seq)
    }
}

/// The areas, checked against the pack's and made sorted and distinct.
fn checked_areas(pack_areas: &[String], given: Vec<String>) -> Result<Vec<String>, RdyError> {
    for a in &given {
        if !pack_areas.contains(a) {
            let fix = if pack_areas.is_empty() {
                "the pack declares no areas: pass none".to_string()
            } else {
                format!("pass areas the pack declares: {}", pack_areas.join(", "))
            };
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("area {a:?} is not declared in the pack"),
                fix,
            ));
        }
    }
    let mut out = given;
    out.sort();
    out.dedup();
    Ok(out)
}

/// What to write for session `s`, given what is persisted: nothing when it already holds the
/// session's areas (if given) and role. A new subscription starts at `head`.
fn wanted(
    persisted: Option<InboxSub>,
    s: &Session,
    areas: Option<&[String]>,
    head: rdy_store::segment::Location,
) -> Option<InboxSub> {
    let role = role_name(s.role);
    match persisted {
        None => Some(InboxSub {
            actor: s.actor.clone(),
            run: s.run.clone(),
            cursor: InboxCursor { at: head, skip: 0 },
            areas: areas.map(<[String]>::to_vec).unwrap_or_default(),
            role: role.to_string(),
        }),
        Some(p) => {
            let mut next = p.clone();
            if let Some(a) = areas {
                next.areas = a.to_vec();
            }
            next.role = role.to_string();
            (next != p).then_some(next)
        }
    }
}

/// The session's persisted subscription. It is written only when there is none, or when `areas`
/// (if given) or the role differ. A new one starts at the head of the log. An area the pack
/// lacks is refused with `E_BAD_INPUT`, which lists the pack's areas (D-TPZ0W). A read-only
/// store (D-WWAK7) writes nothing: the reply is the subscription the session would have, or the
/// persisted one unchanged.
pub fn subscribe(
    engine: &dyn Engine,
    s: &Session,
    areas: Option<Vec<String>>,
) -> Result<InboxSub, RdyError> {
    let st = engine.read();
    let areas = match areas {
        Some(a) => {
            let pack: Vec<String> = st.pack().areas.keys().cloned().collect();
            Some(checked_areas(&pack, a)?)
        }
        None => None,
    };
    let persisted = st.reader().inbox(&s.actor, s.run.as_deref())?;
    let want = wanted(persisted.clone(), s, areas.as_deref(), st.seq());
    if st.reader().access() == rdy_store::format::StoreAccess::ReadOnly {
        return Ok(match want {
            Some(sub) => sub,
            None => persisted.expect("an unchanged subscription is persisted"),
        });
    }
    if want.is_none() {
        return Ok(persisted.expect("an unchanged subscription is persisted"));
    }
    let (session, given) = (s.clone(), areas);
    let v = write_typed(engine, move |r| {
        // re-read inside the write: a racing ack's newer cursor is kept, never undone
        let st = r.read();
        let persisted = st.reader().inbox(&session.actor, session.run.as_deref())?;
        match wanted(persisted.clone(), &session, given.as_deref(), st.seq()) {
            Some(sub) => {
                r.put_subscription(&sub)?;
                Ok(sub)
            }
            None => Ok(persisted.expect("an unchanged subscription is persisted")),
        }
    })?;
    serde_json::from_value(v).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a subscription did not round-trip: {e}"),
            "report this as a bug in rdy",
        )
    })
}
