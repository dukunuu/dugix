use anyhow::{Result, bail, ensure};
use serde_json::{Map, Value, json};

use super::check::EXPR_OPS;
use super::node::Node;
use super::{MAX_TEXT, MAX_VALUE, data};

pub(super) fn https_url(url: &str) -> Result<()> {
    line_ok(url, MAX_VALUE)?;
    let parsed = reqwest::Url::parse(url)?;
    ensure!(
        parsed.scheme() == "https"
            && parsed.has_host()
            && parsed.username().is_empty()
            && parsed.password().is_none(),
        "OpenUrl requires an HTTPS URL without credentials"
    );
    Ok(())
}

pub(super) fn item_count(count: usize, min: usize, max: usize) -> Result<()> {
    ensure!(
        (min..=max).contains(&count),
        "component needs {min}–{max} items"
    );
    Ok(())
}
pub(super) fn binding_ok(
    state: &Map<String, Value>,
    key: &str,
    valid: impl Fn(&Value) -> bool,
) -> Result<()> {
    let value = state.get(key).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown bound field: {key} — valid: {}",
            state.keys().cloned().collect::<Vec<_>>().join(", ")
        )
    })?;
    if let Some(values) = value.get("random").and_then(Value::as_array) {
        ensure!(
            values.iter().all(valid),
            "invalid bound field type or value: {key}"
        );
    } else {
        ensure!(valid(value), "invalid bound field type or value: {key}");
    }
    Ok(())
}
pub(super) fn numeric_expr(value: &Value, state: &Map<String, Value>) -> bool {
    if !value.is_object() {
        return value.is_number();
    }
    let Ok((op, arg)) = operator(value) else {
        return false;
    };
    match op {
        "get" => arg
            .as_str()
            .is_some_and(|key| binding_ok(state, key, Value::is_number).is_ok()),
        "random" => arg
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_number)),
        "if" => arg
            .as_array()
            .is_some_and(|args| args[1..].iter().all(|v| numeric_expr(v, state))),
        "add" | "sub" | "mul" | "div" | "mod" => arg
            .as_array()
            .is_some_and(|args| args.iter().all(|v| numeric_expr(v, state))),
        "number" | "count" | "sum" | "avg" | "min" | "max" | "round" | "abs" | "floor" | "ceil"
        | "field" | "index" => true,
        _ => false,
    }
}

pub(super) fn identifier(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty()
            && key.len() <= 32
            && key
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'-')),
        "invalid UI identifier"
    );
    Ok(())
}
pub(super) fn text_ok(text: &str, max: usize) -> Result<()> {
    ensure!(
        text.len() <= max && !text.chars().any(|ch| ch.is_control() && ch != '\n'),
        "oversized text or terminal controls in UI"
    );
    Ok(())
}
pub(super) fn line_ok(text: &str, max: usize) -> Result<()> {
    text_ok(text, max)?;
    ensure!(!text.contains('\n'), "expected single-line UI text");
    Ok(())
}
pub(super) fn input_ok(value: &Value, field: &Node) -> Result<()> {
    let text = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("input state must be a string"))?;
    let limit = field.input_limit().unwrap_or(MAX_VALUE);
    if field.is_textarea() {
        text_ok(text, limit)
    } else {
        line_ok(text, limit)
    }
}
pub(super) fn scalar_ok(value: &Value) -> Result<()> {
    match value {
        Value::Null | Value::Bool(_) => Ok(()),
        Value::String(text) => text_ok(text, MAX_TEXT),
        Value::Number(number) => {
            ensure!(
                number
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n.abs() <= 1e12),
                "number exceeds UI budget"
            );
            Ok(())
        }
        _ => bail!("UI state must be scalar"),
    }
}
pub(super) fn spend(budget: &mut usize) -> Result<()> {
    ensure!(*budget > 0, "UI work budget exceeded");
    *budget -= 1;
    Ok(())
}
fn operator(value: &Value) -> Result<(&str, &Value)> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("invalid expression"))?;
    ensure!(obj.len() == 1, "expressions need one operator");
    obj.iter()
        .next()
        .map(|(k, v)| (k.as_str(), v))
        .ok_or_else(|| anyhow::anyhow!("empty expression"))
}
pub(super) fn validate_expr(
    value: &Value,
    state: &Map<String, Value>,
    budget: &mut usize,
    depth: usize,
) -> Result<()> {
    spend(budget)?;
    ensure!(depth <= 12, "UI expression is too deep");
    if let Some(items) = value.as_array() {
        ensure!(items.len() <= 200, "UI array exceeds 200 items");
        for item in items {
            validate_expr(item, state, budget, depth + 1)?;
        }
        return Ok(());
    }
    if !value.is_object() {
        return scalar_ok(value);
    }
    let (op, arg) = operator(value)?;
    match op {
        "get" => ensure!(
            arg.as_str().is_some_and(|key| state.contains_key(key)),
            "unknown expression field — valid: {}",
            state.keys().cloned().collect::<Vec<_>>().join(", ")
        ),
        "random" => {
            let values = arg
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("random needs a choice array"))?;
            ensure!(
                !values.is_empty() && values.len() <= 32,
                "random needs 1–32 scalar choices"
            );
            for value in values {
                scalar_ok(value)?;
            }
        }
        "literal" => data::value_ok(arg, budget, depth + 1)?,
        "object" => {
            let fields = arg
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("object needs fields"))?;
            ensure!(fields.len() <= 200, "object exceeds 200 fields");
            for (key, value) in fields {
                text_ok(key, MAX_VALUE)?;
                validate_expr(value, state, budget, depth + 1)?;
            }
        }
        "field" | "index" => {
            let args = arg
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("access needs arguments"))?;
            ensure!(args.len() == 2, "access needs two arguments");
            for value in args {
                validate_expr(value, state, budget, depth + 1)?;
            }
        }
        "count" | "first" | "last" | "sum" | "avg" | "min" | "max" | "filter" | "sort"
        | "round" | "abs" | "floor" | "ceil" | "append" | "remove" | "replace" | "slice" => {
            let args = arg
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("builtin needs arguments"))?;
            data::arity(op, args.len())?;
            for value in args {
                validate_expr(value, state, budget, depth + 1)?;
            }
        }
        "not" | "number" | "array" => validate_expr(arg, state, budget, depth + 1)?,
        "add" | "sub" | "mul" | "div" | "mod" | "eq" | "lt" | "gt" | "and" | "or" | "if" => {
            let args = arg
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("operator needs an argument array"))?;
            ensure!(
                args.len() == if op == "if" { 3 } else { 2 },
                "wrong expression arity"
            );
            for value in args {
                validate_expr(value, state, budget, depth + 1)?;
            }
        }
        _ => bail!("unsupported UI operator: {op} — valid: {EXPR_OPS}"),
    }
    Ok(())
}
#[allow(clippy::too_many_lines)]
pub(super) fn eval(value: &Value, state: &Map<String, Value>, budget: &mut usize) -> Result<Value> {
    spend(budget)?;
    if let Some(items) = value.as_array() {
        return items
            .iter()
            .map(|value| eval(value, state, budget))
            .collect();
    }
    if !value.is_object() {
        scalar_ok(value)?;
        return Ok(value.clone());
    }
    let (op, arg) = operator(value)?;
    match op {
        "literal" => return Ok(arg.clone()),
        "array" => return eval(arg, state, budget),
        "object" => {
            return arg
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("invalid object"))?
                .iter()
                .map(|(key, value)| Ok((key.clone(), eval(value, state, budget)?)))
                .collect::<Result<Map<_, _>>>()
                .map(Value::Object);
        }
        "field" | "index" => {
            let args = arg
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("invalid access"))?;
            ensure!(args.len() == 2, "invalid access arity");
            let value = eval(&args[0], state, budget)?;
            let key = eval(&args[1], state, budget)?;
            return Ok(if op == "field" {
                data::field(&value, key.as_str().unwrap_or(""))
            } else if let Some(key) = key.as_str() {
                value
                    .as_object()
                    .and_then(|o| o.get(key))
                    .cloned()
                    .unwrap_or(Value::Null)
            } else {
                key.as_f64()
                    .and_then(|index| {
                        value.as_array().and_then(|items| {
                            items.iter().enumerate().find_map(|(i, item)| {
                                (f64::from(u32::try_from(i).ok()?).partial_cmp(&index)
                                    == Some(std::cmp::Ordering::Equal))
                                .then(|| item.clone())
                            })
                        })
                    })
                    .unwrap_or(Value::Null)
            });
        }
        "count" | "first" | "last" | "sum" | "avg" | "min" | "max" | "filter" | "sort"
        | "round" | "abs" | "floor" | "ceil" | "append" | "remove" | "replace" | "slice" => {
            let args = arg
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("invalid builtin arguments"))?
                .iter()
                .map(|v| eval(v, state, budget))
                .collect::<Result<Vec<_>>>()?;
            let value = data::evaluate(op, &args)?;
            data::value_ok(&value, budget, 0)?;
            return Ok(value);
        }
        "get" => {
            return state
                .get(arg.as_str().unwrap_or(""))
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown state field"));
        }
        "random" => {
            let args = arg
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("invalid random choices"))?;
            ensure!(!args.is_empty(), "empty random choices");
            return Ok(args[rand::random_range(0..args.len())].clone());
        }
        "not" => return Ok(json!(!boolean(&eval(arg, state, budget)?)?)),
        "number" => return parse_number(&eval(arg, state, budget)?),
        _ => {}
    }
    let args = arg
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid expression arguments"))?;
    let left = eval(&args[0], state, budget)?;
    if op == "if" {
        return eval(&args[if boolean(&left)? { 1 } else { 2 }], state, budget);
    }
    if op == "and" && !boolean(&left)? {
        return Ok(json!(false));
    }
    if op == "or" && boolean(&left)? {
        return Ok(json!(true));
    }
    let right = eval(&args[1], state, budget)?;
    let result = match op {
        "eq" => json!(if left.is_number() && right.is_number() {
            number(&left)?
                .partial_cmp(&number(&right)?)
                .is_some_and(std::cmp::Ordering::is_eq)
        } else {
            left == right
        }),
        "and" => json!(boolean(&left)? && boolean(&right)?),
        "or" => json!(boolean(&left)? || boolean(&right)?),
        "lt" => json!(number(&left)? < number(&right)?),
        "gt" => json!(number(&left)? > number(&right)?),
        "add" => json!(number(&left)? + number(&right)?),
        "sub" => json!(number(&left)? - number(&right)?),
        "mul" => json!(number(&left)? * number(&right)?),
        "div" | "mod" => {
            let divisor = number(&right)?;
            ensure!(
                divisor.classify() != std::num::FpCategory::Zero,
                "division by zero"
            );
            json!(if op == "div" {
                number(&left)? / divisor
            } else {
                number(&left)? % divisor
            })
        }
        _ => bail!("unsupported operator"),
    };
    ensure!(!result.is_null(), "non-finite UI arithmetic");
    scalar_ok(&result)?;
    Ok(result)
}
fn parse_number(value: &Value) -> Result<Value> {
    let parsed = if let Some(text) = value.as_str() {
        text.trim().parse::<f64>()?
    } else {
        number(value)?
    };
    let result = json!(parsed);
    ensure!(!result.is_null(), "expected a finite number");
    scalar_ok(&result)?;
    Ok(result)
}
pub(super) fn number(value: &Value) -> Result<f64> {
    value
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("expected number"))
}
pub(super) fn boolean(value: &Value) -> Result<bool> {
    value
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("expected boolean"))
}
/// How a scalar (or any data value) reads as text in a panel.
#[must_use]
pub fn scalar_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => value.to_string().trim_end_matches(".0").to_string(),
    }
}
pub(super) fn expand(template: &str, state: &Map<String, Value>) -> Result<String> {
    let mut result = String::new();
    let mut rest = template;
    while let Some((head, tail)) = rest.split_once("{{") {
        let (key, after) = tail
            .split_once("}}")
            .ok_or_else(|| anyhow::anyhow!("unclosed template"))?;
        result.push_str(head);
        if let Some(value) = state.get(key.trim()) {
            result.push_str(&scalar_text(value));
        }
        ensure!(result.len() <= MAX_TEXT, "expanded UI text exceeds budget");
        rest = after;
    }
    result.push_str(rest);
    ensure!(result.len() <= MAX_TEXT, "expanded UI text exceeds budget");
    Ok(result)
}
