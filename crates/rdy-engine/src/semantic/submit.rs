//! rdy_review_submit (spec §9.3 step 4): a reviewer's verdict on a review request. The schema,
//! the case hash, precedence (D-TF2W6, D-RF315), actor independence, and the advice to block
//! (D-WMCFW), which gates nothing. A request id names a derived request of a pair or a durable
//! check request (D-JQD5X); the durable path is `crate::check::submit`.
use crate::check::submit::FoundCheck;
use crate::semantic::candidates::{Item, request_hex, request_label};
use crate::semantic::case::case_hash;
use crate::semantic::lease::LeaseRegistry;
use crate::tensions::semantic::pair_of;
use crate::tensions::{SideRef, TensionSeverity, tension_label};
use crate::{
    E_BAD_INPUT, E_NOT_FOUND, E_PERMISSION, E_REVIEW_INVALID, E_REVIEW_STALE, E_SELF_REVIEW, Rdy,
    RdyError, ReadState, Session, now_ms,
};
use rdy_model::{
    BlockId, Citation, Hash, PairItem, Review, ReviewSubject, Role, SemanticTension, Verdict,
    hash_of,
};
use serde::{Deserialize, Serialize};

/// The most citations one verdict takes.
pub const MAX_CITATIONS: usize = 20;
/// The most characters a citation's note, or the advice's reason, takes after trimming.
pub const MAX_NOTE: usize = 2000;

/// `rdy_review_submit`'s input.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSubmitRequest {
    /// The request from `rdy_review_next` (`rq-` and hex digits).
    pub request: String,
    /// The case hash from `rdy_review_next` (64 hex).
    pub case_hash: String,
    /// `tension` or `no_tension`.
    pub verdict: String,
    #[serde(default)]
    pub citations: Vec<CitationRequest>,
    /// `advisory` (the default) or `blocking`: advice to a person; it gates nothing.
    #[serde(default)]
    pub severity: Option<String>,
    /// With severity `blocking` only: why the tension should block.
    #[serde(default)]
    pub reason: Option<String>,
}

/// One citation of a verdict: a block of the case and what in it disagrees.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CitationRequest {
    pub block: String,
    pub note: String,
}

/// What a submit recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SubmitInfo {
    /// The request's label.
    pub request: String,
    /// The review's id (64 hex).
    pub review: String,
    pub verdict: String,
    /// The label of the semantic tension a `tension` verdict opened; absent when the state after
    /// the write could not be read (the verdict is stored all the same).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tension: Option<String>,
    /// The tension's severity: always `advisory` for a new one; absent when `tension` is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<TensionSeverity>,
    /// The reason, when the verdict advised blocking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advice: Option<String>,
    /// True when a `no_tension` verdict suppressed the pair.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub suppressed: bool,
}

pub(crate) fn invalid(message: impl Into<String>, fix: &str) -> RdyError {
    RdyError::new(E_REVIEW_INVALID, message, fix)
}

pub(crate) fn stale(message: impl Into<String>) -> RdyError {
    RdyError::new(E_REVIEW_STALE, message, "call rdy_review_next again")
}

fn not_current(label: &str) -> RdyError {
    stale(format!(
        "request {label} is no longer current: a side changed, landed or left every view"
    ))
}

/// The trimmed text when it has 1 to [`MAX_NOTE`] characters.
fn bounded(text: &str) -> Option<&str> {
    let t = text.trim();
    (!t.is_empty() && t.chars().count() <= MAX_NOTE).then_some(t)
}

/// A submit's input after the checks that read no state: the request id and the case hash text.
/// The role and the verdict word are checked once the request is resolved: its kind says which
/// apply.
#[derive(Clone, Debug)]
pub struct CheckedSubmit {
    session: Session,
    req: ReviewSubmitRequest,
    hex: String,
    sent_hash: Hash,
}

impl CheckedSubmit {
    /// Checks session `s`'s submit `req` against everything but the state.
    pub fn new(s: &Session, req: ReviewSubmitRequest) -> Result<CheckedSubmit, RdyError> {
        // 1. request id
        let Some(hex) = request_hex(&req.request) else {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{:?} is not a review request id", req.request),
                "pass the request id from rdy_review or rdy_review_next",
            ));
        };
        // 2. case hash text
        let case_text = req.case_hash.to_ascii_lowercase();
        let Ok(sent_hash) = case_text.parse::<Hash>() else {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "case_hash must be 64 hex digits",
                "pass case_hash from rdy_review_next",
            ));
        };
        Ok(CheckedSubmit {
            session: s.clone(),
            req,
            hex,
            sent_hash,
        })
    }

    pub(crate) fn session(&self) -> &Session {
        &self.session
    }

    pub(crate) fn request(&self) -> &ReviewSubmitRequest {
        &self.req
    }

    pub(crate) fn sent_hash(&self) -> Hash {
        self.sent_hash
    }
}

/// The reviewer role a derived request and an independent check need.
pub(crate) fn require_reviewer(s: &Session) -> Result<(), RdyError> {
    if s.role != Role::Reviewer {
        return Err(RdyError::new(
            E_PERMISSION,
            "rdy_review_submit needs the reviewer role",
            "start a session with role reviewer",
        ));
    }
    Ok(())
}

/// The verdict word of a pair: `tension` or `no_tension`.
pub(crate) fn pair_verdict(word: &str) -> Result<Verdict, RdyError> {
    match word {
        "tension" => Ok(Verdict::Tension),
        "no_tension" => Ok(Verdict::NoTension),
        v => Err(invalid(
            format!("verdict {v:?} is not tension or no_tension"),
            "send the corrected payload: verdict tension or no_tension",
        )),
    }
}

/// The trimmed text when it has 1 to [`MAX_NOTE`] characters.
pub(crate) fn bounded_note(text: &str) -> Option<&str> {
    bounded(text)
}

/// The request a submit names: its key and its pair, as (block, content).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundRequest {
    pub key: Hash,
    pub pair: Vec<(BlockId, Hash)>,
}

/// The request a submit names: a derived request of a pair, or a durable check request.
#[derive(Clone, Debug)]
pub enum ResolvedRequest {
    Derived(FoundRequest),
    Check(FoundCheck),
}

/// The ambiguity refusal: `given` names `n` requests.
fn ambiguous(given: &str, n: usize) -> RdyError {
    RdyError::new(
        E_NOT_FOUND,
        format!("{given} matches {n} requests"),
        "pass more hex digits",
    )
}

impl ReadState {
    /// The derived requests whose key starts with `hex`: from the live leases of pairs, then the
    /// settled pairs, and only when both find nothing, the queue. A durable request's lease
    /// names no pair and is not one of them.
    fn derived_matches(
        &self,
        leases: &LeaseRegistry,
        hex: &str,
        now: i64,
    ) -> Result<Vec<(Hash, Vec<(BlockId, Hash)>)>, RdyError> {
        let mut found: Vec<(Hash, Vec<(BlockId, Hash)>)> = leases
            .with_prefix(hex, now)
            .into_iter()
            .filter(|(_, l)| !l.pair.is_empty())
            .map(|(k, l)| (k, l.pair))
            .collect();
        if found.is_empty() {
            found = self
                .semantic()
                .settled_with_prefix(hex)
                .into_iter()
                .map(|(k, v)| {
                    let pair = v.items.iter().map(|i| (i.block.clone(), i.content));
                    (k, pair.collect())
                })
                .collect();
        }
        if found.is_empty() {
            found = self
                .review_requests()?
                .into_iter()
                .filter(|q| q.key.to_string().starts_with(hex))
                .map(|q| {
                    let pair = q.items.iter().map(|i| (i.block.clone(), i.content));
                    (q.key, pair.collect())
                })
                .collect();
        }
        Ok(found)
    }

    /// Step 5: the derived request `c` names, by its key prefix: from the live leases, then the
    /// settled pairs, and only when both find nothing, the queue. It only reads, so the daemon
    /// runs it on a read, outside the writer (candidate requests are never computed on the
    /// writer); the key is content-addressed, so [`Rdy::submit_found`] re-proves on the writer
    /// that the pair is still current.
    pub fn find_request(
        &self,
        leases: &LeaseRegistry,
        c: &CheckedSubmit,
        now: i64,
    ) -> Result<FoundRequest, RdyError> {
        let hex = &c.hex;
        let given = format!("rq-{}", &hex[..hex.len().min(12)]);
        let mut found = self.derived_matches(leases, hex, now)?;
        match found.len() {
            0 => Err(not_current(&given)),
            1 => {
                let (key, pair) = found.remove(0);
                Ok(FoundRequest { key, pair })
            }
            n => Err(ambiguous(&c.req.request, n)),
        }
    }

    /// The request `c` names, in both namespaces: a durable check request (12 to 64 hex digits)
    /// and a derived request. A prefix that names more than one request in all is refused. With
    /// no durable request, the session needs the reviewer role before the derived requests are
    /// read, as before. A durable request is read and its case rebuilt here, on a read; the
    /// writer re-proves it when the log moved ([`Rdy::submit_resolved`]).
    pub fn resolve_submit(
        &self,
        leases: &LeaseRegistry,
        c: &CheckedSubmit,
        now: i64,
    ) -> Result<ResolvedRequest, RdyError> {
        let durable = crate::check::requests::durable_matches(self, &c.hex)?;
        if durable.is_empty() {
            require_reviewer(&c.session)?;
            return Ok(ResolvedRequest::Derived(self.find_request(leases, c, now)?));
        }
        let derived = self.derived_matches(leases, &c.hex, now)?;
        let n = durable.len() + derived.len();
        if n > 1 {
            return Err(ambiguous(&c.req.request, n));
        }
        Ok(ResolvedRequest::Check(self.found_check(durable[0])?))
    }
}

/// Step 7: the citations of a pair verdict, each on one of the pair's two blocks.
pub(crate) fn pair_citations(
    items: &[Item],
    verdict: Verdict,
    req: &ReviewSubmitRequest,
) -> Result<Vec<Citation>, RdyError> {
    if req.citations.len() > MAX_CITATIONS {
        return Err(invalid(
            "a verdict takes at most 20 citations",
            "keep the 20 citations that matter most",
        ));
    }
    if verdict == Verdict::Tension && req.citations.is_empty() {
        return Err(invalid(
            "a tension verdict needs at least one citation",
            "add a citation that says what in a block disagrees",
        ));
    }
    let mut citations = vec![];
    for (n, f) in req.citations.iter().enumerate() {
        let i = n + 1;
        let block = BlockId::parse(&f.block)
            .ok()
            .filter(|b| items.iter().any(|it| &it.block == b));
        let Some(block) = block else {
            let names: Vec<String> = items.iter().map(|it| it.block.to_string()).collect();
            return Err(invalid(
                format!(
                    "citation {i}: block {} is not one of {}",
                    f.block,
                    names.join(" and ")
                ),
                "cite one of the two blocks of the case",
            ));
        };
        let Some(note) = bounded(&f.note) else {
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
    Ok(citations)
}

/// Step 7b: the advice to block of a pair verdict (D-WMCFW).
pub(crate) fn pair_advice(
    verdict: Verdict,
    req: &ReviewSubmitRequest,
) -> Result<Option<String>, RdyError> {
    match req.severity.as_deref() {
        None | Some("advisory") => {
            if req.reason.is_some() {
                return Err(invalid(
                    "reason goes with severity blocking: it says why the tension should block",
                    "drop reason, or pass severity blocking",
                ));
            }
            Ok(None)
        }
        Some("blocking") => {
            if verdict == Verdict::NoTension {
                return Err(invalid(
                    "severity applies to a tension verdict only",
                    "drop severity",
                ));
            }
            let Some(reason) = req.reason.as_deref().and_then(bounded) else {
                return Err(invalid(
                    "advice to block needs a reason of 1 to 2000 characters",
                    "say in a few words why a landing should wait",
                ));
            };
            Ok(Some(reason.to_string()))
        }
        Some(other) => Err(invalid(
            format!("severity {other:?} is not advisory or blocking"),
            "pass severity advisory or blocking",
        )),
    }
}

/// Step 8: precedence (D-TF2W6, D-RF315): a tension wins; only a tension overrides a
/// no_tension.
pub(crate) fn pair_precedence(
    st: &ReadState,
    key: &Hash,
    verdict: Verdict,
    label: &str,
) -> Result<(), RdyError> {
    if let Some(settled) = st.semantic().settled(key) {
        let found = match (settled.verdict, verdict) {
            (Verdict::Tension, _) => Some("found a tension"),
            (Verdict::NoTension, Verdict::NoTension) => Some("found no tension"),
            _ => None,
        };
        if let Some(found) = found {
            return Err(stale(format!(
                "request {label} is settled: review {} by {} {found}",
                settled.review.short(),
                settled.actor
            )));
        }
    }
    Ok(())
}

/// Step 10: actor independence: the session's actor wrote no item of the pair.
pub(crate) fn pair_independence(
    st: &ReadState,
    key: &Hash,
    items: &[Item],
    s: &Session,
    label: &str,
) -> Result<(), RdyError> {
    for i in items {
        if st.item_authors_of(key, i)?.contains(&s.actor) {
            return Err(self_review(&s.actor, &i.block, label));
        }
    }
    Ok(())
}

/// `E_SELF_REVIEW`: `actor` wrote `block` of the request `label`.
pub(crate) fn self_review(actor: &str, block: &BlockId, label: &str) -> RdyError {
    RdyError::new(
        E_SELF_REVIEW,
        format!(
            "actor {actor} wrote block {block} of {label}; another run of the same actor is not independent"
        ),
        "a reviewer of another actor judges it",
    )
}

/// What a pair verdict writes: its pair, verdict, citations and advice.
pub(crate) struct PairVerdict {
    pub(crate) key: Hash,
    pub(crate) items: Vec<Item>,
    pub(crate) verdict: Verdict,
    pub(crate) sent_hash: Hash,
    pub(crate) citations: Vec<Citation>,
    pub(crate) advice: Option<String>,
}

impl Rdy {
    /// Records session `s`'s verdict on review request `req.request`: a derived request of a
    /// pair, or a durable check request (see [`ReadState::resolve_submit`]).
    ///
    /// In process there is no writer to keep free, so it finds the request on its own state.
    /// The daemon finds it on a read and runs only [`Rdy::submit_resolved`] on the writer
    /// (`run_review_submit`).
    pub fn submit_verdict(
        &mut self,
        s: &Session,
        req: ReviewSubmitRequest,
    ) -> Result<SubmitInfo, RdyError> {
        let c = CheckedSubmit::new(s, req)?;
        let found = self.read().resolve_submit(&self.leases(), &c, now_ms())?;
        self.submit_resolved(c, found)
    }

    /// The writer part of a submit whose request `found` was resolved on a read.
    pub fn submit_resolved(
        &mut self,
        c: CheckedSubmit,
        found: ResolvedRequest,
    ) -> Result<SubmitInfo, RdyError> {
        match found {
            ResolvedRequest::Derived(f) => self.submit_found(c, f),
            ResolvedRequest::Check(f) => self.submit_check(c, f),
        }
    }

    /// Steps 6 to 11 of a submit whose derived request `found` was found on a read: the role
    /// and the verdict word, then re-proves that the pair is current (the items now, then the
    /// case hash), then precedence, actor independence and the write. Computes no candidate
    /// requests.
    pub fn submit_found(
        &mut self,
        c: CheckedSubmit,
        found: FoundRequest,
    ) -> Result<SubmitInfo, RdyError> {
        let CheckedSubmit {
            session,
            req,
            sent_hash,
            ..
        } = c;
        let s = &session;
        require_reviewer(s)?;
        let verdict = pair_verdict(&req.verdict)?;
        let FoundRequest { key, pair } = found;
        let st = self.read();
        let label = request_label(&key);
        // 6. the items now (final ruling 2: as the queue names them, per view)
        let Some(items) = st.pair_now(&pair)? else {
            return Err(not_current(&label));
        };
        if !items.iter().any(|i| matches!(i.side, SideRef::Change(_))) {
            return Err(stale(format!(
                "request {label} ended: both blocks are in canon now"
            )));
        }
        // 7. citations, 7b. advice
        let citations = pair_citations(&items, verdict, &req)?;
        let advice = pair_advice(verdict, &req)?;
        // 8. precedence
        pair_precedence(&st, &key, verdict, &label)?;
        // 9. case hash
        if case_hash(&st.case_of(&key, &items)?) != sent_hash {
            return Err(stale(format!(
                "the case of {label} changed since you read it"
            )));
        }
        // 10. actor independence
        pair_independence(&st, &key, &items, s, &label)?;
        drop(st);
        // 11. write
        self.write_pair(
            s,
            label,
            &req.verdict,
            PairVerdict {
                key,
                items,
                verdict,
                sent_hash,
                citations,
                advice,
            },
            &[key],
        )
    }

    /// Step 11: writes pair verdict `v` as session `s` and frees the leases `release`.
    pub(crate) fn write_pair(
        &mut self,
        s: &Session,
        label: String,
        word: &str,
        v: PairVerdict,
        release: &[Hash],
    ) -> Result<SubmitInfo, RdyError> {
        let PairVerdict {
            key,
            items,
            verdict,
            sent_hash,
            citations,
            advice,
        } = v;
        let leases = self.leases();
        let pair_items: Vec<PairItem> = items
            .iter()
            .map(|i| PairItem {
                block: i.block.clone(),
                content: i.content,
            })
            .collect();
        let provenance = s.provenance();
        let review = Review {
            subject: ReviewSubject::Pair {
                key,
                items: pair_items.clone(),
            },
            case_hash: Some(sent_hash),
            verdict,
            reason: String::new(),
            citations,
            provenance: provenance.clone(),
        };
        let first = (verdict == Verdict::Tension).then(|| SemanticTension {
            key,
            items: pair_items,
            review: hash_of(&review),
            advice: advice.clone(),
            blocking: false,
            relay: None,
            relayed_by: None,
            prev: None,
            provenance,
        });
        let (id, _) = self.record_verdict(&review, first.as_ref())?;
        for k in release {
            leases.release(k);
        }
        let mut info = SubmitInfo {
            request: label,
            review: id.to_string(),
            verdict: word.to_string(),
            tension: None,
            severity: None,
            advice: None,
            suppressed: false,
        };
        if verdict == Verdict::Tension {
            // the verdict is stored: a failed lookup gives no label and never fails the reply
            // final review M8: the tensions of one of the pair's blocks, not the whole set
            let open = self.read().tensions().ok().and_then(|set| {
                set.involving_block(&items[0].block)
                    .into_iter()
                    .find(|t| pair_of(t) == Some(key))
                    .map(|t| (tension_label(&t.id), t.severity))
            });
            info.tension = open.as_ref().map(|(label, _)| label.clone());
            info.severity = open.map(|(_, severity)| severity);
            info.advice = advice;
        } else {
            info.suppressed = true;
        }
        Ok(info)
    }
}
