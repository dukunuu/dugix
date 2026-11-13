// Floating range-backed charts. SVG comes only from the backend's shared renderer;
// geometry previews stay local until one atomic edit is sent on release.
import * as M from './model.js';
import { glyph } from './glyphs.js';

const el = (tag, cls, text) => {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
};
const KINDS = ['column', 'bar', 'line', 'area', 'pie', 'scatter'];
const clamp = (n, min, max) => Math.max(min, Math.min(max, n));

export function chartsController(api) {
  const panel = el('aside', 'sg-sidebar sg-chart-editor');
  panel.hidden = true; panel.setAttribute('aria-label', 'Chart editor');
  const layer = el('div', 'sg-charts');
  api.grid.overlay.append(layer);
  const nodes = new Map(), previews = new Map(), rollback = new Map();
  let selected = null, inserting = null, drag = null, nudgeTimer = 0, nudgeId = null;
  const chartOf = id => api.tab()?.charts?.find(c => c.id === id);
  const showing = id => ({ ...chartOf(id), ...previews.get(id) });
  const axes = () => api.grid.geometry.axes;
  const send = (id, patch) => {
    if (!chartOf(id)) return;
    if (!rollback.has(id)) rollback.set(id, structuredClone(chartOf(id)));
    api.apply([M.editChartOp(api.tabName(), id, patch)]);
  };

  function render() {
    if (!axes()) return;
    const charts = api.tab()?.charts || [], keep = new Set();
    for (const chart of charts) {
      keep.add(chart.id);
      let entry = nodes.get(chart.id);
      if (!entry) {
        const node = el('div', 'sg-chart');
        node.dataset.chart = chart.id; node.setAttribute('aria-label', `Chart ${chart.id}`);
        const image = el('div', 'sg-chart-image'), menu = el('button', 'sg-chart-menu', '⋮');
        menu.type = 'button'; menu.setAttribute('aria-label', 'Chart menu');
        node.append(image, menu);
        for (const side of ['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w']) {
          const handle = el('span', `sg-chart-handle h-${side}`);
          handle.dataset.resize = side; handle.setAttribute('aria-label', `Resize chart ${side}`);
          node.append(handle);
        }
        node.addEventListener('pointerdown', e => start(e, chart.id));
        node.addEventListener('dblclick', e => { e.stopPropagation(); open(chart.id); });
        node.addEventListener('contextmenu', e => { e.preventDefault(); e.stopPropagation(); select(chart.id); menuFor(chart.id, { left: e.clientX, top: e.clientY, right: e.clientX, bottom: e.clientY }); });
        menu.addEventListener('pointerdown', e => e.stopPropagation());
        menu.addEventListener('click', e => { e.stopPropagation(); select(chart.id); menuFor(chart.id, menu.getBoundingClientRect()); });
        entry = { node, image, svg: null }; nodes.set(chart.id, entry); layer.append(node);
      }
      const box = M.chartBox(showing(chart.id), axes());
      Object.assign(entry.node.style, { left: `${box.left}px`, top: `${box.top}px`, width: `${box.width}px`, height: `${box.height}px` });
      entry.node.classList.toggle('selected', selected === chart.id);
      const svg = api.state.charts[api.state.tabId]?.find(c => c.id === chart.id)?.svg || '';
      if (svg !== entry.svg) {
        entry.svg = svg;
        const parsed = new DOMParser().parseFromString(svg, 'image/svg+xml');
        if (parsed.documentElement.localName === 'svg') entry.image.replaceChildren(document.importNode(parsed.documentElement, true));
        else entry.image.replaceChildren();
      }
    }
    for (const [id, entry] of nodes) if (!keep.has(id)) { entry.node.remove(); nodes.delete(id); }
    if (selected && !chartOf(selected)) deselect();
  }

  function select(id) { selected = id; api.grid.focus(); render(); }
  function flushNudge() {
    clearTimeout(nudgeTimer); nudgeTimer = 0;
    if (nudgeId && previews.has(nudgeId)) {
      const id = nudgeId, patch = previews.get(id);
      nudgeId = null; previews.delete(id); send(id, patch);
    }
  }
  function close() { panel.hidden = true; }
  function deselect() { flushNudge(); selected = null; close(); renderSelection(); }
  function renderSelection() { for (const [id, e] of nodes) e.node.classList.toggle('selected', id === selected); }
  function remove(id) {
    flushNudge(); previews.delete(id);
    if (!rollback.has(id)) rollback.set(id, structuredClone(chartOf(id)));
    api.apply([M.deleteChartOp(api.tabName(), id)]); deselect();
  }
  function menuFor(id, rect) {
    api.menus.show([{ label: 'Edit chart', run: () => open(id) }, { label: 'Delete chart', danger: true, run: () => remove(id) }], rect, { label: 'Chart' });
  }
  function insert() {
    api.editor.commit();
    inserting = new Set((api.tab().charts || []).map(c => c.id));
    const range = M.chartDataBlock(api.tab(), M.opRect(api.state.sel, M.usedRect(api.tab())));
    api.apply([M.addChartOp(api.tabName(), M.rectStr(range))]);
  }
  function open(id) { select(id); api.closeComments(); panel.hidden = false; editor(); }

  function editor() {
    const chart = chartOf(selected);
    if (!chart || panel.hidden) return;
    panel.replaceChildren();
    const head = el('div', 'side-tabs'), title = el('span', 'side-tab', 'Chart editor'), exit = el('button', 'quiet', '×');
    exit.type = 'button'; exit.setAttribute('aria-label', 'Close chart editor'); exit.addEventListener('click', close);
    head.append(title, el('span', 'spacer'), exit);
    const body = el('div', 'side-pane sg-chart-fields'); panel.append(head, body);
    const types = el('div', 'sg-chart-types'); types.setAttribute('aria-label', 'Chart type');
    for (const kind of KINDS) {
      const b = el('button', 'sg-tb'); b.type = 'button'; b.title = kind; b.dataset.kind = kind;
      b.setAttribute('aria-label', `${kind} chart`); b.setAttribute('aria-pressed', String(chart.kind === kind)); b.append(glyph(`chart-${kind}`));
      b.addEventListener('click', () => {
        const patch = { kind };
        if (chart.stacked && !['column', 'bar', 'area'].includes(kind)) patch.stacked = false;
        if (kind === 'pie') for (const key of ['x_title', 'y_title', 'y_min', 'y_max']) if (chart[key] !== undefined && chart[key] !== null) patch[key] = null;
        send(chart.id, patch); editor();
      });
      types.append(b);
    }
    body.append(el('div', 'sg-chart-label', 'Chart type'), types);
    function field(key, label, options = null, numeric = false) {
      const row = el('label', 'sg-chart-field'), input = el(options ? 'select' : 'input');
      row.append(el('span', '', label), input); input.setAttribute('aria-label', label); input.dataset.field = key;
      if (options) for (const [value, text] of options) { const o = el('option', '', text); o.value = value; input.append(o); }
      else input.type = numeric ? 'number' : 'text';
      input.value = chart[key] ?? (options ? options[0][0] : '');
      input.addEventListener('change', () => send(chart.id, { [key]: numeric ? (input.value === '' ? null : Number(input.value)) : input.value || null }));
      body.append(row);
    }
    field('data', 'Data range');
    field('series_in', 'Series in', [['cols', 'Columns'], ['rows', 'Rows']]);
    field('headers', 'Use first row as headers', [['auto', 'Auto'], ['yes', 'Yes'], ['no', 'No']]);
    if (['column', 'bar', 'area'].includes(chart.kind)) {
      const row = el('label', 'sg-chart-check'), input = el('input'); input.type = 'checkbox'; input.checked = chart.stacked;
      input.setAttribute('aria-label', 'Stacked'); input.addEventListener('change', () => send(chart.id, { stacked: input.checked }));
      row.append(input, document.createTextNode('Stacked')); body.append(row);
    }
    field('legend', 'Legend', [['right', 'Right'], ['bottom', 'Bottom'], ['none', 'None']]);
    field('title', 'Title');
    if (chart.kind !== 'pie') { field('x_title', 'X axis title'); field('y_title', 'Y axis title'); field('y_min', 'Y min', null, true); field('y_max', 'Y max', null, true); }
  }

  function start(e, id) {
    if (e.button !== 0) return;
    e.preventDefault(); e.stopPropagation(); api.editor.commit(); flushNudge(); api.menus.close(false); select(id);
    const node = nodes.get(id).node;
    drag = { id, node, side: e.target.dataset.resize || '', x: e.clientX, y: e.clientY, scroll: api.grid.position, box: M.chartBox(showing(id), axes()) };
    node.setPointerCapture(e.pointerId);
  }
  function move(e) {
    if (!drag) return;
    const { box: b, side } = drag, scroll = api.grid.position;
    const dx = e.clientX - drag.x + scroll.left - drag.scroll.left, dy = e.clientY - drag.y + scroll.top - drag.scroll.top;
    let { left, top, width, height } = b;
    if (!side) { left = Math.max(0, left + dx); top = Math.max(0, top + dy); }
    else {
      if (side.includes('e')) width = clamp(b.width + dx, 160, 2000);
      if (side.includes('s')) height = clamp(b.height + dy, 120, 2000);
      if (side.includes('w')) { width = clamp(b.width - Math.max(-b.left, dx), 160, 2000); left = b.left + b.width - width; }
      if (side.includes('n')) { height = clamp(b.height - Math.max(-b.top, dy), 120, 2000); top = b.top + b.height - height; }
    }
    previews.set(drag.id, { ...M.chartPlacement(Math.round(left), Math.round(top), axes()), w: Math.round(width), h: Math.round(height) });
    render();
  }
  function finish(e) {
    if (!drag) return;
    const { id, node } = drag; drag = null;
    if (node.hasPointerCapture(e.pointerId)) node.releasePointerCapture(e.pointerId);
    const patch = previews.get(id); previews.delete(id);
    if (patch) send(id, patch);
  }
  document.addEventListener('pointermove', move, { signal: api.signal });
  document.addEventListener('pointerup', finish, { signal: api.signal });
  document.addEventListener('pointercancel', () => { if (drag) { previews.delete(drag.id); drag = null; render(); } }, { signal: api.signal });

  function key(e) {
    if (!selected || /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName)) return false;
    if (e.key === 'Escape') { e.preventDefault(); deselect(); return true; }
    if (e.key === 'Delete' || e.key === 'Backspace') { e.preventDefault(); remove(selected); return true; }
    const delta = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
    if (!delta) return false;
    e.preventDefault();
    const b = M.chartBox(showing(selected), axes()), step = e.shiftKey ? 10 : 1;
    previews.set(selected, M.chartPlacement(b.left + delta[0] * step, b.top + delta[1] * step, axes()));
    nudgeId = selected; clearTimeout(nudgeTimer); nudgeTimer = setTimeout(flushNudge, 300); render();
    return true;
  }
  function received() {
    rollback.clear();
    if (inserting) {
      const added = api.tab()?.charts?.find(c => !inserting.has(c.id));
      if (added) { inserting = null; open(added.id); }
    }
    if (!panel.contains(document.activeElement)) editor();
  }
  function rejected() {
    inserting = null; previews.clear(); clearTimeout(nudgeTimer); nudgeId = null;
    const tab = api.tab();
    if (tab) {
      tab.charts = (tab.charts || []).map(c => rollback.get(c.id) || c);
      for (const [id, chart] of rollback) if (!tab.charts.some(c => c.id === id)) tab.charts.push(chart);
    }
    rollback.clear(); render(); editor();
  }
  return { panel, render, insert, deselect, close, key, received, rejected, destroy() { clearTimeout(nudgeTimer); } };
}
