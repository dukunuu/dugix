// Icons for the sheet toolbar, in the stroke style of ../icons.js; the doc and shared sets fill in the rest.
import { glyph as base } from '../doc/glyphs.js';

const paths = {
  underline: 'M7 4v7a5 5 0 0 0 10 0V4M5 20h14',
  merge: 'M3 5h18v14H3ZM3 12h5m8 0h5M8 9l-3 3 3 3m8-6 3 3-3 3',
  'text-color': 'M6 17 12 4l6 13M8.5 12h7M5 21h14',
  fill: 'm5 13 7-8 7 8-7 7ZM5 13h14M20 16c1 1.4 1.5 2.3 1.5 3a1.5 1.5 0 0 1-3 0c0-.7.5-1.6 1.5-3Z',
  freeze: 'M3 4h18v16H3ZM3 9h18M9 4v16',
  sort: 'M4 6h10M4 12h7M4 18h4M17 5v14m-3-3 3 3 3-3',
  ban: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18ZM6 18 18 6',
  'wrap-overflow': 'M4 6h16M4 11h9M4 16h7M17 11h3m-2-2 2 2-2 2',
  wrap: 'M4 6h16M4 11h12a2.5 2.5 0 0 1 0 5h-4m2-2-2 2 2 2M4 21h8',
  clip: 'M4 6h16M4 11h13M4 16h9M18 10v6',
  'valign-top': 'M4 4h16M8 8h8v8H8Z',
  'valign-middle': 'M4 12h16M8 7h8v10H8Z',
  'valign-bottom': 'M4 20h16M8 8h8v8H8Z',
};
export function glyph(name) {
  if (!paths[name]) return base(name);
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  for (const [key, value] of Object.entries({ viewBox: '0 0 24 24', width: 16, height: 16, fill: 'none', stroke: 'currentColor', 'stroke-width': 1.5, 'stroke-linecap': 'round', 'stroke-linejoin': 'round', 'aria-hidden': true })) svg.setAttribute(key, value);
  const path = document.createElementNS(svg.namespaceURI, 'path');
  path.setAttribute('d', paths[name]);
  svg.append(path);
  return svg;
}
