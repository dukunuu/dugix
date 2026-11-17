// Copy, cut and paste with the system clipboard. Copy writes the display values as TSV and
// an HTML table, and remembers the source range: pasting that same text back inside the
// sheet sends a `copy` op (so references shift as in spreadsheets, and cut moves the cells);
// anything else is text from outside, parsed into `set_cells` at the selection's corner.
import * as M from './model.js';

const escapeHtml = text => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');

/** An HTML table of strings, the flavour Google Sheets and Excel read. */
export function htmlTable(rows) {
  const body = rows.map(r => `<tr>${r.map(t => `<td>${escapeHtml(t).replaceAll('\n', '<br>')}</td>`).join('')}</tr>`).join('');
  return `<table><tbody>${body}</tbody></table>`;
}
/** The rows of the first table in pasted HTML, as text; null when there is none. */
export function tableRows(html) {
  const table = new DOMParser().parseFromString(html, 'text/html').querySelector('table');
  if (!table) return null;
  const rows = [...table.querySelectorAll('tr')].map(tr => [...tr.children].filter(c => /^t[dh]$/i.test(c.tagName)).map(c => {
    for (const br of c.querySelectorAll('br')) br.replaceWith('\n');
    return c.textContent.replace(/ /g, ' ').replace(/\s*\n\s*/g, '\n').trim();
  }));
  return rows.length ? rows : null;
}

export function clipboardController(api) {
  const { state } = api;

  /** The range copy and cut take: the selection, stopping at the data for whole columns and rows. */
  const source = () => M.opRect(state.sel, M.usedRect(api.tab()));
  function rowsOf(r) {
    const out = [];
    for (let row = r.r1; row <= r.r2; row++) {
      const line = [];
      for (let col = r.c1; col <= r.c2; col++) line.push(api.displayAt(row, col));
      out.push(line);
    }
    return out;
  }
  function copy(event, cut) {
    const r = source(), rows = rowsOf(r), tsv = M.toTsv(rows);
    if (event?.clipboardData) {
      event.clipboardData.setData('text/plain', tsv);
      event.clipboardData.setData('text/html', htmlTable(rows));
      event.preventDefault();
    }
    state.clip = { tabId: state.tabId, tabName: api.tabName(), rect: r, cut, tsv };
    api.grid.paintBoxes();
    api.toast(cut ? 'Cut. Paste to move it.' : 'Copied');
  }
  function paste(event) {
    const data = event.clipboardData;
    if (!data) return;
    event.preventDefault();
    const plain = data.getData('text/plain'), html = data.getData('text/html');
    const r = M.selRect(state.sel), at = { row: r.r1, col: r.c1 };
    const tab = api.tabName(), clip = state.clip;
    // Our own copy, not something copied since: move or copy the cells inside the sheet.
    if (clip && plain === clip.tsv) {
      const target = M.addr(at.row, at.col), size = M.rectSize(clip.rect);
      api.apply([M.op.copy(clip.tabName, M.rectStr(clip.rect), target, clip.cut, clip.tabName === tab ? null : tab)]);
      api.select({ kind: 'cells', anchor: at, head: { row: at.row + size.rows - 1, col: at.col + size.cols - 1 }, active: at });
      if (clip.cut) { state.clip = null; api.grid.paintBoxes(); }
      return;
    }
    let rows = plain ? M.parseTsv(plain) : null;
    if (!rows && html) rows = tableRows(html);
    if (!rows) return;
    rows = M.tileBlock(rows, M.rectSize(state.sel.kind === 'cells' ? r : M.cellRect(at.row, at.col)));
    const width = rows[0].length;
    api.apply([M.setBlock(tab, at.row, at.col, rows)]);
    api.select({ kind: 'cells', anchor: at, head: { row: at.row + rows.length - 1, col: at.col + width - 1 }, active: at });
  }

  /** Copy, cut and paste events count only when the grid has the focus and no cell is being edited. */
  const ours = e => !api.editor.active && state.sel && (e.target === document.body || api.grid.root.contains(e.target));
  document.addEventListener('copy', e => { if (ours(e)) copy(e, false); }, { signal: api.signal });
  document.addEventListener('cut', e => { if (ours(e)) copy(e, true); }, { signal: api.signal });
  document.addEventListener('paste', e => { if (ours(e)) paste(e); }, { signal: api.signal });

  return { clear() { state.clip = null; api.grid.paintBoxes(); } };
}
