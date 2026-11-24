//! Hub ports (D-A3H7S, D-DZDB7, D-W6WA5): landed changes of one line of a family, carried into a
//! new pushed view of another line. Each call reads or writes one holder. The bundle is a value
//! that travels between them; the two holders may sit on different nodes.
//!
//! - **Make.** [`Snapshot::port_bundle`] reads this snapshot alone. It takes landings of the
//!   captured hub canon only, 1 to 1,000 entries counted before repeats collapse, and emits each
//!   once, oldest first. It holds at most 1,000 changes and 67,108,864 bytes. The bundle holds no
//!   epoch, removal set or expiry. The hub makes it again for every carry attempt, and keeps
//!   none.
//! - **Bytes.** [`PortBundle::to_bytes`] gives the `rdy-port-bundle-1` form.
//!   [`PortBundle::from_bytes`] verifies the length first, then the form, the hash and the
//!   structure. A bundle is an integrity-checked export, not a signature or a proof of who made
//!   it.
//! - **Take.** [`StoreHandle::port_into_view`] writes a new pushed view under the target's one
//!   writer. The first fault wins: the length; the family; the same store; the author; the
//!   target's hub canon; the view-head compare; the target's removals; the op checks; the
//!   briefing. An existing view is [`PortOutcome::Behind`], with nothing written.
//!
//! A carried view starts on hub canon's tip at the writer's turn. It holds one new change per
//! source change, each with its source in `links.ported_from`. Its conflicts read through the
//! ordinary view scopes, `view:<name>@<op>` included. A carry makes no landing, no request and
//! no hold. The person lands and pushes the view as any other.
use crate::store::{Snapshot, StoreHandle};
use rdy_engine::RdyError;
use rdy_engine::engine::with_writer_result;
use rdy_engine::port;
use rdy_model::Hash;

pub use rdy_engine::port::{
    MAX_PORT_BUNDLE_BYTES, MAX_PORT_CHANGES, MAX_PORT_ENTRIES, PortBundleFacts, PortLandingFacts,
    PortedChange,
};
pub use rdy_engine::port_hub::{HUB_CARRY_RUN, PortOutcome, PortedView};
pub use rdy_model::PortedFrom;

/// A verified port bundle: its bytes and its facts. It holds no handle and no snapshot.
#[derive(Clone, Debug)]
pub struct PortBundle {
    inner: port::PortBundle,
}

impl PortBundle {
    /// The bundle's bytes, in the `rdy-port-bundle-1` form.
    pub fn to_bytes(&self) -> &[u8] {
        self.inner.to_bytes()
    }

    /// The bundle that `bytes` hold. The length is checked before anything is read or copied,
    /// then the form, the hash and the structure. A fault is `the port bundle does not verify`.
    pub fn from_bytes(bytes: &[u8]) -> Result<PortBundle, RdyError> {
        if bytes.len() > MAX_PORT_BUNDLE_BYTES {
            return Err(port::unverified());
        }
        Ok(PortBundle {
            inner: port::verify_bundle(bytes)?,
        })
    }

    /// What the bundle holds, in its order.
    pub fn facts(&self) -> &PortBundleFacts {
        self.inner.facts()
    }
}

impl Snapshot {
    /// The port bundle of `landings` from this holder snapshot (see the module docs). The first
    /// fault wins: a project; a removal since the snapshot; no landing; more than 1,000 entries;
    /// then each landing, in input order, that is not on the captured hub canon. Then, oldest
    /// first: more than 1,000 changes, a removed source blob, and more than 67,108,864 bytes.
    /// The removal epoch is checked again before the bundle returns.
    pub fn port_bundle(&self, landings: &[Hash]) -> Result<PortBundle, RdyError> {
        Ok(PortBundle {
            inner: port::make_bundle(self.read_state(), landings)?,
        })
    }
}

impl StoreHandle {
    /// Carries `bundle` into the new pushed view `view` of this holder, for the person `author`
    /// (see the module docs). The changes and their view ops record `author`, role Writer, run
    /// `hub-carry` and the time of the calling thread's clock. Returns
    /// [`PortOutcome::Created`] with the view's root, head, base, the source-to-target change map
    /// and its conflicts. Returns [`PortOutcome::Behind`] with the current head when the view
    /// exists. A refusal or a failed write leaves the store as it was.
    pub fn port_into_view(
        &self,
        bundle: &PortBundle,
        view: &str,
        author: &str,
    ) -> Result<PortOutcome, RdyError> {
        if bundle.to_bytes().len() > MAX_PORT_BUNDLE_BYTES {
            return Err(port::unverified());
        }
        // a bounded copy the writer owns: the verified bytes and content are shared, not grown
        let bundle = bundle.inner.clone();
        let view = view.to_string();
        let author = author.to_string();
        let at_ms = rdy_engine::now_ms();
        with_writer_result(self.local(), move |rdy| {
            rdy.port_into_view(&bundle, &view, &author, at_ms)
        })
    }
}
