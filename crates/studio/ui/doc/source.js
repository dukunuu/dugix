// The doc as Markdown, in a code editor: vim keys (on by default, toggled in the
// bar and remembered here) and tree-sitter colors from the backend. The text
// is the lossless Markdown form, so edits made here come back as the same
// blocks; they are applied after a pause, each as one undo step.

import { Buffer, Vim } from './vim.js';

const APPLY_MS = 700, COLOR_MS = 120;
const VIM_KEY = 'dugix.studio.sourceVim';

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text != null) node.textContent = text;
  return node;
}
function readVim() {
  try { return localStorage.getItem(VIM_KEY) !== 'off'; } catch { return true; }
}

/** The source view; `ask` sends a backend command and resolves with its reply. */
export function sourceController({ ask }) {
  const pane = el('div', 'source-pane');
  pane.hidden = true;
  const bar = el('div', 'source-bar');
  const title = el('span', 'source-title', 'Markdown source');
  const mode = el('span', 'source-mode');
  const status = el('span', 'source-status');
  status.setAttribute('role', 'status');
  const vimToggle = el('button', 'quiet source-vim');
  vimToggle.type = 'button';
  vimToggle.title = 'Vim keys in the source view';
  const done = el('button', 'quiet', 'Back to editing');
  done.type = 'button';
  bar.append(title, mode, status, vimToggle, done);
  const wrap = el('div', 'source-wrap');
  const colors = el('pre', 'source-colors');
  colors.setAttribute('aria-hidden', 'true');
  const input = el('textarea', 'source-input');
  input.spellcheck = false;
  input.setAttribute('aria-label', 'Markdown source');
  input.setAttribute('autocapitalize', 'off');
  input.setAttribute('autocomplete', 'off');
  wrap.append(colors, input);
  pane.append(bar, wrap);

  const vim = new Vim();
  let vimOn = readVim(), buf = null, caret = -1, spans = [], applyTimer = 0, colorTimer = 0, applying = null, dirty = false;
  /** Vim's cursor block or Visual selection, painted in the colored layer. */
  let mark = null;

  function renderMode() {
    mode.textContent = vimOn ? vim.label() : '';
    mode.classList.toggle('insert', vim.mode === 'insert');
    vimToggle.textContent = vimOn ? 'vim on' : 'vim off';
    vimToggle.setAttribute('aria-pressed', String(vimOn));
  }
  /**
   * Repaint the colored copy under the textarea: syntax spans (stale ones are
   * clipped), vim's cursor or selection, and a marker where it starts.
   */
  function paint() {
    const text = input.value, n = text.length;
    const cuts = new Set([0, n]);
    for (const [start, end] of spans) { cuts.add(Math.min(start, n)); cuts.add(Math.min(end, n)); }
    if (mark) { cuts.add(Math.min(mark[0], n)); cuts.add(Math.min(mark[1], n)); }
    const points = [...cuts].sort((a, b) => a - b);
    const out = document.createDocumentFragment();
    let next = 0;
    for (let i = 0; i < points.length - 1; i++) {
      const a = points[i], b = points[i + 1];
      if (mark && a === mark[0]) out.append(el('span', 'source-at'));
      while (next < spans.length && spans[next][1] <= a) next++;
      const span = next < spans.length && spans[next][0] <= a ? spans[next] : null;
      const classes = [span && `hl-${span[2]}`, mark && a >= mark[0] && b <= mark[1] && 'source-mark'].filter(Boolean);
      const chunk = text.slice(a, b);
      out.append(classes.length ? el('span', classes.join(' '), chunk) : chunk);
    }
    if (mark && mark[0] >= n) out.append(el('span', 'source-at'));
    // A trailing space keeps a final empty line as tall as the textarea's.
    out.append(' ');
    colors.replaceChildren(out);
    colors.scrollTop = input.scrollTop;
  }
  /** Scroll so the vim cursor is on screen. */
  function reveal() {
    const at = colors.querySelector('.source-at');
    if (!at) return;
    const lh = parseFloat(getComputedStyle(input).lineHeight) || 20, pad = 24;
    if (at.offsetTop < input.scrollTop + pad) input.scrollTop = Math.max(at.offsetTop - pad, 0);
    else if (at.offsetTop + lh > input.scrollTop + input.clientHeight - pad) input.scrollTop = at.offsetTop + lh - input.clientHeight + pad;
    colors.scrollTop = input.scrollTop;
  }
  async function recolor() {
    const text = input.value;
    try {
      const reply = await ask({ command: 'Highlight', blocks: [{ id: 'source', lang: 'markdown', text }] });
      if (input.value !== text) return;
      spans = (reply?.blocks?.[0]?.spans || []).filter(([, , key]) => key !== 'fg');
      paint();
    } catch { /* colors are decoration */ }
  }
  function changed() {
    dirty = true;
    paint();
    clearTimeout(colorTimer); colorTimer = setTimeout(recolor, COLOR_MS);
    clearTimeout(applyTimer); applyTimer = setTimeout(flush, APPLY_MS);
  }
  /** Send the text now; resolves once the doc has it (or the error is shown). */
  async function flush() {
    clearTimeout(applyTimer);
    if (!dirty) return applying;
    dirty = false;
    status.textContent = 'Saving…';
    applying = ask({ command: 'DocApplySource', markdown: input.value })
      .then(() => { status.textContent = dirty ? 'Saving…' : 'Saved'; })
      .catch(error => { status.textContent = `Not applied: ${error.message}`; });
    return applying;
  }

  /** Show the vim cursor: a one-character block in Normal mode, the selection in Visual. */
  function showCursor() {
    if (!buf) return;
    const range = vim.visualRange(buf);
    let start, end;
    if (range) { start = buf.offsetOf(range[0]); end = buf.offsetOf(range[1]); }
    else {
      start = buf.offsetOf(buf.cursor);
      const line = buf.lines[buf.cursor[0]];
      end = vim.mode !== 'insert' && buf.cursor[1] < line.length ? start + line[buf.cursor[1]].length : start;
    }
    input.setSelectionRange(start, end);
    caret = start;
    const vimView = vimOn && vim.mode !== 'insert';
    input.classList.toggle('vim-view', vimView);
    mark = vimView ? [start, end] : null;
    paint();
    if (vimView) reveal();
  }
  function sync() {
    const text = input.value;
    if (!buf || buf.text() !== text) buf = new Buffer(text, input.selectionStart);
    else if (input.selectionStart !== caret) buf.cursor = buf.posOf(input.selectionStart);
  }

  input.addEventListener('keydown', e => {
    if (!vimOn || e.isComposing) return;
    sync();
    const before = buf.text();
    const out = vim.key(buf, { key: e.key, ctrlKey: e.ctrlKey, metaKey: e.metaKey, altKey: e.altKey });
    renderMode();
    if (out === 'passthrough') return;
    e.preventDefault();
    if (out === 'leave') return;
    if (buf.text() !== before) { input.value = buf.text(); changed(); }
    showCursor();
  });
  input.addEventListener('input', () => { buf = null; mark = null; input.classList.remove('vim-view'); changed(); });
  input.addEventListener('scroll', () => { colors.scrollTop = input.scrollTop; });
  input.addEventListener('mouseup', () => { if (vimOn && vim.mode !== 'insert') { sync(); showCursor(); } });
  vimToggle.addEventListener('click', () => {
    vimOn = !vimOn;
    try { localStorage.setItem(VIM_KEY, vimOn ? 'on' : 'off'); } catch { /* per-device convenience only */ }
    vim.reset();
    mark = null;
    input.classList.remove('vim-view');
    paint();
    renderMode();
    input.focus();
  });

  renderMode();
  return {
    pane,
    done,
    get open() { return !pane.hidden; },
    /** Load the doc's Markdown and show the editor, the caret at `offset`. */
    async show(offset = 0) {
      pane.hidden = false;
      status.textContent = 'Loading…';
      try {
        const reply = await ask({ command: 'DocSource' });
        input.value = reply.markdown;
        status.textContent = '';
      } catch (error) { status.textContent = error.message; }
      vim.reset(); buf = null; spans = []; dirty = false;
      const at = Math.min(offset, input.value.length);
      // Vim opens in Normal mode, as an editor does.
      if (vimOn) { buf = new Buffer(input.value, at); vim.key(buf, { key: 'Escape' }); }
      mark = null;
      paint(); recolor(); renderMode();
      input.focus();
      input.setSelectionRange(at, at);
      // Setting the value scrolled to the end; start where the caret is.
      input.scrollTop = 0;
      colors.scrollTop = 0;
      if (vimOn) showCursor();
    },
    /** Apply what is left and hide. */
    async hide() {
      await flush();
      pane.hidden = true;
      clearTimeout(colorTimer);
    },
    /** A change from elsewhere (the model, undo) reloads the text unless it is being edited. */
    onExternal() {
      if (!pane.hidden && !dirty) this.show(input.selectionStart);
    },
  };
}
