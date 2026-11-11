//! The cited side of repo links, over one captured snapshot (D-DAPR8, D-C94DV, D-WDKWQ).
//!
//! [`Snapshot::link_export`] writes one tag's filtered view of a publication profile as a link
//! delivery stream. The selection, the derived facts and the stream grammar are core's
//! (`rdy_engine::link_export`, `rdy_store::sync_stream`); this module only pins them to a
//! captured tag and hands the host the result. [`Snapshot::tag_deliveries`] lists the foreign
//! pins the tag holds with the delivered versions and deliveries behind them. Neither adds a CLI
//! or MCP exchange, and neither reads a path or the network: the host owns link permission, live
//! publications, scheduling and fan-out (D-WDKWQ). The receiving side is a
//! project store's own take, not this module's.

use crate::store::Snapshot;
use rdy_engine::{RdyError, link_export};
use std::io::Write;

pub use rdy_engine::link_export::{MAX_TAG_USES, TagUse};
pub use rdy_model::foreign::{
    ForeignDerived, ForeignId, ForeignPin, ForeignPublication, ForeignVersion,
};
pub use rdy_store::sync_stream::{DeliveryRecord, DeliveryReport};

impl Snapshot {
    /// The scope of the captured tag `tag`: its landing, never a live tag-name lookup. The
    /// snapshot's epoch is checked first, as every content entry must.
    fn delivery_scope(&self, tag: &str) -> Result<(rdy_model::Hash, rdy_model::Tag), RdyError> {
        self.read_state().ensure_current_epoch()?;
        self.captured_tag(tag)
    }

    /// Writes the filtered view of `profile` (a profile id, a key, or `implicit:org`) at the
    /// captured tag `tag` (`1.2.0` finds `v1.2.0`) to `out` as one complete link delivery
    /// stream, and returns what it holds.
    ///
    /// The delivery holds the selected `BLOCK` objects of the publication selection at the tag,
    /// each with its source-derived facts, or its residue where a removal dropped it. A block that
    /// a hiding profile keeps only as a shell is not delivered. It holds no foreign copy and no
    /// `TAG` object; its header names the tag's hash for provenance only. The stream is written
    /// as it is read and reads only the snapshot's captured refs and log end, so a writer that
    /// advances meanwhile changes nothing in it. A tag the snapshot lacks is `E_NOT_FOUND`, a
    /// profile it lacks the publication's own refusal, and nothing is promised about the bytes
    /// of `out` after a refusal.
    pub fn link_export<W: Write>(
        &self,
        tag: &str,
        profile: &str,
        out: W,
    ) -> Result<DeliveryReport, RdyError> {
        let (hash, captured) = self.delivery_scope(tag)?;
        link_export::write(
            self.read_state(),
            &captured.name,
            hash,
            captured.landing,
            profile,
            out,
        )
    }

    /// The foreign pins of the blocks the captured tag `tag` holds, sorted by citing block and
    /// cited block. Each row names the delivered version this snapshot holds for the pin, and the
    /// source and the entry that delivered it, from the delivery history this snapshot captured,
    /// also after a later delivery replaced or removed that version. A pin whose version no
    /// delivery holds has none. A tag with no foreign pin lists none.
    pub fn tag_deliveries(&self, tag: &str) -> Result<Vec<TagUse>, RdyError> {
        let (_, captured) = self.delivery_scope(tag)?;
        link_export::tag_uses(self.read_state(), captured.landing)
    }
}

#[cfg(test)]
mod tests {
    use crate::store::StoreHandle;
    use rdy_engine::{Engine, Rdy, Session, SessionKind, WriteRequest};
    use rdy_model::Role;
    use serde_json::{Value, json};

    fn session(role: Role) -> Session {
        Session {
            actor: if role == Role::Approver {
                "human:founder".into()
            } else {
                "agent:a".into()
            },
            run: None,
            role,
            kind: SessionKind::Human,
        }
    }

    fn write(r: &mut Rdy, ops: Value) {
        r.write(
            &session(Role::Writer),
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(ops).unwrap(),
                justification: None,
                message: "rpl9".into(),
            },
        )
        .unwrap();
    }

    fn land_and_tag(r: &mut Rdy, tag: &str) {
        let p = r
            .propose(&session(Role::Writer), "work", "rpl9", Some("test summary"))
            .unwrap();
        r.land(&session(Role::Approver), &p.id).unwrap();
        r.tag(&session(Role::Approver), tag, None, None).unwrap();
    }

    /// A tagged project with one doc and a profile over it, closed again.
    fn tagged() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut r = Rdy::init(dir.path(), "rdy-standard").unwrap();
        let w = r
            .write(
                &session(Role::Writer),
                WriteRequest {
                    view: "work".into(),
                    ops: serde_json::from_value(json!([
                        {"op": "create", "type": "doc", "id": "$d", "fields": {"name": "d", "title": "D", "doc_type": "page"}},
                        {"op": "create", "type": "prose", "parent": "$d", "body": "Words."},
                        {"op": "create", "type": "doc", "id": "$admin", "fields": {"name": "admin", "title": "Admin", "doc_type": "page"}}
                    ]))
                    .unwrap(),
                    justification: None,
                    message: "rpl9".into(),
                },
            )
            .unwrap();
        write(
            &mut r,
            json!([{"op": "create", "type": "publication_profile", "parent": w.created["$admin"],
                "fields": {"key": "site", "title": "Site", "audience": "org", "select": [w.created["$d"]]}}]),
        );
        land_and_tag(&mut r, "v1.0.0");
        r.close().unwrap();
        dir
    }

    #[test]
    fn rpl9_snapshot_export_survives_a_writer_advancing() {
        let _clock = rdy_engine::pin_clock(1_000_000);
        let dir = tagged();
        let handle = StoreHandle::open(dir.path()).unwrap();
        let snap = handle.snapshot().unwrap();
        let mut first = Vec::new();
        let r1 = snap.link_export("v1.0.0", "site", &mut first).unwrap();

        // the writer lane advances: a new landing, a new tag
        handle
            .local()
            .write(Box::new(|rdy| {
                write(
                    rdy,
                    json!([{"op": "create", "type": "doc", "fields": {"name": "e", "title": "E", "doc_type": "page"}}]),
                );
                land_and_tag(rdy, "v2.0.0");
                Ok(Value::Null)
            }))
            .unwrap();

        // the captured snapshot answers as before, byte for byte, and never sees the new tag
        let mut again = Vec::new();
        let r2 = snap.link_export("v1.0.0", "site", &mut again).unwrap();
        assert_eq!(first, again);
        assert_eq!(r1, r2);
        let e = snap.link_export("v2.0.0", "site", Vec::new()).unwrap_err();
        assert_eq!(e.code, "E_NOT_FOUND", "{e}");
        // a new snapshot sees it, and the old tag's delivery is unchanged
        let fresh = handle.snapshot().unwrap();
        fresh.link_export("v2.0.0", "site", Vec::new()).unwrap();
        let mut third = Vec::new();
        fresh.link_export("v1.0.0", "site", &mut third).unwrap();
        assert_eq!(first, third);
        handle.close().unwrap();
    }
}
