// The pointer: click and drag to select, header clicks, the fill handle, and dragging
// header borders to resize. Keys live in sheet.js; this file only turns pointer events
// into selections and edits.
import * as M from './model.js';

export function interactController(api) {
  const { state } = api;
  const grid = api.grid, scroll = grid.scroll;
  let drag = null, last = null, scrollFrame = 0, downTarget = null;
  const clampCell = p => ({ row: Math.max(1, p.row), col: Math.max(1, p.col) });
  const cellsSel = r => ({ kind: 'cells', anchor: { row: r.r1, col: r.c1 }, head: { row: r.r2, col: r.c2 }, active: { row: r.r1, col: r.c1 } });

  /** While dragging near an edge the grid scrolls and the selection follows the pointer. */
  function autoscroll() {
    scrollFrame = 0;
    if (!drag || !last) return;
    const b = scroll.getBoundingClientRect(), edge = 28, left = b.left + grid.geometry.headerW + edge, top = b.top + 22 + edge;
    const speed = d => Math.min(40, Math.round(d / 2) + 4);
    let dx = 0, dy = 0;
    if (last.x > b.right - edge - 16) dx = speed(last.x - (b.right - edge - 16)); else if (last.x < left) dx = -speed(left - last.x);
    if (last.y > b.bottom - edge - 16) dy = speed(last.y - (b.bottom - edge - 16)); else if (last.y < top) dy = -speed(top - last.y);
    if (!dx && !dy) return;
    scroll.scrollLeft += dx; scroll.scrollTop += dy;
    move(last.x, last.y);
    scrollFrame = requestAnimationFrame(autoscroll);
  }

  function move(x, y) {
    if (!drag) return;
    const c = clampCell(grid.cellAt(x, y));
    if (drag.kind === 'cells') api.select(M.extendTo(drag.sel, c.row, c.col), { reveal: false });
    else if (drag.kind === 'cols') api.select(M.selectCol(drag.sel, c.col, true), { reveal: false });
    else if (drag.kind === 'rows') api.select(M.selectRow(drag.sel, c.row, true), { reveal: false });
    else if (drag.kind === 'point') api.editor.pointCell(c.row, c.col, true);
    else if (drag.kind === 'fill') {
      drag.target = M.fillTarget(drag.src, c);
      grid.box('fill', drag.target);
    } else if (drag.kind === 'resize') {
      const colWise = drag.axis === 'col', axis = colWise ? grid.geometry.axes.cols : grid.geometry.axes.rows;
      drag.size = Math.max(colWise ? 20 : 12, Math.round(drag.start + (colWise ? x - drag.x : y - drag.y)));
      const at = axis.pos(drag.index) + drag.size, guide = grid.resizeGuide;
      if (colWise) Object.assign(guide.style, { left: `${at - 1}px`, top: `${scroll.scrollTop}px`, width: '2px', height: `${scroll.clientHeight}px` });
      else Object.assign(guide.style, { left: `${scroll.scrollLeft}px`, top: `${at - 1}px`, width: `${scroll.clientWidth}px`, height: '2px' });
      guide.hidden = false;
    }
  }

  function finish() {
    if (!drag) return;
    const d = drag;
    drag = null;
    cancelAnimationFrame(scrollFrame); scrollFrame = 0;
    grid.box('fill', null);
    grid.resizeGuide.hidden = true;
    scroll.classList.remove('sg-dragging');
    if (d.kind === 'fill' && d.target) {
      api.apply([M.op.fill(api.tabName(), M.rectStr(d.src), M.rectStr(d.target))]);
      api.select(cellsSel(d.target));
    } else if (d.kind === 'resize' && d.size && d.size !== d.start) api.resize(d.axis, d.index, d.size);
  }

  scroll.addEventListener('pointerdown', e => {
    if (e.button !== 0 || e.target === scroll || e.target.closest?.('.sg-chart')) return;
    api.charts?.deselect();
    const pin = e.target.closest?.('.sg-pin');
    if (pin) { e.preventDefault(); api.comments.openAt(pin.dataset.pin); return; }
    api.menus.close(false);
    downTarget = e.target; // pointer capture retargets the click and double-click to the grid
    scroll.setPointerCapture(e.pointerId);
    last = { x: e.clientX, y: e.clientY };
    const sel = state.sel;
    if (e.target.classList?.contains('sg-handle')) {
      e.preventDefault(); api.editor.commit();
      drag = { kind: 'fill', src: M.opRect(sel, M.usedRect(api.tab())), target: null };
      scroll.classList.add('sg-dragging');
      return;
    }
    const edge = grid.edgeAt(e.clientX, e.clientY);
    if (edge) {
      e.preventDefault(); api.editor.commit();
      const axis = edge.axis === 'col' ? grid.geometry.axes.cols : grid.geometry.axes.rows;
      drag = { kind: 'resize', ...edge, start: axis.size(edge.index), x: e.clientX, y: e.clientY, size: 0 };
      scroll.classList.add('sg-dragging');
      return;
    }
    const p = grid.cellAt(e.clientX, e.clientY);
    if (e.detail > 1 && p.row > 0 && p.col > 0) return; // the second click of a double-click edits (dblclick)
    if (p.row === 0 && p.col === 0) { e.preventDefault(); api.editor.commit(); api.select(M.selectAll(sel)); grid.focus(); return; }
    if (p.row === 0) {
      e.preventDefault(); api.editor.commit();
      api.select(M.selectCol(sel, p.col, e.shiftKey));
      drag = { kind: 'cols', sel: state.sel }; grid.focus(); scroll.classList.add('sg-dragging');
      return;
    }
    if (p.col === 0) {
      e.preventDefault(); api.editor.commit();
      api.select(M.selectRow(sel, p.row, e.shiftKey));
      drag = { kind: 'rows', sel: state.sel }; grid.focus(); scroll.classList.add('sg-dragging');
      return;
    }
    if (api.editor.active) {
      if (api.editor.canPointNow()) {
        e.preventDefault();
        api.editor.pointCell(p.row, p.col, e.shiftKey);
        drag = { kind: 'point' };
        return;
      }
      api.editor.commit();
    }
    e.preventDefault();
    grid.focus();
    const next = e.shiftKey ? M.extendTo(state.sel, p.row, p.col) : M.newSel(p.row, p.col);
    api.select(next, { reveal: false });
    drag = { kind: 'cells', sel: next };
    scroll.classList.add('sg-dragging');
  });
  scroll.addEventListener('pointermove', e => {
    last = { x: e.clientX, y: e.clientY };
    if (drag) { move(e.clientX, e.clientY); if (!scrollFrame) scrollFrame = requestAnimationFrame(autoscroll); return; }
    const edge = grid.edgeAt(e.clientX, e.clientY);
    scroll.style.cursor = edge ? (edge.axis === 'col' ? 'col-resize' : 'row-resize') : '';
  });
  for (const name of ['pointerup', 'pointercancel', 'lostpointercapture']) scroll.addEventListener(name, finish);

  scroll.addEventListener('dblclick', e => {
    if (downTarget?.classList?.contains('sg-handle')) {
      const src = M.opRect(state.sel, M.usedRect(api.tab())), target = M.fillDownEnd(src, M.dataOf(api.tab(), grid.geometry.extent));
      if (target) { api.apply([M.op.fill(api.tabName(), M.rectStr(src), M.rectStr(target))]); api.select(cellsSel(target)); }
      return;
    }
    const edge = grid.edgeAt(e.clientX, e.clientY);
    if (edge) { api.fit(edge.axis, edge.index); return; }
    const p = grid.cellAt(e.clientX, e.clientY);
    if (p.row > 0 && p.col > 0 && !downTarget?.closest?.('.sg-pin')) api.editor.begin({ mode: 'edit' });
  });

  scroll.addEventListener('contextmenu', e => {
    e.preventDefault();
    const p = grid.cellAt(e.clientX, e.clientY), at = { x: e.clientX, y: e.clientY }, r = M.selRect(state.sel);
    api.editor.commit();
    if (p.row === 0 && p.col === 0) return;
    if (p.row === 0) {
      if (state.sel.kind !== 'cols' || p.col < r.c1 || p.col > r.c2) api.select(M.selectCol(state.sel, p.col, false));
      api.contextMenu('col', p.col, at);
    } else if (p.col === 0) {
      if (state.sel.kind !== 'rows' || p.row < r.r1 || p.row > r.r2) api.select(M.selectRow(state.sel, p.row, false));
      api.contextMenu('row', p.row, at);
    } else {
      if (!M.inRect(r, p.row, p.col)) api.select(M.newSel(p.row, p.col));
      api.contextMenu('cell', p, at);
    }
  });

  return {};
}
