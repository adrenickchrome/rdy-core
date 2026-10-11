//! The review queue: every request, oldest change first, settled pairs out; and the next request a session may take.
use crate::semantic::candidates::{Item, Request, request_label};
use crate::tensions::SideRef;
use crate::{E_SELF_REVIEW, RdyError, ReadState};
use rdy_model::{Block, Hash};
use std::collections::{BTreeSet, VecDeque};
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

    /// The actors who wrote the text item `i` carries (Review Focus 4, D-TF2W6). A change
    /// item: its change, and every other live write of the block that holds the item's content.
    /// A canon item: the changes [`ReadState::canon_item_writers`] names; none fails closed
    /// (`E_SELF_REVIEW`). From those writes the walk goes back while a write's base holds the
    /// same content (a repin or a tracking-field edit keeps the text): the base's writers count
    /// too, up to the first write that changed the content. An intent item: the intent's actor.
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
        let reader = self.reader();
        let seeds: Vec<Hash> = match &i.side {
            SideRef::Change(c) => {
                let set = self.tensions()?;
                let mut seeds = vec![*c];
                for w in set.writes().live_on(&i.block) {
                    if let Some(b) = w.new.as_deref()
                        && self.text_content(b) == i.content
                        && !seeds.contains(&w.change)
                    {
                        seeds.push(w.change);
                    }
                }
                seeds
            }
            SideRef::Canon { .. } => {
                let writers = self.canon_item_writers(&i.block, Some(i.version))?;
                if writers.is_empty() {
                    return Err(RdyError::new(
                        E_SELF_REVIEW,
                        format!(
                            "the author of block {} on side {} of {label} cannot be read, so no session is shown independent of it",
                            i.block,
                            i.side.short()
                        ),
                        "run rdy fsck; ask a person to look at the pair",
                    ));
                }
                writers
            }
            SideRef::Intent { version, .. } => return Ok(vec![reader.intent(version)?.actor]),
        };
        let mut out: Vec<String> = vec![];
        let mut seen: BTreeSet<Hash> = BTreeSet::new();
        // breadth first, so the item's own change's actor comes first
        let mut todo: VecDeque<Hash> = seeds.into();
        while let Some(c) = todo.pop_front() {
            if !seen.insert(c) {
                continue;
            }
            let change = reader.change(&c)?;
            if !out.contains(&change.provenance.actor) {
                out.push(change.provenance.actor.clone());
            }
            // the change's first op on the block names the version it started from
            let Some(base) = change
                .ops
                .iter()
                .find(|op| op.block_id() == &i.block)
                .and_then(|op| op.base())
            else {
                continue;
            };
            if self.text_content(&reader.get_block(&base)?) == i.content {
                todo.extend(self.canon_item_writers(&i.block, Some(base))?);
            }
        }
        Ok(out)
    }

    /// The content hash of `b` as an item carries it: tracking fields, pins and derived fields
    /// left out.
    fn text_content(&self, b: &Block) -> Hash {
        b.content_hash_with(self.reader().tracking())
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
