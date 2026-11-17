// Dugix component library: real renderer exports, painted like a terminal.
import { metrics, paintFrame } from './terminal-paint.js';

const $ = selector => document.querySelector(selector);
const esc = value => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const FONT = 'Menlo, "SF Mono", Consolas, monospace';
const state = { selected: null, query: '', size: 14 };
let sheet;

function title(item) { return item.names[0]; }
function covers(item) { return item.names.length > 1 ? item.names.slice(1).join(', ') : ''; }

function renderList() {
  const query = state.query.trim().toLowerCase();
  const groups = [...new Set(sheet.cases.map(item => item.group))];
  $('#list').innerHTML = groups.map(group => {
    const items = sheet.cases.filter(item => item.group === group && (!query || `${item.names.join(' ')} ${group}`.toLowerCase().includes(query)));
    if (!items.length) return '';
    return `<div class="group">${esc(group)}</div>${items.map(item => `<button class="item" data-id="${item.id}" aria-current="${state.selected === item.id}">${esc(title(item))}${covers(item) ? ` <small>· ${esc(covers(item))}</small>` : ''}</button>`).join('')}`;
  }).join('');
  $('#show-all').setAttribute('aria-pressed', String(!state.selected));
  $('#count').textContent = `${sheet.contracts.length}`;
}

function canvasFor(frame) {
  const canvas = document.createElement('canvas');
  paintFrame(canvas, frame, metrics(state.size, FONT));
  return canvas;
}

function renderAll() {
  $('#stage').innerHTML = `<h1>All components</h1>
    <p class="lead">${sheet.cases.length} panels covering all ${sheet.contracts.length} dugix-ui contracts, drawn by Dugix's real panel renderer at 76 columns in a Tokyo Night terminal theme. Select any panel for its focused state, a 44-column render and its source.</p>
    <div class="sheet" id="sheet"></div>`;
  // Cards fit a whole 76-column panel at the chosen cell size.
  $('#sheet').style.setProperty('--card-min', `${Math.ceil(76 * metrics(state.size, FONT).width) + 30}px`);
  for (const item of sheet.cases) {
    const card = document.createElement('article');
    card.className = 'card'; card.dataset.id = item.id;
    card.innerHTML = `<header><strong>${esc(title(item))}</strong><span>${esc(item.group)}${covers(item) ? ` · ${esc(covers(item))}` : ''}</span></header><div class="screen"></div>`;
    card.querySelector('.screen').append(canvasFor(item.frames['wide/default']));
    $('#sheet').append(card);
  }
}

function renderOne(item) {
  const signatures = sheet.contracts.filter(contract => item.names.includes(contract.name)).map(contract => contract.signature);
  $('#stage').innerHTML = `<div class="detail">
    <div><h1>${esc(title(item))}</h1><p class="lead">${esc(item.note || `${item.group}${covers(item) ? ` · also covers ${covers(item)}` : ''}`)}</p></div>
    <div class="views" id="views"></div>
    ${signatures.length ? `<pre>${esc(signatures.join('\n'))}</pre>` : ''}
    <pre>${esc(item.source)}</pre>
    <p class="note">Rendered by the Rust panel renderer (Entry::canvas) and exported cell by cell. Focused shows keyboard focus on the first control.</p>
  </div>`;
  for (const [key, label] of [['wide/default', '76 columns'], ['wide/focused', '76 columns · focused'], ['narrow/default', '44 columns']]) {
    const view = document.createElement('section');
    view.className = 'view';
    view.innerHTML = `<h2>${label}</h2><div class="screen"></div>`;
    view.querySelector('.screen').append(canvasFor(item.frames[key]));
    $('#views').append(view);
  }
}

function render() {
  renderList();
  const item = sheet.cases.find(entry => entry.id === state.selected);
  if (item) renderOne(item); else renderAll();
  const url = new URL(location.href);
  if (state.selected) url.hash = state.selected; else url.hash = '';
  history.replaceState(null, '', url);
}

document.addEventListener('click', event => {
  const target = event.target.closest('[data-id]');
  if (target) { state.selected = target.dataset.id; render(); $('main').scrollTop = 0; return; }
  if (event.target.closest('#show-all')) { state.selected = null; render(); return; }
  const size = event.target.closest('[data-size]');
  if (size) {
    state.size = Number(size.dataset.size);
    document.querySelectorAll('[data-size]').forEach(button => button.setAttribute('aria-pressed', String(button === size)));
    render();
  }
});
$('#search').addEventListener('input', event => { state.query = event.target.value; renderList(); });

try {
  const response = await fetch('./component-sheet-frames.json');
  if (!response.ok) throw new Error('Export the sheet first: scripts/preview-terminal-panels.sh --export-only');
  sheet = await response.json();
  const hash = location.hash.slice(1);
  state.selected = sheet.cases.some(item => item.id === hash) ? hash : null;
  render();
} catch (error) {
  $('#stage').innerHTML = `<pre>${esc(error.message)}</pre>`;
}
