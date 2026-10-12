//! The inbox page (plan M2e2, spec §10.2; D-TPZ0W, D-V4NZX, D-Z0EMP): a session's entries from
//! its cursor, read from the log, the published state and the persisted subscription only. The
//! logged tension and suspect moves are entries of their batch, with no cap and no seen set.
use super::Entry;
use super::inflight::RunInFlight;
use super::matcher::batch_items;
use super::moves::{MoveRef, transition_entries};
use crate::events::events_for_batch;
use crate::{E_BAD_INPUT, Event, EventKind, RdyError, ReadState};
use rdy_model::Hash;
use rdy_store::StoreError;
use rdy_store::inbox::{InboxCursor, InboxSub};
use rdy_store::kinds::{self, BatchCommit};
use rdy_store::segment::Location;
use serde::Serialize;
use std::borrow::Cow;

/// The most entries one page holds.
pub const MAX_ENTRIES: usize = 200;
/// The token budget of a page when the caller gives none.
pub const DEFAULT_MAX_TOKENS: usize = 4000;

/// The cursor as `rdy_inbox` shows it: `{segment}.{offset}.{skip}`.
pub fn cursor_text(c: &InboxCursor) -> String {
    format!("{}.{}.{}", c.at.segment, c.at.offset, c.skip)
}

/// The cursor that `t` names, if `t` is three dot-separated decimal numbers.
pub fn parse_cursor(t: &str) -> Option<InboxCursor> {
    fn num<T: std::str::FromStr>(s: &str) -> Option<T> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }
    let mut parts = t.split('.');
    let (segment, offset, skip) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    Some(InboxCursor {
        at: Location {
            segment: num(segment)?,
            offset: num(offset)?,
        },
        skip: num(skip)?,
    })
}

/// The refusal of the cursor text `t`.
pub(crate) fn bad_cursor(t: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{t:?} is not an inbox cursor"),
        "pass cursor from an earlier rdy_inbox reply, such as 0.4096.0",
    )
}

/// The refusal of a cursor past the end of the log.
fn past_end(t: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("inbox cursor {t} is past the end of the log"),
        "pass cursor from an earlier rdy_inbox reply",
    )
}

/// One page of a session's inbox: its entries, the cursor after them (`cursor`, as text; `next`)
/// and whether more entries wait (`more`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InboxPage {
    pub entries: Vec<Entry>,
    pub cursor: String,
    pub more: bool,
    #[serde(skip)]
    pub next: InboxCursor,
}

/// A page's measure of an entry, as search measures a result: its JSON length / 4, plus 1.
fn tokens_of(e: &Entry) -> usize {
    serde_json::to_string(e).map_or(0, |s| s.len()) / 4 + 1
}

/// The logged move an event carries, if it is one.
fn as_move(e: &Event) -> Option<MoveRef<'_>> {
    match &e.kind {
        EventKind::TensionResolved(m) => Some(MoveRef::Resolved(m)),
        EventKind::TensionOpened(m) => Some(MoveRef::Opened(m)),
        EventKind::SuspectGone(m) => Some(MoveRef::Gone(m)),
        EventKind::SuspectNew(m) => Some(MoveRef::New(m)),
        _ => None,
    }
}

/// The entries of one batch (`events`, all of one `seq`) from raw item `skip` on, each with the
/// raw index of the last item it consumes. The raw items are the batch's other events, in order
/// (indexes `0..e`), then its logged moves in record order (`e..e+m`), as rulings task-4.1 and
/// G2 number them. Every move is passed, so a re-key fold reads its partner; a fold reports its
/// later move, so a cut never splits it.
fn batch_walk(
    st: &ReadState,
    sub: &InboxSub,
    infl: &RunInFlight,
    events: &[Event],
    skip: usize,
) -> Result<Vec<(usize, Entry)>, RdyError> {
    let Some(seq) = events.first().map(|e| e.seq) else {
        return Ok(vec![]);
    };
    // the record is the batch's last data frame, so its moves are the batch's last events; a
    // batch that breaks this is still read, with its other events cloned out in order
    let split = events
        .iter()
        .position(|e| as_move(e).is_some())
        .unwrap_or(events.len());
    let others: Cow<'_, [Event]> = if events[split..].iter().all(|e| as_move(e).is_some()) {
        Cow::Borrowed(&events[..split])
    } else {
        Cow::Owned(
            events
                .iter()
                .filter(|e| as_move(e).is_none())
                .cloned()
                .collect(),
        )
    };
    let mut out = Vec::new();
    for item in batch_items(st, sub, infl, &others, skip) {
        if let (i, Some(e)) = item? {
            out.push((i, e));
        }
    }
    let moves: Vec<MoveRef<'_>> = events.iter().filter_map(as_move).collect();
    if !moves.is_empty() {
        out.extend(
            transition_entries(sub, infl, &moves, others.len(), seq)
                .into_iter()
                .filter(|(i, _)| *i >= skip),
        );
    }
    Ok(out)
}

/// The entries of one batch's events (all of one `seq`): its matched events in order, then the
/// entries of its logged moves (plan M2e2, founder decision C, D-Z0EMP).
pub fn batch_entries(
    st: &ReadState,
    sub: &InboxSub,
    infl: &RunInFlight,
    events: &[Event],
) -> Result<Vec<Entry>, RdyError> {
    Ok(batch_walk(st, sub, infl, events, 0)?
        .into_iter()
        .map(|(_, e)| e)
        .collect())
}

/// A page being filled, batch by batch.
struct Fill {
    entries: Vec<Entry>,
    tokens: usize,
    max_tokens: usize,
    /// Everything before this cursor is delivered. Only a batch with raw items moves it (ruling
    /// task-6.1), so batches of ref writes alone, such as an ack's, never move it.
    pos: InboxCursor,
    /// The end of the last batch with raw items before the one being walked (or the start).
    prev: Location,
    /// The raw items of the next batch with events to pass over: the start's skip, then 0.
    skip: usize,
    full: bool,
}

impl Fill {
    /// Takes the entries of the batch that ends at `seq`. False when the page is full.
    fn take(&mut self, seq: Location, items: Vec<(usize, Entry)>) -> bool {
        for (i, e) in items {
            let t = tokens_of(&e);
            if self.entries.len() >= MAX_ENTRIES
                || (!self.entries.is_empty() && self.tokens.saturating_add(t) > self.max_tokens)
            {
                self.full = true;
                return false;
            }
            self.entries.push(e);
            self.tokens = self.tokens.saturating_add(t);
            self.pos = InboxCursor {
                at: self.prev,
                skip: u32::try_from(i + 1).unwrap_or(u32::MAX),
            };
        }
        self.pos = InboxCursor { at: seq, skip: 0 };
        self.prev = seq;
        true
    }
}

/// A walk error of the log from a cursor. Only a frame that does not read at the cursor itself
/// (`walked` false: no frame was read) is the cursor's fault: a location that does not start a
/// frame reads as a corrupt frame. A frame that does not read after one that did is damage to
/// the log, reported as the store's error.
fn walk_error(e: StoreError, start: &InboxCursor, walked: bool) -> RdyError {
    match e {
        StoreError::Io(io)
            if !walked
                && matches!(
                    io.kind(),
                    std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
                ) =>
        {
            bad_cursor(&cursor_text(start))
        }
        e => e.into(),
    }
}

impl ReadState {
    /// One page of the inbox of `sub` from `since` (else its persisted cursor): at most
    /// `MAX_ENTRIES` entries, cut where the next entry would pass `max_tokens` (one entry always
    /// fits). A cut inside a batch gives the cursor `{the batch before, raw items walked}`; at
    /// the end of the log, the end of the last batch walked with raw items (else the start) and
    /// `more: false` (ruling task-6.1: a batch with no raw items never moves the cursor, so an
    /// idle ack writes nothing). A cursor whose location is not the start of a batch is refused.
    pub fn inbox_page(
        &self,
        sub: &InboxSub,
        since: Option<InboxCursor>,
        max_tokens: usize,
    ) -> Result<InboxPage, RdyError> {
        let start = since.unwrap_or(sub.cursor);
        let end = self.seq();
        if start.at > end {
            return Err(past_end(&cursor_text(&start)));
        }
        let infl = self.run_in_flight(&sub.actor, sub.run.as_deref())?;
        let reader = self.reader();
        let mut fill = Fill {
            entries: Vec::new(),
            tokens: 0,
            max_tokens,
            pos: start,
            prev: start.at,
            skip: start.skip as usize,
            full: false,
        };
        let mut group: Vec<(u8, Hash)> = Vec::new();
        let mut failure: Option<RdyError> = None;
        // the first frame walked; the first marker met checks that it opens its batch
        let (mut first, mut checked) = (None, false);
        let walk = reader.for_each_frame_from(start.at, end, |loc, kind, hash, payload, next| {
            let first = *first.get_or_insert(loc);
            if kind != kinds::BATCH_COMMIT {
                group.push((kind, hash));
                return Ok(());
            }
            if !checked {
                checked = true;
                // the walk starts at `start.at`, or at the next segment's start when `start.at`
                // ends its segment: either way the first frame walked must open its batch
                if BatchCommit::decode(payload).map(|m| m.start) != Some(first) {
                    failure = Some(bad_cursor(&cursor_text(&start)));
                    return Err(StoreError::Db("the inbox cursor is inside a batch".into()));
                }
            }
            let frames = std::mem::take(&mut group);
            let step = events_for_batch(reader, next, &frames).and_then(|events| {
                if events.is_empty() {
                    // no raw items: the cursor stays where it is (ruling task-6.1)
                    return Ok(true);
                }
                let items = batch_walk(self, sub, &infl, &events, fill.skip)?;
                fill.skip = 0;
                Ok(fill.take(next, items))
            });
            match step {
                Ok(true) => Ok(()),
                // the page is full: stop the walk
                Ok(false) => Err(StoreError::Db("the inbox page is full".into())),
                Err(e) => {
                    failure = Some(e);
                    Err(StoreError::Db("the inbox read stopped".into()))
                }
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
        let (next, more) = if fill.full {
            (fill.pos, true)
        } else {
            walk.map_err(|e| walk_error(e, &start, first.is_some()))?;
            (fill.pos, false)
        };
        Ok(InboxPage {
            entries: fill.entries,
            cursor: cursor_text(&next),
            more,
            next,
        })
    }
}
