// Comment-thread pieces shared by the doc and sheet sidebars: message text,
// author rows, the activity line while nexus works on a thread, and the reply
// box. Everything is built as DOM nodes; nothing is interpreted as HTML.
import { inlineTokens, markdownTokens } from './helpers.js';
import { glyph } from './doc/glyphs.js';

export const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
export const KNOWN = new Set(['queued', 'working', 'progress', 'done', 'failed', 'stopped']);
export const busy = activity => ['queued', 'working', 'progress'].includes(activity?.state);
export const SEVERITY = { must: 'Must fix', should: 'Should fix', consider: 'Consider' };

/** Inline Markdown as DOM nodes. Links stay text; nothing is interpreted as HTML. */
export function rich(text) {
  const fragment = document.createDocumentFragment();
  for (const token of inlineTokens(text)) fragment.append(el(['strong', 'em', 'code'].includes(token.kind) ? token.kind : 'span', '', token.text));
  return fragment;
}
export function paragraphs(text) {
  const out = el('div', 'rich');
  for (const line of markdownTokens(text)) {
    const p = el(['h1', 'h2', 'h3'].includes(line.kind) ? 'strong' : 'p');
    p.append(rich(line.text));
    out.append(p);
  }
  return out;
}

/** A small icon action for a card's header row. */
export function action(name, label, run, className = '') {
  const b = el('button', `icon-button quiet card-action ${className}`.trim());
  b.type = 'button';
  b.append(glyph(name));
  b.title = label;
  b.setAttribute('aria-label', label);
  b.addEventListener('click', e => { e.stopPropagation(); run(); });
  return b;
}

export function author(message) {
  const row = el('div', `author ${message.role}`);
  const name = el('span', 'name', message.role === 'model' ? 'nexus' : 'You');
  name.title = new Date(message.created * 1000).toLocaleString();
  row.append(el('span', 'd-avatar', message.role === 'model' ? '✦' : '❯'), name);
  return row;
}

/**
 * Follow the activity of every thread. `ctx` holds `state` (with `activities`,
 * `activeThread`), `progress`, `doneUntil` and `threadOf`; `notify(thread, text)`
 * announces a finished run and `refresh` redraws the cards.
 */
export function trackActivities(ctx, next, { notify, refresh }) {
  if (!next) return;
  const { state, progress, doneUntil } = ctx;
  const before = state.activities;
  state.activities = Object.fromEntries(Object.entries(next).flatMap(([id, a]) => KNOWN.has(a.state) ? [[id, a]] : before[id] ? [[id, before[id]]] : []));
  for (const [id, activity] of Object.entries(state.activities)) {
    const prior = before[id];
    if (['queued', 'working'].includes(activity.state) && prior?.state !== activity.state) { progress[id] = []; delete doneUntil[id]; }
    if (activity.state === 'progress' && activity.detail && !progress[id]?.includes(activity.detail)) (progress[id] ||= []).push(activity.detail);
    if (['done', 'failed', 'stopped'].includes(activity.state) && prior?.state !== activity.state) {
      if (activity.state === 'done') { doneUntil[id] = Date.now() + 4000; setTimeout(refresh, 4000); }
      const thread = ctx.threadOf(id);
      if (thread && state.activeThread !== id) notify(thread, `nexus ${activity.detail || (activity.state === 'done' ? 'finished' : activity.state)}`);
    }
  }
}

/** The line under a card while nexus works on its thread, or after it finished. `ctx.undo` and `ctx.undoPattern` (which finished details offer Undo) are optional. */
export function activityRow(thread, ctx) {
  const { state, progress, command } = ctx;
  const activity = state.activities[thread.id];
  if (!activity) return null;
  const row = el('div', `activity ${activity.state}`);
  row.setAttribute('role', 'status');
  row.setAttribute('aria-live', 'polite');
  const label = el('div', 'd-activity-label');
  if (busy(activity)) label.append(el('span', 'd-activity-dot'));
  if (activity.state === 'done') label.append(glyph('check'));
  const waiting = { draft: 'Waiting — you have a draft in nexus', streaming: 'Waiting — nexus is answering something else', no_model: 'Waiting — no model selected' };
  label.append(el('span', '', activity.state === 'queued' ? waiting[activity.reason] || 'Waiting for nexus' : ['working', 'progress'].includes(activity.state) ? 'nexus is working…' : activity.state === 'done' ? 'Done' : activity.state === 'stopped' ? 'Stopped' : 'Failed'));
  row.append(label);
  if (['working', 'progress'].includes(activity.state)) for (const line of progress[thread.id] || []) row.append(el('div', 'activity-line', line));
  if (!busy(activity) && activity.detail) row.append(paragraphs(activity.detail));
  if (activity.state === 'done' && ctx.undo && (ctx.undoPattern || /^edited (blocks?|slides?) /).test(activity.detail || '')) {
    const undo = el('button', 'quiet', 'Undo');
    undo.type = 'button';
    undo.addEventListener('click', () => ctx.undo());
    row.append(undo);
  }
  if (['failed', 'stopped'].includes(activity.state)) {
    const retry = el('button', 'quiet', 'Send again');
    retry.type = 'button';
    retry.disabled = !state.linked;
    retry.addEventListener('click', () => {
      const message = thread.messages.findLast(m => m.role === 'you');
      if (message) command({ command: 'CommentReply', thread: thread.id, text: message.text, send: true });
    });
    row.append(retry);
  }
  return row;
}

/** The reply field under the open thread; `ctx.grow` runs after the field resizes. */
export function replyBox(thread, ctx) {
  const { state, command } = ctx;
  const wrap = el('div', 'reply');
  const area = el('textarea');
  area.rows = 1; area.maxLength = 2000; area.placeholder = 'Reply…';
  area.setAttribute('aria-label', 'Reply to thread');
  area.value = state.drafts[thread.id] || '';
  area.addEventListener('input', () => { state.drafts[thread.id] = area.value; area.style.height = 'auto'; area.style.height = `${area.scrollHeight}px`; ctx.grow?.(); });
  const send = toNexus => {
    if (!area.value.trim()) { area.focus(); return; }
    const text = area.value;
    delete state.drafts[thread.id];
    command({ command: 'CommentReply', thread: thread.id, text, send: toNexus }, e => { if (e.event === 'Error') state.drafts[thread.id] = text; });
  };
  area.addEventListener('keydown', e => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); send(state.linked); } });
  const note = el('button', 'small quiet', 'Reply'), ask = el('button', 'small primary-quiet', 'Send to nexus');
  note.type = ask.type = 'button';
  ask.prepend(glyph('send'));
  ask.disabled = !state.linked;
  ask.title = state.linked ? 'Send this reply to the conversation' : 'Not linked to a conversation — open it from nexus with /studio';
  note.addEventListener('click', () => send(false));
  ask.addEventListener('click', () => send(true));
  const actions = el('div', 'reply-actions');
  actions.append(note, ask);
  wrap.append(area, actions);
  return wrap;
}
