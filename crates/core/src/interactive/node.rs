use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::check::bounded_rows;
use super::expr::line_ok;
use super::practice::{self, Question};
use super::{
    Branch, ChartKind, ChartSeries, DIAGRAM_ROWS, Lane, MAX_TEXT, MAX_VALUE, Section, Shape, Tab,
    TimelineEvent, components, extras,
};

/// Semantic text emphasis, supplied by the frontend's theme.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// Ordinary foreground text.
    #[default]
    Normal,
    /// Secondary information.
    Muted,
    /// Highlighted information.
    Accent,
    /// Positive result.
    Success,
    /// Caution or attention.
    Warning,
    /// Negative result or error.
    Error,
}

/// Optional document border style.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Frame {
    /// Rounded corners.
    #[default]
    Rounded,
    /// Double-line border.
    Double,
    /// Heavy border.
    Heavy,
    /// Dashed border.
    Dashed,
    /// Title rule without an enclosing border.
    Plain,
}

/// Theme role for document highlights; never an arbitrary color.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Accent {
    /// Primary theme accent.
    Accent,
    /// Assistant accent (the default).
    #[default]
    Accent2,
    /// Positive emphasis.
    Success,
    /// Caution.
    Warning,
    /// Negative emphasis.
    Error,
    /// Subdued emphasis.
    Muted,
}

/// Semantic marker on a comparison item.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mark {
    /// Positive item.
    Ok,
    /// Negative item.
    No,
    /// Neutral bullet.
    Dot,
    /// No marker.
    #[default]
    None,
}

/// A comparison item, with plain-text template content.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnItem {
    pub text: String,
    /// Optional semantic marker.
    #[serde(default)]
    pub mark: Mark,
}

/// One comparison column.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Column {
    /// Column title template.
    pub title: String,
    /// Up to eight items.
    pub items: Vec<ColumnItem>,
}

/// A bordered display card.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Card {
    /// Optional remote picture; never a template.
    pub image: Option<String>,
    /// Plain-text image description.
    pub alt: Option<String>,
    /// Title template.
    pub title: String,
    /// Body template.
    pub text: String,
    /// Optional badge template.
    pub badge: Option<String>,
    /// Title emphasis; absent uses the document accent.
    pub tone: Option<Tone>,
}

/// A labelled metric.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stat {
    /// Value template.
    pub value: String,
    /// Dim label template.
    pub label: String,
    /// Signed delta template, for example "+12%".
    pub delta: Option<String>,
    /// Value emphasis; absent uses the document accent.
    pub tone: Option<Tone>,
}

/// Progress along a display-only timeline.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Completed step.
    Done,
    /// Active step.
    Current,
    /// Future step.
    #[default]
    Todo,
}

/// One display-only timeline step (not an executable action).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayStep {
    /// Title template.
    pub title: String,
    /// Optional description template.
    pub text: Option<String>,
    /// Completion state.
    #[serde(default)]
    pub state: StepState,
}

/// Semantic color for a callout rail.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalloutTone {
    /// Informational theme accent.
    Info,
    /// Tip in the document accent.
    Tip,
    /// Warning theme role.
    Warning,
    /// Error theme role.
    Danger,
}

/// An action control; labels are templates, never markup.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Button {
    /// Optional picture above this tile's label.
    pub image: Option<String>,
    /// Plain-text image description.
    pub alt: Option<String>,
    /// Visible label (a sending action gets an additional frontend badge).
    pub label: String,
    /// Identifier in the document's actions map.
    pub action: String,
    /// Optional plain-text artwork or descriptive lines.
    #[serde(default)]
    pub body: Vec<String>,
    /// Prefer this item as the document's primary action.
    #[serde(default)]
    pub primary: bool,
}

/// One labelled numeric expression in a bar chart.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BarItem {
    /// Plain-text label template.
    pub label: String,
    /// Numeric expression.
    pub value: Value,
}

/// One locally toggleable boolean in a checklist.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckItem {
    /// Plain-text label template.
    pub label: String,
    /// Bound boolean state field.
    pub key: String,
}

/// Declarative presentation nodes. Declared rows count toward validation caps.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Node {
    /// A fixed-height remote image.
    Image {
        /// HTTPS URL, never a template.
        src: String,
        /// Plain-text description.
        #[serde(default)]
        alt: String,
        /// Reserved terminal rows, four to twelve.
        #[serde(default = "six")]
        rows: u16,
    },
    /// A prominent title, optional dim subtitle and badge.
    Hero {
        /// Optional remote picture.
        image: Option<String>,
        /// Plain-text image description.
        alt: Option<String>,
        /// Title template.
        title: String,
        /// Subtitle template.
        subtitle: Option<String>,
        /// Badge template.
        badge: Option<String>,
    },
    /// Two or three comparison columns, stacked on narrow terminals.
    Columns {
        /// Columns with up to eight items each.
        items: Vec<Column>,
    },
    /// One to six bordered sub-cards in a responsive grid.
    Cards {
        /// Display cards.
        items: Vec<Card>,
    },
    /// One to six labelled metrics.
    Stats {
        /// Display metrics.
        items: Vec<Stat>,
        /// Draw values in big display numerals (two extra rows).
        #[serde(default)]
        big: bool,
    },
    /// One to eight steps in a row or connected timeline.
    Steps {
        /// Display steps, not executable actions.
        items: Vec<DisplayStep>,
    },
    /// A semantic rail alongside short text.
    Callout {
        /// Rail emphasis.
        tone: CalloutTone,
        /// Optional title template.
        title: Option<String>,
        /// Body template.
        text: String,
    },
    /// A quotation with optional attribution.
    Quote {
        /// Quoted text template.
        text: String,
        /// Attribution template.
        by: Option<String>,
    },
    /// A separator rule, optionally labelled.
    Divider {
        /// Label template.
        label: Option<String>,
    },
    /// Plain text with `{{state_key}}` substitutions.
    Text {
        /// Plain-text template.
        text: String,
        /// Theme-provided semantic emphasis.
        #[serde(default)]
        tone: Tone,
        /// Declared region height.
        #[serde(default = "one")]
        rows: u16,
    },
    /// A row of locally executed action buttons.
    Buttons {
        /// Controls arranged within the declared region.
        items: Vec<Button>,
        /// Declared region height.
        #[serde(default = "three")]
        rows: u16,
    },
    /// A bounded string state editor, single-line or multiline.
    Input {
        /// Bound string state field.
        key: String,
        /// Visible field label.
        label: String,
        /// Optional bounded validation rules.
        #[serde(default)]
        rules: Map<String, Value>,
        /// Example text shown only while the field is empty.
        #[serde(default)]
        placeholder: String,
        /// Persistent help text, replaced by a validation error when invalid.
        #[serde(default)]
        hint: String,
        /// Visible text rows: one for an input, two to eight for a textarea.
        #[serde(default = "one")]
        rows: u16,
    },
    /// Aligned label/value template pairs.
    Kv {
        /// One to eight pairs.
        items: Vec<[String; 2]>,
    },
    /// A fixed table with optional local row selection.
    Table {
        /// One to six header templates.
        columns: Vec<String>,
        /// Up to eight rows of cell templates.
        #[serde(alias = "rows")]
        rows_data: Vec<Vec<String>>,
        /// Optional numeric state field holding the selected index.
        #[serde(default)]
        select: Option<String>,
        /// Enable local column sorting.
        #[serde(default)]
        sortable: bool,
        /// Visible page size, at most eight rows.
        page_size: Option<usize>,
        /// Enable local text filtering.
        #[serde(default)]
        search: bool,
    },
    /// A clamped numeric bar with a label.
    Progress {
        /// Plain-text label template.
        label: String,
        /// Numeric expression.
        value: Value,
        /// Numeric maximum; defaults to one.
        #[serde(default = "unit")]
        max: Value,
    },
    /// Horizontal bars scaled to their largest nonnegative value.
    Bars {
        /// One to eight labelled numeric expressions.
        items: Vec<BarItem>,
    },
    /// A fixed-height series of numeric expressions.
    Sparkline {
        /// One to sixty-four samples.
        values: Vec<Value>,
        /// Declared height, one to three rows.
        #[serde(default = "one")]
        rows: u16,
    },
    /// Independently toggleable boolean fields.
    Checklist {
        /// One to eight boolean controls.
        items: Vec<CheckItem>,
    },
    /// A string choice, cycled locally without actions.
    Select {
        /// Bound string state field.
        key: String,
        /// Plain-text label template.
        label: String,
        /// Two to eight allowed string values.
        options: Vec<String>,
    },
    /// A locally adjusted numeric field, clamped to a fixed range.
    Stepper {
        /// Bound numeric state field.
        key: String,
        /// Plain-text label template.
        label: String,
        /// Inclusive lower bound.
        min: f64,
        /// Inclusive upper bound.
        max: f64,
        /// Positive increment or decrement.
        step: f64,
    },
    /// Switchable child views.
    Tabs {
        /// One to eight labelled views.
        items: Vec<Tab>,
        /// Optional numeric selected-tab binding.
        key: Option<String>,
    },
    /// Independently expandable child views.
    Accordion {
        /// One to eight sections.
        items: Vec<Section>,
    },
    /// A terminal chart with themed series.
    Chart {
        /// Plot geometry.
        kind: ChartKind,
        /// One to six series of at most sixty-four samples.
        series: Vec<ChartSeries>,
        /// Optional sample labels.
        #[serde(default)]
        labels: Vec<String>,
        /// Fixed plot height, four to twelve rows.
        #[serde(default = "six")]
        rows: u16,
    },
    /// Two to four children side by side; stacks on narrow terminals.
    Row {
        /// Child nodes, left to right.
        items: Vec<Node>,
        /// Columns between children, zero to four.
        #[serde(default = "two_cols")]
        gap: u16,
    },
    /// Two to twelve children filling 2–4 columns row by row.
    Grid {
        /// Child nodes in reading order.
        items: Vec<Node>,
        /// Columns, two to four; fewer when the terminal is narrow.
        #[serde(default = "two_cols")]
        cols: u16,
    },
    /// One to eight children grouped vertically with their own spacing.
    Stack {
        /// Child nodes, top to bottom.
        items: Vec<Node>,
        /// Blank rows between children, zero to three.
        #[serde(default = "one")]
        gap: u16,
    },
    /// Blank vertical space.
    Spacer {
        /// Rows, one to four.
        #[serde(default = "one")]
        rows: u16,
    },
    /// One big display numeral with a label and optional delta.
    Number {
        /// Value template; leading digits draw in the display font.
        value: String,
        /// Label template.
        label: String,
        /// Signed delta template.
        delta: Option<String>,
        /// Value emphasis; absent uses the document accent.
        tone: Option<Tone>,
    },
    /// A half-ring gauge.
    Gauge {
        /// Label template.
        label: String,
        /// Numeric expression.
        value: Value,
        /// Numeric maximum.
        #[serde(default = "unit")]
        max: Value,
        /// Theme role; normal uses the panel accent.
        #[serde(default)]
        tone: Tone,
    },
    /// Rows of shaded cells, darker for larger values.
    Heatmap {
        /// One to twelve rows of up to 53 numeric expressions.
        values: Vec<Vec<Value>>,
        /// Optional row labels.
        #[serde(default)]
        labels: Vec<String>,
        /// Theme role; normal uses the panel accent.
        #[serde(default)]
        tone: Tone,
    },
    /// Markdown text clipped to a fixed number of rows.
    Md {
        /// Markdown template.
        text: String,
        /// Visible rows, one to sixteen.
        #[serde(default = "six")]
        rows: u16,
    },
    /// An expandable outline.
    Tree {
        /// Top-level branches; at most 24 in all, four levels deep.
        items: Vec<Branch>,
    },
    /// Kanban lanes of short cards.
    Board {
        /// Two to five lanes.
        items: Vec<Lane>,
        /// Card rows per lane, two to ten.
        #[serde(default = "six")]
        rows: u16,
    },
    /// A Mermaid diagram, drawn as an image where the terminal allows.
    Diagram {
        /// Mermaid source.
        source: String,
        /// Plain-text description.
        #[serde(default)]
        alt: String,
    },
    /// Shapes on a 0–100 grid with optional keyframe animation.
    Canvas {
        /// One to forty-eight shapes, drawn in order.
        shapes: Vec<Shape>,
        /// Fixed height, three to sixteen rows.
        #[serde(default = "eight_rows")]
        rows: u16,
        /// Plain-text description of what the drawing shows.
        #[serde(default)]
        alt: String,
    },
    /// Controls submitted together through a transactional action.
    Form {
        /// Group title.
        title: String,
        /// Bound controls.
        fields: Vec<Node>,
        /// Submit button label.
        submit_label: String,
        /// Named action.
        action: String,
    },
    /// A chronological event list.
    Timeline {
        /// One to eight events.
        items: Vec<TimelineEvent>,
    },
    /// A numeric star rating.
    Rating {
        /// Bound numeric field.
        key: String,
        /// Visible label.
        label: String,
        /// Maximum stars, one to ten.
        #[serde(default = "components::five")]
        max: u16,
    },
    /// Literal syntax-highlighted source with a copy control.
    Code {
        /// Literal source, not a template.
        source: String,
        /// Optional language token.
        lang: Option<String>,
    },
    /// Compact inline emphasis.
    Badge {
        /// Text template.
        text: String,
        /// Theme role.
        #[serde(default)]
        tone: Tone,
    },
    /// A clamped themed gauge.
    Meter {
        /// Label template.
        label: String,
        /// Numeric expression.
        value: Value,
        /// Numeric maximum.
        #[serde(default = "unit")]
        max: Value,
        /// Theme role.
        #[serde(default)]
        tone: Tone,
    },
    /// A numeric range control.
    Slider {
        /// Numeric binding.
        key: String,
        /// Label template.
        label: String,
        /// Lower bound.
        min: f64,
        /// Upper bound.
        max: f64,
        /// Positive step.
        step: f64,
    },
    /// A multi-line code editor bound to string state.
    Editor {
        /// Bound string field.
        key: String,
        /// Label template.
        label: String,
        /// Optional language token for highlighting.
        lang: Option<String>,
        /// Visible editing rows, three to sixteen.
        #[serde(default = "practice::eight")]
        rows: u16,
    },
    /// A vertical single-choice list that marks the right answer once picked.
    Choice {
        /// Bound string field; empty until something is picked.
        key: String,
        /// Label template.
        label: String,
        /// Two to eight option templates.
        options: Vec<String>,
        /// Optional correct option.
        answer: Option<String>,
    },
    /// Questions shown one at a time, each with an optional revealable answer.
    Quiz {
        /// One to twelve questions.
        items: Vec<Question>,
        /// Optional numeric current-question binding.
        key: Option<String>,
    },
    /// A locally toggleable boolean field.
    Toggle {
        /// Bound boolean state field.
        key: String,
        /// Plain-text label template.
        label: String,
    },
}
fn unit() -> Value {
    json!(1)
}
const fn one() -> u16 {
    1
}
const fn three() -> u16 {
    3
}
const fn six() -> u16 {
    6
}
const fn eight_rows() -> u16 {
    8
}
const fn two_cols() -> u16 {
    2
}

pub(super) fn image_ok(src: Option<&str>, alt: Option<&str>) -> Result<()> {
    if let Some(src) = src {
        crate::remote_image::validate_url(src)?;
    }
    if let Some(alt) = alt {
        line_ok(alt, 120)?;
        ensure!(
            !alt.contains(['{', '}']),
            "image descriptions cannot contain templates"
        );
    }
    Ok(())
}
impl Node {
    /// Whether this text field is a multiline `Textarea`.
    #[must_use]
    pub fn is_textarea(&self) -> bool {
        matches!(self, Self::Input { rows, .. } if *rows > 1)
    }

    /// Byte budget for a text field's value; `None` for other nodes.
    #[must_use]
    pub fn input_limit(&self) -> Option<usize> {
        matches!(self, Self::Input { .. }).then(|| {
            if self.is_textarea() {
                MAX_TEXT
            } else {
                MAX_VALUE
            }
        })
    }

    /// The tallest this node may be declared.
    pub(super) fn max_rows(&self) -> u16 {
        match self {
            Self::Columns { .. } => 27,
            Self::Cards { .. } => 30,
            Self::Image { .. }
            | Self::Table { .. }
            | Self::Chart { .. }
            | Self::Heatmap { .. }
            | Self::Board { .. }
            | Self::Diagram { .. } => 12,
            Self::Buttons { .. } => 14,
            Self::Steps { .. } | Self::Timeline { .. } | Self::Canvas { .. } | Self::Md { .. } => {
                16
            }
            Self::Tree { .. } => 24,
            Self::Hero { .. } | Self::Code { .. } | Self::Choice { .. } => 9,
            Self::Editor { .. } => 17,
            // Up to eight text rows plus the field's two border rows.
            Self::Input { .. } => 10,
            node if node.is_container() => 45,
            _ => 8,
        }
    }

    /// Whether this node lays out child views of its own.
    fn is_container(&self) -> bool {
        matches!(
            self,
            Self::Tabs { .. }
                | Self::Accordion { .. }
                | Self::Form { .. }
                | Self::Quiz { .. }
                | Self::Row { .. }
                | Self::Grid { .. }
                | Self::Stack { .. }
        )
    }

    pub(super) fn is_display(&self) -> bool {
        matches!(
            self,
            Self::Hero { .. }
                | Self::Image { .. }
                | Self::Columns { .. }
                | Self::Cards { .. }
                | Self::Stats { .. }
                | Self::Steps { .. }
                | Self::Callout { .. }
                | Self::Quote { .. }
                | Self::Divider { .. }
                | Self::Chart { .. }
                | Self::Canvas { .. }
                | Self::Number { .. }
                | Self::Gauge { .. }
                | Self::Heatmap { .. }
                | Self::Md { .. }
                | Self::Board { .. }
                | Self::Diagram { .. }
                | Self::Timeline { .. }
                | Self::Code { .. }
                | Self::Badge { .. }
                | Self::Meter { .. }
        )
    }

    /// Declared validation budget, independent of frontend layout.
    #[must_use]
    pub fn rows(&self) -> u16 {
        match self {
            Self::Columns { items } => items
                .iter()
                .map(|item| bounded_rows(item.items.len() + 1))
                .sum(),
            Self::Cards { items } => {
                bounded_rows(items.len().saturating_mul(4))
                    + if items.iter().any(|item| item.image.is_some()) {
                        6
                    } else {
                        0
                    }
            }
            Self::Steps { items } => bounded_rows(items.len().saturating_mul(2)),
            Self::Callout { .. } | Self::Quote { .. } => 2,
            Self::Buttons { rows, items } => {
                // A tile picture adds a fixed six-row band to its tile row.
                *rows
                    + if items.iter().any(|item| item.image.is_some()) {
                        6
                    } else {
                        0
                    }
            }
            Self::Image { rows, .. } | Self::Text { rows, .. } | Self::Sparkline { rows, .. } => {
                *rows
            }
            Self::Hero { image, .. } => {
                if image.is_some() {
                    9
                } else {
                    3
                }
            }
            Self::Stats { big, .. } => 3 + 2 * u16::from(*big),
            Self::Input { rows, .. } | Self::Board { rows, .. } => rows.saturating_add(2),
            Self::Kv { items } => u16::try_from(items.len()).unwrap_or(u16::MAX),
            Self::Table {
                rows_data,
                page_size,
                search,
                ..
            } => bounded_rows(
                page_size
                    .unwrap_or(rows_data.len())
                    .saturating_add(1)
                    .saturating_add(usize::from(*search))
                    .saturating_add(usize::from(page_size.is_some())),
            ),
            Self::Tabs { items, .. } => items
                .iter()
                .map(|item| components::view_rows(&item.view))
                .max()
                .unwrap_or(0)
                .saturating_add(1),
            Self::Accordion { items } => items.iter().fold(0u16, |rows, item| {
                rows.saturating_add(components::view_rows(&item.view).saturating_add(1))
            }),
            Self::Form { fields, .. } => components::view_rows(fields).saturating_add(2),
            // Containers budget their stacked worst case: a narrow terminal
            // stacks a row's children, never exceeding what was validated.
            Self::Row { items, .. } | Self::Grid { items, .. } => components::view_rows(items)
                .saturating_add(bounded_rows(items.len().saturating_sub(1))),
            Self::Stack { items, gap } => components::view_rows(items)
                .saturating_add(gap.saturating_mul(bounded_rows(items.len().saturating_sub(1)))),
            Self::Chart { rows, .. }
            | Self::Canvas { rows, .. }
            | Self::Spacer { rows }
            | Self::Md { rows, .. } => *rows,
            Self::Number { .. } | Self::Gauge { .. } => 5,
            Self::Heatmap { values, .. } => bounded_rows(values.len()),
            Self::Tree { items } => bounded_rows(extras::flatten(items).len()),
            Self::Diagram { .. } => DIAGRAM_ROWS,
            Self::Timeline { items } => bounded_rows(
                items
                    .iter()
                    .map(|item| 1 + usize::from(item.text.is_some()))
                    .sum(),
            ),
            Self::Code { source, .. } => bounded_rows(source.lines().count().max(1) + 1),
            Self::Editor { rows, .. } => rows.saturating_add(1),
            Self::Choice { options, .. } => bounded_rows(options.len() + 1),
            Self::Quiz { items, .. } => items
                .iter()
                .map(Question::rows)
                .max()
                .unwrap_or(0)
                .saturating_add(1),
            Self::Bars { items } => u16::try_from(items.len()).unwrap_or(u16::MAX),
            Self::Checklist { items } => u16::try_from(items.len()).unwrap_or(u16::MAX),
            Self::Rating { .. }
            | Self::Badge { .. }
            | Self::Meter { .. }
            | Self::Slider { .. }
            | Self::Divider { .. }
            | Self::Progress { .. }
            | Self::Select { .. }
            | Self::Stepper { .. }
            | Self::Toggle { .. } => 1,
        }
    }
}
