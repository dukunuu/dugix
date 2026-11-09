//! Phase-7 components: Number, Gauge, Heatmap, Md, Tree, Board and Diagram.
//! Display-only except Tree, whose branches expand like Accordion sections
//! (keyed by each branch's preorder index).

use super::{Document, MAX_TEXT, Node, Session, item_count, line_ok, text_ok};
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::Value;

/// Rows a Mermaid diagram reserves, matching the transcript's preview slot.
pub const DIAGRAM_ROWS: u16 = 12;
/// Most branches one tree may hold, all expanded.
const MAX_BRANCHES: usize = 24;

/// One outline entry, optionally holding children.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Branch {
    /// Label template.
    pub label: String,
    /// Child branches.
    #[serde(default)]
    pub items: Vec<Branch>,
    /// Initially expanded.
    #[serde(default)]
    pub open: bool,
}

/// One kanban column.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lane {
    /// Title template.
    pub title: String,
    /// Card texts: a literal list or an expression (for example a state list).
    pub items: Value,
    /// Theme role for the lane's rail.
    pub tone: Option<super::Tone>,
}

/// Every branch in preorder with its depth.
#[must_use]
pub fn flatten(branches: &[Branch]) -> Vec<(usize, &Branch)> {
    fn visit<'a>(branches: &'a [Branch], depth: usize, out: &mut Vec<(usize, &'a Branch)>) {
        for branch in branches {
            out.push((depth, branch));
            visit(&branch.items, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    visit(branches, 0, &mut out);
    out
}

impl Session {
    /// Evaluate a data expression against current state.
    /// # Errors
    /// Rejects invalid expressions.
    pub fn evaluate(&self, expression: &Value) -> Result<Value> {
        super::eval(expression, &self.state, &mut 4096)
    }
    /// Whether tree branch `branch` (preorder index) is expanded. A branch
    /// is visible when every ancestor is expanded.
    #[must_use]
    pub fn branch_visible(&self, node: usize, branch: usize) -> bool {
        let Some(Node::Tree { items }) = self.document.node(node) else {
            return false;
        };
        let flat = flatten(items);
        let Some(&(depth, _)) = flat.get(branch) else {
            return false;
        };
        // Walk back to each ancestor: the nearest earlier entry one level up.
        let mut wanted = depth;
        for index in (0..branch).rev() {
            if wanted == 0 {
                break;
            }
            if flat[index].0 == wanted - 1 {
                if !self.section_open(node, index) {
                    return false;
                }
                wanted -= 1;
            }
        }
        true
    }
}

impl Document {
    pub(super) fn validate_extra(&self, node: &Node, budget: &mut usize) -> Result<Option<usize>> {
        Ok(Some(match node {
            Node::Number {
                value,
                label,
                delta,
                ..
            } => {
                for text in [value, label].into_iter().chain(delta) {
                    line_ok(text, 120)?;
                    Self::template_ok(text)?;
                }
                0
            }
            Node::Gauge {
                label, value, max, ..
            } => {
                Self::template_ok(label)?;
                self.numeric_ok(value, budget)?;
                self.numeric_ok(max, budget)?;
                0
            }
            Node::Heatmap { values, labels, .. } => {
                item_count(values.len(), 1, 12)?;
                for row in values {
                    item_count(row.len(), 1, 53)?;
                    for value in row {
                        self.numeric_ok(value, budget)?;
                    }
                }
                ensure!(
                    labels.len() <= values.len(),
                    "one label per heatmap row at most"
                );
                for label in labels {
                    line_ok(label, 32)?;
                    Self::template_ok(label)?;
                }
                0
            }
            Node::Md { text, rows } => {
                text_ok(text, MAX_TEXT)?;
                Self::template_ok(text)?;
                ensure!((1..=16).contains(rows), "md rows must be 1–16");
                0
            }
            Node::Tree { items } => {
                let flat = flatten(items);
                item_count(flat.len(), 1, MAX_BRANCHES)?;
                for (depth, branch) in &flat {
                    ensure!(*depth < 4, "trees nest at most four levels");
                    line_ok(&branch.label, 120)?;
                    Self::template_ok(&branch.label)?;
                }
                flat.iter()
                    .filter(|(_, branch)| !branch.items.is_empty())
                    .count()
            }
            Node::Board { items, rows } => {
                item_count(items.len(), 2, 5)?;
                ensure!((2..=10).contains(rows), "board rows must be 2–10");
                for lane in items {
                    line_ok(&lane.title, 60)?;
                    Self::template_ok(&lane.title)?;
                    super::validate_expr(&lane.items, &self.state, budget, 0)?;
                }
                0
            }
            Node::Diagram { source, alt } => {
                text_ok(source, 8 * 1024)?;
                ensure!(
                    source.lines().count() <= 64,
                    "diagram source exceeds 64 lines"
                );
                line_ok(alt, 120)?;
                0
            }
            _ => return Ok(None),
        }))
    }
}

#[cfg(test)]
mod tests {
    use crate::interactive::{Document, Node, Session};

    #[test]
    fn tree_branches_hide_under_collapsed_ancestors() {
        let mut session = Session::new(
            Document::parse(
                r#"root = Panel("Files", [Tree([Branch("src", [Branch("app", [Branch("mod.rs")]), Branch("lib.rs")], true), Branch("README")])])"#,
            )
            .unwrap(),
        )
        .unwrap();
        // Preorder: src(0) app(1) mod.rs(2) lib.rs(3) README(4).
        assert!(session.branch_visible(0, 1), "src starts open");
        assert!(!session.branch_visible(0, 2), "app starts closed");
        assert!(session.branch_visible(0, 3));
        session.expand_section(0, 1).unwrap();
        assert!(session.branch_visible(0, 2));
        session.expand_section(0, 0).unwrap();
        assert!(
            !session.branch_visible(0, 2),
            "closing src hides everything under it"
        );
        assert!(session.branch_visible(0, 4));
        assert_eq!(session.document().control_count(), 2);
        let Node::Tree { .. } = &session.document().nodes()[0] else {
            panic!("tree")
        };
    }

    #[test]
    fn extras_validate_their_limits() {
        let wide = format!("Heatmap([[{}]])", vec!["0"; 54].join(", "));
        for (source, message) in [
            (r#"Md("x", 20)"#, "height exceeds its limit"),
            (r#"Board([Lane("A", ["x"])])"#, "2–5 items"),
            (wide.as_str(), "1–53 items"),
            (
                r#"Tree([Branch("a", [Branch("b", [Branch("c", [Branch("d", [Branch("e")])])])])])"#,
                "four levels",
            ),
        ] {
            let error = Document::parse(&format!("root = Panel(\"Bad\", [x])\nx = {source}"))
                .err()
                .unwrap_or_else(|| panic!("{source} should fail"))
                .to_string();
            assert!(error.contains(message), "{source}: {error}");
        }
    }
}
