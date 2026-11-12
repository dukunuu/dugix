use anyhow::{Result, bail, ensure};
use serde_json::{Map, Value, json};

use super::check::Step;
use super::document::Document;
use super::expr::{
    boolean, eval, expand, https_url, input_ok, line_ok, number, scalar_text, spend,
};
use super::node::{Mark, Node, StepState};
use super::{
    components, data, extras, live, merge_program, motion, play, practice, stream, validation,
};

/// Local runtime for one document. Frontends own its lifetime and identity.
pub struct Session {
    pub(super) document: Document,
    pub(super) state: Map<String, Value>,
    pub(super) components: components::ComponentState,
    pub(super) defaults: Map<String, Value>,
    pending_url: Option<String>,
    assistant_payload: Option<Value>,
    pub(super) live: live::Panel,
    pub(super) continuation: Option<(String, Vec<(String, usize)>)>,
    /// Saved state names the current program does not declare (yet): kept so
    /// a replayed patch that introduces them again finds their saved values.
    pub(super) held: Map<String, Value>,
    /// Bumped by every state or presentation change; frontends use it to
    /// notice what needs saving.
    pub(super) revision: u64,
    /// Seconds of animation time; see [`Session::clock`].
    pub(super) clock: f64,
    /// Value glides and chart draw-in; see `motion.rs`.
    pub(super) motion: motion::Motion,
    /// Timer due times; see `play.rs`.
    pub(super) schedule: play::Schedule,
}

#[derive(Default)]
pub(super) struct Effects {
    message: Option<String>,
    url: Option<String>,
    payload: Option<Value>,
    requests: Vec<String>,
    waiting: Option<String>,
    remaining: Vec<(String, usize)>,
}

impl Session {
    /// Initialize scalar state; random choices happen once, not during drawing.
    pub fn new(document: Document) -> Result<Self> {
        let mut state = Map::new();
        for (key, value) in &document.state {
            state.insert(key.clone(), eval(value, &Map::new(), &mut 1024)?);
        }
        let live = live::Panel::new(&document.calls);
        let mut session = Self {
            document,
            components: components::ComponentState::default(),
            defaults: state.clone(),
            state,
            pending_url: None,
            assistant_payload: None,
            live,
            continuation: None,
            held: Map::new(),
            revision: 0,
            clock: 0.0,
            motion: motion::Motion::default(),
            schedule: play::Schedule::default(),
        };
        let mut state = session.state.clone();
        session.bindings_ok(&mut state)?;
        session.commit(state)?;
        Ok(session)
    }
    pub(super) fn plain_node(&self, node: &Node) -> Vec<String> {
        let text = |s: &str| self.text(s).unwrap_or_default();
        match node {
            Node::Image { alt, .. } => vec![format!("[image: {alt}]")],
            Node::Hero {
                title,
                subtitle,
                badge,
                image,
                alt,
            } => std::iter::once(text(title))
                .chain(
                    image
                        .iter()
                        .map(|_| format!("[image: {}]", alt.as_deref().unwrap_or_default())),
                )
                .chain(subtitle.iter().chain(badge).map(|s| text(s)))
                .collect(),
            Node::Columns { items } => items
                .iter()
                .flat_map(|column| {
                    std::iter::once(text(&column.title)).chain(column.items.iter().map(|item| {
                        let mark = match item.mark {
                            Mark::Ok => "✓ ",
                            Mark::No => "✗ ",
                            Mark::Dot => "● ",
                            Mark::None => "",
                        };
                        format!("{mark}{}", text(&item.text))
                    }))
                })
                .collect(),
            Node::Cards { items } => items
                .iter()
                .flat_map(|item| {
                    std::iter::once(text(&item.title))
                        .chain(item.badge.iter().map(|s| text(s)))
                        .chain(std::iter::once(text(&item.text)))
                        .chain(item.image.iter().map(|_| {
                            format!("[image: {}]", item.alt.as_deref().unwrap_or_default())
                        }))
                })
                .collect(),
            Node::Stats { items, .. } => items
                .iter()
                .map(|item| {
                    let delta = item
                        .delta
                        .as_ref()
                        .map_or_else(String::new, |s| format!(" ({})", text(s)));
                    format!("{}: {}{delta}", text(&item.label), text(&item.value))
                })
                .collect(),
            Node::Steps { items } => items
                .iter()
                .flat_map(|item| {
                    let mark = match item.state {
                        StepState::Done => "●",
                        StepState::Current => "◉",
                        StepState::Todo => "○",
                    };
                    std::iter::once(format!("{mark} {}", text(&item.title)))
                        .chain(item.text.iter().map(|s| text(s)))
                })
                .collect(),
            Node::Callout {
                title, text: body, ..
            } => title
                .iter()
                .map(|s| text(s))
                .chain(std::iter::once(text(body)))
                .collect(),
            Node::Quote { text: body, by } => std::iter::once(format!("\"{}\"", text(body)))
                .chain(by.iter().map(|s| format!("— {}", text(s))))
                .collect(),
            Node::Divider { label } => {
                vec![label.as_ref().map_or_else(|| "---".into(), |s| text(s))]
            }
            _ => self.plain_component(node),
        }
    }

    #[allow(clippy::too_many_lines)]
    fn plain_component(&self, node: &Node) -> Vec<String> {
        let text = |s: &str| self.text(s).unwrap_or_default();
        match node {
            Node::Text { text: body, .. }
            | Node::Badge { text: body, .. }
            | Node::Md { text: body, .. } => vec![text(body)],
            Node::Buttons { items, .. } => items
                .iter()
                .flat_map(|item| {
                    std::iter::once(text(&item.label))
                        .chain(item.body.iter().map(|s| text(s)))
                        .chain(item.image.iter().map(|_| {
                            format!("[image: {}]", item.alt.as_deref().unwrap_or_default())
                        }))
                })
                .collect(),
            Node::Input { key, label, .. }
            | Node::Select { key, label, .. }
            | Node::Stepper { key, label, .. }
            | Node::Toggle { key, label }
            | Node::Rating { key, label, .. }
            | Node::Slider { key, label, .. } => {
                vec![format!("{}: {}", text(label), self.value(key))]
            }
            Node::Kv { items } => items
                .iter()
                .map(|pair| format!("{}: {}", text(&pair[0]), text(&pair[1])))
                .collect(),
            Node::Table {
                columns, rows_data, ..
            } => std::iter::once(columns)
                .chain(rows_data)
                .map(|row| row.iter().map(|s| text(s)).collect::<Vec<_>>().join(" | "))
                .collect(),
            Node::Progress { label, value, max }
            | Node::Meter {
                label, value, max, ..
            } => {
                let (value, max) = self.progress(value, max).unwrap_or_default();
                vec![format!("{}: {value}/{max}", text(label))]
            }
            Node::Bars { items } => items
                .iter()
                .map(|item| {
                    format!(
                        "{}: {}",
                        text(&item.label),
                        self.number(&item.value).unwrap_or_default()
                    )
                })
                .collect(),
            Node::Sparkline { values, .. } => vec![
                values
                    .iter()
                    .map(|v| self.number(v).unwrap_or_default().to_string())
                    .collect::<Vec<_>>()
                    .join(" "),
            ],
            Node::Checklist { items } => items
                .iter()
                .map(|item| {
                    format!(
                        "{} {}",
                        if self.value(&item.key) == "true" {
                            "✓"
                        } else {
                            "○"
                        },
                        text(&item.label)
                    )
                })
                .collect(),
            Node::Tabs { items, .. } => items
                .iter()
                .flat_map(|item| {
                    std::iter::once(text(&item.label))
                        .chain(item.view.iter().flat_map(|node| self.plain_node(node)))
                })
                .collect(),
            Node::Row { items, .. } | Node::Grid { items, .. } | Node::Stack { items, .. } => items
                .iter()
                .flat_map(|node| self.plain_node(node))
                .collect(),
            Node::Number {
                value,
                label,
                delta,
                ..
            } => vec![format!(
                "{}: {}{}",
                text(label),
                text(value),
                delta
                    .as_ref()
                    .map(|d| format!(" ({})", text(d)))
                    .unwrap_or_default()
            )],
            Node::Gauge {
                label, value, max, ..
            } => {
                let (value, max) = self.progress(value, max).unwrap_or_default();
                vec![format!("{}: {value} / {max}", text(label))]
            }
            Node::Heatmap { values, labels, .. } => values
                .iter()
                .enumerate()
                .map(|(row, values)| {
                    let label = labels.get(row).map(|l| text(l) + ": ").unwrap_or_default();
                    let values: Vec<_> = values
                        .iter()
                        .map(|v| self.number(v).unwrap_or_default().to_string())
                        .collect();
                    format!("{label}{}", values.join(", "))
                })
                .collect(),
            Node::Tree { items } => extras::flatten(items)
                .into_iter()
                .map(|(depth, branch)| format!("{}- {}", "  ".repeat(depth), text(&branch.label)))
                .collect(),
            Node::Board { items, .. } => items
                .iter()
                .map(|lane| {
                    let cards = self
                        .evaluate(&lane.items)
                        .ok()
                        .and_then(|v| v.as_array().cloned())
                        .unwrap_or_default();
                    let cards: Vec<_> = cards.iter().map(scalar_text).collect();
                    format!("{}: {}", text(&lane.title), cards.join(", "))
                })
                .collect(),
            Node::Diagram { alt, .. } => vec![if alt.is_empty() {
                "[diagram]".into()
            } else {
                format!("[diagram: {alt}]")
            }],
            Node::Accordion { items } => items
                .iter()
                .flat_map(|item| {
                    std::iter::once(text(&item.title))
                        .chain(item.view.iter().flat_map(|node| self.plain_node(node)))
                })
                .collect(),
            Node::Form {
                title,
                fields,
                submit_label,
                ..
            } => std::iter::once(text(title))
                .chain(fields.iter().flat_map(|node| self.plain_node(node)))
                .chain(std::iter::once(text(submit_label)))
                .collect(),
            Node::Chart { series, .. } => series
                .iter()
                .map(|series| {
                    format!(
                        "{}: {}",
                        text(&series.name),
                        series
                            .values
                            .iter()
                            .map(|value| self.number(value).unwrap_or_default().to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect(),
            Node::Timeline { items } => items
                .iter()
                .flat_map(|item| {
                    std::iter::once(format!("{} {}", text(&item.time), text(&item.title)))
                        .chain(item.text.iter().map(|value| text(value)))
                })
                .collect(),
            Node::Code { source, .. } => vec![source.clone()],
            Node::Canvas { alt, shapes, .. } => vec![if alt.is_empty() {
                format!("[drawing: {} shapes]", shapes.len())
            } else {
                format!("[drawing: {alt}]")
            }],
            Node::Editor { key, label, .. } => {
                vec![format!("{}:", text(label)), self.value(key)]
            }
            Node::Choice {
                key,
                label,
                options,
                ..
            } => std::iter::once(format!("{}: {}", text(label), self.value(key)))
                .chain(options.iter().map(|option| format!("- {}", text(option))))
                .collect(),
            Node::Quiz { items, .. } => items
                .iter()
                .flat_map(|item| {
                    std::iter::once(text(&item.title))
                        .chain(item.view.iter().flat_map(|node| self.plain_node(node)))
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    #[must_use]
    pub const fn document(&self) -> &Document {
        &self.document
    }
    /// Scalar display value; substitution is single-pass, never recursive.
    #[must_use]
    pub fn value(&self, key: &str) -> String {
        self.state.get(key).map_or_else(String::new, scalar_text)
    }
    /// Interpolate bounded plain text.
    pub fn text(&self, template: &str) -> Result<String> {
        expand(template, &self.state)
    }
    /// Edit only a declared string input. No markup or terminal escapes.
    pub fn input(&mut self, key: &str, value: String) -> Result<()> {
        ensure!(
            self.document
                .all_nodes()
                .into_iter()
                .any(|n| matches!(n, Node::Input { key: k, .. } if k == key)),
            "not an input field"
        );
        let mut next = self.state.clone();
        next.insert(key.into(), Value::String(value));
        self.bindings_ok(&mut next)?;
        self.commit(next)
    }
    /// Evaluate a numeric presentation expression against current state.
    pub fn number(&self, expression: &Value) -> Result<f64> {
        number(&eval(expression, &self.state, &mut 1024)?)
    }
    /// Evaluate progress, clamped to a nonnegative maximum.
    pub fn progress(&self, value: &Value, max: &Value) -> Result<(f64, f64)> {
        let max = self.number(max)?.max(0.0);
        Ok((self.number(value)?.clamp(0.0, max), max))
    }
    /// Flip a declared checklist or toggle field, without running an action.
    pub fn toggle(&mut self, key: &str) -> Result<()> {
        ensure!(
            self.document
                .all_nodes()
                .into_iter()
                .any(|node| match node {
                    Node::Toggle { key: k, .. } => k == key,
                    Node::Checklist { items } => items.iter().any(|item| item.key == key),
                    _ => false,
                }),
            "not a boolean control"
        );
        let mut next = self.state.clone();
        next.insert(key.into(), json!(!boolean(&self.state[key])?));
        self.bindings_ok(&mut next)?;
        self.commit(next)
    }
    /// Cycle one select node, wrapping within its fixed options.
    pub fn cycle(&mut self, node: usize, forward: bool) -> Result<()> {
        let Some(Node::Select { key, options, .. }) = self.document.node(node) else {
            bail!("not a select control");
        };
        let index = options
            .iter()
            .position(|s| Some(s.as_str()) == self.state[key].as_str())
            .ok_or_else(|| anyhow::anyhow!("invalid select value"))?;
        let index = (1..options.len())
            .map(|offset| {
                (index
                    + if forward {
                        offset
                    } else {
                        options.len() - offset
                    })
                    % options.len()
            })
            .find(|next| options[*next] != options[index])
            .unwrap_or(index);
        let mut next = self.state.clone();
        next.insert(key.clone(), json!(options[index]));
        self.bindings_ok(&mut next)?;
        self.commit(next)
    }
    /// Adjust one stepper locally by its step, clamping at either bound.
    pub fn step(&mut self, node: usize, forward: bool) -> Result<()> {
        let Some(
            Node::Stepper {
                key,
                min,
                max,
                step,
                ..
            }
            | Node::Slider {
                key,
                min,
                max,
                step,
                ..
            },
        ) = self.document.node(node)
        else {
            bail!("not a stepper control");
        };
        let value = number(&self.state[key])? + if forward { *step } else { -*step };
        let mut next = self.state.clone();
        next.insert(key.clone(), json!(value.clamp(*min, *max)));
        self.bindings_ok(&mut next)?;
        self.commit(next)
    }
    /// Current normalized table index (zero for an empty table).
    #[must_use]
    pub fn selected_row(&self, key: &str) -> usize {
        self.state
            .get(key)
            .and_then(Value::as_u64)
            .and_then(|row| usize::try_from(row).ok())
            .unwrap_or(0)
    }
    /// Select a table row locally, clamping to the table's data bounds.
    pub fn select_row(&mut self, node: usize, row: usize) -> Result<()> {
        let Some(Node::Table {
            select: Some(key),
            rows_data,
            ..
        }) = self.document.node(node)
        else {
            bail!("not a selectable table");
        };
        let mut next = self.state.clone();
        next.insert(
            key.clone(),
            json!(row.min(rows_data.len().saturating_sub(1))),
        );
        self.bindings_ok(&mut next)?;
        self.commit(next)?;
        if let Some(Node::Table {
            page_size: Some(size),
            ..
        }) = self.document.node(node)
            && let Some(position) = self.table_rows(node).iter().position(|index| *index == row)
        {
            self.follow_table_row(node, position / size);
        }
        Ok(())
    }
    /// Execute an action transactionally. At most one explicit message is
    /// returned; the frontend decides when/how to send it as plain user text.
    pub fn activate(&mut self, action: &str) -> Result<Option<String>> {
        ensure!(
            !self.document.answers(action),
            "answer actions require a button"
        );
        self.activate_action(action)
    }
    /// Activate a button, capturing its rendered label before any state changes.
    /// Answer messages are single-line labels, without the review-message prefix.
    pub fn activate_button(&mut self, node: usize, item: usize) -> Result<Option<String>> {
        let Some(Node::Buttons { items, .. }) = self.document.node(node) else {
            bail!("answer actions require a button");
        };
        let button = items
            .get(item)
            .ok_or_else(|| anyhow::anyhow!("unknown button"))?;
        let action = button.action.clone();
        let answer = if self.document.answers(&action) {
            let label = self.text(&button.label)?;
            line_ok(&label, 120)?;
            Some(label)
        } else {
            None
        };
        let message = self.activate_action(&action)?;
        Ok(answer.or(message))
    }
    pub(super) fn activate_action(&mut self, action: &str) -> Result<Option<String>> {
        ensure!(self.document.actions.contains_key(action), "unknown action");
        ensure!(self.continuation.is_none(), "mutation is still loading");
        self.interact_data(std::time::Instant::now());
        let mut next = self.state.clone();
        let mut effects = Effects::default();
        self.run(action, &mut next, &mut effects, &mut 1024)?;
        self.bindings_ok(&mut next)?;
        self.commit(next)?;
        self.apply_effects(effects)
    }
    pub(super) fn run(
        &self,
        action: &str,
        state: &mut Map<String, Value>,
        effects: &mut Effects,
        budget: &mut usize,
    ) -> Result<()> {
        self.run_steps(action, 0, state, effects, budget)
    }
    fn run_steps(
        &self,
        action: &str,
        start: usize,
        state: &mut Map<String, Value>,
        effects: &mut Effects,
        budget: &mut usize,
    ) -> Result<()> {
        for (index, step) in self.document.actions[action].iter().enumerate().skip(start) {
            spend(budget)?;
            match step {
                Step::Set { key, value } => {
                    self.validate_reads(value, state)?;
                    let value = eval(value, state, budget)?;
                    state.insert(key.clone(), value);
                }
                Step::Run { id } => {
                    let call = self
                        .document
                        .calls
                        .iter()
                        .find(|call| call.id == *id)
                        .ok_or_else(|| anyhow::anyhow!("unknown data call"))?;
                    effects.requests.push(id.clone());
                    if !call.query {
                        effects.waiting = Some(id.clone());
                    }
                }
                Step::Call { action } => self.run(action, state, effects, budget)?,
                Step::Reset { keys } => {
                    for key in keys {
                        state.insert(key.clone(), self.defaults[key].clone());
                    }
                }
                Step::OpenUrl { url: expression } => {
                    self.validate_reads(expression, state)?;
                    let target = eval(expression, state, budget)?;
                    let target = target
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("URL must be a string"))?;
                    https_url(target)?;
                    ensure!(effects.url.is_none(), "only one URL per activation");
                    effects.url = Some(target.into());
                }
                Step::Answer { .. } => {
                    self.validate_form(state)?;
                }
                Step::Send { text } | Step::ToAssistant { text, .. } => {
                    self.validate_form(state)?;
                    ensure!(
                        effects.message.is_none(),
                        "an action may send only one message"
                    );
                    if let Step::ToAssistant { context, .. } = step {
                        let context = context
                            .as_ref()
                            .map(|context| eval(context, state, budget))
                            .transpose()?;
                        effects.payload = Some(json!({"context":context,"state":state}));
                    }
                    effects.message = Some(format!(
                        "UI response — {}\n{}",
                        self.document.title,
                        expand(text, state)?
                    ));
                }
            }
            if effects.waiting.is_some() {
                effects.remaining.push((action.into(), index + 1));
                break;
            }
        }
        Ok(())
    }
    fn apply_effects(&mut self, effects: Effects) -> Result<Option<String>> {
        for id in effects.requests {
            self.live.request(&id)?;
        }
        self.continuation = effects.waiting.map(|id| (id, effects.remaining));
        self.pending_url = effects.url;
        self.assistant_payload = effects.payload;
        Ok(effects.message)
    }
    pub(super) fn commit(&mut self, state: Map<String, Value>) -> Result<()> {
        data::value_ok(&Value::Object(state.clone()), &mut 4096, 0)?;
        if let Some(source) = &self.document.program {
            let salvaged = self.document.salvaged;
            let value =
                stream::program_state(source, Some(&state), Some(&self.live.values()), salvaged)?;
            let mut document = Document::check_value(value)
                .map_err(|errors| anyhow::anyhow!(errors[0].message.clone()))?;
            document.program = Some(source.clone());
            document.salvaged = salvaged;
            self.document = document;
        }
        self.state = state;
        self.revision += 1;
        Ok(())
    }

    /// Replace a program after a completed edit, retaining surviving state names.
    pub fn patch(&mut self, patch: &str) -> Result<()> {
        let source = self
            .document
            .program
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("legacy panels cannot be patched"))?;
        if self.document.salvaged {
            // A panel already missing broken pieces takes any patch that leaves
            // something to show; patching the broken statement restores it.
            let source = stream::merge_statements(source, patch)?;
            let (document, _) = Document::recover(&source).map_err(|errors| {
                anyhow::anyhow!(
                    errors
                        .iter()
                        .map(|error| format!("{}: {}", error.path, error.message))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            })?;
            return self.replace_document(document);
        }
        let source = merge_program(source, patch)?;
        self.replace_program(&source)
    }

    /// Install a validated program revision, retaining surviving state names.
    pub fn replace_program(&mut self, source: &str) -> Result<()> {
        self.replace_document(Document::parse(source)?)
    }

    fn replace_document(&mut self, document: Document) -> Result<()> {
        let mut next = Self::new(document)?;
        let mut state = next.state.clone();
        state.extend(self.state.clone());
        let mut held = Map::new();
        for (key, value) in &self.held {
            if next.defaults.contains_key(key) {
                state.insert(key.clone(), value.clone());
            } else {
                held.insert(key.clone(), value.clone());
            }
        }
        ensure!(
            state.len() <= 32,
            "too many retained UI state fields — maximum: 32"
        );
        next.bindings_ok(&mut state)?;
        next.commit(state)?;
        next.live = std::mem::take(&mut self.live);
        next.live.revise(&next.document.calls);
        next.held = held;
        next.revision = self.revision + 1;
        *self = next;
        Ok(())
    }

    /// Explicit user activation; restored and merely rendered panels stay inert.
    pub fn interact_data(&mut self, now: std::time::Instant) {
        self.live.interact(now);
    }

    /// Consent to the first potentially paid query calls in this panel.
    pub fn load_data(&mut self, now: std::time::Instant) {
        self.live.load(now);
    }

    /// Whether this program declares live data work.
    #[must_use]
    pub fn has_data(&self) -> bool {
        !self.document.calls.is_empty()
    }

    /// Whether the host should offer an explicit Load button.
    #[must_use]
    pub fn needs_data_load(&self) -> bool {
        self.live.needs_load(&self.document.calls, &self.state)
    }

    /// Subtle host status, separate from the query's data shape.
    #[must_use]
    pub fn data_hint(&self) -> Option<&str> {
        self.live.hint()
    }

    /// Schedule at most one call. The host supplies visibility, focus and stream
    /// readiness by calling this only when all three permit execution.
    pub fn next_data_call(
        &mut self,
        now: std::time::Instant,
        session_calls: &mut usize,
    ) -> Result<Option<live::Call>> {
        let call = self
            .live
            .next(&self.document.calls, &self.state, now, session_calls)?;
        if call.is_some() {
            self.commit(self.state.clone())?;
        }
        Ok(call)
    }

    /// Apply a completion only to its live revision and current arguments.
    /// Awaited mutation actions continue only on success; failure drops the tail.
    pub fn finish_data_call(
        &mut self,
        call: &live::Call,
        outcome: &live::Outcome,
        now: std::time::Instant,
    ) -> Result<Option<String>> {
        let current = self.document.calls.iter().find(|d| d.id == call.id);
        let accepted = self.live.finish(call, outcome, current, &self.state, now)?;
        self.commit(self.state.clone())?;
        if accepted
            && self
                .continuation
                .as_ref()
                .is_some_and(|(id, _)| *id == call.id)
        {
            let (_, steps) = self.continuation.take().expect("matched continuation");
            if outcome.success {
                let mut state = self.state.clone();
                let mut effects = Effects::default();
                for (index, (action, start)) in steps.iter().enumerate() {
                    ensure!(
                        self.document.actions.contains_key(action),
                        "continued action is no longer visible"
                    );
                    self.run_steps(action, *start, &mut state, &mut effects, &mut 1024)?;
                    if effects.waiting.is_some() {
                        effects.remaining.extend_from_slice(&steps[index + 1..]);
                        break;
                    }
                }
                self.bindings_ok(&mut state)?;
                self.commit(state)?;
                return self.apply_effects(effects);
            }
        }
        Ok(None)
    }

    /// Context and page/form state captured at the most recent send step.
    /// Frontends append this JSON to the reviewed user turn, not to answers.
    #[must_use]
    pub const fn assistant_payload(&self) -> Option<&Value> {
        self.assistant_payload.as_ref()
    }

    /// URL awaiting a separate user confirmation. Parsing never sets this.
    #[must_use]
    pub fn pending_url(&self) -> Option<&str> {
        self.pending_url.as_deref()
    }

    /// Consume a pending URL, opening it only when explicitly confirmed.
    /// The injected opener lets frontends and tests control OS interaction.
    pub fn confirm_url(
        &mut self,
        confirmed: bool,
        opener: impl FnOnce(&str) -> Result<()>,
    ) -> Result<Option<String>> {
        let Some(url) = self.pending_url.take() else {
            return Ok(None);
        };
        if !confirmed {
            return Ok(None);
        }
        https_url(&url)?;
        opener(&url)?;
        Ok(Some(format!("Opened {url}")))
    }

    /// Current inline validation message for an input.
    #[must_use]
    pub fn input_error(&self, key: &str) -> Option<String> {
        self.field_error(key, &self.state)
    }
    fn field_error(&self, key: &str, state: &Map<String, Value>) -> Option<String> {
        self.document
            .all_nodes()
            .into_iter()
            .find_map(|node| match node {
                Node::Input {
                    key: field, rules, ..
                } if field == key => {
                    validation::error(rules, state.get(key).and_then(Value::as_str).unwrap_or(""))
                }
                _ => None,
            })
    }
    fn validate_form(&self, state: &Map<String, Value>) -> Result<()> {
        for node in self.document.all_nodes() {
            if let Node::Input { key, .. } = node
                && let Some(error) = self.field_error(key, state)
            {
                bail!("{key}: {error}");
            }
        }
        Ok(())
    }
    fn validate_reads(&self, value: &Value, state: &Map<String, Value>) -> Result<()> {
        if let Some(key) = value.get("get").and_then(Value::as_str)
            && let Some(error) = self.field_error(key, state)
        {
            bail!("{key}: {error}");
        }
        match value {
            Value::Object(object) => {
                for value in object.values() {
                    self.validate_reads(value, state)?;
                }
            }
            Value::Array(array) => {
                for value in array {
                    self.validate_reads(value, state)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn bindings_ok(&self, state: &mut Map<String, Value>) -> Result<()> {
        for node in self.document.all_nodes() {
            match node {
                Node::Input { key, .. } => input_ok(&state[key], node)?,
                Node::Toggle { key, .. } => {
                    boolean(&state[key])?;
                }
                Node::Checklist { items } => {
                    for item in items {
                        boolean(&state[&item.key])?;
                    }
                }
                Node::Select { key, options, .. } => {
                    ensure!(
                        state[key]
                            .as_str()
                            .is_some_and(|s| options.iter().any(|o| o == s)),
                        "select value must be an option"
                    );
                }
                Node::Stepper { key, min, max, .. } | Node::Slider { key, min, max, .. } => {
                    let value = number(&state[key])?.clamp(*min, *max);
                    state.insert(key.clone(), json!(value));
                }
                Node::Tabs {
                    key: Some(key),
                    items,
                } => {
                    let max = u32::try_from(items.len().saturating_sub(1))?;
                    let value = number(&state[key])?.clamp(0.0, f64::from(max));
                    state.insert(
                        key.clone(),
                        json!(
                            (0..=max)
                                .rev()
                                .find(|index| f64::from(*index) <= value)
                                .unwrap_or(0)
                        ),
                    );
                }
                Node::Editor { key, .. } => practice::code_ok(&state[key])?,
                Node::Choice { key, options, .. } => {
                    ensure!(
                        state[key]
                            .as_str()
                            .is_some_and(|s| s.is_empty() || options.iter().any(|o| o == s)),
                        "choice value must be empty or an option"
                    );
                }
                Node::Quiz {
                    key: Some(key),
                    items,
                } => {
                    let max = u32::try_from(items.len().saturating_sub(1))?;
                    let value = number(&state[key])?.clamp(0.0, f64::from(max));
                    state.insert(
                        key.clone(),
                        json!(
                            (0..=max)
                                .rev()
                                .find(|index| f64::from(*index) <= value)
                                .unwrap_or(0)
                        ),
                    );
                }
                Node::Rating { key, max, .. } => {
                    let value = number(&state[key])?.clamp(0.0, f64::from(*max));
                    state.insert(
                        key.clone(),
                        json!(
                            (0..=*max)
                                .rev()
                                .find(|index| f64::from(*index) <= value)
                                .unwrap_or(0)
                        ),
                    );
                }
                Node::Table {
                    select: Some(key),
                    rows_data,
                    ..
                } => {
                    let max = u32::try_from(rows_data.len().saturating_sub(1))?;
                    let value = number(&state[key])?.clamp(0.0, f64::from(max));
                    let row = (0..=max)
                        .rev()
                        .find(|row| f64::from(*row) <= value)
                        .unwrap_or(0);
                    state.insert(key.clone(), json!(row));
                }
                _ => {}
            }
        }
        Ok(())
    }
}
