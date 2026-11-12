// The key map, as a pure function: a key event (and what the editor is doing) in, an
// action out, or null when the browser should handle the key itself. Google Sheets' defaults.
const ARROWS = { ArrowUp: [-1, 0], ArrowDown: [1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] };

/**
 * `e` is `{key, code, shiftKey, metaKey, ctrlKey, altKey}`. `ctx`: `editing`, the edit
 * `mode` ('enter' after typing, 'edit' after Enter or F2), `pointable` (a reference may
 * go at the caret) and `pointing` (one was just inserted).
 */
export function keyAction(e, ctx = {}) {
  const mod = e.metaKey || e.ctrlKey, { shiftKey: shift, altKey: alt, key } = e;
  const arrow = ARROWS[key];
  if (ctx.editing) {
    if (key === 'Escape') return { do: 'cancel' };
    if (key === 'F2') return { do: 'toggleMode' };
    if (key === 'Enter') return alt || mod ? { do: 'newline' } : { do: 'commit', step: { dir: 'down', back: shift } };
    if (key === 'Tab') return { do: 'commit', step: { dir: 'right', back: shift } };
    if (arrow && !mod && !alt) {
      if (ctx.pointing || ctx.pointable) return { do: 'point', dr: arrow[0], dc: arrow[1], extend: shift };
      if (ctx.mode === 'enter' && !shift) return { do: 'commit', move: { dr: arrow[0], dc: arrow[1] } };
    }
    return null;
  }
  const lower = key.length === 1 ? key.toLowerCase() : key;
  if (mod && !alt) {
    if (lower === 'z') return { do: shift ? 'redo' : 'undo' };
    if (lower === 'y') return { do: 'redo' };
    if (lower === 'a') return { do: 'selectAll' };
    if (lower === 'b') return { do: 'format', key: 'bold' };
    if (lower === 'i') return { do: 'format', key: 'italic' };
    if (lower === 'u') return { do: 'format', key: 'underline' };
    if (lower === 'd') return { do: 'fillDown' };
    if (lower === 'r') return { do: 'fillRight' };
    if (key === 'Home') return { do: 'home', shift };
    if (key === 'End') return { do: 'end', shift };
    if (key === ' ') return { do: 'selectColumn' };
    if (arrow) return { do: 'move', dr: arrow[0], dc: arrow[1], extend: shift, jump: true };
    return null;
  }
  if (mod && alt && e.code === 'KeyM') return { do: 'comment' };
  if (mod || (alt && key.length !== 1)) return null;
  if (arrow) return { do: 'move', dr: arrow[0], dc: arrow[1], extend: shift, jump: false };
  if (key === 'Tab') return { do: 'step', dir: 'right', back: shift };
  if (key === 'Enter') return shift ? { do: 'step', dir: 'down', back: true } : { do: 'edit' };
  if (key === 'F2') return { do: 'edit' };
  if (key === 'Delete' || key === 'Backspace') return { do: 'clear' };
  if (key === 'Escape') return { do: 'escape' };
  if (key === 'PageDown' || key === 'PageUp') return { do: 'page', dir: key === 'PageDown' ? 1 : -1, extend: shift };
  if (key === 'Home') return { do: 'home', row: true, shift };
  if (key === ' ' && shift) return { do: 'selectRow' };
  if (key.length === 1) return { do: 'type', text: key };
  if (key === 'Process' || key === 'Dead') return { do: 'edit', ime: true };
  return null;
}
