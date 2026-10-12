//! Publishing read states and the event bus: what every write runs after it lands.

use crate::config::site_identity;
use crate::read_state::{self, CoordHeads, Entry, InflightCell};
use crate::semantic::state::SemanticState;
use crate::tensions::set::TensionCell;
use crate::{Delta, Event, Rdy, RdyError, ReadState, events, transitions};
use rdy_model::{Hash, Op, Verdict};
use rdy_store::segment::Location;
use rdy_store::transition::Cause;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock};

impl Rdy {
    /// Publishes the state after a write (snap1), re-reading what `delta` names and carrying the
    /// rest over from the current state. Every write calls it before returning `Ok`. If reading
    /// fails, the published state holds the error, every read of it returns that error, and the
    /// next publish re-reads everything; the error is returned too.
    ///
    /// Then, while the write's batch is still open, it logs how the write moved the open tensions and the suspects (plan M2e2, founder decision C) as the batch's last frame, and seals the batch: one sync. Only then is the state stored and the batch's events emitted, so a replay sends the same events. A raw write through store_mut() joins the open batch; the next publish or seal_open seals it.
    ///
    /// A batch whose events cannot be read is not the write's failure (the write is durable): its
    /// error is recorded in `last_emit_error` and the batch counts as emitted, so a later batch
    /// never masks it; a replay over it returns the same error. A failed seal is the write's
    /// failure (the store is poisoned until reopen).
    pub(crate) fn publish(&mut self, delta: Delta) -> Result<(), RdyError> {
        self.publish_logging(delta).map(drop)
    }

    /// `publish`, returning how many moves the sealed batch logged.
    fn publish_logging(&mut self, delta: Delta) -> Result<usize, RdyError> {
        if matches!(delta, Delta::Full) && self.full_publish_panics > 0 {
            self.full_publish_panics -= 1;
            panic!("test hook: a full publish panics");
        }
        let recheck = self.recheck.take();
        let prev = self.state.load_full();
        // a state that holds an error has nothing to carry over
        let delta = if prev.error.is_some() {
            Delta::Full
        } else {
            delta
        };
        match self.next_state(&prev, delta) {
            Ok(mut next) => {
                let logged = self.seal_with_moves(&prev, &next, recheck)?;
                next.seq = self.store.log_end();
                let next = Arc::new(next);
                let n = match logged {
                    Some(n) => {
                        self.logged = Some(Arc::clone(&next));
                        n
                    }
                    None => 0,
                };
                self.state.store(next);
                self.emit_last_batch();
                Ok(n)
            }
            Err(e) => {
                // with no state the moves cannot be computed: the batch is sealed without them,
                // and the next batch that computes logs every move since `logged`
                let sealed = self.store.seal(vec![]);
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
                    semantic: Arc::clone(&prev.semantic),
                    lease_ttl_ms: prev.lease_ttl_ms,
                    coverage_memo: Default::default(),
                    detectors: self.detectors,
                    error: Some(e.clone()),
                }));
                sealed?;
                Err(e)
            }
        }
    }

    /// Seals the open batch (or writes a batch of its own) with the moves between the logged
    /// state and `next`, by the closed rule: a batch with a frame that can move something logs
    /// under its own cause; else `recheck` names the cause (a `recheck` before anything was
    /// logged computes nothing); else nothing is computed. With nothing logged since open, the
    /// baseline is the store's open-move tables (`reconcile_moves`). A failure or a panic of
    /// the computation never fails the write: the batch is sealed with no record, `logged`
    /// stays, and `transition_error` holds the error. Returns the number of moves logged, or
    /// `None` when none were computed. A failed seal is returned.
    fn seal_with_moves(
        &mut self,
        prev: &ReadState,
        next: &ReadState,
        recheck: Option<Cause>,
    ) -> Result<Option<usize>, RdyError> {
        let frames: Vec<(u8, Hash)> = self.store.open_batch().unwrap_or_default().to_vec();
        let reader = self.store.reader();
        // `None`: nothing to compute; `Some(None)`: the batch's own cause; `Some(Some(c))`: c
        let cause = if !frames.is_empty() && transitions::can_move(&reader, &frames) {
            Some(None)
        } else {
            match recheck {
                Some(Cause::Recheck) if self.logged.is_none() => None,
                Some(c) => Some(Some(c)),
                None => None,
            }
        };
        let Some(cause) = cause else {
            self.store.seal(vec![])?;
            return Ok(None);
        };
        let logged = self.logged.as_deref();
        let computed = catch_unwind(AssertUnwindSafe(|| match logged {
            Some(base) => transitions::moves(base, next, &frames, cause),
            None => {
                let other = match cause {
                    Some(c) => c,
                    None if recheck == Some(Cause::PackReload) => Cause::PackReload,
                    None => Cause::Reopened,
                };
                transitions::reconcile_moves(next, Some(prev), &frames, other)
            }
        }));
        let rec = match computed {
            Ok(Ok(rec)) => rec,
            Ok(Err(e)) => return self.seal_without_moves(e),
            Err(p) => {
                let text = match p.downcast_ref::<&str>() {
                    Some(s) => (*s).to_string(),
                    None => p
                        .downcast_ref::<String>()
                        .cloned()
                        .unwrap_or_else(|| "a panic with no message".to_string()),
                };
                return self.seal_without_moves(RdyError::new(
                    crate::E_INTERNAL,
                    format!("internal error: {text}"),
                    "report this as a bug in rdy",
                ));
            }
        };
        if rec.is_empty() {
            self.store.seal(vec![])?;
        } else {
            debug_assert_eq!(rec.invalid(), None, "{rec:?}");
            self.store.seal(vec![rec.frame()])?;
        }
        self.transition_error = None;
        Ok(Some(transitions::count(&rec)))
    }

    /// Seals the open batch with no record after the moves could not be computed (`e`).
    fn seal_without_moves(&mut self, e: RdyError) -> Result<Option<usize>, RdyError> {
        self.store.seal(vec![])?;
        self.transition_error = Some(e);
        Ok(None)
    }

    /// Seals the open batch, if one is open, with its moves (`publish(Delta::Seq)`); else does
    /// nothing. A raw write through `store_mut()` stays in the open batch until this or the next
    /// publish.
    pub fn seal_open(&mut self) -> Result<(), RdyError> {
        if self.store.open_batch().is_some() {
            self.publish(Delta::Seq)
        } else {
            Ok(())
        }
    }

    /// Logs the moves between the store's open-move tables and the current state, as one batch
    /// of cause `reopened`: what changed while no rdy ran (the pack, or the rdy build). Returns
    /// the number of moves logged: 0 when the log already folds to the current sets, or when a
    /// write since open already reconciled. The daemon calls it once after `ready`.
    pub fn reconcile_transitions(&mut self) -> Result<usize, RdyError> {
        if self.logged.is_some() {
            return Ok(0);
        }
        self.recheck = Some(Cause::Reopened);
        self.publish_logging(Delta::Seq)
    }

    /// The error of the last batch whose moves could not be computed; cleared by the next
    /// batch that computes them.
    pub fn last_transition_error(&self) -> Option<&RdyError> {
        self.transition_error.as_ref()
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
        events::replay(&self.store.reader(), from, self.store.log_end())
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
        let (
            name,
            description,
            require_up_to_date,
            idle_days,
            coverage_target,
            decisions_home,
            lease_ttl_ms,
        ) = match delta {
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
                    i64::from(cfg.lease_ttl_minutes()) * 60_000,
                )
            }
            _ => (
                Arc::clone(&prev.name),
                prev.description.clone(),
                prev.require_up_to_date,
                prev.idle_days,
                prev.coverage_target,
                prev.decisions_home.clone(),
                prev.lease_ttl_ms,
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
            // a verdict or an acceptance: a built set (or one the state before can carry to)
            // is carried, its stored kinds re-detected around the pairs' blocks; else the next
            // read builds in full
            Delta::Verdict { blocks, .. } => match prev.tensions_without_build() {
                Some(set) => Arc::new(TensionCell::after_verdict(set, blocks.clone())),
                None => Arc::default(),
            },
            _ => Arc::default(),
        };
        // the verdicts and semantic tensions: re-read by a full publish, the written pairs re-read
        // by a verdict, else carried
        let semantic = match &delta {
            Delta::Full => Arc::new(SemanticState::read(&self.store.reader())?),
            Delta::Verdict { keys, .. } => {
                Arc::new(prev.semantic.reread(&self.store.reader(), keys)?)
            }
            _ => Arc::clone(&prev.semantic),
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
            Delta::Seq
            | Delta::Dismissed { .. }
            | Delta::MentionReviewed { .. }
            | Delta::Verdict { .. } => (
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
            semantic,
            lease_ttl_ms,
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
    /// computed, no op writes a block its merge holds a conflict on, every op applies cleanly to
    /// its materialization, no op creates an id that was ever deleted, and the change depends on
    /// the last writer (in the merge's order) of every block it writes: a new change is depended
    /// on by none, so it then merges after every writer of its blocks and a rebuild equals the
    /// carry. The merge settles each block on its own, so a conflict on a block the change does
    /// not write stays as it was (perf3: one open conflict made every commit merge the whole
    /// in-flight set again). Anything else starts unbuilt.
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
                if !writes_a_conflict(&b.conflicts, ops)
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
    /// The publish logs what moved since the last logged state (cause `recheck`).
    pub fn invalidate(&mut self) {
        self.recheck = Some(Cause::Recheck);
        let _ = self.publish(Delta::Full);
    }

    /// Rebuilds the store's index from its log (an open batch is sealed first) and republishes
    /// the state on the new index; the publish logs what moved since the last logged state
    /// (cause `recheck`).
    pub fn reindex(&mut self) -> Result<(), RdyError> {
        self.seal_open()?;
        self.store.reindex()?;
        self.canon_step_cache.clear();
        self.recheck = Some(Cause::Recheck);
        self.publish(Delta::Full)
    }
}

/// Whether one of `ops` writes a block that `conflicts` (a merge's, in block-id order) holds a
/// conflict on.
fn writes_a_conflict(conflicts: &[rdy_store::merge::Conflict], ops: &[rdy_model::Op]) -> bool {
    ops.iter().any(|op| {
        conflicts
            .binary_search_by(|c| c.block.cmp(op.block_id()))
            .is_ok()
    })
}
