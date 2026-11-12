use std::collections::BTreeMap;

use anyhow::Result;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::node::{
    Accent, BarItem, Button, CalloutTone, Card, CheckItem, Column, ColumnItem, DisplayStep, Frame,
    Mark, Node, Stat, StepState, Tone,
};
use super::{ChartKind, Timer, live};

pub(super) fn bounded_rows(count: usize) -> u16 {
    u16::try_from(count).unwrap_or(u16::MAX)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Step {
    Set {
        key: String,
        value: Value,
    },
    Call {
        action: String,
    },
    Run {
        id: String,
    },
    Reset {
        keys: Vec<String>,
    },
    OpenUrl {
        url: Value,
    },
    Send {
        text: String,
    },
    ToAssistant {
        text: String,
        #[serde(default)]
        context: Option<Value>,
    },
    /// The clicked button's label is sent; a model-supplied label is ignored.
    Answer {
        #[serde(default)]
        #[allow(dead_code)]
        label: Option<Value>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Definition {
    #[serde(default)]
    pub(super) calls: Vec<live::Definition>,
    pub(super) version: u8,
    pub(super) title: String,
    #[serde(default)]
    pub(super) frame: Frame,
    #[serde(default)]
    pub(super) accent: Accent,
    #[serde(default)]
    pub(super) state: Map<String, Value>,
    pub(super) view: Vec<Node>,
    #[serde(default)]
    pub(super) actions: BTreeMap<String, Vec<Step>>,
    #[serde(default)]
    pub(super) timers: Vec<Timer>,
    #[serde(default)]
    pub(super) keys: BTreeMap<String, String>,
}

/// A located problem in a model-authored UI document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckError {
    /// JSON location, or `$` for the entire source.
    pub path: String,
    /// Explanation including valid alternatives for unknown constructs.
    pub message: String,
}

/// Valid presentation node types, shared by validation and tool reports.
pub const NODE_TYPES: &str = "hero, image, columns, cards, stats, steps, callout, quote, divider, text, buttons, input, kv, table, progress, bars, sparkline, checklist, select, stepper, toggle, tabs, accordion, row, grid, stack, spacer, chart, canvas, number, gauge, heatmap, md, tree, board, diagram, form, timeline, rating, code, badge, meter, slider, editor, choice, quiz";
const ACTION_OPS: &str = "set, call, run, send, answer, reset, open_url, to_assistant";
pub(super) const EXPR_OPS: &str = "get, random, not, number, if, add, sub, mul, div, mod, eq, lt, gt, and, or, literal, object, array, field, index, count, first, last, sum, avg, min, max, filter, sort, round, abs, floor, ceil, append, remove, replace, slice";
const MAX_ERRORS: usize = 10;

pub(super) fn check_result(errors: &mut Vec<CheckError>, path: &str, result: Result<()>) {
    if let Err(error) = result
        && errors.len() < MAX_ERRORS
    {
        errors.push(CheckError {
            path: path.into(),
            message: error.to_string(),
        });
    }
}

// Check fields independently so serde's first failure doesn't hide later ones.
// Optional fields use `?`; their defaults/requiredness remain owned by serde.
#[allow(clippy::too_many_lines)]
pub(super) fn check_shape(value: &Value, path: &str, shape: &str, errors: &mut Vec<CheckError>) {
    if errors.len() >= MAX_ERRORS {
        return;
    }
    if let Some(inner) = shape.strip_prefix('?') {
        if !value.is_null() {
            check_shape(value, path, inner, errors);
        }
        return;
    }
    if let Some(inner) = shape.strip_suffix("[]") {
        if let Some(items) = value.as_array() {
            for (index, item) in items.iter().enumerate() {
                check_shape(item, &format!("{path}[{index}]"), inner, errors);
            }
        } else {
            check_result(
                errors,
                path,
                Err(anyhow::anyhow!("expected an array of {inner}")),
            );
        }
        return;
    }
    macro_rules! typed {
        ($ty:ty) => {{
            check_result(
                errors,
                path,
                serde_json::from_value::<$ty>(value.clone())
                    .map(|_| ())
                    .map_err(Into::into),
            );
            return;
        }};
    }
    match shape {
        "string" => typed!(String),
        "bool" => typed!(bool),
        "u8" => typed!(u8),
        "u16" => typed!(u16),
        "number" => typed!(f64),
        "tone" => typed!(Tone),
        "chart_kind" => typed!(ChartKind),
        "frame" => typed!(Frame),
        "accent" => typed!(Accent),
        "mark" => typed!(Mark),
        "step_state" => typed!(StepState),
        "callout_tone" => typed!(CalloutTone),
        "pair" => typed!([String; 2]),
        "value" => return,
        _ => {}
    }
    let Some(object) = value.as_object() else {
        check_result(errors, path, Err(anyhow::anyhow!("expected an object")));
        return;
    };
    let (owner, fields) = match shape {
        "document" => (
            shape,
            "version:u8 title:string frame:frame accent:accent state:state view:node[] actions:actions calls:value timers:value keys:value",
        ),
        "state" | "actions" => {
            for (key, value) in object {
                check_shape(
                    value,
                    &format!("{path}.{key}"),
                    if shape == "state" { "value" } else { "step[]" },
                    errors,
                );
            }
            return;
        }
        "node" | "step" => {
            let (tag, valid, kind) = if shape == "node" {
                ("type", NODE_TYPES, "node type")
            } else {
                ("op", ACTION_OPS, "action op")
            };
            let Some(name) = object.get(tag).and_then(Value::as_str) else {
                check_result(
                    errors,
                    &format!("{path}.{tag}"),
                    Err(anyhow::anyhow!("expected {tag} string — valid: {valid}")),
                );
                return;
            };
            let fields = match (shape, name) {
                ("node", "hero") => {
                    "title:string subtitle:?string badge:?string image:?string alt:?string"
                }
                ("node", "image") => "src:string alt:?string rows:u16",
                ("node", "columns") => "items:column[]",
                ("node", "cards") => "items:card[]",
                ("node", "stats") => "items:stat[] big:bool",
                ("node", "steps") => "items:display_step[]",
                ("node", "callout") => "tone:callout_tone title:?string text:string",
                ("node", "quote") => "text:string by:?string",
                ("node", "divider") => "label:?string",
                ("node", "text") => "text:string tone:tone rows:u16",
                ("node", "buttons") => "items:button[] rows:u16",
                ("node", "input") => {
                    "key:string label:string rules:value placeholder:string hint:string rows:u16"
                }
                ("node", "toggle") => "key:string label:string",
                ("node", "kv") => "items:pair[]",
                ("node", "table") => {
                    "columns:string[] rows_data:string[][] rows:string[][] select:?string sortable:bool page_size:?u16 search:bool"
                }
                ("node", "progress") => "label:string value:value max:value",
                ("node", "bars") => "items:bar[]",
                ("node", "sparkline") => "values:value[] rows:u16",
                ("node", "checklist") => "items:check[]",
                ("node", "select") => "key:string label:string options:string[]",
                ("node", "stepper" | "slider") => {
                    "key:string label:string min:number max:number step:number"
                }
                ("node", "tabs") => "items:tab[] key:?string",
                ("node", "accordion") => "items:section[]",
                ("node", "chart") => "kind:chart_kind series:series[] labels:string[] rows:u16",
                ("node", "canvas") => "shapes:value rows:u16 alt:string",
                ("node", "row" | "stack") => "items:node[] gap:u16",
                ("node", "number") => "value:string label:string delta:?string tone:?tone",
                ("node", "heatmap") => "values:value labels:string[] tone:tone",
                ("node", "md") => "text:string rows:u16",
                ("node", "tree") => "items:value",
                ("node", "board") => "items:value rows:u16",
                ("node", "diagram") => "source:string alt:string",
                ("node", "grid") => "items:node[] cols:u16",
                ("node", "spacer") => "rows:u16",
                ("node", "form") => "title:string fields:node[] submit_label:string action:string",
                ("node", "timeline") => "items:event[]",
                ("node", "rating") => "key:string label:string max:u16",
                ("node", "code") => "source:string lang:?string",
                ("node", "badge") => "text:string tone:tone",
                ("node", "meter" | "gauge") => "label:string value:value max:value tone:tone",
                ("node", "editor") => "key:string label:string lang:?string rows:u16",
                ("node", "choice") => "key:string label:string options:string[] answer:?string",
                ("node", "quiz") => "items:question[] key:?string",
                ("step", "set") => "key:string value:value",
                ("step", "call") => "action:string",
                ("step", "run") => "id:string",
                ("step", "send") => "text:string",
                ("step", "to_assistant") => "text:string context:value",
                ("step", "answer") => "",
                ("step", "reset") => "keys:string[]",
                ("step", "open_url") => "url:value",
                _ => {
                    check_result(
                        errors,
                        path,
                        Err(anyhow::anyhow!("unknown {kind} {name:?} — valid: {valid}")),
                    );
                    return;
                }
            };
            (name, fields)
        }
        "button" => (
            shape,
            "label:string action:string body:string[] primary:bool image:?string alt:?string",
        ),
        "column" => (shape, "title:string items:column_item[]"),
        "column_item" => (shape, "text:string mark:mark"),
        "card" => (
            shape,
            "title:string text:string badge:?string tone:?tone image:?string alt:?string",
        ),
        "stat" => (shape, "value:string label:string delta:?string tone:?tone"),
        "display_step" => (shape, "title:string text:?string state:step_state"),
        "tab" => (shape, "label:string view:node[]"),
        "section" => (shape, "title:string view:node[] open:bool"),
        "question" => (
            shape,
            "title:string view:node[] answer:?string explain:?string",
        ),
        "series" => (shape, "name:string values:value[] tone:tone"),
        "event" => (shape, "time:string title:string text:?string tone:tone"),
        "bar" => (shape, "label:string value:value"),
        "check" => (shape, "label:string key:string"),
        _ => unreachable!("known schema shape"),
    };
    let fields: Vec<_> = fields
        .split_whitespace()
        .filter_map(|field| field.split_once(':'))
        .collect();
    for (key, value) in object {
        if (shape == "node" && key == "type") || (shape == "step" && key == "op") {
            continue;
        }
        if let Some((_, field_shape)) = fields.iter().find(|(name, _)| name == key) {
            check_shape(value, &format!("{path}.{key}"), field_shape, errors);
        } else {
            let alternatives = if fields.is_empty() {
                format!("{owner} takes no fields")
            } else {
                format!(
                    "valid: {}",
                    fields
                        .iter()
                        .map(|(name, _)| *name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            check_result(
                errors,
                path,
                Err(anyhow::anyhow!(
                    "unknown field {key:?} on {owner} — {alternatives}"
                )),
            );
        }
    }
    let required = match owner {
        "document" => "version title view",
        "hero" | "column" => {
            if owner == "hero" {
                "title"
            } else {
                "title items"
            }
        }
        "columns" | "cards" | "stats" | "steps" | "buttons" | "kv" | "bars" | "checklist"
        | "tabs" | "accordion" | "timeline" | "quiz" | "row" | "grid" | "stack" | "tree"
        | "board" => "items",
        "callout" => "tone text",
        "image" => "src",
        "quote" | "text" | "column_item" | "send" | "badge" | "to_assistant" | "md" => "text",
        "input" | "toggle" | "check" | "rating" | "editor" => "key label",
        "table" => "columns rows_data",
        "progress" | "bar" | "meter" | "gauge" => "label value",
        "sparkline" | "heatmap" => "values",
        "select" | "choice" => "key label options",
        "stepper" | "slider" => "key label min max step",
        "tab" => "label view",
        "section" => "title view",
        "chart" => "kind series",
        "canvas" => "shapes",
        "series" => "name values",
        "event" => "time title",
        "form" => "title fields submit_label action",
        "code" | "diagram" => "source",
        "set" => "key value",
        "call" => "action",
        "run" => "id",
        "reset" => "keys",
        "open_url" => "url",
        "button" => "label action",
        "card" => "title text",
        "stat" | "number" => "value label",
        "display_step" | "question" => "title",
        _ => "",
    };
    for field in required.split_whitespace() {
        if !(object.contains_key(field)
            || owner == "table" && field == "rows_data" && object.contains_key("rows"))
        {
            check_result(
                errors,
                &format!("{path}.{field}"),
                Err(anyhow::anyhow!("missing required field {field:?}")),
            );
        }
    }
    // Deserialize every node/step independently, preserving serde's schema as
    // the authority if a future field is added without a diagnostic shape.
    let result = match shape {
        "document" => serde_json::from_value::<Definition>(value.clone()).map(|_| ()),
        "node" => serde_json::from_value::<Node>(value.clone()).map(|_| ()),
        "step" => serde_json::from_value::<Step>(value.clone()).map(|_| ()),
        "button" => serde_json::from_value::<Button>(value.clone()).map(|_| ()),
        "column" => serde_json::from_value::<Column>(value.clone()).map(|_| ()),
        "column_item" => serde_json::from_value::<ColumnItem>(value.clone()).map(|_| ()),
        "card" => serde_json::from_value::<Card>(value.clone()).map(|_| ()),
        "stat" => serde_json::from_value::<Stat>(value.clone()).map(|_| ()),
        "display_step" => serde_json::from_value::<DisplayStep>(value.clone()).map(|_| ()),
        "bar" => serde_json::from_value::<BarItem>(value.clone()).map(|_| ()),
        "check" => serde_json::from_value::<CheckItem>(value.clone()).map(|_| ()),
        _ => return,
    };
    if let Err(error) = result {
        let message = error.to_string();
        if !errors.iter().any(|error| error.path.starts_with(path)) {
            let location = message
                .strip_prefix("missing field `")
                .and_then(|s| s.strip_suffix('`'))
                .map_or_else(|| path.to_string(), |field| format!("{path}.{field}"));
            check_result(errors, &location, Err(error.into()));
        }
    }
}
