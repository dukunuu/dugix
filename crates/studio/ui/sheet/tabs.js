// The tab bar along the bottom: switch, add, rename (double-click), reorder (drag) and
// delete (context menu). Which tab is showing is this window's state, not the file's.
import * as M from './model.js';
import { glyph } from './glyphs.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

export function tabsController(api) {
  const { state } = api;
  const bar = el('div', 'sg-tabbar');
  const add = el('button', 'sg-tab-add');
  add.type = 'button'; add.title = 'Add a sheet'; add.setAttribute('aria-label', 'Add a sheet');
  add.append(glyph('plus'));
  const list = el('div', 'sg-tablist');
  list.setAttribute('role', 'tablist'); list.setAttribute('aria-label', 'Sheets');
  bar.append(add, list);
  let drag = null;

  add.addEventListener('click', () => {
    const names = state.sheet.tabs.map(t => t.name), name = M.nextTabName(names);
    state.addedTab = name;
    api.apply([M.op.addTab(name, names.at(-1))]);
  });

  /** Bring the buttons in line with the sheet's tabs, keeping the ones that stay (a click may be in progress on one). */
  function render() {
    if (!state.sheet) return;
    const existing = new Map([...list.children].map(b => [b.dataset.tab, b]));
    const nodes = state.sheet.tabs.map(tab => {
      let b = existing.get(tab.id);
      if (!b || b.querySelector('input')) {
        b = el('button', 'sg-tab');
        b.type = 'button'; b.setAttribute('role', 'tab'); b.dataset.tab = tab.id;
        b.addEventListener('mousedown', e => e.preventDefault());
        b.addEventListener('pointerdown', e => start(e, b.tab));
        b.addEventListener('dblclick', () => rename(b.tab, b));
        b.addEventListener('contextmenu', e => { e.preventDefault(); menu(b.tab, { x: e.clientX, y: e.clientY }); });
        b.append(el('span', 'sg-tab-name'));
      }
      b.tab = tab; b.dataset.name = tab.name;
      b.classList.toggle('on', tab.id === state.tabId);
      b.setAttribute('aria-selected', String(tab.id === state.tabId));
      if (!b.querySelector('input')) b.firstChild.textContent = tab.name;
      return b;
    });
    if (nodes.length !== list.children.length || nodes.some((n, i) => list.children[i] !== n)) list.replaceChildren(...nodes);
  }

  function rename(tab, button) {
    api.editor.commit();
    const input = el('input', 'sg-tab-input');
    input.type = 'text'; input.value = tab.name; input.maxLength = 31; input.spellcheck = false;
    input.setAttribute('aria-label', 'Sheet name');
    button.replaceChildren(input);
    input.focus(); input.select();
    let done = false;
    const finish = save => {
      if (done) return;
      done = true;
      const name = input.value.trim();
      if (save && name && name !== tab.name) api.apply([M.op.renameTab(tab.name, name)]);
      render();
      api.grid.focus();
    };
    input.addEventListener('keydown', e => {
      e.stopPropagation();
      if (e.key === 'Enter') { e.preventDefault(); finish(true); } else if (e.key === 'Escape') { e.preventDefault(); finish(false); }
    });
    input.addEventListener('blur', () => finish(true));
    input.addEventListener('pointerdown', e => e.stopPropagation());
  }

  function menu(tab, at) {
    api.menus.show([
      { label: 'Rename', run: () => rename(tab, list.querySelector(`[data-tab="${CSS.escape(tab.id)}"]`)) },
      { label: 'Delete', danger: true, disabled: state.sheet.tabs.length < 2, run: () => api.apply([M.op.deleteTab(tab.name)]) },
    ], at, { label: `${tab.name} sheet` });
  }

  // Reordering by dragging: the tab follows the pointer's slot among the others.
  function start(e, tab) {
    if (e.button !== 0 || e.target.closest('input')) return;
    api.switchTab(tab.id);
    const names = state.sheet.tabs.map(t => t.name), from = names.indexOf(tab.name);
    drag = { tab, from, to: from, x: e.clientX, moved: false, names };
    const move = ev => {
      if (!drag) return;
      if (!drag.moved && Math.abs(ev.clientX - drag.x) < 5) return;
      drag.moved = true;
      const buttons = [...list.children].filter(b => b.dataset.name !== tab.name);
      let to = 0;
      for (const b of buttons) { const r = b.getBoundingClientRect(); if (ev.clientX > r.left + r.width / 2) to++; }
      drag.to = to;
      list.classList.add('dragging');
      for (const b of list.children) b.classList.toggle('drag-source', b.dataset.name === tab.name);
      const target = buttons[to];
      list.style.setProperty('--sg-drop', target ? `${target.offsetLeft}px` : `${list.scrollWidth}px`);
    };
    const up = () => {
      removeEventListener('pointermove', move); removeEventListener('pointerup', up);
      const d = drag; drag = null;
      list.classList.remove('dragging');
      for (const b of list.children) b.classList.remove('drag-source');
      if (!d?.moved || d.to === d.from) return;
      const ops = M.reorderOps(d.names, M.dragOrder(d.names, d.from, d.to));
      if (ops.length) api.apply(ops);
    };
    addEventListener('pointermove', move); addEventListener('pointerup', up);
  }

  return { bar, render };
}
