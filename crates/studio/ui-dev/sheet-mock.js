// Dev-only IPC fixture for sheet mode. Injected by sheet-e2e.mjs, never embedded.
// It plays the backend's part: the sheet is the single source of truth, every SheetApply
// is one undo step, SheetValues answers calculated windows, and the model can edit from
// outside. It applies a subset of the op language to plain JSON and calculates a few
// formulas (+ - * /, cell references, SUM) so the grid has something to show.
(() => {
  const fixture = structuredClone(window.__sheetFixture);
  let sheet = fixture.sheet, threads = fixture.threads || [], sequence = 1;
  const undo = [], redo = [], commands = [], sent = [], activities = {};
  const colors = window.__sheetColors;
  const emit = (event, async_ = false) => (async_ ? window.studioAsyncEvent : window.studioEvent)(structuredClone(event));
  let M;
  const model = async () => (M ||= await import('/sheet/model.js'));

  const tabOf = (name, source = sheet) => {
    const tab = source.tabs.find(t => t.name.toLowerCase() === String(name).toLowerCase() || t.id === name);
    if (!tab) throw new Error(`no tab ${JSON.stringify(name)}`);
    return tab;
  };
  const revision = () => `r${sequence}`;

  // -- calculation ----------------------------------------------------------------------------------------

  const NUMFMT = {
    '0.00%': n => `${(n * 100).toFixed(2)}%`,
    '#,##0.00': n => n.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 }),
    '$#,##0.00': n => `$${n.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`,
    'yyyy-mm-dd': n => new Date(Date.UTC(1899, 11, 30) + n * 864e5).toISOString().slice(0, 10),
  };
  /** + - * / and parentheses over numbers: all the mock's formulas reduce to this. */
  function calc(source) {
    const s = source.replace(/\s+/g, '');
    let i = 0;
    const atom = () => {
      if (s[i] === '(') { i++; const v = expr(); i++; return v; }
      if (s[i] === '-') { i++; return -atom(); }
      if (s[i] === '+') { i++; return atom(); }
      const m = /^\d*\.?\d+(?:[eE][+-]?\d+)?/.exec(s.slice(i));
      if (!m) throw new Error('syntax');
      i += m[0].length;
      return Number(m[0]);
    };
    const term = () => { let v = atom(); while (s[i] === '*' || s[i] === '/') { const op = s[i++], r = atom(); v = op === '*' ? v * r : v / r; } return v; };
    const expr = () => { let v = term(); while (s[i] === '+' || s[i] === '-') { const op = s[i++], r = term(); v = op === '+' ? v + r : v - r; } return v; };
    const v = expr();
    if (i !== s.length) throw new Error('syntax');
    return v;
  }
  function evaluate(tab, input, depth = 0) {
    if (depth > 20) return { error: '#REF!' };
    const refValue = (t, a1) => {
      const cell = t.cells[a1];
      if (!cell || cell.input === undefined || cell.input === '') return 0;
      const v = valueOf(t, a1, depth + 1);
      return v.kind === 'number' ? v.n : v.kind === 'error' ? Object.assign(new Error(v.error), { code: v.error }) : 0;
    };
    try {
      let text = input.slice(1);
      const refPattern = "(?:('[^']+'|[A-Za-z0-9_]+)!)?\\$?([A-Z]{1,3})\\$?(\\d+)";
      text = text.replace(new RegExp(`SUM\\(${refPattern}:\\$?([A-Z]{1,3})\\$?(\\d+)\\)`, 'g'), (_, name, c1, r1, c2, r2) => {
        const t = name ? tabOf(name.replaceAll("'", '')) : tab;
        const a = M.parseAddr(`${c1}${r1}`), b = M.parseAddr(`${c2}${r2}`);
        let sum = 0;
        for (let r = a.row; r <= b.row; r++) for (let c = a.col; c <= b.col; c++) { const v = refValue(t, M.addr(r, c)); if (v instanceof Error) throw v; sum += v; }
        return String(sum);
      });
      text = text.replace(new RegExp(refPattern, 'g'), (_, name, c, r) => {
        const v = refValue(name ? tabOf(name.replaceAll("'", '')) : tab, `${c}${r}`);
        if (v instanceof Error) throw v;
        return `(${v})`;
      });
      if (!/^[0-9+\-*/().\s eE]*$/.test(text)) return { error: '#NAME?' };
      const n = calc(text || '0');
      if (!Number.isFinite(n)) return { error: '#DIV/0!' };
      return { n };
    } catch (error) { return { error: error.code || '#ERROR!' }; }
  }
  function valueOf(tab, a1, depth = 0) {
    const cell = tab.cells[a1];
    const input = cell?.input ?? '';
    if (input === '') return { kind: 'empty', display: '' };
    const fmt = cell?.fmt?.num;
    const shown = n => (fmt && NUMFMT[fmt] ? NUMFMT[fmt](n) : String(Math.round(n * 1e9) / 1e9));
    if (fmt === '@') return { kind: 'text', display: input };
    if (input.startsWith('=')) {
      const r = evaluate(tab, input, depth);
      return r.error ? { kind: 'error', display: r.error, error: r.error } : { kind: 'number', n: r.n, display: shown(r.n) };
    }
    if (input.trim() !== '' && Number.isFinite(Number(input))) return { kind: 'number', n: Number(input), display: shown(Number(input)) };
    if (/^(true|false)$/i.test(input)) return { kind: 'bool', display: input.toUpperCase() };
    return { kind: 'text', display: input };
  }
  function values(tabName, range) {
    const tab = tabOf(tabName), r = M.parseRange(range), out = [];
    // Only the stored cells can be non-empty: walk those, not the (possibly huge) rectangle.
    for (const a1 of Object.keys(tab.cells)) {
      const a = M.parseAddr(a1);
      if (!a || !M.inRect(r, a.row, a.col)) continue;
      const v = valueOf(tab, a1);
      if (v.kind === 'empty') continue;
      out.push({ a1, display: v.display, kind: v.kind, error: v.error ?? null });
    }
    return out;
  }

  // -- derived geometry: the mock measures text the way the core does ------------------------------------------

  let measureCtx = null;
  function measureText(text, bold) {
    measureCtx ||= document.createElement('canvas').getContext('2d');
    measureCtx.font = `${bold ? '700 ' : '400 '}14px system-ui, -apple-system, "Segoe UI", sans-serif`;
    return measureCtx.measureText(text).width;
  }
  /** Greedy word wrap into `width`; overlong words break anywhere, as in the core. */
  function wrappedLines(text, width) {
    const space = measureText(' ', false);
    let lines = 0;
    for (const paragraph of String(text).split('\n')) {
      let count = 1, used = 0;
      for (const word of paragraph.split(/\s+/).filter(Boolean)) {
        const w = measureText(word, false);
        if (used > 0 && used + space + w > width) { count++; used = 0; }
        if (w <= width) { used += (used > 0 ? space : 0) + w; continue; }
        for (const ch of word) { const cw = measureText(ch, false); if (used > 0 && used + cw > width) { count++; used = 0; } used += cw; }
      }
      lines += count;
    }
    return Math.max(1, lines);
  }
  /** The height each row needs for its wrapped cells, per tab id. */
  function autoRows() {
    const out = {};
    for (const tab of sheet.tabs) {
      const heights = {};
      for (const [a1, cell] of Object.entries(tab.cells)) {
        if (cell.fmt?.wrap !== 'wrap') continue;
        const a = M.parseAddr(a1);
        if (!a || tab.rows?.[a.row] !== undefined) continue;
        const display = valueOf(tab, a1).display;
        if (!display) continue;
        const width = Math.max(1, (tab.cols?.[M.colName(a.col)] ?? 100) - 10);
        const height = wrappedLines(display, width) * 17 + 4;
        if (height > 24) heights[a.row] = Math.max(heights[a.row] || 0, height);
      }
      out[tab.id] = heights;
    }
    return out;
  }

  // -- ops ------------------------------------------------------------------------------------------------------

  const shiftFormula = (input, dr, dc) => (input.startsWith('=')
    ? input.replace(/(?<![A-Za-z0-9_$!'])(\$?)([A-Z]{1,3})(\$?)(\d+)/g, (m, d1, c, d2, r) => `${d1}${d1 ? c : M.colName(M.colIndex(c) + dc)}${d2}${d2 ? r : Number(r) + dr}`)
    : input);
  const keyOf = (row, col) => M.addr(row, col);
  function remap(tab, fn) {
    const cells = {};
    for (const [key, cell] of Object.entries(tab.cells)) { const a = M.parseAddr(key); const to = fn(a); if (to) cells[keyOf(to.row, to.col)] = cell; }
    tab.cells = cells;
  }
  function step(next, o) {
    const tab = o.tab === undefined ? null : tabOf(o.tab, next);
    switch (o.op) {
      case 'set_cells': {
        const at = M.parseAddr(o.at);
        o.values.forEach((line, r) => line.forEach((input, c) => {
          const key = keyOf(at.row + r, at.col + c);
          if (input === '' && !tab.cells[key]) return;
          tab.cells[key] = { ...tab.cells[key], input };
          if (input === '') delete tab.cells[key].input;
        }));
        break;
      }
      case 'clear': {
        const r = M.parseRange(o.range);
        for (const [key, cell] of Object.entries(tab.cells)) {
          const a = M.parseAddr(key);
          if (!M.inRect(r, a.row, a.col)) continue;
          if (o.what !== 'formats') delete cell.input;
          if (o.what !== 'inputs') delete cell.fmt;
          if (!Object.keys(cell).length) delete tab.cells[key];
        }
        break;
      }
      case 'format': {
        const r = M.parseRange(o.range);
        for (let row = r.r1; row <= r.r2; row++) for (let col = r.c1; col <= r.c2; col++) {
          const key = keyOf(row, col), cell = tab.cells[key] || {};
          const fmt = M.mergeFormat(cell.fmt || {}, o.patch);
          if (Object.keys(fmt).length) cell.fmt = fmt; else delete cell.fmt;
          if (Object.keys(cell).length) tab.cells[key] = cell; else delete tab.cells[key];
        }
        break;
      }
      case 'insert_rows': remap(tab, a => ({ row: a.row >= o.at ? a.row + o.count : a.row, col: a.col })); break;
      case 'delete_rows': remap(tab, a => (a.row >= o.at && a.row < o.at + o.count ? null : { row: a.row >= o.at + o.count ? a.row - o.count : a.row, col: a.col })); break;
      case 'insert_cols': { const at = M.colIndex(o.at); remap(tab, a => ({ row: a.row, col: a.col >= at ? a.col + o.count : a.col })); break; }
      case 'delete_cols': { const at = M.colIndex(o.at); remap(tab, a => (a.col >= at && a.col < at + o.count ? null : { row: a.row, col: a.col >= at + o.count ? a.col - o.count : a.col })); break; }
      case 'copy': {
        const from = M.parseRange(o.from), to = M.parseAddr(o.to), target = o.to_tab ? tabOf(o.to_tab, next) : tab;
        const moved = [];
        for (let row = from.r1; row <= from.r2; row++) for (let col = from.c1; col <= from.c2; col++) moved.push([row, col, structuredClone(tab.cells[keyOf(row, col)])]);
        if (o.cut) for (const [row, col] of moved) delete tab.cells[keyOf(row, col)];
        for (const [row, col, cell] of moved) {
          const key = keyOf(to.row + row - from.r1, to.col + col - from.c1);
          if (!cell) { delete target.cells[key]; continue; }
          if (cell.input) cell.input = shiftFormula(cell.input, to.row - from.r1, to.col - from.c1);
          target.cells[key] = cell;
        }
        break;
      }
      case 'fill': {
        const from = M.parseRange(o.from), to = M.parseRange(o.to), down = to.r2 > from.r2;
        const seeds = [];
        const numbers = [];
        if (down && from.c1 === from.c2) for (let r = from.r1; r <= from.r2; r++) numbers.push(Number(tab.cells[keyOf(r, from.c1)]?.input));
        const series = numbers.length >= 2 && numbers.every(Number.isFinite) && numbers.every((n, i) => i < 2 || Math.abs(n - numbers[i - 1] - (numbers[1] - numbers[0])) < 1e-9);
        for (let row = from.r1; row <= to.r2; row++) for (let col = from.c1; col <= to.c2; col++) {
          if (M.inRect(from, row, col)) continue;
          const size = down ? from.r2 - from.r1 + 1 : from.c2 - from.c1 + 1;
          const sr = down ? from.r1 + ((row - from.r1) % size) : row, sc = down ? col : from.c1 + ((col - from.c1) % size);
          const src = tab.cells[keyOf(sr, sc)];
          if (!src) { delete tab.cells[keyOf(row, col)]; continue; }
          const cell = structuredClone(src);
          if (series) cell.input = String(numbers[0] + (numbers[1] - numbers[0]) * (row - from.r1));
          else if (cell.input) cell.input = shiftFormula(cell.input, row - sr, col - sc);
          seeds.push([keyOf(row, col), cell]);
        }
        for (const [key, cell] of seeds) tab.cells[key] = cell;
        break;
      }
      case 'sort': {
        const r = M.parseRange(o.range), by = M.colIndex(o.by), first = r.r1 + (o.header ? 1 : 0);
        const rows = [];
        for (let row = first; row <= r.r2; row++) rows.push(row);
        const keyed = rows.map(row => [row, valueOf(tab, keyOf(row, by))]);
        const rank = v => (v.kind === 'empty' ? 2 : v.kind === 'number' ? 0 : 1);
        keyed.sort(([, a], [, b]) => (rank(a) !== rank(b) ? rank(a) - rank(b) : a.kind === 'number' ? (o.desc ? b.n - a.n : a.n - b.n) : (o.desc ? -1 : 1) * a.display.localeCompare(b.display)));
        const snapshot = new Map();
        for (const row of rows) for (let col = r.c1; col <= r.c2; col++) snapshot.set(keyOf(row, col), tab.cells[keyOf(row, col)]);
        keyed.forEach(([from], i) => { for (let col = r.c1; col <= r.c2; col++) { const cell = snapshot.get(keyOf(from, col)); const key = keyOf(first + i, col); if (cell) tab.cells[key] = cell; else delete tab.cells[key]; } });
        break;
      }
      case 'set_col_width': { const r = M.parseRange(o.cols); tab.cols = tab.cols || {}; for (let c = r.c1; c <= r.c2; c++) tab.cols[M.colName(c)] = o.width; break; }
      case 'fit_cols': {
        const r = M.parseRange(o.cols);
        tab.cols = tab.cols || {};
        for (let c = r.c1; c <= r.c2; c++) {
          let widest = null;
          const merges = M.mergesOf(tab);
          for (const [a1, cell] of Object.entries(tab.cells)) {
            const a = M.parseAddr(a1);
            if (!a || a.col !== c || M.mergeAt(merges, a.row, a.col)) continue;
            const display = valueOf(tab, a1).display;
            if (!display) continue;
            const words = display.split(/\s+/).filter(Boolean);
            const w = cell.fmt?.wrap === 'wrap' ? Math.max(0, ...words.map(word => measureText(word, cell.fmt?.bold))) : measureText(display, cell.fmt?.bold);
            widest = widest === null ? w : Math.max(widest, w);
          }
          tab.cols[M.colName(c)] = widest === null ? 100 : Math.min(480, Math.max(40, widest + 10));
        }
        break;
      }
      case 'set_row_height': { const r = M.parseRange(o.rows); tab.rows = tab.rows || {}; for (let row = r.r1; row <= r.r2; row++) tab.rows[row] = o.height; break; }
      case 'freeze': tab.frozen = { rows: o.rows, cols: o.cols }; break;
      case 'merge': tab.merges = [...(tab.merges || []), o.range]; break;
      case 'unmerge': { const r = M.parseRange(o.range); tab.merges = (tab.merges || []).filter(m => !M.overlaps(M.parseRange(m), r)); break; }
      case 'add_tab': {
        const at = o.after ? next.tabs.indexOf(tabOf(o.after, next)) + 1 : next.tabs.length;
        next.tabs.splice(at, 0, { id: `t${++sequence}-${next.tabs.length}`, name: o.name, cells: {} });
        break;
      }
      case 'rename_tab': tab.name = o.name; break;
      case 'delete_tab': next.tabs.splice(next.tabs.indexOf(tab), 1); break;
      case 'move_tab': {
        next.tabs.splice(next.tabs.indexOf(tab), 1);
        next.tabs.splice(o.after ? next.tabs.indexOf(tabOf(o.after, next)) + 1 : next.tabs.length, 0, tab);
        break;
      }
      case 'set_title': next.title = o.title; break;
      default: throw new Error(`mock cannot apply ${o.op}`);
    }
  }
  function apply(ops) {
    const next = structuredClone(sheet);
    ops.forEach((o, i) => { try { step(next, o); } catch (error) { throw new Error(`ops[${i}]: ${error.message}`); } });
    return next;
  }
  const changedCells = (before, after) => {
    const out = [];
    for (const tab of after.tabs) {
      const old = before.tabs.find(t => t.id === tab.id);
      for (const key of new Set([...Object.keys(tab.cells), ...Object.keys(old?.cells || {})])) if (JSON.stringify(old?.cells[key]) !== JSON.stringify(tab.cells[key])) out.push(`${tab.name}!${key}`);
    }
    return out;
  };

  // -- events ------------------------------------------------------------------------------------------------------

  const current = (extra = {}) => ({
    event: 'Sheet', sheet, revision: revision(), relative: 'sheets/q3-budget.sheet.json', values: values(sheet.tabs[0].name, 'A1:AZ200'), auto_rows: autoRows(), threads,
    linked: window.__mock?.linked ?? true, activities, changed: [], notice: null, colors, can_undo: undo.length > 0, can_redo: redo.length > 0, home: false, ...extra,
  });
  const feedback = () => ({ event: 'SheetFeedback', threads, activities, notes: [] });
  const commit = next => { undo.push(sheet); redo.length = 0; sheet = next; sequence++; };

  window.__mock = {
    sent, commands, linked: true, latency: window.__sheetLatency ?? 10, rejectNext: false,
    get sheet() { return structuredClone(sheet); },
    get threads() { return structuredClone(threads); },
    get undoDepth() { return undo.length; },
    emitCurrent: extra => emit(current(extra), true),
    async externalEdit(ops) {
      await model();
      const before = sheet, next = apply(ops);
      undo.push(before); redo.length = 0; sheet = next; sequence++;
      emit(current({ changed: changedCells(before, next), notice: 'The sheet changed on disk and was reloaded. Undo takes the change back.' }), true);
    },
    remoteFocus(tab, range) { emit({ event: 'SheetFocus', tab, range }, true); },
    addThread(thread) { threads.push(thread); emit(feedback(), true); },
    valuesOf: (tab, range) => values(tab, range),
  };
  window.ipc = {
    postMessage(source) {
      const m = JSON.parse(source);
      commands.push(m);
      setTimeout(async () => {
        await model();
        try {
          if (m.command === 'SheetApply') {
            if (window.__mock.rejectNext) { window.__mock.rejectNext = false; return emit({ event: 'Error', message: 'Mock rejected apply' }); }
            const next = apply(m.ops);
            if (JSON.stringify(next) !== JSON.stringify(sheet)) commit(next);
            return emit(current());
          }
          if (m.command === 'SheetValues') return emit({ event: 'SheetValues', tab: m.tab, range: m.range, values: values(m.tab, m.range) });
          if (m.command === 'SheetFocus') { sent.push({ type: 'Focus', tab: m.tab, range: m.range }); return emit(feedback()); }
          if (m.command === 'Undo') { if (undo.length) { redo.push(sheet); sheet = undo.pop(); sequence++; } return emit(current()); }
          if (m.command === 'Redo') { if (redo.length) { undo.push(sheet); sheet = redo.pop(); sequence++; } return emit(current()); }
          if (m.command === 'SheetCommentCreate') {
            const tab = tabOf(m.tab);
            threads.push({ id: `new-${++sequence}`, anchor: { tab: tab.id, range: m.range, quote: valueOf(tab, m.range.split(':')[0]).display }, orphaned: false, severity: 'consider', created: Date.now() / 1000, resolved: null, messages: [{ id: `msg-${sequence}`, role: 'you', text: m.text, created: Date.now() / 1000, edits: [] }] });
            if (m.send) sent.push({ type: 'Comment', tab: m.tab, range: m.range, text: m.text });
            return emit(feedback());
          }
          if (m.command === 'CommentReply') {
            threads.find(t => t.id === m.thread).messages.push({ id: `msg-${++sequence}`, role: 'you', text: m.text, created: Date.now() / 1000, edits: [] });
            if (m.send) sent.push({ type: 'Reply', text: m.text, thread: m.thread });
            return emit(feedback());
          }
          if (m.command === 'CommentResolve' || m.command === 'CommentReopen') { threads.find(t => t.id === m.thread).resolved = m.command === 'CommentResolve' ? Date.now() / 1000 : null; return emit(feedback()); }
          if (m.command === 'SheetExport' || m.command === 'ExportAs') return emit({ event: 'Exported', path: `/tmp/exports/q3-budget.${m.format}`, warnings: [] });
          if (m.command === 'Reveal') return emit({ event: 'Revealed' });
          return emit(current());
        } catch (error) { return emit({ event: 'Error', message: error.message }); }
      }, window.__mock.latency);
    },
  };
})();
