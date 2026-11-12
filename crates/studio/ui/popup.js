// Placement shared by the doc and sheet popups.

/** Keep a popup inside the window, below or above its anchor. */
export function placePopup(node, rect, { prefer = 'below', align = 'left' } = {}) {
  node.style.left = '0px'; node.style.top = '0px';
  const { width, height } = node.getBoundingClientRect();
  const margin = 8, W = innerWidth, H = innerHeight;
  let top = prefer === 'above' ? rect.top - height - 6 : rect.bottom + 6;
  if (top + height > H - margin) top = Math.max(margin, rect.top - height - 6);
  if (top < margin) top = Math.min(margin, H - height - margin);
  let left = align === 'center' ? rect.left + rect.width / 2 - width / 2 : align === 'right' ? rect.right - width : rect.left;
  left = Math.max(margin, Math.min(left, W - width - margin));
  node.style.left = `${Math.round(left)}px`; node.style.top = `${Math.round(top)}px`;
}
