// Popup menus: a list of rows at a point or under a button, shared by the toolbar, the
// header and cell context menus and the tab bar. Rows are buttons in the overlay layer.
import { placePopup } from '../popup.js';
import { glyph } from './glyphs.js';

const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};

export function menusController(api) {
  let open = null;

  function close(restoreFocus = true) {
    if (!open) return;
    const { node, onClose } = open;
    open = null;
    node.remove();
    onClose?.();
    if (restoreFocus) api.grid.focus();
  }
  /**
   * Show a menu. Items are `{ label, run, hint?, icon?, checked?, disabled?, danger?, rule? }`;
   * `at` is a point `{x, y}` or an element's rectangle.
   */
  function show(items, at, { label = 'Menu', className = '', onClose = null } = {}) {
    close(false);
    const node = el('div', `popup menu sg-menu ${className}`.trim());
    node.setAttribute('role', 'menu'); node.setAttribute('aria-label', label);
    for (const item of items) {
      if (item.rule) { node.append(el('div', 'menu-rule')); continue; }
      if (item.group) { node.append(el('div', 'menu-group', item.group)); continue; }
      const row = el('button', `menu-row compact${item.danger ? ' danger' : ''}${item.checked ? ' on' : ''}`);
      row.type = 'button'; row.setAttribute('role', item.checked === undefined ? 'menuitem' : item.radio ? 'menuitemradio' : 'menuitemcheckbox');
      if (item.checked !== undefined) row.setAttribute('aria-checked', String(item.checked));
      row.disabled = Boolean(item.disabled);
      if (item.icon) row.append(glyph(item.icon));
      row.append(el('span', 'menu-label', item.label));
      if (item.hint) row.append(el('span', 'menu-hint', item.hint));
      if (item.checked) row.append(glyph('check'));
      row.addEventListener('mousedown', e => e.preventDefault());
      row.addEventListener('click', () => { close(true); item.run(); });
      node.append(row);
    }
    api.overlay.append(node);
    open = { node, onClose };
    const rect = at.width === undefined ? { left: at.x, right: at.x, top: at.y, bottom: at.y, width: 0, height: 0 } : at;
    placePopup(node, rect, { prefer: 'below', align: 'left' });
    node.querySelector('button:not(:disabled)')?.focus({ preventScroll: true });
    return node;
  }
  /** A popup with arbitrary content, closed the same ways. */
  function showCustom(node, at, onClose = null) {
    close(false);
    api.overlay.append(node);
    open = { node, onClose };
    placePopup(node, at, { prefer: 'below', align: 'left' });
    return node;
  }

  api.overlay.addEventListener('keydown', e => {
    if (!open) return;
    const rows = [...open.node.querySelectorAll('button:not(:disabled)')], at = rows.indexOf(document.activeElement);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); rows[(at + (e.key === 'ArrowDown' ? 1 : -1) + rows.length) % rows.length]?.focus(); }
    else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(true); }
  });
  document.addEventListener('pointerdown', e => { if (open && !open.node.contains(e.target) && !e.target.closest?.('[data-menu-owner]')) close(false); }, { signal: api.signal, capture: true });

  return { show, showCustom, close };
}
