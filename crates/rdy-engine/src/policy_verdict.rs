//! ap6: the authority policy's verdict on one ready version (D-ZWP7Q, D-DDSBJ, D-5W9CM, D-2WR0G).
//!
//! [`verdict`] is the only place that decides whether the policy accepts a version. It is pure:
//! the caller reads the store and gives the facts as [`VerdictInputs`]. The order:
//!
//! 1. no policy block, or a policy block whose fields do not read: a person;
//! 2. the never-list, every item that applies, in its order: a person;
//! 3. a policy with no rule: a person;
//! 4. the first rule, in policy order, that covers the whole version: accepted;
//! 5. else a person, with one reason per rule: its first failed condition.
//!
//! [`fact_value`] gives the card's `Policy` fact from the stored verdict.
use crate::decision_fields::DELEGATED;
use crate::pending::ExceptionKind;
use crate::readiness::assessment::cut_text;
use rdy_model::BlockId;
use rdy_model::assessment::{AssessedPolicy, MAX_POLICY_REASONS, PolicyRef, PolicyVerdict};
use rdy_schema::policy::{DecisionsRule, JustificationKind, Lifecycle, PolicyFields, Rule};

/// The reason with no policy block.
pub const NO_POLICY: &str = "no authority policy is set";
/// The reason of a policy with no rule.
pub const NO_RULE: &str = "the policy has no agent_may_accept rule";
/// The bases of a person's ruling, which the policy never accepts.
const RULING_BASES: [&str; 2] = ["direct_ruling", "recorded_ruling"];
/// The basis an absent `authority_basis` reads as.
const NO_BASIS: &str = "none";
/// The reversibility a decision declares to count as `two_way`.
const TWO_WAY: &str = "two_way";

/// A decision block the version creates or updates, read in the prospective canon (O7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrittenDecision {
    pub id: BlockId,
    pub basis: Option<String>,
    pub reversibility: Option<String>,
    pub attested_by: Option<String>,
    pub areas: Vec<String>,
}

/// A block the version creates, updates or deletes: its areas and whether it is in stable content
/// in canon or in the prospective canon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WrittenBlock {
    pub id: BlockId,
    pub areas: Vec<String>,
    pub stable: bool,
}

/// One change of the version: its short hash, its author and its justification kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeFacts {
    pub short: String,
    pub author: String,
    pub justification: Option<JustificationKind>,
}

/// What the verdict reads of a version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VerdictInputs {
    pub proposer: String,
    pub changes: Vec<ChangeFacts>,
    pub blocks: Vec<WrittenBlock>,
    pub decisions: Vec<WrittenDecision>,
    pub exceptions: Vec<(ExceptionKind, String)>,
    pub redaction: bool,
    pub changes_policy: bool,
    pub noncode_deferrals: usize,
    pub stable_docs: Vec<BlockId>,
    pub plan_items: usize,
}

/// The one verdict (O8). `policy` is the live policy's ref and its fields (None when the block no
/// longer validates, O8 step 1); `role` the approve item's required role label.
pub fn verdict(
    policy: Option<(&PolicyRef, Option<&PolicyFields>)>,
    i: &VerdictInputs,
    role: Option<String>,
) -> AssessedPolicy {
    let Some((pref, fields)) = policy else {
        return needs_person(None, None, vec![NO_POLICY.to_string()]);
    };
    let pref = Some(pref.clone());
    let Some(fields) = fields else {
        let reason = format!("the authority policy {} does not validate", block_of(&pref));
        return needs_person(pref, None, vec![reason]);
    };
    let view = Sorted::of(i);
    let never = never_list(i, &view);
    if !never.is_empty() {
        return needs_person(pref, role, never);
    }
    if fields.rules.is_empty() {
        return needs_person(pref, role, vec![NO_RULE.to_string()]);
    }
    let policy_id = block_of(&pref);
    let mut reasons = Vec::with_capacity(fields.rules.len());
    for rule in &fields.rules {
        match first_failure(rule, i, &view, &policy_id) {
            None => {
                return AssessedPolicy {
                    policy: pref,
                    verdict: PolicyVerdict::Accepts {
                        rule: rule.name.clone(),
                    },
                };
            }
            Some(why) => reasons.push(format!("rule {}: {why}", rule.name)),
        }
    }
    needs_person(pref, role, reasons)
}

/// O9: the `Policy` card fact's value. The label `<ap-id>@<short>` comes from `a.policy`.
pub fn fact_value(a: &AssessedPolicy) -> String {
    let label = a
        .policy
        .as_ref()
        .map(|p| format!("{}@{}: ", p.block, p.version.short()))
        .unwrap_or_default();
    let text = match &a.verdict {
        PolicyVerdict::Accepts { rule } => format!("{label}accepts under rule {rule}"),
        PolicyVerdict::NeedsPerson {
            role: Some(role),
            reasons,
        } => format!("{label}needs a person from {role}: {}", reasons.join("; ")),
        PolicyVerdict::NeedsPerson {
            role: None,
            reasons,
        } => format!("{label}needs a person: {}", reasons.join("; ")),
    };
    cut_text(text)
}

/// A verdict that needs a person, its reasons cut to [`MAX_POLICY_REASONS`] and each cut as a
/// card fact is.
fn needs_person(
    policy: Option<PolicyRef>,
    role: Option<String>,
    mut reasons: Vec<String>,
) -> AssessedPolicy {
    reasons.truncate(MAX_POLICY_REASONS);
    AssessedPolicy {
        policy,
        verdict: PolicyVerdict::NeedsPerson {
            role,
            reasons: reasons.into_iter().map(cut_text).collect(),
        },
    }
}

/// The policy block's id text.
fn block_of(p: &Option<PolicyRef>) -> String {
    p.as_ref().map(|p| p.block.to_string()).unwrap_or_default()
}

/// O8 step 2: every never-list item that applies, in O8's order.
fn never_list(i: &VerdictInputs, view: &Sorted<'_>) -> Vec<String> {
    let mut out = Vec::new();
    if i.redaction {
        out.push("it carries a redaction request".to_string());
    }
    if i.changes_policy {
        out.push("it changes the authority policy".to_string());
    }
    for (kind, reference) in &i.exceptions {
        out.push(format!(
            "it has the exception {} on {reference}",
            kind.name()
        ));
        if out.len() > MAX_POLICY_REASONS {
            return out;
        }
    }
    if i.noncode_deferrals > 0 {
        out.push(format!(
            "it defers {} items that are not code items",
            i.noncode_deferrals
        ));
    }
    for d in &view.decisions {
        if let Some(basis) = d.basis.as_deref()
            && RULING_BASES.contains(&basis)
        {
            out.push(format!("decision {} has basis {basis}", d.id));
        }
    }
    let mut docs: Vec<&BlockId> = i.stable_docs.iter().collect();
    docs.sort();
    docs.dedup();
    for doc in docs {
        out.push(format!("it makes doc {doc} stable"));
    }
    out
}

/// The written blocks and decisions in id order.
struct Sorted<'a> {
    blocks: Vec<&'a WrittenBlock>,
    decisions: Vec<&'a WrittenDecision>,
}

impl<'a> Sorted<'a> {
    fn of(i: &'a VerdictInputs) -> Sorted<'a> {
        let mut blocks: Vec<&WrittenBlock> = i.blocks.iter().collect();
        blocks.sort_by(|a, b| a.id.cmp(&b.id));
        let mut decisions: Vec<&WrittenDecision> = i.decisions.iter().collect();
        decisions.sort_by(|a, b| a.id.cmp(&b.id));
        Sorted { blocks, decisions }
    }
}

/// Whether every one of `areas` is in `allowed`. A block in no area is inside none of them.
fn inside(areas: &[String], allowed: &[String]) -> bool {
    !areas.is_empty() && areas.iter().all(|a| allowed.contains(a))
}

/// Whether `text` names `id` as a whole word: the id with no id character on either side.
fn names(text: &str, id: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '-';
    text.match_indices(id).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + id.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

fn justification_name(k: JustificationKind) -> &'static str {
    match k {
        JustificationKind::Implements => "implements",
        JustificationKind::Editorial => "editorial",
        JustificationKind::Reflects => "reflects",
    }
}

/// O8 step 5: the first condition of `rule` the version fails, in O8's order; None when the rule
/// covers the whole version.
fn first_failure(
    rule: &Rule,
    i: &VerdictInputs,
    view: &Sorted<'_>,
    policy_id: &str,
) -> Option<String> {
    let covered = |actor: &str| {
        rule.actors
            .iter()
            .any(|p| rdy_schema::policy::pattern_matches(p, actor))
    };
    let actors =
        std::iter::once(i.proposer.as_str()).chain(i.changes.iter().map(|c| c.author.as_str()));
    for actor in actors {
        if !covered(actor) {
            return Some(format!("actor {actor} matches none of its actors"));
        }
    }
    if !rule.areas.is_empty()
        && let Some(b) = view.blocks.iter().find(|b| !inside(&b.areas, &rule.areas))
    {
        return Some(format!("block {} is outside its areas", b.id));
    }
    if rule.lifecycle == Lifecycle::Draft
        && let Some(b) = view.blocks.iter().find(|b| b.stable)
    {
        return Some(format!("block {} is in stable content", b.id));
    }
    for c in &i.changes {
        match c.justification {
            None => return Some(format!("change {} has no justification", c.short)),
            Some(k) if !rule.justifications.contains(&k) => {
                return Some(format!(
                    "change {} is {}, which it does not accept",
                    c.short,
                    justification_name(k)
                ));
            }
            Some(_) => {}
        }
    }
    if let Some(why) = decision_failure(rule, view, policy_id) {
        return Some(why);
    }
    if i.blocks.len() > rule.max_blocks as usize {
        return Some(format!(
            "it changes {} blocks; the rule allows {}",
            i.blocks.len(),
            rule.max_blocks
        ));
    }
    if i.plan_items > rule.max_items as usize {
        return Some(format!(
            "its change plan holds {} items; the rule allows {}",
            i.plan_items, rule.max_items
        ));
    }
    None
}

/// The decision conditions of `rule` (D-DDSBJ, D-2WR0G), each over every decision in id order
/// before the next condition.
fn decision_failure(rule: &Rule, view: &Sorted<'_>, policy_id: &str) -> Option<String> {
    let ds = &view.decisions;
    if rule.decisions == DecisionsRule::None {
        return ds.first().map(|d| format!("it records decision {}", d.id));
    }
    if let Some(d) = ds.iter().find(|d| !inside(&d.areas, &rule.two_way_areas)) {
        return Some(format!("decision {} is outside its two_way_areas", d.id));
    }
    if let Some(d) = ds
        .iter()
        .find(|d| d.reversibility.as_deref() != Some(TWO_WAY))
    {
        return Some(format!("decision {} is not declared two_way", d.id));
    }
    let basis = |d: &WrittenDecision| d.basis.clone().unwrap_or_else(|| NO_BASIS.to_string());
    if let Some(d) = ds.iter().find(|d| !rule.basis.contains(&basis(d))) {
        return Some(format!(
            "decision {} has basis {}, which it does not accept",
            d.id,
            basis(d)
        ));
    }
    if let Some(d) = ds.iter().find(|d| {
        basis(d) == DELEGATED
            && !d
                .attested_by
                .as_deref()
                .is_some_and(|a| names(a, policy_id))
    }) {
        return Some(format!(
            "decision {} is delegated, and its attested_by does not name {policy_id}",
            d.id
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use rdy_model::Hash;
    use rdy_model::assessment::{MAX_POLICY_NAME_BYTES, MAX_POLICY_REASON_BYTES};
    use std::collections::{BTreeMap, BTreeSet};

    fn id(s: &str) -> BlockId {
        BlockId::parse(s).unwrap()
    }

    fn pref() -> PolicyRef {
        PolicyRef {
            block: id("ap-AAAAA"),
            version: Hash::of(b"policy v1"),
        }
    }

    fn rule(name: &str) -> Rule {
        Rule {
            name: name.to_string(),
            actors: vec!["agent:nick/*".to_string()],
            areas: vec![],
            lifecycle: Lifecycle::Any,
            justifications: BTreeSet::from([
                JustificationKind::Implements,
                JustificationKind::Editorial,
            ]),
            decisions: DecisionsRule::None,
            two_way_areas: vec![],
            basis: BTreeSet::new(),
            max_blocks: 20,
            max_items: 20,
        }
    }

    fn two_way_rule(name: &str) -> Rule {
        Rule {
            decisions: DecisionsRule::TwoWay,
            two_way_areas: vec!["api".to_string()],
            basis: BTreeSet::from(["none".to_string(), "delegated".to_string()]),
            ..rule(name)
        }
    }

    fn fields(rules: Vec<Rule>) -> PolicyFields {
        PolicyFields {
            rules,
            roles: BTreeMap::new(),
            require: vec![],
            owners: vec![],
        }
    }

    fn block(s: &str) -> WrittenBlock {
        WrittenBlock {
            id: id(s),
            areas: vec!["api".to_string()],
            stable: false,
        }
    }

    fn decision(s: &str) -> WrittenDecision {
        WrittenDecision {
            id: id(s),
            basis: None,
            reversibility: Some("two_way".to_string()),
            attested_by: None,
            areas: vec!["api".to_string()],
        }
    }

    /// A version by `agent:nick/a1` of one editorial change to one block.
    fn version() -> VerdictInputs {
        VerdictInputs {
            proposer: "agent:nick/a1".to_string(),
            changes: vec![ChangeFacts {
                short: "c0ffee000001".to_string(),
                author: "agent:nick/a1".to_string(),
                justification: Some(JustificationKind::Editorial),
            }],
            blocks: vec![block("b-BBBBB")],
            ..VerdictInputs::default()
        }
    }

    fn run(rules: Vec<Rule>, i: &VerdictInputs) -> AssessedPolicy {
        let p = pref();
        let f = fields(rules);
        verdict(Some((&p, Some(&f))), i, Some("approvers".to_string()))
    }

    fn reasons(a: &AssessedPolicy) -> Vec<String> {
        match &a.verdict {
            PolicyVerdict::NeedsPerson { reasons, .. } => reasons.clone(),
            PolicyVerdict::Accepts { rule } => panic!("accepted under {rule}"),
        }
    }

    fn accepted(a: &AssessedPolicy) -> String {
        match &a.verdict {
            PolicyVerdict::Accepts { rule } => rule.clone(),
            PolicyVerdict::NeedsPerson { reasons, .. } => panic!("needs a person: {reasons:?}"),
        }
    }

    #[test]
    fn ap6_no_policy_needs_a_person() {
        let a = verdict(None, &version(), Some("approvers".to_string()));
        assert_eq!(
            a,
            AssessedPolicy {
                policy: None,
                verdict: PolicyVerdict::NeedsPerson {
                    role: None,
                    reasons: vec!["no authority policy is set".to_string()],
                },
            }
        );
    }

    #[test]
    fn ap6_a_policy_that_does_not_validate_needs_a_person() {
        let p = pref();
        let a = verdict(Some((&p, None)), &version(), Some("approvers".to_string()));
        assert_eq!(
            a,
            AssessedPolicy {
                policy: Some(pref()),
                verdict: PolicyVerdict::NeedsPerson {
                    role: None,
                    reasons: vec!["the authority policy ap-AAAAA does not validate".to_string()],
                },
            }
        );
    }

    #[test]
    fn ap6_a_policy_with_no_rule_needs_a_person() {
        let a = run(vec![], &version());
        assert_eq!(a.policy, Some(pref()));
        assert_eq!(
            a.verdict,
            PolicyVerdict::NeedsPerson {
                role: Some("approvers".to_string()),
                reasons: vec!["the policy has no agent_may_accept rule".to_string()],
            }
        );
    }

    #[test]
    fn ap6_each_never_list_item_needs_a_person() {
        // One case each: the rule would otherwise accept every one of these versions.
        let open = two_way_rule("open");
        let base = || {
            let mut v = version();
            v.decisions = vec![decision("D-DDDDD")];
            v
        };
        assert_eq!(accepted(&run(vec![open.clone()], &base())), "open");
        let cases: Vec<(Box<dyn Fn(&mut VerdictInputs)>, &str)> = vec![
            (
                Box::new(|v| v.redaction = true),
                "it carries a redaction request",
            ),
            (
                Box::new(|v| v.changes_policy = true),
                "it changes the authority policy",
            ),
            (
                Box::new(|v| v.exceptions = vec![(ExceptionKind::OneWay, "D-DDDDD".to_string())]),
                "it has the exception one_way on D-DDDDD",
            ),
            (
                Box::new(|v| v.exceptions = vec![(ExceptionKind::Against, "D-EEEEE".to_string())]),
                "it has the exception against on D-EEEEE",
            ),
            (
                Box::new(|v| {
                    v.exceptions = vec![(ExceptionKind::ReaffirmOneWay, "D-EEEEE".to_string())]
                }),
                "it has the exception reaffirm_one_way on D-EEEEE",
            ),
            (
                Box::new(|v| v.noncode_deferrals = 2),
                "it defers 2 items that are not code items",
            ),
            (
                Box::new(|v| v.decisions[0].basis = Some("direct_ruling".to_string())),
                "decision D-DDDDD has basis direct_ruling",
            ),
            (
                Box::new(|v| v.decisions[0].basis = Some("recorded_ruling".to_string())),
                "decision D-DDDDD has basis recorded_ruling",
            ),
            (
                Box::new(|v| v.stable_docs = vec![id("d-SSSSS")]),
                "it makes doc d-SSSSS stable",
            ),
        ];
        for (alter, text) in cases {
            let mut v = base();
            alter(&mut v);
            let a = run(vec![open.clone()], &v);
            assert_eq!(reasons(&a), vec![text.to_string()], "case {text}");
            assert!(matches!(
                &a.verdict,
                PolicyVerdict::NeedsPerson { role: Some(r), .. } if r == "approvers"
            ));
        }
        // Several at once: every one, in O8's order.
        let mut v = base();
        v.stable_docs = vec![id("d-TTTTT"), id("d-SSSSS")];
        v.decisions.push(WrittenDecision {
            basis: Some("recorded_ruling".to_string()),
            ..decision("D-CCCCC")
        });
        v.decisions[0].basis = Some("direct_ruling".to_string());
        v.noncode_deferrals = 1;
        v.exceptions = vec![
            (ExceptionKind::Unapplied, "D-DDDDD".to_string()),
            (ExceptionKind::BlockingTension, "t-XXXXX".to_string()),
        ];
        v.changes_policy = true;
        v.redaction = true;
        assert_eq!(
            reasons(&run(vec![open], &v)),
            vec![
                "it carries a redaction request",
                "it changes the authority policy",
                "it has the exception unapplied on D-DDDDD",
                "it has the exception blocking_tension on t-XXXXX",
                "it defers 1 items that are not code items",
                "decision D-CCCCC has basis recorded_ruling",
                "decision D-DDDDD has basis direct_ruling",
                "it makes doc d-SSSSS stable",
                "it makes doc d-TTTTT stable",
            ]
        );
    }

    #[test]
    fn ap6_a_covered_version_is_accepted_by_the_first_rule() {
        let mut narrow = rule("narrow");
        narrow.actors = vec!["agent:other".to_string()];
        let a = run(vec![narrow, rule("first"), rule("second")], &version());
        assert_eq!(a.policy, Some(pref()));
        assert_eq!(
            a.verdict,
            PolicyVerdict::Accepts {
                rule: "first".to_string()
            }
        );
    }

    #[test]
    fn ap6_an_actor_outside_the_patterns_fails_the_rule() {
        // The proposer first.
        let mut v = version();
        v.proposer = "agent:nick2/a1".to_string();
        v.changes[0].author = "agent:eve".to_string();
        assert_eq!(
            reasons(&run(vec![rule("r")], &v)),
            vec!["rule r: actor agent:nick2/a1 matches none of its actors"]
        );
        // Then each change author, in change order.
        let mut v = version();
        v.changes.push(ChangeFacts {
            short: "c0ffee000002".to_string(),
            author: "agent:eve".to_string(),
            justification: Some(JustificationKind::Editorial),
        });
        v.changes.push(ChangeFacts {
            short: "c0ffee000003".to_string(),
            author: "agent:mallory".to_string(),
            justification: Some(JustificationKind::Editorial),
        });
        let mut exact = rule("exact");
        exact.actors = vec!["agent:nick/a1".to_string()];
        assert_eq!(
            reasons(&run(vec![rule("r"), exact], &v)),
            vec![
                "rule r: actor agent:eve matches none of its actors",
                "rule exact: actor agent:eve matches none of its actors",
            ]
        );
    }

    #[test]
    fn ap6_a_block_outside_the_areas_fails_the_rule() {
        let mut r = rule("r");
        r.areas = vec!["api".to_string()];
        let mut v = version();
        v.blocks = vec![
            WrittenBlock {
                areas: vec!["web".to_string()],
                ..block("b-ZZZZZ")
            },
            WrittenBlock {
                areas: vec![],
                ..block("b-CCCCC")
            },
            block("b-AAAAA"),
        ];
        // Each written block in id order: b-CCCCC is in no area.
        assert_eq!(
            reasons(&run(vec![r.clone()], &v)),
            vec!["rule r: block b-CCCCC is outside its areas"]
        );
        v.blocks.retain(|b| b.id != id("b-CCCCC"));
        assert_eq!(
            reasons(&run(vec![r.clone()], &v)),
            vec!["rule r: block b-ZZZZZ is outside its areas"]
        );
        // A block also in an area the rule does not list is outside it.
        v.blocks = vec![WrittenBlock {
            areas: vec!["api".to_string(), "web".to_string()],
            ..block("b-AAAAA")
        }];
        assert_eq!(
            reasons(&run(vec![r.clone()], &v)),
            vec!["rule r: block b-AAAAA is outside its areas"]
        );
        // An empty `areas` means every area.
        assert_eq!(accepted(&run(vec![rule("all")], &v)), "all");
    }

    #[test]
    fn ap6_draft_fails_on_stable_content() {
        let mut r = rule("drafts");
        r.lifecycle = Lifecycle::Draft;
        let mut v = version();
        v.blocks.push(WrittenBlock {
            stable: true,
            ..block("b-SSSSS")
        });
        v.blocks.push(WrittenBlock {
            stable: true,
            ..block("b-RRRRR")
        });
        assert_eq!(
            reasons(&run(vec![r], &v)),
            vec!["rule drafts: block b-RRRRR is in stable content"]
        );
        assert_eq!(accepted(&run(vec![rule("any")], &v)), "any");
    }

    #[test]
    fn ap6_an_unjustified_change_fails_the_rule() {
        let mut v = version();
        v.changes[0].justification = None;
        assert_eq!(
            reasons(&run(vec![rule("r")], &v)),
            vec!["rule r: change c0ffee000001 has no justification"]
        );
        v.changes[0].justification = Some(JustificationKind::Reflects);
        assert_eq!(
            reasons(&run(vec![rule("r")], &v)),
            vec!["rule r: change c0ffee000001 is reflects, which it does not accept"]
        );
    }

    #[test]
    fn ap6_decisions_none_fails_on_a_tracking_edit() {
        // O7: a tracking edit of a decision records it.
        let mut v = version();
        v.decisions = vec![decision("D-TTTTT")];
        assert_eq!(
            reasons(&run(vec![rule("r")], &v)),
            vec!["rule r: it records decision D-TTTTT"]
        );
    }

    #[test]
    fn ap6_a_declared_two_way_outside_two_way_areas_needs_a_person() {
        let mut v = version();
        v.decisions = vec![WrittenDecision {
            areas: vec!["billing".to_string()],
            ..decision("D-DDDDD")
        }];
        assert_eq!(
            reasons(&run(vec![two_way_rule("r")], &v)),
            vec!["rule r: decision D-DDDDD is outside its two_way_areas"]
        );
        // A decision in no area is outside them too.
        v.decisions[0].areas = vec![];
        assert_eq!(
            reasons(&run(vec![two_way_rule("r")], &v)),
            vec!["rule r: decision D-DDDDD is outside its two_way_areas"]
        );
    }

    #[test]
    fn ap6_a_decision_with_no_reversibility_needs_a_person() {
        let mut v = version();
        v.decisions = vec![WrittenDecision {
            reversibility: None,
            ..decision("D-DDDDD")
        }];
        assert_eq!(
            reasons(&run(vec![two_way_rule("r")], &v)),
            vec!["rule r: decision D-DDDDD is not declared two_way"]
        );
        v.decisions[0].reversibility = Some("one_way".to_string());
        assert_eq!(
            reasons(&run(vec![two_way_rule("r")], &v)),
            vec!["rule r: decision D-DDDDD is not declared two_way"]
        );
    }

    #[test]
    fn ap6_a_declared_two_way_inside_two_way_areas_is_accepted() {
        let mut v = version();
        v.decisions = vec![decision("D-DDDDD")];
        assert_eq!(accepted(&run(vec![two_way_rule("r")], &v)), "r");
        // A basis the rule does not list fails; an absent basis reads `none`.
        let mut r = two_way_rule("r");
        r.basis = BTreeSet::from(["precedent".to_string()]);
        assert_eq!(
            reasons(&run(vec![r.clone()], &v)),
            vec!["rule r: decision D-DDDDD has basis none, which it does not accept"]
        );
        v.decisions[0].basis = Some("elimination".to_string());
        assert_eq!(
            reasons(&run(vec![r.clone()], &v)),
            vec!["rule r: decision D-DDDDD has basis elimination, which it does not accept"]
        );
        // An empty basis accepts no decision.
        r.basis = BTreeSet::new();
        assert_eq!(
            reasons(&run(vec![r], &v)),
            vec!["rule r: decision D-DDDDD has basis elimination, which it does not accept"]
        );
    }

    #[test]
    fn ap6_delegated_needs_attested_by_to_name_the_policy_block() {
        let mut v = version();
        v.decisions = vec![WrittenDecision {
            basis: Some("delegated".to_string()),
            attested_by: Some("agent:nick/a1 under human:nick".to_string()),
            ..decision("D-DDDDD")
        }];
        let fail = vec![
            "rule r: decision D-DDDDD is delegated, and its attested_by does not name ap-AAAAA",
        ];
        assert_eq!(reasons(&run(vec![two_way_rule("r")], &v)), fail);
        v.decisions[0].attested_by = None;
        assert_eq!(reasons(&run(vec![two_way_rule("r")], &v)), fail);
        // A longer id that starts with the policy's id does not name it.
        v.decisions[0].attested_by = Some("agent:nick/a1 under ap-AAAAAB".to_string());
        assert_eq!(reasons(&run(vec![two_way_rule("r")], &v)), fail);
        v.decisions[0].attested_by = Some("agent:nick/a1 under ap-AAAAA.".to_string());
        assert_eq!(accepted(&run(vec![two_way_rule("r")], &v)), "r");
        v.decisions[0].attested_by = Some("ap-AAAAA".to_string());
        assert_eq!(accepted(&run(vec![two_way_rule("r")], &v)), "r");
    }

    #[test]
    fn ap6_max_blocks_and_max_items_bound_the_version() {
        let mut r = rule("r");
        r.max_blocks = 3;
        r.max_items = 2;
        let mut v = version();
        v.blocks = vec![block("b-AAAAA"), block("b-BBBBB"), block("b-CCCCC")];
        v.plan_items = 2;
        assert_eq!(accepted(&run(vec![r.clone()], &v)), "r");
        v.blocks.push(block("b-DDDDD"));
        assert_eq!(
            reasons(&run(vec![r.clone()], &v)),
            vec!["rule r: it changes 4 blocks; the rule allows 3"]
        );
        v.blocks.pop();
        v.plan_items = 3;
        assert_eq!(
            reasons(&run(vec![r.clone()], &v)),
            vec!["rule r: its change plan holds 3 items; the rule allows 2"]
        );
        // max_items 0 allows no plan item.
        r.max_items = 0;
        v.plan_items = 0;
        assert_eq!(accepted(&run(vec![r], &v)), "r");
    }

    #[test]
    fn ap6_reasons_are_cut_at_100() {
        let mut v = version();
        v.exceptions = (0..150)
            .map(|n| (ExceptionKind::NotRecommended, format!("D-{n:05}")))
            .collect();
        let a = run(vec![rule("r")], &v);
        let got = reasons(&a);
        assert_eq!(got.len(), 100);
        assert_eq!(got[0], "it has the exception not_recommended on D-00000");
        assert_eq!(got[99], "it has the exception not_recommended on D-00099");
        // A long reason is cut as a card fact is.
        let mut v = version();
        v.exceptions = vec![(ExceptionKind::Against, "x".repeat(5000))];
        let got = reasons(&run(vec![rule("r")], &v));
        assert_eq!(got.len(), 1);
        assert!(got[0].len() <= MAX_POLICY_REASON_BYTES);
        assert!(got[0].starts_with("it has the exception against on xxx"));
        assert!(got[0].ends_with("..."));
        assert!(a.validate().is_ok());
    }

    #[test]
    fn ap6_fact_value_follows_o9() {
        let p = pref();
        let short = p.version.short();
        assert_eq!(short.len(), 12);
        // Accepts.
        let a = run(vec![rule("drafts")], &version());
        assert_eq!(
            fact_value(&a),
            format!("ap-AAAAA@{short}: accepts under rule drafts")
        );
        // Needs a person from a role.
        let mut v = version();
        v.redaction = true;
        v.noncode_deferrals = 3;
        assert_eq!(
            fact_value(&run(vec![rule("drafts")], &v)),
            format!(
                "ap-AAAAA@{short}: needs a person from approvers: it carries a redaction request; \
                 it defers 3 items that are not code items"
            )
        );
        // Needs a person, no role applies.
        let f = fields(vec![rule("drafts")]);
        let a = verdict(Some((&p, Some(&f))), &v, None);
        assert_eq!(
            fact_value(&a),
            format!(
                "ap-AAAAA@{short}: needs a person: it carries a redaction request; \
                 it defers 3 items that are not code items"
            )
        );
        // No policy.
        assert_eq!(
            fact_value(&verdict(None, &v, None)),
            "needs a person: no authority policy is set"
        );
        // A policy that does not validate.
        assert_eq!(
            fact_value(&verdict(
                Some((&p, None)),
                &v,
                Some("approvers".to_string())
            )),
            format!(
                "ap-AAAAA@{short}: needs a person: the authority policy ap-AAAAA does not validate"
            )
        );
    }

    #[test]
    fn ap6_the_verdict_passes_assessed_policy_validate() {
        let p = pref();
        let f = fields(vec![rule("r"), two_way_rule("t")]);
        let mut many = version();
        many.exceptions = (0..500)
            .map(|n| (ExceptionKind::Against, format!("{n}{}", "y".repeat(5000))))
            .collect();
        let mut failing = version();
        failing.proposer = "human:nick".to_string();
        let verdicts = [
            verdict(None, &version(), None),
            verdict(None, &version(), Some("approvers".to_string())),
            verdict(Some((&p, None)), &version(), None),
            verdict(Some((&p, Some(&fields(vec![])))), &version(), None),
            verdict(Some((&p, Some(&f))), &version(), None),
            verdict(
                Some((&p, Some(&f))),
                &failing,
                Some("approvers".to_string()),
            ),
            verdict(Some((&p, Some(&f))), &many, Some("approvers".to_string())),
        ];
        for a in &verdicts {
            assert_eq!(a.validate(), Ok(()), "{a:?}");
        }
    }

    #[test]
    fn ap6_an_owner_role_label_fits_the_stored_bound() {
        let path = format!(
            "docs/{}",
            "p".repeat(rdy_schema::policy::MAX_PATH_BYTES - 5)
        );
        assert_eq!(path.len(), rdy_schema::policy::MAX_PATH_BYTES);
        let label = format!("owners of {path}");
        assert!(label.len() <= MAX_POLICY_NAME_BYTES);
        let p = pref();
        let f = fields(vec![rule("r")]);
        let mut v = version();
        v.changes_policy = true;
        let a = verdict(Some((&p, Some(&f))), &v, Some(label.clone()));
        assert_eq!(
            a.verdict,
            PolicyVerdict::NeedsPerson {
                role: Some(label.clone()),
                reasons: vec!["it changes the authority policy".to_string()],
            }
        );
        assert_eq!(a.validate(), Ok(()));
        assert!(fact_value(&a).contains(&format!("needs a person from {label}: ")));
    }
}
