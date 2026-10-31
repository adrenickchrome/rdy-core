//! A verdict on a durable check request (D-JQD5X, O10). A contradiction request writes the
//! existing pair review and advisory tension; every other check writes one Check review. The
//! request is resolved and its case rebuilt on a read; the writer rebuilds it again when the log
//! moved since, so a verdict never binds a case the writer did not see.
use super::catalogue::AgentCheck;
use super::requests::{
    Answers, Rebuilt, authored_block, build_subject, durable_label, holder, rebuild_now,
    verdict_word,
};
use crate::semantic::candidates::Item;
use crate::semantic::submit::{
    CheckedSubmit, MAX_CITATIONS, PairVerdict, SubmitInfo, bounded_note, invalid, pair_advice,
    pair_citations, pair_independence, pair_precedence, pair_verdict, require_reviewer,
    self_review, stale,
};
use crate::{E_PERMISSION, E_REVIEW_STALE, Rdy, RdyError, ReadState, Session};
use rdy_model::check_request::{CheckMode, CheckRequest};
use rdy_model::{BlockId, Citation, Hash, Review, ReviewSubject, Verdict, pair_key};
use rdy_store::StoreError;
use rdy_store::segment::Location;
use serde_json::Value;

/// The most bytes a reason or a note takes, checked before its characters are counted.
const MAX_TEXT_BYTES: usize = 8000;

/// A durable request a submit names, read on a state: the request, that state's log end, and its
/// case rebuilt there.
#[derive(Clone, Debug)]
pub struct FoundCheck {
    request: Hash,
    record: CheckRequest,
    seq: Location,
    rebuilt: Rebuilt,
}

impl ReadState {
    /// The durable request `h`, with its case rebuilt against its scope in this state.
    pub(crate) fn found_check(&self, h: Hash) -> Result<FoundCheck, RdyError> {
        let record = self.reader().check_request(&h)?;
        let rebuilt = rebuild_now(self, &record)?;
        Ok(FoundCheck {
            request: h,
            record,
            seq: self.seq(),
            rebuilt,
        })
    }
}

/// `E_REVIEW_STALE`: the request's case changed. An author-run request is run again; an
/// independent one is claimed again.
fn case_changed(label: &str, mode: CheckMode) -> RdyError {
    let fix = match mode {
        CheckMode::Author => "run the check again: rdy_review {op: \"run\"} / rdy review run",
        CheckMode::Independent => "call rdy_review_next again",
    };
    RdyError::new(
        E_REVIEW_STALE,
        format!("request {label} is no longer current: its case changed"),
        fix,
    )
}

/// `text` trimmed when it holds at most [`MAX_TEXT_BYTES`] bytes and 1 to 2000 characters.
fn bounded_text(text: &str) -> Option<&str> {
    (text.len() <= MAX_TEXT_BYTES)
        .then(|| bounded_note(text))
        .flatten()
}

/// The block ids a check case shows.
fn shown_blocks(case: &Value) -> Vec<&str> {
    case["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b["id"].as_str())
        .collect()
}

/// The verdict word of a check other than contradiction.
fn check_verdict(word: &str) -> Result<Verdict, RdyError> {
    match word {
        "pass" => Ok(Verdict::Pass),
        "concern" => Ok(Verdict::Concern),
        "fail" => Ok(Verdict::Fail),
        v => Err(invalid(
            format!("verdict {v:?} is not pass, concern or fail"),
            "send the corrected payload: verdict pass, concern or fail",
        )),
    }
}

/// The citations and the stored reason of a check verdict, against the blocks its case shows.
fn check_schema(
    verdict: Verdict,
    case: &Value,
    c: &CheckedSubmit,
) -> Result<(Vec<Citation>, String), RdyError> {
    let req = c.request();
    let word = verdict_word(verdict);
    if req.citations.len() > MAX_CITATIONS {
        return Err(invalid(
            "a verdict takes at most 20 citations",
            "keep the 20 citations that matter most",
        ));
    }
    if verdict != Verdict::Pass && req.citations.is_empty() {
        return Err(invalid(
            format!("a {word} verdict needs at least one citation"),
            "cite the blocks that support the verdict",
        ));
    }
    let shown = shown_blocks(case);
    let mut citations = Vec::with_capacity(req.citations.len());
    for (n, f) in req.citations.iter().enumerate() {
        let i = n + 1;
        let block = BlockId::parse(&f.block)
            .ok()
            .filter(|b| shown.contains(&b.as_str()));
        let Some(block) = block else {
            return Err(invalid(
                format!("citation {i}: block {} is not shown in this case", f.block),
                "cite a block shown in the case",
            ));
        };
        let Some(note) = bounded_text(&f.note) else {
            return Err(invalid(
                format!("citation {i}: a note needs 1 to 2000 characters"),
                "say in a few words what disagrees",
            ));
        };
        citations.push(Citation {
            block,
            note: note.to_string(),
        });
    }
    let reason = match (verdict, req.reason.as_deref()) {
        (Verdict::Pass, None) => String::new(),
        (Verdict::Pass, Some(r)) => bounded_text(r)
            .ok_or_else(|| {
                invalid(
                    "a pass reason needs 1 to 2000 characters",
                    "give a short reason, or omit reason",
                )
            })?
            .to_string(),
        (_, r) => r
            .and_then(bounded_text)
            .ok_or_else(|| {
                invalid(
                    format!("a {word} verdict needs a reason of 1 to 2000 characters"),
                    "say what the check found",
                )
            })?
            .to_string(),
    };
    Ok((citations, reason))
}

/// A store refusal of a verdict as the stale refusal of `label`; any other store error as it maps
/// everywhere.
fn refused(e: StoreError) -> RdyError {
    match e {
        StoreError::Refused(message) => {
            RdyError::new(E_REVIEW_STALE, message, "call rdy_review_next again")
        }
        e => e.into(),
    }
}

impl Rdy {
    /// The writer part of a verdict on durable request `f`: who may answer (O10), whether a
    /// verdict settled it, the verdict word, the case now (rebuilt when the log moved since `f`
    /// was read), the case hash sent, the schema, independence, then the write. No refusal
    /// writes anything.
    pub(crate) fn submit_check(
        &mut self,
        c: CheckedSubmit,
        f: FoundCheck,
    ) -> Result<SubmitInfo, RdyError> {
        let st = self.read();
        let s = c.session().clone();
        let req = c.request().clone();
        let FoundCheck {
            request,
            record,
            seq,
            rebuilt,
        } = f;
        let label = durable_label(&request);
        let check = AgentCheck::parse(&record.check);
        // who may answer
        match record.mode {
            CheckMode::Author => {
                if s.actor != record.requested_by.actor {
                    return Err(RdyError::new(
                        E_PERMISSION,
                        format!(
                            "{label} is an author-run check of {}",
                            record.requested_by.actor
                        ),
                        "claim an independent request: rdy_review_next / rdy review list --open",
                    ));
                }
                if !s.role.can_write() {
                    return Err(RdyError::new(
                        E_PERMISSION,
                        format!("role {:?} cannot write", s.role),
                        "start the session as a writer",
                    ));
                }
            }
            CheckMode::Independent => require_reviewer(&s)?,
        }
        let contradiction = check == Some(AgentCheck::Contradiction);
        // a verdict that settles the request
        if !contradiction && let Some((h, r)) = Answers::read(&st)?.of(&request, &record).first() {
            return Err(stale(format!(
                "request {label} is settled: review {} by {} answered {}",
                h.short(),
                r.provenance.actor,
                verdict_word(r.verdict)
            )));
        }
        // the verdict word, and no severity on a check
        let verdict = if contradiction {
            pair_verdict(&req.verdict)?
        } else {
            let v = check_verdict(&req.verdict)?;
            if req.severity.is_some() {
                return Err(invalid(
                    "severity is not part of a check verdict",
                    "drop severity; a person can accept the verdict as blocking",
                ));
            }
            v
        };
        // the case now: the read's rebuild holds only while the log has not moved
        let rebuilt = if st.seq() == seq {
            rebuilt
        } else {
            rebuild_now(&st, &record)?
        };
        let case = match rebuilt {
            Rebuilt::Built(case) if case.hash == record.case_hash => case,
            Rebuilt::Built(_) | Rebuilt::Changed => return Err(case_changed(&label, record.mode)),
            Rebuilt::Unavailable(e) => return Err(e),
        };
        if c.sent_hash() != record.case_hash {
            return Err(stale(format!(
                "the case of {label} changed since you read it"
            )));
        }
        if contradiction {
            drop(case);
            return self.submit_pair(&s, c, &request, &record, verdict, &label);
        }
        let (citations, reason) = check_schema(verdict, &case.case, &c)?;
        // independence: an independent reviewer wrote no primary block of the subject
        if record.mode == CheckMode::Independent {
            let h = holder(&st, &record.scope, &record.subject)?
                .ok_or_else(|| case_changed(&label, record.mode))?;
            if let Some(b) = authored_block(&st, &h, &record.subject.blocks, &s.actor)? {
                return Err(self_review(&s.actor, &b, &label));
            }
        }
        drop(st);
        let review = Review {
            subject: ReviewSubject::Check { request },
            case_hash: Some(record.case_hash),
            verdict,
            reason,
            citations,
            provenance: s.provenance(),
        };
        let id = self
            .store_mut()
            .put_check_review(&review)
            .map_err(refused)?;
        self.publish(crate::read_state::Delta::Seq)?;
        self.leases().release(&request);
        Ok(SubmitInfo {
            request: label,
            review: id.to_string(),
            verdict: req.verdict,
            tension: None,
            severity: None,
            advice: None,
            suppressed: false,
        })
    }

    /// A verdict on a durable contradiction request: the existing pair review and advisory
    /// tension, with today's citations, advice and precedence. The pair may stand in canon, and
    /// an author-run request skips the independence step; nothing else differs.
    fn submit_pair(
        &mut self,
        s: &Session,
        c: CheckedSubmit,
        request: &Hash,
        record: &CheckRequest,
        verdict: Verdict,
        label: &str,
    ) -> Result<SubmitInfo, RdyError> {
        let st = self.read();
        let req = c.request().clone();
        let items = pair_items(&st, record)?.ok_or_else(|| case_changed(label, record.mode))?;
        let key = pair_key(
            &items
                .iter()
                .map(|i| rdy_model::PairItem {
                    block: i.block.clone(),
                    content: i.content,
                })
                .collect::<Vec<_>>(),
        );
        let citations = pair_citations(&items, verdict, &req)?;
        let advice = pair_advice(verdict, &req)?;
        pair_precedence(&st, &key, verdict, label)?;
        if record.mode == CheckMode::Independent {
            pair_independence(&st, &key, &items, s, label)?;
        }
        drop(st);
        self.write_pair(
            s,
            label.to_string(),
            &req.verdict,
            PairVerdict {
                key,
                items,
                verdict,
                sent_hash: c.sent_hash(),
                citations,
                advice,
            },
            &[*request, key],
        )
    }
}

/// The items of a contradiction request's pair at the contents its scope holds now: the in-flight
/// form, else canon's.
fn pair_items(st: &ReadState, record: &CheckRequest) -> Result<Option<Vec<Item>>, RdyError> {
    let subject = build_subject(&record.subject);
    let [a, b] = subject.blocks.as_slice() else {
        return Ok(None);
    };
    let Some(h) = holder(st, &record.scope, &subject)? else {
        return Ok(None);
    };
    let tracking = st.reader().tracking();
    let content = |id: &BlockId| h.after().get(id).map(|x| x.content_hash_with(tracking));
    let (Some(ca), Some(cb)) = (content(a), content(b)) else {
        return Ok(None);
    };
    let pair = [(a.clone(), ca), (b.clone(), cb)];
    Ok(match st.pair_now(&pair)? {
        Some(items) => Some(items),
        None => st.pair_in_canon(&pair)?,
    })
}
