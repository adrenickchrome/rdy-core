//! The case of a check other than contradiction (O4): a closed shape, capped with omitted counts
//! and bound by a digest of every input, omitted ones included. Contradiction keeps its pair case
//! and hash (`semantic::case`).
use super::catalogue::AgentCheck;
use super::code::CodeSource;
use super::recipes;
use crate::lexicon::shown_text_in;
use crate::semantic::case::{case_hash, headings_of};
use crate::{E_BAD_INPUT, E_LIMIT, Materialized, RdyError, ReadState};
use rdy_model::check_request::CheckSubject;
use rdy_model::entity::{shown_sources, source_field};
use rdy_model::{Block, BlockId, Change, FieldValue, Hash, PairItem, pair_key};
use rdy_schema::Pack;
use serde_json::{Value, json};

/// The most block rows in one case.
pub(super) const MAX_ROWS: usize = 64;
/// The most text bytes in one block row.
pub(super) const MAX_ROW_TEXT: usize = 16_384;
/// The most text bytes in one case, code text included.
pub(super) const MAX_CASE_TEXT: usize = 262_144;
/// The most headings one row shows.
const MAX_HEADINGS: usize = 16;
/// The most characters one heading shows.
const MAX_HEADING_CHARS: usize = 256;
/// The most contextual rows of an entity or a prose-drift case.
pub(super) const MAX_CONTEXT: usize = 20;
/// The most candidate block identities one case may visit: the carried subject bound.
pub(super) const MAX_VISITED: usize = 100_000;
/// The domain of a case hash.
const CASE_DOMAIN: &[u8] = b"rdy-case-2\0";
/// The domain of a case's digest of its inputs.
const INPUTS_DOMAIN: &[u8] = b"rdy-case-inputs-1\0";

/// What a recipe reads. `after` is the state the case judges and `before` the state it compares
/// with; `changes` are the selected changes of a change or a proposal version, and only those.
#[derive(Clone, Copy)]
pub struct CaseInputs<'a> {
    pub st: &'a ReadState,
    pub before: &'a Materialized,
    pub after: &'a Materialized,
    pub changes: &'a [Change],
}

/// One built case: its JSON, the hash a verdict binds to, and the recipe's primary blocks, sorted
/// and distinct.
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltCase {
    pub case: Value,
    pub hash: Hash,
    pub subject_blocks: Vec<BlockId>,
}

/// The case of `check` on `subject`. A contradiction subject that is a decision gives its first
/// pair; [`contradiction_cases`] gives them all.
pub fn build_case(
    check: AgentCheck,
    subject: &CheckSubject,
    inputs: CaseInputs<'_>,
) -> Result<BuiltCase, RdyError> {
    match check {
        AgentCheck::Contradiction => {
            let (cases, _) = contradiction_cases(subject, inputs)?;
            cases
                .into_iter()
                .next()
                .ok_or_else(|| recipes::no_pairs(subject))
        }
        AgentCheck::MeaningPreservation => recipes::meaning(subject, inputs),
        AgentCheck::ImplementationFidelity => recipes::fidelity(subject, inputs),
        AgentCheck::DecisionReview => recipes::decision_review(subject, inputs),
        AgentCheck::EntityDivergence => recipes::entity(subject, inputs),
        AgentCheck::ProseDrift => recipes::prose(subject, inputs),
        AgentCheck::DiffContradicts => recipes::diff(subject, inputs),
        AgentCheck::Confirmation => recipes::confirmation(subject, inputs),
    }
}

/// The contradiction cases of `subject`, one per pair, and the count of pairs left out. Two blocks
/// are one pair. A decision's pairs follow its related rows, at most 20 distinct ones; a pair with
/// no current items and each row the related query's cap dropped count as left out. A decision
/// with no pair gives no case.
pub fn contradiction_cases(
    subject: &CheckSubject,
    inputs: CaseInputs<'_>,
) -> Result<(Vec<BuiltCase>, usize), RdyError> {
    let (pairs, mut omitted, single) = recipes::contradiction_pairs(subject, inputs)?;
    let mut cases = Vec::with_capacity(pairs.len());
    for [a, b] in pairs {
        match pair_case(inputs, &a, &b)? {
            Some(c) => cases.push(c),
            None if single => return Err(recipes::no_form(&a, &b)),
            None => omitted += 1,
        }
    }
    Ok((cases, omitted))
}

/// The existing case of the pair `a`, `b` at the contents `after` holds, through the existing
/// item and case builders; `None` when no view holds the pair.
fn pair_case(
    inputs: CaseInputs<'_>,
    a: &BlockId,
    b: &BlockId,
) -> Result<Option<BuiltCase>, RdyError> {
    let st = inputs.st;
    let tracking = st.reader().tracking();
    let content = |id: &BlockId| {
        inputs
            .after
            .get(id)
            .map(|x| x.content_hash_with(tracking))
            .ok_or_else(|| recipes::unknown(id))
    };
    let pair = [(a.clone(), content(a)?), (b.clone(), content(b)?)];
    let items = match st.pair_now(&pair)? {
        Some(items) => items,
        None => match st.pair_in_canon(&pair)? {
            Some(items) => items,
            None => return Ok(None),
        },
    };
    let key = pair_key(
        &items
            .iter()
            .map(|i| PairItem {
                block: i.block.clone(),
                content: i.content,
            })
            .collect::<Vec<_>>(),
    );
    let file = st.case_of(&key, &items)?;
    let hash = case_hash(&file);
    let case = serde_json::to_value(&file).map_err(|e| {
        RdyError::new(
            crate::E_INTERNAL,
            format!("a case file does not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })?;
    Ok(Some(BuiltCase {
        case,
        hash,
        subject_blocks: vec![a.clone(), b.clone()],
    }))
}

/// The refusal of a subject of the wrong kind or an empty qualifying set.
pub(super) fn wrong_subject(check: AgentCheck, who: &str, kind: &str) -> RdyError {
    let spec = check.spec();
    RdyError::new(
        E_BAD_INPUT,
        format!("{} takes {}; {who} is a {kind}", spec.name, spec.subject),
        format!(
            "pass {}: rdy_review {{op: \"checks\"}} / rdy review checks",
            spec.subject
        ),
    )
}

/// The count of candidate block identities one case has visited.
#[derive(Default)]
pub(super) struct Visited(usize);

impl Visited {
    /// Counts one identity; `E_LIMIT` one past the bound.
    pub(super) fn visit(&mut self) -> Result<(), RdyError> {
        self.0 += 1;
        if self.0 > MAX_VISITED {
            return Err(RdyError::new(
                E_LIMIT,
                format!("check case exceeds {MAX_VISITED} blocks"),
                "run the check on a smaller subject",
            ));
        }
        Ok(())
    }
}

/// Which text of a block a row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Selection {
    /// The existing shown text.
    Shown,
    /// A decision's statement and body.
    Decision,
    /// Decision review's six selected fields, in order.
    Review,
    /// A decision's statement, body and confirmation text.
    Confirmation,
}

/// The selected metadata of a decision review: shown although a mention scan skips it.
const REVIEW_METADATA: [&str; 3] = ["reversibility", "authority_basis", "attested_by"];

/// One `{field, text}` row before its cut. `counted` says a metadata row's field name counts
/// toward the byte caps too.
struct TextRow {
    field: String,
    text: String,
    counted: bool,
}

impl TextRow {
    fn plain((field, text): (String, String)) -> TextRow {
        TextRow {
            field,
            text,
            counted: false,
        }
    }

    fn bytes(&self) -> usize {
        self.text.len() + if self.counted { self.field.len() } else { 0 }
    }
}

/// The rows of `sources` whose field is `name`, in order.
fn of_field(sources: &[(String, String)], name: &str) -> Vec<TextRow> {
    sources
        .iter()
        .filter(|(n, _)| source_field(n) == name)
        .cloned()
        .map(TextRow::plain)
        .collect()
}

fn scalar(v: &FieldValue) -> Option<String> {
    match v {
        FieldValue::Str(s) => Some(s.clone()),
        FieldValue::Int(n) => Some(n.to_string()),
        FieldValue::Bool(b) => Some(b.to_string()),
        FieldValue::Null | FieldValue::List(_) | FieldValue::Map(_) => None,
    }
}

impl Selection {
    fn render(self, pack: &Pack, b: &Block) -> Vec<TextRow> {
        match self {
            Selection::Shown => shown_text_in(pack, b)
                .into_iter()
                .map(TextRow::plain)
                .collect(),
            Selection::Decision | Selection::Confirmation | Selection::Review => {
                let sources = shown_sources(b);
                let mut out = of_field(&sources, "statement");
                out.extend(of_field(&sources, "body"));
                match self {
                    Selection::Confirmation => out.extend(of_field(&sources, "confirmation")),
                    Selection::Review => {
                        out.extend(of_field(&sources, "options"));
                        out.extend(REVIEW_METADATA.iter().filter_map(|f| {
                            Some(TextRow {
                                field: (*f).to_string(),
                                text: scalar(b.fields.get(*f)?)?,
                                counted: true,
                            })
                        }));
                    }
                    Selection::Shown | Selection::Decision => {}
                }
                out
            }
        }
    }
}

/// One candidate row of a case: read from the states, copied only when it is shown.
#[derive(Clone, Debug)]
pub(super) struct Row<'a> {
    pub(super) role: &'static str,
    pub(super) id: BlockId,
    pub(super) before: Option<&'a Block>,
    pub(super) after: Option<&'a Block>,
    pub(super) selection: Selection,
    /// A contextual row follows every primary row, under the plan's context cap.
    pub(super) contextual: bool,
}

/// What a recipe hands to [`assemble`].
pub(super) struct Plan<'a> {
    /// Primary rows, then contextual ones.
    pub(super) rows: Vec<Row<'a>>,
    /// The cap on contextual rows, tighter than the block cap.
    pub(super) context_cap: Option<usize>,
    pub(super) subject_blocks: Vec<BlockId>,
    pub(super) code: CodeSource,
}

/// The incremental digest of a case's ordered inputs. Each string is length-prefixed, so no two
/// input lists feed the same bytes, and nothing unbounded is held in memory.
pub(super) struct Digest(blake3::Hasher);

impl Digest {
    fn new() -> Digest {
        let mut h = blake3::Hasher::new();
        h.update(INPUTS_DOMAIN);
        Digest(h)
    }

    pub(super) fn str(&mut self, s: &str) {
        self.0.update(&(s.len() as u64).to_le_bytes());
        self.0.update(s.as_bytes());
    }

    pub(super) fn opt(&mut self, s: Option<&str>) {
        match s {
            Some(s) => {
                self.0.update(&[1]);
                self.str(s);
            }
            None => {
                self.0.update(&[0]);
            }
        }
    }

    fn side(&mut self, version: Option<Hash>, text: &[TextRow]) {
        self.opt(version.map(|v| v.to_string()).as_deref());
        self.0.update(&(text.len() as u64).to_le_bytes());
        for t in text {
            self.str(&t.field);
            self.str(&t.text);
        }
    }

    fn finish(self) -> Hash {
        Hash(*self.0.finalize().as_bytes())
    }
}

/// `s` cut to at most `n` bytes at a Unicode boundary, and the bytes the cut leaves out.
pub(super) fn clip(s: &str, n: usize) -> (&str, usize) {
    if s.len() <= n {
        return (s, 0);
    }
    let mut at = n;
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    (&s[..at], s.len() - at)
}

/// The omitted counts and the text budget of one case.
#[derive(Default)]
pub(super) struct Omitted {
    pub(super) blocks: usize,
    pub(super) text_bytes: usize,
    pub(super) code_files: usize,
    pub(super) code_bytes: usize,
}

/// Each side's share of `budget`: a side under half keeps all it has and the other side takes
/// the rest, so a long earlier text cannot hide the later one.
fn shares(before: usize, after: usize, budget: usize) -> (usize, usize) {
    let half = budget / 2;
    if before + after <= budget {
        (before, after)
    } else if before <= half {
        (before, budget - before)
    } else if after <= half {
        (budget - after, after)
    } else {
        (half, budget - half)
    }
}

/// The JSON of one side of a row, cut to `budget`: `{version, text}`. Returns the bytes it spent.
fn side_json(
    version: Option<Hash>,
    text: &[TextRow],
    budget: usize,
    omitted: &mut Omitted,
) -> (Value, usize) {
    let mut left = budget;
    let mut rows = Vec::with_capacity(text.len());
    for t in text {
        if t.counted {
            if t.field.len() > left {
                omitted.text_bytes += t.bytes();
                continue;
            }
            left -= t.field.len();
        }
        let (kept, cut) = clip(&t.text, left);
        left -= kept.len();
        omitted.text_bytes += cut;
        rows.push(json!({"field": t.field, "text": kept}));
    }
    (
        json!({
            "version": version.map(|v| v.to_string()),
            "text": rows,
        }),
        budget - left,
    )
}

/// The headings a row shows, cut; what is left out counts as text bytes.
fn clipped_headings(headings: Vec<String>, omitted: &mut Omitted) -> Vec<String> {
    let mut out = Vec::with_capacity(headings.len().min(MAX_HEADINGS));
    for (i, h) in headings.into_iter().enumerate() {
        if i >= MAX_HEADINGS {
            omitted.text_bytes += h.len();
            continue;
        }
        match h.char_indices().nth(MAX_HEADING_CHARS) {
            Some((at, _)) => {
                omitted.text_bytes += h.len() - at;
                out.push(h[..at].to_string());
            }
            None => out.push(h),
        }
    }
    out
}

/// Builds the case of `check` from `plan`: every candidate row feeds the digest, the rows under
/// the caps enter the case, and the code source reads last, with what the block text left.
pub(super) fn assemble(
    check: AgentCheck,
    inputs: CaseInputs<'_>,
    plan: Plan<'_>,
) -> Result<BuiltCase, RdyError> {
    let pack = inputs.st.pack();
    let spec = check.spec();
    let mut digest = Digest::new();
    digest.str(spec.name);
    let mut omitted = Omitted::default();
    let mut shown = Vec::with_capacity(plan.rows.len().min(MAX_ROWS));
    let mut used = 0;
    let mut context = 0;
    for row in &plan.rows {
        let before_text = row
            .before
            .map(|b| row.selection.render(pack, b))
            .unwrap_or_default();
        let after_text = row
            .after
            .map(|b| row.selection.render(pack, b))
            .unwrap_or_default();
        let before_version = row.before.map(|_| inputs.before.version_of(&row.id));
        let after_version = row.after.map(|_| inputs.after.version_of(&row.id));
        let block = row.after.or(row.before);
        let holder = if row.after.is_some() {
            inputs.after
        } else {
            inputs.before
        };
        let (doc, headings) = block.map(|b| headings_of(holder, b)).unwrap_or_default();

        digest.str(row.role);
        digest.str(row.id.as_str());
        digest.opt(block.map(|b| b.block_type.as_str()));
        digest.str(&doc);
        for h in &headings {
            digest.str(h);
        }
        digest.side(before_version.flatten(), &before_text);
        digest.side(after_version.flatten(), &after_text);

        let total = |t: &[TextRow]| t.iter().map(TextRow::bytes).sum::<usize>();
        let (b_len, a_len) = (total(&before_text), total(&after_text));
        let in_context = row.contextual;
        let keep = shown.len() < MAX_ROWS
            && !(in_context && plan.context_cap.is_some_and(|cap| context >= cap));
        if !keep {
            omitted.blocks += 1;
            omitted.text_bytes += b_len + a_len;
            continue;
        }
        if in_context {
            context += 1;
        }
        let budget = MAX_ROW_TEXT.min(MAX_CASE_TEXT - used);
        let (b_share, a_share) = shares(b_len, a_len, budget);
        let mut side =
            |present: bool, version: Option<Option<Hash>>, text: &[TextRow], n: usize| {
                if !present {
                    return Value::Null;
                }
                let (value, spent) = side_json(version.flatten(), text, n, &mut omitted);
                used += spent;
                value
            };
        let before = side(row.before.is_some(), before_version, &before_text, b_share);
        let after = side(row.after.is_some(), after_version, &after_text, a_share);
        let headings = clipped_headings(headings, &mut omitted);
        shown.push(json!({
            "id": row.id.to_string(),
            "type": block.map(|b| b.block_type.clone()),
            "doc": (!doc.is_empty()).then_some(doc),
            "headings": headings,
            "role": row.role,
            "before": before,
            "after": after,
        }));
    }
    let code = plan
        .code
        .read(&mut digest, &mut omitted, MAX_CASE_TEXT - used)?;
    let case = json!({
        "kind": spec.name,
        "question": spec.question,
        "blocks": shown,
        "code": code,
        "omitted": {
            "blocks": omitted.blocks,
            "text_bytes": omitted.text_bytes,
            "code_files": omitted.code_files,
            "code_bytes": omitted.code_bytes,
        },
        "inputs_hash": digest.finish().to_string(),
        "schema": check_schema(),
    });
    Ok(BuiltCase {
        hash: check_case_hash(&case),
        case,
        subject_blocks: plan.subject_blocks,
    })
}

/// The verdict schema every check but contradiction shows: submissions validate against it.
pub(crate) fn check_schema() -> Value {
    json!({
        "verdict": ["pass", "concern", "fail"],
        "citations": "0 to 20 {block, note} entries; concern and fail need at least one; each note has 1 to 2000 characters",
        "reason": "concern and fail need 1 to 2000 characters; pass may omit it",
    })
}

/// The hash a verdict binds to: `rdy-case-2\0`, then the case as JSON, keys sorted.
fn check_case_hash(case: &Value) -> Hash {
    let mut bytes = CASE_DOMAIN.to_vec();
    bytes.extend(serde_json::to_vec(case).expect("a JSON value serializes"));
    Hash::of(&bytes)
}
