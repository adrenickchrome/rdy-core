//! ain3 (D-8X04F, D-YQPB6, D-4W1ZR, D-A8N0E): agent instructions as one reply. A source is one
//! captured read state of a project; its layer says where it sits in the layer order. The
//! collector walks the source's canon once in doc order ([`collect`]) and gives its
//! contribution: the menu of its rulesets and the whole bodies of its required instructions and
//! of the rulesets the call loads. The merge ([`merge`]) combines the contributions in layer
//! order, resolves each requested load id and builds the [`Reply`] that MCP and the CLI
//! serialize. Neither step looks up a file or writes. A reply holds every rule it owes, or the
//! call is refused: there is no partial success, no paging and no budget cut.
//!
//! A ruleset is a doc with instructions under it. Its menu entry gives the doc's title, the
//! text of its first prose block (its lead), its layer, whether it is required, the estimated
//! tokens of its bodies and its selectors as hints. A hint never selects a rule.
use crate::embedded_guide;
use crate::error::{E_BAD_INPUT, E_INTERNAL, E_IO, E_LIMIT, E_NOT_FOUND, E_VALIDATION, RdyError};
use crate::instructions_config::{PersonPath, PersonSource, resolve_person_from_env};
use crate::query::source_body;
use crate::read_state::ReadState;
use crate::session::Scope;
use rdy_model::{Block, BlockId, FieldValue, decision};
use rdy_schema::instruction::{ALL, APPLIES_TO, INSTRUCTION, LOCKED, REQUIRED_FIELD};
use rdy_schema::{Issue, Severity, validate_read_block};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

/// The fix of every collector bound.
pub const LIMIT_FIX: &str = "reduce the selected rulesets or the required instruction set, then call rdy_instructions again";
/// The message of a load value that is not a list of ruleset ids.
pub const BAD_LOAD: &str = "load must be a list of ruleset ids";
/// Its fix.
pub const BAD_LOAD_FIX: &str = "call rdy_instructions and pass ids from its menu";
/// The fix of a load id that names no ruleset.
pub const NOT_FOUND_FIX: &str = "call rdy_instructions and load an id from its menu";
/// The fix of a bare load id that more than one layer holds.
pub const AMBIGUOUS_FIX: &str = "use the layer-qualified id from the rdy_instructions menu";

/// The work and reply bounds of one call. [`BOUNDS`] holds the numbers every call uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The most blocks one source's walk visits.
    pub visits: usize,
    /// The most rulesets one source lists.
    pub rulesets: usize,
    /// The most load ids one call takes.
    pub load_ids: usize,
    /// The most UTF-8 bytes one load id holds.
    pub load_id_bytes: usize,
    /// The most selectors one instruction holds.
    pub selectors: usize,
    /// The most UTF-8 bytes one selector holds.
    pub selector_bytes: usize,
    /// The most bytes of serialized reply JSON.
    pub reply_bytes: u64,
}

/// The bounds of every call.
pub const BOUNDS: Bounds = Bounds {
    visits: 100_000,
    rulesets: 10_000,
    load_ids: 1_024,
    load_id_bytes: 256,
    selectors: 1_024,
    selector_bytes: 256,
    reply_bytes: 16_777_216,
};

/// The refusal of a bound: `instructions exceed <limit> <unit> (found <count>)`.
fn exceeded(limit: u64, unit: &str, found: u64) -> RdyError {
    RdyError::new(
        E_LIMIT,
        format!("instructions exceed {limit} {unit} (found {found})"),
        LIMIT_FIX,
    )
}

/// The estimated tokens of `bytes` UTF-8 bytes of instruction bodies: `ceil(bytes / 4)`, a
/// deterministic estimate (no tokenizer); a nonempty body is never zero.
pub fn estimate_tokens(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}

/// A source's place in the layer order: rdy, organization, team sets, cited sets, project,
/// person. A later layer overrides an earlier one, except that a locked rdy, organization or team
/// rule wins. Delivery shows every layer's rules and removes none. Teams sort by name for display
/// only: every team has one precedence. This build emits `rdy` (the built-in guide), `project`
/// and `person`; the others are reserved.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Layer {
    Rdy,
    Organization,
    Team(String),
    Cited(String),
    Project,
    Person,
}

impl Layer {
    /// The layer's precedence: 0 for rdy up to 5 for person; equal for every team.
    pub fn precedence(&self) -> u8 {
        match self {
            Layer::Rdy => 0,
            Layer::Organization => 1,
            Layer::Team(_) => 2,
            Layer::Cited(_) => 3,
            Layer::Project => 4,
            Layer::Person => 5,
        }
    }

    /// Whether a locked rule of this layer wins over a later layer: only rdy, organization and
    /// team locks do. A local `locked: true` is shown and has no force.
    pub fn locks_have_force(&self) -> bool {
        matches!(self, Layer::Rdy | Layer::Organization | Layer::Team(_))
    }

    fn parse(s: &str) -> Option<Layer> {
        match s {
            "rdy" => Some(Layer::Rdy),
            "organization" => Some(Layer::Organization),
            "project" => Some(Layer::Project),
            "person" => Some(Layer::Person),
            _ => match s.split_once(':') {
                Some(("team", n)) if !n.is_empty() => Some(Layer::Team(n.to_string())),
                Some(("cited", n)) if !n.is_empty() => Some(Layer::Cited(n.to_string())),
                _ => None,
            },
        }
    }

    /// The layer a load handle may name: `project` or `person`.
    fn of_handle(s: &str) -> Option<Layer> {
        match s {
            "project" => Some(Layer::Project),
            "person" => Some(Layer::Person),
            _ => None,
        }
    }
}

impl fmt::Display for Layer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Layer::Rdy => f.write_str("rdy"),
            Layer::Organization => f.write_str("organization"),
            Layer::Team(n) => write!(f, "team:{n}"),
            Layer::Cited(a) => write!(f, "cited:{a}"),
            Layer::Project => f.write_str("project"),
            Layer::Person => f.write_str("person"),
        }
    }
}

impl Serialize for Layer {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Layer {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Layer, D::Error> {
        let s = Cow::<str>::deserialize(d)?;
        Layer::parse(&s).ok_or_else(|| serde::de::Error::custom(format!("unknown layer {s:?}")))
    }
}

/// What a load id names: a stored doc, or a built-in ruleset by name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LoadTarget {
    /// A stored doc: its layer when the handle is qualified (`None` for a bare id) and its id.
    Doc { layer: Option<Layer>, doc: BlockId },
    /// A built-in ruleset: the name after `rdy:`.
    Builtin(String),
}

/// One requested ruleset: `project:<doc-id>`, `person:<doc-id>`, a bare doc id that loads only
/// when one layer holds it (D-A8N0E), or `rdy:<name>` for a built-in ruleset.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LoadId {
    pub target: LoadTarget,
    /// The id as the caller gave it.
    pub given: String,
}

/// The load ids of one call, without duplicates, in first-occurrence order, and whether the call
/// asks for every built-in ruleset (`full`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadRequest {
    ids: Vec<LoadId>,
    full: bool,
}

impl LoadRequest {
    /// The request of `ids` under [`BOUNDS`].
    pub fn parse(ids: &[String]) -> Result<LoadRequest, RdyError> {
        LoadRequest::parse_with(ids, &BOUNDS)
    }

    /// The request of `ids`: their count and each one's length are checked first (`E_LIMIT`),
    /// then each one's form (`E_BAD_INPUT`).
    pub fn parse_with(ids: &[String], b: &Bounds) -> Result<LoadRequest, RdyError> {
        if ids.len() > b.load_ids {
            return Err(exceeded(b.load_ids as u64, "load ids", ids.len() as u64));
        }
        if let Some(long) = ids.iter().find(|s| s.len() > b.load_id_bytes) {
            return Err(exceeded(
                b.load_id_bytes as u64,
                "bytes in one load id",
                long.len() as u64,
            ));
        }
        let mut out: Vec<LoadId> = Vec::with_capacity(ids.len());
        let mut seen = BTreeSet::new();
        for s in ids {
            let id = parse_load_id(s).ok_or_else(bad_load)?;
            if seen.insert(s.as_str()) {
                out.push(id);
            }
        }
        Ok(LoadRequest {
            ids: out,
            full: false,
        })
    }

    /// This request with `full` set: every built-in ruleset arrives whole. Stored sources are
    /// unaffected.
    pub fn with_full(mut self, full: bool) -> LoadRequest {
        self.full = full;
        self
    }

    /// Whether the call asks for every built-in ruleset.
    pub fn full(&self) -> bool {
        self.full
    }

    /// The request of a tool's `load` value under [`BOUNDS`]: absent or null loads nothing; any
    /// value but a list of strings is `E_BAD_INPUT`.
    pub fn from_json(v: Option<&serde_json::Value>) -> Result<LoadRequest, RdyError> {
        LoadRequest::from_json_with(v, &BOUNDS)
    }

    /// [`LoadRequest::from_json`] under bounds `b`; the list's length is checked before its
    /// entries are read.
    pub fn from_json_with(
        v: Option<&serde_json::Value>,
        b: &Bounds,
    ) -> Result<LoadRequest, RdyError> {
        let items = match v {
            None | Some(serde_json::Value::Null) => return Ok(LoadRequest::default()),
            Some(serde_json::Value::Array(items)) => items,
            Some(_) => return Err(bad_load()),
        };
        if items.len() > b.load_ids {
            return Err(exceeded(b.load_ids as u64, "load ids", items.len() as u64));
        }
        let ids = items
            .iter()
            .map(|i| i.as_str().map(str::to_string).ok_or_else(bad_load))
            .collect::<Result<Vec<String>, RdyError>>()?;
        LoadRequest::parse_with(&ids, b)
    }

    pub fn ids(&self) -> &[LoadId] {
        &self.ids
    }

    /// The ids as the caller gave them, in order: what a host sends to another source, which
    /// parses them again.
    pub fn given(&self) -> Vec<&str> {
        self.ids.iter().map(|i| i.given.as_str()).collect()
    }

    /// Whether this request loads doc `doc` of `layer`, qualified or bare.
    fn names(&self, layer: &Layer, doc: &BlockId) -> bool {
        self.ids.iter().any(|i| match &i.target {
            LoadTarget::Doc { layer: l, doc: d } => {
                d == doc && l.as_ref().is_none_or(|l| l == layer)
            }
            LoadTarget::Builtin(_) => false,
        })
    }

    /// Whether this request loads built-in ruleset `name`.
    fn names_builtin(&self, name: &str) -> bool {
        self.ids
            .iter()
            .any(|i| matches!(&i.target, LoadTarget::Builtin(n) if n == name))
    }
}

fn bad_load() -> RdyError {
    RdyError::new(E_BAD_INPUT, BAD_LOAD, BAD_LOAD_FIX)
}

/// Whether `n` is a built-in name: 1-64 ASCII kebab-case bytes.
fn builtin_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && !n.starts_with('-')
        && !n.ends_with('-')
        && !n.contains("--")
        && n.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn parse_load_id(s: &str) -> Option<LoadId> {
    let target = match s.split_once(':') {
        Some(("rdy", name)) => {
            if !builtin_name(name) {
                return None;
            }
            LoadTarget::Builtin(name.to_string())
        }
        Some((l, doc)) => LoadTarget::Doc {
            layer: Some(Layer::of_handle(l)?),
            doc: BlockId::parse(doc).ok()?,
        },
        None => LoadTarget::Doc {
            layer: None,
            doc: BlockId::parse(s).ok()?,
        },
    };
    Some(LoadId {
        target,
        given: s.to_string(),
    })
}

/// The load handle of doc `doc` in `layer`: `<layer>:<doc-id>`.
fn handle(layer: &Layer, doc: &BlockId) -> String {
    format!("{layer}:{doc}")
}

/// One reason of an instruction: a `decided_by` decision's id and its statement (the source
/// read's notice when its content was removed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reason<S = String> {
    pub id: S,
    pub statement: S,
}

/// One delivered instruction: its source block id, its ruleset's load handle, its whole body,
/// its layer, its stored lock and its reasons.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionRow<S = String> {
    pub id: S,
    pub ruleset: S,
    pub text: S,
    pub layer: Layer,
    pub locked: bool,
    pub reason: Vec<Reason<S>>,
}

/// One ruleset on the menu: its load handle, the doc's title, its lead, its layer, whether the
/// doc is required, the estimated tokens of its bodies and its distinct selectors.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuEntry<S = String> {
    pub id: S,
    pub name: S,
    pub when: S,
    pub layer: Layer,
    pub required: bool,
    pub tokens: u64,
    pub hints: Vec<S>,
}

/// The reply of one instructions call, the same for MCP and the CLI: the required and loaded
/// instructions, the menu, and the host's warnings and hint.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub instructions: Vec<InstructionRow>,
    pub menu: Vec<MenuEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl Reply {
    /// The reply's JSON under [`BOUNDS`].
    pub fn to_json(&self) -> Result<Vec<u8>, RdyError> {
        self.to_json_with(&BOUNDS)
    }

    /// The reply's JSON: its bytes are counted first, without being kept, and a reply past
    /// `b.reply_bytes` is `E_LIMIT` before any buffer is allocated.
    pub fn to_json_with(&self, b: &Bounds) -> Result<Vec<u8>, RdyError> {
        let n = json_len(self);
        if n > b.reply_bytes {
            return Err(exceeded(b.reply_bytes, "bytes of reply JSON", n));
        }
        let mut out = Vec::with_capacity(n as usize);
        serde_json::to_writer(&mut out, self)
            .map_err(|e| RdyError::new(E_INTERNAL, e.to_string(), "report this as a bug in rdy"))?;
        Ok(out)
    }
}

/// One source's part of a reply: its layer, its menu and its delivered rows. Only [`collect`]
/// makes one; a host carries it between processes as JSON and [`merge`] consumes it. It is not
/// a tool, a reply or a stored object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contribution {
    layer: Layer,
    menu: Vec<MenuEntry>,
    instructions: Vec<InstructionRow>,
}

impl Contribution {
    /// The layer the source was collected as.
    pub fn layer(&self) -> &Layer {
        &self.layer
    }
}

/// A writer that counts the bytes written to it and keeps none.
struct Count(u64);

impl std::io::Write for Count {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.saturating_add(buf.len() as u64);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The bytes of `v`'s JSON, escapes included, counted without building it.
pub fn json_len<T: Serialize + ?Sized>(v: &T) -> u64 {
    let mut c = Count(0);
    // a count never fails to write, and these types always serialize
    let _ = serde_json::to_writer(&mut c, v);
    c.0
}

/// The borrowed shape of a reply, for counting a source's bytes before anything is copied.
#[derive(Serialize)]
struct ReplyView<'a, S: Serialize> {
    instructions: &'a [InstructionRow<S>],
    menu: &'a [MenuEntry<S>],
}

/// One doc met on the walk: its block, its first prose block and its instructions.
struct DocAcc<'a> {
    doc: &'a Block,
    lead: Option<&'a Block>,
    rules: Vec<&'a Block>,
}

/// [`collect_with`] under [`BOUNDS`].
pub fn collect(
    state: &ReadState,
    layer: Layer,
    load: &LoadRequest,
) -> Result<Contribution, RdyError> {
    collect_with(state, layer, load, &BOUNDS)
}

/// The contribution of the source `state` as layer `layer`: its canon (never a view, D-A8N0E),
/// walked once in doc order. The menu lists every ruleset. The rows are, in doc order, every
/// instruction of a required doc or of a ruleset `load` names in this layer, and every
/// instruction whose selectors hold `all`. Each delivered instruction must pass the read schema
/// and name decisions as its reasons (`E_VALIDATION`, labelled with the source). Every count is
/// checked before anything is copied (`E_LIMIT`). A store refusal stays the refusal.
pub fn collect_with(
    state: &ReadState,
    layer: Layer,
    load: &LoadRequest,
    b: &Bounds,
) -> Result<Contribution, RdyError> {
    if layer == Layer::Rdy {
        return Err(RdyError::new(
            E_INTERNAL,
            "a stored source cannot be the rdy layer",
            "report this as a bug in the host",
        ));
    }
    let m = state.materialized(&Scope::Canon)?;
    if m.len() > b.visits {
        return Err(exceeded(
            b.visits as u64,
            "blocks in one source",
            m.len() as u64,
        ));
    }
    // the walk: depth first, in order; each block knows the doc it is in
    let mut docs: Vec<DocAcc> = vec![];
    let mut stack: Vec<(&Block, Option<usize>)> = m
        .children(None)
        .into_iter()
        .rev()
        .map(|c| (c, None))
        .collect();
    let mut visited = 0usize;
    while let Some((blk, inside)) = stack.pop() {
        visited += 1;
        if visited > b.visits {
            return Err(exceeded(
                b.visits as u64,
                "blocks in one source",
                visited as u64,
            ));
        }
        let here = if blk.block_type == rdy_model::tree::DOC {
            docs.push(DocAcc {
                doc: blk,
                lead: None,
                rules: vec![],
            });
            Some(docs.len() - 1)
        } else {
            inside
        };
        if let Some(i) = here.filter(|_| blk.block_type != rdy_model::tree::DOC) {
            let acc = &mut docs[i];
            if blk.block_type == INSTRUCTION {
                acc.rules.push(blk);
            } else if blk.block_type == "prose"
                && acc.lead.is_none()
                && !source_body(blk).trim().is_empty()
            {
                acc.lead = Some(blk);
            }
        }
        stack.extend(
            m.children(Some(&blk.id))
                .into_iter()
                .rev()
                .map(|c| (c, here)),
        );
    }
    let sets: Vec<&DocAcc> = docs.iter().filter(|d| !d.rules.is_empty()).collect();
    if sets.len() > b.rulesets {
        return Err(exceeded(b.rulesets as u64, "rulesets", sets.len() as u64));
    }
    // the selectors of every rule, bounded before any is copied
    for set in &sets {
        for r in &set.rules {
            if let Some(FieldValue::List(items)) = r.fields.get(APPLIES_TO) {
                if items.len() > b.selectors {
                    return Err(exceeded(
                        b.selectors as u64,
                        "selectors in one instruction",
                        items.len() as u64,
                    ));
                }
                if let Some(n) = items
                    .iter()
                    .filter_map(FieldValue::as_str)
                    .map(str::len)
                    .find(|n| *n > b.selector_bytes)
                {
                    return Err(exceeded(
                        b.selector_bytes as u64,
                        "bytes in one selector",
                        n as u64,
                    ));
                }
            }
        }
    }
    let pack = state.pack();
    let mut menu: Vec<MenuEntry<Cow<str>>> = Vec::with_capacity(sets.len());
    let mut rows: Vec<InstructionRow<Cow<str>>> = vec![];
    for set in &sets {
        let id = handle(&layer, &set.doc.id);
        let required = set.doc.fields.get(REQUIRED_FIELD) == Some(&FieldValue::Bool(true));
        let whole = required || load.names(&layer, &set.doc.id);
        let mut bytes: u64 = 0;
        let mut hints: Vec<Cow<str>> = vec![];
        let mut seen = BTreeSet::new();
        for r in &set.rules {
            let text = text_of(r);
            bytes = bytes
                .checked_add(text.len() as u64)
                .ok_or_else(|| exceeded(u64::MAX, "bytes of instruction bodies", u64::MAX))?;
            let sels = selectors(r);
            for s in &sels {
                if seen.insert(*s) {
                    hints.push(Cow::Borrowed(*s));
                }
            }
            if whole || sels.contains(&ALL) {
                check(pack, &m, r, &layer)?;
                rows.push(InstructionRow {
                    id: Cow::Borrowed(r.id.as_str()),
                    ruleset: Cow::Owned(id.clone()),
                    text,
                    layer: layer.clone(),
                    locked: r.fields.get(LOCKED) == Some(&FieldValue::Bool(true)),
                    reason: reasons(&m, r),
                });
            }
        }
        menu.push(MenuEntry {
            id: Cow::Owned(id),
            name: Cow::Borrowed(set.doc.field_str("title").unwrap_or("")),
            when: set.lead.map(text_of).unwrap_or(Cow::Borrowed("")),
            layer: layer.clone(),
            required,
            tokens: estimate_tokens(bytes),
            hints,
        });
    }
    let n = json_len(&ReplyView {
        instructions: &rows,
        menu: &menu,
    });
    if n > b.reply_bytes {
        return Err(exceeded(b.reply_bytes, "bytes of reply JSON", n));
    }
    Ok(Contribution {
        layer,
        menu: menu.into_iter().map(owned_entry).collect(),
        instructions: rows.into_iter().map(owned_row).collect(),
    })
}

/// A block's text as a source read shows it: its body, or the notice of removed content.
fn text_of(b: &Block) -> Cow<'_, str> {
    if b.is_redacted() {
        Cow::Owned(source_body(b))
    } else {
        Cow::Borrowed(b.body.as_str())
    }
}

/// The string selectors of an instruction, in order.
fn selectors(b: &Block) -> Vec<&str> {
    match b.fields.get(APPLIES_TO) {
        Some(FieldValue::List(items)) => items.iter().filter_map(FieldValue::as_str).collect(),
        _ => vec![],
    }
}

/// The read schema's issues of a delivered instruction, and its reason check: every
/// `decided_by` target is a decision of the source (a removed version keeps no links and is
/// not checked). Any error refuses the call with the source's label and the original issues.
fn check(
    pack: &rdy_schema::Pack,
    m: &crate::Materialized,
    r: &Block,
    layer: &Layer,
) -> Result<(), RdyError> {
    let parent = r.parent.as_ref().and_then(|p| m.get(p));
    let mut issues: Vec<Issue> = validate_read_block(pack, r, parent)
        .into_iter()
        .filter(|i| i.severity == Severity::Error)
        .collect();
    if !r.is_redacted() && issues.is_empty() {
        let targets = r.links.get(decision::DECIDED_BY);
        let ok = targets.is_some_and(|ls| {
            !ls.is_empty()
                && ls.iter().all(|l| {
                    m.get(&l.target)
                        .is_some_and(|t| t.block_type == decision::DECISION)
                })
        });
        if !ok {
            issues.push(rdy_schema::instruction::reason_issue(r));
        }
    }
    match issues.first() {
        None => Ok(()),
        Some(first) => {
            let mut e = RdyError::new(
                E_VALIDATION,
                format!("{layer} source: {}", first.message),
                first.fix.clone(),
            );
            e.issues = issues;
            Err(e)
        }
    }
}

/// The reasons of a delivered instruction, in link order: each decision's id and statement.
fn reasons<'a>(m: &'a crate::Materialized, r: &'a Block) -> Vec<Reason<Cow<'a, str>>> {
    r.links
        .get(decision::DECIDED_BY)
        .map(|ls| {
            ls.iter()
                .filter_map(|l| m.get(&l.target))
                .map(|t| Reason {
                    id: Cow::Borrowed(t.id.as_str()),
                    statement: if t.is_redacted() {
                        Cow::Owned(source_body(t))
                    } else {
                        Cow::Borrowed(t.field_str("statement").unwrap_or(""))
                    },
                })
                .collect()
        })
        .unwrap_or_default()
}

fn owned_row(r: InstructionRow<Cow<str>>) -> InstructionRow {
    InstructionRow {
        id: r.id.into_owned(),
        ruleset: r.ruleset.into_owned(),
        text: r.text.into_owned(),
        layer: r.layer,
        locked: r.locked,
        reason: r
            .reason
            .into_iter()
            .map(|x| Reason {
                id: x.id.into_owned(),
                statement: x.statement.into_owned(),
            })
            .collect(),
    }
}

fn owned_entry(e: MenuEntry<Cow<str>>) -> MenuEntry {
    MenuEntry {
        id: e.id.into_owned(),
        name: e.name.into_owned(),
        when: e.when.into_owned(),
        layer: e.layer,
        required: e.required,
        tokens: e.tokens,
        hints: e.hints.into_iter().map(Cow::into_owned).collect(),
    }
}

/// [`merge_with`] under [`BOUNDS`].
pub fn merge(sources: Vec<Contribution>, load: &LoadRequest) -> Result<Reply, RdyError> {
    merge_with(sources, load, &BOUNDS)
}

/// The reply of `sources`: their rows and menus in layer order (each source's in its doc
/// order), once every id of `load` names exactly one ruleset. A qualified id needs its layer's
/// ruleset; a bare id one layer's only (`E_BAD_INPUT` when more hold it). An id that names no
/// ruleset is `E_NOT_FOUND`. The reply's JSON bytes are bounded (`E_LIMIT`). Two sources of
/// one layer are a host error.
pub fn merge_with(
    mut sources: Vec<Contribution>,
    load: &LoadRequest,
    b: &Bounds,
) -> Result<Reply, RdyError> {
    sources.sort_by(|x, y| {
        x.layer
            .precedence()
            .cmp(&y.layer.precedence())
            .then(x.layer.cmp(&y.layer))
    });
    if let Some(w) = sources.windows(2).find(|w| w[0].layer == w[1].layer) {
        return Err(RdyError::new(
            E_INTERNAL,
            format!("two instruction sources are layer {}", w[0].layer),
            "report this as a bug in the host",
        ));
    }
    for id in load.ids() {
        let holders: Vec<&Layer> = sources
            .iter()
            .filter(|s| match &id.target {
                LoadTarget::Builtin(_) => s.layer == Layer::Rdy,
                LoadTarget::Doc { layer, .. } => {
                    s.layer != Layer::Rdy && layer.as_ref().is_none_or(|l| *l == s.layer)
                }
            })
            .filter(|s| {
                let h = match &id.target {
                    LoadTarget::Builtin(name) => format!("rdy:{name}"),
                    LoadTarget::Doc { doc, .. } => handle(&s.layer, doc),
                };
                s.menu.iter().any(|m| m.id == h)
            })
            .map(|s| &s.layer)
            .collect();
        match holders.len() {
            0 => {
                return Err(RdyError::new(
                    E_NOT_FOUND,
                    format!("ruleset {} was not found", id.given),
                    NOT_FOUND_FIX,
                ));
            }
            1 => {}
            _ => {
                let names: Vec<String> = holders.iter().map(|l| l.to_string()).collect();
                return Err(RdyError::new(
                    E_BAD_INPUT,
                    format!("ruleset {} occurs in {}", id.given, names.join(" and ")),
                    AMBIGUOUS_FIX,
                ));
            }
        }
    }
    let mut reply = Reply::default();
    for s in sources {
        reply.instructions.extend(s.instructions);
        reply.menu.extend(s.menu);
    }
    let n = json_len(&reply);
    if n > b.reply_bytes {
        return Err(exceeded(b.reply_bytes, "bytes of reply JSON", n));
    }
    Ok(reply)
}

/// The line a session's overview and briefings carry until one of its instructions calls
/// succeeds (ain4). It is part of the reply, never of the briefing's facts or tokens.
pub const REMINDER: &str = "Call rdy_instructions at the start of every task.";

/// The reply key that carries [`REMINDER`].
pub const REMINDER_KEY: &str = "reminder";

/// The hint of a person layer with no meta project at `path`.
pub fn create_hint(path: &Path) -> String {
    format!(
        "Run rdy setup to create your own rules project at {}. Tell the person when setup finishes.",
        path.display()
    )
}

/// What a projectless connection adds to [`create_hint`]: its server needs a new connection.
pub const PROJECTLESS_SUFFIX: &str = "Reconnect this MCP server after setup.";

/// The hint of a session whose project is the meta project.
pub const META_SESSION_HINT: &str = "This session is on your own rules project. Use it for your rules. For other work, run rdy init in a folder and reconnect.";

/// The hint of a projectless connection whose meta project now exists.
pub const META_READY_HINT: &str = "Your own rules project is ready. Reconnect this MCP server to use it. For other work, run rdy init in a folder and reconnect.";

/// The contribution of the built-in guide (layer `rdy`): every ruleset on the menu, whole when
/// required, named by `load` or asked for by `load.full()`. It is made from the static
/// catalogue alone: no store, disk or network is read. Its reply bytes are bounded like a
/// stored source's (`E_LIMIT`).
pub fn builtin(load: &LoadRequest) -> Result<Contribution, RdyError> {
    builtin_with(load, &BOUNDS)
}

/// [`builtin`] under bounds `b`.
pub fn builtin_with(load: &LoadRequest, b: &Bounds) -> Result<Contribution, RdyError> {
    let mut menu = Vec::with_capacity(embedded_guide::RULESETS.len());
    let mut rows = vec![];
    for set in embedded_guide::RULESETS {
        let id = format!("rdy:{}", set.name);
        let whole = set.required || load.full() || load.names_builtin(set.name);
        let bytes: u64 = set.rows.iter().map(|r| r.text.len() as u64).sum();
        if whole {
            rows.extend(set.rows.iter().map(|r| InstructionRow {
                id: r.id.to_string(),
                ruleset: id.clone(),
                text: r.text.to_string(),
                layer: Layer::Rdy,
                locked: r.locked,
                reason: vec![],
            }));
        }
        menu.push(MenuEntry {
            id,
            name: set.title.to_string(),
            when: set.when.to_string(),
            layer: Layer::Rdy,
            required: set.required,
            tokens: estimate_tokens(bytes),
            hints: vec![],
        });
    }
    let n = json_len(&ReplyView {
        instructions: &rows,
        menu: &menu,
    });
    if n > b.reply_bytes {
        return Err(exceeded(b.reply_bytes, "bytes of reply JSON", n));
    }
    Ok(Contribution {
        layer: Layer::Rdy,
        menu,
        instructions: rows,
    })
}

/// Who asks for instructions: the caller's actor and run. A host that reads another source
/// asks it as this caller, never as the human.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caller {
    pub actor: String,
    pub run: Option<String>,
}

/// The host's reader of the person layer (D-YQPB6): where the meta project is, and its
/// contribution. The engine never opens another project's store itself; the host reads it
/// through that project's own daemon, or opens it when no process holds it.
pub trait PersonHost: Send + Sync {
    /// The person source, from the user config alone.
    fn locate(&self) -> Result<PersonSource, RdyError> {
        resolve_person_from_env()
    }

    /// The contribution of the meta project at `meta`, its path as configured, as the person
    /// layer for `load`. A refusal of that project stays the refusal.
    fn fetch(
        &self,
        meta: &Path,
        load: &LoadRequest,
        caller: &Caller,
    ) -> Result<Contribution, RdyError>;
}

/// Where the person layer is for one call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PersonPlace {
    /// The meta project is the project the call reads: it is read once, as the person layer.
    ThisProject,
    /// Another project, at its path as configured (its refusals name it so).
    Elsewhere(PathBuf),
    /// No meta project: an empty person layer, with this hint.
    Missing(String),
}

/// The place of `person` for a call whose project root is `project` (none outside a project).
/// A path with neither `.rdy.yaml` nor `.rdy` is no project (an empty person layer and the
/// setup hint naming it); any other path is a project, and its open says what is wrong with
/// it. The same directory under another spelling is the same project.
pub fn place_person(project: Option<&Path>, person: &PersonPath) -> Result<PersonPlace, RdyError> {
    let meta = match person {
        PersonPath::Unavailable => {
            return Ok(PersonPlace::Missing(
                crate::instructions_config::NO_HOME_MESSAGE.to_string(),
            ));
        }
        PersonPath::Override(p) | PersonPath::Default(p) => p,
    };
    let marked = |p: &Path| std::fs::symlink_metadata(p).is_ok();
    if !marked(&meta.join(crate::config_levels::PROJECT_FILE)) && !marked(&meta.join(".rdy")) {
        let mut hint = create_hint(meta);
        if project.is_none() {
            hint.push(' ');
            hint.push_str(PROJECTLESS_SUFFIX);
        }
        return Ok(PersonPlace::Missing(hint));
    }
    let canonical = |p: &Path| {
        std::fs::canonicalize(p).map_err(|e| {
            RdyError::new(
                E_IO,
                format!("cannot resolve {}: {e}", p.display()),
                "check that the path exists and that you can read it",
            )
        })
    };
    let same = match project {
        Some(root) => canonical(root)? == canonical(meta)?,
        None => false,
    };
    Ok(if same {
        PersonPlace::ThisProject
    } else {
        PersonPlace::Elsewhere(meta.clone())
    })
}

/// The refusal of a call whose person layer is another project while its host has no reader.
fn no_person_reader(meta: &Path) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!(
            "rdy_instructions reads the person layer at {} through the daemon, and this session runs without one",
            meta.display()
        ),
        "drop --no-daemon",
    )
}

/// [`deliver_with`] under [`BOUNDS`].
pub fn deliver(
    project: Option<(&ReadState, &Path)>,
    load: &LoadRequest,
    caller: &Caller,
    host: Option<&dyn PersonHost>,
) -> Result<Reply, RdyError> {
    deliver_with(project, load, caller, host, &BOUNDS)
}

/// One public instructions call: the project's captured state (none outside a project) and the
/// person layer, each read once, merged into one reply. The person source is located first (a
/// config refusal is the call's); the project's contribution is collected from its captured
/// state before the host reads another project. A missing meta project is an empty person
/// layer with the setup hint. The built-in guide (layer `rdy`) is added here once, so no host
/// adds it. The user config's warnings ride on the reply. The whole reply is bounded (`E_LIMIT`).
pub fn deliver_with(
    project: Option<(&ReadState, &Path)>,
    load: &LoadRequest,
    caller: &Caller,
    host: Option<&dyn PersonHost>,
    b: &Bounds,
) -> Result<Reply, RdyError> {
    let source = match host {
        Some(h) => h.locate()?,
        None => resolve_person_from_env()?,
    };
    let place = place_person(project.map(|p| p.1), &source.path)?;
    let mut sources = Vec::with_capacity(3);
    sources.push(builtin_with(load, b)?);
    let mut hint = None;
    match place {
        PersonPlace::ThisProject => {
            if let Some((state, _)) = project {
                sources.push(collect_with(state, Layer::Person, load, b)?);
            }
            hint = Some(META_SESSION_HINT.to_string());
        }
        PersonPlace::Elsewhere(meta) => {
            if project.is_none() {
                hint = Some(META_READY_HINT.to_string());
            }
            if let Some((state, _)) = project {
                sources.push(collect_with(state, Layer::Project, load, b)?);
            }
            let host = host.ok_or_else(|| no_person_reader(&meta))?;
            let person = host.fetch(&meta, load, caller)?;
            if person.layer != Layer::Person {
                return Err(RdyError::new(
                    E_INTERNAL,
                    format!("the person source answered as layer {}", person.layer),
                    "report this as a bug in the host",
                ));
            }
            sources.push(person);
        }
        PersonPlace::Missing(h) => {
            if let Some((state, _)) = project {
                sources.push(collect_with(state, Layer::Project, load, b)?);
            }
            hint = Some(h);
        }
    }
    let mut reply = merge_with(sources, load, b)?;
    reply.warnings = source
        .warnings
        .iter()
        .map(|w| serde_json::to_value(w).unwrap_or(serde_json::Value::Null))
        .collect();
    reply.hint = hint;
    let n = json_len(&reply);
    if n > b.reply_bytes {
        return Err(exceeded(b.reply_bytes, "bytes of reply JSON", n));
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ain3_tokens_round_up_over_utf8_bytes() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(5), 2);
        assert_eq!(estimate_tokens(u64::MAX), u64::MAX / 4 + 1);
        assert_eq!("é".len(), 2);
    }

    #[test]
    fn ain3_layer_order_is_reserved_and_teams_share_a_rank() {
        let mut ls = [
            Layer::Person,
            Layer::Rdy,
            Layer::Project,
            Layer::Cited("c".into()),
            Layer::Team("b".into()),
            Layer::Team("a".into()),
            Layer::Organization,
        ];
        ls.sort();
        assert_eq!(
            ls.iter().map(Layer::to_string).collect::<Vec<_>>(),
            vec![
                "rdy",
                "organization",
                "team:a",
                "team:b",
                "cited:c",
                "project",
                "person"
            ]
        );
        assert_eq!(
            Layer::Team("a".into()).precedence(),
            Layer::Team("b".into()).precedence()
        );
        assert_eq!(Layer::parse("team:"), None);
        assert_eq!(Layer::of_handle("team"), None);
    }

    #[test]
    fn ain3_counted_json_is_the_written_json() {
        let r = Reply {
            instructions: vec![InstructionRow {
                id: "ai-AAAAA".into(),
                ruleset: "project:d-AAAAA".into(),
                text: "\"q\"\n\té€\u{1}".into(),
                layer: Layer::Project,
                locked: true,
                reason: vec![Reason {
                    id: "D-AAAAA".into(),
                    statement: "s\\".into(),
                }],
            }],
            menu: vec![],
            warnings: vec![],
            hint: Some("h".into()),
        };
        assert_eq!(json_len(&r), serde_json::to_vec(&r).unwrap().len() as u64);
        let view = ReplyView {
            instructions: &[InstructionRow {
                id: Cow::Borrowed("ai-AAAAA"),
                ruleset: Cow::Borrowed("project:d-AAAAA"),
                text: Cow::Borrowed("\"q\"\n\té€\u{1}"),
                layer: Layer::Project,
                locked: true,
                reason: vec![Reason {
                    id: Cow::Borrowed("D-AAAAA"),
                    statement: Cow::Borrowed("s\\"),
                }],
            }],
            menu: &[],
        };
        let without_hint = Reply { hint: None, ..r };
        assert_eq!(json_len(&view), json_len(&without_hint));
    }
}
