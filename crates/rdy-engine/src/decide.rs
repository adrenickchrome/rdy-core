use crate::error::*;
use crate::write::{LinkInput, OpInput, WriteRequest, WriteResult};
use crate::{Rdy, Scope, Session, rfc3339_now};
use rdy_model::{BlockId, decision};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct DecisionInput {
    pub parent: String,
    pub statement: String,
    /// The card's plain-words title (FW-I1).
    #[serde(default)]
    pub title: Option<String>,
    /// The card's one line on why a person must answer it.
    #[serde(default)]
    pub why: Option<String>,
    /// What the author recommends the person does with the decision: `confirm` or `reject`.
    #[serde(default)]
    pub recommend: Option<String>,
    /// The recommendation's reason, in one line.
    #[serde(default)]
    pub recommend_reason: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub authority_basis: Option<String>,
    #[serde(default)]
    pub attested_by: Option<String>,
    #[serde(default)]
    pub reversibility: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub review_by: Option<String>,
    /// When the decision takes effect (RFC 3339); recorded_at when absent.
    #[serde(default)]
    pub effective_from: Option<String>,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub affects: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub conditional_on: Vec<String>,
    #[serde(default)]
    pub refines: Vec<String>,
    #[serde(default)]
    pub answers: Vec<String>,
    /// sup1: link kinds rdy_supersede must not copy from the old decision.
    #[serde(default)]
    pub no_inherit: Vec<String>,
    /// bwr1: the briefing this decision's write cites (needed under a stable parent).
    #[serde(default)]
    pub brief: Option<String>,
    /// Pack-defined fields (e.g. key, or a pack's own decision fields), validated against the pack by the write.
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    /// iwr1: the intent this decision's write is made under (recorded on its change).
    #[serde(default)]
    pub intent: Option<String>,
}

/// The decision fields that rdy_decide sets itself (from its own parameters, or rdy); `fields` may not carry them.
pub const DECISION_OWN_FIELDS: [&str; 15] = [
    "statement",
    "title",
    "why",
    "recommend",
    "recommend_reason",
    "status",
    "options",
    "authority_basis",
    "attested_by",
    "reversibility",
    "kind",
    "review_by",
    "effective_from",
    "decided_at",
    "recorded_at",
];

fn links_of(d: &DecisionInput) -> BTreeMap<String, Vec<LinkInput>> {
    let mut links = BTreeMap::new();
    for (name, ids) in [
        (decision::AFFECTS, &d.affects),
        (decision::DEPENDS_ON, &d.depends_on),
        (decision::CONDITIONAL_ON, &d.conditional_on),
        (decision::REFINES, &d.refines),
        (decision::ANSWERS, &d.answers),
    ] {
        if !ids.is_empty() {
            links.insert(
                name.to_string(),
                ids.iter()
                    .map(|t| LinkInput {
                        target: t.clone(),
                        part: None,
                    })
                    .collect(),
            );
        }
    }
    links
}

fn fields_of(d: &DecisionInput) -> BTreeMap<String, Value> {
    let mut f = BTreeMap::new();
    f.insert("statement".into(), json!(d.statement));
    f.insert("status".into(), json!("proposed"));
    f.insert("recorded_at".into(), json!(rfc3339_now()));
    if !d.options.is_empty() {
        f.insert("options".into(), json!(d.options));
    }
    for (k, v) in [
        ("authority_basis", &d.authority_basis),
        ("attested_by", &d.attested_by),
        ("reversibility", &d.reversibility),
        ("kind", &d.kind),
        ("review_by", &d.review_by),
        ("title", &d.title),
        ("why", &d.why),
        ("recommend", &d.recommend),
        ("recommend_reason", &d.recommend_reason),
        (decision::EFFECTIVE_FROM, &d.effective_from),
    ] {
        if let Some(v) = v {
            f.insert(k.into(), json!(v));
        }
    }
    for (k, v) in &d.fields {
        f.insert(k.clone(), v.clone());
    }
    f
}

impl Rdy {
    /// The parent a decision is recorded under (FW-PR2): `given` as it is, else the doc that
    /// `decisions.home` names in `view`. Fails with `E_BAD_INPUT` when there is neither, and with
    /// `E_CONFIG` when the home names no block or a block that is not a doc.
    pub fn decision_parent(&self, view: &str, given: Option<&str>) -> Result<String, RdyError> {
        if let Some(p) = given {
            return Ok(p.to_string());
        }
        let st = self.read();
        let Some(home) = st.decisions_home() else {
            return Err(RdyError::new(
                E_BAD_INPUT,
                "no parent given, and the project names no decision home",
                "pass parent (the doc or section to record the decision in), or set decisions.home in .rdy.yaml to a doc's path, key or id",
            ));
        };
        let scope = Scope::View(view.to_string());
        let named = st
            .resolve_id(&scope, home)
            .and_then(|id| st.block(&scope, &id));
        let block = named.map_err(|e| {
            RdyError::new(
                E_CONFIG,
                format!(
                    "decisions.home {home:?} names no block in view {view}: {}",
                    e.message
                ),
                "set decisions.home in .rdy.yaml to an existing doc's path, key or id",
            )
        })?;
        if block.block_type != "doc" {
            return Err(RdyError::new(
                E_CONFIG,
                format!(
                    "decisions.home {home:?} is a {}, not a doc",
                    block.block_type
                ),
                "name a doc: a decision cannot sit directly in a folder yet",
            ));
        }
        Ok(block.id.to_string())
    }

    fn write_decision(
        &mut self,
        s: &Session,
        view: &str,
        d: DecisionInput,
        extra: Vec<(String, LinkInput)>,
        message: String,
    ) -> Result<WriteResult, RdyError> {
        if let Some(name) = d
            .fields
            .keys()
            .find(|k| DECISION_OWN_FIELDS.contains(&k.as_str()))
        {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("fields.{name} is set by rdy_decide, not through fields"),
                "pass statement, title, why, recommend, recommend_reason, options, authority_basis, attested_by, reversibility, kind, review_by and effective_from as their own parameters; rdy sets status, decided_at and recorded_at",
            ));
        }
        let brief = d.brief.clone();
        let intent = d.intent.clone();
        let mut links = links_of(&d);
        for (name, l) in extra {
            links.entry(name).or_default().push(l);
        }
        let op = OpInput::Create {
            block_type: decision::DECISION.into(),
            id: Some("$decision".into()),
            parent: Some(d.parent.clone()),
            after: None,
            before: None,
            fields: fields_of(&d),
            body: d.body.clone(),
            links,
        };
        self.write_with_intent(
            s,
            WriteRequest {
                view: view.into(),
                ops: vec![op],
                justification: None,
                message,
            },
            brief,
            intent.as_deref(),
        )
    }

    pub fn decide(
        &mut self,
        s: &Session,
        view: &str,
        d: DecisionInput,
    ) -> Result<WriteResult, RdyError> {
        let msg = format!("decide: {}", d.statement);
        self.write_decision(s, view, d, vec![], msg)
    }

    fn require_decision(&mut self, view: &str, id: &str) -> Result<BlockId, RdyError> {
        let bid = BlockId::parse(id).map_err(|e| {
            RdyError::new(
                E_BAD_INPUT,
                e.to_string(),
                "pass a decision id like D-W2HXR",
            )
        })?;
        let b = self.read().block(&Scope::View(view.into()), &bid)?;
        if b.block_type != decision::DECISION {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("{id} is a {}, not a decision", b.block_type),
                "only decisions can be superseded or revoked",
            ));
        }
        Ok(bid)
    }

    pub fn supersede(
        &mut self,
        s: &Session,
        view: &str,
        old: &str,
        part: Option<String>,
        d: DecisionInput,
    ) -> Result<WriteResult, RdyError> {
        let old_id = self.require_decision(view, old)?;
        let old_block = self.read().block(&Scope::View(view.into()), &old_id)?;
        // sup1: the new decision keeps the old one's scope and premises, kind by kind, unless the
        // caller gives that kind or lists it in no_inherit
        const INHERITED: [&str; 4] = [
            decision::AFFECTS,
            decision::DEPENDS_ON,
            decision::CONDITIONAL_ON,
            decision::REFINES,
        ];
        if let Some(k) = d
            .no_inherit
            .iter()
            .find(|k| !INHERITED.contains(&k.as_str()))
        {
            return Err(RdyError::new(
                E_BAD_INPUT,
                format!("no_inherit names {k:?}, which is not a link kind rdy_supersede inherits"),
                format!("no_inherit takes {}", INHERITED.join(", ")),
            ));
        }
        let scope = Scope::View(view.into());
        let mut inherited = BTreeMap::new();
        let mut extra = vec![];
        for kind in INHERITED {
            let given = match kind {
                decision::AFFECTS => &d.affects,
                decision::DEPENDS_ON => &d.depends_on,
                decision::CONDITIONAL_ON => &d.conditional_on,
                _ => &d.refines,
            };
            if !given.is_empty() || d.no_inherit.iter().any(|k| k == kind) {
                continue;
            }
            let links = old_block.links.get(kind).into_iter().flatten();
            let mut targets = vec![];
            for l in links {
                // a link to a block deleted in this view can't be copied: say so before writing
                if self.read().materialized(&scope)?.get(&l.target).is_none() {
                    return Err(RdyError::new(
                        E_UNKNOWN_REF,
                        format!(
                            "{old_id}'s {kind} link to {} no longer exists, so it can't be inherited",
                            l.target
                        ),
                        format!(
                            "pass {kind} yourself or drop it (MCP: \"{kind}\": []; engine: no_inherit)"
                        ),
                    ));
                }
                targets.push(l.target.to_string());
                extra.push((
                    kind.to_string(),
                    LinkInput {
                        target: l.target.to_string(),
                        part: l.part.clone(),
                    },
                ));
            }
            if !targets.is_empty() {
                inherited.insert(kind.to_string(), targets);
            }
        }
        let msg = format!("supersede {old_id}: {}", d.statement);
        let mut res = self.write_decision(
            s,
            view,
            d,
            {
                extra.push((
                    decision::SUPERSEDES.into(),
                    LinkInput {
                        target: old_id.to_string(),
                        part,
                    },
                ));
                extra
            },
            msg,
        )?;
        res.inherited = inherited;
        Ok(res)
    }

    pub fn revoke(
        &mut self,
        s: &Session,
        view: &str,
        target: &str,
        reason: &str,
        parent: &str,
        brief: Option<String>,
    ) -> Result<WriteResult, RdyError> {
        self.revoke_with_intent(s, view, target, reason, parent, brief, None)
    }

    /// [`Rdy::revoke`] under `intent` (mint1): recorded on the revoking change.
    #[allow(clippy::too_many_arguments)]
    pub fn revoke_with_intent(
        &mut self,
        s: &Session,
        view: &str,
        target: &str,
        reason: &str,
        parent: &str,
        brief: Option<String>,
        intent: Option<String>,
    ) -> Result<WriteResult, RdyError> {
        let tid = self.require_decision(view, target)?;
        let d = DecisionInput {
            parent: parent.into(),
            statement: format!("Revoke {tid}: {reason}"),
            brief,
            intent,
            ..Default::default()
        };
        self.write_decision(
            s,
            view,
            d,
            vec![(
                decision::REVOKES.into(),
                LinkInput {
                    target: tid.to_string(),
                    part: None,
                },
            )],
            format!("revoke {tid}"),
        )
    }
}
