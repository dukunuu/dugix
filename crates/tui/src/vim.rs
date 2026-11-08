//! Modal vim keys over a `tui_textarea::TextArea`: Normal, Insert, Visual and
//! Visual-line modes, operators (`d c y`) over motions, counts, `.` repeat, a
//! single unnamed register and change-sized undo. It holds no app state; the
//! caller asks [`Vim::key`] first and keeps its own handling for whatever
//! comes back as [`Outcome::Passthrough`]. See `docs/editor.md`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::{CursorMove, TextArea};

/// The db setting that stores the keymap (`vim` or `standard`).
pub const KEYMAP_SETTING: &str = "keymap";

/// The editing mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    /// Typing goes into the text; every key but Esc passes through.
    #[default]
    Insert,
    /// Keys are commands.
    Normal,
    /// Charwise selection.
    Visual,
    /// Linewise selection.
    VisualLine,
}

/// What the caller should do with a key after [`Vim::key`].
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Vim consumed it.
    Handled,
    /// Not vim's: run the caller's usual handling.
    Passthrough,
    /// Enter in Normal mode: submit, as Enter does without vim.
    Submit,
    /// Esc in Normal mode with nothing pending: leave, as Esc does without vim.
    Leave,
}

/// A (row, char column) buffer position.
type Pos = (usize, usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Delete,
    Change,
    Yank,
}

impl Op {
    fn key(self) -> char {
        match self {
            Op::Delete => 'd',
            Op::Change => 'c',
            Op::Yank => 'y',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Motion {
    Left,
    Right,
    Down,
    Up,
    WordForward(bool),
    WordEnd(bool),
    WordBack(bool),
    LineStart,
    FirstNonBlank,
    LineEnd,
    FileStart,
    FileEnd,
    ParagraphForward,
    ParagraphBack,
}

impl Motion {
    fn of(ch: char) -> Option<Self> {
        Some(match ch {
            'h' => Self::Left,
            'l' | ' ' => Self::Right,
            'j' => Self::Down,
            'k' => Self::Up,
            'w' => Self::WordForward(false),
            'W' => Self::WordForward(true),
            'e' => Self::WordEnd(false),
            'E' => Self::WordEnd(true),
            'b' => Self::WordBack(false),
            'B' => Self::WordBack(true),
            '0' => Self::LineStart,
            '^' => Self::FirstNonBlank,
            '$' => Self::LineEnd,
            'G' => Self::FileEnd,
            '}' => Self::ParagraphForward,
            '{' => Self::ParagraphBack,
            _ => return None,
        })
    }
}

/// Where a motion lands and how an operator treats the span it covers.
struct Target {
    pos: Pos,
    linewise: bool,
    inclusive: bool,
}

#[derive(Clone)]
struct Snapshot {
    lines: Vec<String>,
    cursor: Pos,
}

struct Edit {
    before: Snapshot,
    after: Snapshot,
}

/// Vim state for one text field.
#[derive(Default)]
pub struct Vim {
    mode: Mode,
    count: Option<usize>,
    op: Option<(Op, Option<usize>)>,
    prefix: Option<char>,
    /// Keys of the command in progress, for `.`.
    keys: Vec<KeyEvent>,
    /// A change that continued into Insert mode, still collecting keys.
    recording: Option<Vec<KeyEvent>>,
    last_change: Vec<KeyEvent>,
    /// The command just run was `u`, Ctrl+R or `.`: never recorded for `.`.
    skip_record: bool,
    replaying: bool,
    register: String,
    linewise: bool,
    anchor: Pos,
    want_col: Option<usize>,
    insert_start: Option<Snapshot>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// The buffer as vim last left it; any other change clears the undo stack.
    seen: Option<Vec<String>>,
}

fn snapshot(ta: &TextArea<'_>) -> Snapshot {
    Snapshot {
        lines: ta.lines().to_vec(),
        cursor: ta.cursor(),
    }
}

fn chars(ta: &TextArea<'_>) -> Vec<Vec<char>> {
    ta.lines()
        .iter()
        .map(|line| line.chars().collect())
        .collect()
}

fn jump(ta: &mut TextArea<'_>, (row, col): Pos) {
    let clamp = |n: usize| u16::try_from(n).unwrap_or(u16::MAX);
    ta.move_cursor(CursorMove::Jump(clamp(row), clamp(col)));
}

fn first_non_blank(line: &[char]) -> usize {
    line.iter()
        .position(|c| !c.is_whitespace())
        .unwrap_or(line.len())
}

/// The last column Normal mode may rest on.
fn last_col(line: &[char]) -> usize {
    line.len().saturating_sub(1)
}

/// Word class at a position: 0 blank (or the virtual newline at a line's
/// end), 1 keyword (or any non-blank for WORDs), 2 punctuation, 3 an empty
/// line, which is a word of its own.
fn class(text: &[Vec<char>], (row, col): Pos, big: bool) -> u8 {
    let line = &text[row];
    if line.is_empty() {
        return 3;
    }
    match line.get(col) {
        None => 0,
        Some(c) if c.is_whitespace() => 0,
        Some(c) if big || c.is_alphanumeric() || *c == '_' => 1,
        Some(_) => 2,
    }
}

fn next(text: &[Vec<char>], (row, col): Pos) -> Option<Pos> {
    if col < text[row].len() {
        Some((row, col + 1))
    } else if row + 1 < text.len() {
        Some((row + 1, 0))
    } else {
        None
    }
}

fn prev(text: &[Vec<char>], (row, col): Pos) -> Option<Pos> {
    if col > 0 {
        Some((row, col - 1))
    } else if row > 0 {
        Some((row - 1, text[row - 1].len()))
    } else {
        None
    }
}

fn word_forward(text: &[Vec<char>], from: Pos, big: bool) -> Pos {
    let start = class(text, from, big);
    let end = (text.len() - 1, text[text.len() - 1].len());
    let mut at = from;
    if start != 0 {
        loop {
            match next(text, at) {
                Some(n) if start != 3 && class(text, n, big) == start => at = n,
                Some(n) => {
                    at = n;
                    break;
                }
                None => return end,
            }
        }
    }
    while class(text, at, big) == 0 {
        match next(text, at) {
            Some(n) => at = n,
            None => return end,
        }
    }
    at
}

fn word_end(text: &[Vec<char>], from: Pos, big: bool) -> Pos {
    let Some(mut at) = next(text, from) else {
        return from;
    };
    while matches!(class(text, at, big), 0 | 3) {
        match next(text, at) {
            Some(n) => at = n,
            None => return from,
        }
    }
    let cls = class(text, at, big);
    while let Some(n) = next(text, at) {
        if class(text, n, big) != cls {
            break;
        }
        at = n;
    }
    at
}

fn word_back(text: &[Vec<char>], from: Pos, big: bool) -> Pos {
    let Some(mut at) = prev(text, from) else {
        return from;
    };
    while class(text, at, big) == 0 {
        match prev(text, at) {
            Some(p) => at = p,
            None => return at,
        }
    }
    let cls = class(text, at, big);
    if cls == 3 {
        return at;
    }
    while let Some(p) = prev(text, at) {
        if class(text, p, big) != cls {
            break;
        }
        at = p;
    }
    at
}

/// Characters between two ordered positions, newlines counted as one.
fn span_len(text: &[Vec<char>], a: Pos, b: Pos) -> usize {
    if a.0 == b.0 {
        return b.1.saturating_sub(a.1);
    }
    let mut n = text[a.0].len() - a.1 + 1;
    for line in &text[a.0 + 1..b.0] {
        n += line.len() + 1;
    }
    n + b.1
}

fn span_text(text: &[Vec<char>], a: Pos, b: Pos) -> String {
    if a.0 == b.0 {
        return text[a.0][a.1..b.1].iter().collect();
    }
    let mut out: String = text[a.0][a.1..].iter().collect();
    for line in &text[a.0 + 1..b.0] {
        out.push('\n');
        out.extend(line);
    }
    out.push('\n');
    out.extend(&text[b.0][..b.1]);
    out
}

fn is_plain(key: KeyEvent) -> bool {
    !key.modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

impl Vim {
    /// The current mode.
    #[must_use]
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The mode's status label.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self.mode {
            Mode::Insert => "INSERT",
            Mode::Normal => "NORMAL",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
        }
    }

    /// Back to Insert with nothing pending, for a buffer that was replaced
    /// (sent, cleared, recalled). The register and `.` survive.
    pub fn reset(&mut self) {
        self.mode = Mode::Insert;
        self.clear_pending();
        self.recording = None;
        self.insert_start = None;
        self.undo.clear();
        self.redo.clear();
        self.seen = None;
    }

    /// The Visual selection as an ordered, end-exclusive range to paint.
    #[must_use]
    pub fn visual_range(&self, ta: &TextArea<'_>) -> Option<(Pos, Pos)> {
        let text = chars(ta);
        let cursor = ta.cursor();
        match self.mode {
            Mode::Visual => {
                let (a, b) = order(self.anchor, cursor);
                Some((a, (b.0, (b.1 + 1).min(text[b.0].len()))))
            }
            Mode::VisualLine => {
                let (r0, r1) = (self.anchor.0.min(cursor.0), self.anchor.0.max(cursor.0));
                Some(((r0, 0), (r1, text[r1].len())))
            }
            _ => None,
        }
    }

    /// Handle one key. Insert mode passes everything but Esc through.
    pub fn key(&mut self, ta: &mut TextArea<'_>, key: KeyEvent) -> Outcome {
        if self.mode == Mode::Insert {
            return self.insert_key(ta, key);
        }
        if let Some(seen) = &self.seen
            && seen.as_slice() != ta.lines()
        {
            self.undo.clear();
            self.redo.clear();
        }
        let outcome = self.normal_key(ta, key);
        self.seen = Some(ta.lines().to_vec());
        outcome
    }

    fn insert_key(&mut self, ta: &mut TextArea<'_>, key: KeyEvent) -> Outcome {
        if self.insert_start.is_none() {
            self.insert_start = Some(snapshot(ta));
        }
        if let Some(keys) = &mut self.recording {
            keys.push(key);
        }
        if key.code != KeyCode::Esc || !is_plain(key) {
            return Outcome::Passthrough;
        }
        if let Some(before) = self.insert_start.take() {
            self.record(before, ta);
        }
        if let Some(keys) = self.recording.take()
            && !self.replaying
        {
            self.last_change = keys;
        }
        self.mode = Mode::Normal;
        let (row, col) = ta.cursor();
        if col > 0 {
            jump(ta, (row, col - 1));
        }
        self.seen = Some(ta.lines().to_vec());
        Outcome::Handled
    }

    fn pending(&self) -> bool {
        self.count.is_some() || self.op.is_some() || self.prefix.is_some()
    }

    fn clear_pending(&mut self) {
        self.count = None;
        self.op = None;
        self.prefix = None;
        self.keys.clear();
    }

    fn normal_key(&mut self, ta: &mut TextArea<'_>, key: KeyEvent) -> Outcome {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::SUPER)
            || (ctrl && key.code != KeyCode::Char('r'))
        {
            return Outcome::Passthrough;
        }
        let visual = matches!(self.mode, Mode::Visual | Mode::VisualLine);
        let ch = match key.code {
            KeyCode::Char('r') if ctrl => {
                self.clear_pending();
                self.redo(ta);
                return Outcome::Handled;
            }
            KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::F(_)
            | KeyCode::PageUp
            | KeyCode::PageDown => {
                return Outcome::Passthrough;
            }
            KeyCode::Esc if self.pending() || visual => {
                self.clear_pending();
                self.mode = Mode::Normal;
                return Outcome::Handled;
            }
            KeyCode::Esc => return Outcome::Leave,
            KeyCode::Enter if self.pending() || visual => {
                self.clear_pending();
                return Outcome::Handled;
            }
            KeyCode::Enter => return Outcome::Submit,
            KeyCode::Char(c) => c,
            KeyCode::Left | KeyCode::Backspace => 'h',
            KeyCode::Right => 'l',
            KeyCode::Up => 'k',
            KeyCode::Down => 'j',
            KeyCode::Home => '0',
            KeyCode::Delete => 'x',
            KeyCode::End => '$',
            _ => {
                self.clear_pending();
                return Outcome::Handled;
            }
        };
        self.keys.push(key);
        if !matches!(ch, 'j' | 'k' | '0'..='9') {
            self.want_col = None;
        }
        let before = snapshot(ta);
        self.skip_record = false;
        if visual {
            self.visual_command(ta, ch);
        } else {
            self.command(ta, ch);
        }
        if self.mode == Mode::Insert {
            self.insert_start = Some(before);
            self.recording = Some(std::mem::take(&mut self.keys));
            if self.replaying || visual {
                self.recording = None;
            }
        } else if !self.pending() {
            let changed = before.lines != ta.lines();
            let keys = std::mem::take(&mut self.keys);
            if changed && !self.skip_record && !visual && !self.replaying {
                self.last_change = keys;
            }
            if !self.skip_record {
                self.record(before, ta);
            }
            self.clamp(ta);
        }
        Outcome::Handled
    }

    fn record(&mut self, before: Snapshot, ta: &TextArea<'_>) {
        if self.replaying || before.lines == ta.lines() {
            return;
        }
        self.undo.push(Edit {
            before,
            after: snapshot(ta),
        });
        self.redo.clear();
    }

    fn undo(&mut self, ta: &mut TextArea<'_>) {
        if let Some(edit) = self.undo.pop() {
            ta.set_lines(edit.before.lines.clone(), edit.before.cursor);
            self.redo.push(edit);
        }
    }

    fn redo(&mut self, ta: &mut TextArea<'_>) {
        if let Some(edit) = self.redo.pop() {
            ta.set_lines(edit.after.lines.clone(), edit.after.cursor);
            self.undo.push(edit);
        }
    }

    /// Normal mode never rests past the last character.
    fn clamp(&self, ta: &mut TextArea<'_>) {
        if self.mode == Mode::Insert {
            return;
        }
        let text = chars(ta);
        let (row, col) = ta.cursor();
        let max = last_col(&text[row]);
        if col > max {
            jump(ta, (row, max));
        }
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1)
    }

    fn command(&mut self, ta: &mut TextArea<'_>, ch: char) {
        if let Some(prefix) = self.prefix.take() {
            self.prefixed(ta, prefix, ch);
            return;
        }
        if let Some(digit) = ch.to_digit(10)
            && (digit != 0 || self.count.is_some())
        {
            let count = self.count.unwrap_or(0).saturating_mul(10);
            self.count = Some(count.saturating_add(digit as usize));
            return;
        }
        if let Some((op, op_count)) = self.op {
            self.operator_pending(ta, op, op_count, ch);
            return;
        }
        match ch {
            'd' | 'c' | 'y' => {
                let op = match ch {
                    'd' => Op::Delete,
                    'c' => Op::Change,
                    _ => Op::Yank,
                };
                self.op = Some((op, self.count.take()));
            }
            'g' | 'r' => self.prefix = Some(ch),
            'x' => self.shorthand(ta, Op::Delete, Motion::Right),
            'X' => self.shorthand(ta, Op::Delete, Motion::Left),
            'D' => self.shorthand(ta, Op::Delete, Motion::LineEnd),
            'C' => self.shorthand(ta, Op::Change, Motion::LineEnd),
            's' => self.shorthand(ta, Op::Change, Motion::Right),
            'S' => {
                let count = self.take_count();
                self.lines_op(ta, Op::Change, count);
            }
            'Y' => {
                let count = self.take_count();
                self.lines_op(ta, Op::Yank, count);
            }
            'p' | 'P' => {
                let count = self.take_count();
                self.put(ta, ch == 'P', count);
            }
            'J' => {
                let count = self.take_count();
                Self::join(ta, count.max(2) - 1);
            }
            '~' => {
                let count = self.take_count();
                Self::toggle_case(ta, count);
            }
            'u' => {
                for _ in 0..self.take_count() {
                    self.undo(ta);
                }
                self.skip_record = true;
            }
            '.' => {
                let count = self.count.take();
                self.repeat(ta, count);
                self.skip_record = true;
            }
            'v' | 'V' => {
                self.count = None;
                self.anchor = ta.cursor();
                self.mode = if ch == 'v' {
                    Mode::Visual
                } else {
                    Mode::VisualLine
                };
            }
            'i' | 'a' | 'I' | 'A' | 'o' | 'O' => self.insert(ta, ch),
            _ => {
                if let Some(motion) = Motion::of(ch) {
                    let count = self.count.take();
                    let target = self.target(ta, motion, count, false);
                    self.move_to(ta, motion, target.pos);
                } else {
                    self.clear_pending();
                }
            }
        }
    }

    fn prefixed(&mut self, ta: &mut TextArea<'_>, prefix: char, ch: char) {
        match (prefix, ch) {
            ('g', 'g') => {
                if let Some((op, op_count)) = self.op.take() {
                    let count = combine(op_count, self.count.take());
                    self.apply(ta, op, Motion::FileStart, count);
                } else {
                    let count = self.count.take();
                    let target = self.target(ta, Motion::FileStart, count, false);
                    self.move_to(ta, Motion::FileStart, target.pos);
                }
            }
            ('r', c) => {
                let count = self.take_count();
                Self::replace_chars(ta, c, count);
            }
            _ => self.clear_pending(),
        }
    }

    fn operator_pending(
        &mut self,
        ta: &mut TextArea<'_>,
        op: Op,
        op_count: Option<usize>,
        ch: char,
    ) {
        if ch == 'g' {
            self.prefix = Some('g');
            return;
        }
        self.op = None;
        let count = combine(op_count, self.count.take());
        if ch == op.key() {
            self.lines_op(ta, op, count.unwrap_or(1).max(1));
        } else if let Some(motion) = Motion::of(ch) {
            self.apply(ta, op, motion, count);
        } else {
            self.clear_pending();
        }
    }

    fn shorthand(&mut self, ta: &mut TextArea<'_>, op: Op, motion: Motion) {
        let count = self.count.take();
        self.apply(ta, op, motion, count);
    }

    fn move_to(&mut self, ta: &mut TextArea<'_>, motion: Motion, pos: Pos) {
        if motion == Motion::LineEnd {
            self.want_col = Some(usize::MAX);
        }
        jump(ta, pos);
        self.clamp(ta);
    }

    /// Where `motion` lands from the cursor. `op` widens `l`/`$` to the
    /// line's end so operators reach the last character.
    fn target(
        &mut self,
        ta: &TextArea<'_>,
        motion: Motion,
        count: Option<usize>,
        op: bool,
    ) -> Target {
        let text = chars(ta);
        let (row, col) = ta.cursor();
        let n = count.unwrap_or(1).max(1);
        let last_row = text.len() - 1;
        let line = |r: usize| &text[r];
        let charwise = |pos: Pos, inclusive: bool| Target {
            pos,
            linewise: false,
            inclusive,
        };
        let lines = |r: usize| Target {
            pos: (r, first_non_blank(line(r))),
            linewise: true,
            inclusive: false,
        };
        match motion {
            Motion::Left => charwise((row, col.saturating_sub(n)), false),
            Motion::Right => {
                let max = if op {
                    line(row).len()
                } else {
                    last_col(line(row))
                };
                charwise((row, (col + n).min(max)), false)
            }
            Motion::Down | Motion::Up => {
                let r = if motion == Motion::Down {
                    (row + n).min(last_row)
                } else {
                    row.saturating_sub(n)
                };
                let want = *self.want_col.get_or_insert(col);
                Target {
                    pos: (r, want.min(last_col(line(r)))),
                    linewise: true,
                    inclusive: false,
                }
            }
            Motion::WordForward(big) => {
                let mut at = (row, col);
                for _ in 0..n {
                    at = word_forward(&text, at, big);
                }
                // `dw` on a line's last word stops at its end, not the next line.
                if op && at.0 > row && at.1 <= first_non_blank(line(at.0)) {
                    let mut r = at.0 - 1;
                    while r > row && line(r).is_empty() {
                        r -= 1;
                    }
                    at = (r, line(r).len());
                }
                charwise(at, false)
            }
            Motion::WordEnd(big) => {
                let mut at = (row, col);
                for _ in 0..n {
                    at = word_end(&text, at, big);
                }
                charwise(at, true)
            }
            Motion::WordBack(big) => {
                let mut at = (row, col);
                for _ in 0..n {
                    at = word_back(&text, at, big);
                }
                charwise(at, false)
            }
            Motion::LineStart => charwise((row, 0), false),
            Motion::FirstNonBlank => charwise((row, first_non_blank(line(row))), false),
            Motion::LineEnd => {
                let r = (row + n - 1).min(last_row);
                charwise((r, last_col(line(r))), true)
            }
            Motion::FileStart => lines(count.map_or(0, |c| c - 1).min(last_row)),
            Motion::FileEnd => lines(count.map_or(last_row, |c| c - 1).min(last_row)),
            Motion::ParagraphForward | Motion::ParagraphBack => {
                let forward = motion == Motion::ParagraphForward;
                let mut r = row;
                for _ in 0..n {
                    r = paragraph(&text, r, forward);
                }
                let pos = if forward && r == last_row && !line(r).is_empty() {
                    (r, line(r).len())
                } else {
                    (r, 0)
                };
                charwise(pos, false)
            }
        }
    }

    fn apply(&mut self, ta: &mut TextArea<'_>, op: Op, motion: Motion, count: Option<usize>) {
        let from = ta.cursor();
        let text = chars(ta);
        // `cw` on a word changes to its end, like `ce`.
        let motion = match motion {
            Motion::WordForward(big) if op == Op::Change && class(&text, from, big) != 0 => {
                Motion::WordEnd(big)
            }
            m => m,
        };
        let cw_end = op == Op::Change && matches!(motion, Motion::WordEnd(_));
        let target = if cw_end && count.unwrap_or(1) == 1 && is_word_end(&text, from) {
            Target {
                pos: from,
                linewise: false,
                inclusive: true,
            }
        } else {
            self.target(ta, motion, count, true)
        };
        if target.linewise {
            let (r0, r1) = (from.0.min(target.pos.0), from.0.max(target.pos.0));
            self.rows(ta, op, r0, r1);
            return;
        }
        let (a, b) = order(from, target.pos);
        let b = if target.inclusive {
            (b.0, (b.1 + 1).min(text[b.0].len()))
        } else {
            b
        };
        self.chars_op(ta, op, a, b);
    }

    /// `dd`, `cc`, `yy` with a count.
    fn lines_op(&mut self, ta: &mut TextArea<'_>, op: Op, count: usize) {
        let row = ta.cursor().0;
        let last = ta.lines().len() - 1;
        self.rows(ta, op, row, (row + count - 1).min(last));
    }

    fn chars_op(&mut self, ta: &mut TextArea<'_>, op: Op, a: Pos, b: Pos) {
        let text = chars(ta);
        let len = span_len(&text, a, b);
        if len > 0 {
            self.register = span_text(&text, a, b);
            self.linewise = false;
        }
        jump(ta, a);
        if op != Op::Yank && len > 0 {
            ta.cancel_selection();
            ta.delete_str(len);
        }
        if op == Op::Change {
            self.mode = Mode::Insert;
        }
    }

    fn rows(&mut self, ta: &mut TextArea<'_>, op: Op, r0: usize, r1: usize) {
        let text = chars(ta);
        let mut yank: String = text[r0..=r1]
            .iter()
            .map(|line| line.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        yank.push('\n');
        self.register = yank;
        self.linewise = true;
        ta.cancel_selection();
        let body = span_len(&text, (r0, 0), (r1, text[r1].len()));
        match op {
            Op::Yank => {
                let col = if ta.cursor().0 == r0 {
                    ta.cursor().1
                } else {
                    0
                };
                jump(ta, (r0, col));
            }
            Op::Change => {
                let indent = first_non_blank(&text[r0]);
                jump(ta, (r0, indent));
                ta.delete_str(body - indent);
                self.mode = Mode::Insert;
            }
            Op::Delete => {
                if r1 + 1 < text.len() {
                    jump(ta, (r0, 0));
                    ta.delete_str(body + 1);
                } else if r0 > 0 {
                    jump(ta, (r0 - 1, text[r0 - 1].len()));
                    ta.delete_str(body + 1);
                } else {
                    jump(ta, (0, 0));
                    ta.delete_str(body);
                }
                let rest = chars(ta);
                let row = r0.min(rest.len() - 1);
                jump(ta, (row, first_non_blank(&rest[row])));
            }
        }
    }

    fn put(&mut self, ta: &mut TextArea<'_>, before: bool, count: usize) {
        if self.register.is_empty() {
            return;
        }
        let text = chars(ta);
        let (row, col) = ta.cursor();
        let repeated = self.register.repeat(count);
        ta.cancel_selection();
        if self.linewise {
            let body = repeated.strip_suffix('\n').unwrap_or(&repeated);
            let row = if before {
                jump(ta, (row, 0));
                ta.insert_str(format!("{body}\n"));
                row
            } else {
                jump(ta, (row, text[row].len()));
                ta.insert_str(format!("\n{body}"));
                row + 1
            };
            let rest = chars(ta);
            jump(ta, (row, first_non_blank(&rest[row])));
        } else {
            if !before && !text[row].is_empty() {
                jump(ta, (row, col + 1));
            }
            ta.insert_str(&repeated);
            ta.move_cursor(CursorMove::Back);
        }
    }

    fn join(ta: &mut TextArea<'_>, joins: usize) {
        for _ in 0..joins {
            let text = chars(ta);
            let row = ta.cursor().0;
            if row + 1 >= text.len() {
                break;
            }
            let here = &text[row];
            let next = &text[row + 1];
            let blank = first_non_blank(next);
            jump(ta, (row, here.len()));
            ta.delete_str(1 + blank);
            let space = !here.is_empty()
                && blank < next.len()
                && here.last() != Some(&' ')
                && next[blank] != ')';
            if space {
                ta.insert_char(' ');
            }
            jump(ta, (row, here.len()));
        }
    }

    fn toggle_case(ta: &mut TextArea<'_>, count: usize) {
        let text = chars(ta);
        let (row, col) = ta.cursor();
        let end = (col + count).min(text[row].len());
        if end == col {
            return;
        }
        let swapped: String = text[row][col..end]
            .iter()
            .flat_map(|c| {
                if c.is_uppercase() {
                    c.to_lowercase().collect::<Vec<_>>()
                } else {
                    c.to_uppercase().collect()
                }
            })
            .collect();
        ta.cancel_selection();
        ta.delete_str(end - col);
        ta.insert_str(swapped);
        jump(ta, (row, end.min(last_col(&text[row]))));
    }

    fn replace_chars(ta: &mut TextArea<'_>, ch: char, count: usize) {
        let text = chars(ta);
        let (row, col) = ta.cursor();
        if col + count > text[row].len() {
            return;
        }
        ta.cancel_selection();
        ta.delete_str(count);
        ta.insert_str(ch.to_string().repeat(count));
        jump(ta, (row, col + count - 1));
    }

    fn insert(&mut self, ta: &mut TextArea<'_>, ch: char) {
        self.count = None;
        let text = chars(ta);
        let (row, col) = ta.cursor();
        let line = &text[row];
        match ch {
            'a' if !line.is_empty() => jump(ta, (row, col + 1)),
            'I' => jump(ta, (row, first_non_blank(line))),
            'A' => jump(ta, (row, line.len())),
            'o' => {
                jump(ta, (row, line.len()));
                ta.insert_newline();
            }
            'O' => {
                jump(ta, (row, 0));
                ta.insert_newline();
                jump(ta, (row, 0));
            }
            _ => {}
        }
        self.mode = Mode::Insert;
    }

    /// Replay the last change as one undo step; a count replaces its count.
    fn repeat(&mut self, ta: &mut TextArea<'_>, count: Option<usize>) {
        let mut keys = self.last_change.clone();
        if keys.is_empty() {
            return;
        }
        if let Some(count) = count {
            let digits = keys
                .iter()
                .take_while(|k| matches!(k.code, KeyCode::Char(c) if c.is_ascii_digit()))
                .count();
            keys.drain(..digits);
            let prefix: Vec<KeyEvent> = count
                .to_string()
                .chars()
                .map(|c| KeyEvent::from(KeyCode::Char(c)))
                .collect();
            keys.splice(0..0, prefix);
        }
        let before = snapshot(ta);
        self.replaying = true;
        for key in keys {
            if self.mode == Mode::Insert && key.code != KeyCode::Esc {
                ta.input(key);
            } else {
                self.key(ta, key);
            }
        }
        if self.mode == Mode::Insert {
            self.key(ta, KeyEvent::from(KeyCode::Esc));
        }
        self.replaying = false;
        self.record(before, ta);
    }

    fn visual_command(&mut self, ta: &mut TextArea<'_>, ch: char) {
        if let Some(digit) = ch.to_digit(10)
            && (digit != 0 || self.count.is_some())
        {
            let count = self.count.unwrap_or(0).saturating_mul(10);
            self.count = Some(count.saturating_add(digit as usize));
            return;
        }
        if self.prefix.take() == Some('g') {
            if ch == 'g' {
                let count = self.count.take();
                let target = self.target(ta, Motion::FileStart, count, false);
                self.move_to(ta, Motion::FileStart, target.pos);
            }
            return;
        }
        let linewise = self.mode == Mode::VisualLine;
        match ch {
            'g' => self.prefix = Some('g'),
            'v' | 'V' => {
                let next = if ch == 'v' {
                    Mode::Visual
                } else {
                    Mode::VisualLine
                };
                self.mode = if self.mode == next {
                    Mode::Normal
                } else {
                    next
                };
            }
            'o' => {
                let cursor = ta.cursor();
                jump(ta, self.anchor);
                self.anchor = cursor;
            }
            'd' | 'x' | 'c' | 's' | 'y' => {
                let op = match ch {
                    'd' | 'x' => Op::Delete,
                    'c' | 's' => Op::Change,
                    _ => Op::Yank,
                };
                self.mode = Mode::Normal;
                self.visual_op(ta, op, linewise);
            }
            'p' | 'P' => {
                // The selection goes; the register keeps what was yanked.
                let (register, was_linewise) = (self.register.clone(), self.linewise);
                let top = self.anchor.0.min(ta.cursor().0);
                self.mode = Mode::Normal;
                self.visual_op(ta, Op::Delete, linewise);
                self.register = register;
                self.linewise = was_linewise;
                // Deleting the last lines leaves the cursor above the gap.
                let before = !linewise || ta.cursor().0 >= top;
                self.put(ta, before, 1);
            }
            _ => {
                if let Some(motion) = Motion::of(ch) {
                    let count = self.count.take();
                    let target = self.target(ta, motion, count, false);
                    self.move_to(ta, motion, target.pos);
                }
            }
        }
        self.count = None;
    }

    fn visual_op(&mut self, ta: &mut TextArea<'_>, op: Op, linewise: bool) {
        let cursor = ta.cursor();
        let (a, b) = order(self.anchor, cursor);
        if linewise {
            self.rows(ta, op, a.0, b.0);
            if op == Op::Yank {
                jump(ta, (a.0, 0));
            }
        } else {
            let text = chars(ta);
            let end = (b.0, (b.1 + 1).min(text[b.0].len()));
            self.chars_op(ta, op, a, end);
        }
    }
}

/// An operator's count times its motion's, or None when neither was typed.
fn combine(a: Option<usize>, b: Option<usize>) -> Option<usize> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(1) * b.unwrap_or(1)),
    }
}

fn order(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b { (a, b) } else { (b, a) }
}

fn is_word_end(text: &[Vec<char>], at: Pos) -> bool {
    let cls = class(text, at, false);
    cls != 0 && next(text, at).is_none_or(|n| class(text, n, false) != cls)
}

/// The next (or previous) blank line beyond the current paragraph.
fn paragraph(text: &[Vec<char>], row: usize, forward: bool) -> usize {
    let last = text.len() - 1;
    let step = |r: usize| {
        if forward {
            (r < last).then(|| r + 1)
        } else {
            r.checked_sub(1)
        }
    };
    let mut r = row;
    while let Some(n) = step(r) {
        r = n;
        if !text[r].is_empty() {
            break;
        }
    }
    while let Some(n) = step(r) {
        r = n;
        if text[r].is_empty() {
            return r;
        }
    }
    r
}

#[cfg(test)]
#[path = "vim_tests.rs"]
mod tests;
