// Editing: the in-cell editor and the formula bar are two views of one edit. Typing
// starts it ("enter" mode: arrows commit and move); Enter, F2 or a double-click starts
// it "in place" (arrows move the caret). In a formula, arrows and clicks insert or
// extend a cell reference at the caret (point mode), as in Google Sheets.
import * as M from './model.js';
import { keyAction } from './keys.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

export function editorController(api) {
  const { state } = api;
  let edit = null;

  // -- the formula bar ----------------------------------------------------------------------------------

  const bar = el('div', 'sg-formula');
  const address = el('input', 'sg-addr');
  address.type = 'text'; address.spellcheck = false; address.autocomplete = 'off';
  address.setAttribute('aria-label', 'Cell address');
  const fx = el('span', 'sg-fx', 'fx');
  fx.setAttribute('aria-hidden', 'true');
  const text = el('textarea', 'sg-bar-text');
  text.rows = 1; text.spellcheck = false; text.wrap = 'off';
  text.setAttribute('aria-label', 'Formula bar');
  bar.append(address, fx, text);

  // -- the in-cell editor ---------------------------------------------------------------------------------

  const cell = el('textarea', 'sg-cell-editor');
  cell.rows = 1; cell.spellcheck = false; cell.wrap = 'off'; cell.hidden = true;
  cell.setAttribute('aria-label', 'Cell editor');
  api.grid.root.append(cell);
  const field = () => (edit?.source === 'bar' ? text : cell);

  function place() {
    if (!edit) return;
    const r = api.grid.screenRect(edit.row, edit.col), host = api.grid.root.getBoundingClientRect();
    const z = api.grid.geometry.z, lines = cell.value.split('\n'), widest = Math.max(...lines.map(l => api.grid.measure(l, false))) * z;
    const left = r.left - host.left, top = r.top - host.top;
    const width = Math.min(Math.max(r.width, widest + 18), Math.max(r.width, host.width - left - 12));
    Object.assign(cell.style, { left: `${left}px`, top: `${top}px`, width: `${width}px`, height: `${Math.max(r.height, lines.length * 17 * z + 7)}px` });
  }
  const mirror = from => { const other = from === cell ? text : cell; if (other.value !== from.value) other.value = from.value; };
  cell.addEventListener('input', () => { mirror(cell); if (edit) edit.point = null; hidePoint(); place(); });
  text.addEventListener('input', () => { mirror(text); if (edit) edit.point = null; hidePoint(); place(); });

  const hidePoint = () => api.grid.box('point', null);
  /** Start editing the active cell. `typed` replaces its contents; otherwise the caret goes to the end. */
  function begin({ typed = null, mode = 'edit', source = 'cell' } = {}) {
    if (edit) return;
    const { row, col } = state.sel.active;
    const original = api.inputAt(row, col);
    edit = { tabId: state.tabId, tabName: api.tabName(), row, col, original, mode, source, point: null };
    const value = typed ?? original;
    cell.value = text.value = value;
    cell.hidden = false;
    api.grid.reveal(row, col);
    place();
    const f = field();
    f.focus({ preventScroll: true });
    f.setSelectionRange(value.length, value.length);
    api.grid.paintSelection();
  }
  /** Finish: send the change (one undo step), then move the selection. */
  function commit({ step = null, move = null } = {}) {
    if (!edit) return;
    const e = edit, value = field().value;
    edit = null;
    cell.hidden = true; hidePoint();
    if (value !== e.original) api.apply([M.setCell(e.tabName, e.row, e.col, value)]);
    if (state.tabId === e.tabId) {
      const data = M.dataOf(api.tab(), api.extent());
      if (step) api.select(M.stepWithin(state.sel, step.dir, step.back));
      else if (move) api.select(M.moveSel(state.sel, move.dr, move.dc, {}, data));
    }
    api.grid.focus();
    sync();
  }
  function cancel() {
    if (!edit) return;
    edit = null;
    cell.hidden = true; hidePoint();
    api.grid.focus(); api.grid.paintSelection();
    sync();
  }

  // -- point mode ---------------------------------------------------------------------------------------------------

  const canPointNow = () => Boolean(edit) && (Boolean(edit.point) || M.canPoint(field().value, field().selectionStart));
  function writeRef() {
    const p = edit.point, r = M.rect(p.anchor, p.head), f = field();
    const res = M.pointInsert(f.value, f.selectionStart, p.span, M.refText(r, edit.tabName, edit.tabName));
    f.value = res.text; mirror(f);
    f.setSelectionRange(res.caret, res.caret);
    p.span = res.span;
    api.grid.box('point', r); api.grid.reveal(p.head.row, p.head.col);
    place();
  }
  /** Arrow keys: move the pointed cell (starting beside the one being edited), or with shift grow the range. */
  function point(dr, dc, extend) {
    if (!edit.point) edit.point = { anchor: { row: edit.row, col: edit.col }, head: { row: edit.row, col: edit.col }, span: null };
    const p = edit.point, head = { row: Math.max(1, Math.min(M.MAX_ROWS, p.head.row + dr)), col: Math.max(1, Math.min(M.MAX_COLS, p.head.col + dc)) };
    p.head = head;
    if (!extend) p.anchor = head;
    writeRef();
  }
  /** A click or drag on a cell while a reference may go at the caret. */
  function pointCell(row, col, extend) {
    if (!edit.point || !extend) edit.point = { anchor: { row, col }, head: { row, col }, span: edit.point?.span ?? null };
    else edit.point.head = { row, col };
    writeRef();
  }

  // -- keys ------------------------------------------------------------------------------------------------------------

  function onKey(e) {
    if (!edit || e.isComposing) return;
    const f = field();
    const act = keyAction(e, { editing: true, mode: edit.mode, pointable: M.canPoint(f.value, f.selectionStart), pointing: Boolean(edit.point) });
    if (!act) return;
    e.preventDefault();
    if (act.do === 'commit') commit(act);
    else if (act.do === 'cancel') cancel();
    else if (act.do === 'toggleMode') edit.mode = edit.mode === 'enter' ? 'edit' : 'enter';
    else if (act.do === 'newline') { f.setRangeText('\n', f.selectionStart, f.selectionEnd, 'end'); f.dispatchEvent(new Event('input')); }
    else if (act.do === 'point') point(act.dr, act.dc, act.extend);
  }
  cell.addEventListener('keydown', onKey);
  text.addEventListener('keydown', onKey);
  // Focusing the bar edits the active cell; leaving a field with the edit open commits it.
  text.addEventListener('focus', () => { if (!edit && state.sel) begin({ source: 'bar' }); });
  // Switching to another app blurs the field too; the edit stays open until the window is back.
  const leave = source => () => { if (edit?.source === source && document.hasFocus()) commit(); };
  cell.addEventListener('blur', leave('cell'));
  text.addEventListener('blur', leave('bar'));

  // -- the address box -----------------------------------------------------------------------------------------------------

  address.addEventListener('keydown', e => {
    if (e.key === 'Enter') { e.preventDefault(); api.goTo(address.value); }
    else if (e.key === 'Escape') { e.preventDefault(); sync(); api.grid.focus(); }
  });
  address.addEventListener('focus', () => address.select());
  address.addEventListener('blur', () => { if (!edit) sync(); });

  /** Show the selection's address and the active cell's input in the bar. */
  function sync() {
    if (!state.sel || !state.sheet) return;
    const r = M.opRect(state.sel, M.usedRect(api.tab()));
    if (document.activeElement !== address) address.value = M.rectStr(r);
    if (!edit) text.value = api.inputAt(state.sel.active.row, state.sel.active.col);
  }

  return {
    bar, cell, begin, commit, cancel, place, sync, canPointNow, pointCell, point,
    get active() { return Boolean(edit); },
  };
}
