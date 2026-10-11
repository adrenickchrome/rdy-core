//! Publishing read states and the event bus: what every write runs after it lands.

use crate::config::site_identity;
use crate::read_state::{self, CoordHeads, Entry, InflightCell};
use crate::tensions::SideRef;
use crate::tensions::set::TensionCell;
use crate::{Delta, Event, EventKind, Rdy, RdyError, ReadState, events};
use rdy_model::{Hash, Op, Verdict};
use rdy_store::kinds;
use rdy_store::segment::Location;
use rdy_store::view::ViewOpKind;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

impl Rdy {
    /// Publishes the state after a write (snap1), re-reading what `delta` names and carrying the
    /// rest over from the current state. Every write calls it before returning `Ok`. If reading
    /// fails, the published state holds the error, every read of it returns that error, and the
    /// next publish re-reads everything; the error is returned too.
    ///
    /// Then, after the state swap, it emits the events of the store's last batch if that batch
    /// has not been emitted yet (evt1): a `Full` publish from `invalidate` or `open` emits
    /// nothing new, and a batch that was fully deduplicated has no frames and emits nothing.
    /// A batch whose events cannot be read is not the write's failure (the write is durable): its
    /// error is recorded in `last_emit_error` and the batch counts as emitted, so a later batch
    /// never masks it; a replay over it returns the same error.
    /// Known gap: a write made through `store_mut()` is emitted live only if it is the last batch
    /// before the next publish; an earlier one is only in `replay_events`. M2b routes every
    /// write through the engine.
    pub(crate) fn publish(&mut self, delta: Delta) -> Result<(), RdyError> {
        if matches!(delta, Delta::Full) && self.full_publish_panics > 0 {
            self.full_publish_panics -= 1;
            panic!("test hook: a full publish panics");
        }
        self.publish_gen += 1;
        let prev = self.state.load_full();
        // nothing settled since open: the first settle diffs against the state before the
        // first write
        if self.tension_seen.is_none() && self.tension_base.is_none() {
            self.tension_base = Some(Arc::clone(&prev));
        }
        // a state that holds an error has nothing to carry over
        let delta = if prev.error.is_some() {
            Delta::Full
        } else {
            delta
        };
        match self.next_state(&prev, delta) {
            Ok(next) => {
                self.state.store(Arc::new(next));
                self.emit_last_batch();
                Ok(())
            }
            Err(e) => {
                self.state.store(Arc::new(ReadState {
                    seq: self.store.log_end(),
                    canon_head: prev.canon_head,
                    canon: Arc::clone(&prev.canon),
                    views: prev.views.clone(),
                    tags: Arc::clone(&prev.tags),
                    pack: Arc::clone(&self.pack),
                    areas: Arc::clone(&self.areas),
                    root: Arc::clone(&prev.root),
                    name: Arc::clone(&prev.name),
                    description: prev.description.clone(),
                    require_up_to_date: prev.require_up_to_date,
                    idle_days: prev.idle_days,
                    coverage_target: prev.coverage_target,
                    decisions_home: prev.decisions_home.clone(),
                    reader: self.store.reader(),
                    as_of: Arc::clone(&self.as_of),
                    vpos: Arc::clone(&self.vpos),
                    proposal_touched: Arc::clone(&self.proposal_touched),
                    canon_step_cache: Arc::clone(&self.canon_step_cache),
                    inflight_cell: Arc::clone(&prev.inflight_cell),
                    coord_heads: Arc::clone(&prev.coord_heads),
                    coord: OnceLock::new(),
                    tensions: Arc::default(),
                    dismissals: Arc::clone(&prev.dismissals),
                    mention_dismissals: Arc::clone(&prev.mention_dismissals),
                    coverage_memo: Default::default(),
                    detectors: self.detectors,
                    error: Some(e.clone()),
                }));
                Err(e)
            }
        }
    }

    fn emit_last_batch(&mut self) {
        let end = self.store.last_batch_end();
        if end > self.last_emitted {
            match events::events_for_batch(&self.store.reader(), end, self.store.last_batch()) {
                Ok(events) => {
                    for e in events {
                        self.events.publish(Arc::new(e));
                    }
                }
                Err(e) => self.emit_error = Some(e),
            }
            self.last_emitted = end;
        }
    }

    /// A receiver of every event emitted from now on (evt1). Events committed before the
    /// subscription are not sent; `replay_events` covers them.
    pub fn subscribe(&self) -> std::sync::mpsc::Receiver<Arc<Event>> {
        self.events.subscribe()
    }

    /// The events of every committed batch in the log from `from` to its end, in log order: what
    /// a subscriber would have received had it been listening since `from`. The first batch
    /// whose events cannot be read ends the walk with its error.
    pub fn replay_events(&self, from: Location) -> Result<Vec<Event>, RdyError> {
        let reader = self.store.reader();
        let mut out = Vec::new();
        let mut group: Vec<(u8, rdy_model::Hash)> = Vec::new();
        let mut failure: Option<RdyError> = None;
        let walk =
            reader.for_each_frame_from(from, self.store.log_end(), |_, kind, hash, _, next| {
                if kind == kinds::BATCH_COMMIT {
                    match events::events_for_batch(&reader, next, &group) {
                        Ok(events) => out.extend(events),
                        Err(e) => {
                            failure = Some(e);
                            // stops the walk; the batch's own error is returned below
                            return Err(rdy_store::StoreError::Db("replay stopped".into()));
                        }
                    }
                    group.clear();
                } else {
                    group.push((kind, hash));
                }
                Ok(())
            });
        if let Some(e) = failure {
            return Err(e);
        }
        walk?;
        Ok(out)
    }

    /// The error of the latest batch since open whose events could not be read and so were never
    /// emitted live; later batches that emit do not clear it. A replay over that batch returns
    /// the same error.
    pub fn last_emit_error(&self) -> Option<&RdyError> {
        self.emit_error.as_ref()
    }

    /// Test hook: the end of the last batch whose events were emitted.
    #[doc(hidden)]
    pub fn last_emitted_seq(&self) -> Location {
        self.last_emitted
    }

    fn next_state(&self, prev: &ReadState, delta: Delta) -> Result<ReadState, RdyError> {
        let seq = self.store.log_end();
        let canon_head = self.store.canon_head()?;
        // fix round 2 (FW-PF2): a first coverage read may carry the coverage the state before
        // memoised (the carry checks what it read); a full publish starts over
        let coverage_memo = match delta {
            Delta::Full => crate::coverage::CoverageMemo::default(),
            _ => crate::coverage::CoverageMemo::after(&prev.coverage_memo),
        };
        // a full publish (open, invalidate, reindex, a pack reload) takes the name, description
        // and review rules from the config loaded at open or by the last successful reload, never
        // from the files again: a broken file must not turn the review gate off (#832). Every
        // other publish carries them from the previous state.
        let (name, description, require_up_to_date, idle_days, coverage_target, decisions_home) =
            match delta {
                Delta::Full => {
                    let cfg = &self.config;
                    let (name, description) = site_identity(&prev.root, cfg);
                    (
                        name,
                        description,
                        cfg.require_up_to_date(),
                        cfg.idle_days(),
                        cfg.coverage_target(),
                        cfg.decisions_home(),
                    )
                }
                _ => (
                    Arc::clone(&prev.name),
                    prev.description.clone(),
                    prev.require_up_to_date,
                    prev.idle_days,
                    prev.coverage_target,
                    prev.decisions_home.clone(),
                ),
            };
        // the coordination refs: re-read by a publish that may have moved them, else carried. A
        // landing moves them only when it closed members of a set or chose an alternative (its
        // record says so; altg1)
        let coord_moved = match &delta {
            Delta::Full | Delta::Coord { .. } => true,
            Delta::Landed {
                landing, closed, ..
            } => !closed.is_empty() || self.store.landing(landing)?.chosen.is_some(),
            _ => false,
        };
        let coord_heads = if coord_moved {
            Arc::new(self.coord_heads(&prev.coord_heads)?)
        } else {
            Arc::clone(&prev.coord_heads)
        };
        // the tension set reads canon, the in-flight changes and the open alternative sets
        // (edit collision skips members of one set): a publish that moves none keeps it
        let tensions = match &delta {
            Delta::Seq | Delta::Tags | Delta::Dismissed { .. } | Delta::MentionReviewed { .. } => {
                Arc::clone(&prev.tensions)
            }
            Delta::Coord { closed }
                if closed.is_empty() && coord_heads.alt_members == prev.coord_heads.alt_members =>
            {
                Arc::clone(&prev.tensions)
            }
            // tcar1: a commit carries a built set, re-detecting around the blocks it writes. A
            // set the state before has not read yet but can carry is carried now (its cost
            // follows that commit), so a run of commits that nobody reads in between keeps
            // carrying instead of leaving the next read a full build
            Delta::Commit { view, change, ops } if self.carries_tensions(ops) => {
                match prev.tensions_without_build() {
                    Some(set) if self.writes_fresh(prev, &set, ops) => {
                        Arc::new(TensionCell::carrying(set, *change, view.clone()))
                    }
                    _ => Arc::default(),
                }
            }
            _ => Arc::default(),
        };
        // the dismissals: re-read by a full publish, one added by a dismissal, else carried
        let dismissals = match &delta {
            Delta::Full => Arc::new(self.store.dismissals()?.into_iter().collect()),
            Delta::Dismissed { vkey, review } => Arc::new(prev.dismissals.update(*vkey, *review)),
            _ => Arc::clone(&prev.dismissals),
        };
        // the dismissed mentions: re-read by a full publish (only the pairs whose latest review
        // dismisses), one changed by a mention review, else carried
        let mention_dismissals = match &delta {
            Delta::Full => {
                let mut map = imbl::OrdMap::new();
                for (block, entity, review) in self.store.mention_reviews()? {
                    if self.store.review(&review)?.verdict == Verdict::Dismiss {
                        map.insert((block, entity), review);
                    }
                }
                Arc::new(map)
            }
            Delta::MentionReviewed {
                block,
                entity,
                review,
                dismissed,
            } => {
                let key = (block.clone(), entity.clone());
                Arc::new(if *dismissed {
                    prev.mention_dismissals.update(key, *review)
                } else {
                    prev.mention_dismissals.without(&key)
                })
            }
            _ => Arc::clone(&prev.mention_dismissals),
        };
        let (canon, views, tags, inflight_cell) = match delta {
            Delta::Full => {
                let mut views = imbl::HashMap::new();
                for (name, _) in self.store.refs_with_prefix("view/")? {
                    let name = name.strip_prefix("view/").unwrap_or(&name).to_string();
                    if let Some(e) = self.view_entry(&name)? {
                        views.insert(name, Arc::new(e));
                    }
                }
                let canon = Entry::new(None, canon_head, None, Arc::new(vec![]));
                (
                    Arc::new(canon),
                    views,
                    Arc::new(self.version_tags()?),
                    Arc::default(),
                )
            }
            Delta::Commit { view, change, ops } => {
                let mut views = prev.views.clone();
                match self.view_entry(&view)? {
                    Some(mut next) => {
                        // the build the write applied to: the view's own on the same base; or
                        // canon's, for a view that read canon (no ref yet, or empty) and now
                        // sits on canon's head (the float catch-up may have moved its base).
                        // A build on an old base never seeds the write after a catch-up.
                        let pred = match prev.views.get(&view) {
                            Some(e) if e.base == next.base && e.built_ok().is_some() => {
                                e.built_ok()
                            }
                            Some(e) if e.included.is_empty() && next.base == prev.canon_head => {
                                prev.canon.built_ok()
                            }
                            None if next.base == prev.canon_head => prev.canon.built_ok(),
                            _ => None,
                        };
                        // carrying the build assumes the ops merge clean; a create of an id that was
                        // ever deleted merges against the delete that removed it, so that view
                        // is built again instead
                        let recreates = ops.iter().filter_map(|op| match op {
                            Op::Create { block } => Some(&block.id),
                            _ => None,
                        });
                        if let Some(b) = pred.filter(|b| b.conflicts.is_empty())
                            && !self.store.ever_deleted(recreates)?
                        {
                            next.seed(b.after(&ops, self.pack()));
                        }
                        views.insert(view.clone(), Arc::new(next));
                    }
                    None => {
                        views.remove(&view);
                    }
                }
                let inflight =
                    self.inflight_after_commit(prev, &views, &view, change, &ops, canon_head);
                (
                    Arc::clone(&prev.canon),
                    views,
                    Arc::clone(&prev.tags),
                    inflight,
                )
            }
            Delta::Landed {
                landing,
                view,
                ops,
                rebased,
                closed,
            } => {
                let mut canon = Entry::new(None, Some(landing), None, Arc::new(vec![]));
                if let Some(b) = prev.canon.built_ok() {
                    canon.seed(b.after(&ops, self.pack()));
                }
                // the proposing view and every view the landing rebased (it is one of them) start
                // unbuilt, on the landing: a stale entry would seed the next commit into it from
                // the old base
                let mut views = prev.views.clone();
                let mut names = rebased;
                if !names.contains(&view) {
                    names.push(view);
                }
                for name in names {
                    match self.view_entry(&name)? {
                        Some(next) => {
                            views.insert(name, Arc::new(next));
                        }
                        None => {
                            views.remove(&name);
                        }
                    }
                }
                // the members the landing closed (not rebased): their heads re-read, their
                // builds kept
                for name in closed {
                    self.reread_view(prev, &mut views, name)?;
                }
                (
                    Arc::new(canon),
                    views,
                    Arc::clone(&prev.tags),
                    Arc::default(),
                )
            }
            Delta::View { view } => {
                let mut views = prev.views.clone();
                self.reread_view(prev, &mut views, view)?;
                let inflight = self.inflight_after_views(prev, &views, canon_head)?;
                (
                    Arc::clone(&prev.canon),
                    views,
                    Arc::clone(&prev.tags),
                    inflight,
                )
            }
            Delta::Coord { closed } => {
                let mut views = prev.views.clone();
                for view in closed {
                    self.reread_view(prev, &mut views, view)?;
                }
                let inflight = self.inflight_after_views(prev, &views, canon_head)?;
                (
                    Arc::clone(&prev.canon),
                    views,
                    Arc::clone(&prev.tags),
                    inflight,
                )
            }
            Delta::Tags => (
                Arc::clone(&prev.canon),
                prev.views.clone(),
                Arc::new(self.version_tags()?),
                Arc::clone(&prev.inflight_cell),
            ),
            Delta::Seq | Delta::Dismissed { .. } | Delta::MentionReviewed { .. } => (
                Arc::clone(&prev.canon),
                prev.views.clone(),
                Arc::clone(&prev.tags),
                Arc::clone(&prev.inflight_cell),
            ),
        };
        Ok(ReadState {
            seq,
            canon_head,
            canon,
            views,
            tags,
            pack: Arc::clone(&self.pack),
            areas: Arc::clone(&self.areas),
            root: Arc::clone(&prev.root),
            name,
            description,
            require_up_to_date,
            idle_days,
            coverage_target,
            decisions_home,
            reader: self.store.reader(),
            as_of: Arc::clone(&self.as_of),
            vpos: Arc::clone(&self.vpos),
            proposal_touched: Arc::clone(&self.proposal_touched),
            canon_step_cache: Arc::clone(&self.canon_step_cache),
            inflight_cell,
            coord_heads,
            coord: OnceLock::new(),
            tensions,
            dismissals,
            mention_dismissals,
            coverage_memo,
            detectors: self.detectors,
            error: None,
        })
    }

    /// Whether a commit of `ops` may carry the tension set: no op is a delete, no update writes
    /// a structural block (a rename or retitle moves every descendant's path, and so its
    /// areas), and no update moves a block or changes its `lifecycle` against its base (which
    /// every descendant's effective lifecycle reads). A created structural block (a new doc,
    /// section or folder) carries: it has no descendant but the blocks the same change
    /// creates, which are seeds, so it moves no other block's path, areas or lifecycle, when
    /// its id is new (`writes_fresh`, checked once the set before is in hand). An unreadable
    /// base never carries.
    fn carries_tensions(&self, ops: &[rdy_model::Op]) -> bool {
        use rdy_model::Op;
        let structural = |ty: Option<&str>| {
            ty.and_then(|t| self.pack.block_types.get(t))
                .is_some_and(|d| d.traits.contains(&rdy_schema::Trait::Structural))
        };
        let lifecycle = |b: &rdy_model::Block| {
            b.fields
                .get("lifecycle")
                .filter(|v| **v != rdy_model::FieldValue::Null)
                .cloned()
        };
        let reader = self.store.reader();
        ops.iter().all(|op| match op {
            Op::Create { .. } => true,
            Op::Update { base, block } => {
                !structural(Some(&block.block_type))
                    && reader.get_block(base).is_ok_and(|old| {
                        old.parent == block.parent && lifecycle(&old) == lifecycle(block)
                    })
            }
            // a delete takes a block out of other blocks' reads that the seeds do not cover (an
            // `affects` target's path gives its deciders their areas), so it never carries
            Op::Delete { .. } => false,
        })
    }

    /// Whether `ops` leave every other block's reads as the seeds' re-detection sees them, by
    /// what the in-flight state before holds (`set` is its tension set): no block they write
    /// has an in-flight delete, and every block they create is new (no change ever deleted it,
    /// canon does not hold it, no in-flight change writes it). A write on a block another
    /// change deletes, or the re-create of a deleted id, gives back to blocks no seed covers
    /// what the delete took (an `affects` target's path gives its deciders their areas); a
    /// create of an id another view created is a create/create conflict the in-flight merge
    /// drops, so the blocks written under that id elsewhere lose their path. None of them
    /// carries. A failed index read never carries.
    fn writes_fresh(
        &self,
        prev: &ReadState,
        set: &crate::tensions::set::TensionSet,
        ops: &[rdy_model::Op],
    ) -> bool {
        use rdy_model::Op;
        let deleted_in_flight =
            |b: &rdy_model::BlockId| set.writes().all_on(b).iter().any(|w| w.new.is_none());
        if ops.iter().any(|op| deleted_in_flight(op.block_id())) {
            return false;
        }
        let created: Vec<&rdy_model::BlockId> = ops
            .iter()
            .filter_map(|op| match op {
                Op::Create { block } => Some(&block.id),
                _ => None,
            })
            .collect();
        if created.is_empty() {
            return true;
        }
        let Ok(canon) = prev.materialized(&crate::Scope::Canon) else {
            return false;
        };
        created
            .iter()
            .all(|b| canon.get(b).is_none() && set.writes().all_on(b).is_empty())
            && !self
                .store
                .ever_deleted(created.iter().copied())
                .unwrap_or(true)
    }

    /// Settles the tension set (tevt1): emits the difference between the last settled set and
    /// the published state's, and records the published one. A resolved tension and an opened
    /// one of the same vkey are a re-key (paired in id order): `tension_resolved` names the new
    /// id in `rekeyed_to`, `tension_opened` the old one in `rekeyed_from`. Every resolved
    /// tension is emitted first, then every opened one; `by` and `at_ms` come from the store's
    /// last batch when a batch was written since the last settle (a pack reload writes none:
    /// its events have no `by`). Before the first settle since open, the set of the state
    /// before the first write is the last settled one; with no write either, the published set
    /// is recorded and nothing is emitted. Returns the number of events. Reads never settle:
    /// the writer calls this after each write and each pack reload.
    pub fn settle_tensions(&mut self) -> Result<usize, RdyError> {
        let st = self.read();
        let generation = self.publish_gen;
        if self
            .tension_seen
            .as_ref()
            .is_some_and(|(_, g)| *g == generation)
        {
            return Ok(0);
        }
        let current = st.tensions()?;
        let prev = match (&self.tension_seen, self.tension_base.take()) {
            (Some((set, _)), _) => Some(Arc::clone(set)),
            // a base whose set cannot be built (a state that held an error) is no baseline
            (None, Some(base)) => base.tensions().ok(),
            (None, None) => None,
        };
        self.tension_seen = Some((Arc::clone(&current), generation));
        let batch = self.store.last_batch_end();
        let batch_since = batch != self.settled_batch;
        self.settled_batch = batch;
        let Some(prev) = prev else {
            return Ok(0);
        };
        if Arc::ptr_eq(&prev, &current) {
            return Ok(0);
        }
        let (opened, resolved) = prev.diff(&current);
        if opened.is_empty() && resolved.is_empty() {
            return Ok(0);
        }
        // re-keys: per vkey, the resolved and the opened tensions paired in id order
        let mut by_vkey: BTreeMap<Hash, Vec<Hash>> = BTreeMap::new();
        for t in &opened {
            by_vkey.entry(t.vkey).or_default().push(t.id);
        }
        let mut rekeyed_to: BTreeMap<Hash, Hash> = BTreeMap::new();
        let mut rekeyed_from: BTreeMap<Hash, Hash> = BTreeMap::new();
        let mut taken: BTreeMap<Hash, usize> = BTreeMap::new();
        for t in &resolved {
            let n = taken.entry(t.vkey).or_default();
            if let Some(to) = by_vkey.get(&t.vkey).and_then(|ids| ids.get(*n)) {
                rekeyed_to.insert(t.id, *to);
                rekeyed_from.insert(*to, t.id);
                *n += 1;
            }
        }
        let (by, at_ms) = if batch_since {
            self.settled_by()
        } else {
            (None, crate::now_ms())
        };
        let seq = st.seq();
        let mut events = Vec::with_capacity(opened.len() + resolved.len());
        for t in &resolved {
            events.push(EventKind::TensionResolved {
                tension: t.id,
                kind: t.kind.name().to_string(),
                by: by.clone(),
                rekeyed_to: rekeyed_to.get(&t.id).copied(),
            });
        }
        for t in &opened {
            events.push(EventKind::TensionOpened {
                tension: t.id,
                kind: t.kind.name().to_string(),
                severity: t.severity.to_string(),
                sides: t.sides.iter().map(SideRef::label).collect(),
                rekeyed_from: rekeyed_from.get(&t.id).copied(),
            });
        }
        let n = events.len();
        for kind in events {
            self.events.publish(Arc::new(Event { seq, at_ms, kind }));
        }
        Ok(n)
    }

    /// What the store's last batch did, as a resolution's `by` (`landing:<hex>` for a landing,
    /// else `change:<hex>` for a commit, else `op:<hex>` for the last view op, else none), and
    /// the `at_ms` of its last event (now when it has none).
    fn settled_by(&self) -> (Option<String>, i64) {
        let reader = self.store.reader();
        let frames = self.store.last_batch();
        let at_ms = events::events_for_batch(&reader, self.store.last_batch_end(), frames)
            .ok()
            .and_then(|e| e.last().map(|e| e.at_ms))
            .unwrap_or_else(crate::now_ms);
        if let Some((_, h)) = frames.iter().find(|(k, _)| *k == kinds::LANDING) {
            return (Some(format!("landing:{h}")), at_ms);
        }
        let ops: Vec<&Hash> = frames
            .iter()
            .filter(|(k, _)| *k == kinds::VIEW_OP)
            .map(|(_, h)| h)
            .collect();
        for h in &ops {
            if let Ok(op) = reader.view_op(h)
                && let ViewOpKind::Commit { change } = op.op
            {
                return (Some(format!("change:{change}")), at_ms);
            }
        }
        (ops.last().map(|h| format!("op:{h}")), at_ms)
    }

    /// Re-reads view `view`'s entry into `views` (`Delta::View`, and each view `Delta::Coord`
    /// closed). View ops that left the base and the set as they were (a close, an own) keep the
    /// build; any other change starts unbuilt.
    fn reread_view(
        &self,
        prev: &ReadState,
        views: &mut imbl::HashMap<String, Arc<Entry>>,
        view: String,
    ) -> Result<(), RdyError> {
        match self.view_entry(&view)? {
            Some(mut next) => {
                if let Some(e) = prev.views.get(&view)
                    && e.base == next.base
                    && e.included == next.included
                    && let Some(b) = e.built_ok()
                {
                    next.seed_shared(Arc::clone(b));
                }
                views.insert(view, Arc::new(next));
            }
            None => {
                views.remove(&view);
            }
        }
        Ok(())
    }

    /// The coordination refs as the store holds them now, with the open-set index carried from
    /// `prev` for every set that did not move (`CoordHeads::read`).
    fn coord_heads(&self, prev: &CoordHeads) -> Result<CoordHeads, RdyError> {
        let reader = self.store.reader();
        CoordHeads::read(
            &reader,
            prev,
            reader.intent_heads()?.into_iter().collect(),
            reader.alt_heads()?.into_iter().collect(),
        )
    }

    /// insc1: the in-flight cell after `change` (with `ops`) was committed to `view`. When the
    /// previous set was computed and `view` is open, the set is the previous one plus `change`
    /// (held by `view` alone). The previous build is carried, with `ops` applied, when it is
    /// computed and has no conflicts, every op applies cleanly to its materialization, no op
    /// creates an id that was ever deleted, and the change depends on the last writer (in the merge's order) of every block it writes: a new
    /// change is depended on by none, so it then merges after every writer of its blocks and a
    /// rebuild equals the carry. Anything else starts unbuilt.
    fn inflight_after_commit(
        &self,
        prev: &ReadState,
        views: &imbl::HashMap<String, Arc<Entry>>,
        view: &str,
        change: rdy_model::Hash,
        ops: &[rdy_model::Op],
        canon_head: Option<rdy_model::Hash>,
    ) -> Arc<InflightCell> {
        let Some(before) = prev.inflight_cell.set_ok() else {
            return Arc::default();
        };
        let open = views
            .get(view)
            .and_then(|e| e.head.as_ref())
            .is_some_and(|h| !h.closed);
        if !open || prev.canon_head != canon_head || before.views_of.contains_key(&change) {
            return Arc::default();
        }
        let mut set = before.as_ref().clone();
        let at = set.changes.partition_point(|c| *c < change);
        set.changes.insert(at, change);
        set.views_of.insert(change, vec![view.to_string()]);
        let cell = InflightCell::with_set(Arc::new(set));
        let carry = match (prev.inflight_cell.built_ok(), prev.inflight_cell.last.get()) {
            // a create of an id that was ever deleted merges against the delete that removed
            // it, not cleanly: such a commit (or a failed index read) rebuilds the set
            (Some(b), Some(last))
                if b.conflicts.is_empty()
                    && read_state::applies_cleanly(&b.m, ops)
                    && !self
                        .store
                        .ever_deleted(ops.iter().filter_map(|op| match op {
                            Op::Create { block } => Some(&block.id),
                            _ => None,
                        }))
                        .unwrap_or(true) =>
            {
                self.merges_last(change, ops, last).then_some((b, last))
            }
            _ => None,
        };
        if let Some((b, last)) = carry {
            let set = cell.set_ok().expect("set above");
            let mut entry = read_state::inflight_entry(canon_head, set);
            entry.seed(b.after(ops, self.pack()));
            let _ = cell.entry.set(Ok(Arc::new(entry)));
            let mut last = last.as_ref().clone();
            for op in ops {
                last.insert(op.block_id().clone(), change);
            }
            let _ = cell.last.set(Arc::new(last));
        }
        Arc::new(cell)
    }

    /// Whether `change` depends on the last in-flight writer of every block `ops` write; an
    /// unreadable change never carries.
    fn merges_last(
        &self,
        change: rdy_model::Hash,
        ops: &[rdy_model::Op],
        last: &read_state::LastWriters,
    ) -> bool {
        let writers: Vec<&rdy_model::Hash> = ops
            .iter()
            .filter_map(|op| last.get(op.block_id()))
            .collect();
        if writers.is_empty() {
            return true;
        }
        let Ok(c) = self.store.reader().change(&change) else {
            return false;
        };
        writers.iter().all(|w| c.deps.contains(w))
    }

    /// insc1: the in-flight cell after view ops (`Delta::View`, `Delta::Coord`). When the
    /// previous set was computed, the set is recomputed from `views`: an equal set keeps the
    /// previous cell, the same changes held by other views keep its build, anything else starts
    /// unbuilt. A previous set never computed leaves the new cell to compute on first read.
    fn inflight_after_views(
        &self,
        prev: &ReadState,
        views: &imbl::HashMap<String, Arc<Entry>>,
        canon_head: Option<rdy_model::Hash>,
    ) -> Result<Arc<InflightCell>, RdyError> {
        let Some(before) = prev.inflight_cell.set_ok() else {
            return Ok(Arc::default());
        };
        let set = read_state::inflight_set_of(&self.store.reader(), views)?;
        let same_base = prev.canon_head == canon_head;
        let entry_ok = !matches!(prev.inflight_cell.entry.get(), Some(Err(_)));
        if same_base && entry_ok && **before == set {
            return Ok(Arc::clone(&prev.inflight_cell));
        }
        let same_changes = same_base && before.changes == set.changes;
        let cell = InflightCell::with_set(Arc::new(set));
        if same_changes && let Some(Ok(entry)) = prev.inflight_cell.entry.get() {
            let _ = cell.entry.set(Ok(Arc::clone(entry)));
            // the shared entry may finish building through the previous state, which then
            // records `last` there only: this cell then lacks it and never carries the build
            if let Some(last) = prev.inflight_cell.last.get() {
                let _ = cell.last.set(Arc::clone(last));
            }
        }
        Ok(Arc::new(cell))
    }

    /// An unbuilt entry for view `name` as the store holds it now (head and set read together);
    /// None if the view has no ref.
    fn view_entry(&self, name: &str) -> Result<Option<Entry>, RdyError> {
        let Some(head) = self.store.view_head(name)? else {
            return Ok(None);
        };
        let included = self.store.included_at(name, &head)?;
        Ok(Some(Entry::new(
            Some(name.to_string()),
            head.base,
            Some(head),
            included,
        )))
    }

    /// Republishes a state with every cell empty (`Delta::Full`). Never fails: an error in
    /// reading the heads is kept in the state and returned by its reads. As-of builds are shared
    /// across states and kept. Call it after writing to the store outside the engine.
    pub fn invalidate(&mut self) {
        let _ = self.publish(Delta::Full);
    }

    /// Rebuilds the store's index from its log and republishes the state on the new index.
    pub fn reindex(&mut self) -> Result<(), RdyError> {
        self.store.reindex()?;
        self.canon_step_cache.clear();
        self.publish(Delta::Full)
    }
}
