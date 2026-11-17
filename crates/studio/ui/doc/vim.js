// Vim keys for the doc's Markdown source view: the same modes, motions,
// operators, counts, `.` and change-sized undo as the terminal's engine
// (crates/tui/src/vim.rs). It edits a plain line buffer; the caller copies it
// in from the textarea before a key and back out after.

/** A text buffer of lines of characters (code points) with a cursor. */
export class Buffer {
  constructor(text = '', offset = 0) {
    this.lines = text.split('\n').map(line => Array.from(line));
    this.cursor = this.posOf(offset);
  }
  text() { return this.lines.map(l => l.join('')).join('\n'); }
  /** UTF-16 offset of a position (textarea selection units). */
  offsetOf([row, col]) {
    let at = 0;
    for (let r = 0; r < row; r++) at += this.lines[r].join('').length + 1;
    return at + this.lines[row].slice(0, col).join('').length;
  }
  posOf(offset) {
    let rest = offset;
    for (let row = 0; row < this.lines.length; row++) {
      const line = this.lines[row];
      const units = line.join('').length;
      if (rest <= units || row === this.lines.length - 1) {
        let col = 0, used = 0;
        while (col < line.length && used + line[col].length <= rest) used += line[col++].length;
        return [row, col];
      }
      rest -= units + 1;
    }
    return [0, 0];
  }
  /** Delete from `a` to `b` (ordered, end exclusive); returns the text removed. */
  remove(a, b) {
    const removed = spanText(this.lines, a, b);
    const head = this.lines[a[0]].slice(0, a[1]), tail = this.lines[b[0]].slice(b[1]);
    this.lines.splice(a[0], b[0] - a[0] + 1, head.concat(tail));
    this.cursor = a;
    return removed;
  }
  /** Insert `text` at `at`; the cursor ends after it. */
  insert(at, text) {
    const parts = text.split('\n').map(p => Array.from(p));
    const line = this.lines[at[0]], head = line.slice(0, at[1]), tail = line.slice(at[1]);
    const rows = parts.map((p, i) => (i === 0 ? head.concat(p) : p));
    const last = rows.length - 1, col = rows[last].length;
    rows[last] = rows[last].concat(tail);
    this.lines.splice(at[0], 1, ...rows);
    this.cursor = [at[0] + last, col];
  }
}

const isWord = c => /[\p{L}\p{N}_]/u.test(c);
const isBlank = c => /\s/u.test(c);
function cls(text, [row, col], big) {
  const line = text[row];
  if (!line.length) return 3;
  if (col >= line.length) return 0;
  const c = line[col];
  if (isBlank(c)) return 0;
  return big || isWord(c) ? 1 : 2;
}
function next(text, [row, col]) {
  if (col < text[row].length) return [row, col + 1];
  return row + 1 < text.length ? [row + 1, 0] : null;
}
function prev(text, [row, col]) {
  if (col > 0) return [row, col - 1];
  return row > 0 ? [row - 1, text[row - 1].length] : null;
}
const firstNonBlank = line => { const i = line.findIndex(c => !isBlank(c)); return i < 0 ? line.length : i; };
const lastCol = line => Math.max(line.length - 1, 0);
const cmp = (a, b) => a[0] - b[0] || a[1] - b[1];
const order = (a, b) => (cmp(a, b) <= 0 ? [a, b] : [b, a]);
function spanText(text, a, b) {
  if (a[0] === b[0]) return text[a[0]].slice(a[1], b[1]).join('');
  const out = [text[a[0]].slice(a[1]).join('')];
  for (let r = a[0] + 1; r < b[0]; r++) out.push(text[r].join(''));
  out.push(text[b[0]].slice(0, b[1]).join(''));
  return out.join('\n');
}

function wordForward(text, from, big) {
  const start = cls(text, from, big), end = [text.length - 1, text.at(-1).length];
  let at = from;
  if (start !== 0) {
    for (;;) {
      const n = next(text, at);
      if (!n) return end;
      at = n;
      if (start === 3 || cls(text, n, big) !== start) break;
    }
  }
  while (cls(text, at, big) === 0) { const n = next(text, at); if (!n) return end; at = n; }
  return at;
}
function wordEnd(text, from, big) {
  let at = next(text, from);
  if (!at) return from;
  while ([0, 3].includes(cls(text, at, big))) { const n = next(text, at); if (!n) return from; at = n; }
  const c = cls(text, at, big);
  for (let n = next(text, at); n && cls(text, n, big) === c; n = next(text, n)) at = n;
  return at;
}
function wordBack(text, from, big) {
  let at = prev(text, from);
  if (!at) return from;
  while (cls(text, at, big) === 0) { const p = prev(text, at); if (!p) return at; at = p; }
  const c = cls(text, at, big);
  if (c === 3) return at;
  for (let p = prev(text, at); p && cls(text, p, big) === c; p = prev(text, p)) at = p;
  return at;
}
function paragraph(text, row, forward) {
  const last = text.length - 1;
  const step = r => (forward ? (r < last ? r + 1 : null) : (r > 0 ? r - 1 : null));
  let r = row;
  for (let n = step(r); n !== null; n = step(r)) { r = n; if (text[r].length) break; }
  for (let n = step(r); n !== null; n = step(r)) { r = n; if (!text[r].length) return r; }
  return r;
}

const MOTIONS = {
  h: 'left', l: 'right', ' ': 'right', j: 'down', k: 'up', w: 'w', W: 'W', e: 'e', E: 'E', b: 'b', B: 'B',
  0: 'lineStart', '^': 'firstNonBlank', $: 'lineEnd', G: 'fileEnd', '}': 'paraForward', '{': 'paraBack',
};
const ARROWS = { ArrowLeft: 'h', Backspace: 'h', ArrowRight: 'l', ArrowUp: 'k', ArrowDown: 'j', Home: '0', End: '$', Delete: 'x' };
const OPS = { d: 'delete', c: 'change', y: 'yank' };
const combine = (a, b) => (a == null && b == null ? null : (a ?? 1) * (b ?? 1));

export class Vim {
  constructor() {
    this.register = ''; this.linewise = false; this.lastChange = [];
    this.reset();
  }
  get mode() { return this._mode; }
  label() { return { insert: 'INSERT', normal: 'NORMAL', visual: 'VISUAL', vline: 'V-LINE' }[this._mode]; }
  /** Back to Insert with nothing pending (the register and `.` survive). */
  reset() {
    this._mode = 'insert'; this.count = null; this.op = null; this.prefix = null; this.keys = [];
    this.recording = null; this.insertStart = null; this.undoStack = []; this.redoStack = [];
    this.seen = null; this.wantCol = null; this.anchor = [0, 0]; this.replaying = false; this.skipRecord = false;
  }
  /** The Visual selection as an ordered, end-exclusive range. */
  visualRange(buf) {
    const c = buf.cursor;
    if (this._mode === 'visual') { const [a, b] = order(this.anchor, c); return [a, [b[0], Math.min(b[1] + 1, buf.lines[b[0]].length)]]; }
    if (this._mode === 'vline') { const r0 = Math.min(this.anchor[0], c[0]), r1 = Math.max(this.anchor[0], c[0]); return [[r0, 0], [r1, buf.lines[r1].length]]; }
    return null;
  }
  /** Handle a key ({ key, ctrlKey, metaKey, altKey }); returns 'handled', 'passthrough' or 'leave'. */
  key(buf, ev) {
    if (this._mode === 'insert') return this.insertKey(buf, ev);
    if (this.seen !== null && this.seen !== buf.text()) { this.undoStack = []; this.redoStack = []; }
    const out = this.normalKey(buf, ev);
    this.seen = buf.text();
    return out;
  }
  snapshot(buf) { return { text: buf.text(), cursor: [...buf.cursor] }; }
  restore(buf, snap) { const b = new Buffer(snap.text); buf.lines = b.lines; buf.cursor = [...snap.cursor]; }
  record(before, buf) {
    if (this.replaying || before.text === buf.text()) return;
    this.undoStack.push({ before, after: this.snapshot(buf) }); this.redoStack = [];
  }
  insertKey(buf, ev) {
    if (!this.insertStart) this.insertStart = this.snapshot(buf);
    if (this.recording) this.recording.push(ev);
    if (ev.key !== 'Escape' || ev.ctrlKey || ev.metaKey || ev.altKey) return 'passthrough';
    const before = this.insertStart; this.insertStart = null;
    if (before) this.record(before, buf);
    if (this.recording && !this.replaying) this.lastChange = this.recording;
    this.recording = null;
    this._mode = 'normal';
    if (buf.cursor[1] > 0) buf.cursor = [buf.cursor[0], buf.cursor[1] - 1];
    this.seen = buf.text();
    return 'handled';
  }
  pending() { return this.count !== null || this.op !== null || this.prefix !== null; }
  clearPending() { this.count = null; this.op = null; this.prefix = null; this.keys = []; }
  normalKey(buf, ev) {
    if (ev.metaKey || ev.altKey || (ev.ctrlKey && ev.key !== 'r')) return 'passthrough';
    const visual = this._mode === 'visual' || this._mode === 'vline';
    if (ev.ctrlKey) { this.clearPending(); this.redo(buf); return 'handled'; }
    if (ev.key === 'Tab') return 'passthrough';
    if (ev.key === 'Escape') {
      if (this.pending() || visual) { this.clearPending(); this._mode = 'normal'; return 'handled'; }
      return 'leave';
    }
    if (ev.key === 'Enter') { this.clearPending(); if (!visual) this.move(buf, 'down', null); return 'handled'; }
    const ch = ev.key.length === 1 ? ev.key : ARROWS[ev.key];
    if (!ch) { this.clearPending(); return 'handled'; }
    this.keys.push(ev);
    if (!'jk0123456789'.includes(ch)) this.wantCol = null;
    const before = this.snapshot(buf);
    this.skipRecord = false;
    if (visual) this.visualCommand(buf, ch); else this.command(buf, ch);
    if (this._mode === 'insert') {
      this.insertStart = before;
      this.recording = this.replaying || visual ? null : this.keys;
      this.keys = [];
    } else if (!this.pending()) {
      const keys = this.keys; this.keys = [];
      if (before.text !== buf.text() && !this.skipRecord && !visual && !this.replaying) this.lastChange = keys;
      if (!this.skipRecord) this.record(before, buf);
      this.clamp(buf);
    }
    return 'handled';
  }
  clamp(buf) {
    if (this._mode === 'insert') return;
    const [row, col] = buf.cursor, max = lastCol(buf.lines[row]);
    if (col > max) buf.cursor = [row, max];
  }
  takeCount() { const n = Math.max(this.count ?? 1, 1); this.count = null; return n; }
  digit(ch) {
    if (!/[0-9]/.test(ch) || (ch === '0' && this.count === null)) return false;
    this.count = (this.count ?? 0) * 10 + Number(ch);
    return true;
  }
  command(buf, ch) {
    if (this.prefix) { const p = this.prefix; this.prefix = null; this.prefixed(buf, p, ch); return; }
    if (this.digit(ch)) return;
    if (this.op) { this.operatorPending(buf, ch); return; }
    if (OPS[ch]) { this.op = { op: OPS[ch], count: this.count }; this.count = null; return; }
    switch (ch) {
      case 'g': case 'r': this.prefix = ch; return;
      case 'x': this.apply(buf, 'delete', 'right', this.takeCountOrNull()); return;
      case 'X': this.apply(buf, 'delete', 'left', this.takeCountOrNull()); return;
      case 'D': this.apply(buf, 'delete', 'lineEnd', this.takeCountOrNull()); return;
      case 'C': this.apply(buf, 'change', 'lineEnd', this.takeCountOrNull()); return;
      case 's': this.apply(buf, 'change', 'right', this.takeCountOrNull()); return;
      case 'S': this.linesOp(buf, 'change', this.takeCount()); return;
      case 'Y': this.linesOp(buf, 'yank', this.takeCount()); return;
      case 'p': case 'P': this.put(buf, ch === 'P', this.takeCount()); return;
      case 'J': this.join(buf, Math.max(this.takeCount(), 2) - 1); return;
      case '~': this.toggleCase(buf, this.takeCount()); return;
      case 'u': for (let n = this.takeCount(); n > 0; n--) this.undo(buf); this.skipRecord = true; return;
      case '.': { const c = this.count; this.count = null; this.repeat(buf, c); this.skipRecord = true; return; }
      case 'v': case 'V': this.count = null; this.anchor = [...buf.cursor]; this._mode = ch === 'v' ? 'visual' : 'vline'; return;
      case 'i': case 'a': case 'I': case 'A': case 'o': case 'O': this.insert(buf, ch); return;
      default:
        if (MOTIONS[ch]) this.move(buf, MOTIONS[ch], this.takeCountOrNull());
        else this.clearPending();
    }
  }
  takeCountOrNull() { const c = this.count; this.count = null; return c; }
  prefixed(buf, prefix, ch) {
    if (prefix === 'g' && ch === 'g') {
      if (this.op) { const { op, count } = this.op; this.op = null; this.apply(buf, op, 'fileStart', combine(count, this.takeCountOrNull())); }
      else this.move(buf, 'fileStart', this.takeCountOrNull());
    } else if (prefix === 'r') this.replaceChars(buf, ch, this.takeCount());
    else this.clearPending();
  }
  operatorPending(buf, ch) {
    if (ch === 'g') { this.prefix = 'g'; return; }
    const { op, count: opCount } = this.op; this.op = null;
    const count = combine(opCount, this.takeCountOrNull());
    if (OPS[ch] === op) this.linesOp(buf, op, Math.max(count ?? 1, 1));
    else if (MOTIONS[ch]) this.apply(buf, op, MOTIONS[ch], count);
    else this.clearPending();
  }
  move(buf, motion, count) {
    const t = this.target(buf, motion, count, false);
    if (motion === 'lineEnd') this.wantCol = Infinity;
    buf.cursor = t.pos;
    this.clamp(buf);
  }
  target(buf, motion, count, op) {
    const text = buf.lines, [row, col] = buf.cursor, n = Math.max(count ?? 1, 1), last = text.length - 1;
    const cw = (pos, inclusive = false) => ({ pos, linewise: false, inclusive });
    const lines = r => ({ pos: [r, firstNonBlank(text[r])], linewise: true });
    switch (motion) {
      case 'left': return cw([row, Math.max(col - n, 0)]);
      case 'right': return cw([row, Math.min(col + n, op ? text[row].length : lastCol(text[row]))]);
      case 'down': case 'up': {
        const r = motion === 'down' ? Math.min(row + n, last) : Math.max(row - n, 0);
        if (this.wantCol === null) this.wantCol = col;
        return { pos: [r, Math.min(this.wantCol, lastCol(text[r]))], linewise: true };
      }
      case 'w': case 'W': {
        let at = [row, col];
        for (let i = 0; i < n; i++) at = wordForward(text, at, motion === 'W');
        if (op && at[0] > row && at[1] <= firstNonBlank(text[at[0]])) {
          let r = at[0] - 1;
          while (r > row && !text[r].length) r--;
          at = [r, text[r].length];
        }
        return cw(at);
      }
      case 'e': case 'E': { let at = [row, col]; for (let i = 0; i < n; i++) at = wordEnd(text, at, motion === 'E'); return cw(at, true); }
      case 'b': case 'B': { let at = [row, col]; for (let i = 0; i < n; i++) at = wordBack(text, at, motion === 'B'); return cw(at); }
      case 'lineStart': return cw([row, 0]);
      case 'firstNonBlank': return cw([row, firstNonBlank(text[row])]);
      case 'lineEnd': { const r = Math.min(row + n - 1, last); return cw([r, lastCol(text[r])], true); }
      case 'fileStart': return lines(Math.min(count == null ? 0 : count - 1, last));
      case 'fileEnd': return lines(Math.min(count == null ? last : count - 1, last));
      default: {
        const forward = motion === 'paraForward';
        let r = row;
        for (let i = 0; i < n; i++) r = paragraph(text, r, forward);
        return cw(forward && r === last && text[r].length ? [r, text[r].length] : [r, 0]);
      }
    }
  }
  apply(buf, op, motion, count) {
    const from = [...buf.cursor], text = buf.lines;
    if (op === 'change' && (motion === 'w' || motion === 'W') && cls(text, from, motion === 'W') !== 0) motion = motion === 'w' ? 'e' : 'E';
    let t;
    const atWordEnd = () => { const c = cls(text, from, false), n = next(text, from); return c !== 0 && (!n || cls(text, n, false) !== c); };
    if (op === 'change' && (motion === 'e' || motion === 'E') && (count ?? 1) === 1 && atWordEnd()) t = { pos: from, linewise: false, inclusive: true };
    else t = this.target(buf, motion, count, true);
    if (t.linewise) { this.rows(buf, op, Math.min(from[0], t.pos[0]), Math.max(from[0], t.pos[0])); return; }
    let [a, b] = order(from, t.pos);
    if (t.inclusive) b = [b[0], Math.min(b[1] + 1, text[b[0]].length)];
    this.charsOp(buf, op, a, b);
  }
  linesOp(buf, op, count) {
    const row = buf.cursor[0];
    this.rows(buf, op, row, Math.min(row + count - 1, buf.lines.length - 1));
  }
  charsOp(buf, op, a, b) {
    const removed = spanText(buf.lines, a, b);
    if (removed.length) { this.register = removed; this.linewise = false; }
    if (op === 'yank') buf.cursor = a;
    else buf.remove(a, b);
    if (op === 'change') this._mode = 'insert';
  }
  rows(buf, op, r0, r1) {
    const text = buf.lines;
    this.register = text.slice(r0, r1 + 1).map(l => l.join('')).join('\n') + '\n';
    this.linewise = true;
    if (op === 'yank') { buf.cursor = [r0, buf.cursor[0] === r0 ? buf.cursor[1] : 0]; return; }
    if (op === 'change') {
      const indent = firstNonBlank(text[r0]);
      buf.remove([r0, indent], [r1, text[r1].length]);
      this._mode = 'insert';
      return;
    }
    if (r1 + 1 < text.length) buf.remove([r0, 0], [r1 + 1, 0]);
    else if (r0 > 0) buf.remove([r0 - 1, text[r0 - 1].length], [r1, text[r1].length]);
    else buf.remove([0, 0], [r1, text[r1].length]);
    const row = Math.min(r0, buf.lines.length - 1);
    buf.cursor = [row, firstNonBlank(buf.lines[row])];
  }
  put(buf, before, count) {
    if (!this.register) return;
    const [row, col] = buf.cursor, repeated = this.register.repeat(count);
    if (this.linewise) {
      const body = repeated.replace(/\n$/, '');
      let target;
      if (before) { buf.insert([row, 0], `${body}\n`); target = row; }
      else { buf.insert([row, buf.lines[row].length], `\n${body}`); target = row + 1; }
      buf.cursor = [target, firstNonBlank(buf.lines[target])];
    } else {
      const at = !before && buf.lines[row].length ? [row, col + 1] : [row, col];
      buf.insert(at, repeated);
      buf.cursor = prev(buf.lines, buf.cursor) ?? buf.cursor;
    }
  }
  join(buf, joins) {
    for (let i = 0; i < joins; i++) {
      const row = buf.cursor[0];
      if (row + 1 >= buf.lines.length) break;
      const here = buf.lines[row], nxt = buf.lines[row + 1], blank = firstNonBlank(nxt), len = here.length;
      buf.remove([row, len], [row + 1, blank]);
      if (len && blank < nxt.length && here.at(-1) !== ' ' && nxt[blank] !== ')') buf.insert([row, len], ' ');
      buf.cursor = [row, len];
    }
  }
  toggleCase(buf, count) {
    const [row, col] = buf.cursor, line = buf.lines[row], end = Math.min(col + count, line.length);
    if (end === col) return;
    for (let i = col; i < end; i++) { const c = line[i]; line[i] = c === c.toUpperCase() ? c.toLowerCase() : c.toUpperCase(); }
    buf.cursor = [row, Math.min(end, lastCol(line))];
  }
  replaceChars(buf, ch, count) {
    const [row, col] = buf.cursor, line = buf.lines[row];
    if (ch.length !== 1 || col + count > line.length) return;
    for (let i = col; i < col + count; i++) line[i] = ch;
    buf.cursor = [row, col + count - 1];
  }
  insert(buf, ch) {
    this.count = null;
    const [row, col] = buf.cursor, line = buf.lines[row];
    if (ch === 'a' && line.length) buf.cursor = [row, col + 1];
    else if (ch === 'I') buf.cursor = [row, firstNonBlank(line)];
    else if (ch === 'A') buf.cursor = [row, line.length];
    else if (ch === 'o') buf.insert([row, line.length], '\n');
    else if (ch === 'O') { buf.insert([row, 0], '\n'); buf.cursor = [row, 0]; }
    this._mode = 'insert';
  }
  undo(buf) { const e = this.undoStack.pop(); if (e) { this.restore(buf, e.before); this.redoStack.push(e); } }
  redo(buf) { const e = this.redoStack.pop(); if (e) { this.restore(buf, e.after); this.undoStack.push(e); } }
  /** Replay the last change as one undo step; `typeInsert` types Insert-mode keys. */
  repeat(buf, count) {
    let keys = [...this.lastChange];
    if (!keys.length) return;
    if (count !== null) {
      while (keys.length && /^[0-9]$/.test(keys[0].key)) keys.shift();
      keys = [...String(count)].map(key => ({ key })).concat(keys);
    }
    const before = this.snapshot(buf);
    this.replaying = true;
    for (const ev of keys) {
      if (this._mode === 'insert' && ev.key !== 'Escape') typeKey(buf, ev);
      else this.key(buf, ev);
    }
    if (this._mode === 'insert') this.key(buf, { key: 'Escape' });
    this.replaying = false;
    this.record(before, buf);
  }
  visualCommand(buf, ch) {
    if (this.digit(ch)) return;
    if (this.prefix === 'g') { this.prefix = null; if (ch === 'g') this.move(buf, 'fileStart', this.takeCountOrNull()); return; }
    const linewise = this._mode === 'vline';
    if (ch === 'g') this.prefix = 'g';
    else if (ch === 'v' || ch === 'V') { const want = ch === 'v' ? 'visual' : 'vline'; this._mode = this._mode === want ? 'normal' : want; }
    else if (ch === 'o') { const c = buf.cursor; buf.cursor = this.anchor; this.anchor = c; }
    else if ('dxcsy'.includes(ch)) { this._mode = 'normal'; this.visualOp(buf, 'dx'.includes(ch) ? 'delete' : 'cs'.includes(ch) ? 'change' : 'yank', linewise); }
    else if (ch === 'p' || ch === 'P') {
      const register = this.register, wasLinewise = this.linewise, top = Math.min(this.anchor[0], buf.cursor[0]);
      this._mode = 'normal';
      this.visualOp(buf, 'delete', linewise);
      this.register = register; this.linewise = wasLinewise;
      this.put(buf, !linewise || buf.cursor[0] >= top, 1);
    } else if (MOTIONS[ch]) this.move(buf, MOTIONS[ch], this.takeCountOrNull());
    this.count = null;
  }
  visualOp(buf, op, linewise) {
    const [a, b] = order(this.anchor, buf.cursor);
    if (linewise) { this.rows(buf, op, a[0], b[0]); if (op === 'yank') buf.cursor = [a[0], 0]; }
    else this.charsOp(buf, op, a, [b[0], Math.min(b[1] + 1, buf.lines[b[0]].length)]);
  }
}

/** Type an Insert-mode key into the buffer (what the textarea does natively). */
export function typeKey(buf, ev) {
  if (ev.key === 'Enter') buf.insert(buf.cursor, '\n');
  else if (ev.key === 'Backspace') { const p = prev(buf.lines, buf.cursor); if (p) buf.remove(p, buf.cursor); }
  else if (ev.key === 'Tab') buf.insert(buf.cursor, '  ');
  else if (ev.key.length === 1 || [...ev.key].length === 1) buf.insert(buf.cursor, ev.key);
}
