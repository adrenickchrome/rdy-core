//! The read surfaces of signing (D-1Z951, D-QVH4N, D-KQ4CA): key show, the verified log and the
//! signature states of a verbose fsck, with the text lines every surface prints.
//!
//! Nothing here writes, contacts a hub or reads a private key. The verified log and the fsck
//! states never ask an SSH agent: each row's checks are the one offline check
//! ([`crate::signature_check::signature_checks`]), with the fork lineage read once per report.
//! Only [`key_show`] asks the caller's agent, and only for the identities it holds, when a key
//! is configured.

use crate::allowed_signers::{AllowedSigners, read_allowed_signers};
use crate::config_levels::{SIGNING_AGENT_KEY, SIGNING_KEY, effective, user_file_from_env};
use crate::pending::reaffirm::landing_of_version;
use crate::read_state::ReadState;
use crate::signature_check::{
    Lineage, PRESENCE_ABSENT, SIGNED_INVALID, SIGNED_VALID, STATEMENT_FOREIGN_REPO,
    STATEMENT_MISMATCH, SignatureCheck,
};
use crate::signing_config::{SignMode, SigningConfig};
use crate::signing_crypto::{SigningAgent, blob_fingerprint, blob_type, key_fingerprint};
use crate::{E_INTERNAL, E_SIG_INVALID, RdyError, signing_selector};
use rdy_model::{Assurance, Hash};
use rdy_store::StoreError;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// The signing settings of the project at `root`, for the person of this process (the user
/// config named by the environment, then `.rdy/local.yaml`). Reads no key and asks no agent.
pub fn signing_config(root: &Path) -> Result<SigningConfig, RdyError> {
    let eff = effective(root, user_file_from_env().as_deref())?;
    SigningConfig::from_effective(root, &eff)
}

/// The configured trust file of the project at `root`, parsed once: `None` when none is
/// configured. A configured file that cannot be read or parsed is an error; no key becomes
/// trusted through a partial parse.
pub fn configured_trust(root: &Path) -> Result<Option<AllowedSigners>, RdyError> {
    let config = signing_config(root)?;
    read_allowed_signers(config.allowed_signers.as_deref())
}

/// The word of `mode`, as `signing.sign` spells it.
pub fn sign_mode_word(mode: SignMode) -> &'static str {
    match mode {
        SignMode::Never => "never",
        SignMode::Auto => "auto",
        SignMode::Always => "always",
    }
}

/// A configured key selector, read by the pure parser: its fingerprint and, for a public-key
/// line, the line's key type.
struct Selected {
    fingerprint: String,
    key_type: Option<String>,
}

/// The selector of `setting`: `None` only when it is unset (null); a set one that is malformed,
/// an empty or blank one included, is the shared signing `E_BAD_INPUT`.
fn selected(setting: &str, selector: Option<&str>) -> Result<Option<Selected>, RdyError> {
    let Some(text) = selector.map(str::trim) else {
        return Ok(None);
    };
    let fingerprint = key_fingerprint(text).map_err(|_| signing_selector(setting))?;
    // the parser accepted a public-key line only when its first field is the blob's own type
    let key_type = (!text.starts_with("SHA256:"))
        .then(|| text.split_ascii_whitespace().next().map(str::to_string))
        .flatten();
    Ok(Some(Selected {
        fingerprint,
        key_type,
    }))
}

/// `rdy key show` / `rdy_key_show`: exactly `{key, agent_key, sign}`. Each configured key is
/// `{fingerprint, type, held}`; an unset key, or an agent key the settings ignore, is `null`.
/// `type` is the key's public-key algorithm: a public-key line's own type, else the type of the
/// identity the agent returned for the fingerprint, else `null`. `held` is true only when the
/// agent returned the key.
///
/// Both selectors are parsed before any agent request; with neither set, no agent is asked.
/// An agent that cannot be reached is `E_NO_AGENT`, never a claim that it lacks the key.
pub fn key_show(config: &SigningConfig, agent: &dyn SigningAgent) -> Result<Value, RdyError> {
    let person = selected(SIGNING_KEY, config.key.as_deref())?;
    let machine = selected(SIGNING_AGENT_KEY, config.agent_key.as_deref())?;
    let identities = if person.is_some() || machine.is_some() {
        agent.identities()?
    } else {
        vec![]
    };
    let row = |s: Option<Selected>| -> Value {
        let Some(s) = s else {
            return Value::Null;
        };
        let held = identities
            .iter()
            .find(|id| blob_fingerprint(&id.public_key) == s.fingerprint);
        let key_type = s
            .key_type
            .or_else(|| held.and_then(|id| blob_type(&id.public_key)));
        json!({"fingerprint": s.fingerprint, "type": key_type, "held": held.is_some()})
    };
    Ok(json!({
        "key": row(person),
        "agent_key": row(machine),
        "sign": sign_mode_word(config.sign),
    }))
}

/// The text of a key show reply: three lines, person, agent, then the mode.
pub fn key_show_text(v: &Value) -> String {
    let line = |who: &str, k: &Value| -> String {
        if !k.is_object() {
            return format!("{who}: not set");
        }
        let held = if k["held"] == Value::Bool(true) {
            "yes"
        } else {
            "no"
        };
        format!(
            "{who}: {} ({}), held {held}",
            k["fingerprint"].as_str().unwrap_or("unknown"),
            k["type"].as_str().unwrap_or("unknown")
        )
    };
    format!(
        "{}\n{}\nsign: {}",
        line("person", &v["key"]),
        line("agent", &v["agent_key"]),
        v["sign"].as_str().unwrap_or("unknown")
    )
}

/// The hash of the tag `name`, from its `tag/` ref.
fn tag_hash(state: &ReadState, name: &str) -> Result<Option<Hash>, RdyError> {
    let Some(value) = state.reader().get_ref(&format!("tag/{name}"))? else {
        return Ok(None);
    };
    let bytes = <[u8; 32]>::try_from(value.as_slice()).map_err(|_| {
        RdyError::from(StoreError::Decode(format!(
            "tag ref of {name} is not a hash"
        )))
    })?;
    Ok(Some(Hash(bytes)))
}

fn checks_json(checks: Vec<SignatureCheck>) -> Result<Value, RdyError> {
    serde_json::to_value(checks).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("a signature report did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })
}

/// The signatures of the landing `landing` and of each of its tags `tags`, with `trust`:
/// `(signatures, tag_signatures)`. Tag keys sort by name; each list is in signature-hash order;
/// an empty list is an unsigned subject. `lineage` is the report's, read once for all its rows.
fn subject_reports(
    lineage: &Lineage,
    landing: &str,
    tags: &[String],
    trust: Option<&AllowedSigners>,
) -> Result<(Value, Value), RdyError> {
    let state = lineage.state();
    let hash: Hash = landing.parse().map_err(|_| {
        RdyError::from(StoreError::Decode(format!(
            "log row landing {landing:?} is not a hash"
        )))
    })?;
    let signatures = checks_json(lineage.checks(&hash, trust)?)?;
    let mut by_tag: BTreeMap<&str, Value> = BTreeMap::new();
    for name in tags {
        let checks = match tag_hash(state, name)? {
            Some(h) => lineage.checks(&h, trust)?,
            None => vec![],
        };
        by_tag.insert(name, checks_json(checks)?);
    }
    let tag_signatures: Map<String, Value> = by_tag
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    Ok((signatures, Value::Object(tag_signatures)))
}

/// The tag names of a serialized log row.
fn row_tags(row: &Value) -> Vec<String> {
    row["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// The `policy_check.signed` state of a landing that holds no signature check (O24).
const POLICY_SIGNED_NO: &str = "no";

/// The members a verified row or fsck state of a policy landing adds (D-DKAMT, O24): `signed:
/// "policy"`, `policy: {block, version}` (the policy version that accepted the landing) and
/// `policy_check: {landing, signed}`. `landing` is the landing that carried the policy version
/// ([`landing_of_version`]: one index read), `null` when none did; `signed` is its checks' state.
/// `signed` is `valid` when one of that landing's checks is valid, `invalid` when it has checks
/// and none is, `no` when it has none. A row whose assurance is not `policy`, or whose record
/// names no policy (fsck refuses it), adds nothing. The checks are the report's own, on its lineage.
fn policy_members(
    lineage: &Lineage,
    row: &Value,
    trust: Option<&AllowedSigners>,
) -> Result<Option<[(&'static str, Value); 3]>, RdyError> {
    if row["assurance"].as_str() != Some(Assurance::Policy.as_str()) {
        return Ok(None);
    }
    let state = lineage.state();
    let landing: Hash = row["landing"]
        .as_str()
        .unwrap_or_default()
        .parse()
        .map_err(|_| {
            RdyError::from(StoreError::Decode(
                "log row landing is not a hash".to_string(),
            ))
        })?;
    let Some(policy) = state.reader().landing(&landing)?.policy else {
        return Ok(None);
    };
    let holder = landing_of_version(state, &policy.version)?;
    let signed = match holder {
        None => POLICY_SIGNED_NO,
        Some(h) => {
            let checks = lineage.checks(&h, trust)?;
            if checks.iter().any(|c| c.signed == SIGNED_VALID) {
                SIGNED_VALID
            } else if checks.is_empty() {
                POLICY_SIGNED_NO
            } else {
                SIGNED_INVALID
            }
        }
    };
    Ok(Some([
        ("signed", json!("policy")),
        (
            "policy",
            json!({"block": policy.block.to_string(), "version": policy.version.to_string()}),
        ),
        (
            "policy_check",
            json!({"landing": holder.map(|h| h.to_string()), "signed": signed}),
        ),
    ]))
}

/// `rdy log --verify` / `rdy_log {verify: true}`: the log page `log_page` gives, in its order
/// and with its fields, each row with `signatures` (the landing's checks) and `tag_signatures`
/// (`{<tag name>: [check]}` for every tag the row lists, unsigned ones included). `trust` is the
/// parsed allowed-signers data of the call, used by every check. No agent and no hub is asked.
/// An invalid signature is data here: the read succeeds. A row whose assurance is `policy` adds
/// `signed: "policy"`, `policy` (`{block, version}`) and `policy_check` (`{landing, signed}`):
/// see [`policy_members`].
pub fn verified_log(
    state: &ReadState,
    since: Option<&str>,
    limit: Option<usize>,
    trust: Option<&AllowedSigners>,
) -> Result<Value, RdyError> {
    let rows = serde_json::to_value(state.log_page(since, limit)?).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("the log did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })?;
    let Value::Array(mut rows) = rows else {
        return Ok(rows);
    };
    let lineage = Lineage::new(state);
    for row in &mut rows {
        let landing = row["landing"].as_str().unwrap_or_default().to_string();
        let (signatures, tag_signatures) =
            subject_reports(&lineage, &landing, &row_tags(row), trust)?;
        let policy = policy_members(&lineage, row, trust)?;
        row["signatures"] = signatures;
        row["tag_signatures"] = tag_signatures;
        for (key, value) in policy.into_iter().flatten() {
            row[key] = value;
        }
    }
    Ok(Value::Array(rows))
}

/// The `signature_states` of a verbose fsck: every current canon landing in log order, each
/// `{landing, signatures, tag_signatures}`, unsigned ones included. Checked as fsck checks, with
/// no trust file. A policy landing's state adds the same members as its verified log row.
pub fn signature_states(state: &ReadState) -> Result<Value, RdyError> {
    let rows = serde_json::to_value(state.log_rows()?).map_err(|e| {
        RdyError::new(
            E_INTERNAL,
            format!("the log did not serialize: {e}"),
            "report this as a bug in rdy",
        )
    })?;
    let mut out = vec![];
    let lineage = Lineage::new(state);
    for row in rows.as_array().into_iter().flatten() {
        let landing = row["landing"].as_str().unwrap_or_default().to_string();
        let (signatures, tag_signatures) =
            subject_reports(&lineage, &landing, &row_tags(row), None)?;
        let mut state = json!({
            "landing": landing,
            "signatures": signatures,
            "tag_signatures": tag_signatures,
        });
        for (key, value) in policy_members(&lineage, row, None)?.into_iter().flatten() {
            state[key] = value;
        }
        out.push(state);
    }
    Ok(Value::Array(out))
}

/// The text of one signature check: `signed <kind> <valid|invalid> by <key> (<key type>), key
/// <trusted|unknown>, presence <present|absent>`. An invalid signature prints `E_SIG_INVALID`
/// in place of its key trust; a valid one whose statement does not match ends with `, statement
/// mismatch`. A holder's valid signature of another project (`foreign_repo`) prints `signed in
/// another project (<repo id>): not verified here` in place of its key trust. An unreadable
/// kind, key or key type prints `unknown`.
pub fn signature_line(check: &Value) -> String {
    let word = |k: &str| check[k].as_str().unwrap_or("unknown");
    let signed = word("signed");
    let middle = if signed == SIGNED_INVALID {
        E_SIG_INVALID.to_string()
    } else if check["statement"].as_str() == Some(STATEMENT_FOREIGN_REPO) {
        foreign_repo_text(word("foreign_repo"))
    } else {
        format!("key {}", word("trust"))
    };
    let presence = check["presence"].as_str().unwrap_or(PRESENCE_ABSENT);
    let mut line = format!(
        "signed {} {signed} by {} ({}), {middle}, presence {presence}",
        word("kind"),
        word("key"),
        word("key_type"),
    );
    if signed != SIGNED_INVALID && check["statement"].as_str() == Some(STATEMENT_MISMATCH) {
        line.push_str(", statement mismatch");
    }
    line
}

/// What a holder shows for a valid signature made in the project `repo` (a repo id): it cannot
/// verify it, as it holds no fork history (ruling task-13).
pub fn foreign_repo_text(repo: &str) -> String {
    format!("signed in another project ({repo}): not verified here")
}

/// The fsck text line of a holder's signatures of another project, when there are any: `<n>
/// signatures signed in another project: not verified here`.
pub fn foreign_signatures_line(report: &Value) -> Option<String> {
    let n = report["foreign_signatures"].as_u64().filter(|n| *n > 0)?;
    Some(format!(
        "{n} signatures signed in another project: not verified here"
    ))
}

/// The lines of a list of checks, each indented `indent` spaces; `signed: no` for an empty list.
pub fn signature_lines(checks: &Value, indent: usize) -> Vec<String> {
    let pad = " ".repeat(indent);
    let list = checks.as_array().map(Vec::as_slice).unwrap_or_default();
    if list.is_empty() {
        return vec![format!("{pad}signed: no")];
    }
    list.iter()
        .map(|c| format!("{pad}{}", signature_line(c)))
        .collect()
}

/// The lines of a policy landing (O24), in place of `signed: no`: `    signed: policy`, then
/// `    policy <ap-id>@<short> landed in <short>: signed <state>`, or `    policy
/// <ap-id>@<short>: no landing holds it` when no landing carried the policy version. Empty for
/// any other row.
fn policy_lines(row: &Value) -> Vec<String> {
    if row["signed"].as_str() != Some("policy") {
        return vec![];
    }
    let short = |v: &Value| {
        v.as_str()
            .unwrap_or("unknown")
            .chars()
            .take(12)
            .collect::<String>()
    };
    let who = format!(
        "policy {}@{}",
        row["policy"]["block"].as_str().unwrap_or("unknown"),
        short(&row["policy"]["version"])
    );
    let check = &row["policy_check"];
    let second = match check["landing"].as_str() {
        Some(_) => format!(
            "    {who} landed in {}: signed {}",
            short(&check["landing"]),
            check["signed"].as_str().unwrap_or("unknown")
        ),
        None => format!("    {who}: no landing holds it"),
    };
    vec!["    signed: policy".to_string(), second]
}

fn has_checks(checks: &Value) -> bool {
    checks.as_array().is_some_and(|c| !c.is_empty())
}

/// The verification lines under one verified row (a log row or an fsck state): the landing's
/// signatures at four spaces (a policy landing prints [`policy_lines`] in place of `signed: no`), then each tag by name, `tag <name>:` at four spaces and its
/// signatures at eight.
pub fn verified_lines(row: &Value) -> Vec<String> {
    let policy = policy_lines(row);
    let mut out = if policy.is_empty() || has_checks(&row["signatures"]) {
        signature_lines(&row["signatures"], 4)
    } else {
        vec![]
    };
    out.extend(policy);
    let mut tags: Vec<(&String, &Value)> = row["tag_signatures"]
        .as_object()
        .into_iter()
        .flatten()
        .collect();
    tags.sort_by(|a, b| a.0.cmp(b.0));
    for (name, checks) in tags {
        out.push(format!("    tag {name}:"));
        out.extend(signature_lines(checks, 8));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clone::{CloneSource, clone};
    use crate::signature_check::LINEAGE_SCANS;
    use crate::{Rdy, Session, WriteRequest};
    use rdy_model::signature::Signature;
    use rdy_model::{Provenance, Role};

    /// Writes one doc to view `work`, proposes and lands it; the landing.
    fn land_one(r: &mut Rdy, title: &str) -> Hash {
        let s = Session::human("human:t".into(), Role::Writer).unwrap();
        r.write(
            &s,
            WriteRequest {
                view: "work".into(),
                ops: serde_json::from_value(json!([
                    {"op": "create", "type": "doc", "fields": {"title": title, "doc_type": "page"}}
                ]))
                .unwrap(),
                justification: None,
                message: String::new(),
            },
        )
        .unwrap();
        let p = r.propose(&s, "work", title, Some("test summary")).unwrap();
        let a = Session::human("human:t".into(), Role::Approver).unwrap();
        r.land(&a, &p.id).unwrap().parse().unwrap()
    }

    /// Stores a damaged signature of `subject` (a check reads the lineage for it all the same).
    fn put_damaged(r: &mut Rdy, subject: Hash) {
        let g = Signature {
            subject,
            namespace: "rdy-approval-v1".into(),
            statement: b"rdy-statement: 1\n".to_vec(),
            sig: b"damaged".to_vec(),
            signed_by: Provenance {
                actor: "human:t".into(),
                run: None,
                role: Role::Approver,
                at_ms: 1,
            },
        };
        r.store_mut().put_signature(&g).unwrap();
        r.seal_open().unwrap();
    }

    fn scans(f: impl FnOnce()) -> usize {
        LINEAGE_SCANS.with(|n| n.set(0));
        f();
        LINEAGE_SCANS.with(|n| n.get())
    }

    #[test]
    fn sig9_a_verified_report_reads_the_lineage_once() {
        let d = tempfile::tempdir().unwrap();
        let src_dir = d.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        let mut src = Rdy::init(&src_dir, "rdy-standard").unwrap();
        let landings: Vec<Hash> = ["One", "Two", "Three"]
            .into_iter()
            .map(|title| land_one(&mut src, title))
            .collect();
        // an ordinary clone has an origin, so its lineage is a pass over its ref updates
        let st = src.read();
        let plain = d.path().join("clone");
        clone(CloneSource::State(&st), &plain, false).unwrap();
        drop(st);
        let mut cl = Rdy::open(&plain).unwrap();
        for l in landings {
            put_damaged(&mut cl, l);
        }
        let st = cl.read();
        assert!(st.origin().unwrap().is_some());
        let mut log = Value::Null;
        assert_eq!(
            scans(|| log = verified_log(&st, None, None, None).unwrap()),
            1,
            "one lineage pass for the whole verified log"
        );
        let signed = log
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["signatures"].as_array().is_some_and(|s| s.len() == 1))
            .count();
        assert_eq!(signed, 3, "{log:#}");
        let mut states = Value::Null;
        assert_eq!(
            scans(|| states = signature_states(&st).unwrap()),
            1,
            "one lineage pass for the verbose fsck states"
        );
        assert_eq!(states.as_array().unwrap().len(), 3);
        // an unsigned report reads no lineage at all
        let empty = d.path().join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let mut e = Rdy::init(&empty, "rdy-standard").unwrap();
        land_one(&mut e, "Unsigned");
        let est = e.read();
        let unsigned_clone = d.path().join("empty-clone");
        clone(CloneSource::State(&est), &unsigned_clone, false).unwrap();
        let ec = Rdy::open(&unsigned_clone).unwrap();
        let ecs = ec.read();
        assert_eq!(
            scans(|| drop(verified_log(&ecs, None, None, None).unwrap())),
            0
        );
    }
}
