// Run: node crates/studio/ui-dev/sheet-unit.mjs
// Pure-function tests for sheet mode's model and key map (no DOM, no browser): A1 math
// and ranges, the selection over a sparse tab, the key map, TSV, the JSON of every
// toolbar and menu action, fill targets and viewport windows. Needs Node 22+.
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
const ui = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../ui/sheet');
const M = await import(`${ui}/model.js`);
const { keyAction } = await import(`${ui}/keys.js`);

const results = [];
const test = (name, run) => { try { run(); results.push([name, null]); } catch (error) { results.push([name, error]); } };

/** A tab with the given inputs, `{A1: '1', B2: '=A1'}`. */
const tabOf = (inputs = {}, extra = {}) => ({ id: 't1', name: 'Sheet1', cells: Object.fromEntries(Object.entries(inputs).map(([k, input]) => [k, { input }])), ...extra });
const data = (inputs, extent = { rows: 100, cols: 26 }) => M.dataOf(tabOf(inputs), extent);
const sel = (row, col) => M.newSel(row, col);

// -- addresses and ranges ------------------------------------------------------------------

test('column letters and indices round trip, to the last column', () => {
  assert.equal(M.colName(1), 'A'); assert.equal(M.colName(26), 'Z'); assert.equal(M.colName(27), 'AA');
  assert.equal(M.colName(702), 'ZZ'); assert.equal(M.colName(703), 'AAA'); assert.equal(M.colName(M.MAX_COLS), 'XFD');
  for (const n of [1, 2, 25, 26, 27, 52, 53, 702, 703, 16384]) assert.equal(M.colIndex(M.colName(n)), n);
  assert.equal(M.colIndex('XFE'), 0, 'past the last column'); assert.equal(M.colIndex('a'), 0); assert.equal(M.colIndex(''), 0);
});
test('A1 addresses parse strictly and print canonically', () => {
  assert.deepEqual(M.parseAddr('B12'), { row: 12, col: 2 });
  assert.deepEqual(M.parseAddr(' $c$3 '), { row: 3, col: 3 });
  for (const bad of ['A0', 'A1048577', '1A', 'A', 'XFE1', 'A1:B2', '']) assert.equal(M.parseAddr(bad), null, bad);
  assert.equal(M.addr(12, 2), 'B12');
});
test('ranges: cells, columns, rows; reversed corners normalize', () => {
  assert.deepEqual(M.parseRange('A1'), { r1: 1, c1: 1, r2: 1, c2: 1 });
  assert.deepEqual(M.parseRange('C5:A2'), { r1: 2, c1: 1, r2: 5, c2: 3 });
  assert.deepEqual(M.parseRange('B:C'), { r1: 1, c1: 2, r2: M.MAX_ROWS, c2: 3 });
  assert.deepEqual(M.parseRange('3:4'), { r1: 3, c1: 1, r2: 4, c2: M.MAX_COLS });
  assert.equal(M.parseRange('A1:'), null); assert.equal(M.parseRange('foo'), null);
  assert.equal(M.rectStr({ r1: 2, c1: 1, r2: 5, c2: 3 }), 'A2:C5'); assert.equal(M.rectStr(M.cellRect(7, 28)), 'AB7');
  assert.ok(M.inRect(M.parseRange('A1:C3'), 3, 3)); assert.ok(!M.inRect(M.parseRange('A1:C3'), 4, 1));
  assert.ok(M.overlaps(M.parseRange('A1:C3'), M.parseRange('C3:D4'))); assert.ok(!M.overlaps(M.parseRange('A1:B2'), M.parseRange('C1:D2')));
});
test('tab-qualified references, quoted when the name needs it', () => {
  assert.deepEqual(M.parseRef('B12'), { tab: null, rect: M.cellRect(12, 2) });
  assert.deepEqual(M.parseRef('Data!A1'), { tab: 'Data', rect: M.cellRect(1, 1) });
  assert.deepEqual(M.parseRef("'My Tab'!A1:B2"), { tab: 'My Tab', rect: M.parseRange('A1:B2') });
  assert.deepEqual(M.parseRef("'It''s'!C3"), { tab: "It's", rect: M.cellRect(3, 3) });
  assert.equal(M.parseRef('nonsense'), null); assert.equal(M.parseRef('Data!zzz'), null);
  assert.equal(M.refText(M.cellRect(2, 2), 'Sheet1', 'Sheet1'), 'B2');
  assert.equal(M.refText(M.parseRange('A1:B2'), 'Data', 'Sheet1'), 'Data!A1:B2');
  assert.equal(M.refText(M.cellRect(1, 1), 'My Tab', 'Sheet1'), "'My Tab'!A1");
  assert.equal(M.refText(M.cellRect(1, 1), "It's", 'Sheet1'), "'It''s'!A1");
});

// -- axes and the viewport ----------------------------------------------------------------------

test('axes: positions with sparse overrides, and the index under an offset', () => {
  const axis = M.makeAxis(24, new Map([[2, 50], [5, 0]]), 100);
  assert.equal(axis.pos(1), 0); assert.equal(axis.pos(2), 24); assert.equal(axis.pos(3), 74);
  assert.equal(axis.pos(5), 122); assert.equal(axis.pos(6), 122, 'a zero-height row takes no room');
  assert.equal(axis.total, 100 * 24 + 26 - 24);
  assert.equal(axis.size(2), 50); assert.equal(axis.size(3), 24);
  assert.equal(axis.at(0), 1); assert.equal(axis.at(23), 1); assert.equal(axis.at(24), 2); assert.equal(axis.at(73), 2); assert.equal(axis.at(74), 3);
  assert.equal(axis.at(1e9), 100, 'clamped to the last index'); assert.equal(axis.at(-5), 1);
  for (let i = 1; i <= 100; i++) if (axis.size(i) > 0) assert.equal(axis.at(axis.pos(i)), i, `row ${i} starts at its own position`);
});
test('axes of a tab: widths by column letters, heights by row number', () => {
  const axes = M.axesOf({ cols: { B: 150, AA: 40 }, rows: { 3: 60 } }, { rows: 50, cols: 30 });
  assert.equal(axes.cols.size(1), 100); assert.equal(axes.cols.size(2), 150); assert.equal(axes.cols.size(27), 40);
  assert.equal(axes.rows.size(3), 60); assert.equal(axes.rows.pos(4), 3 * 24 + 36);
});
test('axes of a tab: derived wrapped-row heights, with a user-set height winning', () => {
  const axes = M.axesOf({ rows: { 3: 60 } }, { rows: 50, cols: 30 }, { 3: 90, 4: 38 });
  assert.equal(axes.rows.size(3), 60, 'the user-set height wins');
  assert.equal(axes.rows.size(4), 38, 'a wrapped row takes its derived height');
  assert.equal(axes.rows.size(5), 24);
  assert.equal(axes.rows.pos(5), 24 * 4 + 36 + 14, 'offsets add up the mixed heights');
});
test('overflow spans: to the next filled cell, in the reading direction', () => {
  const empty = () => false, filled = () => true;
  assert.deepEqual(M.overflowSpan('left', false, false, 1, 5, empty), { c1: 1, c2: 5 });
  assert.deepEqual(M.overflowSpan('left', false, false, 1, 5, c => c === 3), { c1: 1, c2: 2 }, 'a filled cell stops it');
  assert.deepEqual(M.overflowSpan('left', false, false, 4, 5, empty), { c1: 4, c2: 5 });
  assert.deepEqual(M.overflowSpan('right', false, false, 5, 5, empty), { c1: 1, c2: 5 }, 'right-aligned text spills left');
  assert.deepEqual(M.overflowSpan('right', false, false, 5, 5, c => c === 4), { c1: 5, c2: 5 });
  assert.deepEqual(M.overflowSpan('center', false, false, 3, 5, empty), { c1: 1, c2: 5 }, 'centered text spills both ways');
  assert.deepEqual(M.overflowSpan('center', false, false, 3, 5, c => c === 2), { c1: 3, c2: 5 });
  assert.deepEqual(M.overflowSpan('left', false, true, 1, 5, empty), { c1: 1, c2: 1 }, 'numbers never overflow');
  for (const wrap of ['wrap', 'clip']) assert.deepEqual(M.overflowSpan('left', Boolean(wrap), false, 1, 5, empty), { c1: 1, c2: 1 }, `${wrap} never overflows`);
  assert.deepEqual(M.overflowSpan('left', false, false, 1, 5, filled), { c1: 1, c2: 1 }, 'a full row leaves no room');
});
test('viewport window: the visible rows and columns plus a margin, clamped to the grid', () => {
  const axes = M.axesOf(tabOf(), { rows: 10000, cols: 26 });
  const win = M.windowOf(axes, { left: 0, top: 24 * 9000, width: 600, height: 480 });
  assert.equal(win.r1, 9001 - 5); assert.equal(win.r2, 9001 + 20 + 5);
  assert.equal(win.c1, 1); assert.equal(win.c2, 7 + 2);
  const top = M.windowOf(axes, { left: 0, top: 0, width: 600, height: 480 });
  assert.equal(top.r1, 1);
  const end = M.windowOf(axes, { left: 0, top: 24 * 9990, width: 600, height: 480 });
  assert.equal(end.r2, 10000);
});
test('windows split into requests of at most 10,000 cells', () => {
  const small = M.fetchRanges({ r1: 1, c1: 1, r2: 40, c2: 12 });
  assert.deepEqual(small, [{ r1: 1, c1: 1, r2: 40, c2: 12 }]);
  const big = M.fetchRanges({ r1: 1, c1: 1, r2: 1000, c2: 26 });
  assert.ok(big.length > 1);
  for (const r of big) assert.ok(M.rectSize(r).rows * M.rectSize(r).cols <= 10000);
  assert.equal(big[0].r1, 1); assert.equal(big.at(-1).r2, 1000);
  assert.deepEqual(big.slice(1).map((r, i) => r.r1 - big[i].r2), big.slice(1).map(() => 1), 'contiguous, no overlap');
  assert.ok(M.covers([{ r1: 1, c1: 1, r2: 50, c2: 20 }], { r1: 5, c1: 2, r2: 40, c2: 12 }));
  assert.ok(!M.covers([{ r1: 1, c1: 1, r2: 50, c2: 20 }], { r1: 5, c1: 2, r2: 60, c2: 12 }));
});
test('the grid is bigger than the data and grows when the selection reaches past it', () => {
  const used = M.parseRange('A1:C500');
  const e = M.extentOf(used, null, sel(1, 1));
  assert.ok(e.rows >= 550 && e.cols >= 26);
  assert.ok(M.extentOf(used, e, sel(900, 40)).rows >= 925);
  assert.equal(M.extentOf(used, { rows: 5000, cols: 80 }, sel(1, 1)).rows, 5000, 'it never shrinks');
  assert.equal(M.extentOf(null, null, null).rows, 100);
});

// -- the selection --------------------------------------------------------------------------------

test('arrows move the active cell and stop at the corner', () => {
  const d = data({});
  let s = M.moveSel(sel(2, 2), 1, 0, {}, d); assert.deepEqual(s.active, { row: 3, col: 2 });
  s = M.moveSel(sel(1, 1), -1, 0, {}, d); assert.deepEqual(s.active, { row: 1, col: 1 });
  s = M.moveSel(sel(1, 1), 0, -1, {}, d); assert.deepEqual(s.active, { row: 1, col: 1 });
  s = M.moveSel(sel(1, 1), 0, 1, {}, d); assert.deepEqual(M.selRect(s), M.cellRect(1, 2));
});
test('shift+arrows extend from the anchor and shrink back', () => {
  const d = data({});
  let s = sel(3, 3);
  s = M.moveSel(s, 1, 0, { extend: true }, d); s = M.moveSel(s, 1, 0, { extend: true }, d); s = M.moveSel(s, 0, 1, { extend: true }, d);
  assert.equal(M.rectStr(M.selRect(s)), 'C3:D5'); assert.deepEqual(s.active, { row: 3, col: 3 }, 'the active cell stays put');
  s = M.moveSel(s, -1, 0, { extend: true }, d); assert.equal(M.rectStr(M.selRect(s)), 'C3:D4');
  s = M.moveSel(s, 0, -1, { extend: true }, d); s = M.moveSel(s, 0, -1, { extend: true }, d); assert.equal(M.rectStr(M.selRect(s)), 'B3:C4', 'it crosses the anchor and flips');
});
test('Ctrl+arrow jumps over a run, to the next data, or to the edge of a sparse grid', () => {
  const d = data({ A1: '1', A2: '2', A3: '3', A6: '6', A7: '7' });
  assert.deepEqual(M.jumpTo(d, { row: 1, col: 1 }, 1, 0), { row: 3, col: 1 }, 'the end of the run');
  assert.deepEqual(M.jumpTo(d, { row: 3, col: 1 }, 1, 0), { row: 6, col: 1 }, 'the next data across a gap');
  assert.deepEqual(M.jumpTo(d, { row: 4, col: 1 }, 1, 0), { row: 6, col: 1 }, 'from a blank cell');
  assert.deepEqual(M.jumpTo(d, { row: 7, col: 1 }, 1, 0), { row: 100, col: 1 }, 'the last row of the grid');
  assert.deepEqual(M.jumpTo(d, { row: 7, col: 1 }, -1, 0), { row: 6, col: 1 });
  assert.deepEqual(M.jumpTo(d, { row: 6, col: 1 }, -1, 0), { row: 3, col: 1 });
  assert.deepEqual(M.jumpTo(d, { row: 3, col: 1 }, -1, 0), { row: 1, col: 1 });
  assert.deepEqual(M.jumpTo(d, { row: 1, col: 1 }, 0, 1), { row: 1, col: 26 }, 'nothing to the right: the last column');
  assert.deepEqual(M.jumpTo(d, { row: 5, col: 5 }, 0, -1), { row: 5, col: 1 });
  assert.deepEqual(M.jumpTo(d, { row: 1, col: 1 }, -1, 0), { row: 1, col: 1 }, 'already at the edge');
  const s = M.moveSel(sel(1, 1), 1, 0, { jump: true, extend: true }, d);
  assert.equal(M.rectStr(M.selRect(s)), 'A1:A3', 'shift+Ctrl+arrow extends to the data edge');
});
test('a blank-input cell does not count as data', () => {
  const d = M.dataOf({ cells: { A1: { input: '1' }, A2: { fmt: { bold: true } }, A3: { input: '' }, A4: { input: '4' } } }, { rows: 20, cols: 5 });
  assert.deepEqual(M.jumpTo(d, { row: 1, col: 1 }, 1, 0), { row: 4, col: 1 }, 'formatting alone is not data');
});
test('Enter and Tab walk the selection, wrap, and move one cell from a single cell', () => {
  let s = M.selectCol(sel(1, 1), 1, false); s = { ...s, kind: 'cells', anchor: { row: 1, col: 1 }, head: { row: 2, col: 2 }, active: { row: 1, col: 1 } };
  const walk = (start, dir, back, n) => { const out = []; for (let i = 0, cur = start; i < n; i++) { cur = M.stepWithin(cur, dir, back); out.push(`${cur.active.row},${cur.active.col}`); } return out; };
  assert.deepEqual(walk(s, 'down', false, 5), ['2,1', '1,2', '2,2', '1,1', '2,1']);
  assert.deepEqual(walk(s, 'right', false, 5), ['1,2', '2,1', '2,2', '1,1', '1,2']);
  assert.deepEqual(walk(s, 'down', true, 2), ['2,2', '1,2']);
  assert.equal(M.rectStr(M.selRect(M.stepWithin(s, 'down', false))), 'A1:B2', 'the selection stays');
  const one = M.stepWithin(sel(4, 4), 'right', false); assert.equal(M.rectStr(M.selRect(one)), 'E4');
  assert.deepEqual(M.stepWithin(sel(1, 1), 'right', true).active, { row: 1, col: 1 });
});
test('header clicks select columns and rows; shift extends; select all', () => {
  let s = M.selectCol(sel(5, 3), 4, false);
  assert.equal(s.kind, 'cols'); assert.equal(M.rectStr(M.selRect(s)), `D1:D${M.MAX_ROWS}`); assert.deepEqual(s.active, { row: 5, col: 4 });
  s = M.selectCol(s, 6, true); assert.equal(M.rectStr(M.selRect(s)), `D1:F${M.MAX_ROWS}`);
  s = M.selectRow(sel(2, 2), 7, false); assert.equal(M.rectStr(M.selRect(s)), `A7:${M.colName(M.MAX_COLS)}7`);
  s = M.selectRow(s, 9, true); assert.equal(M.selRect(s).r2, 9);
  s = M.selectAll(sel(3, 3)); assert.equal(M.selRect(s).r2, M.MAX_ROWS); assert.deepEqual(s.active, { row: 3, col: 3 });
  s = M.extendTo(sel(2, 2), 5, 4); assert.equal(M.rectStr(M.selRect(s)), 'B2:D5');
  s = M.extendTo(M.selectCol(sel(5, 3), 4, false), 8, 6); assert.equal(M.rectStr(M.selRect(s)), 'D5:F8', 'dragging from a column selection starts at its active cell');
});
test('edits on a whole column or row stop at the used range', () => {
  const used = M.parseRange('A1:D200');
  assert.equal(M.rectStr(M.opRect(M.selectCol(sel(1, 1), 2, false), used)), 'B1:B200');
  assert.equal(M.rectStr(M.opRect(M.selectRow(sel(1, 1), 5, false), used)), 'A5:D5');
  assert.equal(M.rectStr(M.opRect(M.selectAll(sel(1, 1)), used)), 'A1:D200');
  assert.equal(M.rectStr(M.opRect(M.selectCol(sel(1, 1), 3, false), null)), 'C1');
  assert.equal(M.rectStr(M.opRect(M.extendTo(sel(2, 2), 3, 3), used)), 'B2:C3');
});
test('used range and merges come from the sparse tab', () => {
  const tab = tabOf({ B3: 'x', D9: 'y' }, { merges: ['A1:B2', 'F5:G5'] });
  assert.deepEqual(M.usedRect(tab), { r1: 3, c1: 2, r2: 9, c2: 4 });
  assert.equal(M.usedRect(tabOf()), null);
  const merges = M.mergesOf(tab);
  assert.deepEqual(M.mergeAt(merges, 2, 2), M.parseRange('A1:B2')); assert.equal(M.mergeAt(merges, 3, 3), null);
});

// -- the key map -----------------------------------------------------------------------------------

const key = (k, mods = {}) => ({ key: k, code: mods.code || '', shiftKey: false, metaKey: false, ctrlKey: false, altKey: false, ...mods });
test('keys: moving, extending and jumping', () => {
  assert.deepEqual(keyAction(key('ArrowDown')), { do: 'move', dr: 1, dc: 0, extend: false, jump: false });
  assert.deepEqual(keyAction(key('ArrowLeft', { shiftKey: true })), { do: 'move', dr: 0, dc: -1, extend: true, jump: false });
  assert.deepEqual(keyAction(key('ArrowRight', { metaKey: true })), { do: 'move', dr: 0, dc: 1, extend: false, jump: true });
  assert.deepEqual(keyAction(key('ArrowUp', { ctrlKey: true, shiftKey: true })), { do: 'move', dr: -1, dc: 0, extend: true, jump: true });
  assert.deepEqual(keyAction(key('Tab')), { do: 'step', dir: 'right', back: false });
  assert.deepEqual(keyAction(key('Tab', { shiftKey: true })), { do: 'step', dir: 'right', back: true });
  assert.deepEqual(keyAction(key('Enter', { shiftKey: true })), { do: 'step', dir: 'down', back: true });
  assert.deepEqual(keyAction(key('a', { metaKey: true })), { do: 'selectAll' });
  assert.deepEqual(keyAction(key('Home', { ctrlKey: true })), { do: 'home', shift: false });
  assert.deepEqual(keyAction(key('PageDown')), { do: 'page', dir: 1, extend: false });
});
test('keys: starting an edit', () => {
  assert.deepEqual(keyAction(key('Enter')), { do: 'edit' });
  assert.deepEqual(keyAction(key('F2')), { do: 'edit' });
  assert.deepEqual(keyAction(key('a')), { do: 'type', text: 'a' });
  assert.deepEqual(keyAction(key('=')), { do: 'type', text: '=' });
  assert.deepEqual(keyAction(key(' ')), { do: 'type', text: ' ' });
  assert.deepEqual(keyAction(key('ø', { altKey: true })), { do: 'type', text: 'ø' });
  assert.deepEqual(keyAction(key('Process')), { do: 'edit', ime: true });
  assert.equal(keyAction(key('F5')), null); assert.equal(keyAction(key('Shift', { shiftKey: true })), null);
  assert.equal(keyAction(key('c', { metaKey: true })), null, 'copy and paste belong to the clipboard events');
});
test('keys: clearing, undo, formatting, comment, fill', () => {
  assert.deepEqual(keyAction(key('Delete')), { do: 'clear' }); assert.deepEqual(keyAction(key('Backspace')), { do: 'clear' });
  assert.deepEqual(keyAction(key('z', { metaKey: true })), { do: 'undo' });
  assert.deepEqual(keyAction(key('Z', { metaKey: true, shiftKey: true })), { do: 'redo' });
  assert.deepEqual(keyAction(key('y', { ctrlKey: true })), { do: 'redo' });
  assert.deepEqual(keyAction(key('b', { ctrlKey: true })), { do: 'format', key: 'bold' });
  assert.deepEqual(keyAction(key('u', { metaKey: true })), { do: 'format', key: 'underline' });
  assert.deepEqual(keyAction(key('µ', { metaKey: true, altKey: true, code: 'KeyM' })), { do: 'comment' });
  assert.deepEqual(keyAction(key('∑', { metaKey: true, altKey: true, code: 'KeyW' })), { do: 'wrap' }, 'Cmd+Alt+W toggles wrap');
  assert.deepEqual(keyAction(key('w', { ctrlKey: true, altKey: true, code: 'KeyW' })), { do: 'wrap' });
  assert.deepEqual(keyAction(key('m', { ctrlKey: true, altKey: true, code: 'KeyM' })), { do: 'comment' });
  assert.deepEqual(keyAction(key('d', { metaKey: true })), { do: 'fillDown' }); assert.deepEqual(keyAction(key('r', { metaKey: true })), { do: 'fillRight' });
  assert.deepEqual(keyAction(key(' ', { ctrlKey: true })), { do: 'selectColumn' }); assert.deepEqual(keyAction(key(' ', { shiftKey: true })), { do: 'selectRow' });
  assert.deepEqual(keyAction(key('Escape')), { do: 'escape' });
});
test('keys while editing: commit, cancel, mode, and arrows in enter, edit and point mode', () => {
  const ed = (extra = {}) => ({ editing: true, mode: 'edit', pointable: false, pointing: false, ...extra });
  assert.deepEqual(keyAction(key('Enter'), ed()), { do: 'commit', step: { dir: 'down', back: false } });
  assert.deepEqual(keyAction(key('Enter', { shiftKey: true }), ed()), { do: 'commit', step: { dir: 'down', back: true } });
  assert.deepEqual(keyAction(key('Tab'), ed()), { do: 'commit', step: { dir: 'right', back: false } });
  assert.deepEqual(keyAction(key('Tab', { shiftKey: true }), ed()), { do: 'commit', step: { dir: 'right', back: true } });
  assert.deepEqual(keyAction(key('Enter', { altKey: true }), ed()), { do: 'newline' });
  assert.deepEqual(keyAction(key('Escape'), ed()), { do: 'cancel' });
  assert.deepEqual(keyAction(key('F2'), ed()), { do: 'toggleMode' });
  assert.equal(keyAction(key('ArrowLeft'), ed()), null, 'edit mode: the caret moves');
  assert.deepEqual(keyAction(key('ArrowDown'), ed({ mode: 'enter' })), { do: 'commit', move: { dr: 1, dc: 0 } }, 'enter mode: arrows commit and move');
  assert.equal(keyAction(key('ArrowRight', { shiftKey: true }), ed({ mode: 'enter' })), null, 'shift+arrow selects text');
  assert.deepEqual(keyAction(key('ArrowRight'), ed({ mode: 'enter', pointable: true })), { do: 'point', dr: 0, dc: 1, extend: false }, 'a formula points instead');
  assert.deepEqual(keyAction(key('ArrowDown', { shiftKey: true }), ed({ pointing: true })), { do: 'point', dr: 1, dc: 0, extend: true });
  assert.equal(keyAction(key('z', { metaKey: true }), ed()), null, 'text undo stays with the field');
  assert.equal(keyAction(key('a'), ed()), null);
});

// -- pointing at cells while typing a formula -----------------------------------------------------------

test('point mode: where a reference may go and how it is inserted or replaced', () => {
  for (const [text, caret] of [['=', 1], ['=SUM(', 5], ['=A1+', 4], ['=A1 + ', 6], ['=IF(A1,', 7], ['=A1:', 4], ['=-', 2]]) assert.ok(M.canPoint(text, caret), `${text}|${caret}`);
  for (const [text, caret] of [['abc', 3], ['=A1', 3], ['=SUM(A1)', 8], ['', 0], ['=A1+B2', 3]]) assert.ok(!M.canPoint(text, caret), `${text}|${caret}`);
  let r = M.pointInsert('=SUM(', 5, null, 'B2');
  assert.deepEqual(r, { text: '=SUM(B2', span: { start: 5, end: 7 }, caret: 7 });
  r = M.pointInsert(r.text, r.caret, r.span, 'B2:B5');
  assert.equal(r.text, '=SUM(B2:B5'); assert.deepEqual(r.span, { start: 5, end: 10 });
  r = M.pointInsert('=A1+)', 4, null, 'C3'); assert.equal(r.text, '=A1+C3)', 'inserted at the caret, not the end');
});

// -- TSV -----------------------------------------------------------------------------------------------------

test('TSV parses quotes, tabs and newlines inside quotes, CRLF and a trailing newline', () => {
  assert.deepEqual(M.parseTsv('a\tb\nc\td\n'), [['a', 'b'], ['c', 'd']]);
  assert.deepEqual(M.parseTsv('a\tb\r\nc\td\r\n'), [['a', 'b'], ['c', 'd']]);
  assert.deepEqual(M.parseTsv('"x\ty"\t"line1\nline2"\n"say ""hi"""\tz'), [['x\ty', 'line1\nline2'], ['say "hi"', 'z']]);
  assert.deepEqual(M.parseTsv('a\t\tb'), [['a', '', 'b']], 'empty fields stay');
  assert.deepEqual(M.parseTsv('a\n\nb'), [['a'], [''], ['b']], 'a blank line is a row');
  assert.deepEqual(M.parseTsv('a\tb\nc'), [['a', 'b'], ['c', '']], 'short rows are padded');
  assert.deepEqual(M.parseTsv('single'), [['single']]);
  assert.deepEqual(M.parseTsv(''), [['']]);
  assert.deepEqual(M.parseTsv('5"\t6'), [['5"', '6']], 'a quote inside a field is just text');
  assert.deepEqual(M.parseTsv('""\tx'), [['', 'x']]);
});
test('TSV round trip: values with quotes, tabs and newlines survive', () => {
  const rows = [['plain', 'tab\there', 'multi\nline', 'say "hi"'], ['', ' padded ', '=A1+B1', '1,234.50'], ['""', '"', 'a\r\nb', '\n']];
  assert.equal(M.toTsv([['a', 'b'], ['c', 'd']]), 'a\tb\nc\td');
  assert.equal(M.toTsv([['x\ty']]), '"x\ty"');
  assert.deepEqual(M.parseTsv(M.toTsv(rows)), rows.map(r => r.map(f => f)));
});
test('a single pasted value tiles over a larger selection; blocks stay whole', () => {
  assert.deepEqual(M.tileBlock([['x']], { rows: 2, cols: 3 }), [['x', 'x', 'x'], ['x', 'x', 'x']]);
  assert.deepEqual(M.tileBlock([['x']], { rows: 1, cols: 1 }), [['x']]);
  assert.deepEqual(M.tileBlock([['x', 'y']], { rows: 3, cols: 3 }), [['x', 'y']]);
  assert.deepEqual(M.tileBlock([['x']], { rows: 5000, cols: 5000 }), [['x']], 'not for a whole-column selection');
});

// -- the JSON of every action ---------------------------------------------------------------------------------------

test('typing and clearing', () => {
  assert.deepEqual(M.setCell('Sheet1', 3, 2, '=A1*2'), { op: 'set_cells', tab: 'Sheet1', at: 'B3', values: [['=A1*2']] });
  assert.deepEqual(M.setBlock('Sheet1', 1, 1, [['a'], ['b', 'c']]), { op: 'set_cells', tab: 'Sheet1', at: 'A1', values: [['a', ''], ['b', 'c']] });
  assert.deepEqual(M.op.clear('Sheet1', 'A1:B2'), { op: 'clear', tab: 'Sheet1', range: 'A1:B2', what: 'inputs' });
});
test('toolbar: bold, italic, underline toggle by the active cell; number formats; align; colours', () => {
  assert.deepEqual(M.toggleFormat('S', 'A1:B2', {}, 'bold'), { op: 'format', tab: 'S', range: 'A1:B2', patch: { bold: true } });
  assert.deepEqual(M.toggleFormat('S', 'A1:B2', { bold: true }, 'bold'), { op: 'format', tab: 'S', range: 'A1:B2', patch: { bold: false } });
  assert.deepEqual(M.toggleFormat('S', 'A1', undefined, 'underline').patch, { underline: true });
  assert.deepEqual(M.NUMBER_FORMATS.map(([name, code]) => [name, code]), [['Automatic', null], ['Number', '#,##0.00'], ['Percent', '0.00%'], ['Currency', '$#,##0.00'], ['Date', 'yyyy-mm-dd'], ['Plain text', '@']]);
  assert.deepEqual(M.numberFormat('S', 'A1', '0.00%').patch, { num: '0.00%' });
  assert.deepEqual(M.numberFormat('S', 'A1', null).patch, { num: null }, 'Automatic removes the format');
  assert.deepEqual(M.alignFormat('S', 'A1', 'center').patch, { align: 'center' });
  assert.deepEqual(M.colorFormat('S', 'A1', 'color', 'accent').patch, { color: 'accent' });
  assert.deepEqual(M.colorFormat('S', 'A1', 'fill', null).patch, { fill: null }, '"none" removes the colour');
  assert.equal(M.numberFormatName('0.00%'), 'Percent'); assert.equal(M.numberFormatName(undefined), 'Automatic'); assert.equal(M.numberFormatName('0.0'), 'Custom');
});
test('toolbar: wrapping and vertical alignment, and fitting columns', () => {
  assert.deepEqual(M.WRAP_MODES, [['Overflow', 'overflow'], ['Wrap', 'wrap'], ['Clip', 'clip']]);
  assert.deepEqual(M.VALIGNS, [['Top', 'top'], ['Middle', 'middle'], ['Bottom', 'bottom']]);
  assert.deepEqual(M.wrapFormat('S', 'A1:B2', 'wrap'), { op: 'format', tab: 'S', range: 'A1:B2', patch: { wrap: 'wrap' } });
  assert.deepEqual(M.wrapFormat('S', 'A1', 'overflow'), { op: 'format', tab: 'S', range: 'A1', patch: { wrap: 'overflow' } });
  assert.deepEqual(M.valignFormat('S', 'A1', 'middle'), { op: 'format', tab: 'S', range: 'A1', patch: { valign: 'middle' } });
  assert.deepEqual(M.valignFormat('S', 'A1', null), { op: 'format', tab: 'S', range: 'A1', patch: { valign: null } }, 'the default removes the field');
  assert.deepEqual(M.toggleWrap('S', 'A1', {}).patch, { wrap: 'wrap' });
  assert.deepEqual(M.toggleWrap('S', 'A1', { wrap: 'wrap' }).patch, { wrap: null }, 'wrap toggles back to overflow');
  assert.deepEqual(M.toggleWrap('S', 'A1', { wrap: 'clip' }).patch, { wrap: 'wrap' }, 'clip becomes wrap');
  assert.deepEqual(M.op.fitCols('S', 'A:C'), { op: 'fit_cols', tab: 'S', cols: 'A:C' });
  assert.equal(M.applyLocal({ title: 'T', tabs: [{ id: 't1', name: 'Sheet1', cells: {} }] }, [M.op.fitCols('Sheet1', 'A:A')]), null, 'fitting waits for the backend to measure');
});
test('toolbar: merge toggles with the merges the range touches', () => {
  assert.deepEqual(M.mergeToggle('S', M.parseRange('A1:B2'), []), { op: 'merge', tab: 'S', range: 'A1:B2' });
  assert.deepEqual(M.mergeToggle('S', M.parseRange('A1:B2'), [M.parseRange('A1:C3')]), { op: 'unmerge', tab: 'S', range: 'A1:B2' });
  assert.deepEqual(M.mergeToggle('S', M.cellRect(2, 2), [M.parseRange('A1:C3')]), { op: 'unmerge', tab: 'S', range: 'B2' }, 'a cell inside a merge unmerges it');
  assert.equal(M.mergeToggle('S', M.cellRect(5, 5), []), null, 'one cell has nothing to merge');
});
test('header menu: insert, delete, sort, freeze, resize', () => {
  const col = M.parseRange('C:D'), row = M.parseRange('5:7');
  assert.deepEqual(M.op.insertRows('S', M.insertAt(row, 'row', false), 3), { op: 'insert_rows', tab: 'S', at: 5, count: 3 });
  assert.deepEqual(M.op.insertRows('S', M.insertAt(row, 'row', true), 1), { op: 'insert_rows', tab: 'S', at: 8, count: 1 });
  assert.deepEqual(M.op.insertCols('S', M.insertAt(col, 'col', false), 2), { op: 'insert_cols', tab: 'S', at: 'C', count: 2 });
  assert.deepEqual(M.op.insertCols('S', M.insertAt(col, 'col', true), 1), { op: 'insert_cols', tab: 'S', at: 'E', count: 1 });
  assert.deepEqual(M.op.deleteRows('S', 5, 3), { op: 'delete_rows', tab: 'S', at: 5, count: 3 });
  assert.deepEqual(M.op.deleteCols('S', 3, 2), { op: 'delete_cols', tab: 'S', at: 'C', count: 2 });
  const used = M.parseRange('A1:E40');
  assert.deepEqual(M.sortOps('S', used, 2, false, 0), [{ op: 'sort', tab: 'S', range: 'A1:E40', by: 'B', desc: false, header: false }]);
  assert.deepEqual(M.sortOps('S', used, 3, true, 1), [{ op: 'sort', tab: 'S', range: 'A1:E40', by: 'C', desc: true, header: true }], 'a frozen first row is the header');
  assert.equal(M.sortOps('S', used, 9, false, 0)[0].range, 'A1:I40', 'sorting by a column beyond the data widens the range');
  assert.deepEqual(M.op.freeze('S', 2, 1), { op: 'freeze', tab: 'S', rows: 2, cols: 1 });
  assert.deepEqual(M.resizeOp('S', 'col', 2, 4, 140), { op: 'set_col_width', tab: 'S', cols: 'B:D', width: 140 });
  assert.deepEqual(M.resizeOp('S', 'row', 3, 3, 60), { op: 'set_row_height', tab: 'S', rows: '3:3', height: 60 });
});
test('copy, cut and fill ops', () => {
  assert.deepEqual(M.op.copy('S', 'A1:B2', 'D4'), { op: 'copy', tab: 'S', from: 'A1:B2', to: 'D4', cut: false });
  assert.deepEqual(M.op.copy('S', 'A1', 'C3', true), { op: 'copy', tab: 'S', from: 'A1', to: 'C3', cut: true });
  assert.deepEqual(M.op.fill('S', 'A1:A2', 'A1:A10'), { op: 'fill', tab: 'S', from: 'A1:A2', to: 'A1:A10' });
});
test('tab ops: new names, rename, delete, reorder by dragging', () => {
  assert.equal(M.nextTabName(['Sheet1']), 'Sheet2'); assert.equal(M.nextTabName(['Sheet2', 'Data']), 'Sheet3'); assert.equal(M.nextTabName(['Sheet1', 'sheet3', 'x']), 'Sheet4');
  assert.deepEqual(M.op.addTab('Sheet2', 'Sheet1'), { op: 'add_tab', name: 'Sheet2', after: 'Sheet1' });
  assert.deepEqual(M.op.renameTab('Sheet1', 'Data'), { op: 'rename_tab', tab: 'Sheet1', name: 'Data' });
  assert.deepEqual(M.op.deleteTab('Data'), { op: 'delete_tab', tab: 'Data' });
  const run = (names, ops) => { const order = [...names]; for (const o of ops) { order.splice(order.indexOf(o.tab), 1); order.splice(o.after === null ? order.length : order.indexOf(o.after) + 1, 0, o.tab); } return order; };
  const names = ['A', 'B', 'C', 'D'];
  for (const [from, to] of [[0, 3], [3, 0], [1, 2], [2, 0], [0, 1], [1, 1]]) {
    const target = M.dragOrder(names, from, to), ops = M.reorderOps(names, target);
    assert.deepEqual(run(names, ops), target, `move ${from} to ${to}`);
  }
  assert.deepEqual(M.reorderOps(names, names), [], 'nothing to do');
  assert.deepEqual(M.dragOrder(names, 3, 0), ['D', 'A', 'B', 'C']);
  assert.equal(M.reorderOps(names, ['A', 'B', 'D', 'C']).length, 1, 'swapping two neighbours is one move');
});

// -- the fill handle ----------------------------------------------------------------------------------------------------

test('fill handle: the target extends down or right, never inside, never up or left', () => {
  const src = M.parseRange('B2:C3');
  assert.equal(M.rectStr(M.fillTarget(src, { row: 8, col: 3 })), 'B2:C8');
  assert.equal(M.rectStr(M.fillTarget(src, { row: 3, col: 7 })), 'B2:G3');
  assert.equal(M.rectStr(M.fillTarget(src, { row: 8, col: 5 })), 'B2:C8', 'the longer pull wins');
  assert.equal(M.rectStr(M.fillTarget(src, { row: 4, col: 8 })), 'B2:H3');
  assert.equal(M.fillTarget(src, { row: 3, col: 3 }), null); assert.equal(M.fillTarget(src, { row: 1, col: 1 }), null); assert.equal(M.fillTarget(src, { row: 2, col: 2 }), null);
});
test('double-clicking the fill handle fills down to the end of the neighbouring column', () => {
  const d = data({ A1: '1', A2: '2', A3: '3', A4: '4', A5: '5', B1: 'x', C1: 'y', C2: 'z', A8: '8' });
  assert.equal(M.rectStr(M.fillDownEnd(M.cellRect(1, 2), d)), 'B1:B5', 'follows the left column, stopping at the gap');
  assert.equal(M.rectStr(M.fillDownEnd(M.parseRange('B1:B1'), d)), 'B1:B5');
  assert.equal(M.rectStr(M.fillDownEnd(M.cellRect(1, 2), data({ C1: '1', C2: '2', C3: '3', B1: 'x' }))), 'B1:B3', 'else the right column');
  assert.equal(M.fillDownEnd(M.cellRect(1, 1), data({ A1: 'x' })), null, 'no neighbouring data');
  assert.equal(M.fillDownEnd(M.cellRect(5, 2), d), null, 'already at the end');
});

// -- showing edits before the backend answers ------------------------------------------------------------------------------

test('local edits: typing, clearing, formatting, sizes and merges show at once; the rest wait', () => {
  const sheet = { title: 'T', tabs: [{ id: 't1', name: 'Sheet1', cells: { A1: { input: '1', fmt: { bold: true } }, B2: { input: 'x' } } }, { id: 't2', name: 'Other', cells: {} }] };
  const frozen = JSON.stringify(sheet);
  let next = M.applyLocal(sheet, [M.setBlock('Sheet1', 1, 1, [['9', '10'], ['', '']])]);
  assert.equal(JSON.stringify(sheet), frozen, 'the original is untouched');
  assert.deepEqual(next.tabs[0].cells.A1, { input: '9', fmt: { bold: true } }); assert.deepEqual(next.tabs[0].cells.B1, { input: '10' });
  assert.equal(next.tabs[0].cells.B2.input, undefined, 'typing nothing clears the input'); assert.ok(!('A2' in next.tabs[0].cells), 'and adds no empty cell');
  next = M.applyLocal(sheet, [M.op.clear('Sheet1', 'A1:B2')]);
  assert.deepEqual(next.tabs[0].cells, { A1: { fmt: { bold: true } } }, 'clear inputs keeps formats');
  next = M.applyLocal(sheet, [M.op.clear('sheet1', 'A1:B2', 'all')]);
  assert.deepEqual(next.tabs[0].cells, {}, 'tab names match without case');
  next = M.applyLocal(sheet, [M.op.format('Sheet1', 'A1:B1', { bold: false, fill: 'accent', num: '0.00%' })]);
  assert.deepEqual(next.tabs[0].cells.A1, { input: '1', fmt: { fill: 'accent', num: '0.00%' } }, 'false removes a flag');
  assert.deepEqual(next.tabs[0].cells.B1, { fmt: { fill: 'accent', num: '0.00%' } });
  next = M.applyLocal(sheet, [M.op.format('Sheet1', 'A1', { fill: null })]); assert.deepEqual(next.tabs[0].cells.A1.fmt, { bold: true });
  next = M.applyLocal(sheet, [M.op.colWidth('Sheet1', 'B:C', 150), M.op.rowHeight('Sheet1', '2:3', 40), M.op.freeze('Sheet1', 1, 0), M.op.merge('Sheet1', 'A1:B2'), M.op.setTitle('New')]);
  assert.deepEqual(next.tabs[0].cols, { B: 150, C: 150 }); assert.deepEqual(next.tabs[0].rows, { 2: 40, 3: 40 });
  assert.deepEqual(next.tabs[0].frozen, { rows: 1, cols: 0 }); assert.deepEqual(next.tabs[0].merges, ['A1:B2']); assert.equal(next.title, 'New');
  next = M.applyLocal(next, [M.op.unmerge('Sheet1', 'B2')]); assert.deepEqual(next.tabs[0].merges, []);
  assert.equal(M.applyLocal(sheet, [M.op.insertRows('Sheet1', 1)]), null, 'structure waits for the backend');
  assert.equal(M.applyLocal(sheet, [M.setCell('Nope', 1, 1, 'x')]), null, 'an unknown tab too');
  assert.equal(M.applyLocal(sheet, [M.setCell('Sheet1', 1, 1, 'x'), M.op.sort('Sheet1', 'A1:B2', 'A', false, false)]), null, 'one op that needs the backend holds back the batch');
});
test('values before calculation: literals show as typed, formulas wait', () => {
  assert.deepEqual(M.literalValue('42'), { display: '42', kind: 'number' }); assert.deepEqual(M.literalValue('hello'), { display: 'hello', kind: 'text' });
  assert.deepEqual(M.literalValue('true'), { display: 'TRUE', kind: 'bool' });
  assert.equal(M.literalValue('=A1+1'), null); assert.equal(M.literalValue(''), null); assert.deepEqual(M.literalValue(' '), { display: ' ', kind: 'text' });
  assert.equal(M.alignOf({}, { kind: 'number' }), 'right'); assert.equal(M.alignOf({}, { kind: 'text' }), 'left'); assert.equal(M.alignOf({}, { kind: 'bool' }), 'center');
  assert.equal(M.alignOf({ align: 'center' }, { kind: 'number' }), 'center', 'the format wins');
  assert.equal(M.alignOf(undefined, undefined), 'left');
});
test('colours: tokens go through the sheet theme, hex passes, unknown is none', () => {
  const theme = { colors: { accent: '#ae593e', fg: '#282923', broken: 'red' } };
  assert.equal(M.cssColor(theme, 'accent'), '#ae593e'); assert.equal(M.cssColor(theme, '#00ff00'), '#00ff00');
  assert.equal(M.cssColor(theme, 'nope'), null); assert.equal(M.cssColor(theme, 'broken'), null); assert.equal(M.cssColor(theme, null), null); assert.equal(M.cssColor(null, 'accent'), null);
});
test('threads and external changes', () => {
  assert.deepEqual(M.anchorRect({ orphaned: false, anchor: { tab: 't1', range: 'B2:C3', quote: '' } }), M.parseRange('B2:C3'));
  assert.equal(M.anchorRect({ orphaned: true, anchor: { tab: 't1', range: 'B2', quote: '' } }), null);
  assert.deepEqual(M.splitChanged('Sheet1!B2'), { tab: 'Sheet1', a1: 'B2' }); assert.deepEqual(M.splitChanged('Q3!2024!B2'), { tab: 'Q3!2024', a1: 'B2' });
  assert.equal(M.splitChanged('B2'), null);
});

test('charts: insertion, edit, movement, resize and deletion use atomic op JSON', () => {
  assert.deepEqual(M.addChartOp('Data', 'A1:B4'), { op: 'add_chart', tab: 'Data', data: 'A1:B4' });
  assert.deepEqual(M.editChartOp('Data', 'c1', { kind: 'line' }), { op: 'edit_chart', tab: 'Data', id: 'c1', patch: { kind: 'line' } });
  assert.deepEqual(M.editChartOp('Data', 'c1', { at: 'D2', dx: 3, dy: 4, w: 600, h: 300 }).patch, { at: 'D2', dx: 3, dy: 4, w: 600, h: 300 });
  assert.deepEqual(M.deleteChartOp('Data', 'c1'), { op: 'delete_chart', tab: 'Data', id: 'c1' });
});
test('charts: floating geometry uses custom widths, heights and automatic rows', () => {
  const tab = tabOf({}, { cols: { A: 150, B: 80 }, rows: { 1: 40 } });
  const axes = M.axesOf(tab, { rows: 100, cols: 26 }, { 2: 60 });
  assert.deepEqual(M.chartBox({ at: 'C3', dx: 5, dy: 7, w: 480, h: 296 }, axes), { left: 235, top: 107, width: 480, height: 296 });
  assert.deepEqual(M.chartPlacement(238, 110, axes), { at: 'C3', dx: 8, dy: 10 });
  assert.deepEqual(M.chartPlacement(151, 42, axes), { at: 'B2', dx: 1, dy: 2 });
  assert.deepEqual(M.chartPlacement(-4, -8, axes), { at: 'A1', dx: 0, dy: 0 });
});
test('charts: a single cell expands only to its connected data block', () => {
  const tab = tabOf({ A1: 'Name', B1: 'N', A2: 'One', B2: '1', A3: 'Two', B3: '2', D1: 'Separate', D2: '9' });
  assert.equal(M.rectStr(M.chartDataBlock(tab, M.cellRect(2, 2))), 'A1:B3');
  assert.equal(M.rectStr(M.chartDataBlock(tab, M.cellRect(1, 4))), 'D1:D2');
  assert.equal(M.rectStr(M.chartDataBlock(tab, M.cellRect(7, 8))), 'H7');
  assert.equal(M.rectStr(M.chartDataBlock(tab, M.parseRange('B2:C3'))), 'B2:C3');
});

let failed = 0;
for (const [name, error] of results) {
  if (error) { failed++; console.log(`FAIL ${name}\n${error.stack}`); } else console.log(`ok   ${name}`);
}
console.log(`${results.length - failed}/${results.length} passed`);
if (failed) process.exit(1);
