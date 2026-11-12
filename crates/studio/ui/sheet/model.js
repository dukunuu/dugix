// Sheet mode's pure model: A1 addresses and ranges, the selection, the JSON of every
// edit the UI can make, TSV, the fill handle and the cells a viewport needs. No DOM.
// Rows and columns are one-based, as in the core.
export const MAX_ROWS = 1048576, MAX_COLS = 16384;
export const DEFAULT_COL = 100, DEFAULT_ROW = 24;
const clamp = (n, lo, hi) => Math.max(lo, Math.min(hi, n));

// -- addresses and ranges ----------------------------------------------------------------

export function colName(n) {
  let out = '';
  for (; n > 0; n = Math.floor((n - 1) / 26)) out = String.fromCharCode(65 + (n - 1) % 26) + out;
  return out;
}
export function colIndex(letters) {
  if (!/^[A-Z]{1,3}$/.test(letters)) return 0;
  let n = 0;
  for (const ch of letters) n = n * 26 + ch.charCodeAt(0) - 64;
  return n <= MAX_COLS ? n : 0;
}
export const addr = (row, col) => `${colName(col)}${row}`;
/** `B12` (or `$B$12`) as {row, col}; null when it is not a cell. */
export function parseAddr(text) {
  const m = /^\$?([A-Za-z]{1,3})\$?(\d{1,7})$/.exec(String(text).trim());
  if (!m) return null;
  const col = colIndex(m[1].toUpperCase()), row = Number(m[2]);
  return col && row >= 1 && row <= MAX_ROWS ? { row, col } : null;
}
export const rect = (a, b) => ({ r1: Math.min(a.row, b.row), c1: Math.min(a.col, b.col), r2: Math.max(a.row, b.row), c2: Math.max(a.col, b.col) });
export const cellRect = (row, col) => ({ r1: row, c1: col, r2: row, c2: col });
export const isCell = r => r.r1 === r.r2 && r.c1 === r.c2;
export const rectStr = r => (isCell(r) ? addr(r.r1, r.c1) : `${addr(r.r1, r.c1)}:${addr(r.r2, r.c2)}`);
export const inRect = (r, row, col) => row >= r.r1 && row <= r.r2 && col >= r.c1 && col <= r.c2;
export const overlaps = (a, b) => a.r1 <= b.r2 && a.r2 >= b.r1 && a.c1 <= b.c2 && a.c2 >= b.c1;
export const rectSize = r => ({ rows: r.r2 - r.r1 + 1, cols: r.c2 - r.c1 + 1 });
/** `A1`, `A1:C5`, `A:C` or `2:4`; null when malformed. */
export function parseRange(text) {
  const t = String(text).trim();
  const [first, last] = t.split(':');
  if (last === undefined) { const a = parseAddr(first); return a && cellRect(a.row, a.col); }
  if (/^[A-Za-z]+$/.test(first) && /^[A-Za-z]+$/.test(last)) {
    const a = colIndex(first.toUpperCase()), b = colIndex(last.toUpperCase());
    return a && b ? { r1: 1, c1: Math.min(a, b), r2: MAX_ROWS, c2: Math.max(a, b) } : null;
  }
  if (/^\d+$/.test(first) && /^\d+$/.test(last)) {
    const a = Number(first), b = Number(last);
    return a >= 1 && b >= 1 && a <= MAX_ROWS && b <= MAX_ROWS ? { r1: Math.min(a, b), c1: 1, r2: Math.max(a, b), c2: MAX_COLS } : null;
  }
  const a = parseAddr(first), b = parseAddr(last);
  return a && b ? rect(a, b) : null;
}
const bareTab = /^[A-Za-z_][A-Za-z0-9_.]*$/;
/** A tab name as a formula writes it before the `!`. */
export const quoteTab = name => (bareTab.test(name) ? name : `'${name.replaceAll("'", "''")}'`);
/** `Data!A1` or `'My Tab'!B2:C3`; the tab is null when the text has none. */
export function parseRef(text) {
  const t = String(text).trim();
  const m = /^(?:('(?:[^']|'')+'|[^'!]+)!)?([^!]+)$/.exec(t);
  if (!m) return null;
  const range = parseRange(m[2]);
  if (!range) return null;
  const tab = m[1] === undefined ? null : m[1].startsWith("'") ? m[1].slice(1, -1).replaceAll("''", "'") : m[1];
  return { tab, rect: range };
}
/** The reference a click or arrow inserts into a formula; the tab shows only when it is another one. */
export const refText = (r, tab, current) => `${tab && tab !== current ? `${quoteTab(tab)}!` : ''}${rectStr(r)}`;

// -- axes: sparse sizes, positions and the index under a pixel ----------------------------

/** Rows or columns of one default size with a few overrides (index -> size). */
export function makeAxis(def, overrides, count) {
  const idx = [...overrides.keys()].filter(i => i >= 1).sort((a, b) => a - b);
  const sizes = idx.map(i => overrides.get(i));
  const prefix = [0];
  sizes.forEach((s, k) => prefix.push(prefix[k] + s - def));
  const before = i => { let lo = 0, hi = idx.length; while (lo < hi) { const mid = (lo + hi) >> 1; if (idx[mid] < i) lo = mid + 1; else hi = mid; } return lo; };
  const pos = i => (i - 1) * def + prefix[before(i)];
  const size = i => (overrides.has(i) ? overrides.get(i) : def);
  const total = pos(count + 1);
  const at = offset => {
    let lo = 1, hi = count;
    while (lo < hi) { const mid = (lo + hi + 1) >> 1; if (pos(mid) <= offset) lo = mid; else hi = mid - 1; }
    return clamp(lo, 1, count);
  };
  return { pos, size, at, total, count, def };
}
/** The axes of a tab: column widths are stored by letters, row heights by number.
 *  `auto` holds the derived heights of wrapped rows; a user-set height wins. */
export function axesOf(tab, extent, auto = null) {
  const cols = new Map(), rows = new Map();
  for (const [name, w] of Object.entries(tab.cols || {})) { const i = colIndex(name); if (i) cols.set(i, w); }
  for (const [row, h] of Object.entries(auto || {})) rows.set(Number(row), h);
  for (const [row, h] of Object.entries(tab.rows || {})) rows.set(Number(row), h);
  return { cols: makeAxis(DEFAULT_COL, cols, extent.cols), rows: makeAxis(DEFAULT_ROW, rows, extent.rows) };
}
/**
 * The columns a cell's text may spill over: the cell itself when it is wrapped, clipped,
 * a number or empty, else the run of empty neighbours in the reading direction. `filled`
 * reports whether a column in this row holds a value.
 */
export function overflowSpan(align, wrap, numeric, col, last, filled) {
  if (wrap || numeric) return { c1: col, c2: col };
  let c1 = col, c2 = col;
  if (align === 'left' || align === 'center') while (c2 < last && !filled(c2 + 1)) c2++;
  if (align === 'right' || align === 'center') while (c1 > 1 && !filled(c1 - 1)) c1--;
  return { c1, c2 };
}
/** The rows and columns a scroll position shows, plus a margin. */
export function windowOf(axes, { left, top, width, height }, margin = { rows: 5, cols: 2 }) {
  return {
    r1: Math.max(1, axes.rows.at(top) - margin.rows), r2: Math.min(axes.rows.count, axes.rows.at(top + height) + margin.rows),
    c1: Math.max(1, axes.cols.at(left) - margin.cols), c2: Math.min(axes.cols.count, axes.cols.at(left + width) + margin.cols),
  };
}
/** The grid's size: a screenful of blank cells beyond the data, growing as the selection goes further. */
export function extentOf(used, previous, sel) {
  const last = sel ? Math.max(sel.head.row, sel.active.row, sel.anchor.row) : 1;
  const wide = sel ? Math.max(sel.head.col, sel.active.col, sel.anchor.col) : 1;
  return {
    rows: Math.min(MAX_ROWS, Math.max(previous?.rows || 0, 100, (used?.r2 || 0) + 50, last + 25)),
    cols: Math.min(MAX_COLS, Math.max(previous?.cols || 0, 26, (used?.c2 || 0) + 3, wide + 3)),
  };
}
/** The ranges to ask the backend for, at most `max` cells each (it answers 10,000 at most). */
export function fetchRanges(win, max = 10000) {
  const cols = win.c2 - win.c1 + 1, step = Math.max(1, Math.floor(max / cols)), out = [];
  for (let r = win.r1; r <= win.r2; r += step) out.push({ r1: r, c1: win.c1, r2: Math.min(win.r2, r + step - 1), c2: win.c2 });
  return out;
}
export const covers = (loaded, want) => loaded.some(r => r.r1 <= want.r1 && r.r2 >= want.r2 && r.c1 <= want.c1 && r.c2 >= want.c2);

// -- the tab's data: used range, sparse cells ----------------------------------------------

const indexes = new WeakMap();
/** Bounds of the stored cells, cached by tab object. */
export function usedRect(tab) {
  let hit = indexes.get(tab.cells);
  if (hit === undefined) {
    hit = null;
    for (const key in tab.cells) {
      const a = parseAddr(key);
      if (!a) continue;
      hit = hit ? { r1: Math.min(hit.r1, a.row), c1: Math.min(hit.c1, a.col), r2: Math.max(hit.r2, a.row), c2: Math.max(hit.c2, a.col) } : cellRect(a.row, a.col);
    }
    indexes.set(tab.cells, hit);
  }
  return hit;
}
/** What jumps and fills ask of a tab: which cells hold an input, and how far the grid goes. */
export function dataOf(tab, extent) {
  return { filled: (row, col) => { const c = tab.cells[addr(row, col)]; return Boolean(c && c.input !== undefined && c.input !== ''); }, maxRow: extent?.rows || MAX_ROWS, maxCol: extent?.cols || MAX_COLS };
}
export const mergesOf = tab => (tab.merges || []).map(parseRange).filter(Boolean);
export const mergeAt = (merges, row, col) => merges.find(m => inRect(m, row, col)) || null;
export const isFormula = text => typeof text === 'string' && text.startsWith('=');

// -- the selection ---------------------------------------------------------------------------

export const newSel = (row, col) => ({ kind: 'cells', anchor: { row, col }, head: { row, col }, active: { row, col } });
export function selRect(sel) {
  const { anchor: a, head: h } = sel;
  if (sel.kind === 'all') return { r1: 1, c1: 1, r2: MAX_ROWS, c2: MAX_COLS };
  if (sel.kind === 'cols') return { r1: 1, c1: Math.min(a.col, h.col), r2: MAX_ROWS, c2: Math.max(a.col, h.col) };
  if (sel.kind === 'rows') return { r1: Math.min(a.row, h.row), c1: 1, r2: Math.max(a.row, h.row), c2: MAX_COLS };
  return rect(a, h);
}
/** The part of the selection edits apply to: a whole column or row stops at the used range. */
export function opRect(sel, used) {
  const r = selRect(sel);
  if (sel.kind === 'cols' || sel.kind === 'all') r.r2 = Math.max(1, Math.min(r.r2, used?.r2 || 1));
  if (sel.kind === 'rows' || sel.kind === 'all') r.c2 = Math.max(1, Math.min(r.c2, used?.c2 || 1));
  return r;
}
export function selectCol(sel, col, extend) {
  if (extend && sel.kind === 'cols') return { ...sel, head: { row: 1, col } };
  return { kind: 'cols', anchor: { row: 1, col }, head: { row: 1, col }, active: { row: sel?.active?.row || 1, col } };
}
export function selectRow(sel, row, extend) {
  if (extend && sel.kind === 'rows') return { ...sel, head: { row, col: 1 } };
  return { kind: 'rows', anchor: { row, col: 1 }, head: { row, col: 1 }, active: { row, col: sel?.active?.col || 1 } };
}
export const selectAll = sel => ({ kind: 'all', anchor: { row: 1, col: 1 }, head: { row: MAX_ROWS, col: MAX_COLS }, active: sel?.active || { row: 1, col: 1 } });
/** Click-drag or shift-click: the range from the anchor to a cell. */
export const extendTo = (sel, row, col) => ({ ...sel, kind: 'cells', anchor: sel.kind === 'cells' ? sel.anchor : sel.active, head: { row, col } });

/** Where a Ctrl/Cmd+arrow lands: the end of a run of filled cells, the next filled cell, or the grid's edge. */
export function jumpTo(data, pos, dr, dc) {
  const limit = { row: data.maxRow, col: data.maxCol };
  const step = p => ({ row: p.row + dr, col: p.col + dc });
  const inside = p => p.row >= 1 && p.col >= 1 && p.row <= limit.row && p.col <= limit.col;
  const full = p => data.filled(p.row, p.col);
  let next = step(pos);
  if (!inside(next)) return pos;
  if (full(pos) && full(next)) {
    for (let after = step(next); inside(after) && full(after); after = step(after)) next = after;
    return next;
  }
  for (; inside(next) && !full(next); next = step(next)) { /* walk to the next input */ }
  if (inside(next)) return next;
  return { row: dr < 0 ? 1 : dr > 0 ? limit.row : pos.row, col: dc < 0 ? 1 : dc > 0 ? limit.col : pos.col };
}
/** An arrow key: move the active cell, or with `extend` the far corner; `jump` goes to the data edge. */
export function moveSel(sel, dr, dc, { extend = false, jump = false } = {}, data) {
  const from = extend && sel.kind === 'cells' ? sel.head : sel.active;
  let to = jump ? jumpTo(data, from, dr, dc) : { row: from.row + dr, col: from.col + dc };
  to = { row: clamp(to.row, 1, MAX_ROWS), col: clamp(to.col, 1, MAX_COLS) };
  if (extend) {
    if (sel.kind === 'cols') return { ...sel, head: { row: 1, col: to.col } };
    if (sel.kind === 'rows') return { ...sel, head: { row: to.row, col: 1 } };
    const anchor = sel.kind === 'cells' ? sel.anchor : sel.active;
    return { kind: 'cells', anchor, head: to, active: sel.active };
  }
  return newSel(to.row, to.col);
}
/** Enter and Tab: the next cell inside a multi-cell selection (wrapping), else one cell on. */
export function stepWithin(sel, dir, back) {
  const r = selRect(sel), a = sel.active, d = back ? -1 : 1;
  if (sel.kind !== 'cells' || isCell(r)) return newSel(clamp(a.row + (dir === 'down' ? d : 0), 1, MAX_ROWS), clamp(a.col + (dir === 'right' ? d : 0), 1, MAX_COLS));
  let { row, col } = a;
  if (dir === 'down') {
    row += d;
    if (row > r.r2) { row = r.r1; col = col + 1 > r.c2 ? r.c1 : col + 1; }
    else if (row < r.r1) { row = r.r2; col = col - 1 < r.c1 ? r.c2 : col - 1; }
  } else {
    col += d;
    if (col > r.c2) { col = r.c1; row = row + 1 > r.r2 ? r.r1 : row + 1; }
    else if (col < r.c1) { col = r.c2; row = row - 1 < r.r1 ? r.r2 : row - 1; }
  }
  return { ...sel, active: { row, col } };
}

// -- the fill handle ---------------------------------------------------------------------------------

/** The range a fill-handle drag covers when the pointer is over `hover`: the source extended down or right. */
export function fillTarget(src, hover) {
  const down = hover.row - src.r2, right = hover.col - src.c2;
  if (down <= 0 && right <= 0) return null;
  return down >= right ? { ...src, r2: Math.min(MAX_ROWS, hover.row) } : { ...src, c2: Math.min(MAX_COLS, hover.col) };
}
/** Double-clicking the handle: fill down as far as the neighbouring column has data. */
export function fillDownEnd(src, data) {
  for (const col of [src.c1 - 1, src.c2 + 1]) {
    if (col < 1 || col > MAX_COLS || !data.filled(src.r2 + 1, col)) continue;
    let row = src.r2 + 1;
    while (row < MAX_ROWS && data.filled(row + 1, col)) row++;
    return { ...src, r2: row };
  }
  return null;
}
/** The width that fits the widest of `texts`; `measure(text)` is the pixel width of one. */
export function fitWidth(texts, measure, { pad = 16, min = 28, max = 600 } = {}) {
  let widest = 0;
  for (const t of texts) widest = Math.max(widest, measure(t));
  return clamp(Math.ceil(widest + pad), min, max);
}

// -- formats ----------------------------------------------------------------------------------------------

export const NUMBER_FORMATS = [['Automatic', null], ['Number', '#,##0.00'], ['Percent', '0.00%'], ['Currency', '$#,##0.00'], ['Date', 'yyyy-mm-dd'], ['Plain text', '@']];
export const numberFormatName = code => (NUMBER_FORMATS.find(([, c]) => c === (code ?? null)) || ['Custom'])[0];
/** A JSON merge patch applied to a cell format; false and null both remove a field. */
export function mergeFormat(fmt, patch) {
  const out = { ...fmt };
  for (const [key, value] of Object.entries(patch)) {
    if (value === null || value === false) delete out[key]; else out[key] = value;
  }
  return out;
}
/** A colour token or hex of a sheet's theme as CSS, or null. */
export function cssColor(theme, value) {
  if (!value) return null;
  if (/^#[0-9a-f]{3,8}$/i.test(value)) return value;
  const hex = theme?.colors?.[value];
  return /^#[0-9a-f]{3,8}$/i.test(hex || '') ? hex : null;
}
/** How an input shows before the backend has calculated it. */
export function literalValue(input) {
  if (input === '') return null;
  if (isFormula(input)) return null;
  if (input.trim() !== '' && Number.isFinite(Number(input))) return { display: input, kind: 'number' };
  if (/^(true|false)$/i.test(input)) return { display: input.toUpperCase(), kind: 'bool' };
  return { display: input, kind: 'text' };
}
/** `left`, `center` or `right` for a cell: its format, else the kind of its value. */
export function alignOf(fmt, value) {
  if (fmt?.align) return fmt.align;
  return value?.kind === 'number' ? 'right' : value?.kind === 'bool' ? 'center' : 'left';
}

// -- edits: the JSON of every action ------------------------------------------------------------------------

/** Each builder returns one op; a user action sends its ops in one `SheetApply`, one undo step. */
export const op = {
  setCells: (tab, at, values) => ({ op: 'set_cells', tab, at, values }),
  clear: (tab, range, what = 'inputs') => ({ op: 'clear', tab, range, what }),
  format: (tab, range, patch) => ({ op: 'format', tab, range, patch }),
  insertRows: (tab, at, count = 1) => ({ op: 'insert_rows', tab, at, count }),
  deleteRows: (tab, at, count = 1) => ({ op: 'delete_rows', tab, at, count }),
  insertCols: (tab, at, count = 1) => ({ op: 'insert_cols', tab, at: typeof at === 'number' ? colName(at) : at, count }),
  deleteCols: (tab, at, count = 1) => ({ op: 'delete_cols', tab, at: typeof at === 'number' ? colName(at) : at, count }),
  copy: (tab, from, to, cut = false, toTab = null) => ({ op: 'copy', tab, from, to, cut, ...(toTab ? { to_tab: toTab } : {}) }),
  fill: (tab, from, to) => ({ op: 'fill', tab, from, to }),
  sort: (tab, range, by, desc, header) => ({ op: 'sort', tab, range, by, desc, header }),
  colWidth: (tab, cols, width) => ({ op: 'set_col_width', tab, cols, width }),
  fitCols: (tab, cols) => ({ op: 'fit_cols', tab, cols }),
  rowHeight: (tab, rows, height) => ({ op: 'set_row_height', tab, rows, height }),
  freeze: (tab, rows, cols) => ({ op: 'freeze', tab, rows, cols }),
  merge: (tab, range) => ({ op: 'merge', tab, range }),
  unmerge: (tab, range) => ({ op: 'unmerge', tab, range }),
  addTab: (name, after = null) => ({ op: 'add_tab', name, after }),
  renameTab: (tab, name) => ({ op: 'rename_tab', tab, name }),
  deleteTab: tab => ({ op: 'delete_tab', tab }),
  moveTab: (tab, after = null) => ({ op: 'move_tab', tab, after }),
  setTitle: title => ({ op: 'set_title', title }),
};
/** Typed text into a cell. */
export const setCell = (tab, row, col, text) => op.setCells(tab, addr(row, col), [[text]]);
/** A block of text at a cell, padded to a rectangle. */
export function setBlock(tab, row, col, rows) {
  const width = Math.max(1, ...rows.map(r => r.length));
  return op.setCells(tab, addr(row, col), rows.map(r => Array.from({ length: width }, (_, i) => r[i] ?? '')));
}
/** Bold, italic or underline: switched off when the active cell has it, otherwise on for the whole range. */
export const toggleFormat = (tab, range, current, key) => op.format(tab, range, { [key]: !current?.[key] });
export const numberFormat = (tab, range, code) => op.format(tab, range, { num: code });
export const alignFormat = (tab, range, align) => op.format(tab, range, { align });
/** How text that does not fit its cell is drawn, and where it sits vertically. */
export const WRAP_MODES = [['Overflow', 'overflow'], ['Wrap', 'wrap'], ['Clip', 'clip']];
export const VALIGNS = [['Top', 'top'], ['Middle', 'middle'], ['Bottom', 'bottom']];
export const wrapFormat = (tab, range, wrap) => op.format(tab, range, { wrap });
export const valignFormat = (tab, range, valign) => op.format(tab, range, { valign });
/** Overflow ↔ wrap, as ⌘/Ctrl+Alt+W does it. */
export const toggleWrap = (tab, range, current) => wrapFormat(tab, range, current?.wrap === 'wrap' ? null : 'wrap');
export const colorFormat = (tab, range, kind, token) => op.format(tab, range, { [kind === 'fill' ? 'fill' : 'color']: token });
/** Merge the range, or split it when it already lies inside a merge. */
export function mergeToggle(tab, r, merges) {
  const hit = merges.find(m => overlaps(m, r));
  if (hit) return op.unmerge(tab, rectStr(r));
  return isCell(r) ? null : op.merge(tab, rectStr(r));
}
/** Sort by a column over the used range; row 1 is a header when it is frozen. */
export function sortOps(tab, used, col, desc, frozenRows) {
  const range = used ? { r1: used.r1, c1: Math.min(used.c1, col), r2: used.r2, c2: Math.max(used.c2, col) } : cellRect(1, col);
  return [op.sort(tab, rectStr(range), colName(col), desc, frozenRows >= 1)];
}
/** Resize: one op for the contiguous columns or rows. */
export const resizeOp = (tab, axis, from, to, size) => (axis === 'col'
  ? op.colWidth(tab, `${colName(from)}:${colName(to)}`, size)
  : op.rowHeight(tab, `${from}:${to}`, size));
/** The first name `SheetN` that no tab uses. */
export function nextTabName(names) {
  const taken = new Set(names.map(n => n.toLowerCase()));
  for (let n = names.length + 1; ; n++) if (!taken.has(`sheet${n}`)) return `Sheet${n}`;
}
/** `move_tab` ops that bring `names` to `target` order; each op puts one tab right after its predecessor. */
export function reorderOps(names, target) {
  const order = [...names], out = [];
  for (let i = 1; i < target.length; i++) {
    const at = order.indexOf(target[i]);
    if (at > 0 && order[at - 1] === target[i - 1]) continue;
    order.splice(at, 1);
    order.splice(order.indexOf(target[i - 1]) + 1, 0, target[i]);
    out.push(op.moveTab(target[i], target[i - 1]));
  }
  return out;
}
/** The order after dragging the tab at `from` to slot `to` (0 = first). */
export function dragOrder(names, from, to) {
  const out = [...names], [name] = out.splice(from, 1);
  out.splice(to, 0, name);
  return out;
}
/** Which header-menu edit goes where: `at` for rows and columns above/left or below/right of the selection. */
export const insertAt = (r, axis, after) => (axis === 'row' ? (after ? r.r2 + 1 : r.r1) : (after ? r.c2 + 1 : r.c1));

// -- applying ops to the page's copy at once ------------------------------------------------------------------

/**
 * The sheet after the ops that need no calculation (typing, clearing, formatting, sizes,
 * merges), so the page can show them before the backend answers; null for any other op.
 */
export function applyLocal(sheet, ops) {
  const tabs = sheet.tabs.slice();
  let title = sheet.title;
  const tabFor = name => {
    const i = tabs.findIndex(t => t.name.toLowerCase() === String(name).toLowerCase());
    return i < 0 ? null : i;
  };
  for (const o of ops) {
    if (o.op === 'set_title') { title = o.title; continue; }
    const i = tabFor(o.tab);
    if (i === null) return null;
    const tab = { ...tabs[i] };
    if (o.op === 'set_cells') {
      const at = parseAddr(o.at);
      if (!at) return null;
      tab.cells = { ...tab.cells };
      o.values.forEach((line, r) => line.forEach((input, c) => {
        const key = addr(at.row + r, at.col + c), had = tab.cells[key];
        if (input === '' && !had) return;
        tab.cells[key] = { ...had, input };
        if (input === '') delete tab.cells[key].input;
      }));
    } else if (o.op === 'clear') {
      const r = parseRange(o.range);
      if (!r) return null;
      tab.cells = { ...tab.cells };
      for (const key of Object.keys(tab.cells)) {
        const a = parseAddr(key);
        if (!a || !inRect(r, a.row, a.col)) continue;
        const next = { ...tab.cells[key] };
        if (o.what !== 'formats') delete next.input;
        if (o.what !== 'inputs') delete next.fmt;
        if (Object.keys(next).length) tab.cells[key] = next; else delete tab.cells[key];
      }
    } else if (o.op === 'format') {
      const r = parseRange(o.range);
      if (!r || rectSize(r).rows * rectSize(r).cols > 50000) return null;
      tab.cells = { ...tab.cells };
      for (let row = r.r1; row <= r.r2; row++) for (let col = r.c1; col <= r.c2; col++) {
        const key = addr(row, col), cell = { ...tab.cells[key] }, fmt = mergeFormat(cell.fmt || {}, o.patch);
        if (Object.keys(fmt).length) cell.fmt = fmt; else delete cell.fmt;
        if (Object.keys(cell).length) tab.cells[key] = cell; else delete tab.cells[key];
      }
    } else if (o.op === 'set_col_width') {
      const r = parseRange(o.cols);
      if (!r) return null;
      tab.cols = { ...tab.cols };
      for (let c = r.c1; c <= r.c2; c++) tab.cols[colName(c)] = o.width;
    } else if (o.op === 'set_row_height') {
      const r = parseRange(o.rows);
      if (!r) return null;
      tab.rows = { ...tab.rows };
      for (let row = r.r1; row <= r.r2; row++) tab.rows[row] = o.height;
    } else if (o.op === 'freeze') tab.frozen = { rows: o.rows, cols: o.cols };
    else if (o.op === 'merge') tab.merges = [...(tab.merges || []), o.range];
    else if (o.op === 'unmerge') { const r = parseRange(o.range); tab.merges = (tab.merges || []).filter(m => !overlaps(parseRange(m), r)); }
    else return null;
    tabs[i] = tab;
  }
  return { ...sheet, title, tabs };
}

// -- formulas: pointing at cells ----------------------------------------------------------------------------------

/** Whether an arrow key or a click may insert a reference at the caret of a formula being typed. */
export function canPoint(text, caret) {
  return isFormula(text) && /[=(,+\-*/^&<>;:]$/.test(text.slice(0, caret).replace(/\s+$/, ''));
}
/** Put `ref` into a formula: over the reference just pointed at (`span`), or at the caret. */
export function pointInsert(text, caret, span, ref) {
  const start = span ? span.start : caret, end = span ? span.end : caret;
  return { text: text.slice(0, start) + ref + text.slice(end), span: { start, end: start + ref.length }, caret: start + ref.length };
}

// -- TSV: what travels through the system clipboard ---------------------------------------------------------------------

/** Tab-separated text as a rectangle of strings. Quoted fields may hold tabs, newlines and doubled quotes. */
export function parseTsv(text) {
  const rows = [];
  let row = [], field = '', quoted = false, inQuotes = false;
  const endRow = () => { row.push(field); rows.push(row); row = []; field = ''; quoted = false; };
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (inQuotes) {
      if (ch !== '"') field += ch;
      else if (text[i + 1] === '"') { field += '"'; i++; } else inQuotes = false;
    } else if (ch === '"' && field === '' && !quoted) { inQuotes = true; quoted = true; }
    else if (ch === '\t') { row.push(field); field = ''; quoted = false; }
    else if (ch === '\n' || ch === '\r') { if (ch === '\r' && text[i + 1] === '\n') i++; endRow(); }
    else field += ch;
  }
  if (field !== '' || row.length || quoted || !rows.length) endRow();
  const width = Math.max(1, ...rows.map(r => r.length));
  return rows.map(r => Array.from({ length: width }, (_, i) => r[i] ?? ''));
}
export function toTsv(rows) {
  const quote = s => (/[\t\n\r"]/.test(s) ? `"${s.replaceAll('"', '""')}"` : s);
  return rows.map(r => r.map(quote).join('\t')).join('\n');
}
/** A single pasted value repeated over a larger selection, as spreadsheets do; larger blocks stay whole. */
export function tileBlock(rows, size) {
  if (rows.length !== 1 || rows[0].length !== 1 || size.rows * size.cols > 10000 || (size.rows === 1 && size.cols === 1)) return rows;
  return Array.from({ length: size.rows }, () => Array.from({ length: size.cols }, () => rows[0][0]));
}

// -- threads and comments --------------------------------------------------------------------------------------------------

/** The cell range a thread points at, as a rect; null when it is orphaned. */
export const anchorRect = thread => (thread.orphaned ? null : parseRange(thread.anchor.range));
/** `Sheet1!B2` as the backend names a changed cell: the tab's name and the address. */
export function splitChanged(text) {
  const bang = text.lastIndexOf('!');
  return bang < 0 ? null : { tab: text.slice(0, bang), a1: text.slice(bang + 1) };
}
