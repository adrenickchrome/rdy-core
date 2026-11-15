//! Bounded in-memory fixture recipes for snapshot deltas (SD1 Task 7; D-RP6SM, D-918YB,
//! D-7K4Z0).
//!
//! A [`Recipe`] names a history by three numbers: how many blocks the first map holds, how many
//! landings there are, and how many entries each later landing changes. [`Recipe::build`] makes
//! every object of that history in memory (block versions, changes, proposals with summaries,
//! landings and their deltas) and hands back one batch of items, so a store takes the whole
//! history in one durable write. The maps and the deltas are made from the recipe's own rule, by
//! hand: no part of the product's fold or diff makes them, so a test can compare the product's
//! reads with [`Recipe::expected_map`], which replays the rule from nothing.
//!
//! [`Recipe::build_legacy`] makes the same history as a store that predates deltas: each landing
//! names a full SNAPSHOT and none has a delta. That form is large (every landing holds its whole
//! map), so only the bench builds it at size.
//!
//! Nothing here asserts a time. The ordinary tests use the small recipes; the bench-only test and
//! the perf workload use the fixed large ones (`PERF_*` below).
use rdy_model::{
    ActorRun, Assurance, Block, BlockId, CROCKFORD, Change, ChangeLinks, FieldValue, Hash,
    LandingRecord, Proposal, Provenance, Role, SnapshotDelta, StateSnapshot, Tag, snapshot_map_id,
};
use rdy_store::{Store, StoreError, codec, kinds};
use std::collections::BTreeMap;
use std::path::Path;

/// The id of block number `i`: `blk-` and five Crockford digits, most significant first. The
/// digits ascend in ASCII, so the order of the numbers is the order of the ids.
pub fn block_id(i: usize) -> BlockId {
    assert!(i < 32usize.pow(5), "block number {i} is out of range");
    let mut body = [0u8; 5];
    let mut n = i;
    for slot in body.iter_mut().rev() {
        *slot = CROCKFORD[n % 32];
        n /= 32;
    }
    BlockId::parse(&format!("blk-{}", std::str::from_utf8(&body).unwrap())).expect("a valid id")
}

/// The order key of block `i`: three base-62 digits and a final `1`, so the keys are valid, never
/// end in `0`, and ascend with `i` (the digits ascend in ASCII).
pub fn order_key(i: usize) -> String {
    const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    assert!(i < 62usize.pow(3), "block number {i} is out of range");
    let mut key = String::new();
    for p in [62 * 62, 62, 1] {
        key.push(DIGITS[(i / p) % 62] as char);
    }
    key.push('1');
    key
}

/// The fixed provenance of every writer and approver of a fixture: a fixed instant, so the bytes
/// of an object depend on the recipe alone.
pub fn provenance(role: Role, landing: usize) -> Provenance {
    Provenance {
        actor: match role {
            Role::Approver => "human:founder".into(),
            _ => "agent:fixture".into(),
        },
        run: None,
        role,
        at_ms: 1_700_000_000_000 + landing as i64,
    }
}

/// One history, by its three numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipe {
    /// Blocks in the first landing's map.
    pub blocks: usize,
    /// Landings, the first included.
    pub landings: usize,
    /// Entries each later landing sets: at least one, at most `blocks - 1`. Block 0 is the
    /// document every other block sits under, and no later landing changes it.
    pub sets: usize,
}

/// The long-history correctness case: 1,025 landings with 1,024 entries in every full map is
/// 1,049,600 logical snapshot entries, a full upload past the one million bound.
pub const LONG: Recipe = Recipe {
    blocks: 1024,
    landings: 1025,
    sets: 1,
};

/// The read recipe of the fixed perf workload: 10,000 blocks, 1,000 deltas, ten set entries per
/// non-root delta.
pub const PERF_READ: Recipe = Recipe {
    blocks: 10_000,
    landings: 1_000,
    sets: 10,
};

/// The upgrade recipe of the fixed perf workload and the bench-only fixture: 10,000 legacy
/// landings, each with 10,000 entries; one changed entry per later landing.
pub const PERF_UPGRADE: Recipe = Recipe {
    blocks: 10_000,
    landings: 10_000,
    sets: 1,
};

/// The land workload's base map: 10,000 blocks (the landing sets ten and drops two of them).
pub const PERF_LAND_BLOCKS: usize = 10_000;
/// Entries the land workload's landing sets.
pub const PERF_LAND_SETS: usize = 10;
/// Entries the land workload's landing drops.
pub const PERF_LAND_DROPS: usize = 2;

/// Everything one recipe made, in memory.
pub struct History {
    pub recipe: Recipe,
    /// The objects in dependency order, as one batch's items: for each landing its block
    /// versions, change, proposal and landing, then (with deltas) its delta.
    pub items: Vec<(u8, Vec<u8>)>,
    /// Each landing's id, oldest first.
    pub landings: Vec<Hash>,
    /// Each landing's delta id (empty for a legacy history).
    pub deltas: Vec<Hash>,
    /// Each landing's `state`: the id of its full map.
    pub states: Vec<Hash>,
    /// Each landing's change.
    pub changes: Vec<Hash>,
    /// The final landing's map.
    pub final_map: BTreeMap<BlockId, Hash>,
    /// Objects the batch holds, and the links they carry.
    pub objects: usize,
}

impl Recipe {
    /// Whether the numbers make a history: a first map, a landing, and sets no larger than it.
    pub fn check(&self) {
        assert!(self.blocks >= 2 && self.landings >= 1, "{self:?}");
        assert!(self.sets >= 1 && self.sets < self.blocks, "{self:?}");
    }

    /// The block numbers landing `j` (at least 1) sets: `sets` distinct numbers that walk the
    /// prose blocks (1 to `blocks - 1`) in order, wrapping.
    pub fn set_numbers(&self, j: usize) -> Vec<usize> {
        let prose = self.blocks - 1;
        (0..self.sets)
            .map(|k| 1 + ((j - 1) * self.sets + k) % prose)
            .collect()
    }

    /// The block version number `i` has after its `generation`th edit (generation 0 is the
    /// first map's).
    pub fn block(&self, i: usize, generation: usize) -> Block {
        let doc = i == 0;
        let text = |s: &str| FieldValue::Str(s.into());
        Block {
            id: block_id(i),
            block_type: if doc { "doc" } else { "prose" }.into(),
            parent: if doc { None } else { Some(block_id(0)) },
            order: order_key(i),
            fields: if doc {
                BTreeMap::from([
                    ("name".to_string(), text("fixture")),
                    ("title".to_string(), text("Fixture")),
                    ("doc_type".to_string(), text("page")),
                ])
            } else {
                BTreeMap::new()
            },
            body: if doc {
                String::new()
            } else {
                format!("Block {i}, edit {generation}.")
            },
            links: BTreeMap::new(),
            pins: BTreeMap::new(),
            foreign: Default::default(),
            read_identity: None,
        }
    }

    /// The hash of block `i` at `generation`, as its stored frame hashes.
    pub fn version(&self, i: usize, generation: usize) -> Hash {
        let (_, payload) = codec::item_new(kinds::BLOCK, &self.block(i, generation));
        Hash::of(&payload)
    }

    /// The map after landing `n` (0 is the first landing), made from nothing: the edit
    /// generation of every block is replayed from the recipe's rule, and no delta, fold or diff
    /// takes part.
    pub fn expected_map(&self, n: usize) -> BTreeMap<BlockId, Hash> {
        self.check();
        assert!(n < self.landings, "landing {n} is past the recipe");
        let mut generation = vec![0usize; self.blocks];
        for j in 1..=n {
            for i in self.set_numbers(j) {
                generation[i] = j;
            }
        }
        generation
            .iter()
            .enumerate()
            .map(|(i, g)| (block_id(i), self.version(i, *g)))
            .collect()
    }

    /// Builds the history with one delta per landing.
    pub fn build(&self) -> History {
        self.make(true)
    }

    /// Builds the same history as a store that predates deltas wrote it: each landing's `state`
    /// is the id of a stored SNAPSHOT of its full map, and no landing has a delta.
    pub fn build_legacy(&self) -> History {
        self.make(false)
    }

    fn make(&self, deltas: bool) -> History {
        self.check();
        let mut chain = Chain::new(*self, deltas);
        let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut landings = Vec::new();
        let mut delta_ids: Vec<Hash> = Vec::new();
        let mut states = Vec::new();
        let mut changes: Vec<Hash> = Vec::new();
        while let Some(step) = chain.step() {
            items.extend(step.items);
            landings.push(step.landing);
            delta_ids.extend(step.delta);
            states.push(step.state);
            changes.push(step.change);
        }
        let objects = items.len();
        History {
            recipe: *self,
            items,
            landings,
            deltas: delta_ids,
            states,
            changes,
            final_map: chain.map,
            objects,
        }
    }
}

/// One landing of a recipe: its objects, in dependency order, and its identities.
pub struct Step {
    pub items: Vec<(u8, Vec<u8>)>,
    pub landing: Hash,
    pub delta: Option<Hash>,
    pub state: Hash,
    pub change: Hash,
}

/// A recipe made one landing at a time, so a history too large to hold whole (the legacy upgrade
/// fixture, each landing with a full SNAPSHOT) streams into a store in batches.
pub struct Chain {
    recipe: Recipe,
    deltas: bool,
    next: usize,
    /// The edit generation of each block.
    generation: Vec<usize>,
    /// The live version of each block: the one list the maps come from.
    versions: Vec<Hash>,
    /// The map after the last step.
    pub map: BTreeMap<BlockId, Hash>,
    prev_landing: Option<Hash>,
    prev_delta: Option<Hash>,
    prev_change: Option<Hash>,
}

impl Chain {
    /// A chain of `recipe`: with deltas, or as a store that predates them wrote it.
    pub fn new(recipe: Recipe, deltas: bool) -> Chain {
        recipe.check();
        Chain {
            recipe,
            deltas,
            next: 0,
            generation: vec![0; recipe.blocks],
            versions: Vec::with_capacity(recipe.blocks),
            map: BTreeMap::new(),
            prev_landing: None,
            prev_delta: None,
            prev_change: None,
        }
    }

    /// The next landing, or `None` after the last.
    pub fn step(&mut self) -> Option<Step> {
        let r = self.recipe;
        let j = self.next;
        if j >= r.landings {
            return None;
        }
        self.next += 1;
        let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
        let touched: Vec<usize> = if j == 0 {
            (0..r.blocks).collect()
        } else {
            r.set_numbers(j)
        };
        let mut ops = Vec::with_capacity(touched.len());
        for &i in &touched {
            if j > 0 {
                self.generation[i] = j;
            }
            let block = r.block(i, self.generation[i]);
            let item = codec::item_new(kinds::BLOCK, &block);
            let v = Hash::of(&item.1);
            debug_assert_eq!(v, block.version_hash());
            items.push(item);
            ops.push(if j == 0 {
                rdy_model::Op::Create { block }
            } else {
                rdy_model::Op::Update {
                    base: self.versions[i],
                    block,
                }
            });
            if j == 0 {
                self.versions.push(v);
            } else {
                self.versions[i] = v;
            }
            self.map.insert(block_id(i), v);
        }
        // the change, its proposal (with the summary an approval needs) and its landing
        let change = Change {
            view: "work".into(),
            ops,
            deps: self.prev_change.into_iter().collect(),
            provenance: provenance(Role::Writer, j),
            justification: None,
            message: format!("Fixture edit {j}."),
            links: ChangeLinks::default(),
            read_identity: None,
        };
        let citem = codec::item(kinds::CHANGE, &change).expect("encode the change");
        let cid = Hash::of(&citem.1);
        items.push(citem);
        self.prev_change = Some(cid);
        let proposal = Proposal {
            stable: None,
            prev: None,
            title: format!("Fixture edit {j}"),
            view: "work".into(),
            changes: vec![cid],
            alternative: None,
            provenance: provenance(Role::Writer, j),
            canon_at: self.prev_landing,
            summary: Some("A fixture edit.".into()),
        };
        let pitem = codec::item(kinds::PROPOSAL, &proposal).expect("encode the proposal");
        let pid = Hash::of(&pitem.1);
        items.push(pitem);
        let state = snapshot_map_id(&self.map);
        if !self.deltas {
            items.push(
                codec::item(
                    kinds::SNAPSHOT,
                    &StateSnapshot {
                        blocks: self.map.clone(),
                    },
                )
                .expect("encode the snapshot"),
            );
        }
        let landing = LandingRecord {
            proposal: pid,
            prev: self.prev_landing,
            state,
            approved_by: provenance(Role::Approver, j),
            changes: vec![cid],
            chosen: None,
            checks: vec![],
            assurance: Assurance::Local,
            relayed_by: None::<ActorRun>,
            relay_evidence: None,
            approved_version: Some(pid),
            rebased_from: vec![],
            evidence: None,
            assessment: None,
            policy: None,
            read_identity: None,
        };
        let litem = codec::item(kinds::LANDING, &landing).expect("encode the landing");
        let lid = Hash::of(&litem.1);
        items.push(litem);
        let mut delta_id = None;
        if self.deltas {
            // the delta by the recipe's own rule: the first sets the whole map, a later one sets
            // the entries it changed, in id order, and drops nothing
            let mut set: Vec<(BlockId, Hash)> = touched
                .iter()
                .map(|&i| (block_id(i), self.versions[i]))
                .collect();
            set.sort();
            let delta = SnapshotDelta {
                landing: lid,
                base: self.prev_delta,
                full: state,
                set,
                drop: vec![],
            };
            let ditem = codec::item_new(kinds::SNAPSHOT_DELTA, &delta);
            let did = Hash::of(&ditem.1);
            debug_assert_eq!(did, delta.id());
            items.push(ditem);
            self.prev_delta = Some(did);
            delta_id = Some(did);
        }
        self.prev_landing = Some(lid);
        Some(Step {
            items,
            landing: lid,
            delta: delta_id,
            state,
            change: cid,
        })
    }
}

impl History {
    /// The tag that names landing `n` as `name`, tagged by the fixture's approver.
    pub fn tag(&self, name: &str, n: usize) -> Tag {
        Tag {
            name: name.into(),
            landing: self.landings[n],
            message: None,
            tagged_by: provenance(Role::Approver, n),
        }
    }

    /// Writes the whole history into `store` as one durable batch and returns its length in
    /// objects. Keep it under the batch bound of one million object records.
    pub fn write_into(&self, store: &mut Store) -> Result<usize, StoreError> {
        store.write(self.items.clone())?;
        Ok(self.items.len())
    }

    /// Writes the history in `per_batch` landings per durable batch: the construction of a
    /// history too large for one batch. Each batch is a whole number of landings, so every
    /// object a batch names is in it or before it. Returns the batches written.
    pub fn write_chunked(&self, store: &mut Store, per_batch: usize) -> Result<usize, StoreError> {
        assert!(per_batch >= 1);
        let mut batches = 0;
        let mut start = 0;
        let mut landings_in = 0;
        for (n, (k, _)) in self.items.iter().enumerate() {
            // a landing's last item is its delta when there are deltas, else the landing itself
            let last = if self.deltas.is_empty() {
                *k == kinds::LANDING
            } else {
                *k == (kinds::SNAPSHOT_DELTA | 0x80)
            };
            if last {
                landings_in += 1;
                if landings_in == per_batch {
                    store.write(self.items[start..=n].to_vec())?;
                    batches += 1;
                    start = n + 1;
                    landings_in = 0;
                }
            }
        }
        if start < self.items.len() {
            store.write(self.items[start..].to_vec())?;
            batches += 1;
        }
        Ok(batches)
    }
}

/// Creates the store at `dir` (a project store: this rdy's baseline) and writes `history` into it
/// as one batch. Returns the open store.
pub fn source_store(dir: &Path, history: &History) -> Store {
    let mut store = Store::create(dir).expect("create the source store");
    history.write_into(&mut store).expect("write the history");
    store
}

/// A nonzero object count and a block count the recipe makes, for the ordinary arithmetic checks:
/// the objects of `recipe` with deltas, without building anything.
pub fn object_count(recipe: &Recipe) -> usize {
    recipe.check();
    // the first landing: its blocks; every later landing: its sets. Each landing adds a change,
    // a proposal, the landing and its delta.
    recipe.blocks + (recipe.landings - 1) * recipe.sets + recipe.landings * 4
}

/// The logical snapshot entries of the recipe's full maps: every landing's map holds all blocks.
pub fn logical_entries(recipe: &Recipe) -> usize {
    recipe.check();
    recipe.blocks * recipe.landings
}

/// The entries the deltas of `recipe` carry in their `set` lists: the first landing's whole map
/// and each later landing's changed entries. Far below the logical count.
pub fn delta_entries(recipe: &Recipe) -> usize {
    recipe.check();
    recipe.blocks + (recipe.landings - 1) * recipe.sets
}

/// What a streamed legacy build holds: every landing and its `state`, and the last map.
pub struct LegacyFacts {
    pub landings: Vec<Hash>,
    pub states: Vec<Hash>,
    pub final_map: BTreeMap<BlockId, Hash>,
    /// Durable batches written.
    pub batches: usize,
}

/// Streams the legacy history of `recipe` into `store` (a store that predates deltas), `per_batch`
/// landings to each durable batch, never holding more than one batch in memory: the form of the
/// large upgrade fixture, whose full SNAPSHOTs together run to gigabytes.
pub fn write_legacy_streamed(
    store: &mut Store,
    recipe: Recipe,
    per_batch: usize,
) -> Result<LegacyFacts, StoreError> {
    assert!(per_batch >= 1);
    let mut chain = Chain::new(recipe, false);
    let mut landings = vec![];
    let mut states = vec![];
    let mut batch: Vec<(u8, Vec<u8>)> = vec![];
    let mut in_batch = 0;
    let mut batches = 0;
    while let Some(step) = chain.step() {
        batch.extend(step.items);
        landings.push(step.landing);
        states.push(step.state);
        in_batch += 1;
        if in_batch == per_batch {
            store.write(std::mem::take(&mut batch))?;
            batches += 1;
            in_batch = 0;
        }
    }
    if !batch.is_empty() {
        store.write(batch)?;
        batches += 1;
    }
    Ok(LegacyFacts {
        landings,
        states,
        final_map: chain.map,
        batches,
    })
}

/// A legacy project of `recipe` under `root`, as the last rdy before snapshot deltas left it: a
/// project made while the legacy writer test hook is held, its history streamed in by
/// [`write_legacy_streamed`], canon at the last landing, closed. Opening it with this rdy's
/// writer is the large upgrade.
pub fn legacy_project(root: &Path, recipe: Recipe, per_batch: usize) -> LegacyFacts {
    let _legacy = rdy_store::format::legacy_writer_for_tests();
    let mut r = rdy_engine::Rdy::init(root, "rdy-standard").expect("init the legacy project");
    let facts = write_legacy_streamed(r.store_mut(), recipe, per_batch).expect("write the history");
    let head = *facts.landings.last().expect("a landing");
    r.store_mut()
        .set_refs(vec![("canon".to_string(), head.0.to_vec())])
        .expect("point canon at the last landing");
    r.close().expect("close the project");
    facts
}
