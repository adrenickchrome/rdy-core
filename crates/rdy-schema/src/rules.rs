//! A pack's rules (D-JQD5X): each names the blocks it selects and the filter they must meet. This
//! file loads and validates them; running a rule belongs to the engine.
//!
//! Every text is bounded in bytes before its characters are counted, so a long text costs nothing
//! to refuse, and a text over the byte bound gets the same words as one over the character bound.
//!
//! A rule's test is a `require` filter or a `module` (D-ZRXY0). [`RuleInput`] is a rule of either
//! kind, as a pack holds it, and [`validate_rule_input`] checks its structure as the pack file is
//! read (D-WV7ZS, D-N1GG5): the module's bytes, `args` and `depth`, never the module's code.

use crate::filter::{Expr, Field, Filter, parse};
use crate::pack::Pack;
use crate::resolve::SchemaError;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use rdy_model::did_you_mean::nearest;
use serde::de::{self, IgnoredAny, Visitor};
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_yaml_ng::Value;
use std::collections::BTreeMap;
use std::fmt;

/// The most rules a resolved pack holds, its parents' rules included.
pub const MAX_RULES: usize = 64;
/// The most characters of a rule's name, and the bytes that 64 of the widest characters take.
pub const MAX_RULE_NAME_CHARS: usize = 64;
const MAX_RULE_NAME_BYTES: usize = 256;
/// The most characters of a rule's message or fix, and of a decision's confirmation.
pub const MAX_TEXT_CHARS: usize = 2_000;
/// The bytes that 2000 of the widest characters take.
pub const MAX_TEXT_BYTES: usize = 8_000;

/// The most bytes of one decoded module.
pub const MAX_MODULE_BYTES: usize = 1_048_576;
/// The most bytes of all the decoded modules of a resolved pack, each inherited rule counted.
pub const MAX_PACK_MODULE_BYTES: usize = 8_388_608;
/// The most bytes of a rule's `args` as compact JSON.
pub const MAX_ARGS_BYTES: usize = 8_192;
/// The largest magnitude of an integer in `args`: 2^53.
pub const MAX_ARGS_INT: i64 = 9_007_199_254_740_992;
/// The most link steps of neighbours a module sees.
pub const MAX_DEPTH: u8 = 3;
/// The link steps of neighbours a module sees when its rule gives no `depth`.
pub const DEFAULT_DEPTH: u8 = 1;

/// The keys of a rule, in the order a rule writes them.
const RULE_KEYS: [&str; 9] = [
    "name", "select", "require", "module", "args", "depth", "severity", "message", "fix",
];

/// The one field the landing sets on a decision: a rule that read it could pass at propose and
/// fail at landing, so it cannot name it.
const LANDING_CLOCK_FIELD: &str = "decided_at";

/// What a failing rule does. Nothing runs here: the engine gives the severity its meaning.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleSeverity {
    #[default]
    Advisory,
    Blocking,
}

/// One value of a module rule's `args`: what compact JSON holds with integers only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArgValue {
    Null,
    Bool(bool),
    /// From -2^53 to 2^53.
    Int(i64),
    Str(String),
    List(Vec<ArgValue>),
    /// Keys in byte order.
    Map(BTreeMap<String, ArgValue>),
}

impl Serialize for ArgValue {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            ArgValue::Null => s.serialize_unit(),
            ArgValue::Bool(b) => s.serialize_bool(*b),
            ArgValue::Int(i) => s.serialize_i64(*i),
            ArgValue::Str(t) => s.serialize_str(t),
            ArgValue::List(items) => items.serialize(s),
            ArgValue::Map(m) => m.serialize(s),
        }
    }
}

/// A module rule's test: the decoded module, and `args` and `depth` as the rule gives them. A key
/// the rule leaves out stays `None`, so it stays out of the rule's serialized form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleTest {
    pub module: Vec<u8>,
    pub args: Option<BTreeMap<String, ArgValue>>,
    pub depth: Option<u8>,
}

impl ModuleTest {
    /// The link steps of neighbours the module sees: `depth`, else [`DEFAULT_DEPTH`].
    pub fn depth(&self) -> u8 {
        self.depth.unwrap_or(DEFAULT_DEPTH)
    }
}

/// What a rule tests on each block its `select` matches: exactly one of a filter and a module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuleTestSpec {
    /// The `require` filter, as written.
    Filter {
        require: String,
    },
    Module(ModuleTest),
}

/// One rule of either kind as the pack file writes it, its structure checked by
/// [`validate_rule_input`]; `select` and `require` are filters in the query grammar. It serializes
/// its keys in the order `name`, `select`, `require` (a filter rule) or `module`, `args` and
/// `depth` (a module rule, `args` and `depth` only when the rule sets them), `severity`,
/// `message`, `fix`; `module` as standard base64 with no spaces. So a filter rule writes the
/// bytes it wrote before module rules existed, and an unchanged pack keeps its digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleInput {
    pub name: String,
    pub select: String,
    pub test: RuleTestSpec,
    pub severity: RuleSeverity,
    pub message: String,
    pub fix: String,
}

impl Serialize for RuleInput {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut st = s.serialize_struct("Rule", RULE_KEYS.len())?;
        st.serialize_field("name", &self.name)?;
        st.serialize_field("select", &self.select)?;
        match &self.test {
            RuleTestSpec::Filter { require } => st.serialize_field("require", require)?,
            RuleTestSpec::Module(m) => {
                st.serialize_field("module", &STANDARD.encode(&m.module))?;
                if let Some(args) = &m.args {
                    st.serialize_field("args", args)?;
                }
                if let Some(depth) = m.depth {
                    st.serialize_field("depth", &depth)?;
                }
            }
        }
        st.serialize_field("severity", &self.severity)?;
        st.serialize_field("message", &self.message)?;
        st.serialize_field("fix", &self.fix)?;
        st.end()
    }
}

impl<'de> Deserialize<'de> for RuleInput {
    /// The rule [`validate_rule_input`] reads from the value; its refusal is the error's text.
    /// A pack file's rules are checked first by [`crate::PackSet::add_yaml_from`], which keeps the
    /// refusal's kind.
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<RuleInput, D::Error> {
        let value = Value::deserialize(d)?;
        validate_rule_input(&value).map_err(|e| match e {
            SchemaError::Yaml(text) => de::Error::custom(text),
            other => de::Error::custom(other),
        })
    }
}

impl RuleInput {
    /// The `require` filter of a filter rule; `None` for a module rule.
    pub fn require(&self) -> Option<&str> {
        match &self.test {
            RuleTestSpec::Filter { require } => Some(require),
            RuleTestSpec::Module(_) => None,
        }
    }
}

/// A rule's keys as serde reads them. `Some(None)` is a key set to null, `None` a key left out.
/// `module` borrows its text from the input, and `args` and `depth` are read in place, so nothing
/// of theirs is copied before its bound is checked.
#[derive(Deserialize)]
#[serde(rename = "Rule", deny_unknown_fields)]
struct RawRule<'a> {
    name: String,
    select: String,
    #[serde(default, deserialize_with = "present")]
    require: Option<Option<String>>,
    #[serde(default, borrow, deserialize_with = "present_text")]
    module: Option<Option<&'a str>>,
    #[serde(default, deserialize_with = "present")]
    args: Option<Option<IgnoredAny>>,
    #[serde(default, deserialize_with = "present")]
    depth: Option<Option<IgnoredAny>>,
    #[serde(default)]
    severity: RuleSeverity,
    message: String,
    fix: String,
}

/// A key that is present: `Some`, holding `None` when it is null.
fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// A text key that is present, borrowed from the input: `Some`, holding `None` when it is null.
fn present_text<'de, D>(d: D) -> Result<Option<Option<&'de str>>, D::Error>
where
    D: Deserializer<'de>,
{
    struct Text;
    impl<'de> Visitor<'de> for Text {
        type Value = Option<&'de str>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a string")
        }
        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            d.deserialize_str(self)
        }
        fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
            Ok(Some(v))
        }
    }
    d.deserialize_option(Text).map(Some)
}

/// The structure of the rule `input`, one entry of a pack's `rules`. Err: serde's words for a
/// value that is no rule, an unknown key with the nearest rule key, or a missing or mistyped key
/// (`SchemaError::Yaml`); else the first of these details (`SchemaError::Invalid`): an explicit
/// null for `require`, `module`, `args` or `depth`; both or neither of `require` and `module`;
/// `args` or `depth` without `module`; the module's base64 and size; `args`; `depth`. The name,
/// texts and filters are checked with the resolved pack, by [`crate::check_resolved_rules`].
pub fn validate_rule_input(input: &Value) -> Result<RuleInput, SchemaError> {
    let raw = RawRule::deserialize(input).map_err(|e| SchemaError::Yaml(with_near_key(e)))?;
    let name = raw.name.as_str();
    for (key, is_null) in [
        ("require", raw.require.as_ref().is_some_and(Option::is_none)),
        ("module", raw.module.as_ref().is_some_and(Option::is_none)),
        ("args", raw.args.as_ref().is_some_and(Option::is_none)),
        ("depth", raw.depth.as_ref().is_some_and(Option::is_none)),
    ] {
        if is_null {
            return Err(invalid(format!("rule {name} {key}: null is not allowed")));
        }
    }
    let (require, module) = (raw.require.flatten(), raw.module.flatten());
    // the nodes themselves, read where they are
    let node = |key: &str, set: bool| set.then(|| input.get(key).unwrap_or(&Value::Null));
    let args = node("args", raw.args.is_some());
    let depth = node("depth", raw.depth.is_some());
    let test = match (require, module) {
        (Some(_), Some(_)) => {
            return Err(invalid(format!(
                "rule {name} takes require or module, not both"
            )));
        }
        (None, None) => return Err(invalid(format!("rule {name} needs require or module"))),
        (Some(require), None) => {
            if let Some(key) = [("args", args.is_some()), ("depth", depth.is_some())]
                .into_iter()
                .find_map(|(k, set)| set.then_some(k))
            {
                return Err(invalid(format!(
                    "rule {name} {key} applies to module rules only"
                )));
            }
            RuleTestSpec::Filter { require }
        }
        (None, Some(text)) => RuleTestSpec::Module(ModuleTest {
            module: decode_module(name, text).map_err(invalid)?,
            args: args
                .map(|a| check_args(name, a))
                .transpose()
                .map_err(invalid)?,
            depth: depth
                .map(|d| check_depth(name, d))
                .transpose()
                .map_err(invalid)?,
        }),
    };
    Ok(RuleInput {
        name: raw.name,
        select: raw.select,
        test,
        severity: raw.severity,
        message: raw.message,
        fix: raw.fix,
    })
}

fn invalid(detail: String) -> SchemaError {
    SchemaError::Invalid(detail)
}

/// serde's words for `e`; for an unknown key, followed by `; did you mean <key>?` with the nearest
/// rule key, when one is near.
fn with_near_key(e: serde_yaml_ng::Error) -> String {
    let text = e.to_string();
    let near = text
        .strip_prefix("unknown field `")
        .and_then(|rest| rest.split('`').next())
        .and_then(|key| nearest(key, &RULE_KEYS));
    match near {
        Some(near) => format!("{text}; did you mean {near}?"),
        None => text,
    }
}

/// The bytes of a module's standard base64 `text`; spaces and line breaks in it are ignored. The
/// decoded length is checked against [`MAX_MODULE_BYTES`] before any buffer is allocated.
fn decode_module(name: &str, text: &str) -> Result<Vec<u8>, String> {
    let ignored = |b: &u8| matches!(b, b' ' | b'\n' | b'\r');
    let not_base64 = || format!("rule {name} module: not base64");
    let count = text.bytes().filter(|b| !ignored(b)).count();
    if count % 4 != 0 {
        return Err(not_base64());
    }
    let padding = text
        .bytes()
        .rev()
        .filter(|b| !ignored(b))
        .take(2)
        .filter(|b| *b == b'=')
        .count();
    let decoded = (count / 4 * 3).saturating_sub(padding);
    if decoded > MAX_MODULE_BYTES {
        return Err(format!(
            "rule {name} module is {decoded} bytes; a module takes at most {MAX_MODULE_BYTES}"
        ));
    }
    let mut packed = Vec::with_capacity(count);
    packed.extend(text.bytes().filter(|b| !ignored(b)));
    STANDARD.decode(&packed).map_err(|_| not_base64())
}

/// Why `args` is refused.
enum ArgsRefusal {
    Kind,
    Size,
}

/// A rule's `args`: a mapping of strings, integers from -2^53 to 2^53, booleans, null, lists and
/// mappings with string keys, at most [`MAX_ARGS_BYTES`] as compact JSON. The bytes are counted
/// as the value is read, so a value past the bound stops the read.
fn check_args(name: &str, args: &Value) -> Result<BTreeMap<String, ArgValue>, String> {
    let mut used = 0;
    let read = match args {
        Value::Mapping(_) => arg_value(args, &mut used),
        _ => Err(ArgsRefusal::Kind),
    };
    match read {
        Ok(ArgValue::Map(m)) => Ok(m),
        Ok(_) | Err(ArgsRefusal::Kind) => Err(format!(
            "rule {name} args: only strings, integers, booleans, null, lists and mappings"
        )),
        Err(ArgsRefusal::Size) => Err(format!("rule {name} args: at most {MAX_ARGS_BYTES} bytes")),
    }
}

/// Adds `n` bytes to `used`; Err past [`MAX_ARGS_BYTES`].
fn spend(used: &mut usize, n: usize) -> Result<(), ArgsRefusal> {
    *used = used.saturating_add(n);
    if *used > MAX_ARGS_BYTES {
        return Err(ArgsRefusal::Size);
    }
    Ok(())
}

/// `v` as an args value, its compact JSON bytes added to `used`.
fn arg_value(v: &Value, used: &mut usize) -> Result<ArgValue, ArgsRefusal> {
    Ok(match v {
        Value::Null => {
            spend(used, 4)?;
            ArgValue::Null
        }
        Value::Bool(b) => {
            spend(used, if *b { 4 } else { 5 })?;
            ArgValue::Bool(*b)
        }
        Value::Number(n) => {
            let i = n
                .as_i64()
                .filter(|i| i.unsigned_abs() <= MAX_ARGS_INT.unsigned_abs())
                .ok_or(ArgsRefusal::Kind)?;
            spend(used, decimal_len(i))?;
            ArgValue::Int(i)
        }
        Value::String(text) => {
            spend_str(used, text)?;
            ArgValue::Str(text.clone())
        }
        Value::Sequence(items) => {
            spend(used, 2 + items.len().saturating_sub(1))?;
            let mut list = Vec::with_capacity(items.len());
            for item in items {
                list.push(arg_value(item, used)?);
            }
            ArgValue::List(list)
        }
        Value::Mapping(m) => {
            // braces and commas, and a colon per entry
            spend(used, 2 + m.len().saturating_sub(1) + m.len())?;
            let mut map = BTreeMap::new();
            for (k, item) in m {
                let Value::String(key) = k else {
                    return Err(ArgsRefusal::Kind);
                };
                spend_str(used, key)?;
                map.insert(key.clone(), arg_value(item, used)?);
            }
            ArgValue::Map(map)
        }
        Value::Tagged(_) => return Err(ArgsRefusal::Kind),
    })
}

/// Adds the compact JSON bytes of the string `text` to `used`: its quotes, `\"` and `\\` for a
/// quote and a backslash, `\u00xx` for a control character, else its UTF-8 bytes. A text whose
/// own bytes are past the bound is refused before its characters are read.
fn spend_str(used: &mut usize, text: &str) -> Result<(), ArgsRefusal> {
    spend(used, text.len().saturating_add(2))?;
    let escapes: usize = text
        .chars()
        .map(|c| match c {
            '"' | '\\' => 1,
            c if u32::from(c) < 0x20 => 5,
            _ => 0,
        })
        .sum();
    spend(used, escapes)
}

/// The characters of `i` in decimal.
fn decimal_len(i: i64) -> usize {
    let mut n = i.unsigned_abs();
    let mut len = usize::from(i < 0) + 1;
    while n >= 10 {
        n /= 10;
        len += 1;
    }
    len
}

/// A rule's `depth`: an integer from 0 to [`MAX_DEPTH`].
fn check_depth(name: &str, depth: &Value) -> Result<u8, String> {
    match depth {
        Value::Number(n) => n.as_u64().and_then(|d| u8::try_from(d).ok()),
        _ => None,
    }
    .filter(|d| *d <= MAX_DEPTH)
    .ok_or_else(|| format!("rule {name} depth must be 0 to {MAX_DEPTH}"))
}

/// Whether `text` has 1 to 2000 characters once trimmed, and at most 8000 bytes.
pub(crate) fn text_fits(text: &str) -> bool {
    if text.len() > MAX_TEXT_BYTES {
        return false;
    }
    let trimmed = text.trim();
    !trimmed.is_empty() && trimmed.chars().nth(MAX_TEXT_CHARS).is_none()
}

/// The rules of a resolved `pack`, checked and parsed, in exact name order. Err: the detail of the
/// first refusal, in the pack's order of rules. `PackSet::resolve` runs the same checks, so a
/// pack that resolves always compiles. The checks are [`crate::check_resolved_rules`]'s.
pub fn compile_rules(pack: &Pack) -> Result<Vec<CompiledRule>, String> {
    crate::check_resolved_rules(pack, &pack.rules).map_err(|e| match e {
        SchemaError::Invalid(detail) => detail,
        other => other.to_string(),
    })
}

/// A rule of either kind with its filters parsed: `require` is the parsed filter of a filter
/// rule and `None` for a module rule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledRule {
    pub rule: RuleInput,
    pub select: Filter,
    pub require: Option<Filter>,
}

/// What the resolved-rule checks read of one rule of either kind, borrowed, so a rule is copied
/// only after every check passes.
pub(crate) struct RuleView<'a> {
    pub name: &'a str,
    pub select: &'a str,
    /// The `require` filter of a filter rule; `None` for a module rule.
    pub require: Option<&'a str>,
    /// The decoded bytes of a module rule's module; 0 for a filter rule.
    pub module_bytes: usize,
    pub message: &'a str,
    pub fix: &'a str,
}

impl<'a> From<&'a RuleInput> for RuleView<'a> {
    fn from(rule: &'a RuleInput) -> RuleView<'a> {
        let (require, module_bytes) = match &rule.test {
            RuleTestSpec::Filter { require } => (Some(require.as_str()), 0),
            RuleTestSpec::Module(m) => (None, m.module.len()),
        };
        RuleView {
            name: &rule.name,
            select: &rule.select,
            require,
            module_bytes,
            message: &rule.message,
            fix: &rule.fix,
        }
    }
}

/// The texts of `rule`, then its filters, parsed with `pack`: its `select` and its `require`, if
/// any. Its name is checked first, by [`check_name`].
pub(crate) fn check_rule(
    pack: &Pack,
    rule: &RuleView<'_>,
) -> Result<(Filter, Option<Filter>), String> {
    for (field, text) in [("message", rule.message), ("fix", rule.fix)] {
        if !text_fits(text) {
            return Err(format!(
                "rule {}: {field} must have 1 to {MAX_TEXT_CHARS} characters",
                rule.name
            ));
        }
    }
    let select = compile_filter(pack, rule.name, "select", rule.select)?;
    let require = match rule.require {
        Some(require) => Some(compile_filter(pack, rule.name, "require", require)?),
        None => None,
    };
    Ok((select, require))
}

pub(crate) fn check_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if name.len() > MAX_RULE_NAME_BYTES
        || trimmed.is_empty()
        || trimmed.chars().nth(MAX_RULE_NAME_CHARS).is_some()
    {
        return Err(format!(
            "rule name must have 1 to {MAX_RULE_NAME_CHARS} characters"
        ));
    }
    if trimmed.len() != name.len() {
        return Err("rule name must have no leading or trailing space".to_string());
    }
    Ok(())
}

fn compile_filter(pack: &Pack, name: &str, field: &str, text: &str) -> Result<Filter, String> {
    let filter = parse(text, pack).map_err(|e| format!("rule {name} {field}: {e}"))?;
    if reads_landing_clock(&filter.expr) {
        return Err(format!(
            "rule {name} {field}: a rule cannot read fields.{LANDING_CLOCK_FIELD}, because the landing sets it"
        ));
    }
    Ok(filter)
}

/// Whether a comparison or a `has` reads the field, at any nesting. The name is compared exactly,
/// as the evaluator reads it: another spelling reads no field.
fn reads_landing_clock(expr: &Expr) -> bool {
    let is_clock = |field: &Field| matches!(field, Field::Custom(n) if n == LANDING_CLOCK_FIELD);
    match expr {
        Expr::Or(all) | Expr::And(all) => all.iter().any(reads_landing_clock),
        Expr::Not(inner) => reads_landing_clock(inner),
        Expr::Linked { inner, .. } => inner.as_deref().is_some_and(reads_landing_clock),
        Expr::Compare { field, .. } | Expr::Has(field) => is_clock(field),
    }
}
