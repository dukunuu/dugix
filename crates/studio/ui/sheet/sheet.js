// Sheet mode: a Google-Sheets-like grid in the studio window. The sheet the backend sends
// is the single source of truth; typing, formatting and sizes show at once and the backend
// confirms, everything else waits for its answer. Every user action is one `SheetApply`
// (one undo step). Values come in windows (`SheetValues`) as the grid scrolls.
import * as M from './model.js';
import { keyAction } from './keys.js';
import { gridController } from './grid.js';
import { editorController } from './editor.js';
import { menusController } from './menus.js';
import { clipboardController } from './clipboard.js';
import { toolbarController } from './toolbar.js';
import { tabsController } from './tabs.js';
import { commentsController } from './comments.js';
import { interactController } from './interact.js';
import { chartsController } from './charts.js';
import { glyph } from './glyphs.js';
import { placePopup } from '../popup.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const FIRST_WINDOW = M.parseRange('A1:AZ200');

/** Build the sheet UI inside `#sheet-app` and return the handle app.js talks to. */
export function mount(shared) {
  const app = document.querySelector('#sheet-app');
  app.replaceChildren();
  app.hidden = false;

  const state = {
    sheet: null, revision: '', relative: '', threads: [], activities: {}, linked: false, canUndo: false, canRedo: false,
    tabId: null, sel: null, sels: {}, extents: {}, scrolls: {}, values: new Map(), generation: 0, clip: null, remote: null, drafts: {}, autoRows: {}, charts: {},
  };
  const lifetime = new AbortController();

  // -- the shell -------------------------------------------------------------------------------------

  const iconButton = (name, label, className = '') => {
    const b = el('button', `icon-button quiet top-icon ${className}`.trim());
    b.type = 'button'; b.append(glyph(name)); b.setAttribute('aria-label', label); b.title = label;
    return b;
  };
  const top = el('header', 'sg-top');
  const crumbs = el('nav', 'sg-crumbs');
  crumbs.setAttribute('aria-label', 'Breadcrumb');
  const libraryLink = el('button', 'crumb-link', 'Library');
  libraryLink.type = 'button'; libraryLink.title = 'Library (⌘/Ctrl+O)';
  const titleInput = el('input', 'doc-title-input sg-title');
  titleInput.type = 'text'; titleInput.placeholder = 'Untitled'; titleInput.maxLength = 200; titleInput.spellcheck = false;
  titleInput.setAttribute('aria-label', 'Sheet title');
  const statusDot = el('span', 'status-dot');
  statusDot.setAttribute('role', 'img');
  crumbs.append(libraryLink, el('span', 'crumb-sep', '/'), titleInput, statusDot);
  const linkLabel = el('span', 'sg-link', 'Not linked');
  const commentsButton = iconButton('comment', 'Comments', 'with-count');
  const commentCount = el('span', 'count', '');
  commentsButton.append(commentCount);
  const exportButton = iconButton('export', 'Export');
  exportButton.setAttribute('aria-haspopup', 'menu');
  top.append(crumbs, el('span', 'spacer'), linkLabel, commentsButton, exportButton);

  const column = el('div', 'sg-column');
  const sidebar = el('aside', 'sg-sidebar');
  sidebar.setAttribute('aria-label', 'Comments'); sidebar.hidden = true;
  const sideHead = el('div', 'side-tabs');
  const sideTitle = el('span', 'side-tab', 'Comments');
  sideTitle.setAttribute('aria-selected', 'true');
  const sideCount = el('span', 'tab-count', '');
  sideTitle.append(sideCount);
  const closeSide = iconButton('x', 'Close sidebar', 'side-close');
  sideHead.append(sideTitle, el('span', 'spacer'), closeSide);
  const sidePane = el('div', 'side-pane');
  sidebar.append(sideHead, sidePane);
  const middle = el('div', 'sg-middle');
  const overlay = el('div', 'sg-overlay');
  const toastNode = el('div', 'sg-toast');
  toastNode.setAttribute('role', 'status'); toastNode.hidden = true;

  // -- the command queue ------------------------------------------------------------------------------

  const queue = [];
  let pending = null, toastTimer = 0, flashTimer = 0, remoteTimer = 0, valuesTimer = 0;
  const inflight = new Set();
  const post = cmd => window.ipc.postMessage(JSON.stringify(cmd));
  function pump() {
    if (pending || !queue.length) return;
    pending = queue.shift();
    try { post(pending.cmd); } catch (error) { toast(error.message); pending = null; pump(); }
    renderSave();
  }
  function command(cmd, complete) { queue.push({ cmd, complete }); pump(); }
  function renderSave() {
    const saving = [pending, ...queue].some(q => q?.cmd.command === 'SheetApply');
    statusDot.className = `status-dot${saving ? ' saving' : ''}`;
    const label = `${saving ? 'Saving…' : 'Saved'} · ${linkLabel.textContent}`;
    statusDot.title = label; statusDot.setAttribute('aria-label', label);
  }
  function toast(text, actions = []) {
    if (shared.library?.visible) { shared.library.toast(text, actions); return; }
    toastNode.replaceChildren(document.createTextNode(text));
    for (const [label, run] of actions) {
      const b = el('button', 'quiet', label);
      b.type = 'button';
      b.addEventListener('click', () => { toastNode.hidden = true; run(); });
      toastNode.append(b);
    }
    toastNode.hidden = !text;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { toastNode.hidden = true; }, actions.length ? 10000 : 5000);
  }

  // -- the api the parts share --------------------------------------------------------------------------

  const api = {
    state, signal: lifetime.signal, overlay, sidebarPane: sidePane, command, toast,
    tab: () => state.sheet?.tabs.find(t => t.id === state.tabId) || null,
    tabName: () => api.tab()?.name ?? '',
    /** Derived heights of wrapped rows for the tab showing; a user-set height wins. */
    autoRows: () => state.autoRows?.[state.tabId] || null,
    editing: () => api.editor.active,
    place: (node, rect) => placePopup(node, rect, { prefer: 'below', align: 'left' }),
    extent() {
      const tab = api.tab();
      let used = M.usedRect(tab);
      for (const chart of tab.charts || []) {
        const at = M.parseAddr(chart.at);
        const end = { r2: at.row + Math.ceil((chart.h + (chart.dy || 0)) / M.DEFAULT_ROW), c2: at.col + Math.ceil((chart.w + (chart.dx || 0)) / M.DEFAULT_COL) };
        used = { r1: used?.r1 || 1, c1: used?.c1 || 1, r2: Math.max(used?.r2 || 1, end.r2), c2: Math.max(used?.c2 || 1, end.c2) };
      }
      return (state.extents[tab.id] = M.extentOf(used, state.extents[tab.id], state.sel));
    },
    /** The raw input of a cell on the tab showing. */
    inputAt: (row, col) => api.tab()?.cells[M.addr(row, col)]?.input ?? '',
    valueAt: a1 => valuesFor(state.tabId).map.get(a1),
    /** What a cell shows: the calculated value, else a literal input as typed. */
    displayAt(row, col) {
      const a1 = M.addr(row, col), v = api.valueAt(a1);
      if (v) return v.display;
      const input = api.inputAt(row, col);
      return M.isFormula(input) ? '' : input;
    },
    valueKind: () => api.valueAt(M.addr(state.sel.active.row, state.sel.active.col))?.kind,
    formatAt: () => api.tab()?.cells[M.addr(state.sel.active.row, state.sel.active.col)]?.fmt || {},
    undo() { api.editor.commit(); command({ command: 'Undo' }); },
    redo() { api.editor.commit(); command({ command: 'Redo' }); },
    apply, select, switchTab, goTo, format, toggle, toggleMerge, resize, fit, contextMenu, openSidebar,
    scrolled() { api.editor.place(); clearTimeout(valuesTimer); valuesTimer = setTimeout(ensureValues, 80); },
    renderLink,
    closeComments: () => setSidebar(false),
    setCommentCount(n) {
      commentCount.textContent = sideCount.textContent = n ? String(n) : '';
      commentsButton.setAttribute('aria-label', n ? `Comments, ${n} open` : 'Comments');
      if (n && !sideChosen && !sidebarOpen && matchMedia('(min-width: 1100px)').matches) setSidebar(true, false);
    },
  };
  api.grid = gridController(api);
  api.editor = editorController(api);
  api.menus = menusController(api);
  api.clipboard = clipboardController(api);
  api.toolbar = toolbarController(api);
  api.tabs = tabsController(api);
  api.comments = commentsController(api);
  api.interact = interactController(api);
  api.charts = chartsController(api);

  middle.append(api.grid.root, sidebar, api.charts.panel);
  column.append(api.toolbar.bar, api.editor.bar, middle, api.tabs.bar);
  app.append(top, column, overlay, toastNode);

  // -- values: windows fetched as the grid scrolls ------------------------------------------------------------

  const valuesFor = id => state.values.get(id) || state.values.set(id, { map: new Map(), loaded: [] }).get(id);
  function store(tabId, r, values) {
    const cache = valuesFor(tabId);
    for (let row = r.r1; row <= r.r2; row++) for (let col = r.c1; col <= r.c2; col++) cache.map.delete(M.addr(row, col));
    for (const v of values) cache.map.set(v.a1, { display: v.display, kind: v.kind, error: v.error });
    cache.loaded.push(r);
    if (cache.loaded.length > 40) cache.loaded.shift();
  }
  function invalidate() { state.generation++; for (const cache of state.values.values()) cache.loaded = []; inflight.clear(); }
  /** Typed text shows as typed before the backend has calculated it. */
  function patchValues(ops) {
    for (const o of ops) {
      const tab = state.sheet.tabs.find(t => t.name.toLowerCase() === String(o.tab).toLowerCase());
      if (!tab) continue;
      const cache = valuesFor(tab.id);
      if (o.op === 'set_cells') {
        const at = M.parseAddr(o.at);
        o.values.forEach((line, r) => line.forEach((input, c) => {
          const key = M.addr(at.row + r, at.col + c), literal = M.literalValue(input);
          if (literal) cache.map.set(key, literal); else if (input === '') cache.map.delete(key);
        }));
      } else if (o.op === 'clear' && o.what !== 'formats') {
        const r = M.parseRange(o.range);
        for (const key of [...cache.map.keys()]) { const a = M.parseAddr(key); if (a && M.inRect(r, a.row, a.col)) cache.map.delete(key); }
      }
    }
  }
  function ensureValues() {
    valuesTimer = 0;
    const tab = api.tab();
    if (!tab || !api.grid.geometry.axes) return;
    const win = M.windowOf(api.grid.geometry.axes, api.grid.view(), { rows: 30, cols: 6 }), cache = valuesFor(tab.id), generation = state.generation;
    for (const r of M.fetchRanges(win)) {
      const range = M.rectStr(r), key = `${tab.id}|${range}`;
      if (M.covers(cache.loaded, r) || inflight.has(key)) continue;
      inflight.add(key);
      command({ command: 'SheetValues', tab: tab.id, range }, event => {
        inflight.delete(key);
        if (event.event === 'SheetValues' && generation === state.generation) { store(tab.id, r, event.values); api.grid.render(true); }
      });
    }
  }

  // -- applying edits ----------------------------------------------------------------------------------------------

  /** One user action: its ops are one `SheetApply`. Typing and formatting show at once. */
  function apply(ops) {
    if (!ops.length) return;
    const next = M.applyLocal(state.sheet, ops);
    queue.push({ cmd: { command: 'SheetApply', ops }, ops: next ? ops : null });
    if (next) { state.sheet = next; patchValues(ops); refresh(); }
    pump();
    renderSave();
  }
  const used = () => M.usedRect(api.tab());
  const opRange = () => M.rectStr(M.opRect(state.sel, used()));
  function format(patch) { if (state.sel) apply([M.op.format(api.tabName(), opRange(), patch)]); }
  function toggle(key) { if (state.sel) apply([M.toggleFormat(api.tabName(), opRange(), api.formatAt(), key)]); }
  function toggleMerge() {
    const o = M.mergeToggle(api.tabName(), M.opRect(state.sel, used()), M.mergesOf(api.tab()));
    if (o) apply([o]); else toast('Select more than one cell to merge them');
  }
  /** Resize: the selected columns or rows when the dragged one is among them, else just that one. */
  function resize(axis, index, size) {
    const r = M.selRect(state.sel), extent = api.extent();
    let from = index, to = index;
    if (axis === 'col' && ['cols', 'all'].includes(state.sel.kind) && index >= r.c1 && index <= r.c2) { from = r.c1; to = Math.min(r.c2, extent.cols); }
    if (axis === 'row' && ['rows', 'all'].includes(state.sel.kind) && index >= r.r1 && index <= r.r2) { from = r.r1; to = Math.min(r.r2, extent.rows); }
    apply([M.resizeOp(api.tabName(), axis, from, to, size)]);
  }
  /** Double-click on a column border: fit the column to every value it holds; a row goes
   *  back to the default height. A border of a selected column fits the whole selection. */
  function fit(axis, index) {
    if (axis === 'row') { resize('row', index, M.DEFAULT_ROW); return; }
    const r = M.selRect(state.sel);
    const last = state.sel.kind === 'all' ? (used()?.c2 || index) : Math.min(r.c2, api.extent().cols);
    const selected = ['cols', 'all'].includes(state.sel.kind) && index >= r.c1 && index <= last;
    const from = selected ? r.c1 : index, to = selected ? last : index;
    apply([M.op.fitCols(api.tabName(), `${M.colName(from)}:${M.colName(to)}`)]);
  }
  /** ⌘/Ctrl+Alt+W: overflow ↔ wrap on the selection. */
  function toggleWrap() {
    if (state.sel) apply([M.toggleWrap(api.tabName(), opRange(), api.formatAt())]);
  }

  // -- selection -------------------------------------------------------------------------------------------------------------

  let lastFocus = '', focusTimer = 0;
  function focusChanged() {
    if (!state.linked || !state.sel) return;
    const range = opRange(), key = `${state.tabId}|${range}`;
    if (key === lastFocus) return;
    lastFocus = key;
    clearTimeout(focusTimer);
    focusTimer = setTimeout(() => command({ command: 'SheetFocus', tab: api.tabName(), range }), 300);
  }
  function select(sel, { reveal = true } = {}) {
    api.charts.deselect();
    const before = state.extents[state.tabId];
    state.sel = state.sels[state.tabId] = sel;
    const after = api.extent();
    if (!before || before.rows !== after.rows || before.cols !== after.cols) api.grid.renderAll(); else api.grid.paintSelection();
    if (reveal) { const at = sel.kind === 'cells' ? sel.head : sel.active; api.grid.reveal(at.row, at.col); }
    api.toolbar.sync(); api.editor.sync();
    focusChanged();
  }
  function switchTab(id) {
    if (id === state.tabId || !state.sheet.tabs.some(t => t.id === id)) return;
    api.editor.commit();
    api.charts.deselect();
    state.scrolls[state.tabId] = api.grid.position;
    state.tabId = id;
    state.sel = state.sels[id] ||= M.newSel(1, 1);
    refresh();
    const at = state.scrolls[id] || { left: 0, top: 0 };
    api.grid.scroll.scrollLeft = at.left; api.grid.scroll.scrollTop = at.top;
    api.grid.render(true);
    ensureValues();
    focusChanged();
  }
  /** The address box: `B12`, `Data!A1`, `A1:C5`, `B:B`. */
  function goTo(text) {
    const ref = M.parseRef(text);
    const tab = ref && (ref.tab === null ? api.tab() : state.sheet.tabs.find(t => t.name.toLowerCase() === ref.tab.toLowerCase()));
    if (!ref || !tab) { toast(`“${text.trim()}” is not a cell or range`); api.editor.sync(); api.grid.focus(); return; }
    if (tab.id !== state.tabId) switchTab(tab.id);
    const r = ref.rect, whole = r.r1 === 1 && r.r2 === M.MAX_ROWS, rows = r.c1 === 1 && r.c2 === M.MAX_COLS;
    const sel = whole && !rows ? { kind: 'cols', anchor: { row: 1, col: r.c1 }, head: { row: 1, col: r.c2 }, active: { row: state.sel.active.row, col: r.c1 } }
      : rows && !whole ? { kind: 'rows', anchor: { row: r.r1, col: 1 }, head: { row: r.r2, col: 1 }, active: { row: r.r1, col: state.sel.active.col } }
        : { kind: 'cells', anchor: { row: r.r1, col: r.c1 }, head: { row: r.r2, col: r.c2 }, active: { row: r.r1, col: r.c1 } };
    reach(Math.min(r.r2, M.MAX_ROWS), Math.min(r.c2, M.MAX_COLS));
    select(sel);
    api.grid.reveal(r.r1, r.c1);
    api.grid.focus();
  }
  /** Make sure the grid is big enough to hold a cell (a range from the TUI may reach past the data). */
  function reach(row, col) {
    const prev = api.extent();
    state.extents[state.tabId] = { rows: Math.max(prev.rows, Math.min(M.MAX_ROWS, row + 25)), cols: Math.max(prev.cols, Math.min(M.MAX_COLS, col + 3)) };
    api.grid.renderAll();
  }

  // -- menus ------------------------------------------------------------------------------------------------------------------

  function contextMenu(kind, which, at) {
    const r = M.selRect(state.sel), name = api.tabName(), tab = api.tab();
    const rows = Math.min(r.r2, api.extent().rows) - r.r1 + 1, cols = Math.min(r.c2, api.extent().cols) - r.c1 + 1;
    const plural = (n, word) => `${n} ${word}${n === 1 ? '' : 's'}`;
    const insertRows = [
      { label: `Insert ${plural(rows, 'row')} above`, run: () => apply([M.op.insertRows(name, M.insertAt(r, 'row', false), rows)]) },
      { label: `Insert ${plural(rows, 'row')} below`, run: () => apply([M.op.insertRows(name, M.insertAt(r, 'row', true), rows)]) },
    ];
    const insertCols = [
      { label: `Insert ${plural(cols, 'column')} left`, run: () => apply([M.op.insertCols(name, M.insertAt(r, 'col', false), cols)]) },
      { label: `Insert ${plural(cols, 'column')} right`, run: () => apply([M.op.insertCols(name, M.insertAt(r, 'col', true), cols)]) },
    ];
    const deleteRows = { label: `Delete ${plural(rows, 'row')}`, danger: true, run: () => apply([M.op.deleteRows(name, r.r1, rows)]) };
    const deleteCols = { label: `Delete ${plural(cols, 'column')}`, danger: true, run: () => apply([M.op.deleteCols(name, r.c1, cols)]) };
    const comment = { label: 'Comment', hint: '⌥⌘M', run: () => api.comments.begin() };
    let items;
    if (kind === 'col') {
      // "Fit to data" fits the selected columns when the clicked one is among them, all
      // used columns when everything is selected, else just this column.
      const inSelection = ['cols', 'all'].includes(state.sel.kind) && which >= r.c1 && which <= r.c2;
      const fitFrom = !inSelection ? which : state.sel.kind === 'all' ? 1 : r.c1;
      const fitTo = !inSelection ? which : state.sel.kind === 'all' ? (used()?.c2 || which) : Math.min(r.c2, api.extent().cols);
      items = [
        ...insertCols, deleteCols, { rule: true },
        { label: 'Fit to data', run: () => apply([M.op.fitCols(name, `${M.colName(fitFrom)}:${M.colName(fitTo)}`)]) },
        { rule: true },
        { label: 'Sort sheet A → Z', run: () => apply(M.sortOps(name, used(), which, false, tab.frozen?.rows || 0)) },
        { label: 'Sort sheet Z → A', run: () => apply(M.sortOps(name, used(), which, true, tab.frozen?.rows || 0)) },
        ...((tab.frozen?.rows || 0) >= 1 ? [{ group: 'Row 1 is frozen and counts as the header' }] : []),
        { rule: true },
        { label: `Freeze up to column ${M.colName(which)}`, run: () => apply([M.op.freeze(name, tab.frozen?.rows || 0, which)]) },
        { label: 'Unfreeze', disabled: !tab.frozen?.rows && !tab.frozen?.cols, run: () => apply([M.op.freeze(name, 0, 0)]) },
        { rule: true }, comment,
      ];
    } else if (kind === 'row') {
      items = [
        ...insertRows, deleteRows, { rule: true },
        { label: `Freeze up to row ${which}`, run: () => apply([M.op.freeze(name, which, tab.frozen?.cols || 0)]) },
        { label: 'Unfreeze', disabled: !tab.frozen?.rows && !tab.frozen?.cols, run: () => apply([M.op.freeze(name, 0, 0)]) },
        { rule: true }, comment,
      ];
    } else {
      items = [comment, { label: 'Insert chart', run: () => api.charts.insert() }, { rule: true }, ...insertRows, ...insertCols, { rule: true }, deleteRows, deleteCols, { rule: true }, { label: 'Clear contents', hint: 'Delete', run: () => apply([M.op.clear(name, opRange())]) }];
    }
    api.menus.show(items, at, { label: kind === 'cell' ? 'Cell' : kind === 'col' ? 'Column' : 'Row' });
  }
  const quickExport = () => { try { return localStorage.getItem('nexus-quick-export') === '1'; } catch { return false; } };
  exportButton.addEventListener('click', () => {
    const run = format => () => command(quickExport()
      ? { command: 'SheetExport', format, path: null, tab: format === 'csv' ? api.tabName() : null }
      : { command: 'ExportAs', format });
    api.menus.show([
      { label: 'Excel workbook', hint: '.xlsx', run: run('xlsx') },
      { label: 'CSV (this sheet)', hint: '.csv', run: run('csv') },
      { rule: true },
      { label: 'Save to exports/ without asking', checked: quickExport(), run: () => { try { localStorage.setItem('nexus-quick-export', quickExport() ? '0' : '1'); } catch { /* storage may be unavailable */ } } },
    ], exportButton.getBoundingClientRect(), { label: 'Export' });
  });

  // -- the sidebar and the chrome ---------------------------------------------------------------------------------------------

  let sidebarOpen = false, sideChosen = false;
  try { const stored = localStorage.getItem('nexus-sheet-sidebar'); sideChosen = stored !== null; sidebarOpen = stored === 'open'; } catch { /* storage may be unavailable */ }
  function setSidebar(open, chosen = true) {
    sidebarOpen = open; sideChosen = sideChosen || chosen;
    if (chosen) { try { localStorage.setItem('nexus-sheet-sidebar', open ? 'open' : 'closed'); } catch { /* ignore */ } }
    if (open) api.charts.close();
    sidebar.hidden = !open;
    commentsButton.setAttribute('aria-pressed', String(open));
    app.classList.toggle('side-open', open);
  }
  function openSidebar() { setSidebar(true); }
  commentsButton.addEventListener('click', () => setSidebar(!sidebarOpen));
  closeSide.addEventListener('click', () => setSidebar(false));
  libraryLink.addEventListener('click', () => shared.library.show());
  titleInput.addEventListener('keydown', e => {
    if (e.key === 'Enter') { e.preventDefault(); titleInput.blur(); api.grid.focus(); }
    else if (e.key === 'Escape') { titleInput.value = state.sheet.title; titleInput.blur(); }
  });
  titleInput.addEventListener('input', () => { titleInput.style.width = `${Math.min(48, Math.max(8, titleInput.value.length + 1))}ch`; });
  titleInput.addEventListener('change', () => {
    const title = titleInput.value.trim() || 'Untitled';
    if (title !== state.sheet.title) apply([M.op.setTitle(title)]);
  });
  function renderLink() {
    const activities = Object.values(state.activities);
    const working = activities.some(a => ['working', 'progress'].includes(a.state)), waiting = activities.filter(a => a.state === 'queued').length;
    linkLabel.textContent = !state.linked ? 'Not linked' : working ? 'nexus is working' : waiting ? `Linked · ${waiting} waiting` : 'Linked to nexus';
    linkLabel.classList.toggle('connected', state.linked);
    linkLabel.classList.toggle('working', state.linked && working);
    linkLabel.title = state.linked ? 'Comments go to your nexus conversation' : 'Not linked to a conversation — open it from nexus with /studio';
    renderSave();
  }
  /** Every part of the page, from the state. */
  function refresh() {
    if (!state.sheet) return;
    applySheetTheme();
    api.grid.setPins(api.comments.pins());
    api.grid.renderAll();
    api.tabs.render(); api.toolbar.sync(); api.editor.sync();
    if (document.activeElement !== titleInput) titleInput.value = state.sheet.title;
    titleInput.style.width = `${Math.min(48, Math.max(8, titleInput.value.length + 1))}ch`;
    document.title = `${state.sheet.title || 'Untitled'} — nexus studio`;
    renderLink();
  }
  function applySheetTheme() {
    const set = (name, value) => app.style.setProperty(name, value);
    const mix = (a, b, pct) => `color-mix(in srgb, ${a} ${pct}%, ${b})`;
    const bg = M.cssColor(state.sheet.theme, 'bg') || '#ffffff', fg = M.cssColor(state.sheet.theme, 'fg') || '#1d1d1f';
    set('--s-bg', bg); set('--s-fg', fg);
    set('--s-muted', M.cssColor(state.sheet.theme, 'muted') || mix(fg, bg, 55));
    set('--s-line', M.cssColor(state.sheet.theme, 'border') || mix(fg, bg, 16));
    set('--s-head', M.cssColor(state.sheet.theme, 'surface') || mix(fg, bg, 6));
  }

  // -- events from the backend -------------------------------------------------------------------------------------------------------

  function handle(event, reply) {
    const finished = reply ? pending : null;
    if (reply) pending = null;
    switch (event.event) {
      case 'Sheet': onSheet(event); break;
      case 'SheetValues': break; // stored by the request that asked for it
      case 'SheetFeedback':
        state.threads = event.threads;
        api.comments.updateActivities(event.activities);
        api.comments.render(); api.grid.setPins(api.comments.pins()); api.grid.render(true);
        if (event.notes?.length) toast(event.notes.join('; '));
        break;
      case 'SheetFocus': onRemoteFocus(event); break;
      case 'Library': case 'Idle': shared.library.onEvent(event); break;
      case 'Saved': {
        const name = event.path.split(/[\\/]/).pop();
        toast(`Saved a copy: ${name}`, [['Reveal', () => command({ command: 'Reveal', path: event.path })]]);
        break;
      }
      case 'Theme': shared.applyTheme(event.colors); break;
      case 'Exported': {
        const actions = [['Reveal', () => command({ command: 'Reveal', path: event.path })]];
        if (event.warnings?.length) actions.push([`${event.warnings.length} approximations`, () => toast(event.warnings.join(' · '))]);
        const omitted = /\.xlsx$/i.test(event.path) && state.sheet.tabs.some(t => t.charts?.length);
        toast(`Exported ${event.path.split(/[\\/]/).pop()}${omitted ? " · charts aren't included in XLSX exports" : ''}`, actions);
        break;
      }
      case 'Error':
        shared.library.onEvent(event);
        toast(event.message);
        api.charts.rejected();
        // A rejected edit may have been shown already: go back to what the backend has.
        if (reply && finished?.cmd?.command === 'SheetApply') { queue.length = 0; command({ command: 'Load' }); }
        break;
      default: break;
    }
    finished?.complete?.(event);
    pump();
    renderSave();
  }

  function onSheet(event) {
    if (event.colors) shared.applyTheme(event.colors);
    const first = !state.sheet, conflict = event.notice && /not applied/.test(event.notice), previous = state.sheet?.tabs.map(t => t.id) ?? [];
    if (event.revision !== state.revision || first) invalidate();
    state.revision = event.revision;
    state.sheet = event.sheet;
    state.autoRows = event.auto_rows || {};
    state.charts = event.charts || {};
    const firstTab = state.sheet.tabs[0].id;
    store(firstTab, FIRST_WINDOW, event.values);
    if (conflict) queue.length = 0;
    else {
      // Edits still on their way (queued) stay visible.
      for (const item of queue) if (item.ops) { const next = M.applyLocal(state.sheet, item.ops); if (next) { state.sheet = next; patchValues(item.ops); } }
    }
    Object.assign(state, { relative: event.relative, threads: event.threads, linked: event.linked, canUndo: event.can_undo, canRedo: event.can_redo });
    if (!state.sheet.tabs.some(t => t.id === state.tabId)) {
      const was = previous.indexOf(state.tabId);
      state.tabId = first || was < 0 ? firstTab : state.sheet.tabs[Math.min(was, state.sheet.tabs.length - 1)].id;
    }
    const added = state.addedTab && state.sheet.tabs.find(t => t.name === state.addedTab);
    if (added) { state.addedTab = null; state.tabId = added.id; }
    state.sel = state.sels[state.tabId] ||= M.newSel(1, 1);
    api.comments.updateActivities(event.activities);
    refresh();
    api.comments.render();
    api.charts.received();
    if (event.notice) toast(event.notice, /reloaded/.test(event.notice) ? [['Undo', api.undo]] : []);
    if (event.changed?.length) flash(event.changed);
    ensureValues();
    shared.library.onDocument(event);
    if (first && !shared.library.visible) api.grid.focus();
  }
  /** Cells that changed behind the page's back glow for a moment; the selection and scroll stay. */
  function flash(changed) {
    const name = api.tabName().toLowerCase(), set = new Set();
    for (const text of changed) { const p = M.splitChanged(text); if (p && p.tab.toLowerCase() === name) set.add(p.a1); }
    api.grid.setFlash(set);
    api.grid.render(true);
    clearTimeout(flashTimer);
    flashTimer = setTimeout(() => { api.grid.setFlash(new Set()); api.grid.render(true); }, 1800);
  }
  /** nexus looked at a range: show it, and outline it for a few seconds. */
  function onRemoteFocus(event) {
    const tab = state.sheet?.tabs.find(t => t.name.toLowerCase() === event.tab.toLowerCase() || t.id === event.tab), r = M.parseRange(event.range);
    if (!tab || !r) return;
    if (tab.id !== state.tabId) switchTab(tab.id);
    reach(Math.min(r.r2, M.MAX_ROWS), Math.min(r.c2, M.MAX_COLS));
    state.remote = { tabId: tab.id, rect: r, until: Date.now() + 3000 };
    api.grid.reveal(r.r1, r.c1);
    api.grid.paintBoxes();
    clearTimeout(remoteTimer);
    remoteTimer = setTimeout(() => api.grid.paintBoxes(), 3050);
  }

  // -- keys ---------------------------------------------------------------------------------------------------------------------------

  function fillSelection(down) {
    const r = M.opRect(state.sel, used()), name = api.tabName();
    let from, to;
    if (down) {
      if (r.r2 > r.r1) { from = { ...r, r2: r.r1 }; to = r; } else if (r.r1 > 1) { from = { ...r, r1: r.r1 - 1, r2: r.r1 - 1 }; to = { ...r, r1: r.r1 - 1 }; } else return;
    } else if (r.c2 > r.c1) { from = { ...r, c2: r.c1 }; to = r; } else if (r.c1 > 1) { from = { ...r, c1: r.c1 - 1, c2: r.c1 - 1 }; to = { ...r, c1: r.c1 - 1 }; } else return;
    apply([M.op.fill(name, M.rectStr(from), M.rectStr(to))]);
  }
  function run(act) {
    const data = () => M.dataOf(api.tab(), api.extent());
    switch (act.do) {
      case 'move': select(M.moveSel(state.sel, act.dr, act.dc, act, data())); break;
      case 'step': select(M.stepWithin(state.sel, act.dir, act.back)); break;
      case 'page': select(M.moveSel(state.sel, act.dir * api.grid.pageRows(), 0, { extend: act.extend }, data())); break;
      case 'selectAll': select(M.selectAll(state.sel)); break;
      case 'selectColumn': select(M.selectCol(state.sel, state.sel.active.col, false)); break;
      case 'selectRow': select(M.selectRow(state.sel, state.sel.active.row, false)); break;
      case 'home': {
        const to = act.row ? { row: state.sel.active.row, col: 1 } : { row: 1, col: 1 };
        select(act.shift ? M.extendTo(state.sel, to.row, to.col) : M.newSel(to.row, to.col));
        api.grid.scroll.scrollLeft = 0;
        if (!act.row) api.grid.scroll.scrollTop = 0;
        break;
      }
      case 'end': {
        const u = used() || M.cellRect(1, 1);
        select(act.shift ? M.extendTo(state.sel, u.r2, u.c2) : M.newSel(u.r2, u.c2));
        break;
      }
      case 'edit': api.editor.begin(act.ime ? { typed: '', mode: 'enter' } : { mode: 'edit' }); break;
      case 'type': api.editor.begin({ typed: act.text, mode: 'enter' }); break;
      case 'clear': if (used()) apply([M.op.clear(api.tabName(), opRange())]); break;
      case 'undo': api.undo(); break;
      case 'redo': api.redo(); break;
      case 'format': toggle(act.key); break;
      case 'wrap': toggleWrap(); break;
      case 'comment': api.comments.begin(); break;
      case 'fillDown': fillSelection(true); break;
      case 'fillRight': fillSelection(false); break;
      case 'escape': api.clipboard.clear(); api.comments.closeComposer(); break;
      default: break;
    }
  }
  document.addEventListener('keydown', e => {
    if (document.querySelector('dialog[open]') || !document.querySelector('#library')?.hidden) return;
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 's') { e.preventDefault(); toast('Every change is saved to the sheet file as you make it.'); return; }
    if (api.charts.key(e)) return;
    if (!state.sel || (e.target !== api.grid.scroll && e.target !== document.body)) return;
    const act = keyAction(e, {});
    if (!act) return;
    if (!act.ime) e.preventDefault();
    run(act);
  }, { signal: lifetime.signal });
  window.addEventListener('resize', () => { api.grid.renderAll(); api.editor.place(); }, { signal: lifetime.signal });

  setSidebar(sidebarOpen, false);

  return {
    handle,
    unmount() {
      api.charts.destroy();
      lifetime.abort();
      for (const t of [toastTimer, flashTimer, remoteTimer, valuesTimer, focusTimer]) clearTimeout(t);
      app.hidden = true;
      app.replaceChildren();
      queue.length = 0;
    },
    api,
  };
}
