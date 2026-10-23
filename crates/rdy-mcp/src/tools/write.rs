//! rdy_write: ops that create, update, move and delete blocks.

use super::*;
use rdy_engine::consequence::write_and_report;
use rdy_engine::*;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditorialParams {
    /// typo, restructure or clarify.
    pub class: String,
    pub reason: String,
}

/// rf8 (D-4WC6R, D-XC9B9): a write's `reflects` justification.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReflectsParams {
    /// The sources (S-…) the change follows: an id, key or path each, or the $name of a source
    /// created in this write. Missing: none.
    pub sources: Option<Vec<String>>,
    /// true records the code state of each written block with reflects globs. Missing: false;
    /// false is refused.
    pub code: Option<bool>,
}

/// llst1: one text, or a list of them.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteParams {
    pub view: Option<String>,
    /// Ops, each a JSON object (rdy_schema lists them under "ops"):
    /// {"op":"create","type":<type>,"id":"$name"?,"parent":<id or path>?,"after":<sibling>?,"before":<sibling>?,"fields":{..}?,"body":".."?,"links":{..}?}
    /// {"op":"update","id":<id or path>,"base":<version>?,"fields":{..}?,"unset":[<field>]?,"body":".."?,"links":{..}?,"type":<new type>?,"move_to":{"parent":<id or path>?,"after":<sibling>?,"before":<sibling>?}?} type changes a content block's type in place (same id); move_to moves it in the same op.
    /// {"op":"move","id":<id or path>,"base":<version>?,"parent":<id or path>?,"after":<sibling>?,"before":<sibling>?}
    /// {"op":"delete","id":<id or path>,"base":<version>?}
    /// {"op":"rebalance","parent":<id or path>?}
    /// {"op":"repin","id":<id or path>,"base":<version>?,"targets":[<id or path>]?,"code":<true>?} code: true also records the code state the block's reflects globs match now, and clears its code suspect; the block must declare reflects.
    /// {"op":"reaffirm","id":<decision id>?,"because":<id or path>?,"base":<version>?,"targets":[<id or path>]?,"reason":<1 to 2000 characters>} Give id or because, not both: id re-affirms one suspect accepted decision, and because re-affirms every decision in the view that is suspect on that block. reason is required. targets limits the op to the suspects on those blocks (default: every current suspect). base goes with id only: it is the version of the decision you read, and the op is refused if the decision has moved. The decision keeps its ruling, body and links.
    /// links: {"<kind>": <target> | [<target>, …] | [{"target": "<id, key or path>", "part": "<optional words>"}]}, e.g. {"decided_by": [{"target": "D-W2HXR"}]}. An update replaces only the link kinds it names; "<kind>": [] removes that kind.
    /// after puts a block behind a sibling and before ahead of one (before the first child makes it first); give one, or neither to append.
    /// repin re-pins a block's references and links (all, or the targets named) to their current versions, after you've checked them: it clears "changed" suspects.
    /// Ids, parents, afters, befores and link targets also take paths (wallets/transactions); a move without parent goes to the top level (docs and folders); rebalance re-keys a parent's children (frozen decisions stay put; in one write it must come before any other op on those children, and a child it re-keys can't be edited again in the same write).
    pub ops: Vec<Value>,
    /// The decisions this change carries out: an id, key or path, or a list of them.
    pub implements: Option<OneOrMany>,
    pub editorial: Option<EditorialParams>,
    /// Pass one of implements, editorial or reflects. reflects is {sources?: [source id, key or path], code?: true}. A source created in this write counts if the final state holds it. code: true records the code state of each written block with reflects globs.
    pub reflects: Option<ReflectsParams>,
    pub message: Option<String>,
    /// The briefing token from rdy_brief. Required when the write touches stable content.
    pub brief: Option<String>,
    /// The budget for the response's consequences.
    pub max_tokens: Option<usize>,
    /// The intent (rdy_intent, in-<hex>) this change is made under: recorded on the change.
    pub intent: Option<String>,
}

/// Whether `v` has the shape of `LINK_SHAPE`: each kind is a string, or a list of strings and
/// `{"target": <string>, ...}` objects.
fn links_shape_ok(v: &Value) -> bool {
    let entry = |x: &Value| x.is_string() || x.get("target").is_some_and(Value::is_string);
    v.as_object().is_some_and(|kinds| {
        kinds
            .values()
            .all(|l| l.is_string() || l.as_array().is_some_and(|items| items.iter().all(entry)))
    })
}

pub fn rdy_write(st: &mut McpState, p: WriteParams) -> Result<Value, RdyError> {
    check_tokens(p.max_tokens)?;
    let s = session(st)?;
    if p.ops.len() > MAX_OPS_PER_WRITE {
        return Err(limit(
            format!(
                "{} ops in one write; the limit is {MAX_OPS_PER_WRITE}",
                p.ops.len()
            ),
            "split the write into batches",
        ));
    }
    let ops: Vec<OpInput> = p
        .ops
        .into_iter()
        .enumerate()
        .map(|(i, v)| {
            let bad_links = v.get("links").is_some_and(|l| !links_shape_ok(l));
            parse_op(i, v).map_err(|mut e| {
                // an unknown field keeps its did-you-mean (FW-HD1); otherwise doc1: a malformed
                // links value gets the shape, not the generic hint (a link object with no
                // target string, whatever key it holds, included)
                let link_fault = e.message.ends_with(LINK_KEY_TAIL);
                if e.fix == OP_FIX || (link_fault && bad_links) {
                    e.fix = if bad_links {
                        format!("write {LINK_SHAPE}, e.g. {LINK_EXAMPLE}")
                    } else {
                        format!("{OP_FIX}; see rdy_schema for fields")
                    };
                }
                e
            })
        })
        .collect::<Result<_, _>>()?;
    let scope = Scope::View(p.view.clone().unwrap_or_else(|| st.view.clone()));
    let parsed = parse_justification(
        &st.engine.read(),
        &scope,
        p.implements.map(|d| match d {
            OneOrMany::One(t) => vec![t],
            OneOrMany::Many(v) => v,
        }),
        p.editorial.map(|e| (e.class, e.reason)),
        p.reflects.map(|r| ReflectsArg {
            sources: r.sources,
            code: r.code,
        }),
    )?;
    let view = p.view.unwrap_or_else(|| st.view.clone());
    let req = WriteRequest {
        view,
        ops,
        justification: parsed.stored,
        message: p.message.unwrap_or_default(),
    };
    let (brief, max_tokens, view, intent) = (p.brief, p.max_tokens, req.view.clone(), p.intent);
    let reflects = parsed.reflects;
    write_and_report(&*st.engine, &view, max_tokens, move |r| {
        r.write_with_intent(&s, req, brief, intent.as_deref(), reflects)
    })
    .map(|v| report_once(st, v))
}
