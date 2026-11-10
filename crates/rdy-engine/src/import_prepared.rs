//! The shared import path (plan M4bc): an import decoded from its source is one prepared value.
//! It is validated, its receipt and reply are prepared, and only then are its blobs, author
//! changes, view ops and claim staged in one held batch, which seals once. Every failure before
//! the seal abandons the whole batch through the store, so no later finalizer (`Local::write`,
//! the close, a republish after a panic) can seal part of it. A refusal before append or a
//! confirmed rollback leaves the project unchanged; a rollback the store cannot prove is the
//! indeterminate refusal, and the handle stays poisoned until reopen.

use crate::error::*;
use crate::io::{
    KeyedLink, assign_import_keys, assign_missing_names, check_import_names, import_skeleton_gaps,
    parse_authors, resolve_import_keys,
};
use crate::write::WriteResult;
use crate::{Delta, Materialized, Rdy, Scope, Session};
use rdy_model::{ActorRun, Block, BlockId, Change, ChangeLinks, Hash, Op, Provenance, tree};
use rdy_schema::{has_errors, validate_block, validate_prefix};
use rdy_store::merge::Conflict;
use rdy_store::{Owner, Rollback};
use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

/// The refusal of an import whose batch the store could not prove it cut back: frames of it may
/// stay in the log, so the project may or may not hold the import.
pub(crate) fn import_indeterminate() -> RdyError {
    RdyError::new(
        E_IO,
        "import outcome is indeterminate",
        "reopen the project and inspect the target view before retrying",
    )
}

/// An import decoded from its source, before any check against the project: the blocks with
/// each one's keyed links, the authors file, the blobs and the source's label.
pub(crate) struct PreparedImport {
    pub(crate) blocks: Vec<Block>,
    /// Each block's links whose targets are keys, in `blocks` order.
    pub(crate) keyed: Vec<Vec<KeyedLink>>,
    /// The authors file's text (`None` when there is none), or the refusal its read gave: the
    /// import returns it where it checks its authors, after the duplicate check.
    pub(crate) authors: Result<Option<String>, RdyError>,
    /// Each blob's bytes, checked against its name; `blob_hashes` holds their hashes.
    pub(crate) blobs: Vec<Vec<u8>>,
    pub(crate) blob_hashes: BTreeSet<Hash>,
    /// The final component of the source directory, which the change messages name (df6).
    pub(crate) label: Option<String>,
}

impl Rdy {
    /// The checks of an import before its source is read: the role rule of every write and the
    /// target view's access.
    pub(crate) fn check_import_access(&self, s: &Session, view: &str) -> Result<(), RdyError> {
        // D-Y9DNZ: an import writes into a view under the role rule of every write; the trust
        // check is at landing, where an imported decision status needs a person
        if !s.role.can_write() {
            return Err(RdyError::new(
                E_PERMISSION,
                format!("role {:?} cannot import", s.role),
                "start the session as a writer",
            ));
        }
        crate::write::check_view_access(&self.read(), view, s, crate::views::Action::Commit)
    }

    /// Imports `prepared` into `view` as `s`: every check, key, pin, author group and the
    /// receipt before the first append, then one batch that seals once ([`Rdy::stage_import`]).
    pub(crate) fn import_prepared(
        &mut self,
        s: &Session,
        view: &str,
        prepared: PreparedImport,
    ) -> Result<WriteResult, RdyError> {
        let PreparedImport {
            mut blocks,
            keyed,
            authors,
            blobs,
            blob_hashes,
            label,
        } = prepared;
        let scope = Scope::View(view.to_string());
        // an import is a commit: it claims an unowned view and checks against the view's
        // recorded conflicts, as `write_linked` does
        let st = self.read();
        let claim = crate::views::commit_claim(&st, s, view)?;
        let conflicts = st.conflicts(&scope)?;
        let existing = st.materialized(&scope)?;
        drop(st);
        // duplicates first: a duplicated doc would otherwise read as a name clash with itself
        let mut ids = BTreeSet::new();
        for b in &blocks {
            if !ids.insert(&b.id) {
                return Err(RdyError::new(
                    E_ALREADY_EXISTS,
                    format!("block {} appears more than once in the import", b.id),
                    "remove the duplicate from the export files",
                ));
            }
        }
        drop(ids);
        let authors = parse_authors(authors?.as_deref(), &blocks)?;
        assign_missing_names(&mut blocks, &existing);
        check_import_names(&blocks, &existing)?;
        resolve_import_keys(&mut blocks, keyed, &existing)?;
        let keys = assign_import_keys(&mut blocks, &existing);
        let incoming: BTreeMap<BlockId, &Block> =
            blocks.iter().map(|b| (b.id.clone(), b)).collect();
        let parent_of = |id: &BlockId| -> Option<Option<BlockId>> {
            incoming
                .get(id)
                .map(|b| b.parent.clone())
                .or_else(|| existing.get(id).map(|b| b.parent.clone()))
        };
        for b in &blocks {
            if let Some(p) = &b.parent
                && parent_of(p).is_none()
            {
                return Err(RdyError::new(
                    E_UNKNOWN_BLOCK,
                    format!(
                        "{} has parent {p}, which is in neither the import nor the view",
                        b.id
                    ),
                    "export the whole project",
                ));
            }
            let mut seen = BTreeSet::from([b.id.clone()]);
            let mut folders = usize::from(b.block_type == tree::FOLDER);
            let mut cur = b.parent.clone();
            while let Some(c) = cur {
                if !seen.insert(c.clone()) {
                    return Err(RdyError::new(
                        E_BAD_INPUT,
                        format!("{} is part of a parent cycle", b.id),
                        "fix the parent fields in the export",
                    ));
                }
                let is_folder = incoming
                    .get(&c)
                    .copied()
                    .or_else(|| existing.get(&c))
                    .is_some_and(|p| p.block_type == tree::FOLDER);
                folders += usize::from(is_folder);
                if folders > tree::MAX_FOLDER_DEPTH {
                    break;
                }
                cur = parent_of(&c).flatten();
            }
            if folders > tree::MAX_FOLDER_DEPTH {
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!(
                        "{} sits {folders} folders deep (at most {} allowed)",
                        b.id,
                        tree::MAX_FOLDER_DEPTH
                    ),
                    format!(
                        "keep folders at most {} deep: move it higher in the export",
                        tree::MAX_FOLDER_DEPTH
                    ),
                ));
            }
        }
        let mut issues = vec![];
        for b in &blocks {
            if existing.get(&b.id).is_some() {
                return Err(RdyError::new(
                    E_ALREADY_EXISTS,
                    format!("block {} already exists in view {view}", b.id),
                    "import into a fresh project or view",
                ));
            }
            // msw1 (#1160): a create/create conflict has no base, so the view reads the block as
            // absent; importing it would silently resolve the conflict with the import's version
            if let Some(c) = conflicts.iter().find(|c| c.block == b.id) {
                return Err(RdyError::new(
                    E_ALREADY_EXISTS,
                    format!(
                        "block {} already exists in view {view}: it is conflicted ({})",
                        b.id,
                        c.id.short()
                    ),
                    "import into a fresh project or view",
                ));
            }
            let exists = |id: &BlockId| incoming.contains_key(id) || existing.get(id).is_some();
            for t in b.ref_tokens() {
                match BlockId::parse(&t) {
                    Ok(id) if exists(&id) => {}
                    _ => {
                        return Err(RdyError::new(
                            E_UNKNOWN_REF,
                            format!(
                                "{} references {{{{{t}}}}}, which is not in the import",
                                b.id
                            ),
                            format!(
                                "export the whole project, or remove the reference; {}",
                                crate::keys::literal_hint(&t)
                            ),
                        ));
                    }
                }
            }
            for l in b.links.values().flatten() {
                if !exists(&l.target) {
                    return Err(RdyError::new(
                        E_UNKNOWN_REF,
                        format!("{} links to {}, which is not in the import", b.id, l.target),
                        "export the whole project",
                    ));
                }
            }
            // blob2: an asset block's blob must be in the export's blobs/ or a blob in this
            // project; non-asset types may have a field named blob that is not an asset
            if self.pack.is_asset(&b.block_type)
                && let Some(v) = b.fields.get("blob")
            {
                let known = match v.as_str().and_then(|s| s.parse::<Hash>().ok()) {
                    Some(h) => blob_hashes.contains(&h) || self.store.has_blob(&h)?,
                    None => false,
                };
                if !known {
                    return Err(RdyError::new(
                        E_UNKNOWN_REF,
                        format!(
                            "{} names blob {}, which is neither in the export's blobs/ nor a blob in this project",
                            b.id,
                            v.as_str().unwrap_or("(not a string)")
                        ),
                        "export the whole project again: rdy export copies every figure's blob",
                    ));
                }
            }
            let parent = b
                .parent
                .as_ref()
                .and_then(|p| incoming.get(p).copied().or_else(|| existing.get(p)));
            issues.extend(validate_prefix(&self.pack, b));
            let mut found = validate_block(&self.pack, b, parent);
            crate::write::name_unknown_evidence_key(b, &mut found);
            // D-2WR0G: an import supplies every field it writes, so each decision's basis and
            // times are checked here, before any blob or change is stored
            let names: BTreeSet<String> = b.fields.keys().cloned().collect();
            crate::decision_fields::merge_set(b, &names, &mut found);
            issues.extend(found);
        }
        if let Some(e) = crate::error::nesting_refusal(&issues) {
            return Err(e);
        }
        if has_errors(&issues) {
            let mut e = RdyError::new(
                E_VALIDATION,
                "the import does not validate against this project's pack",
                "fix the listed blocks or use a compatible pack",
            );
            e.issues = issues;
            return Err(e);
        }
        // lpin1: every pin is recomputed from the imported content, as a write's create pins
        // it: each cited target (reference or link) to its content hash, the incoming block
        // with that id first, else the view's. A block is never pinned to itself. Pins in the
        // files are ignored. A removed version's stub in the view gets no pin: its tracking
        // hash cannot be recomputed, and its recorded hash would be a false one (D-3M81W).
        let tracking = self.store.tracking();
        let content = |id: &BlockId| {
            incoming
                .get(id)
                .copied()
                .or_else(|| existing.get(id))
                .filter(|t| !t.is_redacted())
                .map(|t| t.content_hash_with(tracking))
        };
        let pins: Vec<BTreeMap<BlockId, Hash>> = blocks
            .iter()
            .map(|b| {
                let refs = b.ref_tokens();
                let refs = refs.iter().filter_map(|t| BlockId::parse(t).ok());
                let links = b.links.values().flatten().map(|l| l.target.clone());
                refs.chain(links)
                    .filter(|t| *t != b.id)
                    .filter_map(|t| content(&t).map(|h| (t, h)))
                    .collect()
            })
            .collect();
        for (b, p) in blocks.iter_mut().zip(pins) {
            b.pins = p;
        }
        let warnings = import_skeleton_gaps(&self.pack, &blocks);
        // one change per author (grouped by actor), oldest first; blocks with no author row are
        // the importer's, last. Each change takes the run, role and time of its newest block
        // (largest at_ms, then id). Changes with the same time keep the file's order: each sits
        // at its author's last row with its newest time.
        struct Group {
            /// The newest block: its time, then its id.
            newest: (i64, BlockId),
            provenance: Provenance,
            /// The last row of authors.yaml with this author's newest time.
            tie_row: usize,
            ops: Vec<Op>,
        }
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        let mut mine: Vec<Op> = vec![];
        for block in blocks {
            match authors.get(&block.id) {
                Some((p, row)) => {
                    let newest = (p.at_ms, block.id.clone());
                    let g = groups.entry(p.actor.clone()).or_insert_with(|| Group {
                        newest: newest.clone(),
                        provenance: p.clone(),
                        tie_row: *row,
                        ops: vec![],
                    });
                    if p.at_ms > g.newest.0 {
                        g.tie_row = *row;
                    } else if p.at_ms == g.newest.0 {
                        g.tie_row = g.tie_row.max(*row);
                    }
                    if newest > g.newest {
                        g.newest = newest;
                        g.provenance = p.clone();
                    }
                    g.ops.push(Op::Create { block });
                }
                None => mine.push(Op::Create { block }),
            }
        }
        let mut groups: Vec<Group> = groups.into_values().collect();
        groups.sort_by_key(|g| g.tie_row);
        // stable: groups with the same time stay in row order
        groups.sort_by_key(|g| g.provenance.at_ms);
        let order: Vec<(Provenance, Vec<Op>)> =
            groups.into_iter().map(|g| (g.provenance, g.ops)).collect();
        // df6: the messages name the directory, never its path (a path can hold a user name)
        let from = label.map(|l| format!(" from {l}")).unwrap_or_default();
        let mut planned: Vec<(Provenance, Vec<Op>, String)> = order
            .into_iter()
            .map(|(p, ops)| {
                let message = format!(
                    "import{}: {} block(s) by {}, imported by {}",
                    from,
                    ops.len(),
                    p.actor,
                    s.actor
                );
                (p, ops, message)
            })
            .collect();
        // an export with no blocks still writes the importer's (empty) change, as before
        if !mine.is_empty() || planned.is_empty() {
            planned.push((s.provenance(), mine, format!("import{from}")));
        }
        // one write for every change (all or nothing). Each change keeps its author's
        // provenance; the view ops (the implicit create, a claim, the commits) and the view's
        // last write are the importer's, so the view stays the importer's, `rdy undo` by the
        // importer finds the import, and the view is not idle after old author times. Every
        // change stores the importer (FW-F7b).
        let importer = ActorRun {
            actor: s.actor.clone(),
            run: s.run.clone(),
        };
        let mut all_ops = vec![];
        let changes: Vec<Change> = planned
            .into_iter()
            .map(|(provenance, ops, message)| {
                all_ops.extend(ops.iter().cloned());
                Change {
                    view: view.into(),
                    ops,
                    deps: vec![],
                    provenance,
                    justification: None,
                    message,
                    links: ChangeLinks {
                        imported_by: Some(importer.clone()),
                        ..ChangeLinks::default()
                    },
                    read_identity: None,
                }
            })
            .collect();
        // nb9 (O9): one receipt over every author change's creates, on the final candidate,
        // before anything is appended (the blobs too); registered once the import is published
        let receipt = crate::brief::created_record(
            &self.pack,
            &existing,
            || self.read().graph(&scope),
            &all_ops,
        )?;
        // blobs go in only once the import is known to be valid and its receipt is built, so a
        // refused import stores nothing
        let committed = self.stage_import(
            view,
            &blobs,
            changes,
            &existing,
            &conflicts,
            claim,
            &s.provenance(),
            &all_ops,
        )?;
        let briefing = crate::brief::register_created(&self.briefs, receipt);
        let ids: Vec<String> = committed.iter().map(ToString::to_string).collect();
        let created = all_ops
            .iter()
            .enumerate()
            .map(|(i, op)| (format!("op{i}"), op.block_id().to_string()))
            .collect();
        Ok(WriteResult {
            change: ids.last().cloned().unwrap_or_default(),
            changes: ids,
            created,
            names: BTreeMap::new(),
            keys,
            inherited: BTreeMap::new(),
            reaffirmed: vec![],
            warnings,
            code_recorded: None,
            justification: None,
            briefing,
        })
    }

    /// Stages the import's blobs, then its author changes with the view ops and the claim, in
    /// one held batch, and publishes it through the import's own path
    /// ([`Rdy::publish_import`]), which seals it once. A batch a raw write left open is sealed
    /// first, so the import's batch holds only the import. Any failure or panic before the seal
    /// abandons the whole batch: an error is returned as it is after a confirmed rollback, else
    /// as the indeterminate refusal; a panic is resumed once the batch is abandoned. Returns the
    /// changes' ids.
    #[allow(clippy::too_many_arguments)]
    fn stage_import(
        &mut self,
        view: &str,
        blobs: &[Vec<u8>],
        changes: Vec<Change>,
        existing: &Materialized,
        conflicts: &[Conflict],
        claim: Option<Owner>,
        committer: &Provenance,
        ops: &[Op],
    ) -> Result<Vec<Hash>, RdyError> {
        self.seal_open()?;
        let staged = catch_unwind(AssertUnwindSafe(|| -> Result<Vec<Hash>, RdyError> {
            for bytes in blobs {
                self.store.put_blob(bytes)?;
            }
            fault(ImportFault::StagedBlobs)?;
            let committed = self.store.commit_many(
                view,
                changes,
                &|id| existing.version_of(id),
                conflicts,
                claim,
                committer,
            )?;
            fault(ImportFault::StagedAuthors)?;
            fault(ImportFault::Panic)?;
            // one change carries the build forward as a commit; several are read again
            let delta = match committed.as_slice() {
                [one] => Delta::Commit {
                    view: view.to_string(),
                    change: *one,
                    ops: ops.to_vec(),
                },
                _ => Delta::View {
                    view: view.to_string(),
                },
            };
            self.publish_import(delta)?;
            Ok(committed)
        }));
        match staged {
            Ok(Ok(committed)) => Ok(committed),
            Ok(Err(e)) => Err(self.abandon_import(e)),
            Err(panic) => {
                // a panic must leave no batch that `Local::lock`'s recovery, the close or
                // the daemon writer's republish could seal
                self.store.abandon_open_batch();
                resume_unwind(panic)
            }
        }
    }

    /// Abandons the import's open batch, if one is open, after error `e`: `e` itself when no
    /// batch was open or the rollback is confirmed, else the indeterminate refusal.
    pub(crate) fn abandon_import(&mut self, e: RdyError) -> RdyError {
        match self.store.abandon_open_batch() {
            Rollback::NothingOpen | Rollback::Confirmed => e,
            Rollback::Unproven => import_indeterminate(),
        }
    }
}

/// A test-support fault point of the shared import path. Every point is inert unless a test
/// armed it on its thread (`fail_next_import_at_for_tests`), which needs the `test-support`
/// feature.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportFault {
    /// After the import's blobs are staged in its batch, before its changes are.
    StagedBlobs,
    /// After the author changes and view ops are staged, before the next state is built.
    StagedAuthors,
    /// A panic at the same place as `StagedAuthors`.
    Panic,
    /// In place of building the next read state, before the seal.
    StateBuild,
    /// In place of reading the coverage baseline, after the import is committed.
    Baseline,
}

#[cfg(feature = "test-support")]
thread_local! {
    static FAULT: std::cell::Cell<Option<ImportFault>> = const { std::cell::Cell::new(None) };
}

/// Tests only: the next import on this thread fails at `at` (`E_INTERNAL`, or a panic for
/// [`ImportFault::Panic`]). It disarms when it fires.
#[cfg(feature = "test-support")]
#[doc(hidden)]
pub fn fail_next_import_at_for_tests(at: ImportFault) {
    FAULT.with(|c| c.set(Some(at)));
}

/// The fault armed at `at`, if any (it disarms).
#[cfg(feature = "test-support")]
pub(crate) fn fault(at: ImportFault) -> Result<(), RdyError> {
    if FAULT.with(|c| c.get()) != Some(at) {
        return Ok(());
    }
    FAULT.with(|c| c.set(None));
    if at == ImportFault::Panic {
        panic!("test hook: an import panics before its seal");
    }
    Err(RdyError::new(
        E_INTERNAL,
        format!("internal error: injected import failure at {at:?}"),
        "report this as a bug in rdy",
    ))
}

/// No fault is ever armed in a build without `test-support`.
#[cfg(not(feature = "test-support"))]
pub(crate) fn fault(_: ImportFault) -> Result<(), RdyError> {
    Ok(())
}
