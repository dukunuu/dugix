// The grid: headers, cells and their formats, virtualized. Only the rows and columns in
// view (plus a margin) exist in the DOM; frozen rows and columns live in sticky layers.
// Selection, copy, remote focus and the fill handle are classes on cells and a few boxes.
import * as M from './model.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
export const HEADER_H = 22;
const MARGIN = { rows: 8, cols: 3 };

export function gridController(api) {
  const { state } = api;
  const root = el('div', 'sg-grid');
  const scroll = el('div', 'sg-scroll');
  scroll.tabIndex = 0;
  scroll.setAttribute('role', 'grid');
  scroll.setAttribute('aria-label', 'Spreadsheet');
  const inner = el('div', 'sg-inner');
  const colbar = el('div', 'sg-colbar'), corner = el('button', 'sg-corner'), colheads = el('div', 'sg-colheads'), colheadsF = el('div', 'sg-colheads-f');
  corner.type = 'button'; corner.tabIndex = -1; corner.setAttribute('aria-label', 'Select all');
  colbar.append(corner, colheadsF, colheads);
  const rowheads = el('div', 'sg-rowheads'), rowheadsF = el('div', 'sg-rowheads-f');
  const ftop = el('div', 'sg-layer sg-ftop'), fleft = el('div', 'sg-layer sg-fleft'), fcorner = el('div', 'sg-layer sg-fcorner');
  const main = el('div', 'sg-main'), overlay = el('div', 'sg-ovl');
  main.append(overlay);
  inner.append(colbar, rowheads, rowheadsF, ftop, fleft, fcorner, main);
  scroll.append(inner);
  root.append(scroll);
  const boxes = {};
  for (const name of ['copy', 'remote', 'point', 'fill', 'resize', 'handle']) { boxes[name] = el('div', `sg-box sg-${name}`); boxes[name].hidden = true; overlay.append(boxes[name]); }
  boxes.handle.setAttribute('aria-label', 'Fill handle');

  const g = { tab: null, axes: null, extent: { rows: 100, cols: 26 }, headerW: 46, fr: 0, fc: 0, fw: 0, fh: 0, merges: [], sig: '' };
  const cells = new Map(), heads = new Map();
  let shown = null, frame = 0, pins = new Map(), flash = new Set(), measureCtx = null;

  const layerOf = (row, col) => (row <= g.fr ? (col <= g.fc ? fcorner : ftop) : col <= g.fc ? fleft : main);
  const headerWidth = rows => Math.max(40, String(rows).length * 8 + 18);

  /** Recompute sizes from the current tab; the DOM is rebuilt when anything geometric changed. */
  function layout() {
    const tab = api.tab();
    g.tab = tab;
    if (!tab) return;
    g.extent = api.extent();
    g.axes = M.axesOf(tab, g.extent);
    g.headerW = headerWidth(g.extent.rows);
    g.fr = tab.frozen?.rows || 0; g.fc = tab.frozen?.cols || 0;
    g.fw = g.axes.cols.pos(g.fc + 1); g.fh = g.axes.rows.pos(g.fr + 1);
    g.merges = M.mergesOf(tab);
    const sig = JSON.stringify([tab.id, g.extent, tab.cols, tab.rows, g.fr, g.fc, tab.merges, g.headerW]);
    if (sig !== g.sig) {
      g.sig = sig;
      for (const { node } of cells.values()) node.remove();
      for (const node of heads.values()) node.remove();
      cells.clear(); heads.clear(); shown = null;
    }
    const w = g.headerW, style = scroll.style;
    style.setProperty('--sg-hw', `${w}px`); style.setProperty('--sg-hh', `${HEADER_H}px`);
    inner.style.width = `${w + g.axes.cols.total + 24}px`; inner.style.height = `${HEADER_H + g.axes.rows.total}px`;
    colbar.style.width = inner.style.width;
    colheads.style.width = `${g.axes.cols.total}px`;
    style.setProperty('--sg-fw', `${g.fw}px`);
    ftop.style.marginLeft = fcorner.style.marginLeft = `${w}px`;
    main.style.left = `${w}px`; main.style.top = `${HEADER_H}px`;
    main.style.width = `${g.axes.cols.total}px`; main.style.height = `${g.axes.rows.total}px`;
  }

  const view = () => ({ left: scroll.scrollLeft, top: scroll.scrollTop, width: Math.max(0, scroll.clientWidth - g.headerW), height: Math.max(0, scroll.clientHeight - HEADER_H) });
  const same = (a, b) => a && b && a.r1 === b.r1 && a.r2 === b.r2 && a.c1 === b.c1 && a.c2 === b.c2;
  const inside = (outer, win) => outer && win.r1 >= outer.r1 && win.r2 <= outer.r2 && win.c1 >= outer.c1 && win.c2 <= outer.c2;

  /** The cells a window needs: merges anchor at their top-left, the cells they hide are skipped. */
  function wanted(win) {
    const out = [];
    const add = (row, col) => {
      const m = M.mergeAt(g.merges, row, col);
      if (m && (m.r1 !== row || m.c1 !== col)) return;
      out.push([row, col, m]);
    };
    const rows = [], cols = [];
    for (let r = 1; r <= Math.min(g.fr, g.extent.rows); r++) rows.push(r);
    for (let r = Math.max(win.r1, g.fr + 1); r <= win.r2; r++) rows.push(r);
    for (let c = 1; c <= Math.min(g.fc, g.extent.cols); c++) cols.push(c);
    for (let c = Math.max(win.c1, g.fc + 1); c <= win.c2; c++) cols.push(c);
    for (const r of rows) for (const c of cols) add(r, c);
    for (const m of g.merges) {
      if (!M.overlaps(m, win) || (m.r1 >= win.r1 && m.c1 >= win.c1 && m.r1 <= win.r2 && m.c1 <= win.c2)) continue;
      if (!out.some(([r, c]) => r === m.r1 && c === m.c1)) out.push([m.r1, m.c1, m]);
    }
    return out;
  }

  const css = value => M.cssColor(state.sheet.theme, value);
  function appearance(row, col, merge) {
    const tab = g.tab, a1 = M.addr(row, col), stored = tab.cells[a1];
    const value = api.valueAt(a1) ?? (stored && !M.isFormula(stored.input ?? '') ? M.literalValue(stored.input ?? '') : null);
    const fmt = stored?.fmt;
    const cls = ['sg-cell'];
    if (fmt?.bold) cls.push('b');
    if (fmt?.italic) cls.push('i');
    if (fmt?.underline) cls.push('u');
    cls.push(`al-${M.alignOf(fmt, value)}`);
    if (value?.kind === 'error') cls.push('err');
    if (merge) cls.push('merged');
    if (g.fr && (merge ? merge.r2 : row) === g.fr) cls.push('fz-b');
    if (g.fc && (merge ? merge.c2 : col) === g.fc) cls.push('fz-r');
    const text = value?.display ?? '';
    const pin = pins.get(`${row},${col}`);
    return { cls: cls.join(' '), text, fill: css(fmt?.fill), color: value?.kind === 'error' ? '' : css(fmt?.color), pin: pin ? pin.length : 0, flash: flash.has(a1), tip: value?.error || '' };
  }
  function makeCell(row, col, merge) {
    const node = el('div', 'sg-cell');
    const text = el('span', 'sg-text');
    node.append(text);
    node.dataset.r = row; node.dataset.c = col;
    node.setAttribute('role', 'gridcell');
    const { axes } = g;
    const r2 = merge ? merge.r2 : row, c2 = merge ? merge.c2 : col;
    Object.assign(node.style, { left: `${axes.cols.pos(col)}px`, top: `${axes.rows.pos(row)}px`, width: `${axes.cols.pos(c2 + 1) - axes.cols.pos(col)}px`, height: `${axes.rows.pos(r2 + 1) - axes.rows.pos(row)}px` });
    const entry = { node, text, row, col, r2, c2, sig: '', flags: '' };
    (layerOf(row, col)).append(node);
    return entry;
  }
  function paintCell(entry, merge) {
    const a = appearance(entry.row, entry.col, merge);
    const sig = `${a.cls}|${a.text}|${a.fill}|${a.color}|${a.pin}|${a.flash}|${a.tip}`;
    if (sig === entry.sig) return;
    entry.sig = sig;
    entry.base = a.cls + (a.flash ? ' sg-flash' : '') + (a.pin ? ' has-pin' : '');
    entry.text.textContent = a.text;
    entry.node.style.background = a.fill || '';
    entry.node.style.color = a.color || '';
    entry.node.title = a.tip;
    entry.pin?.remove(); entry.pin = null;
    if (a.pin) { entry.pin = el('i', 'sg-pin'); entry.pin.dataset.pin = `${entry.row},${entry.col}`; entry.node.append(entry.pin); }
    entry.flags = '';
    entry.node.className = entry.base;
  }

  /** Bring the DOM in line with the window: create what scrolled in, drop what scrolled out, repaint what changed. */
  function paintCells(win) {
    const need = wanted(win), keep = new Set();
    for (const [row, col, merge] of need) {
      const key = `${row},${col}`;
      keep.add(key);
      let entry = cells.get(key);
      if (!entry) { entry = makeCell(row, col, merge); cells.set(key, entry); }
      paintCell(entry, merge);
    }
    for (const [key, entry] of cells) if (!keep.has(key)) { entry.node.remove(); cells.delete(key); }
  }

  function headCell(kind, index) {
    const key = `${kind}${index}`;
    let node = heads.get(key);
    if (node) return node;
    node = el('div', kind === 'c' ? 'sg-ch' : 'sg-rh', kind === 'c' ? M.colName(index) : String(index));
    node.dataset[kind] = index;
    const { axes } = g;
    if (kind === 'c') Object.assign(node.style, { left: `${axes.cols.pos(index)}px`, width: `${axes.cols.size(index)}px`, height: `${HEADER_H}px` });
    else Object.assign(node.style, { top: `${axes.rows.pos(index)}px`, height: `${axes.rows.size(index)}px`, width: `${g.headerW}px` });
    if (kind === 'c' ? index === g.fc && g.fc : index === g.fr && g.fr) node.classList.add(kind === 'c' ? 'fz-r' : 'fz-b');
    heads.set(key, node);
    return node;
  }
  function paintHeads(win) {
    const keep = new Set();
    const cols = [], rows = [];
    for (let c = 1; c <= Math.min(g.fc, g.extent.cols); c++) cols.push([c, colheadsF]);
    for (let c = Math.max(win.c1, g.fc + 1); c <= win.c2; c++) cols.push([c, colheads]);
    for (let r = 1; r <= Math.min(g.fr, g.extent.rows); r++) rows.push([r, rowheadsF]);
    for (let r = Math.max(win.r1, g.fr + 1); r <= win.r2; r++) rows.push([r, rowheads]);
    for (const [c, parent] of cols) { const n = headCell('c', c); keep.add(`c${c}`); if (n.parentNode !== parent) parent.append(n); }
    for (const [r, parent] of rows) { const n = headCell('r', r); keep.add(`r${r}`); if (n.parentNode !== parent) parent.append(n); }
    for (const [key, node] of heads) if (!keep.has(key)) { node.remove(); heads.delete(key); }
  }

  /** Selection, active cell and header highlights, as classes on what is on screen. */
  function paintSelection() {
    const sel = state.sel;
    if (!sel || !g.axes) return;
    const r = M.selRect(sel), a = sel.active, single = sel.kind === 'cells' && M.isCell(r);
    for (const entry of cells.values()) {
      const node = entry.node, hit = M.overlaps({ r1: entry.row, c1: entry.col, r2: entry.r2, c2: entry.c2 }, r);
      let flags = '';
      if (hit && !single) {
        flags = ' sg-sel';
        if (entry.row <= r.r1) flags += ' sg-t';
        if (entry.r2 >= r.r2) flags += ' sg-b';
        if (entry.col <= r.c1) flags += ' sg-l';
        if (entry.c2 >= r.c2) flags += ' sg-r';
      }
      if (M.inRect({ r1: entry.row, c1: entry.col, r2: entry.r2, c2: entry.c2 }, a.row, a.col)) flags += ' sg-active';
      if (flags !== entry.flags) { entry.flags = flags; node.className = entry.base + flags; }
    }
    for (const node of heads.values()) {
      const on = node.dataset.c ? Number(node.dataset.c) >= r.c1 && Number(node.dataset.c) <= r.c2 : Number(node.dataset.r) >= r.r1 && Number(node.dataset.r) <= r.r2;
      const whole = node.dataset.c ? sel.kind === 'cols' || sel.kind === 'all' : sel.kind === 'rows' || sel.kind === 'all';
      node.classList.toggle('on', on);
      node.classList.toggle('whole', on && whole);
    }
    corner.classList.toggle('on', sel.kind === 'all');
    placeHandle();
  }

  // -- boxes: copy marquee, remote focus, point mode, fill preview, handle ----------------------------------

  const rectBox = r => {
    const { axes } = g, x = axes.cols.pos(r.c1), y = axes.rows.pos(r.r1);
    return { left: x, top: y, width: axes.cols.pos(r.c2 + 1) - x, height: axes.rows.pos(r.r2 + 1) - y };
  };
  function box(name, r) {
    const node = boxes[name];
    if (!r || !g.axes) { node.hidden = true; return; }
    const b = rectBox({ ...r, r2: Math.min(r.r2, g.extent.rows), c2: Math.min(r.c2, g.extent.cols) });
    Object.assign(node.style, { left: `${b.left}px`, top: `${b.top}px`, width: `${b.width}px`, height: `${b.height}px` });
    node.hidden = false;
  }
  function placeHandle() {
    const sel = state.sel, node = boxes.handle;
    if (!sel || !g.axes || api.editing()) { node.hidden = true; return; }
    const r = M.selRect(sel);
    if (sel.kind !== 'cells' || r.r2 > g.extent.rows || r.c2 > g.extent.cols) { node.hidden = true; return; }
    const x = g.axes.cols.pos(r.c2 + 1), y = g.axes.rows.pos(r.r2 + 1);
    Object.assign(node.style, { left: `${x - 5}px`, top: `${y - 5}px` });
    node.hidden = false;
  }
  function paintBoxes() {
    box('copy', state.clip && state.clip.tabId === g.tab.id ? state.clip.rect : null);
    const remote = state.remote && state.remote.tabId === g.tab.id && state.remote.until > Date.now() ? state.remote.rect : null;
    box('remote', remote);
    placeHandle();
  }

  // -- rendering ----------------------------------------------------------------------------------------------

  /** `force` repaints every cell (data changed); otherwise nothing happens while the window is still covered. */
  function render(force = false) {
    if (!g.tab) return;
    const visible = M.windowOf(g.axes, view(), { rows: 0, cols: 0 });
    if (!force && shown && inside(shown, visible)) return;
    const win = M.windowOf(g.axes, view(), MARGIN);
    shown = win;
    paintHeads(win);
    paintCells(win);
    paintSelection();
    paintBoxes();
  }
  function renderAll() { layout(); if (g.tab) { shown = null; render(true); } }
  function schedule() { if (!frame) frame = requestAnimationFrame(() => { frame = 0; render(); api.scrolled(); }); }
  scroll.addEventListener('scroll', schedule);
  new ResizeObserver(() => { shown = null; schedule(); }).observe(scroll);

  // -- geometry for the pointer and the editor ----------------------------------------------------------------

  /** The row and column under a point; 0 means the header band. Frozen areas do not scroll. */
  function cellAt(clientX, clientY) {
    const b = scroll.getBoundingClientRect(), { axes } = g;
    const x = clientX - b.left - g.headerW, y = clientY - b.top - HEADER_H;
    const bx = x < g.fw ? x : x + scroll.scrollLeft, by = y < g.fh ? y : y + scroll.scrollTop;
    return { col: x < 0 ? 0 : axes.cols.at(bx), row: y < 0 ? 0 : axes.rows.at(by), x, y, bx, by };
  }
  /** Where a column or row boundary lies under the pointer within a header, if it does. */
  function edgeAt(clientX, clientY) {
    const p = cellAt(clientX, clientY), { axes } = g, slop = 4;
    if (p.row === 0 && p.col > 0) {
      const start = axes.cols.pos(p.col), end = axes.cols.pos(p.col + 1), local = p.bx - start;
      if (end - p.bx <= slop) return { axis: 'col', index: p.col };
      if (local <= slop && p.col > 1) return { axis: 'col', index: p.col - 1 };
    }
    if (p.col === 0 && p.row > 0) {
      const start = axes.rows.pos(p.row), end = axes.rows.pos(p.row + 1), local = p.by - start;
      if (end - p.by <= slop) return { axis: 'row', index: p.row };
      if (local <= slop && p.row > 1) return { axis: 'row', index: p.row - 1 };
    }
    return null;
  }
  /** A cell's rectangle on screen, for placing popups and the editor. */
  function screenRect(row, col) {
    const b = scroll.getBoundingClientRect(), { axes } = g;
    const m = M.mergeAt(g.merges, row, col) || M.cellRect(row, col);
    const x = axes.cols.pos(m.c1), y = axes.rows.pos(m.r1);
    const left = b.left + g.headerW + (col <= g.fc ? x : x - scroll.scrollLeft), top = b.top + HEADER_H + (row <= g.fr ? y : y - scroll.scrollTop);
    return { left, top, width: axes.cols.pos(m.c2 + 1) - x, height: axes.rows.pos(m.r2 + 1) - y, right: left + axes.cols.pos(m.c2 + 1) - x, bottom: top + axes.rows.pos(m.r2 + 1) - y };
  }
  /** Scroll just enough to show a cell, clear of the frozen areas and the headers. */
  function reveal(row, col) {
    if (!g.axes) return;
    const { axes } = g, viewW = scroll.clientWidth - g.headerW, viewH = scroll.clientHeight - HEADER_H;
    if (col > g.fc) {
      const left = axes.cols.pos(col), right = axes.cols.pos(col + 1);
      if (left < scroll.scrollLeft + g.fw) scroll.scrollLeft = left - g.fw;
      else if (right > scroll.scrollLeft + viewW) scroll.scrollLeft = right - viewW;
    }
    if (row > g.fr) {
      const top = axes.rows.pos(row), bottom = axes.rows.pos(row + 1);
      if (top < scroll.scrollTop + g.fh) scroll.scrollTop = top - g.fh;
      else if (bottom > scroll.scrollTop + viewH) scroll.scrollTop = bottom - viewH;
    }
  }
  /** Rows the viewport holds, for Page Up and Page Down. */
  const pageRows = () => Math.max(1, Math.floor((scroll.clientHeight - HEADER_H) / M.DEFAULT_ROW) - 1);
  /** Width in pixels of the widest text in a column among the cells loaded, for double-click-to-fit. */
  function measure(text, bold) {
    measureCtx ||= document.createElement('canvas').getContext('2d');
    measureCtx.font = `${bold ? '600 ' : ''}13px ${getComputedStyle(scroll).fontFamily}`;
    return measureCtx.measureText(text).width;
  }

  return {
    root, scroll, layout, render, renderAll, paintSelection, paintBoxes, cellAt, edgeAt, screenRect, reveal, pageRows, measure, box,
    geometry: g, resizeGuide: boxes.resize,
    setPins(map) { pins = map; },
    setFlash(set) { flash = set; },
    focus() { scroll.focus({ preventScroll: true }); },
    view,
    /** The scroll offsets as they are now, for restoring a view. */
    get position() { return { left: scroll.scrollLeft, top: scroll.scrollTop }; },
  };
}
