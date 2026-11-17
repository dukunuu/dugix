// Tree-sitter colors for code blocks. The backend returns spans per block; they
// are painted with the CSS Custom Highlight API, which colors DOM ranges without
// touching the editable DOM, so typing, the caret and undo never notice.

const KEYS = ['accent', 'accent2', 'accent3', 'muted', 'fg'];
const QUIET_MS = 120;

/** Text nodes of `root` with the UTF-16 offset each one starts at. */
function textNodes(root) {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const nodes = [];
  let at = 0;
  for (let node = walker.nextNode(); node; node = walker.nextNode()) {
    nodes.push({ node, at });
    at += node.data.length;
  }
  return nodes;
}

/** The DOM position of UTF-16 offset `offset` among `nodes`. */
function locate(nodes, offset) {
  for (let i = nodes.length - 1; i >= 0; i--) {
    if (nodes[i].at <= offset) return [nodes[i].node, Math.min(offset - nodes[i].at, nodes[i].node.data.length)];
  }
  return null;
}

/** Paints every code block under `surface`; `ask` sends a backend command. */
export function syntaxController({ surface, ask }) {
  const supported = typeof CSS !== 'undefined' && CSS.highlights && typeof Highlight === 'function';
  /** Block id -> { key: lang + text it was painted for, ranges: [key, Range][] }. */
  const painted = new Map();
  let timer = 0, running = false, again = false;

  const blocks = () => [...surface.querySelectorAll('.blk-code[data-block]')].map(wrap => ({
    id: wrap.dataset.block,
    lang: wrap.querySelector('.code-head .lang')?.textContent.trim() || '',
    pre: wrap.querySelector('pre[data-part="text"]'),
  })).filter(b => b.pre);

  function paint() {
    for (const key of KEYS) {
      const ranges = [];
      for (const entry of painted.values()) for (const [k, range] of entry.ranges) if (k === key) ranges.push(range);
      if (ranges.length) CSS.highlights.set(`hl-${key}`, new Highlight(...ranges));
      else CSS.highlights.delete(`hl-${key}`);
    }
  }

  async function refresh() {
    if (running) { again = true; return; }
    running = true;
    try {
      const present = blocks();
      const ids = new Set(present.map(b => b.id));
      for (const id of painted.keys()) if (!ids.has(id)) painted.delete(id);
      const stale = present
        .map(b => ({ ...b, text: b.pre.textContent }))
        .filter(b => painted.get(b.id)?.key !== `${b.lang}\n${b.text}`);
      if (stale.length) {
        const reply = await ask({ command: 'Highlight', blocks: stale.map(({ id, lang, text }) => ({ id, lang, text })) });
        for (const { id, spans } of reply?.blocks || []) {
          const block = stale.find(b => b.id === id);
          // Typing since the request? The next pass paints the newer text.
          if (!block || block.pre.textContent !== block.text) continue;
          const nodes = textNodes(block.pre);
          const ranges = [];
          for (const [start, end, key] of spans) {
            if (key === 'fg') continue;
            const from = locate(nodes, start), to = locate(nodes, end);
            if (!from || !to) continue;
            const range = document.createRange();
            range.setStart(...from);
            range.setEnd(...to);
            ranges.push([key, range]);
          }
          painted.set(id, { key: `${block.lang}\n${block.text}`, ranges });
        }
      }
      paint();
    } catch { /* highlighting is decoration; a failed request leaves code plain */ }
    running = false;
    if (again) { again = false; schedule(); }
  }

  function schedule() {
    clearTimeout(timer);
    timer = setTimeout(refresh, QUIET_MS);
  }

  if (!supported) return { refresh() {}, stop() {} };
  const observer = new MutationObserver(schedule);
  observer.observe(surface, { subtree: true, childList: true, characterData: true });
  schedule();
  return {
    refresh: schedule,
    stop() {
      observer.disconnect();
      clearTimeout(timer);
      painted.clear();
      for (const key of KEYS) CSS.highlights.delete(`hl-${key}`);
    },
  };
}
