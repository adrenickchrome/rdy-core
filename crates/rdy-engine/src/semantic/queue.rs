//! The review queue: every request, oldest change first, settled pairs out; and the next request a session may take.
use crate::semantic::candidates::{Item, Request, request_label};
use crate::{RdyError, ReadState};
use rdy_model::Hash;
use std::collections::BTreeSet;
use std::ops::ControlFlow;

impl ReadState {
    /// Every review request, in queue order: the in-flight changes by (time, id), each change's
    /// requests in order; a pair once, at its first place; pairs a verdict settles left out.
    pub fn review_requests(&self) -> Result<Vec<Request>, RdyError> {
        let mut out = vec![];
        self.walk_queue(|q| {
            out.push(q);
            Ok(ControlFlow::Continue(()))
        })?;
        Ok(out)
    }

    /// The first request in queue order that is not in `leased` and has no item whose authors
    /// include `actor`. A change's requests are read only when the walk reaches that change.
    pub fn next_request(
        &self,
        actor: &str,
        leased: &BTreeSet<Hash>,
    ) -> Result<Option<Request>, RdyError> {
        let mut found = None;
        self.walk_queue(|q| {
            if leased.contains(&q.key) {
                return Ok(ControlFlow::Continue(()));
            }
            let label = request_label(&q.key);
            for i in &q.items {
                if self.authors_of(i, &label)?.iter().any(|a| a == actor) {
                    return Ok(ControlFlow::Continue(()));
                }
            }
            found = Some(q);
            Ok(ControlFlow::Break(()))
        })?;
        Ok(found)
    }

    /// The actors who wrote the text item `i` carries (Review Focus 4, D-TF2W6): the walk
    /// [`ReadState::text_authors`] makes, which the dismissal gate shares. A canon item that
    /// names no writer fails closed (`E_SELF_REVIEW`).
    /// The refusal names the item's pair as `its pair`, so a caller that knows the request uses
    /// [`ReadState::item_authors_of`].
    pub fn item_authors(&self, i: &Item) -> Result<Vec<String>, RdyError> {
        self.authors_of(i, "its pair")
    }

    /// [`ReadState::item_authors`] for an item of the request of key `key`: a refusal names the
    /// request.
    pub fn item_authors_of(&self, key: &Hash, i: &Item) -> Result<Vec<String>, RdyError> {
        self.authors_of(i, &request_label(key))
    }

    fn authors_of(&self, i: &Item, label: &str) -> Result<Vec<String>, RdyError> {
        self.text_authors(
            &i.side,
            &i.block,
            Some(i.version),
            Some(i.content),
            label,
            "run rdy fsck; ask a person to look at the pair",
        )
    }

    /// Visits the requests in queue order until `visit` breaks. Each change's requests are
    /// computed only when the walk reaches it.
    fn walk_queue(
        &self,
        mut visit: impl FnMut(Request) -> Result<ControlFlow<()>, RdyError>,
    ) -> Result<(), RdyError> {
        let set = self.tensions()?;
        let mut changes: Vec<(i64, Hash)> =
            set.writes().changes().map(|f| (f.at_ms, f.id)).collect();
        changes.sort();
        let semantic = self.semantic();
        let mut seen: BTreeSet<Hash> = BTreeSet::new();
        for (_, c) in changes {
            for q in self.requests_from(&c)? {
                if !seen.insert(q.key) || semantic.settled(&q.key).is_some() {
                    continue;
                }
                if visit(q)?.is_break() {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}
