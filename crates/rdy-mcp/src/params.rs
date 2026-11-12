//! mparm1: MCP tool parameters, parsed so that a bad parameter is an rdy error.
//!
//! rmcp's own `Parameters<P>` turns a deserialize failure into a protocol error whose text is
//! serde's line, with no parameter name. This extractor never fails: it hands the tool a
//! `Result`, and a failure is an `E_BAD_INPUT` that names the parameter and the shape the tool's
//! published input schema asks for. The rmcp macro finds the input schema by the identifier
//! `Parameters`, so this type keeps that name.
use rdy_engine::did_you_mean::{nearest, unknown_field_error};
use rdy_engine::{E_BAD_INPUT, RdyError};
use rmcp::handler::server::common::{FromContextPart, schema_for_input};
use rmcp::handler::server::tool::ToolCallContext;
use rmcp::model::JsonObject;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// A tool's parameters, or the refusal for them. `tools/list` shows `P`'s schema unchanged.
#[derive(Debug, Clone)]
pub struct Parameters<P>(pub Result<P, RdyError>);

impl<P: JsonSchema> JsonSchema for Parameters<P> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        P::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        P::json_schema(generator)
    }
}

impl<S, P> FromContextPart<ToolCallContext<'_, S>> for Parameters<P>
where
    P: DeserializeOwned + JsonSchema + 'static,
{
    /// Never `Err`: a bad parameter is the tool's own error reply, not a protocol error.
    fn from_context_part(context: &mut ToolCallContext<'_, S>) -> Result<Self, rmcp::ErrorData> {
        let args = context.arguments.take().unwrap_or_default();
        Ok(Parameters(parse_params::<P>(&context.name, args)))
    }
}

/// The name of the undeclared argument that carries a ruleset's value (FW-MH.kungfu, D-Y18E1).
/// No parameter struct and no schema declares it; the extractor removes it first.
const KUNGFU: &str = "kungfu";

/// What a call's `kungfu` argument was, judged against a ruleset's current value. The supplied
/// value is not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kungfu {
    /// The call has no top-level `kungfu` key.
    Missing,
    /// The key is present with a value that is not exactly the expected string: null, a scalar, a
    /// list, an object, or a string of any other bytes.
    Wrong,
    /// The key is a string that equals the expected value.
    Correct,
}

/// Removes the top-level `kungfu` key from `args` and classifies it against `expected`. Only that
/// exact key goes; every other key and value stays, and a nested `kungfu` is left to its own
/// validator. With no `expected` (the tool is exempt or unknown), a present key is `Wrong`. The
/// length is checked before any byte is compared, so a long value costs nothing more.
pub fn take_kungfu(args: &mut JsonObject, expected: Option<&str>) -> Kungfu {
    match args.remove(KUNGFU) {
        None => Kungfu::Missing,
        Some(Value::String(s)) => match expected {
            Some(e) if s.len() == e.len() && s.as_bytes() == e.as_bytes() => Kungfu::Correct,
            _ => Kungfu::Wrong,
        },
        Some(_) => Kungfu::Wrong,
    }
}

/// Parses `args` as `P` after removing the undeclared top-level `kungfu` argument. Use this for
/// tool arguments; [`parse_params_strict`] is for internal objects.
pub fn parse_params<P: DeserializeOwned + JsonSchema + 'static>(
    tool: &str,
    mut args: JsonObject,
) -> Result<P, RdyError> {
    args.remove(KUNGFU);
    parse_params_strict(tool, args)
}

/// Parses `args` as `P` and removes nothing: every key must be one of `P`'s. The request's
/// `_meta` and other internal objects use this. On failure, the arguments are checked against `P`'s input schema to
/// name the parameter and its shape; if the schema accepts them (an overflow, say), the refusal
/// carries serde's detail instead.
pub fn parse_params_strict<P: DeserializeOwned + JsonSchema + 'static>(
    tool: &str,
    args: JsonObject,
) -> Result<P, RdyError> {
    let value = Value::Object(args);
    let e = match P::deserialize(&value) {
        Ok(p) => return Ok(p),
        Err(e) => e,
    };
    let miss = schema_for_input::<P>().ok().and_then(|s| {
        let schema = Value::Object((*s).clone());
        locate(&value, &schema, &schema, "")
    });
    Err(match miss {
        Some(Miss::Wrong {
            path,
            expected,
            actual,
        }) => wrong_type(tool, &path, &expected, &actual),
        Some(Miss::Missing { path, expected }) => RdyError::new(
            E_BAD_INPUT,
            format!("{tool}: parameter \"{path}\" is required"),
            format!("pass \"{path}\" as {expected}"),
        ),
        Some(Miss::Unknown { path, near, known }) => {
            let all = known.join(", ");
            let fix = match (near, known.is_empty()) {
                (Some(y), _) => format!("did you mean \"{y}\"? the parameters here are {all}"),
                (None, true) => format!("remove \"{path}\"; {tool} takes no parameters"),
                (None, false) => format!("remove \"{path}\"; the parameters here are {all}"),
            };
            RdyError::new(
                E_BAD_INPUT,
                format!("{tool}: unknown parameter \"{path}\""),
                fix,
            )
        }
        None => unknown_field_error(tool, &e.to_string()).unwrap_or_else(|| {
            RdyError::new(
                E_BAD_INPUT,
                format!("{tool}: the parameters do not parse: {e}"),
                "check each parameter against the tool's input schema (tools/list)",
            )
        }),
    })
}

/// The refusal of a wrong-typed parameter, in the words every tool gives (mparm1).
fn wrong_type(tool: &str, path: &str, expected: &str, actual: &str) -> RdyError {
    RdyError::new(
        E_BAD_INPUT,
        format!("{tool}: parameter \"{path}\" must be {expected}, not {actual}"),
        format!("pass \"{path}\" as {expected}"),
    )
}

/// The wrong-typed parameter refusal for the first top-level argument of `args` whose JSON type
/// `P`'s input schema does not accept, or `None`. Only the type is checked: a key outside the
/// schema, a missing key, and a value of an accepted type (an enum choice, a list's items, a
/// bound) are left to the tool's own parser. A tool whose engine parser reads its raw arguments
/// (rdy_hook) calls this first, so a wrong type is refused as every tool refuses it.
pub fn wrong_typed<P: JsonSchema + 'static>(tool: &str, args: &JsonObject) -> Option<RdyError> {
    let schema = Value::Object((*schema_for_input::<P>().ok()?).clone());
    let props = schema.get("properties")?.as_object()?;
    args.iter().find_map(|(key, value)| {
        let prop = props.get(key)?;
        let types = json_types(prop, &schema);
        (!types.is_empty() && !types.iter().any(|t| has_type(value, t)))
            .then(|| wrong_type(tool, key, &shape(prop, &schema), &actual(value)))
    })
}

/// The JSON types `schema` accepts: its own `type`, or those of its `anyOf` or `oneOf` branches,
/// through `$ref`s. Empty when it, or one of its branches, names none (any type fits).
fn json_types(schema: &Value, root: &Value) -> Vec<String> {
    if let Some(def) = deref(schema, root) {
        return json_types(def, root);
    }
    if let Some(bs) = branches(schema) {
        let mut all = vec![];
        for b in bs {
            let types = json_types(b, root);
            if types.is_empty() {
                return vec![];
            }
            all.extend(types);
        }
        return all;
    }
    match schema.get("type") {
        Some(Value::String(t)) => vec![t.clone()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => vec![],
    }
}

/// The first place where a value does not fit the schema.
enum Miss {
    Wrong {
        path: String,
        expected: String,
        actual: String,
    },
    Missing {
        path: String,
        expected: String,
    },
    /// A key outside the properties of a closed object (`additionalProperties: false`).
    Unknown {
        path: String,
        near: Option<String>,
        known: Vec<String>,
    },
}

/// The definition a `#/$defs/<Name>` reference names, if it resolves.
fn deref<'a>(schema: &Value, root: &'a Value) -> Option<&'a Value> {
    let name = schema.get("$ref")?.as_str()?.strip_prefix("#/$defs/")?;
    root.get("$defs")?.get(name)
}

/// The `anyOf` or `oneOf` branches, if the schema has them.
fn branches(schema: &Value) -> Option<&Vec<Value>> {
    schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
}

fn is_null_branch(b: &Value) -> bool {
    b.get("type").and_then(Value::as_str) == Some("null")
}

/// `items` that constrains the elements (not missing, `true` or `{}`).
fn item_schema(schema: &Value) -> Option<&Value> {
    schema
        .get("items")
        .filter(|i| i.as_object().is_some_and(|o| !o.is_empty()))
}

/// The expected shape of a value, in words.
pub fn shape(schema: &Value, root: &Value) -> String {
    if let Some(def) = deref(schema, root) {
        return shape(def, root);
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let values: Vec<String> = values
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect();
        return format!("one of {}", values.join(", "));
    }
    if let Some(bs) = branches(schema) {
        let shapes: Vec<String> = bs
            .iter()
            .filter(|b| !is_null_branch(b))
            .map(|b| shape(b, root))
            .collect();
        if !shapes.is_empty() {
            return shapes.join(" or ");
        }
    }
    let types: Vec<&str> = match schema.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| *t != "null")
            .collect(),
        _ => vec![],
    };
    if types.is_empty() {
        return "any value".into();
    }
    let words: Vec<String> = types
        .iter()
        .map(|t| match *t {
            "string" => "a string".into(),
            "boolean" => "a boolean".into(),
            "number" => "a number".into(),
            "integer" => {
                let non_negative = schema
                    .get("minimum")
                    .and_then(Value::as_f64)
                    .is_some_and(|m| m >= 0.0);
                if non_negative {
                    "a non-negative integer".into()
                } else {
                    "an integer".into()
                }
            }
            "object" => "an object".into(),
            "array" => match item_schema(schema) {
                Some(items) => format!("a list of {}", plural(&shape(items, root))),
                None => "a list".into(),
            },
            "null" => "null".into(),
            other => other.to_string(),
        })
        .collect();
    words.join(" or ")
}

/// The plural of a shape: `a string` → `strings`, `a list of strings` → `lists of strings`.
fn plural(s: &str) -> String {
    if s.starts_with("one of ") {
        return format!("values, each {s}");
    }
    let rest = s
        .strip_prefix("a ")
        .or_else(|| s.strip_prefix("an "))
        .unwrap_or(s);
    match rest.find(" of ") {
        Some(i) => format!("{}s{}", &rest[..i], &rest[i..]),
        None => format!("{rest}s"),
    }
}

/// What a value is, in words.
fn actual(value: &Value) -> String {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
    .into()
}

fn has_type(value: &Value, t: &str) -> bool {
    match t {
        "null" => value.is_null(),
        "boolean" => value.is_boolean(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "string" => value.is_string(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_string()
    } else {
        format!("{path}.{key}")
    }
}

/// The first miss of `value` against `schema` (`root` holds `$defs`), at `path`.
fn locate(value: &Value, schema: &Value, root: &Value, path: &str) -> Option<Miss> {
    let wrong = |actual: String| Miss::Wrong {
        path: path.to_string(),
        expected: shape(schema, root),
        actual,
    };
    if let Some(def) = deref(schema, root) {
        return locate(value, def, root, path);
    }
    if let Some(bs) = branches(schema) {
        if value.is_null() && bs.iter().any(is_null_branch) {
            return None;
        }
        let non_null: Vec<&Value> = bs.iter().filter(|b| !is_null_branch(b)).collect();
        if let [only] = non_null.as_slice() {
            return locate(value, only, root, path);
        }
        if bs.iter().any(|b| locate(value, b, root, path).is_none()) {
            return None;
        }
        return Some(wrong(actual(value)));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.contains(value)
    {
        return Some(wrong(actual(value)));
    }
    let types: Option<Vec<&str>> = match schema.get("type") {
        Some(Value::String(t)) => Some(vec![t.as_str()]),
        Some(Value::Array(ts)) => Some(ts.iter().filter_map(Value::as_str).collect()),
        _ => None,
    };
    if let Some(types) = types {
        if !types.iter().any(|t| has_type(value, t)) {
            return Some(wrong(actual(value)));
        }
        if let (Some(n), Some(min)) = (
            value.as_f64(),
            schema.get("minimum").and_then(Value::as_f64),
        ) && n < min
        {
            return Some(wrong(value.to_string()));
        }
    }
    if let (Some(items), Some(elements)) = (item_schema(schema), value.as_array()) {
        for (i, v) in elements.iter().enumerate() {
            if let Some(m) = locate(v, items, root, &format!("{path}[{i}]")) {
                return Some(m);
            }
        }
    }
    if schema.get("additionalProperties") == Some(&Value::Bool(false))
        && let Some(obj) = value.as_object()
    {
        let known: Vec<String> = schema
            .get("properties")
            .and_then(Value::as_object)
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default();
        if let Some(key) = obj.keys().find(|k| !known.contains(k)) {
            let names: Vec<&str> = known.iter().map(String::as_str).collect();
            return Some(Miss::Unknown {
                path: join(path, key),
                near: nearest(key, &names).map(str::to_string),
                known,
            });
        }
    }
    if let (Some(props), Some(obj)) = (
        schema.get("properties").and_then(Value::as_object),
        value.as_object(),
    ) {
        for (key, v) in obj {
            if let Some(prop) = props.get(key)
                && let Some(m) = locate(v, prop, root, &join(path, key))
            {
                return Some(m);
            }
        }
        let required = schema.get("required").and_then(Value::as_array);
        for name in required.into_iter().flatten().filter_map(Value::as_str) {
            if !obj.contains_key(name) {
                return Some(Miss::Missing {
                    path: join(path, name),
                    expected: props
                        .get(name)
                        .map_or_else(|| "any value".into(), |p| shape(p, root)),
                });
            }
        }
    }
    None
}
