// The toolbar row above the formula bar: undo and redo, number format, bold, italic,
// underline, colours, alignment, merge, comment. Every button sends a `format`, `merge` or
// `unmerge` op for the selection; the buttons show the active cell's format.
import * as M from './model.js';
import { glyph } from './glyphs.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

export function toolbarController(api) {
  const { state } = api;
  const bar = el('div', 'sg-toolbar');
  bar.setAttribute('role', 'toolbar'); bar.setAttribute('aria-label', 'Formatting');
  const buttons = {};
  // A toolbar button must not take the focus: the grid keeps it, so typing carries on.
  const noFocus = node => node.addEventListener('mousedown', e => e.preventDefault());
  function button(key, icon, label, run, extra = '') {
    const b = el('button', `sg-tb ${extra}`.trim());
    b.type = 'button'; b.title = label; b.setAttribute('aria-label', label); b.dataset.tb = key;
    if (icon) b.append(glyph(icon));
    noFocus(b);
    b.addEventListener('click', run);
    buttons[key] = b;
    bar.append(b);
    return b;
  }
  const sep = () => bar.append(el('span', 'sg-tb-sep'));

  button('undo', 'undo', 'Undo (⌘Z)', () => api.undo());
  button('redo', 'redo', 'Redo (⇧⌘Z)', () => api.redo());
  sep();
  const num = button('num', null, 'Number format', () => numberMenu(), 'wide');
  const numLabel = el('span', 'sg-tb-label', 'Automatic');
  num.append(numLabel, glyph('down'));
  num.setAttribute('aria-haspopup', 'menu');
  sep();
  button('bold', 'bold', 'Bold (⌘B)', () => api.toggle('bold'));
  button('italic', 'italic', 'Italic (⌘I)', () => api.toggle('italic'));
  button('underline', 'underline', 'Underline (⌘U)', () => api.toggle('underline'));
  sep();
  for (const [key, icon, label] of [['color', 'text-color', 'Text color'], ['fill', 'fill', 'Fill color']]) {
    const b = button(key, icon, label, () => colorMenu(key, b));
    b.setAttribute('aria-haspopup', 'menu');
    const chip = el('span', 'sg-chip');
    b.append(chip);
  }
  sep();
  for (const side of ['left', 'center', 'right']) button(`align-${side}`, `align-${side}`, `Align ${side}`, () => api.format({ align: api.formatAt().align === side ? null : side }));
  sep();
  button('merge', 'merge', 'Merge cells', () => api.toggleMerge());
  button('comment', 'comment', 'Comment (⌥⌘M)', () => api.comments.begin());

  function numberMenu() {
    const current = api.formatAt().num ?? null;
    api.menus.show(M.NUMBER_FORMATS.map(([label, code]) => ({ label, hint: code && code !== '@' ? code : '', checked: code === current, radio: true, run: () => api.format({ num: code }) })), num.getBoundingClientRect(), { label: 'Number format' });
  }
  function colorMenu(kind, anchor) {
    const node = el('div', 'popup menu sg-colors');
    node.setAttribute('role', 'menu'); node.setAttribute('aria-label', kind === 'fill' ? 'Fill color' : 'Text color');
    const none = el('button', 'menu-row compact', kind === 'fill' ? 'No fill' : 'Default');
    none.type = 'button'; none.setAttribute('role', 'menuitem'); none.dataset.token = ''; noFocus(none);
    none.addEventListener('click', () => { api.menus.close(true); api.format({ [kind]: null }); });
    const grid = el('div', 'sg-swatches');
    for (const token of Object.keys(state.sheet.theme?.colors || {})) {
      const value = M.cssColor(state.sheet.theme, token);
      if (!value) continue;
      const s = el('button', 'sg-swatch');
      s.type = 'button'; s.title = token; s.setAttribute('aria-label', token); s.dataset.token = token;
      s.style.background = value;
      noFocus(s);
      s.addEventListener('click', () => { api.menus.close(true); api.format({ [kind]: token }); });
      grid.append(s);
    }
    node.append(none, grid);
    api.menus.showCustom(node, anchor.getBoundingClientRect());
  }

  /** Reflect the active cell: pressed styles, the format's name, the colours, the alignment. */
  function sync() {
    if (!state.sel || !state.sheet) return;
    const fmt = api.formatAt();
    for (const key of ['bold', 'italic', 'underline']) { buttons[key].classList.toggle('on', Boolean(fmt[key])); buttons[key].setAttribute('aria-pressed', String(Boolean(fmt[key]))); }
    for (const side of ['left', 'center', 'right']) { const on = (fmt.align || (api.valueKind() === 'number' ? 'right' : 'left')) === side && Boolean(fmt.align); buttons[`align-${side}`].classList.toggle('on', on); buttons[`align-${side}`].setAttribute('aria-pressed', String(on)); }
    numLabel.textContent = M.numberFormatName(fmt.num);
    buttons.color.querySelector('.sg-chip').style.background = M.cssColor(state.sheet.theme, fmt.color) || 'var(--sg-fg)';
    buttons.fill.querySelector('.sg-chip').style.background = M.cssColor(state.sheet.theme, fmt.fill) || 'transparent';
    const merged = Boolean(M.mergeAt(M.mergesOf(api.tab()), state.sel.active.row, state.sel.active.col));
    buttons.merge.classList.toggle('on', merged); buttons.merge.setAttribute('aria-pressed', String(merged));
    buttons.undo.disabled = !state.canUndo; buttons.redo.disabled = !state.canRedo;
  }

  return { bar, sync, buttons };
}
