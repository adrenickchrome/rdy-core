//! The authority policy (D-ZWP7Q): an engine block type, injected into every pack, that says what
//! an agent may land without a person and who approves the rest. It has no body and sits directly
//! under a doc, as a publication profile does. Generic validation checks its four top-level
//! fields; this module checks what they hold (the closed forms and bounds below) and reads a
//! valid block. A project holds one live policy block: its key is fixed, and keys are unique.

use crate::pack::{BlockTypeDef, BodyRule, FieldDef, FieldKind, Pack, Trait, TypeClass};
use crate::validate::{Issue, err};
use rdy_model::actor::is_actor;
use rdy_model::actor::is_name;
use rdy_model::did_you_mean::nearest;
use rdy_model::{Block, FieldValue};
use std::collections::{BTreeMap, BTreeSet};

/// The block type of the authority policy.
pub const POLICY_TYPE: &str = "authority_policy";
/// The id prefix of the authority policy.
pub const POLICY_PREFIX: &str = "ap";
/// The one key a policy block has.
pub const POLICY_KEY: &str = "authority-policy";

/// The most rules in `agent_may_accept`.
pub const MAX_RULES: usize = 64;
/// The most actor patterns in one rule's `actors`.
pub const MAX_PATTERNS: usize = 64;
/// The most areas in one rule's `areas` or `two_way_areas`.
pub const MAX_AREAS: usize = 64;
/// The most roles in `roles`.
pub const MAX_ROLES: usize = 64;
/// The most members in one role or one owner entry.
pub const MAX_MEMBERS: usize = 256;
/// The most entries in `require`.
pub const MAX_REQUIRE: usize = 64;
/// The most entries in `owners`.
pub const MAX_OWNERS: usize = 10_000;
/// The most bytes in an owner entry's `path`: the label `owners of <path>` then stays within
/// `rdy_model::assessment::MAX_POLICY_NAME_BYTES`.
pub const MAX_PATH_BYTES: usize = 240;
/// The largest `max_blocks` of a rule.
pub const MAX_BLOCKS: i64 = 100_000;
/// The largest `max_items` of a rule.
pub const MAX_ITEMS: i64 = 100_000;

/// The most justifications in one rule: each kind once.
const MAX_JUSTIFICATIONS: usize = 3;
/// The most bases in one rule: each accepted basis once.
const MAX_BASES: usize = 4;
/// The most characters of a supplied value an issue echoes.
const MAX_ECHO_CHARS: usize = 80;

const FIX: &str = "correct the authority_policy block: rdy_write / rdy edit";

const AGENT_MAY_ACCEPT: &str = "agent_may_accept";
const ROLES: &str = "roles";
const REQUIRE: &str = "require";
const OWNERS: &str = "owners";

/// The keys of a rule, in the order they are checked.
const RULE_KEYS: [&str; 10] = [
    "name",
    "actors",
    "areas",
    "lifecycle",
    "justifications",
    "decisions",
    "two_way_areas",
    "basis",
    "max_blocks",
    "max_items",
];
/// The keys a rule may leave out (each reads as empty).
const RULE_OPTIONAL: [&str; 3] = ["areas", "two_way_areas", "basis"];
const REQUIRE_KEYS: [&str; 2] = ["when", "role"];
const OWNER_KEYS: [&str; 2] = ["path", "owners"];

const NAME_FORM: &str =
    "must be a name of 1 to 64 bytes of a-z, 0-9, -, _ and ., with a letter or digit at each end";
const RULE_FORM: &str = "must be a rule: a map of name, actors, areas, lifecycle, justifications, decisions, two_way_areas, basis, max_blocks and max_items";
const ACTORS_FORM: &str = "must be a list of 1 to 64 actor patterns";
const PATTERN_FORM: &str = "must be an actor pattern, such as agent:nick/*";
const AREAS_FORM: &str = "must be a list of area names";
const AREA_FORM: &str = "must be an area name";
const NO_AREA: &str = "names no area of the pack";
const LIFECYCLE_FORM: &str = "must be draft or any";
const JUSTIFICATIONS_FORM: &str = "must be a list of 1 to 3 of implements, editorial and reflects";
const JUSTIFICATION_FORM: &str = "must be implements, editorial or reflects";
const DECISIONS_FORM: &str = "must be none or two_way";
const TWO_WAY_NEEDS_AREAS: &str = "must be 1 to 64 areas of the pack when decisions is two_way";
const NONE_TAKES_NO_AREAS: &str = "must be empty when decisions is none";
const BASIS_FORM: &str = "must be a list of delegated, precedent, elimination and none";
const BASE_FORM: &str = "must be delegated, precedent, elimination or none";
const MEMBERS_FORM: &str = "must be a list of 1 to 256 members";
const MEMBER_FORM: &str = "must be human:<name> or team:<name>";
const REQUIRE_FORM: &str = "must be a map of when and role";
const WHEN_FORM: &str = "must be one_way, decision, area:<area>, deferral, reaffirm, owner or any";
const ROLE_FORM: &str = "must be a role of roles";
const OWNER_TAKES_NO_ROLE: &str = "must be absent when when is owner";
const OWNER_FORM: &str = "must be a map of path and owners";
const PATH_FORM: &str = "must be a path of 1 to 240 bytes";

/// The bases a rule may accept; an absent basis reads `none`.
const BASES: [&str; 4] = ["delegated", "precedent", "elimination", "none"];
/// The bases of a person's ruling, which a policy never accepts.
const RULING_BASES: [&str; 2] = ["direct_ruling", "recorded_ruling"];

/// Which content a rule covers: draft content only, or any.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    Draft,
    Any,
}

/// Which decisions a rule accepts: none, or those declared `two_way` in its `two_way_areas`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionsRule {
    None,
    TwoWay,
}

/// A change's justification kind that a rule accepts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum JustificationKind {
    Implements,
    Editorial,
    Reflects,
}

/// One rule of `agent_may_accept`. An empty `areas` means every area; an empty `basis` accepts
/// no decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    pub name: String,
    pub actors: Vec<String>,
    pub areas: Vec<String>,
    pub lifecycle: Lifecycle,
    pub justifications: BTreeSet<JustificationKind>,
    pub decisions: DecisionsRule,
    pub two_way_areas: Vec<String>,
    pub basis: BTreeSet<String>,
    pub max_blocks: u32,
    pub max_items: u32,
}

/// When a `require` entry applies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum When {
    OneWay,
    Decision,
    Area(String),
    Deferral,
    Reaffirm,
    Owner,
    Any,
}

/// One `require` entry: the role it names, none for `when: owner`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Require {
    pub when: When,
    pub role: Option<String>,
}

/// One `owners` entry: a path (or block id) and the people and teams who own it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerEntry {
    pub path: String,
    pub owners: Vec<String>,
}

/// The fields of a valid policy block, in the author's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyFields {
    pub rules: Vec<Rule>,
    pub roles: BTreeMap<String, Vec<String>>,
    pub require: Vec<Require>,
    pub owners: Vec<OwnerEntry>,
}

fn optional(kind: FieldKind) -> FieldDef {
    FieldDef {
        kind,
        required: false,
        values: vec![],
    }
}

/// The closed definition of the policy type, the same in every pack. `key` is the engine field
/// every type has, outside this map.
pub(crate) fn type_def() -> BlockTypeDef {
    BlockTypeDef {
        prefix: POLICY_PREFIX.to_string(),
        traits: BTreeSet::from([Trait::Structural]),
        fields: BTreeMap::from([
            (AGENT_MAY_ACCEPT.to_string(), optional(FieldKind::List)),
            (OWNERS.to_string(), optional(FieldKind::List)),
            (REQUIRE.to_string(), optional(FieldKind::List)),
            (ROLES.to_string(), optional(FieldKind::Map)),
        ]),
        children: vec![],
        body: BodyRule::Forbidden,
        class: Some(TypeClass::Supporting),
        tracking: BTreeSet::new(),
    }
}

/// The engine's template: the fields of a new policy block, the same for every pack.
pub fn template_fields() -> BTreeMap<String, FieldValue> {
    BTreeMap::from([
        ("key".to_string(), FieldValue::Str(POLICY_KEY.to_string())),
        (AGENT_MAY_ACCEPT.to_string(), FieldValue::List(vec![])),
        (ROLES.to_string(), FieldValue::Map(BTreeMap::new())),
        (REQUIRE.to_string(), FieldValue::List(vec![])),
        (OWNERS.to_string(), FieldValue::List(vec![])),
    ])
}

/// Whether `pattern` is an actor pattern: an actor, or text that ends in one `*` where the text
/// with `*` replaced by `a` is an actor.
pub fn is_pattern(pattern: &str) -> bool {
    if pattern.len() > rdy_model::actor::MAX_ACTOR_BYTES {
        return false;
    }
    match pattern.strip_suffix('*') {
        Some(head) => !head.contains('*') && is_actor(&format!("{head}a")),
        None => is_actor(pattern),
    }
}

/// Whether `pattern` covers `actor`: a pattern with `*` matches every actor that starts with the
/// text before `*`; one without matches itself only. Text that is no pattern matches nothing.
pub fn pattern_matches(pattern: &str, actor: &str) -> bool {
    if !is_pattern(pattern) {
        return false;
    }
    match pattern.strip_suffix('*') {
        Some(head) => actor.starts_with(head),
        None => actor == pattern,
    }
}

/// The value checks generic validation cannot make on a policy block, appended after its issues.
/// A top-level field of the wrong kind (generic validation reports it) is skipped. Each key of
/// an entry gives at most one issue, and a list of values reports its first failing value.
pub(crate) fn check_values(pack: &Pack, block: &Block, out: &mut Vec<Issue>) {
    read(pack, block, out);
}

/// The fields of a valid policy block; None when `check_values` reports any error for it, when a
/// top-level field has the wrong kind, or when the block is of another type.
pub fn policy_fields(pack: &Pack, block: &Block) -> Option<PolicyFields> {
    if block.block_type != POLICY_TYPE {
        return None;
    }
    let mut out = vec![];
    let fields = read(pack, block, &mut out);
    if out.is_empty() { fields } else { None }
}

/// `value` cut to [`MAX_ECHO_CHARS`] characters, with `…` when cut.
fn clip(value: &str) -> String {
    match value.char_indices().nth(MAX_ECHO_CHARS) {
        Some((at, _)) => format!("{}\u{2026}", &value[..at]),
        None => value.to_string(),
    }
}

struct Reader<'a> {
    pack: &'a Pack,
    block: &'a Block,
    out: &'a mut Vec<Issue>,
}

impl Reader<'_> {
    fn push(&mut self, code: &'static str, path: &str, reason: &str) {
        self.out.push(err(
            self.block,
            code,
            format!("authority policy {path}: {reason}"),
            FIX.to_string(),
        ));
    }

    fn kind(&mut self, path: &str, reason: &str) {
        self.push("E_FIELD_KIND", path, reason);
    }

    fn limit(&mut self, path: &str, max: usize, things: &str) {
        self.push("E_LIMIT", path, &format!("holds more than {max} {things}"));
    }

    fn required(&mut self, path: &str) {
        self.push("E_FIELD_REQUIRED", path, "is required");
    }

    /// One `E_FIELD_UNKNOWN` per key of `m` outside `known`, in key order.
    fn unknown_keys(&mut self, at: &str, m: &BTreeMap<String, FieldValue>, known: &[&str]) -> bool {
        let mut clean = true;
        for k in m.keys().filter(|k| !known.contains(&k.as_str())) {
            let reason = match nearest(k, known) {
                Some(near) => format!("is not a field; did you mean {near}?"),
                None => "is not a field".to_string(),
            };
            self.push("E_FIELD_UNKNOWN", &format!("{at}.{}", clip(k)), &reason);
            clean = false;
        }
        clean
    }

    fn is_area(&self, name: &str) -> bool {
        self.pack.areas.contains_key(name)
    }

    /// A list of `max` or fewer strings, each passing `check` (which gives the first failure's
    /// reason) and each distinct when `distinct`; at least one when `min_one` (else `form`).
    #[allow(clippy::too_many_arguments)]
    fn strings(
        &mut self,
        path: &str,
        v: &FieldValue,
        form: &str,
        max: usize,
        things: &str,
        min_one: bool,
        distinct: bool,
        check: &dyn Fn(&Self, &str) -> Option<String>,
    ) -> Option<Vec<String>> {
        let FieldValue::List(items) = v else {
            self.kind(path, form);
            return None;
        };
        if items.len() > max {
            self.limit(path, max, things);
            return None;
        }
        if min_one && items.is_empty() {
            self.kind(path, form);
            return None;
        }
        let mut seen = BTreeSet::new();
        let mut texts = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let at = format!("{path}[{i}]");
            let Some(text) = item.as_str() else {
                self.kind(&at, form_of_entry(form));
                return None;
            };
            if let Some(reason) = check(self, text) {
                self.kind(&at, &reason);
                return None;
            }
            if distinct && !seen.insert(text) {
                self.kind(&at, &format!("repeats {}", clip(text)));
                return None;
            }
            texts.push(text.to_string());
        }
        Some(texts)
    }

    fn members(&mut self, path: &str, v: &FieldValue) -> Option<Vec<String>> {
        self.strings(
            path,
            v,
            MEMBERS_FORM,
            MAX_MEMBERS,
            "members",
            true,
            true,
            &|_, m| member_problem(m),
        )
    }

    fn areas(&mut self, path: &str, v: &FieldValue) -> Option<Vec<String>> {
        self.strings(
            path,
            v,
            AREAS_FORM,
            MAX_AREAS,
            "areas",
            false,
            true,
            &|r, a| (!r.is_area(a)).then(|| NO_AREA.to_string()),
        )
    }

    fn rules(&mut self, items: &[FieldValue]) -> Option<Vec<Rule>> {
        if items.len() > MAX_RULES {
            self.limit(AGENT_MAY_ACCEPT, MAX_RULES, "rules");
            return None;
        }
        let mut names = BTreeSet::new();
        let mut rules = Some(Vec::with_capacity(items.len()));
        for (i, item) in items.iter().enumerate() {
            let rule = self.rule(&format!("{AGENT_MAY_ACCEPT}[{i}]"), item, &mut names);
            rules = rules.zip(rule).map(|(mut all, r)| {
                all.push(r);
                all
            });
        }
        rules
    }

    fn rule(&mut self, at: &str, v: &FieldValue, names: &mut BTreeSet<String>) -> Option<Rule> {
        let FieldValue::Map(m) = v else {
            self.kind(at, RULE_FORM);
            return None;
        };
        let mut ok = self.unknown_keys(at, m, &RULE_KEYS);
        let mut name = None;
        let mut actors = None;
        let mut areas = Some(vec![]);
        let mut lifecycle = None;
        let mut justifications = None;
        let mut decisions = None;
        let mut two_way_areas = Some(vec![]);
        let mut basis = Some(BTreeSet::new());
        let mut max_blocks = None;
        let mut max_items = None;
        for key in RULE_KEYS {
            let path = format!("{at}.{key}");
            let Some(v) = m.get(key) else {
                if !RULE_OPTIONAL.contains(&key) {
                    self.required(&path);
                    ok = false;
                } else if key == "two_way_areas" && decisions == Some(DecisionsRule::TwoWay) {
                    self.kind(&path, TWO_WAY_NEEDS_AREAS);
                    two_way_areas = None;
                }
                continue;
            };
            match key {
                "name" => name = self.rule_name(&path, v, names),
                "actors" => {
                    actors = self.strings(
                        &path,
                        v,
                        ACTORS_FORM,
                        MAX_PATTERNS,
                        "patterns",
                        true,
                        false,
                        &|_, p| (!is_pattern(p)).then(|| PATTERN_FORM.to_string()),
                    )
                }
                "areas" => areas = self.areas(&path, v),
                "lifecycle" => {
                    lifecycle = match v.as_str() {
                        Some("draft") => Some(Lifecycle::Draft),
                        Some("any") => Some(Lifecycle::Any),
                        _ => {
                            self.kind(&path, LIFECYCLE_FORM);
                            None
                        }
                    }
                }
                "justifications" => justifications = self.justifications(&path, v),
                "decisions" => {
                    decisions = match v.as_str() {
                        Some("none") => Some(DecisionsRule::None),
                        Some("two_way") => Some(DecisionsRule::TwoWay),
                        _ => {
                            self.kind(&path, DECISIONS_FORM);
                            None
                        }
                    }
                }
                "two_way_areas" => {
                    two_way_areas = self.areas(&path, v);
                    let reason = match (&two_way_areas, decisions) {
                        (Some(a), Some(DecisionsRule::TwoWay)) if a.is_empty() => {
                            Some(TWO_WAY_NEEDS_AREAS)
                        }
                        (Some(a), Some(DecisionsRule::None)) if !a.is_empty() => {
                            Some(NONE_TAKES_NO_AREAS)
                        }
                        _ => None,
                    };
                    if let Some(reason) = reason {
                        self.kind(&path, reason);
                        two_way_areas = None;
                    }
                }
                "basis" => basis = self.basis(&path, v),
                "max_blocks" => max_blocks = self.bounded(&path, v, 1, MAX_BLOCKS),
                _ => max_items = self.bounded(&path, v, 0, MAX_ITEMS),
            }
        }
        if !ok {
            return None;
        }
        Some(Rule {
            name: name?,
            actors: actors?,
            areas: areas?,
            lifecycle: lifecycle?,
            justifications: justifications?,
            decisions: decisions?,
            two_way_areas: two_way_areas?,
            basis: basis?,
            max_blocks: max_blocks?,
            max_items: max_items?,
        })
    }

    fn rule_name(
        &mut self,
        path: &str,
        v: &FieldValue,
        names: &mut BTreeSet<String>,
    ) -> Option<String> {
        match v.as_str() {
            Some(n) if is_name(n) => {
                if names.insert(n.to_string()) {
                    Some(n.to_string())
                } else {
                    self.kind(path, &format!("repeats {n}"));
                    None
                }
            }
            _ => {
                self.kind(path, NAME_FORM);
                None
            }
        }
    }

    fn justifications(
        &mut self,
        path: &str,
        v: &FieldValue,
    ) -> Option<BTreeSet<JustificationKind>> {
        let texts = self.strings(
            path,
            v,
            JUSTIFICATIONS_FORM,
            MAX_JUSTIFICATIONS,
            "justifications",
            true,
            true,
            &|_, j| {
                justification(j)
                    .is_none()
                    .then(|| JUSTIFICATION_FORM.to_string())
            },
        )?;
        texts.iter().map(|j| justification(j)).collect()
    }

    fn basis(&mut self, path: &str, v: &FieldValue) -> Option<BTreeSet<String>> {
        let texts = self.strings(
            path,
            v,
            BASIS_FORM,
            MAX_BASES,
            "bases",
            false,
            true,
            &|_, b| {
                if RULING_BASES.contains(&b) {
                    Some(format!(
                        "lists {b}; a policy never accepts a person's ruling"
                    ))
                } else {
                    (!BASES.contains(&b)).then(|| BASE_FORM.to_string())
                }
            },
        )?;
        Some(texts.into_iter().collect())
    }

    fn bounded(&mut self, path: &str, v: &FieldValue, min: i64, max: i64) -> Option<u32> {
        match v {
            FieldValue::Int(n) if (min..=max).contains(n) => u32::try_from(*n).ok(),
            _ => {
                self.kind(path, &format!("must be a whole number from {min} to {max}"));
                None
            }
        }
    }

    fn roles(&mut self, m: &BTreeMap<String, FieldValue>) -> Option<BTreeMap<String, Vec<String>>> {
        if m.len() > MAX_ROLES {
            self.limit(ROLES, MAX_ROLES, "roles");
            return None;
        }
        let mut roles = Some(BTreeMap::new());
        for (name, v) in m {
            let path = format!("{ROLES}.{}", clip(name));
            if !is_name(name) {
                self.kind(&path, NAME_FORM);
                roles = None;
                continue;
            }
            let members = self.members(&path, v);
            roles = roles.zip(members).map(|(mut all, members)| {
                all.insert(name.clone(), members);
                all
            });
        }
        roles
    }

    /// `roles` is the names of the policy's roles, or None when `roles` does not read (its own
    /// issue names it), and then no role name is checked.
    fn require(
        &mut self,
        items: &[FieldValue],
        roles: Option<&BTreeSet<&str>>,
    ) -> Option<Vec<Require>> {
        if items.len() > MAX_REQUIRE {
            self.limit(REQUIRE, MAX_REQUIRE, "entries");
            return None;
        }
        let mut entries = Some(Vec::with_capacity(items.len()));
        for (i, item) in items.iter().enumerate() {
            let entry = self.require_entry(&format!("{REQUIRE}[{i}]"), item, roles);
            entries = entries.zip(entry).map(|(mut all, e)| {
                all.push(e);
                all
            });
        }
        entries
    }

    fn require_entry(
        &mut self,
        at: &str,
        v: &FieldValue,
        roles: Option<&BTreeSet<&str>>,
    ) -> Option<Require> {
        let FieldValue::Map(m) = v else {
            self.kind(at, REQUIRE_FORM);
            return None;
        };
        let ok = self.unknown_keys(at, m, &REQUIRE_KEYS);
        let when_path = format!("{at}.when");
        let when = match m.get("when") {
            None => {
                self.required(&when_path);
                None
            }
            Some(v) => match v.as_str().and_then(when_of) {
                Some(When::Area(a)) if !self.is_area(&a) => {
                    self.kind(&when_path, NO_AREA);
                    None
                }
                Some(w) => Some(w),
                None => {
                    self.kind(&when_path, WHEN_FORM);
                    None
                }
            },
        };
        let role_path = format!("{at}.role");
        let owner = when == Some(When::Owner);
        let role = match (m.get("role"), owner) {
            (None, true) => Some(None),
            (Some(_), true) => {
                self.kind(&role_path, OWNER_TAKES_NO_ROLE);
                None
            }
            (None, false) => {
                self.required(&role_path);
                None
            }
            (Some(v), false) => match (v.as_str(), roles) {
                (Some(r), Some(names)) if names.contains(r) => Some(Some(r.to_string())),
                (Some(_), None) => None,
                _ => {
                    self.kind(&role_path, ROLE_FORM);
                    None
                }
            },
        };
        if !ok {
            return None;
        }
        Some(Require {
            when: when?,
            role: role?,
        })
    }

    fn owners(&mut self, items: &[FieldValue]) -> Option<Vec<OwnerEntry>> {
        if items.len() > MAX_OWNERS {
            self.limit(OWNERS, MAX_OWNERS, "entries");
            return None;
        }
        let mut paths = BTreeSet::new();
        let mut entries = Some(Vec::with_capacity(items.len()));
        for (i, item) in items.iter().enumerate() {
            let entry = self.owner_entry(&format!("{OWNERS}[{i}]"), item, &mut paths);
            entries = entries.zip(entry).map(|(mut all, e)| {
                all.push(e);
                all
            });
        }
        entries
    }

    fn owner_entry<'v>(
        &mut self,
        at: &str,
        v: &'v FieldValue,
        paths: &mut BTreeSet<&'v str>,
    ) -> Option<OwnerEntry> {
        let FieldValue::Map(m) = v else {
            self.kind(at, OWNER_FORM);
            return None;
        };
        let ok = self.unknown_keys(at, m, &OWNER_KEYS);
        let path_at = format!("{at}.path");
        let path = match m.get("path").map(FieldValue::as_str) {
            None => {
                self.required(&path_at);
                None
            }
            Some(Some(p)) if p.len() > MAX_PATH_BYTES => {
                self.limit(&path_at, MAX_PATH_BYTES, "bytes");
                None
            }
            Some(Some(p)) if !p.is_empty() => {
                if paths.insert(p) {
                    Some(p.to_string())
                } else {
                    self.kind(&path_at, &format!("repeats {p}"));
                    None
                }
            }
            Some(_) => {
                self.kind(&path_at, PATH_FORM);
                None
            }
        };
        let owners_at = format!("{at}.owners");
        let owners = match m.get("owners") {
            None => {
                self.required(&owners_at);
                None
            }
            Some(v) => self.members(&owners_at, v),
        };
        if !ok {
            return None;
        }
        Some(OwnerEntry {
            path: path?,
            owners: owners?,
        })
    }
}

/// The reason a list entry of the wrong kind gets: the entry's form for each list form.
fn form_of_entry(list_form: &str) -> &'static str {
    match list_form {
        ACTORS_FORM => PATTERN_FORM,
        AREAS_FORM => AREA_FORM,
        JUSTIFICATIONS_FORM => JUSTIFICATION_FORM,
        BASIS_FORM => BASE_FORM,
        _ => MEMBER_FORM,
    }
}

/// Why `m` is no role member: an `agent:` or `ci:` actor, or text outside the member form.
fn member_problem(m: &str) -> Option<String> {
    if m.starts_with("agent:") || m.starts_with("ci:") {
        return Some(format!("names {}; it holds people and teams only", clip(m)));
    }
    let name = m.strip_prefix("human:").or_else(|| m.strip_prefix("team:"));
    match name {
        Some(n) if is_name(n) => None,
        _ => Some(MEMBER_FORM.to_string()),
    }
}

fn justification(j: &str) -> Option<JustificationKind> {
    match j {
        "implements" => Some(JustificationKind::Implements),
        "editorial" => Some(JustificationKind::Editorial),
        "reflects" => Some(JustificationKind::Reflects),
        _ => None,
    }
}

fn when_of(w: &str) -> Option<When> {
    Some(match w {
        "one_way" => When::OneWay,
        "decision" => When::Decision,
        "deferral" => When::Deferral,
        "reaffirm" => When::Reaffirm,
        "owner" => When::Owner,
        "any" => When::Any,
        _ => When::Area(w.strip_prefix("area:")?.to_string()),
    })
}

/// Checks every field of `block` and reads them; None when a field does not read. A top-level
/// field of the wrong kind gives no issue here (generic validation reports it) but reads as None.
fn read(pack: &Pack, block: &Block, out: &mut Vec<Issue>) -> Option<PolicyFields> {
    let mut r = Reader { pack, block, out };
    let mut ok = true;
    match block.fields.get("key") {
        None => {
            r.required("key");
            ok = false;
        }
        Some(FieldValue::Str(k)) if k != POLICY_KEY => {
            r.kind("key", "key must be authority-policy");
            ok = false;
        }
        Some(FieldValue::Str(_)) => {}
        Some(_) => ok = false,
    }
    let rules = match block.fields.get(AGENT_MAY_ACCEPT) {
        None => Some(vec![]),
        Some(FieldValue::List(items)) => r.rules(items),
        Some(_) => None,
    };
    let roles = match block.fields.get(ROLES) {
        None => Some(BTreeMap::new()),
        Some(FieldValue::Map(m)) => r.roles(m),
        Some(_) => None,
    };
    // the role names `require` may name: every key of a map `roles`, even one whose members
    // do not read, so a bad member is not reported again under `require`
    let role_names: Option<BTreeSet<&str>> = match block.fields.get(ROLES) {
        None => Some(BTreeSet::new()),
        Some(FieldValue::Map(m)) => Some(m.keys().map(String::as_str).collect()),
        Some(_) => None,
    };
    let require = match block.fields.get(REQUIRE) {
        None => Some(vec![]),
        Some(FieldValue::List(items)) => r.require(items, role_names.as_ref()),
        Some(_) => None,
    };
    let owners = match block.fields.get(OWNERS) {
        None => Some(vec![]),
        Some(FieldValue::List(items)) => r.owners(items),
        Some(_) => None,
    };
    if !ok {
        return None;
    }
    Some(PolicyFields {
        rules: rules?,
        roles: roles?,
        require: require?,
        owners: owners?,
    })
}
