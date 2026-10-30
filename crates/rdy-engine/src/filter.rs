//! The filter evaluator: what a parsed filter means for one block, over inputs its caller
//! supplies, and `rdy_query`'s read over a scope.
//!
//! `FilterContext` is pure. It resolves no scope and reads no store, so a caller can evaluate a
//! state that is not stored yet. `ReadState::scope_inputs` is the one place that reads a scope's
//! sets for a filter, and the coverage refusals at a past landing or in a proposal version live
//! there, not in the evaluator.
//!
//! Each link test keeps its inner result per block, so one query costs at most
//! (tests in the filter) x (blocks + edges) evaluations. The parser bounds the recursion.
use crate::areas::block_areas;
use crate::coverage::change::candidate_build;
use crate::coverage::{CoverageIssues, rollup};
use crate::cursor;
use crate::diff::reading_order;
use crate::error::{E_BAD_INPUT, E_LIMIT, E_PAGE_STALE, PAGE_STALE_FIX, RdyError};
use crate::gate::changes_of;
use crate::keys::key_of;
use crate::materialize::Materialized;
use crate::query::compact_summary;
use crate::{ReadState, Scope, now_ms};
use rdy_graph::{EdgeKind, Graph};
use rdy_model::{Block, BlockId, FieldValue, Hash};
use rdy_schema::filter::{Dir, Expr, Field, Filter, FilterError, LinkKind, Op, Value, parse};
use rdy_schema::{AreaMatcher, Pack, Trait};
use serde::Serialize;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;
use std::sync::Arc;

pub const FILTER_FIX: &str = "read the filter guide in rdy_schema, or run rdy q --help";
pub const QUERY_DEFAULT_TOKENS: usize = 4_000;

/// The domain of a query page's token.
const TOKEN_DOMAIN: &[u8] = b"rdy-query-1\0";
/// The tool that a query cursor's refusal names.
const QUERY_TOOL: &str = "rdy_query";

/// `E_BAD_INPUT` with `FilterError`'s text and `FILTER_FIX`.
pub fn filter_error(e: FilterError) -> RdyError {
    RdyError::new(E_BAD_INPUT, e.to_string(), FILTER_FIX)
}

/// What one evaluation reads. The caller supplies every member; an empty set is right for a
/// set that no filter needs (`Filter::needs`).
pub struct FilterInputs<'a> {
    pub pack: &'a Pack,
    pub m: &'a Materialized,
    pub graph: &'a Graph,
    pub areas: &'a AreaMatcher,
    pub suspects: &'a BTreeSet<BlockId>,
    pub tensions: &'a BTreeSet<BlockId>,
    pub coverage: &'a BTreeMap<BlockId, u32>,
}

/// One filter this context has evaluated, and each of its link tests' inner results per block.
/// The results are keyed by the inner expression's address in this copy of the filter, which
/// the context keeps for as long as it keeps the results.
struct Evaluated {
    filter: Filter,
    inner: RefCell<HashMap<usize, HashMap<BlockId, bool>>>,
}

/// The inputs, and each link test's inner results per block.
pub struct FilterContext<'a> {
    inputs: FilterInputs<'a>,
    filters: RefCell<Vec<Rc<Evaluated>>>,
    evaluations: Cell<usize>,
}

impl<'a> FilterContext<'a> {
    /// Pure: no scope resolution and no store read. Never fails.
    pub fn from_parts(inputs: FilterInputs<'a>) -> FilterContext<'a> {
        FilterContext {
            inputs,
            filters: RefCell::new(Vec::new()),
            evaluations: Cell::new(0),
        }
    }

    /// Whether `block` matches; false when `inputs.m` lacks it. The answer depends only on the
    /// inputs.
    pub fn matches(&self, filter: &Filter, block: &BlockId) -> bool {
        let Some(b) = self.inputs.m.get(block) else {
            return false;
        };
        let evaluated = self.evaluated(filter);
        self.eval(&evaluated, &evaluated.filter.expr, b)
    }

    /// Test hook: tests evaluated so far. A link test counts once, and once more for each edge
    /// or child it looks at.
    #[doc(hidden)]
    pub fn evaluations(&self) -> usize {
        self.evaluations.get()
    }

    fn evaluated(&self, filter: &Filter) -> Rc<Evaluated> {
        let mut filters = self.filters.borrow_mut();
        if let Some(found) = filters.iter().find(|e| e.filter == *filter) {
            return Rc::clone(found);
        }
        let added = Rc::new(Evaluated {
            filter: filter.clone(),
            inner: RefCell::new(HashMap::new()),
        });
        filters.push(Rc::clone(&added));
        added
    }

    fn count(&self) {
        self.evaluations
            .set(self.evaluations.get().saturating_add(1));
    }

    fn eval(&self, at: &Evaluated, expr: &Expr, b: &Block) -> bool {
        match expr {
            Expr::Or(all) => all.iter().any(|e| self.eval(at, e, b)),
            Expr::And(all) => all.iter().all(|e| self.eval(at, e, b)),
            Expr::Not(inner) => !self.eval(at, inner, b),
            Expr::Compare { field, op, value } => {
                self.count();
                self.compare(field, *op, value, b)
            }
            Expr::Has(field) => {
                self.count();
                self.has(field, b)
            }
            Expr::Linked {
                dir,
                kind,
                inner,
                none,
            } => {
                self.count();
                self.linked(at, *dir, kind, inner.as_deref(), b) != *none
            }
        }
    }

    /// Whether a block one hop from `b` exists, and matches `inner` when one is given. Only
    /// blocks the supplied materialization holds count.
    fn linked(
        &self,
        at: &Evaluated,
        dir: Dir,
        kind: &LinkKind,
        inner: Option<&Expr>,
        b: &Block,
    ) -> bool {
        let m = self.inputs.m;
        let holds = |other: &BlockId| {
            m.get(other)
                .is_some_and(|o| inner.is_none_or(|e| self.inner_result(at, e, o)))
        };
        let of_kind = |edge: &EdgeKind| {
            self.count();
            match (kind, edge) {
                (LinkKind::References, EdgeKind::References) => true,
                (LinkKind::Link(want), EdgeKind::Link(have)) => want == have,
                _ => false,
            }
        };
        match (kind, dir) {
            (LinkKind::Contains, Dir::Out) => m.children(Some(&b.id)).iter().any(|c| {
                self.count();
                holds(&c.id)
            }),
            (LinkKind::Contains, Dir::In) => b.parent.as_ref().is_some_and(|p| {
                self.count();
                holds(p)
            }),
            (_, Dir::Out) => self
                .inputs
                .graph
                .outgoing(&b.id)
                .iter()
                .filter(|e| of_kind(&e.kind))
                .any(|e| holds(&e.to)),
            (_, Dir::In) => self
                .inputs
                .graph
                .incoming(&b.id)
                .iter()
                .filter(|e| of_kind(&e.kind))
                .any(|e| holds(&e.from)),
        }
    }

    /// `expr`, the inner expression of a link test, at `b`: evaluated once per block.
    fn inner_result(&self, at: &Evaluated, expr: &Expr, b: &Block) -> bool {
        let key = std::ptr::from_ref(expr) as usize;
        if let Some(&known) = at.inner.borrow().get(&key).and_then(|r| r.get(&b.id)) {
            return known;
        }
        let result = self.eval(at, expr, b);
        at.inner
            .borrow_mut()
            .entry(key)
            .or_default()
            .insert(b.id.clone(), result);
        result
    }

    /// `b`'s own `lifecycle` field or its nearest ancestor's, as search reads it.
    fn lifecycle<'b>(&'b self, b: &'b Block) -> Option<&'b str> {
        let m = self.inputs.m;
        let mut cur = b;
        for _ in 0..=m.len() {
            if let Some(FieldValue::Str(l)) = cur.fields.get("lifecycle") {
                return Some(l);
            }
            cur = m.get(cur.parent.as_ref()?)?;
        }
        None
    }

    fn areas_of(&self, b: &Block) -> Vec<String> {
        if self.inputs.areas.is_empty() {
            return Vec::new();
        }
        block_areas(self.inputs.areas, self.inputs.m, b)
    }

    fn compare(&self, field: &Field, op: Op, value: &Value, b: &Block) -> bool {
        let m = self.inputs.m;
        let text = value_text(value);
        match field {
            Field::Id => text_holds(b.id.as_str(), op, text),
            Field::Key => key_of(b).is_some_and(|k| folded_holds(k, op, text)),
            Field::Type => equality_holds(b.block_type == text, op),
            Field::Trait => any_of_holds(self.traits_of(b), op, text),
            Field::Path => m.path_of(&b.id).is_some_and(|p| text_holds(&p, op, text)),
            Field::Doc => m.doc_of(&b.id).is_some_and(|d| {
                let same = d.id.as_str() == text
                    || key_of(d).is_some_and(|k| k.eq_ignore_ascii_case(text))
                    || m.path_of(&d.id).as_deref() == Some(text);
                equality_holds(same, op)
            }),
            Field::Area => any_of_holds(
                self.areas_of(b).iter().map(String::as_str).collect(),
                op,
                text,
            ),
            Field::Lifecycle => self
                .lifecycle(b)
                .is_some_and(|l| equality_holds(l == text, op)),
            Field::Status => self
                .inputs
                .graph
                .status(&b.id)
                .is_some_and(|s| equality_holds(s.tag() == text, op)),
            Field::Suspect => bool_holds(self.inputs.suspects.contains(&b.id), op, value),
            Field::Tension => bool_holds(self.inputs.tensions.contains(&b.id), op, value),
            Field::Coverage => self
                .inputs
                .coverage
                .get(&b.id)
                .is_some_and(|score| int_holds(i64::from(*score), op, value)),
            Field::Custom(name) => b
                .fields
                .get(name)
                .is_some_and(|field| custom_holds(field, op, value)),
        }
    }

    fn has(&self, field: &Field, b: &Block) -> bool {
        let m = self.inputs.m;
        match field {
            Field::Key => key_of(b).is_some(),
            Field::Path => m.path_of(&b.id).is_some(),
            Field::Doc => m.doc_of(&b.id).is_some(),
            Field::Area => !self.areas_of(b).is_empty(),
            Field::Lifecycle => self.lifecycle(b).is_some(),
            Field::Status => self.inputs.graph.status(&b.id).is_some(),
            Field::Coverage => self.inputs.coverage.contains_key(&b.id),
            Field::Custom(name) => b.fields.get(name).is_some_and(|v| *v != FieldValue::Null),
            // the parser refuses `has` of any other field
            Field::Id | Field::Type | Field::Trait | Field::Suspect | Field::Tension => false,
        }
    }

    fn traits_of(&self, b: &Block) -> Vec<&'static str> {
        self.inputs
            .pack
            .block_types
            .get(&b.block_type)
            .map_or_else(Vec::new, |t| {
                t.traits.iter().map(|held| trait_name(*held)).collect()
            })
    }
}

/// The text of a filter value, as written in the filter.
fn value_text(value: &Value) -> &str {
    match value {
        Value::Str(text) | Value::Int { text, .. } | Value::Bool { text, .. } => text,
    }
}

fn equality_holds(same: bool, op: Op) -> bool {
    match op {
        Op::Eq => same,
        Op::Ne => !same,
        _ => false,
    }
}

fn text_holds(have: &str, op: Op, want: &str) -> bool {
    match op {
        Op::StartsWith => have.starts_with(want),
        _ => equality_holds(have == want, op),
    }
}

/// `text_holds` in any ASCII case.
fn folded_holds(have: &str, op: Op, want: &str) -> bool {
    match op {
        Op::StartsWith => have
            .as_bytes()
            .get(..want.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(want.as_bytes())),
        _ => equality_holds(have.eq_ignore_ascii_case(want), op),
    }
}

/// A member of many values: `=` when one equals `want`, `!=` when the member exists and none
/// does.
fn any_of_holds(have: Vec<&str>, op: Op, want: &str) -> bool {
    !have.is_empty() && equality_holds(have.contains(&want), op)
}

fn bool_holds(have: bool, op: Op, want: &Value) -> bool {
    matches!(want, Value::Bool { value, .. } if equality_holds(have == *value, op))
}

fn int_holds(have: i64, op: Op, want: &Value) -> bool {
    let Value::Int { value, .. } = want else {
        return false;
    };
    match op {
        Op::Eq => have == *value,
        Op::Ne => have != *value,
        Op::Lt => have < *value,
        Op::Le => have <= *value,
        Op::Gt => have > *value,
        Op::Ge => have >= *value,
        Op::StartsWith => false,
    }
}

/// A string, number or boolean field value against a filter value of its own kind. A value of
/// another kind holds for no test.
fn scalar_holds(have: &FieldValue, op: Op, want: &Value) -> bool {
    match (have, want) {
        (FieldValue::Str(have), Value::Str(want)) => text_holds(have, op, want),
        (FieldValue::Int(have), Value::Int { .. }) => int_holds(*have, op, want),
        (FieldValue::Bool(have), Value::Bool { .. }) => bool_holds(*have, op, want),
        _ => false,
    }
}

fn custom_holds(have: &FieldValue, op: Op, want: &Value) -> bool {
    match have {
        FieldValue::List(items) => match op {
            Op::Eq | Op::StartsWith => items.iter().any(|i| scalar_holds(i, op, want)),
            Op::Ne => !items.iter().any(|i| scalar_holds(i, Op::Eq, want)),
            _ => false,
        },
        scalar => scalar_holds(scalar, op, want),
    }
}

fn trait_name(held: Trait) -> &'static str {
    match held {
        Trait::Normative => "normative",
        Trait::Informative => "informative",
        Trait::Defines => "defines",
        Trait::Describes => "describes",
        Trait::Asset => "asset",
        Trait::Structural => "structural",
        Trait::Presentational => "presentational",
    }
}

/// Each scored block's coverage score, `rollup(pack, entry).score`, as a search hit scores it.
pub fn coverage_scores(pack: &Pack, issues: &CoverageIssues) -> BTreeMap<BlockId, u32> {
    issues
        .blocks
        .iter()
        .map(|(id, entry)| (id.clone(), rollup(pack, std::iter::once(entry)).score))
        .collect()
}

/// The query adapter's owned inputs for one scope: the scope's materialization and graph, and
/// only the sets that its filters need.
pub(crate) struct ScopeInputs {
    m: Arc<Materialized>,
    graph: Arc<Graph>,
    suspects: BTreeSet<BlockId>,
    tensions: BTreeSet<BlockId>,
    coverage: BTreeMap<BlockId, u32>,
}

impl ScopeInputs {
    pub(crate) fn inputs<'a>(&'a self, st: &'a ReadState) -> FilterInputs<'a> {
        FilterInputs {
            pack: st.pack(),
            m: &self.m,
            graph: &self.graph,
            areas: st.area_matcher(),
            suspects: &self.suspects,
            tensions: &self.tensions,
            coverage: &self.coverage,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QueryMatch {
    pub id: String,
    pub block_type: String,
    pub path: Option<String>,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct QueryReply {
    pub matches: Vec<QueryMatch>,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

fn query_match(m: &Materialized, id: &BlockId) -> Option<QueryMatch> {
    let b = m.get(id)?;
    Some(QueryMatch {
        id: id.to_string(),
        block_type: b.block_type.clone(),
        path: m.path_of(id),
        summary: compact_summary(b),
    })
}

fn json_len(value: &impl Serialize) -> usize {
    serde_json::to_string(value).map_or(0, |s| s.len())
}

fn query_token(filter: &str, scope_text: &str, ids: &[BlockId]) -> Hash {
    let mut token = TOKEN_DOMAIN.to_vec();
    token.extend_from_slice(filter.as_bytes());
    token.push(0);
    token.extend_from_slice(scope_text.as_bytes());
    token.push(0);
    for id in ids {
        token.extend_from_slice(id.as_str().as_bytes());
        token.push(0);
    }
    Hash::of(&token)
}

/// A cursor whose offset is at or past the matches reads as one that does not parse.
fn bad_page() -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("page is not a cursor from {QUERY_TOOL}'s next"),
        "pass next from the previous page, unchanged",
    )
}

/// The matches of `filter` in `scope`: the scope's materialization, and the ids in reading order.
fn matching_ids(
    st: &ReadState,
    scope: &Scope,
    filter: &Filter,
) -> Result<(Arc<Materialized>, Vec<BlockId>), RdyError> {
    let inputs = st.scope_inputs(scope, &[filter])?;
    let m = Arc::clone(&inputs.m);
    let ctx = FilterContext::from_parts(inputs.inputs(st));
    let ids = reading_order(&m)
        .into_iter()
        .filter(|id| ctx.matches(filter, id))
        .collect();
    Ok((m, ids))
}

impl ReadState {
    /// The inputs of `filters` in `scope`. A filter that needs coverage at a past landing or in
    /// a proposal version is refused here, with `coverage_scope`'s refusal.
    pub(crate) fn scope_inputs(
        &self,
        scope: &Scope,
        filters: &[&Filter],
    ) -> Result<ScopeInputs, RdyError> {
        let needs = filters.iter().map(|f| f.needs());
        let (mut suspect, mut tension, mut coverage) = (false, false, false);
        for n in needs {
            suspect |= n.suspect;
            tension |= n.tension;
            coverage |= n.coverage;
        }
        if coverage {
            self.coverage_scope(scope)?;
        }
        Ok(ScopeInputs {
            m: self.materialized(scope)?,
            graph: self.graph(scope)?,
            suspects: if suspect {
                self.suspects(scope)?.into_iter().map(|s| s.block).collect()
            } else {
                BTreeSet::new()
            },
            tensions: if tension {
                self.open_tension_blocks(now_ms())?
            } else {
                BTreeSet::new()
            },
            coverage: if coverage {
                coverage_scores(self.pack(), &*self.coverage_issues(scope)?)
            } else {
                BTreeMap::new()
            },
        })
    }

    /// Every match in reading order, with no cut: what `rdy q` prints.
    pub fn query(&self, scope: &Scope, filter: &str) -> Result<QueryReply, RdyError> {
        let parsed = parse(filter, self.pack()).map_err(filter_error)?;
        let (m, ids) = matching_ids(self, scope, &parsed)?;
        Ok(QueryReply {
            total: ids.len(),
            matches: ids.iter().filter_map(|id| query_match(&m, id)).collect(),
            next: None,
        })
    }

    /// The page that fits `max_tokens`; cursors name `rdy_query`. A page fits when its JSON
    /// length in bytes, divided by 4 and rounded down, is at most `max_tokens`.
    pub fn query_page(
        &self,
        scope: &Scope,
        scope_text: &str,
        filter: &str,
        page: Option<&str>,
        max_tokens: usize,
    ) -> Result<QueryReply, RdyError> {
        let parsed = parse(filter, self.pack()).map_err(filter_error)?;
        let cursor = page.map(|p| cursor::parse(p, QUERY_TOOL)).transpose()?;
        let (m, ids) = matching_ids(self, scope, &parsed)?;
        let total = ids.len();
        let token = query_token(filter, scope_text, &ids);
        let start = match cursor {
            None => 0,
            Some((t, _)) if t != token => {
                return Err(RdyError::new(
                    E_PAGE_STALE,
                    "the matches behind this page changed since the first page, or the query differs",
                    PAGE_STALE_FIX,
                ));
            }
            Some((_, offset)) if offset >= total => return Err(bad_page()),
            Some((_, offset)) => offset,
        };

        let fits = |bytes: usize| bytes / 4 <= max_tokens;
        let fixed = json_len(&QueryReply {
            matches: vec![],
            total,
            next: None,
        });
        let fixed_with_next = json_len(&QueryReply {
            matches: vec![],
            total,
            next: Some(cursor::make(&token, total)),
        });

        // Each match is counted once. A run that does not fit without `next` cannot fit with it,
        // so the walk stops there.
        let mut matches = vec![];
        let mut bytes = 0usize;
        let mut fit_with_next = 0;
        let mut whole = fits(fixed);
        for id in &ids[start..] {
            let Some(row) = query_match(&m, id) else {
                continue;
            };
            let comma = usize::from(!matches.is_empty());
            let joined = bytes.saturating_add(json_len(&row)).saturating_add(comma);
            if !fits(fixed.saturating_add(joined)) {
                whole = false;
                break;
            }
            if fits(fixed_with_next.saturating_add(joined)) {
                fit_with_next = matches.len() + 1;
            }
            matches.push(row);
            bytes = joined;
        }
        if whole {
            return Ok(QueryReply {
                matches,
                total,
                next: None,
            });
        }
        if !fits(fixed_with_next) {
            return Err(RdyError::new(
                E_LIMIT,
                "the token budget cannot hold the reply metadata",
                "increase max_tokens",
            ));
        }
        if fit_with_next == 0 {
            return Err(RdyError::new(
                E_LIMIT,
                "the token budget cannot hold one match",
                "increase max_tokens",
            ));
        }
        matches.truncate(fit_with_next);
        Ok(QueryReply {
            matches,
            total,
            next: Some(cursor::make(&token, start + fit_with_next)),
        })
    }
}

/// Tests only: the prospective canon of `changes` on canon, as the queue builds a candidate's.
#[doc(hidden)]
pub fn candidate_for_tests(
    st: &ReadState,
    changes: &[Hash],
) -> Result<(Arc<Materialized>, Arc<Graph>), RdyError> {
    let changes = changes_of(st, changes)?;
    let built = candidate_build(st, &changes)?;
    let graph = Arc::clone(built.graph.get_or_init(|| {
        Arc::new(Graph::build_with(
            built.m.iter(),
            Arc::clone(st.reader().tracking()),
        ))
    }));
    Ok((Arc::clone(&built.m), graph))
}
