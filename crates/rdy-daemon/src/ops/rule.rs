//! The rule op (FW-MH.rules, D-ZSRBN): one `rdy_rule` request, run in this project. Its arguments are
//! the request and `relayed_by`. `part: true` is the form a host's writer of the person layer sends
//! ([`crate::instructions`]): this store is the destination, and the session is the relay. This op
//! never goes on to another project, because it holds its caller's signer only by reference: a
//! person rule for another project comes from a host that holds one, or from a client that connects
//! to that project's daemon. The destination's configured human is `cx.human`.
use super::*;
use rdy_engine::answer::relay::RelayRefusal;
use rdy_engine::instructions::no_rule_host;
use rdy_engine::rule_types::{CtlRule, parse_ctl};
use rdy_engine::rules::approval::run_rule;
use rdy_engine::rules::{Destination, RuleOrigin, RuleRoute, rule_route};

pub(super) fn rule(cx: &OpContext, args: &Value) -> Result<Value, RdyError> {
    let CtlRule {
        request,
        relayed_by,
        part,
    } = parse_ctl(args)?;
    if !part && relayed_by.is_none() && !cx.session.is_human() {
        return Err(RelayRefusal::NoRelayedBy.error());
    }
    let destination = if part {
        Destination::Meta
    } else {
        let host = cx.engine.person_host();
        let root = cx.engine.root();
        match rule_route(Some(&root), host.as_deref(), request.scope())? {
            RuleRoute::Here(d) => d,
            RuleRoute::Meta(_) => return Err(no_rule_host()),
        }
    };
    let origin = RuleOrigin {
        session: cx.session,
        relayed_by: relayed_by.as_deref(),
    };
    let call = origin.call(cx.human.to_string(), destination);
    run_rule(cx.engine, cx.agent, &call, &request).map(|r| r.to_json())
}
