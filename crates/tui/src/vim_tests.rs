use super::{Mode, Outcome, Vim};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tui_textarea::{CursorMove, TextArea};

/// Keys in vim notation: plain chars, `<Esc>`, `<CR>`, `<BS>`, `<C-r>`, `<C-n>`.
fn keys(spec: &str) -> Vec<KeyEvent> {
    let mut out = Vec::new();
    let mut rest = spec;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(end) = rest.find('>')
        {
            let name = &rest[1..end];
            out.push(match name {
                "Esc" => KeyEvent::from(KeyCode::Esc),
                "CR" => KeyEvent::from(KeyCode::Enter),
                "BS" => KeyEvent::from(KeyCode::Backspace),
                "Up" => KeyEvent::from(KeyCode::Up),
                _ => {
                    let ch = name.strip_prefix("C-").and_then(|k| k.chars().next());
                    KeyEvent::new(KeyCode::Char(ch.unwrap()), KeyModifiers::CONTROL)
                }
            });
            rest = &rest[end + 1..];
            continue;
        }
        out.push(KeyEvent::from(KeyCode::Char(c)));
        rest = &rest[c.len_utf8()..];
    }
    out
}

fn area(text: &str, (row, col): (usize, usize)) -> TextArea<'static> {
    let mut ta = TextArea::new(text.split('\n').map(str::to_owned).collect());
    ta.move_cursor(CursorMove::Jump(
        u16::try_from(row).unwrap(),
        u16::try_from(col).unwrap(),
    ));
    ta
}

/// Feed keys like the composer does: vim first, passthrough keys typed.
fn feed(vim: &mut Vim, ta: &mut TextArea<'_>, spec: &str) -> Vec<Outcome> {
    keys(spec)
        .into_iter()
        .map(|key| {
            let outcome = vim.key(ta, key);
            if outcome == Outcome::Passthrough {
                ta.input(key);
            }
            outcome
        })
        .collect()
}

fn normal() -> Vim {
    let mut vim = Vim::default();
    let mut scratch = TextArea::default();
    vim.key(&mut scratch, KeyEvent::from(KeyCode::Esc));
    assert_eq!(vim.mode(), Mode::Normal);
    vim
}

fn run(text: &str, cursor: (usize, usize), spec: &str) -> (String, (usize, usize)) {
    let mut vim = normal();
    let mut ta = area(text, cursor);
    feed(&mut vim, &mut ta, spec);
    (ta.lines().join("\n"), ta.cursor())
}

#[track_caller]
fn check(text: &str, cursor: (usize, usize), spec: &str, want: &str, want_cursor: (usize, usize)) {
    let got = run(text, cursor, spec);
    assert_eq!(got, (want.to_string(), want_cursor), "{spec:?} on {text:?}");
}

#[test]
fn word_motions_follow_vim_word_classes() {
    let text = "foo.bar baz";
    check(text, (0, 0), "w", text, (0, 3));
    check(text, (0, 0), "ww", text, (0, 4));
    check(text, (0, 0), "3w", text, (0, 8));
    check(text, (0, 0), "W", text, (0, 8));
    check(text, (0, 0), "e", text, (0, 2));
    check(text, (0, 0), "E", text, (0, 6));
    check(text, (0, 10), "b", text, (0, 8));
    check(text, (0, 10), "bb", text, (0, 4));
    check(text, (0, 10), "B", text, (0, 8));
    check(text, (0, 8), "B", text, (0, 0));
    check("ab\n\ncd", (0, 0), "w", "ab\n\ncd", (1, 0));
    check("ab\n\ncd", (0, 0), "ww", "ab\n\ncd", (2, 0));
    check("ab\n  cd", (1, 2), "b", "ab\n  cd", (0, 0));
}

#[test]
fn line_and_file_motions() {
    let text = "  one\ntwo two\nthree";
    check(text, (0, 4), "0", text, (0, 0));
    check(text, (0, 0), "^", text, (0, 2));
    check(text, (1, 0), "$", text, (1, 6));
    check(text, (2, 3), "gg", text, (0, 2));
    check(text, (0, 0), "G", text, (2, 0));
    check(text, (0, 0), "2G", text, (1, 0));
    check(text, (0, 0), "9j", text, (2, 0));
    check(text, (1, 6), "kj", text, (1, 6));
    check(text, (1, 6), "jk", text, (1, 6));
    check(text, (0, 0), "l", text, (0, 1));
    check(text, (0, 4), "l", text, (0, 4));
    let para = "a\nb\n\nc\nd";
    check(para, (0, 0), "}", para, (2, 0));
    check(para, (4, 0), "{", para, (2, 0));
    check(para, (2, 0), "}", para, (4, 0));
}

#[test]
fn deletes() {
    check("hello", (0, 1), "x", "hllo", (0, 1));
    check("hello", (0, 1), "3x", "ho", (0, 1));
    check("hello", (0, 4), "x", "hell", (0, 3));
    check("hello", (0, 3), "X", "helo", (0, 2));
    check("hello world", (0, 5), "D", "hello", (0, 4));
    check("a\nb\nc", (1, 0), "dd", "a\nc", (1, 0));
    check("a\nb\nc", (2, 0), "dd", "a\nb", (1, 0));
    check("a\nb\nc", (0, 0), "2dd", "c", (0, 0));
    check("a\nb\nc", (0, 0), "d2d", "c", (0, 0));
    check("only", (0, 2), "dd", "", (0, 0));
    check("one two three", (0, 0), "dw", "two three", (0, 0));
    check("one two three", (0, 0), "d2w", "three", (0, 0));
    check("one two three", (0, 0), "2dw", "three", (0, 0));
    check("one two", (0, 4), "dw", "one ", (0, 3));
    check("one\ntwo", (0, 0), "dw", "\ntwo", (0, 0));
    check("one two", (0, 4), "d$", "one ", (0, 3));
    check("a\nb\nc", (1, 0), "dG", "a", (0, 0));
    check("a\nb\nc", (1, 0), "dgg", "c", (0, 0));
    check("a\nb\nc\nd", (0, 0), "dj", "c\nd", (0, 0));
    check("  a\nb", (0, 2), "d0", "a\nb", (0, 0));
    check("a\nb\n\nc", (0, 0), "d}", "\nc", (0, 0));
}

#[test]
fn changes_enter_insert_and_return_on_esc() {
    check("hello world", (0, 5), "Cthere<Esc>", "hellothere", (0, 9));
    check("one two", (0, 0), "cwuno<Esc>", "uno two", (0, 2));
    check("one two", (0, 0), "c2wx<Esc>", "x", (0, 0));
    check("  one\ntwo", (0, 3), "ccz<Esc>", "  z\ntwo", (0, 2));
    check("abc", (0, 1), "sX<Esc>", "aXc", (0, 1));
    check("a\nb", (0, 0), "ox<Esc>", "a\nx\nb", (1, 0));
    check("a\nb", (1, 0), "Ox<Esc>", "a\nx\nb", (1, 0));
    check("  ab", (0, 3), "Ix<Esc>", "  xab", (0, 2));
    check("ab", (0, 0), "Ax<Esc>", "abx", (0, 2));
    check("ab", (0, 0), "ax<Esc>", "axb", (0, 1));
    check("ab", (0, 1), "ix<Esc>", "axb", (0, 1));
    check("ab", (0, 0), "i<CR><Esc>", "\nab", (1, 0));
}

#[test]
fn yank_and_put() {
    check("a\nb", (0, 0), "yyp", "a\na\nb", (1, 0));
    check("a\nb", (1, 0), "yyP", "a\nb\nb", (1, 0));
    check("a\nb", (0, 0), "yy2p", "a\na\na\nb", (1, 0));
    check("one two", (0, 0), "ywP", "one one two", (0, 3));
    check("one two", (0, 0), "yw$p", "one twoone ", (0, 10));
    check("abc", (0, 0), "xp", "bac", (0, 1));
    check("a\nb", (0, 0), "ddp", "b\na", (1, 0));
    check("a\nb\nc", (0, 0), "yjGp", "a\nb\nc\na\nb", (3, 0));
}

#[test]
fn join_replace_and_case() {
    check("a\n  b\nc", (0, 0), "J", "a b\nc", (0, 1));
    check("a\nb\nc", (0, 0), "3J", "a b c", (0, 3));
    check("a\n)", (0, 0), "J", "a)", (0, 1));
    check("abc", (0, 0), "rx", "xbc", (0, 0));
    check("abc", (0, 0), "2rx", "xxc", (0, 1));
    check("abc", (0, 2), "2rx", "abc", (0, 2));
    check("aBc", (0, 0), "~", "ABc", (0, 1));
    check("aBc", (0, 0), "3~", "AbC", (0, 2));
}

#[test]
fn undo_redo_take_whole_changes() {
    check("a\nb\nc", (1, 0), "ddu", "a\nb\nc", (1, 0));
    check("a\nb\nc", (1, 0), "ddu<C-r>", "a\nc", (1, 0));
    check("one", (0, 0), "cwtwo three<Esc>u", "one", (0, 0));
    check("a", (0, 0), "xu", "a", (0, 0));
    check("ab", (0, 0), "xxuu", "ab", (0, 0));
    check("ab", (0, 0), "xxuu<C-r>", "b", (0, 0));
}

#[test]
fn undo_reverts_the_first_insert_too() {
    let mut vim = Vim::default();
    let mut ta = TextArea::default();
    feed(&mut vim, &mut ta, "hello<Esc>u");
    assert_eq!(ta.lines(), [""]);
}

#[test]
fn dot_repeats_the_last_change() {
    check("one two three", (0, 0), "dw.", "three", (0, 0));
    check("abcdefg", (0, 0), "3x.", "g", (0, 0));
    check("abcdefg", (0, 0), "x2.", "defg", (0, 0));
    check("a\nb", (0, 0), "ihey<Esc>j0.", "heya\nheyb", (1, 2));
    check("a b", (0, 0), "cwx<Esc>w.", "x x", (0, 2));
    check("a\nb\nc", (0, 0), "dd.", "c", (0, 0));
    check("abc", (0, 0), "xu.", "bc", (0, 0));
    check("abcd", (0, 0), "x.u", "bcd", (0, 0));
}

#[test]
fn visual_modes() {
    check("abcdef", (0, 1), "vlld", "aef", (0, 1));
    check("abcdef", (0, 3), "vhhd", "aef", (0, 1));
    check("a\nb\nc", (0, 0), "Vjd", "c", (0, 0));
    check("a\nb\nc", (1, 0), "Vkd", "c", (0, 0));
    check("a\nb", (0, 0), "VyjP", "a\na\nb", (1, 0));
    check("a\nb", (0, 0), "Vyp", "a\na\nb", (1, 0));
    check("one two", (0, 0), "vecx<Esc>", "x two", (0, 0));
    check("one two", (0, 0), "yewvep", "one one", (0, 6));
    check("abc", (0, 0), "vl<Esc>x", "ac", (0, 1));
    check("abcd", (0, 0), "vlohd", "cd", (0, 0));
}

#[test]
fn visual_range_is_inclusive_for_painting() {
    let mut vim = normal();
    let mut ta = area("abc\ndef", (0, 1));
    feed(&mut vim, &mut ta, "vl");
    assert_eq!(vim.visual_range(&ta), Some(((0, 1), (0, 3))));
    feed(&mut vim, &mut ta, "V");
    assert_eq!(vim.mode(), Mode::VisualLine);
    assert_eq!(vim.visual_range(&ta), Some(((0, 0), (0, 3))));
    feed(&mut vim, &mut ta, "j");
    assert_eq!(vim.visual_range(&ta), Some(((0, 0), (1, 3))));
    feed(&mut vim, &mut ta, "<Esc>");
    assert_eq!(vim.visual_range(&ta), None);
}

#[test]
fn outcomes_tell_the_caller_what_to_do() {
    let mut vim = Vim::default();
    let mut ta = TextArea::default();
    assert_eq!(feed(&mut vim, &mut ta, "a"), [Outcome::Passthrough]);
    assert_eq!(feed(&mut vim, &mut ta, "<CR>"), [Outcome::Passthrough]);
    assert_eq!(feed(&mut vim, &mut ta, "<Esc>"), [Outcome::Handled]);
    assert_eq!(vim.mode(), Mode::Normal);
    assert_eq!(vim.label(), "NORMAL");
    assert_eq!(feed(&mut vim, &mut ta, "<CR>"), [Outcome::Submit]);
    assert_eq!(feed(&mut vim, &mut ta, "<Esc>"), [Outcome::Leave]);
    let before = ta.lines().to_vec();
    assert_eq!(
        feed(&mut vim, &mut ta, "d<Esc>"),
        [Outcome::Handled, Outcome::Handled]
    );
    assert_eq!(ta.lines(), before);
    assert_eq!(feed(&mut vim, &mut ta, "<C-n>"), [Outcome::Passthrough]);
    assert_eq!(feed(&mut vim, &mut ta, "q"), [Outcome::Handled]);
    assert_eq!(ta.lines(), before, "unknown Normal keys never type");
    vim.reset();
    assert_eq!(vim.mode(), Mode::Insert);
}

#[test]
fn outside_edits_drop_stale_undo() {
    let mut vim = normal();
    let mut ta = area("abc", (0, 0));
    feed(&mut vim, &mut ta, "x");
    ta.insert_str("zz");
    feed(&mut vim, &mut ta, "u");
    assert_eq!(ta.lines(), ["zzbc"]);
}
