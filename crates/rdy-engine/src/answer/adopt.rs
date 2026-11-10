//! The answers to an adopt card (repo links, D-8VJAZ, D-MEMMQ). The card stands in for a
//! briefing: its `adopt` answer is the approval of its own isolated landing, and nothing else
//! rides in that landing.
//!
//! | option  | act                                                                        |
//! |---------|----------------------------------------------------------------------------|
//! | `keep`  | one `ANSWER` `keep_pin`: the card's hash and the person's reason; no pin moves |
//! | `adopt` | the isolated landing of the delivery (`Rdy::prepare_adopt_landing`) and one `ANSWER` `adopt` that names it |
//! | `later` | the batch acts' Later ([`super::batch`])                                   |
//!
//! # Keep the approved pins
//!
//! The note is the reason, 1 to 2000 characters after trimming. The `ANSWER` binds the card's
//! complete hash and the person (local) or the relay with the current card evidence rules. No
//! view, change, landing or pin is written: the card leaves until a later delivery changes
//! something again.
//!
//! # Adopt the delivery
//!
//! The whole adoption is prepared before anything is appended. From the card's binding, on the
//! canon the card was read on:
//! - each pin row that names a delivered version moves the pin of every citing block that holds
//!   the row's old version to the delivered content. A rejected decision keeps its pin (it never
//!   changes again); an accepted decision's pin moves under the adoption's own frozen-decision
//!   exception, bound to exactly these moves;
//! - each instruction row moves the instruction baseline the link block pins (C14): an added or
//!   changed instruction is pinned at its delivered content, a removed one is unpinned;
//! - a pin row of a block that is no longer delivered keeps its pin, and a rebind row moves
//!   nothing here.
//!
//! The adoption records the answering person as the approver, the card as its evidence (every
//! label, and `Adopt the delivery`; a relay with text evidence is refused; an elicited answer
//! keeps its `elicited` kind) and one `Adopt` ANSWER that names the landing, so the card's walk
//! reads the delivery as adopted. It writes one change and one landing in the reserved view
//! `adopt-<card hash, first 12 digits>`, asks for no second approval and checks only readiness
//! and freshness. When the card moves no pin (a rebind or a set change alone), the `Adopt` ANSWER
//! is written alone. A refusal writes nothing, and the card stays open with its hash.
use super::{
    AnswerInfo, AnswerStep, Answerer, Bound, PreparedAnswer, assurance, changed, labels, put_alone,
};
use crate::error::{E_BAD_INPUT, E_INTERNAL, E_ITEM_MOVED, E_PERMISSION, RdyError};
use crate::gate::AdoptLanding;
use crate::pending::PlanCardBinding;
use crate::pending::adopt::{ADOPT_LABEL, AdoptInstruction, AdoptPin, KEEP_PIN_LABEL};
use crate::{Rdy, ReadState, Scope};
use rdy_model::decision::{self, DecisionStatus};
use rdy_model::engine_fields::MAX_REASON_CHARS;
use rdy_model::foreign::{ForeignId, ForeignPin};
use rdy_model::{
    Answer, AnswerAct, ApprovalEvidence, Block, BlockId, EvidenceKind, Hash, Op, RepoId,
};
use rdy_store::PinAdoption;
use rdy_store::foreign::HeldVersion;
use std::collections::BTreeMap;

/// Runs the act of the option `b` chose on its adopt card (the table in the module docs). The
/// landing of `adopt` is only prepared: [`super::Rdy::commit_answer`] lands it.
pub(crate) fn act(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerStep, RdyError> {
    match b.option.id.as_str() {
        "adopt" => adopt(r, by, b, note),
        "keep" => keep(r, by, b, note).map(|i| AnswerStep::Done(Box::new(i))),
        other => Err(RdyError::new(
            E_BAD_INPUT,
            format!("{other} is not an option of {}", b.item.r#ref),
            "answer with one of: adopt, keep, later",
        )),
    }
}

/// The card's complete hash, as the queue computed it.
fn card_hash(b: &Bound) -> Result<Hash, RdyError> {
    b.item
        .card_hash
        .parse()
        .map_err(|_| bug(b, "the card has no hash"))
}

fn bug(b: &Bound, what: &str) -> RdyError {
    RdyError::new(
        E_INTERNAL,
        format!("{}: {what}", b.item.r#ref),
        "report this as a bug in rdy",
    )
}

/// The person's answer record of this card: act `act`, the card's labels and hash, the person
/// and the relay, and `note`. An adopt landing fills in its target.
fn record(
    by: &Answerer,
    b: &Bound,
    act: AnswerAct,
    label: &str,
    note: Option<String>,
) -> Result<Answer, RdyError> {
    let relay = by.relay.as_ref();
    Ok(Answer {
        item: b.item.r#ref.clone(),
        act,
        option: Some(b.option.id.clone()),
        label: Some(label.to_string()),
        options: labels(&b.item),
        card_hash: Some(card_hash(b)?),
        targets: vec![],
        person: by.who.provenance(),
        relayed_by: relay.map(|r| r.by.clone()),
        relay_evidence: relay.map(|r| r.evidence.clone()),
        evidence_kind: by.kind,
        note,
    })
}

fn info(b: Bound, by: &Answerer, act: AnswerAct, answer: Hash) -> AnswerInfo {
    let relay = by.relay.clone();
    AnswerInfo {
        r#ref: b.item.r#ref,
        act: act.as_str().into(),
        option: b.option.id,
        label: b.option.label,
        assurance: assurance(&relay),
        answer: Some(answer.to_string()),
        landing: None,
        relayed_by: relay.map(|r| r.by),
        next: None,
    }
}

/// `keep`: one `KeepPin` ANSWER with the person's reason (see the module docs).
fn keep(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerInfo, RdyError> {
    let note = note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty() && n.chars().count() <= MAX_REASON_CHARS)
        .ok_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                format!("{KEEP_PIN_LABEL} needs a note"),
                "pass note with a reason of 1 to 2000 characters",
            )
        })?;
    let a = record(by, &b, AnswerAct::KeepPin, KEEP_PIN_LABEL, Some(note))?;
    let h = put_alone(r, &a)?;
    Ok(info(b, by, AnswerAct::KeepPin, h))
}

/// The adoption a card's binding makes on canon: the updates, each on its block's canon
/// version, and the foreign pin moves they make.
struct Adoption {
    ops: Vec<Op>,
    moves: PinAdoption,
}

/// `adopt`: the card's isolated landing, prepared under the writer, or its `Adopt` ANSWER alone
/// when it moves no pin (see the module docs).
fn adopt(
    r: &mut Rdy,
    by: &Answerer,
    b: Bound,
    note: Option<String>,
) -> Result<AnswerStep, RdyError> {
    if !by.who.role.can_land() {
        return Err(RdyError::new(
            E_PERMISSION,
            format!("role {:?} cannot land proposals", by.who.role),
            "ask an approver to land it",
        ));
    }
    // the landing's evidence is the card the person answered, local or relayed: a relay that
    // calls its evidence the person's own words is refused before anything is written
    if by.kind == Some(EvidenceKind::Text) {
        return Err(RdyError::new(
            E_BAD_INPUT,
            "an adopt card's landing records card evidence, not text",
            "relay the answer with kind card, or leave kind out",
        ));
    }
    let answer = record(by, &b, AnswerAct::Adopt, ADOPT_LABEL, note)?;
    let adoption = adoption_of(&r.read(), &b)?;
    if adoption.ops.is_empty() {
        let h = put_alone(r, &answer)?;
        return Ok(AnswerStep::Done(Box::new(info(b, by, AnswerAct::Adopt, h))));
    }
    // an elicited answer keeps its kind (D-6M0AF); every other answer records the card
    let kind = match by.kind {
        Some(EvidenceKind::Elicited) => EvidenceKind::Elicited,
        _ => EvidenceKind::Card,
    };
    let evidence = ApprovalEvidence {
        kind,
        options: labels(&b.item),
        choice: Some(ADOPT_LABEL.to_string()),
        card_hash: Some(card_hash(&b)?),
    };
    let relay = by.relay.clone();
    let (landing, h) = r.prepare_adopt_landing(AdoptLanding {
        card: &b.item,
        approver: by.who.clone(),
        relay,
        evidence,
        ops: adoption.ops,
        adoption: adoption.moves,
        answer,
    })?;
    Ok(AnswerStep::Approval(Box::new(PreparedAnswer {
        landing,
        info: info(b, by, AnswerAct::Adopt, h),
        next_session: None,
    })))
}

/// The adoption of the card `b` binds, on canon (see the module docs). A row whose delivered
/// version, or whose citing block's held version, is no longer the card's is `E_ITEM_MOVED`.
fn adoption_of(st: &ReadState, b: &Bound) -> Result<Adoption, RdyError> {
    let Some(PlanCardBinding::Adopt {
        repo,
        link,
        pins,
        instructions,
        ..
    }) = &b.item.plan_binding
    else {
        return Err(bug(b, "the card binds no delivery"));
    };
    let moved = || changed(E_ITEM_MOVED, &b.item.r#ref);
    let foreign = st.foreign()?;
    let members = &foreign.link_source(link).ok_or_else(moved)?.members;
    let canon = st.materialized(&Scope::Canon)?;
    let mut blocks: BTreeMap<BlockId, Block> = BTreeMap::new();
    let mut moves = PinAdoption::default();
    for row in pins {
        pin_row(st, &canon, members, row, &mut blocks, &mut moves).map_err(|e| e.or(moved()))?;
    }
    if !instructions.is_empty() {
        let base = canon.get(link).ok_or_else(moved)?;
        let block = blocks.entry(link.clone()).or_insert_with(|| base.clone());
        for row in instructions {
            instruction_row(repo, members, row, block, &mut moves).ok_or_else(moved)?;
        }
    }
    let mut ops = vec![];
    let mut changed_blocks = vec![];
    for (id, block) in blocks {
        let base = canon.get(&id).ok_or_else(moved)?;
        if *base == block {
            continue;
        }
        changed_blocks.push(block.clone());
        ops.push(Op::Update {
            base: base.version_hash(),
            block,
        });
    }
    // every pin the adoption writes names a version a delivery holds
    st.check_held_pins(&Scope::Canon, &changed_blocks)?;
    Ok(Adoption { ops, moves })
}

/// Why a row does not apply: the card moved, or a read failed.
enum RowError {
    Moved,
    Read(RdyError),
}

impl RowError {
    fn or(self, moved: RdyError) -> RdyError {
        match self {
            RowError::Moved => moved,
            RowError::Read(e) => e,
        }
    }
}

impl From<RdyError> for RowError {
    fn from(e: RdyError) -> RowError {
        RowError::Read(e)
    }
}

/// The delivered pin of `id` at version `version`, when the link's newest delivery holds it.
fn delivered(
    members: &BTreeMap<BlockId, HeldVersion>,
    id: &BlockId,
    version: Hash,
) -> Option<ForeignPin> {
    members
        .get(id)
        .filter(|h| h.version.version == version)
        .map(|h| ForeignPin {
            content: h.version.content,
            encoding: h.version.encoding,
        })
}

/// Moves the pin of each citing block of `row` to its delivered content (see the module docs).
fn pin_row(
    st: &ReadState,
    canon: &crate::Materialized,
    members: &BTreeMap<BlockId, HeldVersion>,
    row: &AdoptPin,
    blocks: &mut BTreeMap<BlockId, Block>,
    moves: &mut PinAdoption,
) -> Result<(), RowError> {
    // a block that is no longer delivered keeps its pin
    let Some(new) = row.new else {
        return Ok(());
    };
    let to = delivered(members, &row.target.id, new).ok_or(RowError::Moved)?;
    for citer in &row.citers {
        let base = canon.get(citer).ok_or(RowError::Moved)?;
        if base.is_redacted() || decision::status(base) == Some(DecisionStatus::Rejected) {
            continue;
        }
        let from = base
            .foreign
            .pins
            .get(&row.target)
            .cloned()
            .ok_or(RowError::Moved)?;
        if st.foreign_held_pin(&row.target, &from)? != row.old {
            return Err(RowError::Moved);
        }
        let block = blocks.entry(citer.clone()).or_insert_with(|| base.clone());
        block.foreign.pins.insert(row.target.clone(), to.clone());
        moves.insert(citer.clone(), row.target.clone(), from, to.clone());
    }
    Ok(())
}

/// Moves the link block's pin of the instruction of `row` (C14): the delivered content for an
/// added or changed instruction, no pin for a removed one. None when the delivery no longer
/// holds the row's version.
fn instruction_row(
    repo: &RepoId,
    members: &BTreeMap<BlockId, HeldVersion>,
    row: &AdoptInstruction,
    link: &mut Block,
    moves: &mut PinAdoption,
) -> Option<()> {
    let target = ForeignId::new(*repo, row.id.clone());
    match row.new {
        Some(v) => {
            let to = delivered(members, &row.id, v)?;
            if let Some(from) = link.foreign.pins.insert(target.clone(), to.clone())
                && from != to
            {
                moves.insert(link.id.clone(), target, from, to);
            }
        }
        None => {
            link.foreign.pins.remove(&target);
        }
    }
    Some(())
}
