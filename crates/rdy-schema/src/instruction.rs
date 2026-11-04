//! ain1: the engine type `instruction` (prefix `ai`, D-4W1ZR, D-8X04F) that every pack holds, and
//! the doc field `required` that makes a doc's instructions a required ruleset. An instruction is
//! a normative doc asset with a required body and no key, under a doc or a section. Its fields
//! are `applies_to` (1 to 1024 selectors) and `locked` (false by default), and a `decided_by`
//! link to a decision gives its reason. Its value checks are shared by every validation path; the
//! check that a reason names a decision needs the write's view, so the engine makes it.
use crate::pack::{BlockTypeDef, BodyRule, FieldDef, FieldKind, Pack, Trait, TypeClass};
use crate::validate::{Issue, Severity, err};
use rdy_model::{Block, FieldValue, decision};
use std::collections::{BTreeMap, BTreeSet};

/// The block type of an instruction.
pub const INSTRUCTION: &str = "instruction";
/// The id prefix of an instruction.
pub const INSTRUCTION_PREFIX: &str = "ai";
/// The instruction field holding its selectors.
pub const APPLIES_TO: &str = "applies_to";
/// The instruction field that locks it in a set's own project.
pub const LOCKED: &str = "locked";
/// The doc field that makes the doc a required ruleset.
pub const REQUIRED_FIELD: &str = "required";
/// The selector that makes an instruction required on its own.
pub const ALL: &str = "all";
/// The acts an `act:` selector may name, in order.
pub const ACTS: [&str; 7] = [
    "write", "decide", "propose", "relay", "review", "publish", "redact",
];
/// The fewest selectors one instruction holds.
pub const MIN_SELECTORS: usize = 1;
/// The most selectors one instruction holds.
pub const MAX_SELECTORS: usize = 1024;
/// The most UTF-8 bytes one selector holds.
pub const MAX_SELECTOR_BYTES: usize = 256;
/// The warning of an `area:` selector that names no area of the pack.
pub const W_INSTRUCTION_SELECTOR: &str = "W_INSTRUCTION_SELECTOR";

/// The fix of an invalid selector.
pub const SELECTOR_FIX: &str =
    "use all, area:<name>, type:<name>, or act:<write|decide|propose|relay|review|publish|redact>";
/// The fix of an instruction with no decision as its reason.
pub const REASON_FIX: &str = "link the instruction to its reason with decided_by: rdy_write / rdy new instruction --reason <decision>";
/// The fix of an instruction that carries a key.
pub const KEY_FIX: &str = "remove key; refer to the instruction by its id";
/// The fix of a selector list outside its bounds.
pub const SELECTOR_LIMIT_FIX: &str =
    "write 1 to 1024 selectors in applies_to, each at most 256 bytes";

/// The message of an instruction with no decision as its reason.
pub fn reason_message(id: &str) -> String {
    format!("instruction {id} requires a decided_by decision")
}

/// The error of an instruction whose reason is missing or names no decision.
pub fn reason_issue(block: &Block) -> Issue {
    err(
        block,
        "E_VALIDATION",
        reason_message(block.id.as_str()),
        REASON_FIX.to_string(),
    )
}

fn f(kind: FieldKind, required: bool) -> FieldDef {
    FieldDef {
        kind,
        required,
        values: vec![],
    }
}

/// The core fields of an instruction.
pub(crate) fn core() -> BTreeMap<String, FieldDef> {
    [
        (APPLIES_TO, f(FieldKind::List, true)),
        (LOCKED, f(FieldKind::Bool, false)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// The doc's `required` field: an optional bool.
pub(crate) fn required_field() -> FieldDef {
    f(FieldKind::Bool, false)
}

/// Whether a pack may give an instruction trait `t`: only `normative`.
pub(crate) fn trait_allowed(t: Trait) -> bool {
    t == Trait::Normative
}

/// Places the instruction in `pack`: its one closed definition, kept with the optional fields a
/// pack added (`check_extension` has refused any other declaration), a child of the doc and of
/// a `section` type when the pack has one, after their other children, and of no other type.
/// Runs before the doc's profile and policy are placed, so a doc lists it before them.
pub(crate) fn inject(pack: &mut Pack) {
    let def = pack
        .block_types
        .entry(INSTRUCTION.to_string())
        .or_insert_with(|| BlockTypeDef {
            prefix: INSTRUCTION_PREFIX.to_string(),
            traits: BTreeSet::new(),
            fields: BTreeMap::new(),
            children: vec![],
            body: BodyRule::Required,
            class: None,
            tracking: BTreeSet::new(),
        });
    def.prefix = INSTRUCTION_PREFIX.to_string();
    def.traits = BTreeSet::from([Trait::Normative]);
    def.body = BodyRule::Required;
    def.class = Some(TypeClass::DocAsset);
    def.children.clear();
    def.fields.extend(core());
    for (name, t) in pack.block_types.iter_mut() {
        let holds = name == rdy_model::tree::DOC || name == "section";
        t.children.retain(|c| c != INSTRUCTION);
        if holds {
            t.children.push(INSTRUCTION.to_string());
        }
    }
}

/// The value checks of an instruction that generic validation cannot make, appended after its
/// issues: no key; then, when `applies_to` is a list, its count, string entries, entry lengths
/// and grammar; then a `decided_by` link, unless `projection` (a removed version keeps no
/// links). Empty for another type. Every invalid selector gets its own issue, in list order.
pub(crate) fn check_values(block: &Block, projection: bool, out: &mut Vec<Issue>) {
    if block.block_type != INSTRUCTION {
        return;
    }
    let id = block.id.as_str();
    if block.fields.contains_key("key") {
        out.push(err(
            block,
            "E_VALIDATION",
            format!("instruction {id} cannot have a key"),
            KEY_FIX.to_string(),
        ));
    }
    if let Some(FieldValue::List(items)) = block.fields.get(APPLIES_TO) {
        check_selectors(block, items, out);
    }
    if !projection
        && block
            .links
            .get(decision::DECIDED_BY)
            .is_none_or(|l| l.is_empty())
    {
        out.push(reason_issue(block));
    }
}

fn check_selectors(block: &Block, items: &[FieldValue], out: &mut Vec<Issue>) {
    let id = block.id.as_str();
    if !(MIN_SELECTORS..=MAX_SELECTORS).contains(&items.len()) {
        out.push(err(
            block,
            "E_LIMIT",
            format!(
                "instruction {id} has {} selectors; applies_to holds 1 to 1024",
                items.len()
            ),
            SELECTOR_LIMIT_FIX.to_string(),
        ));
        return;
    }
    let Some(selectors) = items
        .iter()
        .map(FieldValue::as_str)
        .collect::<Option<Vec<&str>>>()
    else {
        out.push(err(
            block,
            "E_FIELD_KIND",
            format!("field {APPLIES_TO:?} must be a list of strings"),
            format!("write {APPLIES_TO} as a list of strings"),
        ));
        return;
    };
    if selectors.iter().any(|s| s.len() > MAX_SELECTOR_BYTES) {
        out.push(err(
            block,
            "E_LIMIT",
            format!("instruction {id} has a selector longer than 256 bytes"),
            SELECTOR_LIMIT_FIX.to_string(),
        ));
        return;
    }
    for s in selectors {
        if !selector_ok(s) {
            out.push(err(
                block,
                "E_VALIDATION",
                format!("instruction {id} has invalid selector {s}"),
                SELECTOR_FIX.to_string(),
            ));
        }
    }
}

/// Whether `s` is `all`, `area:<name>`, `type:<name>` or `act:<act>`. A name is 1 to 64 of
/// a-z, 0-9, `_` and `-`, starting with a letter or digit, as an area name is.
pub fn selector_ok(s: &str) -> bool {
    if s == ALL {
        return true;
    }
    match s.split_once(':') {
        Some(("area" | "type", name)) => name_ok(name),
        Some(("act", act)) => ACTS.contains(&act),
        _ => false,
    }
}

fn name_ok(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

/// The `W_INSTRUCTION_SELECTOR` warnings of an instruction written with an `area:` selector
/// that names no area of `pack`: one per distinct such selector, in list order. Empty for
/// another type, or a selector list that does not validate.
pub fn selector_warnings(pack: &Pack, block: &Block) -> Vec<Issue> {
    if block.block_type != INSTRUCTION {
        return vec![];
    }
    let Some(FieldValue::List(items)) = block.fields.get(APPLIES_TO) else {
        return vec![];
    };
    if items.len() > MAX_SELECTORS {
        return vec![];
    }
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for name in items
        .iter()
        .filter_map(FieldValue::as_str)
        .filter(|s| selector_ok(s))
        .filter_map(|s| s.strip_prefix("area:"))
    {
        if pack.areas.contains_key(name) || !seen.insert(name) {
            continue;
        }
        out.push(Issue {
            severity: Severity::Warning,
            code: W_INSTRUCTION_SELECTOR,
            block: Some(block.id.clone()),
            message: format!("area:{name} matches nothing: the pack has no area {name}"),
            fix: format!("define area {name} in the pack, or correct the selector"),
        });
    }
    out
}
