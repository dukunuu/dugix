use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Result, ensure};
use serde_json::{Map, Value, json};

use super::check::{CheckError, Definition, Step, check_result, check_shape};
use super::expr::{
    binding_ok, https_url, identifier, input_ok, item_count, line_ok, numeric_expr, scalar_ok,
    spend, text_ok, validate_expr,
};
use super::node::{Accent, Frame, Node, image_ok};
use super::session::Session;
use super::{
    MAX_SOURCE, MAX_TEXT, MAX_VALUE, ROWS, Timer, components, data, extras, live, stream,
    validation,
};

/// A validated version-1 UI document. Construction is through `parse` or `check`.
#[derive(Clone, Debug)]
pub struct Document {
    pub(super) calls: Vec<live::Definition>,
    version: u8,
    pub(super) title: String,
    frame: Frame,
    accent: Accent,
    pub(super) state: Map<String, Value>,
    pub(super) view: Vec<Node>,
    pub(super) actions: BTreeMap<String, Vec<Step>>,
    pub(super) timers: Vec<Timer>,
    pub(super) keys: BTreeMap<String, String>,
    sending: BTreeSet<String>,
    pub(super) answering: BTreeSet<String>,
    pub(super) program: Option<String>,
    /// The program had broken pieces that are left out of this document.
    pub(super) salvaged: bool,
}

impl Document {
    /// Parse and validate all controls, expressions and action paths.
    /// # Errors
    /// Rejects unknown constructs, oversized data, cycles and invalid bindings.
    pub fn parse(source: &str) -> Result<Self> {
        Self::check(source).map_err(|errors| {
            anyhow::anyhow!(
                errors
                    .iter()
                    .map(|error| format!("{}: {}", error.path, error.message))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
    }

    /// Image URLs with their authored JSON paths, in document order.
    #[must_use]
    pub fn images(&self) -> Vec<(String, &str)> {
        let mut images = Vec::new();
        for (index, node) in self.view.iter().enumerate() {
            let path = format!("view[{index}]");
            match node {
                Node::Image { src, .. } => images.push((format!("{path}.src"), src.as_str())),
                Node::Hero {
                    image: Some(src), ..
                } => {
                    images.push((format!("{path}.image"), src.as_str()));
                }
                Node::Cards { items } => {
                    for (index, item) in items.iter().enumerate() {
                        if let Some(src) = &item.image {
                            images.push((format!("{path}.items[{index}].image"), src.as_str()));
                        }
                    }
                }
                Node::Buttons { items, .. } => {
                    for (index, item) in items.iter().enumerate() {
                        if let Some(src) = &item.image {
                            images.push((format!("{path}.items[{index}].image"), src.as_str()));
                        }
                    }
                }
                _ => {}
            }
        }
        images
    }

    /// Check authored JSON, reporting up to ten located errors without executing actions.
    /// # Errors
    /// Returns syntax, schema, binding or budget errors.
    pub fn check(source: &str) -> std::result::Result<Self, Vec<CheckError>> {
        let mut errors = Vec::new();
        check_result(
            &mut errors,
            "$",
            (|| {
                ensure!(source.len() <= MAX_SOURCE, "UI document exceeds 16 KiB");
                Ok(())
            })(),
        );
        if !errors.is_empty() {
            return Err(errors);
        }
        let mut document = Self::check_value(stream::decode(source)?)?;
        if !source.trim_start().starts_with('{') {
            document.program = Some(source.into());
        }
        Ok(document)
    }

    /// A finished program's document, leaving out its broken pieces the way
    /// `OpenUI` does: a component, list item or statement that fails is dropped
    /// and the rest renders. Returns the problems that caused anything to be
    /// left out (empty when the program is valid).
    /// # Errors
    /// Returns the check errors when nothing usable remains: no valid root, no
    /// surviving element, or a legacy JSON document.
    pub fn recover(source: &str) -> std::result::Result<(Self, Vec<CheckError>), Vec<CheckError>> {
        let errors = match Self::check(source) {
            Ok(document) => return Ok((document, Vec::new())),
            Err(errors) => errors,
        };
        if source.len() > MAX_SOURCE || source.trim_start().starts_with('{') {
            return Err(errors);
        }
        let program = stream::parseable(source);
        let salvaged = stream::program_state(&program, None, None, true)
            .ok()
            .and_then(|value| Self::check_value(value).ok());
        match salvaged {
            Some(mut document) if !document.view.is_empty() => {
                document.program = Some(program);
                document.salvaged = true;
                Ok((document, errors))
            }
            _ => Err(errors),
        }
    }

    /// The inert document a finished block displays, broken pieces left out;
    /// what layout measures before the live session exists.
    #[must_use]
    pub fn settled(source: &str) -> Option<Self> {
        Self::recover(source)
            .ok()
            .map(|(document, _)| document.inert_preview())
    }

    /// Whether broken pieces of this document's program are left out.
    #[must_use]
    pub const fn is_salvaged(&self) -> bool {
        self.salvaged
    }

    /// Validated, inert snapshot of the complete elements received so far.
    /// An unfinished or invalid element never enters the preview. Random initial
    /// state uses its first choice until the final document is initialized.
    #[must_use]
    pub fn preview(source: &str) -> Option<Self> {
        if source.len() > MAX_SOURCE {
            return None;
        }
        Self::check(source)
            .ok()
            .or_else(|| stream::preview(source))
            .map(Self::inert_preview)
    }

    pub(super) fn inert_preview(mut self) -> Self {
        for value in self.state.values_mut() {
            if let Some(first) = value
                .get("random")
                .and_then(Value::as_array)
                .and_then(|v| v.first())
            {
                *value = first.clone();
            }
        }
        self
    }

    pub(super) fn check_value(mut value: Value) -> std::result::Result<Self, Vec<CheckError>> {
        let mut errors = Vec::new();
        if let Some(actions) = value.get_mut("actions").and_then(Value::as_object_mut) {
            for steps in actions.values_mut().filter_map(Value::as_array_mut) {
                for step in steps {
                    if step.get("op").and_then(Value::as_str) == Some("answer")
                        && let Some(object) = step.as_object_mut()
                    {
                        object.remove("label");
                    }
                }
            }
        }
        check_shape(&value, "$", "document", &mut errors);
        // Top-level fields use their natural JSON paths, without a `$` prefix.
        for error in &mut errors {
            if let Some(path) = error.path.strip_prefix("$.") {
                error.path = path.into();
            }
        }
        // Keep valid elements available for semantic checks even when another
        // element has a schema error. Neutral placeholders preserve JSON indices;
        // they can never escape because the collected errors reject the document.
        let definition =
            serde_json::from_value::<Definition>(value.clone()).unwrap_or_else(|_| Definition {
                calls: Vec::new(),
                version: serde_json::from_value(value["version"].clone()).unwrap_or(1),
                title: serde_json::from_value(value["title"].clone())
                    .unwrap_or_else(|_| "UI".into()),
                frame: serde_json::from_value(value["frame"].clone()).unwrap_or_default(),
                accent: serde_json::from_value(value["accent"].clone()).unwrap_or_default(),
                state: serde_json::from_value(value["state"].clone()).unwrap_or_default(),
                view: value["view"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|node| {
                        serde_json::from_value(node.clone())
                            .unwrap_or(Node::Divider { label: None })
                    })
                    .collect(),
                actions: value["actions"]
                    .as_object()
                    .into_iter()
                    .flatten()
                    .map(|(name, steps)| {
                        (
                            name.clone(),
                            steps
                                .as_array()
                                .into_iter()
                                .flatten()
                                .map(|step| {
                                    serde_json::from_value(step.clone())
                                        .unwrap_or(Step::Answer { label: None })
                                })
                                .collect(),
                        )
                    })
                    .collect(),
                timers: serde_json::from_value(value["timers"].clone()).unwrap_or_default(),
                keys: serde_json::from_value(value["keys"].clone()).unwrap_or_default(),
            });
        Self::check_definition(definition, errors)
    }

    #[allow(clippy::too_many_lines)]
    fn check_definition(
        definition: Definition,
        mut errors: Vec<CheckError>,
    ) -> std::result::Result<Self, Vec<CheckError>> {
        let Definition {
            calls,
            version,
            title,
            frame,
            accent,
            state,
            view,
            actions,
            timers,
            keys,
        } = definition;
        let mut doc = Self {
            calls,
            version,
            title,
            frame,
            accent,
            state,
            view,
            actions,
            timers,
            keys,
            sending: BTreeSet::new(),
            answering: BTreeSet::new(),
            program: None,
            salvaged: false,
        };
        check_result(
            &mut errors,
            "version",
            (|| {
                ensure!(doc.version == 1, "unsupported UI version — valid: 1");
                Ok(())
            })(),
        );
        check_result(
            &mut errors,
            "title",
            (|| {
                line_ok(&doc.title, 120)?;
                ensure!(!doc.title.is_empty(), "UI title is required");
                Ok(())
            })(),
        );
        check_result(
            &mut errors,
            "state",
            (|| {
                ensure!(
                    doc.state.len() <= 32,
                    "too many UI state fields — maximum: 32"
                );
                Ok(())
            })(),
        );
        check_result(
            &mut errors,
            "actions",
            (|| {
                ensure!(doc.actions.len() <= 32, "too many UI actions — maximum: 32");
                Ok(())
            })(),
        );
        let mut budget = 1024;
        for (key, value) in &doc.state {
            check_result(
                &mut errors,
                &format!("state.{key}"),
                (|| {
                    identifier(key)?;
                    ensure!(
                        !value.is_array(),
                        "legacy initial state must be scalar or an expression"
                    );
                    validate_expr(value, &doc.state, &mut budget, 0)
                })(),
            );
        }
        check_result(
            &mut errors,
            "calls",
            (|| {
                ensure!(doc.calls.len() <= 32, "maximum 32 data calls");
                ensure!(
                    doc.calls.iter().filter(|d| d.refresh.is_some()).count() <= 5,
                    "maximum 5 refresh timers"
                );
                for call in &doc.calls {
                    identifier(&call.id)?;
                    ensure!(
                        call.refresh.is_none_or(
                            |seconds| seconds.is_finite() && (15.0..=1e12).contains(&seconds)
                        ),
                        "refresh must be at least 15 seconds"
                    );
                    data::value_ok(&call.defaults, &mut 4096, 0)?;
                }
                Ok(())
            })(),
        );
        doc.check_view(&mut errors);
        let mut steps = 0;
        for (name, action) in &doc.actions {
            check_result(&mut errors, &format!("actions.{name}"), identifier(name));
            steps += action
                .iter()
                .filter(|step| !matches!(step, Step::Answer { .. }))
                .count()
                + usize::from(
                    action
                        .iter()
                        .any(|step| matches!(step, Step::Answer { .. })),
                );
            for (index, step) in action.iter().enumerate() {
                check_result(
                    &mut errors,
                    &format!("actions.{name}[{index}]"),
                    (|| {
                        match step {
                            Step::Set { key, value } => {
                                ensure!(
                                    doc.state.contains_key(key),
                                    "unknown state field: {key} — valid: {}",
                                    doc.state.keys().cloned().collect::<Vec<_>>().join(", ")
                                );
                                validate_expr(value, &doc.state, &mut budget, 0)?;
                            }
                            Step::Run { id } => {
                                ensure!(
                                    doc.calls.iter().any(|call| call.id == *id),
                                    "unknown data reference: {id}"
                                );
                            }
                            Step::Call { action } => {
                                ensure!(
                                    doc.actions.contains_key(action),
                                    "unknown action: {action} — valid: {}",
                                    doc.actions.keys().cloned().collect::<Vec<_>>().join(", ")
                                );
                            }
                            Step::Reset { keys } => {
                                ensure!(
                                    !keys.is_empty() && keys.len() <= 32,
                                    "reset needs 1–32 fields"
                                );
                                ensure!(
                                    keys.iter().all(|key| doc.state.contains_key(key)),
                                    "unknown reset field"
                                );
                            }
                            Step::OpenUrl { url } => {
                                validate_expr(url, &doc.state, &mut budget, 0)?;
                                if let Some(url) = url.as_str() {
                                    https_url(url)?;
                                }
                            }
                            Step::Send { text } => Self::template_ok(text)?,
                            Step::ToAssistant { text, context } => {
                                Self::template_ok(text)?;
                                if let Some(context) = context {
                                    validate_expr(context, &doc.state, &mut budget, 0)?;
                                }
                            }
                            Step::Answer { .. } => {}
                        }
                        Ok(())
                    })(),
                );
            }
        }
        check_result(
            &mut errors,
            "actions",
            (|| {
                ensure!(steps <= 128, "too many action steps — maximum: 128");
                Ok(())
            })(),
        );
        if !errors.is_empty() {
            return Err(errors);
        }
        // A repeated `answer` in one action is a model slip, not a second
        // message: keep the first, after checking the original JSON indices.
        for steps in doc.actions.values_mut() {
            let mut answered = false;
            steps.retain(|step| {
                let answer = matches!(step, Step::Answer { .. });
                let keep = !(answer && answered);
                answered |= answer;
                keep
            });
        }
        for name in doc.actions.keys() {
            let (sends, answers) =
                match doc.action_sends(name, &mut BTreeSet::new(), 0, &mut budget) {
                    Ok(flags) => flags,
                    Err(error) => {
                        check_result(&mut errors, &format!("actions.{name}"), Err(error));
                        continue;
                    }
                };
            if sends {
                doc.sending.insert(name.clone());
            }
            if answers {
                doc.answering.insert(name.clone());
            }
        }
        check_result(&mut errors, "timers", doc.validate_play());
        if errors.is_empty() {
            Ok(doc)
        } else {
            Err(errors)
        }
    }

    fn check_view(&self, errors: &mut Vec<CheckError>) {
        if !errors.iter().any(|error| {
            error.path == "view"
                || error.path.starts_with("view[")
                || error.path.starts_with("view.")
        }) {
            check_result(errors, "view", self.validate_view_budget());
        }
        check_result(errors, "view", components::tree_ok(&self.view, 0, &mut 128));
        let mut budget = 1024;
        for (index, node) in self.view.iter().enumerate() {
            check_result(
                errors,
                &format!("view[{index}]"),
                self.validate_node(node, &mut budget).map(|_| ()),
            );
        }
    }

    fn validate_view_budget(&self) -> Result<()> {
        ensure!(
            !self.view.is_empty() && components::max_visible(&self.view) <= 16,
            "UI needs 1–16 view nodes"
        );
        let rows: u32 = self.view.iter().map(|node| u32::from(node.rows())).sum();
        let controls = self.control_count();
        ensure!(
            rows <= u32::from(ROWS - 3) && controls <= 32,
            "UI exceeds its fixed display budget"
        );
        Ok(())
    }

    pub(super) fn validate_node(&self, node: &Node, budget: &mut usize) -> Result<usize> {
        let max_rows = node.max_rows();
        ensure!(
            (1..=max_rows).contains(&node.rows()),
            "node height exceeds its limit"
        );
        let mut controls = 0;
        match node {
            Node::Text { text, .. } => Self::template_ok(text)?,
            Node::Buttons { items, .. } => {
                ensure!(
                    !items.is_empty() && items.len() <= 8,
                    "a button row needs 1–8 buttons"
                );
                for button in items {
                    image_ok(button.image.as_deref(), button.alt.as_deref())?;
                    line_ok(&button.label, 120)?;
                    Self::template_ok(&button.label)?;
                    ensure!(
                        self.actions.contains_key(&button.action),
                        "unknown button action: {} — valid: {}",
                        button.action,
                        self.actions.keys().cloned().collect::<Vec<_>>().join(", ")
                    );
                    ensure!(button.body.len() <= 8, "too many button artwork lines");
                    for line in &button.body {
                        line_ok(line, 120)?;
                    }
                }
                controls += items.len();
            }
            Node::Stepper {
                key,
                label,
                min,
                max,
                step,
            }
            | Node::Slider {
                key,
                label,
                min,
                max,
                step,
            } => {
                Self::template_ok(label)?;
                for value in [min, max, step] {
                    ensure!(value.is_finite(), "stepper bounds and step must be finite");
                    scalar_ok(&json!(value))?;
                }
                ensure!(min < max, "stepper min must be less than max");
                ensure!(*step > 0.0, "stepper step must be positive");
                binding_ok(&self.state, key, Value::is_number)?;
                controls += 1;
            }
            Node::Input {
                key,
                label,
                rules,
                placeholder,
                hint,
                rows,
            } => {
                validation::check(rules, node.input_limit().unwrap_or(MAX_VALUE))?;
                ensure!((1..=8).contains(rows), "input rows must be 1–8");
                // A Textarea's placeholder may sketch the expected shape over
                // several lines ("1. …\n2. …"); an Input's is one line.
                if node.is_textarea() {
                    text_ok(placeholder, 240)?;
                } else {
                    line_ok(placeholder, 120)?;
                }
                line_ok(hint, 120)?;
                ensure!(
                    self.state.contains_key(key),
                    "unknown input field: {key} — valid: {}",
                    self.state.keys().cloned().collect::<Vec<_>>().join(", ")
                );
                line_ok(label, 120)?;
                let value = &self.state[key];
                if let Some(values) = value.get("random").and_then(Value::as_array) {
                    for value in values {
                        input_ok(value, node)?;
                    }
                } else {
                    input_ok(value, node)?;
                }
                controls += 1;
            }
            _ => controls += self.validate_component(node, budget)?,
        }
        Ok(controls)
    }

    fn validate_display(node: &Node) -> Result<()> {
        match node {
            Node::Image { src, alt, rows } => {
                image_ok(Some(src), Some(alt))?;
                ensure!((4..=12).contains(rows), "image rows must be 4–12");
            }
            Node::Hero {
                title,
                subtitle,
                badge,
                image,
                alt,
            } => {
                image_ok(image.as_deref(), alt.as_deref())?;
                Self::template_ok(title)?;
                for text in subtitle.iter().chain(badge) {
                    Self::template_ok(text)?;
                }
            }
            Node::Columns { items } => {
                item_count(items.len(), 2, 3)?;
                for column in items {
                    Self::template_ok(&column.title)?;
                    item_count(column.items.len(), 0, 8)?;
                    for item in &column.items {
                        Self::template_ok(&item.text)?;
                    }
                }
            }
            Node::Cards { items } => {
                item_count(items.len(), 1, 6)?;
                for item in items {
                    image_ok(item.image.as_deref(), item.alt.as_deref())?;
                    Self::template_ok(&item.title)?;
                    Self::template_ok(&item.text)?;
                    if let Some(badge) = &item.badge {
                        Self::template_ok(badge)?;
                    }
                }
            }
            Node::Stats { items, .. } => {
                item_count(items.len(), 1, 6)?;
                for item in items {
                    Self::template_ok(&item.value)?;
                    Self::template_ok(&item.label)?;
                    if let Some(delta) = &item.delta {
                        Self::template_ok(delta)?;
                    }
                }
            }
            Node::Steps { items } => {
                item_count(items.len(), 1, 8)?;
                for item in items {
                    Self::template_ok(&item.title)?;
                    if let Some(text) = &item.text {
                        Self::template_ok(text)?;
                    }
                }
            }
            Node::Callout { title, text, .. } => {
                Self::template_ok(text)?;
                if let Some(title) = title {
                    Self::template_ok(title)?;
                }
            }
            Node::Quote { text, by } => {
                Self::template_ok(text)?;
                if let Some(by) = by {
                    Self::template_ok(by)?;
                }
            }
            Node::Divider { label: Some(label) } => Self::template_ok(label)?,
            _ => {}
        }
        Ok(())
    }

    fn validate_component(&self, node: &Node, budget: &mut usize) -> Result<usize> {
        if let Some(controls) = self.validate_new_component(node, budget)? {
            return Ok(controls);
        }
        if node.is_display() {
            Self::validate_display(node)?;
            return Ok(0);
        }
        match node {
            Node::Kv { items } => {
                item_count(items.len(), 1, 8)?;
                for pair in items {
                    for text in pair {
                        Self::template_ok(text)?;
                    }
                }
            }
            Node::Table {
                columns,
                rows_data,
                select,
                sortable,
                page_size,
                search,
            } => {
                item_count(columns.len(), 1, 6)?;
                item_count(rows_data.len(), 0, if page_size.is_some() { 64 } else { 8 })?;
                if let Some(size) = page_size {
                    item_count(*size, 1, 8)?;
                }
                for header in columns {
                    Self::template_ok(header)?;
                }
                for row in rows_data {
                    ensure!(row.len() == columns.len(), "table row width mismatch");
                    for cell in row {
                        Self::template_ok(cell)?;
                    }
                }
                if let Some(key) = select {
                    binding_ok(&self.state, key, Value::is_number)?;
                }
                return Ok(usize::from(
                    select.is_some() || *sortable || page_size.is_some() || *search,
                ));
            }
            Node::Progress { label, value, max } => {
                Self::template_ok(label)?;
                self.numeric_ok(value, budget)?;
                self.numeric_ok(max, budget)?;
            }
            Node::Bars { items } => {
                item_count(items.len(), 1, 8)?;
                for item in items {
                    Self::template_ok(&item.label)?;
                    self.numeric_ok(&item.value, budget)?;
                }
            }
            Node::Sparkline { values, rows } => {
                item_count(values.len(), 1, 64)?;
                ensure!((1..=3).contains(rows), "sparkline rows must be 1–3");
                for value in values {
                    self.numeric_ok(value, budget)?;
                }
            }
            Node::Checklist { items } => {
                item_count(items.len(), 1, 8)?;
                for item in items {
                    Self::template_ok(&item.label)?;
                    binding_ok(&self.state, &item.key, Value::is_boolean)?;
                }
                return Ok(items.len());
            }
            Node::Select {
                key,
                label,
                options,
            } => {
                Self::template_ok(label)?;
                item_count(options.len(), 2, 8)?;
                for option in options {
                    line_ok(option, 64)?;
                    Self::template_ok(option)?;
                }
                binding_ok(&self.state, key, |value| {
                    value
                        .as_str()
                        .is_some_and(|text| options.iter().any(|s| s == text))
                })?;
                return Ok(1);
            }
            Node::Toggle { key, label } => {
                Self::template_ok(label)?;
                binding_ok(&self.state, key, Value::is_boolean)?;
                return Ok(1);
            }
            _ => {}
        }
        Ok(0)
    }

    pub(super) fn numeric_ok(&self, value: &Value, budget: &mut usize) -> Result<()> {
        validate_expr(value, &self.state, budget, 0)?;
        ensure!(
            numeric_expr(value, &self.state),
            "expected numeric expression"
        );
        Ok(())
    }

    pub(super) fn template_ok(text: &str) -> Result<()> {
        text_ok(text, MAX_TEXT)?;
        let mut rest = text;
        while let Some((_, tail)) = rest.split_once("{{") {
            let (_, after) = tail
                .split_once("}}")
                .ok_or_else(|| anyhow::anyhow!("unclosed UI template"))?;
            rest = after;
        }
        Ok(())
    }

    fn action_sends(
        &self,
        action: &str,
        seen: &mut BTreeSet<String>,
        depth: usize,
        budget: &mut usize,
    ) -> Result<(bool, bool)> {
        spend(budget)?;
        ensure!(
            depth < 8 && seen.insert(action.into()),
            "cyclic or deeply nested UI action"
        );
        let mut sends = false;
        let mut answers = false;
        for step in &self.actions[action] {
            spend(budget)?;
            let (message, answer) = match step {
                Step::Send { .. } | Step::ToAssistant { .. } => (true, false),
                Step::Answer { .. } => (true, true),
                Step::Call { action } => self.action_sends(action, seen, depth + 1, budget)?,
                Step::Set { .. } | Step::Reset { .. } | Step::OpenUrl { .. } | Step::Run { .. } => {
                    (false, false)
                }
            };
            // An answer can't be combined with another message. Two `send`s
            // stay a runtime error (documents written before answers parse
            // unchanged).
            ensure!(
                !(sends && message && (answers || answer)),
                "an action may send only one message"
            );
            sends |= message;
            answers |= answer;
        }
        seen.remove(action);
        Ok((sends, answers))
    }

    /// Document title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
    /// Document frame style.
    #[must_use]
    pub const fn frame(&self) -> Frame {
        self.frame
    }
    /// Theme role for highlights.
    #[must_use]
    pub const fn accent(&self) -> Accent {
        self.accent
    }
    /// Number of independently focusable controls.
    #[must_use]
    pub fn control_count(&self) -> usize {
        self.all_nodes()
            .into_iter()
            .map(|node| match node {
                Node::Buttons { items, .. } => items.len(),
                Node::Checklist { items } => items.len(),
                Node::Tabs { .. }
                | Node::Rating { .. }
                | Node::Code { .. }
                | Node::Slider { .. }
                | Node::Form { .. }
                | Node::Input { .. }
                | Node::Select { .. }
                | Node::Stepper { .. }
                | Node::Toggle { .. }
                | Node::Editor { .. } => 1,
                Node::Accordion { items } => items.len(),
                Node::Canvas { shapes, .. } => shapes
                    .iter()
                    .filter(|shape| shape.action().is_some())
                    .count(),
                Node::Tree { items } => extras::flatten(items)
                    .iter()
                    .filter(|(_, branch)| !branch.items.is_empty())
                    .count(),
                Node::Choice { options, .. } => options.len(),
                Node::Quiz { .. } => 2,
                Node::Table {
                    select,
                    sortable,
                    page_size,
                    search,
                    ..
                } => usize::from(select.is_some() || *sortable || page_size.is_some() || *search),
                _ => 0,
            })
            .sum()
    }
    /// Number of named actions.
    #[must_use]
    pub fn action_count(&self) -> usize {
        self.actions.len()
    }
    /// Whether this is a display-only layout, with neither actions nor controls.
    #[must_use]
    pub fn is_static(&self) -> bool {
        self.calls.is_empty() && self.actions.is_empty() && self.control_count() == 0
    }
    /// Readable plain rendering using initial state, without JSON or markup.
    #[must_use]
    pub fn plain_text(&self) -> String {
        let Ok(session) = Session::new(self.clone()) else {
            return self.title.clone();
        };
        let mut lines = vec![self.title.clone()];
        for node in &self.view {
            lines.extend(session.plain_node(node));
        }
        lines.join("\n")
    }
    /// Immutable, validated presentation nodes.
    #[must_use]
    pub fn nodes(&self) -> &[Node] {
        &self.view
    }
    /// Declared validation budget; frontends measure their actual layout.
    #[must_use]
    pub fn rows(&self) -> u16 {
        self.view.iter().map(Node::rows).sum::<u16>() + 3
    }
    /// Whether activation can send a message, for explicit frontend labelling.
    #[must_use]
    pub fn sends(&self, action: &str) -> bool {
        self.sending.contains(action)
    }
    /// Whether activation immediately answers with the clicked button's label.
    #[must_use]
    pub fn answers(&self, action: &str) -> bool {
        self.answering.contains(action)
    }
}
