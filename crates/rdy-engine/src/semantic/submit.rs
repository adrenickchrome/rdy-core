//! rdy_review_submit (spec §9.3 step 4): a reviewer's verdict on a review request. The schema,
//! the case hash, precedence (D-TF2W6, D-RF315), actor independence, and the advice to block
//! (D-WMCFW), which gates nothing.
use crate::semantic::candidates::{request_hex, request_label};
use crate::semantic::case::case_hash;
use crate::semantic::lease::LeaseRegistry;
use crate::tensions::semantic::pair_of;
use crate::tensions::{SideRef, TensionSeverity, tension_label};
use crate::{
    E_BAD_INPUT, E_NOT_FOUND, E_PERMISSION, E_REVIEW_INVALID, E_REVIEW_STALE, E_SELF_REVIEW, Rdy,
    RdyError, ReadState, Session, now_ms,
};
use rdy_model::{
    BlockId, Finding, Hash, PairItem, Review, ReviewSubject, Role, SemanticTension, Verdict,
    hash_of,
};
use serde::{Deserialize, Serialize};

/// The most findings one verdict takes.
pub const MAX_FINDINGS: usize = 20;
/// The most characters a finding's note, or the advice's reason, takes after trimming.
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
    pub findings: Vec<FindingRequest>,
    /// `advisory` (the default) or `blocking`: advice to a person; it gates nothing.
    #[serde(default)]
    pub severity: Option<String>,
    /// With severity `blocking` only: why the tension should block.
    #[serde(default)]
    pub reason: Option<String>,
}

/// One finding of a verdict: a block of the case and what in it disagrees.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingRequest {
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

fn invalid(message: impl Into<String>, fix: &str) -> RdyError {
    RdyError::new(E_REVIEW_INVALID, message, fix)
}

fn stale(message: impl Into<String>) -> RdyError {
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

/// A submit's input after the checks that read no state (steps 1 to 4): the role, the verdict
/// word, the request id and the case hash text.
#[derive(Clone, Debug)]
pub struct CheckedSubmit {
    session: Session,
    req: ReviewSubmitRequest,
    verdict: Verdict,
    hex: String,
    sent_hash: Hash,
}

impl CheckedSubmit {
    /// Checks session `s`'s submit `req` against everything but the state.
    pub fn new(s: &Session, req: ReviewSubmitRequest) -> Result<CheckedSubmit, RdyError> {
        // 1. role
        if s.role != Role::Reviewer {
            return Err(RdyError::new(
                E_PERMISSION,
                "rdy_review_submit needs the reviewer role",
                "start a session with role reviewer",
            ));
        }
        // 2. verdict word
        let verdict = match req.verdict.as_str() {
            "tension" => Verdict::Tension,
            "no_tension" => Verdict::NoTension,
            v => {
                return Err(invalid(
                    format!("verdict {v:?} is not tension or no_tension"),
                    "send the corrected payload: verdict tension or no_tension",
                ));
            }
        };
        // 3. request id
        let Some(hex) = request_hex(&req.request) else {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{:?} is not a review request id", req.request),
                "pass the request from rdy_review_next: rq- and 12 hex digits",
            ));
        };
        // 4. case hash text
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
            verdict,
            hex,
            sent_hash,
        })
    }
}

/// The request a submit names: its key and its pair, as (block, content).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundRequest {
    pub key: Hash,
    pub pair: Vec<(BlockId, Hash)>,
}

impl ReadState {
    /// Step 5: the request `c` names, by its key prefix: from the live leases, then the settled
    /// pairs, and only when both find nothing, the queue. It only reads, so the daemon runs it on
    /// a read, outside the writer (candidate requests are never computed on the writer); the
    /// key is content-addressed, so [`Rdy::submit_found`] re-proves on the writer that the pair
    /// is still current.
    pub fn find_request(
        &self,
        leases: &LeaseRegistry,
        c: &CheckedSubmit,
        now: i64,
    ) -> Result<FoundRequest, RdyError> {
        let hex = &c.hex;
        let given = format!("rq-{}", &hex[..hex.len().min(12)]);
        let mut found: Vec<(Hash, Vec<(BlockId, Hash)>)> = leases
            .with_prefix(hex, now)
            .into_iter()
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
                .filter(|q| q.key.to_string().starts_with(hex.as_str()))
                .map(|q| {
                    let pair = q.items.iter().map(|i| (i.block.clone(), i.content));
                    (q.key, pair.collect())
                })
                .collect();
        }
        match found.len() {
            0 => Err(not_current(&given)),
            1 => {
                let (key, pair) = found.remove(0);
                Ok(FoundRequest { key, pair })
            }
            n => Err(RdyError::new(
                E_NOT_FOUND,
                format!("{} matches {n} requests", c.req.request),
                "pass more hex digits",
            )),
        }
    }
}

impl Rdy {
    /// Records session `s`'s verdict on review request `req.request`: checks the role, the
    /// schema, that the request is current, precedence, the case hash and actor independence,
    /// then writes the review (and, for a `tension`, the semantic tension's first version) and
    /// frees the request's lease.
    ///
    /// In process there is no writer to keep free, so it finds the request on its own state.
    /// The daemon finds it on a read and runs only [`Rdy::submit_found`] on the writer
    /// (`run_review_submit`).
    pub fn submit_verdict(
        &mut self,
        s: &Session,
        req: ReviewSubmitRequest,
    ) -> Result<SubmitInfo, RdyError> {
        let c = CheckedSubmit::new(s, req)?;
        let found = self.read().find_request(&self.leases(), &c, now_ms())?;
        self.submit_found(c, found)
    }

    /// Steps 6 to 11 of a submit whose request `found` was found on a read: re-proves that the
    /// pair is current (the items now, then the case hash), then precedence, actor independence
    /// and the write. Computes no candidate requests.
    pub fn submit_found(
        &mut self,
        c: CheckedSubmit,
        found: FoundRequest,
    ) -> Result<SubmitInfo, RdyError> {
        let CheckedSubmit {
            session,
            req,
            verdict,
            sent_hash,
            ..
        } = c;
        let s = &session;
        let FoundRequest { key, pair } = found;
        let st = self.read();
        let leases = self.leases();
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
        // 7. findings
        if req.findings.len() > MAX_FINDINGS {
            return Err(invalid(
                "a verdict takes at most 20 findings",
                "keep the 20 findings that matter most",
            ));
        }
        if verdict == Verdict::Tension && req.findings.is_empty() {
            return Err(invalid(
                "a tension verdict needs at least one finding",
                "add a finding that says what in a block disagrees",
            ));
        }
        let mut findings = vec![];
        for (n, f) in req.findings.iter().enumerate() {
            let i = n + 1;
            let block = BlockId::parse(&f.block)
                .ok()
                .filter(|b| items.iter().any(|it| &it.block == b));
            let Some(block) = block else {
                let names: Vec<String> = items.iter().map(|it| it.block.to_string()).collect();
                return Err(invalid(
                    format!(
                        "finding {i}: block {} is not one of {}",
                        f.block,
                        names.join(" and ")
                    ),
                    "cite one of the two blocks of the case",
                ));
            };
            let Some(note) = bounded(&f.note) else {
                return Err(invalid(
                    format!("finding {i}: a note needs 1 to 2000 characters"),
                    "say in a few words what disagrees",
                ));
            };
            findings.push(Finding {
                block,
                note: note.to_string(),
            });
        }
        // 7b. advice (D-WMCFW)
        let advice = match req.severity.as_deref() {
            None | Some("advisory") => {
                if req.reason.is_some() {
                    return Err(invalid(
                        "reason goes with severity blocking: it says why the tension should block",
                        "drop reason, or pass severity blocking",
                    ));
                }
                None
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
                Some(reason.to_string())
            }
            Some(other) => {
                return Err(invalid(
                    format!("severity {other:?} is not advisory or blocking"),
                    "pass severity advisory or blocking",
                ));
            }
        };
        // 8. precedence (D-TF2W6, D-RF315): a tension wins; only a tension overrides a no_tension
        if let Some(settled) = st.semantic().settled(&key) {
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
        // 9. case hash
        if case_hash(&st.case_of(&key, &items)?) != sent_hash {
            return Err(stale(format!(
                "the case of {label} changed since you read it"
            )));
        }
        // 10. actor independence
        for i in &items {
            if st.item_authors_of(&key, i)?.contains(&s.actor) {
                return Err(RdyError::new(
                    E_SELF_REVIEW,
                    format!(
                        "actor {} wrote block {} of {label}; another run of the same actor is not independent",
                        s.actor, i.block
                    ),
                    "a reviewer of another actor judges it",
                ));
            }
        }
        drop(st);
        // 11. write
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
            findings,
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
        leases.release(&key);
        let mut info = SubmitInfo {
            request: label,
            review: id.to_string(),
            verdict: req.verdict,
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
