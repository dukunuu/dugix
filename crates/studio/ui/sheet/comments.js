// Comments on cells: a pin in the corner of each anchored cell, a composer anchored to the
// selection, and the thread cards in the sidebar. The cards reuse the doc's thread pieces
// (../threads.js); a thread whose cells were deleted is listed there only.
import * as M from './model.js';
import { glyph } from './glyphs.js';
import { el, busy, SEVERITY, paragraphs, action, author, trackActivities, activityRow as sharedActivityRow, replyBox as sharedReplyBox } from '../threads.js';

export function commentsController(api) {
  const { state } = api;
  const progress = {}, doneUntil = {};
  let showResolved = false, composer = null;
  state.activeThread = null;
  state.draft = null;

  const threadOf = id => state.threads.find(t => t.id === id);
  const tabById = id => state.sheet?.tabs.find(t => t.id === id || t.name === id);
  const where = thread => { const tab = tabById(thread.anchor.tab); return `${tab ? tab.name : 'Deleted sheet'}!${thread.anchor.range}`; };

  /** Pins of the open threads on the tab showing: the top-left cell of each anchor. */
  function pins() {
    const map = new Map();
    for (const thread of state.threads) {
      if (thread.resolved || thread.orphaned || thread.anchor.tab !== state.tabId) continue;
      const r = M.anchorRect(thread);
      if (!r) continue;
      const key = `${r.r1},${r.c1}`;
      (map.get(key) || map.set(key, []).get(key)).push(thread.id);
    }
    return map;
  }

  // -- starting a thread -------------------------------------------------------------------------------

  function closeComposer() { composer?.remove(); composer = null; state.draft = null; }
  /** Open the composer under the selection. */
  function begin() {
    if (!state.sel) return;
    api.editor.commit();
    closeComposer();
    const r = M.opRect(state.sel, M.usedRect(api.tab())), top = { row: r.r1, col: r.c1 };
    state.draft = { tab: api.tabName(), range: M.rectStr(r), quote: api.displayAt(top.row, top.col), text: '' };
    const draft = state.draft;
    const node = el('form', 'popup sg-composer card draft active');
    node.setAttribute('aria-label', 'New comment');
    const head = el('div', 'card-head'), who = el('div', 'author you');
    who.append(el('span', 'd-avatar', '❯'), el('span', 'name', 'You'));
    const chip = el('span', 'sg-ref static', `${draft.tab}!${draft.range}`);
    head.append(who, chip, el('span', 'spacer'), action('x', 'Cancel comment (Esc)', () => { closeComposer(); api.grid.focus(); }));
    node.append(head);
    if (draft.quote) node.append(el('blockquote', 'quote', draft.quote.length > 90 ? `${draft.quote.slice(0, 89)}…` : draft.quote));
    const area = el('textarea');
    area.rows = 2; area.maxLength = 2000; area.placeholder = 'Add a comment or request a change…';
    area.setAttribute('aria-label', 'Comment');
    area.addEventListener('input', () => { draft.text = area.value; });
    const submit = send => {
      if (!area.value.trim()) { area.focus(); return; }
      api.command({ command: 'SheetCommentCreate', tab: draft.tab, range: draft.range, text: area.value, send }, e => {
        if (e.event === 'Error') return;
        closeComposer();
        state.activeThread = state.threads.at(-1)?.id ?? null;
        api.openSidebar();
        render();
        api.grid.focus();
      });
    };
    area.addEventListener('keydown', e => {
      if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeComposer(); api.grid.focus(); }
      else if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); submit(state.linked); }
    });
    const note = el('button', 'small quiet', 'Comment'), ask = el('button', 'small primary-quiet', 'Send to nexus');
    note.type = ask.type = 'button';
    ask.prepend(glyph('send'));
    ask.disabled = !state.linked;
    ask.title = state.linked ? 'Send this comment to the conversation' : 'Not linked to a conversation — open it from nexus with /studio';
    note.addEventListener('click', () => submit(false));
    ask.addEventListener('click', () => submit(true));
    const actions = el('div', 'reply-actions');
    actions.append(note, ask);
    node.append(area, actions);
    node.addEventListener('submit', e => e.preventDefault());
    api.overlay.append(node);
    composer = node;
    const rect = api.grid.screenRect(state.sel.active.row, state.sel.active.col);
    api.place(node, rect);
    area.focus();
  }

  // -- activity ---------------------------------------------------------------------------------------------------

  function updateActivities(next) {
    if (!next) return;
    trackActivities({ state, progress, doneUntil, threadOf }, next, { notify: (thread, text) => api.toast(text, [['View', () => open(thread.id)]]), refresh: render });
    api.renderLink();
  }
  const ctx = () => ({ state, progress, command: api.command, undo: api.undo, undoPattern: /^edited (cells? |\d+ cells)/ });

  // -- cards ---------------------------------------------------------------------------------------------------------

  const summarize = edit => `${edit.ops.length} change${edit.ops.length === 1 ? '' : 's'}: ${[...new Set(edit.ops.map(o => String(o.op).replaceAll('_', ' ')))].join(', ')}`;
  function editCard(thread, edit) {
    const card = el('div', `edit ${edit.status}`);
    if (edit.status !== 'pending') {
      card.append(el('span', 'edit-state', edit.status === 'applied' ? 'Applied · undo with ⌘Z' : edit.status === 'outdated' ? 'Outdated — the sheet changed since' : 'Dismissed'));
      return card;
    }
    card.append(el('div', 'diff-line muted', summarize(edit)));
    const actions = el('div', 'edit-actions');
    actions.append(
      action('check', 'Apply suggestion', () => api.command({ command: 'ApplyEdit', owner: thread.id, edit: edit.id }), 'accept'),
      action('x', 'Dismiss suggestion', () => api.command({ command: 'DismissEdit', owner: thread.id, edit: edit.id })),
    );
    card.prepend(actions);
    card.classList.add('with-actions');
    return card;
  }
  function threadCard(thread) {
    const active = state.activeThread === thread.id;
    const card = el('article', `card${active ? ' active' : ''}${thread.resolved ? ' resolved' : ''}${thread.messages[0]?.role === 'model' ? ' model' : ''}`);
    card.dataset.thread = thread.id;
    card.tabIndex = 0;
    card.setAttribute('aria-label', `Comment thread: ${thread.messages[0]?.text.slice(0, 60) ?? ''}`);
    const activity = state.activities[thread.id];
    if (busy(activity)) card.classList.add('busy');
    if (doneUntil[thread.id] > Date.now()) card.classList.add('finished');
    const head = el('div', 'card-head');
    head.append(author(thread.messages[0]));
    if (thread.messages.some(m => m.role === 'model') && !thread.resolved) head.append(el('span', `d-severity ${thread.severity}`, SEVERITY[thread.severity] || thread.severity));
    const chip = el(thread.orphaned ? 'span' : 'button', 'sg-ref', where(thread));
    if (!thread.orphaned) { chip.type = 'button'; chip.title = 'Go to these cells'; chip.addEventListener('click', e => { e.stopPropagation(); open(thread.id); }); }
    head.append(chip, el('span', 'spacer'), action(thread.resolved ? 'redo' : 'check', thread.resolved ? 'Reopen thread' : 'Resolve thread', () => api.command({ command: thread.resolved ? 'CommentReopen' : 'CommentResolve', thread: thread.id })));
    card.append(head);
    if (thread.orphaned) card.append(el('div', 'quote muted', 'The commented cells were deleted'));
    else if (thread.anchor.quote) card.append(el('blockquote', 'quote', thread.anchor.quote.length > 90 ? `${thread.anchor.quote.slice(0, 89)}…` : thread.anchor.quote));
    const shown = active ? thread.messages : thread.messages.slice(0, 1);
    shown.forEach((message, i) => {
      const body = el('div', 'message');
      if (i > 0) body.append(author(message));
      body.append(paragraphs(message.text));
      for (const edit of message.edits) body.append(editCard(thread, edit));
      card.append(body);
    });
    if (!active && thread.messages.length > 1) card.append(el('div', 'more', `${thread.messages.length - 1} ${thread.messages.length === 2 ? 'reply' : 'replies'}`));
    const status = sharedActivityRow(thread, ctx());
    if (status) card.append(status);
    if (active && !thread.resolved) card.append(sharedReplyBox(thread, ctx()));
    card.addEventListener('click', e => { if (!e.target.closest('button, textarea')) open(thread.id); });
    return card;
  }

  // -- the sidebar ----------------------------------------------------------------------------------------------------

  function render() {
    const root = api.sidebarPane;
    root.replaceChildren();
    const open1 = state.threads.filter(t => !t.resolved), done = state.threads.filter(t => t.resolved);
    api.setCommentCount(open1.length);
    const track = el('div', 'margin-track');
    root.append(track);
    for (const thread of open1) track.append(threadCard(thread));
    if (!open1.length) track.append(el('p', 'side-note', 'No open comments. Select cells and press ⌥⌘M, or use the comment button in the toolbar.'));
    if (done.length) {
      const toggle = el('button', 'resolved-fold quiet');
      toggle.type = 'button';
      toggle.setAttribute('aria-expanded', String(showResolved));
      toggle.append(glyph('down'), document.createTextNode(`Resolved (${done.length})`));
      toggle.addEventListener('click', () => { showResolved = !showResolved; render(); });
      const list = el('div', 'resolved-list');
      list.hidden = !showResolved;
      if (showResolved) for (const thread of done) list.append(threadCard(thread));
      track.append(toggle, list);
    }
  }
  /** Open a thread: its card in the sidebar, its cells selected and in view. */
  function open(id) {
    const thread = threadOf(id);
    if (!thread) return;
    api.openSidebar();
    state.activeThread = id;
    const r = M.anchorRect(thread), tab = tabById(thread.anchor.tab);
    if (r && tab) {
      api.switchTab(tab.id);
      api.select({ kind: 'cells', anchor: { row: r.r1, col: r.c1 }, head: { row: r.r2, col: r.c2 }, active: { row: r.r1, col: r.c1 } });
    }
    render();
    api.sidebarPane.querySelector(`.card[data-thread="${CSS.escape(id)}"]`)?.scrollIntoView({ block: 'nearest' });
  }
  /** A pin was clicked: the key is `row,col` of its cell. */
  function openAt(key) {
    const id = pins().get(key)?.[0];
    if (id) open(id);
  }
  return { begin, render, open, openAt, closeComposer, updateActivities, pins };
}
