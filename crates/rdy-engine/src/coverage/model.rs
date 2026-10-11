//! The coverage issue model (FW-CH1, D-XQVM1): each issue names its block, its dimension, a
//! closed code and the fix. Coverage is computed from the current state, never stored (D-NMGD2).
use rdy_model::BlockId;
use serde::Serialize;
use std::collections::BTreeMap;

pub use rdy_schema::CoverageDimension as Dimension;

/// What a coverage issue is. Declaration order is the sort order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    UntiedContent,
    UnlinkedMention,
    OpenTension,
    DismissedTension,
    OpenSuspect,
    UnlinkedDoc,
    DanglingRef,
    StubSection,
    MissingSection,
    OpenQuestion,
    Ungrounded,
    UnappliedDecision,
}

impl IssueCode {
    /// The dimension an issue of this code counts against.
    pub fn dimension(&self) -> Dimension {
        match self {
            IssueCode::UntiedContent | IssueCode::UnlinkedMention => Dimension::Grounded,
            IssueCode::OpenTension | IssueCode::DismissedTension => Dimension::Consistent,
            IssueCode::OpenSuspect => Dimension::Current,
            IssueCode::UnlinkedDoc | IssueCode::DanglingRef => Dimension::Connected,
            IssueCode::StubSection | IssueCode::MissingSection | IssueCode::OpenQuestion => {
                Dimension::Complete
            }
            IssueCode::Ungrounded | IssueCode::UnappliedDecision => Dimension::Governed,
        }
    }
}

/// The review that dismissed a tension an issue reports.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Dismissal {
    pub review: String,
    pub actor: String,
    pub reason: String,
}

/// One coverage issue: the block, the dimension, the code, what it is about (an entity id, a
/// tension label, a suspect's target, a doc path, a section title or a dangling target), and the
/// fix. A dismissed tension carries its dismissal.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CoverageIssue {
    pub block: String,
    pub dimension: Dimension,
    pub code: IssueCode,
    pub subject: Option<String>,
    pub fix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dismissed: Option<Dismissal>,
}

impl CoverageIssue {
    /// An issue of `code` on `block`, about `subject`, with `fix`.
    pub(crate) fn new(
        block: &BlockId,
        code: IssueCode,
        subject: Option<String>,
        fix: String,
    ) -> CoverageIssue {
        CoverageIssue {
            block: block.to_string(),
            dimension: code.dimension(),
            code,
            subject,
            fix,
            dismissed: None,
        }
    }
}

/// One scored block: its doc, how many checks apply to it in each dimension (in
/// `Dimension::ALL` order), and its issues, sorted.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BlockEntry {
    pub doc: Option<BlockId>,
    pub checks: [u32; 6],
    pub issues: Vec<CoverageIssue>,
}

impl BlockEntry {
    /// Adds one check in dimension `d`.
    pub(crate) fn check(&mut self, d: Dimension) {
        self.checks[dim_index(d)] += 1;
    }
}

/// The position of `d` in `Dimension::ALL`.
pub(crate) fn dim_index(d: Dimension) -> usize {
    Dimension::ALL
        .iter()
        .position(|x| *x == d)
        .expect("every dimension is in ALL")
}

/// The structural layer of a scope's coverage: the Grounded, Connected, Complete and Governed
/// checks and issues of every scored block (unlinked mentions listed with no dismissal applied),
/// and each entity mapped to the blocks whose text names it, linked or not.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Structural {
    #[serde(serialize_with = "blocks_map")]
    pub blocks: imbl::OrdMap<BlockId, BlockEntry>,
    #[serde(serialize_with = "mentions_map")]
    pub mentions: imbl::OrdMap<BlockId, imbl::OrdSet<BlockId>>,
}

/// The coverage issues of a scope in all six dimensions: the structural layer with dismissed
/// mentions removed and the tension and suspect issues added. `mentions` as in [`Structural`].
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CoverageIssues {
    #[serde(serialize_with = "blocks_map")]
    pub blocks: imbl::OrdMap<BlockId, BlockEntry>,
    #[serde(serialize_with = "mentions_map")]
    pub mentions: imbl::OrdMap<BlockId, imbl::OrdSet<BlockId>>,
}

/// `blocks` as a JSON object by block id (imbl is built without its serde feature).
fn blocks_map<S: serde::Serializer>(
    m: &imbl::OrdMap<BlockId, BlockEntry>,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.collect_map(m.iter())
}

/// `mentions` as a JSON object: each entity id and the block ids that name it, sorted.
fn mentions_map<S: serde::Serializer>(
    m: &imbl::OrdMap<BlockId, imbl::OrdSet<BlockId>>,
    s: S,
) -> Result<S::Ok, S::Error> {
    s.collect_map(m.iter().map(|(k, v)| (k, v.iter().collect::<Vec<_>>())))
}

/// Sorts `issues` by the closed order: the path of the block's doc (from `paths`, by block id;
/// empty when absent), the block id, the dimension, the code and the subject.
pub fn sort_issues(issues: &mut [CoverageIssue], paths: &BTreeMap<String, String>) {
    let path = |i: &CoverageIssue| paths.get(&i.block).cloned().unwrap_or_default();
    issues.sort_by(|a, b| {
        (path(a), &a.block, a.dimension, a.code, &a.subject).cmp(&(
            path(b),
            &b.block,
            b.dimension,
            b.code,
            &b.subject,
        ))
    });
}

/// The fix of `untied_content`.
pub(crate) fn fix_untied_content(block: &BlockId) -> String {
    format!("link {block} to the entity it describes, with describes or a ref")
}

/// The fix of `unlinked_mention`.
pub(crate) fn fix_unlinked_mention(name: &str, entity: &BlockId) -> String {
    format!("link this mention to {name} ({entity}), or dismiss it as a passing mention")
}

/// The fix of `open_tension`.
pub(crate) fn fix_open_tension(label: &str, kind: &str) -> String {
    format!("resolve {label} ({kind}) with a change, or dismiss it with a reason")
}

/// The fix of `dismissed_tension`.
pub(crate) fn fix_dismissed_tension(label: &str) -> String {
    format!("{label} is dismissed for the gate; resolve it with a change to clear it here")
}

/// The fix of `open_suspect`.
pub(crate) fn fix_open_suspect(block: &BlockId, target: &BlockId) -> String {
    format!("confirm this suspect: update {block}, or repin it to {target}")
}

/// The fix of `unlinked_doc`.
pub(crate) fn fix_unlinked_doc(path: &str) -> String {
    format!("link to {path} from another doc")
}

/// The fix of `dangling_ref`.
pub(crate) fn fix_dangling_ref(target: &BlockId) -> String {
    format!("fix the reference to {target}, which does not exist")
}

/// The fix of `missing_section`.
pub(crate) fn fix_missing_section(title: &str) -> String {
    format!("add the required section {title}")
}

/// The fix of `stub_section`.
pub(crate) fn fix_stub_section(title: &str) -> String {
    format!("write content under {title}, or remove the section")
}

/// The fix of `open_question`.
pub(crate) fn fix_open_question(block: &BlockId) -> String {
    format!("answer {block} with a decision (answers), or remove the question")
}

/// The fix of `ungrounded`.
pub(crate) fn fix_ungrounded(block: &BlockId) -> String {
    format!("link {block} to the decision that governs it, with decided_by or implements")
}

/// The fix of `unapplied_decision`.
pub(crate) fn fix_unapplied_decision(block: &BlockId) -> String {
    format!(
        "apply {block}: link the content that carries it with decided_by, or set needs_content: false"
    )
}
