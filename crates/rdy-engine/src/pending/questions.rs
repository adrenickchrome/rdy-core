//! Open questions (F7): the `answer` items of the queue.
//!
//! A question is an item when both hold:
//! - it is open in canon: no accepted decision links to it with `answers` (as the coverage
//!   report counts an open question, `IssueCode::OpenQuestion`), and the person has not answered
//!   its current version (an answer waits for its decision to land);
//! - it carries authored `options`: a list of two to four non-blank labels (F7).
//!
//! Any other open question is no item and stays on the coverage report. The list is closed.
//!
//! The ref is `<Q-id>@<version short>` (O3), so a new version of the question is a new item
//! and ends a snooze. `title` and `why` are the authored words (the title falls back to the
//! prompt). The options are the authored labels as `o1` to `o4`, then `later` (O4). The question's
//! `recommended` label, when it is one of the options, is the recommendation, with
//! `recommend_reason` as its reason. Whoever last wrote the question raised it.
use super::item::{Item, ItemKind, ItemOption};
use super::queue::{PendingQuery, answered, item, later, text};
use crate::error::RdyError;
use crate::session::{now_ms, rfc3339};
use crate::{ReadState, Scope};
use rdy_graph::EdgeKind;
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::{AnswerAct, Block, BlockId};

/// The most options a card offers (`o1` to `o4`).
const OPTIONS_MAX: usize = 4;

/// One `answer` item per open question in canon that carries authored options, by ref.
pub(crate) fn question_items(st: &ReadState, _q: &PendingQuery) -> Result<Vec<Item>, RdyError> {
    let m = st.materialized(&Scope::Canon)?;
    let g = st.graph(&Scope::Canon)?;
    let mut open: Vec<(&Block, Vec<String>)> = vec![];
    for b in m.iter().filter(|b| b.block_type == decision::QUESTION) {
        let Some(labels) = labels_of(b) else { continue };
        let answered_by_decision = g.incoming(&b.id).iter().any(|e| {
            matches!(&e.kind, EdgeKind::Link(n) if n == decision::ANSWERS)
                && m.get(&e.from)
                    .is_some_and(|d| decision::status(d) == Some(DecisionStatus::Accepted))
        });
        if !answered_by_decision {
            open.push((b, labels));
        }
    }
    let ids: Vec<BlockId> = open.iter().map(|(b, _)| b.id.clone()).collect();
    // who wrote each question last, and when (a read error leaves the line blank)
    let last = st.last_changes(&Scope::Canon, &ids).unwrap_or_default();
    let now = now_ms();
    let mut out = vec![];
    for (b, labels) in open {
        let r = format!("{}@{}", b.id, b.version_hash().short());
        if answered(st, &r, AnswerAct::AnswerQuestion)? {
            continue;
        }
        let title = text(b, "title")
            .or_else(|| text(b, "prompt"))
            .unwrap_or_default();
        let mut it = item(ItemKind::Answer, &r, title);
        it.why = text(b, "why").unwrap_or_default().to_string();
        it.options = options(b, &labels);
        if let Some(h) = last.get(&b.id) {
            it.raised_by.clone_from(&h.author.actor);
            it.raised_at = rfc3339(h.author.at_ms);
            it.age = super::item::age(h.author.at_ms, now);
        }
        out.push(it);
    }
    out.sort_by(|a, b| a.r#ref.cmp(&b.r#ref));
    Ok(out)
}

/// The authored option labels of `b`: two to four non-blank strings, else `None`.
fn labels_of(b: &Block) -> Option<Vec<String>> {
    let list = b.fields.get("options")?;
    let rdy_model::FieldValue::List(entries) = list else {
        return None;
    };
    let labels: Vec<String> = entries
        .iter()
        .map(|e| {
            e.as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        })
        .collect::<Option<_>>()?;
    (2..=OPTIONS_MAX).contains(&labels.len()).then_some(labels)
}

/// `o1` to `o4` for the authored labels, then `later`.
fn options(b: &Block, labels: &[String]) -> Vec<ItemOption> {
    let recommended = text(b, "recommended").map(str::trim);
    let reason = text(b, "recommend_reason").unwrap_or_default();
    let mut out: Vec<ItemOption> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| ItemOption {
            id: format!("o{}", i + 1),
            label: label.clone(),
            consequences: vec!["Records a decision with this answer.".to_string()],
            recommended: (recommended == Some(label.as_str())).then(|| reason.to_string()),
        })
        .collect();
    out.push(later());
    out
}
