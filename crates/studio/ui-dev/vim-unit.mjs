// Run: node crates/studio/ui-dev/vim-unit.mjs
// The doc source view's vim engine against the same cases as the terminal's
// (crates/tui/src/vim_tests.rs), so both editors behave alike.
import assert from 'node:assert/strict';
import { Buffer, Vim, typeKey } from '../ui/doc/vim.js';

function keys(spec) {
  const out = [];
  for (let i = 0; i < spec.length;) {
    if (spec[i] === '<') {
      const end = spec.indexOf('>', i), name = spec.slice(i + 1, end);
      i = end + 1;
      if (name === 'Esc') out.push({ key: 'Escape' });
      else if (name === 'CR') out.push({ key: 'Enter' });
      else if (name.startsWith('C-')) out.push({ key: name.slice(2), ctrlKey: true });
      continue;
    }
    out.push({ key: spec[i++] });
  }
  return out;
}
function feed(vim, buf, spec) {
  return keys(spec).map(ev => { const out = vim.key(buf, ev); if (out === 'passthrough') typeKey(buf, ev); return out; });
}
function normal() { const vim = new Vim(); vim.key(new Buffer(''), { key: 'Escape' }); return vim; }
function check(text, [row, col], spec, want, wantCursor) {
  const buf = new Buffer(text); buf.cursor = [row, col];
  feed(normal(), buf, spec);
  assert.deepEqual([buf.text(), buf.cursor], [want, wantCursor], `${spec} on ${JSON.stringify(text)}`);
}

const cases = {
  wordMotions() {
    const t = 'foo.bar baz';
    check(t, [0, 0], 'w', t, [0, 3]); check(t, [0, 0], 'ww', t, [0, 4]); check(t, [0, 0], '3w', t, [0, 8]);
    check(t, [0, 0], 'W', t, [0, 8]); check(t, [0, 0], 'e', t, [0, 2]); check(t, [0, 0], 'E', t, [0, 6]);
    check(t, [0, 10], 'b', t, [0, 8]); check(t, [0, 10], 'bb', t, [0, 4]); check(t, [0, 10], 'B', t, [0, 8]);
    check(t, [0, 8], 'B', t, [0, 0]);
    check('ab\n\ncd', [0, 0], 'w', 'ab\n\ncd', [1, 0]); check('ab\n\ncd', [0, 0], 'ww', 'ab\n\ncd', [2, 0]);
    check('ab\n  cd', [1, 2], 'b', 'ab\n  cd', [0, 0]);
  },
  lineAndFileMotions() {
    const t = '  one\ntwo two\nthree';
    check(t, [0, 4], '0', t, [0, 0]); check(t, [0, 0], '^', t, [0, 2]); check(t, [1, 0], '$', t, [1, 6]);
    check(t, [2, 3], 'gg', t, [0, 2]); check(t, [0, 0], 'G', t, [2, 0]); check(t, [0, 0], '2G', t, [1, 0]);
    check(t, [0, 0], '9j', t, [2, 0]); check(t, [1, 6], 'kj', t, [1, 6]); check(t, [0, 4], 'l', t, [0, 4]);
    const p = 'a\nb\n\nc\nd';
    check(p, [0, 0], '}', p, [2, 0]); check(p, [4, 0], '{', p, [2, 0]); check(p, [2, 0], '}', p, [4, 0]);
  },
  deletes() {
    check('hello', [0, 1], 'x', 'hllo', [0, 1]); check('hello', [0, 1], '3x', 'ho', [0, 1]);
    check('hello', [0, 4], 'x', 'hell', [0, 3]); check('hello', [0, 3], 'X', 'helo', [0, 2]);
    check('hello world', [0, 5], 'D', 'hello', [0, 4]);
    check('a\nb\nc', [1, 0], 'dd', 'a\nc', [1, 0]); check('a\nb\nc', [2, 0], 'dd', 'a\nb', [1, 0]);
    check('a\nb\nc', [0, 0], '2dd', 'c', [0, 0]); check('only', [0, 2], 'dd', '', [0, 0]);
    check('one two three', [0, 0], 'dw', 'two three', [0, 0]); check('one two three', [0, 0], 'd2w', 'three', [0, 0]);
    check('one two', [0, 4], 'dw', 'one ', [0, 3]); check('one\ntwo', [0, 0], 'dw', '\ntwo', [0, 0]);
    check('a\nb\nc', [1, 0], 'dG', 'a', [0, 0]); check('a\nb\nc', [1, 0], 'dgg', 'c', [0, 0]);
    check('a\nb\nc\nd', [0, 0], 'dj', 'c\nd', [0, 0]); check('a\nb\n\nc', [0, 0], 'd}', '\nc', [0, 0]);
  },
  changes() {
    check('hello world', [0, 5], 'Cthere<Esc>', 'hellothere', [0, 9]);
    check('one two', [0, 0], 'cwuno<Esc>', 'uno two', [0, 2]); check('one two', [0, 0], 'c2wx<Esc>', 'x', [0, 0]);
    check('  one\ntwo', [0, 3], 'ccz<Esc>', '  z\ntwo', [0, 2]);
    check('a\nb', [0, 0], 'ox<Esc>', 'a\nx\nb', [1, 0]); check('a\nb', [1, 0], 'Ox<Esc>', 'a\nx\nb', [1, 0]);
    check('  ab', [0, 3], 'Ix<Esc>', '  xab', [0, 2]); check('ab', [0, 0], 'Ax<Esc>', 'abx', [0, 2]);
    check('ab', [0, 0], 'ax<Esc>', 'axb', [0, 1]);
  },
  yankPut() {
    check('a\nb', [0, 0], 'yyp', 'a\na\nb', [1, 0]); check('a\nb', [1, 0], 'yyP', 'a\nb\nb', [1, 0]);
    check('one two', [0, 0], 'ywP', 'one one two', [0, 3]); check('abc', [0, 0], 'xp', 'bac', [0, 1]);
    check('a\nb', [0, 0], 'ddp', 'b\na', [1, 0]);
  },
  joinReplaceCase() {
    check('a\n  b\nc', [0, 0], 'J', 'a b\nc', [0, 1]); check('a\nb\nc', [0, 0], '3J', 'a b c', [0, 3]);
    check('abc', [0, 0], '2rx', 'xxc', [0, 1]); check('aBc', [0, 0], '3~', 'AbC', [0, 2]);
  },
  undoRedo() {
    check('a\nb\nc', [1, 0], 'ddu', 'a\nb\nc', [1, 0]); check('a\nb\nc', [1, 0], 'ddu<C-r>', 'a\nc', [1, 0]);
    check('one', [0, 0], 'cwtwo three<Esc>u', 'one', [0, 0]); check('ab', [0, 0], 'xxuu', 'ab', [0, 0]);
  },
  dotRepeat() {
    check('one two three', [0, 0], 'dw.', 'three', [0, 0]); check('abcdefg', [0, 0], '3x.', 'g', [0, 0]);
    check('abcdefg', [0, 0], 'x2.', 'defg', [0, 0]); check('a\nb', [0, 0], 'ihey<Esc>j0.', 'heya\nheyb', [1, 2]);
    check('a b', [0, 0], 'cwx<Esc>w.', 'x x', [0, 2]); check('abcd', [0, 0], 'x.u', 'bcd', [0, 0]);
  },
  visual() {
    check('abcdef', [0, 1], 'vlld', 'aef', [0, 1]); check('abcdef', [0, 3], 'vhhd', 'aef', [0, 1]);
    check('a\nb\nc', [0, 0], 'Vjd', 'c', [0, 0]); check('a\nb', [0, 0], 'VyjP', 'a\na\nb', [1, 0]);
    check('one two', [0, 0], 'vecx<Esc>', 'x two', [0, 0]); check('one two', [0, 0], 'yewvep', 'one one', [0, 6]);
    check('abcd', [0, 0], 'vlohd', 'cd', [0, 0]);
  },
  outcomes() {
    const vim = new Vim(), buf = new Buffer('');
    assert.deepEqual(feed(vim, buf, 'a<Esc>'), ['passthrough', 'handled']);
    assert.equal(vim.label(), 'NORMAL');
    assert.deepEqual(feed(vim, buf, '<Esc>'), ['leave']);
    assert.deepEqual(feed(vim, buf, 'q'), ['handled']);
    assert.equal(buf.text(), 'a');
  },
  utf16Offsets() {
    const buf = new Buffer('a🙂b\ncd', 0);
    buf.cursor = [0, 2];
    assert.equal(buf.offsetOf(buf.cursor), 3);
    assert.deepEqual(new Buffer('a🙂b\ncd', 6).cursor, [1, 1]);
  },
};
let failed = 0;
for (const [name, run] of Object.entries(cases)) {
  try { run(); console.log(`ok   ${name}`); } catch (error) { failed++; console.log(`FAIL ${name}\n${error.message}`); }
}
if (failed) process.exitCode = 1;
