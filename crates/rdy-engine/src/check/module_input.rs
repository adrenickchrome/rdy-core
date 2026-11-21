//! The input of a module rule, interface 1 (D-ZR2VZ, D-WV7ZS): one judged block, its neighbours
//! to the rule's depth, the rule's `args` and its name, as one canonical JSON value.
//!
//! - [`block_value`]: what a filter rule can read about one block, apart from global facts. Each
//!   derived value is the filter evaluator's own ([`crate::filter`]), so a module and a filter
//!   rule never disagree on a block.
//! - [`module_neighbours`]: the blocks within `depth` steps, each once at its fewest steps, over
//!   the walk that rule reach uses ([`labelled_neighbours`]).
//! - [`encode_input`]: the envelope's bytes. Compact, object keys in byte order, `\"`, `\\` and
//!   lower-case `\u00xx` for each control character, integers only. The envelope is measured
//!   before its buffer is made; past [`INPUT_BYTES_MAX`] the neighbours at the end drop and
//!   `neighbours_omitted` counts them. The judged block never drops.
//! - [`module_inputs`]: every block an input reads, for an assessment's reach.
//!
//! Everything comes from one state: the judged block, its neighbours and their derived values.
//! The order of every array and object is a byte order, never a map's or the file system's.
use crate::as_built::Resolver;
use crate::check::rules::{Bounded, Step, field_inputs, labelled_neighbours};
use crate::filter::{areas_of, lifecycle_of, planned_of, status_of, traits_of};
use crate::keys::key_of;
use crate::materialize::Materialized;
use rdy_check_wasm::INTERFACE;
use rdy_check_wasm::config::INPUT_BYTES_MAX;
use rdy_graph::{EdgeKind, Graph};
use rdy_model::{Block, BlockId, FieldValue};
use rdy_schema::filter::Field;
use rdy_schema::{AreaMatcher, ArgValue, Pack};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

/// The one field the landing sets on a decision: no rule reads it, so no input holds it.
const LANDING_CLOCK_FIELD: &str = "decided_at";

/// The largest magnitude of an integer the input holds as a number: 2^53. A stored integer past
/// it is a string of its decimal text.
const INT_RANGE: u64 = 1 << 53;

/// The state an input reads: one materialization, its graph, and the pack's types and areas.
pub struct ModuleState<'a> {
    pack: &'a Pack,
    m: &'a Materialized,
    graph: &'a Graph,
    areas: &'a AreaMatcher,
    /// The as-built status of the state, made when a value first reads it.
    as_built: OnceCell<Resolver<'a>>,
}

impl<'a> ModuleState<'a> {
    /// `graph` and `areas` must be `m`'s graph and `pack`'s areas.
    pub fn new(
        pack: &'a Pack,
        m: &'a Materialized,
        graph: &'a Graph,
        areas: &'a AreaMatcher,
    ) -> ModuleState<'a> {
        ModuleState {
            pack,
            m,
            graph,
            areas,
            as_built: OnceCell::new(),
        }
    }

    fn resolver(&self) -> &Resolver<'a> {
        self.as_built
            .get_or_init(|| Resolver::new(self.pack, self.m))
    }
}

/// What one block's object in the input holds. `None` is a fact the block lacks, which the
/// object leaves out. `fields` holds the stored fields; the object leaves out the landing clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockValue<'a> {
    pub id: &'a str,
    pub key: Option<&'a str>,
    pub block_type: &'a str,
    /// Distinct, in byte order.
    pub traits: Vec<&'static str>,
    pub path: Option<String>,
    /// The id of the doc that owns the block.
    pub doc: Option<&'a str>,
    /// Distinct, in byte order.
    pub area: Vec<String>,
    pub lifecycle: Option<&'a str>,
    pub status: Option<&'static str>,
    pub planned: Option<bool>,
    pub parent: Option<&'a str>,
    /// As stored, reference markers included; the object leaves out an empty body.
    pub body: &'a str,
    pub fields: &'a BTreeMap<String, FieldValue>,
    /// The stored links by kind, each kind's targets distinct and in byte order.
    pub links: Vec<(&'a str, Vec<&'a str>)>,
    /// The ids the block's text pins, distinct and in byte order.
    pub references: Vec<&'a str>,
}

/// `b`'s value in `st`; `b` is a block of `st`'s materialization.
pub fn block_value<'a>(st: &ModuleState<'a>, b: &'a Block) -> BlockValue<'a> {
    let mut traits = traits_of(st.pack, b);
    traits.sort_unstable_by(|x, y| x.as_bytes().cmp(y.as_bytes()));
    traits.dedup();
    // ascending and distinct already: a set's order
    let area = areas_of(st.areas, st.m, b);
    let links = b
        .links
        .iter()
        .map(|(kind, targets)| {
            let mut ids: Vec<&str> = targets.iter().map(|l| l.target.as_str()).collect();
            ids.sort_unstable_by(|x, y| x.as_bytes().cmp(y.as_bytes()));
            ids.dedup();
            (kind.as_str(), ids)
        })
        .collect();
    let mut references: Vec<&str> = st
        .graph
        .outgoing(&b.id)
        .iter()
        .filter(|e| e.kind == EdgeKind::References)
        .map(|e| e.to.as_str())
        .collect();
    references.sort_unstable_by(|x, y| x.as_bytes().cmp(y.as_bytes()));
    references.dedup();
    BlockValue {
        id: b.id.as_str(),
        key: key_of(b),
        block_type: &b.block_type,
        traits,
        path: st.m.path_of(&b.id),
        doc: st.m.doc_of(&b.id).map(|d| d.id.as_str()),
        area,
        lifecycle: lifecycle_of(st.m, b),
        status: status_of(st.graph, &b.id),
        planned: planned_of(st.resolver(), &b.id),
        parent: b.parent.as_ref().map(BlockId::as_str),
        body: &b.body,
        fields: &b.fields,
        links,
        references,
    }
}

/// One neighbour of a judged block: `hops` steps away, the last step `via` from the near block
/// `from`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Neighbour<'g> {
    pub id: &'g BlockId,
    pub hops: u8,
    pub from: &'g BlockId,
    pub via: Step<'g>,
}

/// The blocks of `m` within `depth` steps of `root`, by placement, explicit links and pinned
/// references in either direction, each once at its fewest steps. At the same count the step
/// with the smallest `(from, via)` pair wins, in byte order. Ordered by steps, then id. The root,
/// and a block `m` lacks (missing or deleted), are never neighbours, and no walk passes them.
pub fn module_neighbours<'g>(
    m: &'g Materialized,
    g: &'g Graph,
    root: &BlockId,
    depth: u8,
) -> Vec<Neighbour<'g>> {
    let mut out = Vec::new();
    walk(m, g, root, depth, |n| out.push(n));
    out
}

/// [`module_neighbours`], each given to `visit` in order and kept nowhere. The walk holds the
/// blocks it has reached and the best step to each block of the current count: references into
/// `m`, at most one of each per block of `m`.
fn walk<'g>(
    m: &'g Materialized,
    g: &'g Graph,
    root: &BlockId,
    depth: u8,
    mut visit: impl FnMut(Neighbour<'g>),
) {
    let Some(root) = m.get(root).map(|b| &b.id) else {
        return;
    };
    let mut seen: BTreeSet<&BlockId> = BTreeSet::from([root]);
    let mut level: Vec<&BlockId> = vec![root];
    for hops in 1..=depth {
        // the best step to each block first reached at this count, by id
        let mut best: BTreeMap<&BlockId, (&BlockId, Step<'g>)> = BTreeMap::new();
        for &near in &level {
            for (via, n) in labelled_neighbours(m, g, near) {
                if seen.contains(n) || m.get(n).is_none() {
                    continue;
                }
                match best.get_mut(n) {
                    Some(held) if !step_before((near, &via), (held.0, &held.1)) => {}
                    Some(held) => *held = (near, via),
                    None => {
                        best.insert(n, (near, via));
                    }
                }
            }
        }
        if best.is_empty() {
            break;
        }
        level.clear();
        for (id, (from, via)) in best {
            seen.insert(id);
            level.push(id);
            visit(Neighbour {
                id,
                hops,
                from,
                via,
            });
        }
    }
}

/// Whether the step `a` comes before `b`: the near block's id, then the step's words, in byte
/// order.
fn step_before(a: (&BlockId, &Step), b: (&BlockId, &Step)) -> bool {
    match a.0.as_str().as_bytes().cmp(b.0.as_str().as_bytes()) {
        std::cmp::Ordering::Equal => via_bytes(a.1).lt(via_bytes(b.1)),
        order => order.is_lt(),
    }
}

/// The bytes of a step's words, without making them.
fn via_bytes<'s>(step: &'s Step) -> impl Iterator<Item = u8> + 's {
    let (head, name) = via_parts(step);
    head.iter().chain(name).copied()
}

/// A step's words in two parts: `child`, `parent`, `out ` or `in `, then the edge's kind.
fn via_parts<'s>(step: &'s Step) -> (&'static [u8], &'s [u8]) {
    let (head, kind): (&[u8], Option<&EdgeKind>) = match step {
        Step::Child => (b"child", None),
        Step::Parent => (b"parent", None),
        Step::Out(kind) => (b"out ", Some(kind)),
        Step::In(kind) => (b"in ", Some(kind)),
    };
    let name: &[u8] = match kind {
        None => b"",
        Some(EdgeKind::References) => b"references",
        Some(EdgeKind::Link(name)) => name.as_bytes(),
        Some(EdgeKind::Contains) => b"contains",
    };
    (head, name)
}

/// Adds to `out` every block that the input of `root` at `depth` in `st` reads: `root`, each of
/// its neighbours to `depth` (those that the input's cap drops too, whose edits move the cut),
/// and the blocks that each of their derived values reads (ancestors for an inherited
/// lifecycle, path, doc and area; a status's incoming links; an as-built status's decisions).
/// A root that `st` lacks reads nothing. Err: `out` is full.
pub(crate) fn module_inputs(
    st: &ModuleState,
    root: &BlockId,
    depth: u8,
    out: &mut Bounded,
) -> Result<(), ()> {
    if st.m.get(root).is_none() {
        return Ok(());
    }
    value_inputs(st, root, out)?;
    let mut result = Ok(());
    walk(st.m, st.graph, root, depth, |n| {
        if result.is_ok() {
            result = value_inputs(st, n.id, out);
        }
    });
    result
}

/// Adds `b` and the blocks its value's derived facts read.
fn value_inputs(st: &ModuleState, b: &BlockId, out: &mut Bounded) -> Result<(), ()> {
    out.insert(b)?;
    for field in [
        Field::Lifecycle,
        Field::Path,
        Field::Doc,
        Field::Area,
        Field::Status,
        Field::Planned,
    ] {
        field_inputs(&field, st.m, st.graph, b, out)?;
    }
    Ok(())
}

/// Tests only: [`module_inputs`] into an unbounded set.
#[doc(hidden)]
pub fn module_inputs_for_tests(st: &ModuleState, root: &BlockId, depth: u8) -> BTreeSet<BlockId> {
    let mut out = Bounded::new(usize::MAX);
    let _ = module_inputs(st, root, depth, &mut out);
    out.set
}

/// Why a block has no input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    /// The state lacks the judged block.
    Missing,
    /// The judged block alone makes the envelope larger than [`INPUT_BYTES_MAX`] bytes.
    TooLarge,
}

/// The input envelope of rule `rule` with `args` for block `root` of `st`, its neighbours to
/// `depth` steps: at most [`INPUT_BYTES_MAX`] bytes. The neighbours at the end of the order drop
/// until the envelope, its omitted count included, fits. The root is measured before the walk
/// and the whole envelope before its buffer is made, so a root that cannot fit fails with no
/// walk and no buffer of its size, and only the neighbours that can fit are kept.
pub fn encode_input(
    st: &ModuleState,
    rule: &str,
    args: &BTreeMap<String, ArgValue>,
    root: &BlockId,
    depth: u8,
) -> Result<Vec<u8>, InputError> {
    let b = st.m.get(root).ok_or(InputError::Missing)?;
    let value = block_value(st, b);
    let head = measure(|o| put_head(o, args, &value));
    let tail = |omitted: usize| measure(|o| put_tail(o, omitted, rule));
    // the root with the smallest tail, before any walk: a root that cannot fit fails here
    let least = head.saturating_add(tail(0));
    if least > INPUT_BYTES_MAX {
        return Err(InputError::TooLarge);
    }
    // the neighbours that fit with the smallest tail, each with its entry's length; past them the
    // walk only counts. Each kept entry is at least its block's id long, so they are fewer than
    // the cap's bytes, and their values are made again as the buffer is written.
    let mut kept: Vec<(Neighbour, usize)> = Vec::new();
    let mut used = least;
    let mut full = false;
    let mut total = 0usize;
    walk(st.m, st.graph, root, depth, |n| {
        total += 1;
        if full {
            return;
        }
        let Some(nb) = st.m.get(n.id) else {
            full = true;
            return;
        };
        let nv = block_value(st, nb);
        let entry =
            measure(|o| put_neighbour(o, &n, &nv)).saturating_add(usize::from(!kept.is_empty()));
        if used.saturating_add(entry) > INPUT_BYTES_MAX {
            full = true;
            return;
        }
        used = used.saturating_add(entry);
        kept.push((n, entry));
    });
    // the real tail counts every omitted neighbour; drop from the end until the envelope fits,
    // since each dropped neighbour shrinks it
    let size = |kept: &[(Neighbour, usize)]| {
        kept.iter()
            .fold(head, |sum, k| sum.saturating_add(k.1))
            .saturating_add(tail(total - kept.len()))
    };
    let mut whole = size(&kept);
    while whole > INPUT_BYTES_MAX {
        if kept.pop().is_none() {
            return Err(InputError::TooLarge);
        }
        whole = size(&kept);
    }
    let mut buf: Vec<u8> = Vec::with_capacity(whole);
    put_head(&mut buf, args, &value);
    for (i, (n, _)) in kept.iter().enumerate() {
        if i > 0 {
            buf.put(b",");
        }
        if let Some(nb) = st.m.get(n.id) {
            put_neighbour(&mut buf, n, &block_value(st, nb));
        }
    }
    put_tail(&mut buf, total - kept.len(), rule);
    debug_assert_eq!(buf.len(), whole);
    Ok(buf)
}

/// Where encoded bytes go: a buffer, or a count that makes none.
trait Out {
    fn put(&mut self, bytes: &[u8]);
}

impl Out for Vec<u8> {
    fn put(&mut self, bytes: &[u8]) {
        self.extend_from_slice(bytes);
    }
}

/// A count of encoded bytes, saturating.
struct Measure(usize);

impl Out for Measure {
    fn put(&mut self, bytes: &[u8]) {
        self.0 = self.0.saturating_add(bytes.len());
    }
}

fn measure(write: impl FnOnce(&mut Measure)) -> usize {
    let mut m = Measure(0);
    write(&mut m);
    m.0
}

/// The envelope up to its first neighbour: `{"args":…,"block":…,"interface":1,"neighbours":[`.
fn put_head(o: &mut impl Out, args: &BTreeMap<String, ArgValue>, root: &BlockValue) {
    o.put(b"{\"args\":");
    put_args(o, args);
    o.put(b",\"block\":");
    put_block(o, root);
    o.put(b",\"interface\":");
    put_uint(o, u64::from(INTERFACE));
    o.put(b",\"neighbours\":[");
}

/// The envelope after its last neighbour: `],"neighbours_omitted":…,"rule":…}`.
fn put_tail(o: &mut impl Out, omitted: usize, rule: &str) {
    o.put(b"],\"neighbours_omitted\":");
    put_uint(o, omitted as u64);
    o.put(b",\"rule\":");
    put_str(o, rule);
    o.put(b"}");
}

fn put_neighbour(o: &mut impl Out, n: &Neighbour, value: &BlockValue) {
    o.put(b"{\"block\":");
    put_block(o, value);
    o.put(b",\"from\":");
    put_str(o, n.from.as_str());
    o.put(b",\"hops\":");
    put_uint(o, u64::from(n.hops));
    o.put(b",\"via\":\"");
    let (head, name) = via_parts(&n.via);
    put_str_body(o, head);
    put_str_body(o, name);
    o.put(b"\"}");
}

/// A block's object, its keys in byte order.
fn put_block(o: &mut impl Out, v: &BlockValue) {
    o.put(b"{\"area\":[");
    put_strs(o, v.area.iter().map(String::as_str));
    o.put(b"]");
    if !v.body.is_empty() {
        o.put(b",\"body\":");
        put_str(o, v.body);
    }
    if let Some(doc) = v.doc {
        o.put(b",\"doc\":");
        put_str(o, doc);
    }
    o.put(b",\"fields\":{");
    let mut first = true;
    for (name, value) in v.fields {
        if name == LANDING_CLOCK_FIELD {
            continue;
        }
        if !first {
            o.put(b",");
        }
        first = false;
        put_str(o, name);
        o.put(b":");
        put_field(o, value);
    }
    o.put(b"},\"id\":");
    put_str(o, v.id);
    if let Some(key) = v.key {
        o.put(b",\"key\":");
        put_str(o, key);
    }
    if let Some(lifecycle) = v.lifecycle {
        o.put(b",\"lifecycle\":");
        put_str(o, lifecycle);
    }
    o.put(b",\"links\":{");
    for (i, (kind, targets)) in v.links.iter().enumerate() {
        if i > 0 {
            o.put(b",");
        }
        put_str(o, kind);
        o.put(b":[");
        put_strs(o, targets.iter().copied());
        o.put(b"]");
    }
    o.put(b"}");
    if let Some(parent) = v.parent {
        o.put(b",\"parent\":");
        put_str(o, parent);
    }
    if let Some(path) = &v.path {
        o.put(b",\"path\":");
        put_str(o, path);
    }
    if let Some(planned) = v.planned {
        o.put(b",\"planned\":");
        o.put(if planned { b"true" } else { b"false" });
    }
    o.put(b",\"references\":[");
    put_strs(o, v.references.iter().copied());
    o.put(b"]");
    if let Some(status) = v.status {
        o.put(b",\"status\":");
        put_str(o, status);
    }
    o.put(b",\"traits\":[");
    put_strs(o, v.traits.iter().copied());
    o.put(b"],\"type\":");
    put_str(o, v.block_type);
    o.put(b"}");
}

fn put_strs<'s>(o: &mut impl Out, items: impl Iterator<Item = &'s str>) {
    for (i, s) in items.enumerate() {
        if i > 0 {
            o.put(b",");
        }
        put_str(o, s);
    }
}

/// A stored value. An integer past 2^53 is a string of its decimal text.
fn put_field(o: &mut impl Out, v: &FieldValue) {
    match v {
        FieldValue::Null => o.put(b"null"),
        FieldValue::Bool(b) => o.put(if *b { b"true" } else { b"false" }),
        FieldValue::Int(i) if i.unsigned_abs() <= INT_RANGE => put_int(o, *i),
        FieldValue::Int(i) => {
            o.put(b"\"");
            put_int(o, *i);
            o.put(b"\"");
        }
        FieldValue::Str(s) => put_str(o, s),
        FieldValue::List(items) => {
            o.put(b"[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    o.put(b",");
                }
                put_field(o, item);
            }
            o.put(b"]");
        }
        FieldValue::Map(map) => {
            o.put(b"{");
            for (i, (k, item)) in map.iter().enumerate() {
                if i > 0 {
                    o.put(b",");
                }
                put_str(o, k);
                o.put(b":");
                put_field(o, item);
            }
            o.put(b"}");
        }
    }
}

/// A rule's `args`: the pack load keeps every integer within 2^53.
fn put_args(o: &mut impl Out, args: &BTreeMap<String, ArgValue>) {
    o.put(b"{");
    for (i, (k, v)) in args.iter().enumerate() {
        if i > 0 {
            o.put(b",");
        }
        put_str(o, k);
        o.put(b":");
        put_arg(o, v);
    }
    o.put(b"}");
}

fn put_arg(o: &mut impl Out, v: &ArgValue) {
    match v {
        ArgValue::Null => o.put(b"null"),
        ArgValue::Bool(b) => o.put(if *b { b"true" } else { b"false" }),
        ArgValue::Int(i) => put_int(o, *i),
        ArgValue::Str(s) => put_str(o, s),
        ArgValue::List(items) => {
            o.put(b"[");
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    o.put(b",");
                }
                put_arg(o, item);
            }
            o.put(b"]");
        }
        ArgValue::Map(map) => put_args(o, map),
    }
}

fn put_int(o: &mut impl Out, i: i64) {
    if i < 0 {
        o.put(b"-");
    }
    put_uint(o, i.unsigned_abs());
}

fn put_uint(o: &mut impl Out, mut n: u64) {
    let mut digits = [0u8; 20];
    let mut at = digits.len();
    loop {
        at -= 1;
        digits[at] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    o.put(&digits[at..]);
}

/// A string with the minimal escapes: `\"`, `\\`, and `\u00xx` in lower-case hex for each
/// control character, never a short form such as `\n`. Every other character is its UTF-8.
fn put_str(o: &mut impl Out, s: &str) {
    o.put(b"\"");
    put_str_body(o, s.as_bytes());
    o.put(b"\"");
}

fn put_str_body(o: &mut impl Out, bytes: &[u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut start = 0;
    for (i, &c) in bytes.iter().enumerate() {
        let escape: [u8; 6];
        let with: &[u8] = match c {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            0x00..=0x1f => {
                escape = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[usize::from(c >> 4)],
                    HEX[usize::from(c & 0x0f)],
                ];
                &escape
            }
            _ => continue,
        };
        o.put(&bytes[start..i]);
        o.put(with);
        start = i + 1;
    }
    o.put(&bytes[start..]);
}
