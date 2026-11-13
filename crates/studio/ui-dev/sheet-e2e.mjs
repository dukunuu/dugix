// Run: node crates/studio/ui-dev/sheet-e2e.mjs [test ...]
// Drives sheet mode in Chromium against the mock backend (sheet-mock.js) and asserts the
// behaviors that matter: typing, keyboard movement, point mode, ranges, clipboard, fill,
// resizing, header menus, sorting, tabs, formats, comments, focus, external edits and a
// 10,000-row sheet staying virtualized. Writes shots to shots/.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { browser, base, errors, shot, finish, libraryMock } from './doc-harness.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const sample = JSON.parse(fs.readFileSync(path.join(here, 'fixtures/sheet-sample.json'), 'utf8'));
const mock = fs.readFileSync(path.join(here, 'sheet-mock.js'), 'utf8');
const MOD = process.platform === 'darwin' ? 'Meta' : 'Control';

/** A big sheet: `rows` rows of an id, a name, a number and a formula. */
function bigSheet(rows) {
  const cells = { A1: { input: 'Id', fmt: { bold: true } }, B1: { input: 'Name', fmt: { bold: true } }, C1: { input: 'Amount', fmt: { bold: true } }, D1: { input: 'Double', fmt: { bold: true } } };
  for (let r = 2; r <= rows; r++) { cells[`A${r}`] = { input: String(r - 1) }; cells[`B${r}`] = { input: `Item ${r - 1}` }; cells[`C${r}`] = { input: String((r * 37) % 1000) }; cells[`D${r}`] = { input: `=C${r}*2` }; }
  return { sheet: { ...structuredClone(sample.sheet), title: 'Ledger', tabs: [{ id: 't1', name: 'Ledger', cells, frozen: { rows: 1, cols: 0 } }] }, threads: [] };
}

async function open(scheme = 'dark', viewport = { width: 1440, height: 900 }, { fixture = sample, latency, scripts = [] } = {}) {
  const page = await browser.newPage({ viewport, deviceScaleFactor: 1, colorScheme: scheme, reducedMotion: 'reduce' });
  page.setDefaultTimeout(6000);
  page.on('pageerror', error => errors.push(error.message));
  page.on('console', message => { if (message.type() === 'error') errors.push(message.text()); });
  await page.context().grantPermissions(['clipboard-read', 'clipboard-write']).catch(() => {});
  const colors = JSON.parse(fs.readFileSync(path.join(here, `fixtures/theme-${scheme === 'dark' ? 'default' : 'light'}.json`), 'utf8'));
  await page.addInitScript(([f, c, l]) => { window.__sheetFixture = f; window.__sheetColors = c; if (l !== undefined) window.__sheetLatency = l; }, [fixture, colors, latency]);
  await page.addInitScript({ content: mock });
  for (const content of scripts) await page.addInitScript({ content });
  await page.goto(base);
  await page.waitForSelector('#sheet-app .sg-cell');
  await page.waitForTimeout(150);
  return page;
}
const cell = (page, a1) => page.locator(`#sheet-app .sg-cell[data-r="${a1.row}"][data-c="${a1.col}"]`);
const at = (a1) => { const m = /^([A-Z]+)(\d+)$/.exec(a1); let col = 0; for (const ch of m[1]) col = col * 26 + ch.charCodeAt(0) - 64; return { row: Number(m[2]), col }; };
const A = (page, a1) => cell(page, at(a1));
const mockSheet = page => page.evaluate(() => window.__mock.sheet);
const tabOf = async (page, name = 'Budget') => (await mockSheet(page)).tabs.find(t => t.name === name);
const inputOf = async (page, a1, tab) => (await tabOf(page, tab)).cells[a1]?.input;
const commands = (page, name) => page.evaluate(n => window.__mock.commands.filter(c => !n || c.command === n), name);
const applies = page => commands(page, 'SheetApply');
const lastApply = async page => (await applies(page)).at(-1);
const settle = page => page.waitForTimeout(120);
/** Select a cell by clicking it. */
async function click(page, a1, options = {}) { await A(page, a1).click(options); }
const addr = page => page.locator('#sheet-app .sg-addr').inputValue();
const barText = page => page.locator('#sheet-app .sg-bar-text').inputValue();
const shown = async (page, a1) => (await A(page, a1).textContent()).trim();
const selection = page => page.evaluate(() => document.querySelector('#sheet-app .sg-addr').value);

const centerOf = async locator => { const b = await locator.boundingBox(); return { x: b.x + b.width / 2, y: b.y + b.height / 2, b }; };
/** Drag from the middle of one cell to the middle of another. */
async function drag(page, from, to, { shift = false } = {}) {
  const a = await centerOf(A(page, from)), z = await centerOf(A(page, to));
  await page.mouse.move(a.x, a.y);
  await page.mouse.down();
  await page.mouse.move((a.x + z.x) / 2, (a.y + z.y) / 2, { steps: 3 });
  await page.mouse.move(z.x, z.y, { steps: 3 });
  await page.mouse.up();
}
/** A paste event carrying `data` as the clipboard, as the system sends it. */
const pasteInto = (page, data) => page.evaluate(d => {
  const dt = new DataTransfer();
  for (const [type, value] of Object.entries(d)) dt.setData(type, value);
  document.activeElement.dispatchEvent(new ClipboardEvent('paste', { clipboardData: dt, bubbles: true, cancelable: true }));
}, data);
const copyEvent = (page, type = 'copy') => page.evaluate(t => {
  const dt = new DataTransfer();
  const e = new ClipboardEvent(t, { clipboardData: dt, bubbles: true, cancelable: true });
  document.activeElement.dispatchEvent(e);
  return { plain: dt.getData('text/plain'), html: dt.getData('text/html'), prevented: e.defaultPrevented };
}, type);
const rowCount = page => page.evaluate(() => new Set([...document.querySelectorAll('#sheet-app .sg-cell')].map(c => c.dataset.r)).size);
const tabsBar = page => page.locator('#sheet-app .sg-tab');

const tests = {
  async defaultThemeSheetOpens() {
    // A sheet on the default theme serializes without `theme`; it must still draw.
    const sheet = structuredClone(sample.sheet);
    delete sheet.theme;
    const page = await open('light', undefined, { fixture: { sheet, threads: [] } });
    assert.equal(await shown(page, 'A1'), 'Category');
    await page.close();
    // ...and the library opened from it lists the space's files.
    const items = [{ kind: 'sheet', relative: 'sheets/q3-budget.sheet.json', title: 'Q3 budget', modified: new Date().toISOString(), count: 57, design: null, thumb: null, current: true }, { kind: 'doc', relative: 'docs/notes.doc.json', title: 'Notes', modified: new Date().toISOString(), count: 3, design: null, thumb: null, current: false }];
    const lib = await open('light', undefined, { fixture: { sheet, threads: [] }, scripts: [`window.__libraryItems = ${JSON.stringify(items)}; window.__libraryCurrent = 'sheets/q3-budget.sheet.json';`, libraryMock] });
    // A mode event that throws is reported, and later events (the library's) still arrive.
    await lib.evaluate(() => window.studioAsyncEvent({ event: 'Sheet', sheet: null }));
    await lib.locator('.sg-toast', { hasText: 'Studio hit an error' }).waitFor();
    await lib.locator('#sheet-app .crumb-link').click();
    await lib.locator('#library .lib-card').first().waitFor();
    assert.equal(await lib.locator('#library .lib-card').count(), 2);
    await lib.close();
  },
  async charts() {
    const fixture = { sheet: { ...structuredClone(sample.sheet), tabs: [{ id: 't1', name: 'Budget', cells: { A1: { input: 'Month' }, B1: { input: 'Sales' }, A2: { input: 'Jan' }, B2: { input: '10' }, A3: { input: 'Feb' }, B3: { input: '20' }, A4: { input: 'Mar' }, B4: { input: '15' } } }] }, threads: [] };
    const page = await open('light', { width: 1440, height: 1000 }, { fixture });
    await click(page, 'A1'); await page.keyboard.down('Shift'); await click(page, 'B4'); await page.keyboard.up('Shift');
    await page.getByRole('button', { name: 'Insert chart', exact: true }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops, [{ op: 'add_chart', tab: 'Budget', data: 'A1:B4' }]);
    const first = page.locator('.sg-chart[data-chart="c1"]');
    assert.equal(await first.count(), 1); assert.equal(await first.locator('svg').count(), 1);
    assert.equal(await page.getByRole('complementary', { name: 'Chart editor' }).isVisible(), true);
    const start = await first.boundingBox(), count = (await applies(page)).length;
    await page.mouse.move(start.x + 100, start.y + 50); await page.mouse.down();
    await page.mouse.move(start.x + 200, start.y + 98, { steps: 5 }); await page.mouse.up(); await settle(page);
    assert.equal((await applies(page)).length, count + 1, 'one drag is one edit');
    assert.equal((await lastApply(page)).ops[0].patch.at, 'E3');
    const handle = first.locator('[data-resize="se"]'), h = await centerOf(handle);
    await page.mouse.move(h.x, h.y); await page.mouse.down(); await page.mouse.move(h.x + 40, h.y + 24, { steps: 4 }); await page.mouse.up(); await settle(page);
    assert.equal((await lastApply(page)).ops[0].patch.w, 520); assert.equal((await lastApply(page)).ops[0].patch.h, 320);
    await page.locator('.sg-chart-types [data-kind="line"]').click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { kind: 'line' });
    await shot(page, 'sheet-chart-editor');
    await page.getByRole('button', { name: 'Close chart editor', exact: true }).click();
    await click(page, 'A1', { button: 'right' });
    await page.getByRole('menuitem', { name: 'Insert chart', exact: true }).click(); await settle(page);
    assert.equal((await tabOf(page)).charts.length, 2);
    await page.getByRole('button', { name: 'Close chart editor', exact: true }).click();
    const second = page.locator('.sg-chart[data-chart="c2"]'), b = await second.boundingBox();
    await page.mouse.move(b.x + 60, b.y + 30); await page.mouse.down(); await page.mouse.move(b.x + 60, b.y + 438, { steps: 5 }); await page.mouse.up(); await settle(page);
    assert.equal((await lastApply(page)).ops[0].patch.at, 'D18');
    await shot(page, 'sheet-chart');
    const beforeScroll = await first.boundingBox();
    await page.locator('.sg-scroll').evaluate(n => { n.scrollTop = 100; }); await settle(page);
    assert.equal(Math.round((await first.boundingBox()).y), Math.round(beforeScroll.y - 100), 'charts scroll with cells');
    const svg = await first.locator('.sg-chart-image').textContent();
    await page.evaluate(() => window.__mock.externalEdit([{ op: 'set_cells', tab: 'Budget', at: 'B2', values: [['35']] }])); await settle(page);
    assert.notEqual(await first.locator('.sg-chart-image').textContent(), svg, 'calculated values replace SVG');
    await page.locator('.sg-scroll').evaluate(n => { n.scrollTop = 0; }); await settle(page);
    await first.click({ position: { x: 80, y: 50 } });
    const nudges = (await applies(page)).length;
    await page.keyboard.press('ArrowRight'); await page.keyboard.press('ArrowRight'); await page.waitForTimeout(450);
    assert.equal((await applies(page)).length, nudges + 1, 'arrow burst is debounced');
    assert.equal((await lastApply(page)).ops[0].patch.dx, 2);
    const acceptedTop = await first.evaluate(n => n.style.top);
    await page.evaluate(() => { window.__mock.rejectNext = true; });
    await page.keyboard.press('ArrowDown'); await page.waitForTimeout(600);
    assert.equal(await first.evaluate(n => n.style.top), acceptedTop, 'a rejected preview snaps back');
    await page.getByRole('button', { name: 'Export', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Excel workbook' }).click(); await settle(page);
    assert.match(await page.locator('.sg-toast').textContent(), /charts aren't included in XLSX exports/);
    await first.click({ position: { x: 80, y: 50 } });
    await page.keyboard.press('Delete'); await settle(page);
    assert.equal(await first.count(), 0); assert.equal((await lastApply(page)).ops[0].op, 'delete_chart');
    await page.close();
  },
  async opens() {
    const page = await open('dark');
    assert.equal(await shown(page, 'A1'), 'Category');
    assert.equal(await shown(page, 'E2'), '372500', 'the calculated total, not the formula');
    assert.equal(await shown(page, 'B14'), '14.03%', 'the number format shows');
    assert.equal(await shown(page, 'B15'), '#DIV/0!');
    assert.ok((await A(page, 'B15').getAttribute('class')).includes('err'), 'errors are drawn as errors');
    assert.ok((await A(page, 'A1').getAttribute('class')).split(' ').includes('b'), 'bold');
    assert.ok((await A(page, 'A12').getAttribute('class')).split(' ').includes('i'), 'italic');
    assert.equal(await addr(page), 'A1');
    assert.equal(await barText(page), 'Category');
    assert.equal(await page.title(), 'Q3 budget — nexus studio');
    assert.equal(await page.locator('#sheet-app .sg-title').inputValue(), 'Q3 budget');
    // Column letters and row numbers, widths from the sheet (A is 150 wide), merged cells as one.
    assert.equal(await page.locator('#sheet-app .sg-ch').first().textContent(), 'A');
    assert.equal(Math.round((await A(page, 'A1').boundingBox()).width), 150);
    assert.equal(Math.round((await A(page, 'B1').boundingBox()).width), 100);
    assert.equal(Math.round((await A(page, 'A1').boundingBox()).height), 28, 'row 1 is taller');
    assert.equal(Math.round((await A(page, 'A12').boundingBox()).width), 150 + 100 * 3 + 120, 'a merge is one cell');
    assert.equal(await A(page, 'B12').count(), 0, 'the cells it hides are not drawn');
    assert.equal(await page.locator('#sheet-app .sg-pin').count(), 1, 'one comment pin');
    await shot(page, 'sheet-grid');
    await page.close();
  },
  async lightTheme() {
    const page = await open('light');
    await click(page, 'C4');
    await page.keyboard.down('Shift'); await click(page, 'D6'); await page.keyboard.up('Shift');
    await shot(page, 'sheet-grid-light');
    await page.close();
  },
  async typing() {
    const page = await open('dark');
    await click(page, 'G2');
    await page.keyboard.type('Hello');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), 'Hello', 'typing replaces and starts the editor');
    assert.equal(await barText(page), 'Hello', 'the formula bar shows the same edit');
    assert.equal(await applies(page).then(a => a.length), 0, 'nothing is sent while typing');
    await page.keyboard.press('Enter');
    await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'set_cells', tab: 'Budget', at: 'G2', values: [['Hello']] }] });
    assert.equal(await addr(page), 'G3', 'Enter commits and moves down');
    assert.equal(await shown(page, 'G2'), 'Hello');
    assert.equal(await page.evaluate(() => window.__mock.undoDepth), 1, 'one edit is one undo step');
    // A formula: the value shows, the formula bar keeps the formula.
    await click(page, 'G3');
    await page.keyboard.type('=SUM(B2:B4)');
    await page.keyboard.press('Tab');
    await settle(page);
    assert.equal(await inputOf(page, 'G3'), '=SUM(B2:B4)');
    assert.equal(await addr(page), 'H3', 'Tab commits and moves right');
    assert.equal(await shown(page, 'G3'), '172250');
    await click(page, 'G3');
    assert.equal(await barText(page), '=SUM(B2:B4)', 'the formula bar shows the formula, the cell the value');
    // Typing in enter mode: arrows commit and move; Shift+Tab goes left.
    await click(page, 'G5');
    await page.keyboard.type('x');
    await page.keyboard.press('ArrowDown');
    await settle(page);
    assert.equal(await inputOf(page, 'G5'), 'x'); assert.equal(await addr(page), 'G6');
    await page.keyboard.type('y');
    await page.keyboard.press('Shift+Tab');
    await settle(page);
    assert.equal(await inputOf(page, 'G6'), 'y'); assert.equal(await addr(page), 'F6');
    // Typing over a cell with the same text sends nothing.
    const before = (await applies(page)).length;
    await click(page, 'A2');
    await page.keyboard.type('Salaries');
    await page.keyboard.press('Enter');
    await settle(page);
    assert.equal((await applies(page)).length, before, 'an unchanged cell is not an edit');
    await page.close();
  },
  async movingAndSelecting() {
    const page = await open('dark');
    await click(page, 'B2');
    await page.keyboard.press('ArrowDown'); await page.keyboard.press('ArrowRight');
    assert.equal(await addr(page), 'C3');
    await page.keyboard.press('ArrowUp'); await page.keyboard.press('ArrowLeft');
    assert.equal(await addr(page), 'B2');
    await page.keyboard.press('Shift+ArrowDown'); await page.keyboard.press('Shift+ArrowDown'); await page.keyboard.press('Shift+ArrowRight');
    assert.equal(await addr(page), 'B2:C4', 'shift+arrows extend');
    assert.equal(await page.locator('#sheet-app .sg-cell.sg-sel').count(), 6, 'the selection is drawn');
    await page.keyboard.press('ArrowDown');
    assert.equal(await addr(page), 'B3', 'an arrow collapses the range');
    // Ctrl/Cmd+arrow jumps to the edge of the data.
    await click(page, 'A2');
    await page.keyboard.press(`${MOD}+ArrowDown`);
    assert.equal(await addr(page), 'A10');
    await page.keyboard.press(`${MOD}+ArrowDown`);
    assert.equal(await addr(page), 'A12', 'the next data across a gap');
    await click(page, 'A2');
    await page.keyboard.press(`${MOD}+ArrowRight`);
    assert.equal(await addr(page), 'E2');
    await page.keyboard.press(`${MOD}+Shift+ArrowDown`);
    assert.equal(await addr(page), 'E2:E10', 'shift+Cmd+arrow extends to the data edge');
    // Tab and Enter walk the selection; Shift reverses.
    await click(page, 'B2'); await page.keyboard.press('Shift+ArrowDown'); await page.keyboard.press('Shift+ArrowRight');
    await page.keyboard.press('Tab');
    assert.equal(await addr(page), 'B2:C3'); assert.equal((await page.evaluate(() => document.querySelector('#sheet-app .sg-cell.sg-active').dataset.c)), '3', 'Tab moves the active cell inside the selection');
    await page.keyboard.press('Shift+Enter');
    assert.deepEqual(await page.evaluate(() => { const n = document.querySelector('#sheet-app .sg-cell.sg-active'); return [n.dataset.r, n.dataset.c]; }), ['3', '2'], 'Shift+Enter goes back and wraps to the previous column');
    // Click, shift+click, drag, headers and select all.
    await click(page, 'B2'); await click(page, 'D5', { modifiers: ['Shift'] });
    assert.equal(await addr(page), 'B2:D5');
    await drag(page, 'C7', 'E9');
    assert.equal(await addr(page), 'C7:E9');
    await page.locator('#sheet-app .sg-ch', { hasText: /^C$/ }).click();
    assert.equal(await addr(page), 'C1:C16', 'a column header selects the column (to the data)');
    assert.ok((await page.locator('#sheet-app .sg-ch.whole').count()) === 1);
    await page.locator('#sheet-app .sg-rh', { hasText: /^4$/ }).click();
    assert.equal(await addr(page), 'A4:E4', 'a row header selects the row');
    await page.locator('#sheet-app .sg-corner').click();
    assert.equal(await addr(page), 'A1:E16', 'the corner selects everything');
    await page.keyboard.press(`${MOD}+a`);
    assert.equal(await addr(page), 'A1:E16');
    await page.close();
  },
  async editInPlace() {
    const page = await open('dark');
    // F2 edits with the caret at the end; Escape cancels.
    await click(page, 'A2');
    await page.keyboard.press('F2');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), 'Salaries');
    await page.keyboard.type('XYZ');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), 'SalariesXYZ', 'the caret starts at the end');
    await page.keyboard.press('Escape');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').isVisible(), false);
    assert.equal(await shown(page, 'A2'), 'Salaries'); assert.equal((await applies(page)).length, 0, 'Escape sends nothing');
    assert.equal(await addr(page), 'A2');
    // In place the arrows move the caret instead of leaving the cell.
    await page.keyboard.press('Enter');
    await page.keyboard.press('ArrowLeft'); await page.keyboard.press('ArrowLeft');
    await page.keyboard.type('-');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), 'Salari-es');
    await page.keyboard.press('Enter');
    await settle(page);
    assert.equal(await inputOf(page, 'A2'), 'Salari-es'); assert.equal(await addr(page), 'A3');
    // Double-click edits too.
    await A(page, 'A4').dblclick();
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), 'Software');
    await page.keyboard.press('Escape');
    // Editing in the formula bar is the same edit, and the cell editor follows.
    await click(page, 'A5');
    await page.locator('#sheet-app .sg-bar-text').click();
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').isVisible(), true);
    await page.keyboard.press('End'); await page.keyboard.type('!');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), 'Travel!');
    await page.keyboard.press('Enter');
    await settle(page);
    assert.equal(await inputOf(page, 'A5'), 'Travel!'); assert.equal(await addr(page), 'A6');
    // Clicking another cell commits the edit (and does not point: it is not a formula).
    await page.keyboard.press('F2'); await page.keyboard.type('Q');
    await click(page, 'D3');
    await settle(page);
    assert.equal(await inputOf(page, 'A6'), 'MarketingQ'); assert.equal(await addr(page), 'D3');
    // Alt+Enter is a newline inside the cell.
    await click(page, 'G1');
    await page.keyboard.type('one'); await page.keyboard.press('Alt+Enter'); await page.keyboard.type('two');
    await page.keyboard.press('Enter'); await settle(page);
    assert.equal(await inputOf(page, 'G1'), 'one\ntwo');
    await page.close();
  },
  async pointMode() {
    const page = await open('dark');
    await click(page, 'G1');
    await page.keyboard.type('=SUM(');
    await page.keyboard.press('ArrowDown');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), '=SUM(G2', 'an arrow inserts the neighbouring reference');
    await page.keyboard.press('ArrowRight');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), '=SUM(H2', 'and moves it');
    await page.keyboard.press('ArrowLeft'); await page.keyboard.press('ArrowLeft'); await page.keyboard.press('ArrowLeft');
    await page.keyboard.press('Shift+ArrowDown'); await page.keyboard.press('Shift+ArrowDown');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), '=SUM(E2:E4', 'shift+arrow extends the range');
    assert.equal(await page.locator('#sheet-app .sg-point').isVisible(), true, 'the referenced cells are outlined');
    await shot(page, 'sheet-formula-edit');
    await page.keyboard.type(')');
    assert.equal(await page.locator('#sheet-app .sg-point').isVisible(), false);
    await page.keyboard.press('Enter');
    await settle(page);
    assert.equal(await inputOf(page, 'G1'), '=SUM(E2:E4)');
    assert.equal(await shown(page, 'G1'), String(372500 + 103000 + 56300));
    assert.equal(await addr(page), 'G2');
    // Clicks point too: a click after an operator inserts that cell, a drag a range, and the edit stays open.
    await click(page, 'G3');
    await page.keyboard.type('=');
    await click(page, 'B2');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), '=B2');
    assert.equal(await addr(page), 'G3', 'the selection stays on the cell being edited');
    await page.keyboard.type('+');
    await drag(page, 'C2', 'C3');
    assert.equal(await page.locator('#sheet-app .sg-cell-editor').inputValue(), '=B2+C2:C3');
    await page.keyboard.press('Enter'); await settle(page);
    assert.equal(await inputOf(page, 'G3'), '=B2+C2:C3');
    // Where no reference can go, an arrow commits (typing) or moves the caret (editing in place).
    await click(page, 'G5');
    await page.keyboard.type('=B2');
    await page.keyboard.press('ArrowDown'); await settle(page);
    assert.equal(await inputOf(page, 'G5'), '=B2'); assert.equal(await addr(page), 'G6');
    await page.close();
  },
  async rangesAndClearing() {
    const page = await open('dark');
    await drag(page, 'B2', 'C3');
    assert.equal(await addr(page), 'B2:C3');
    await page.keyboard.press('Delete');
    await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'clear', tab: 'Budget', range: 'B2:C3', what: 'inputs' }] });
    assert.equal(await shown(page, 'B2'), ''); assert.equal(await shown(page, 'C3'), ''); assert.equal(await shown(page, 'D3'), '41000');
    assert.equal(await shown(page, 'E2'), '128500', 'dependent formulas recalculate');
    await page.keyboard.press('Backspace');
    await page.close();
  },
  async undoRedo() {
    const page = await open('dark');
    await click(page, 'G2'); await page.keyboard.type('7'); await page.keyboard.press('Enter'); await settle(page);
    assert.equal(await shown(page, 'G2'), '7');
    await page.keyboard.press(`${MOD}+z`); await settle(page);
    assert.equal((await commands(page, 'Undo')).length, 1);
    assert.equal(await shown(page, 'G2'), '');
    await page.keyboard.press(`${MOD}+Shift+z`); await settle(page);
    assert.equal((await commands(page, 'Redo')).length, 1);
    assert.equal(await shown(page, 'G2'), '7');
    await page.locator('#sheet-app [data-tb="undo"]').click(); await settle(page);
    assert.equal((await commands(page, 'Undo')).length, 2, 'the toolbar button too');
    await page.close();
  },
  async clipboard() {
    const page = await open('dark');
    // Copy writes the display values as TSV and an HTML table, and shows the source range.
    await drag(page, 'A2', 'B3');
    const copied = await copyEvent(page, 'copy');
    assert.equal(copied.plain, 'Salaries\t120000\nContractors\t34000');
    assert.ok(copied.html.includes('<table>') && copied.html.includes('<td>Salaries</td>'));
    assert.ok(copied.prevented);
    assert.equal(await page.locator('#sheet-app .sg-copy').isVisible(), true);
    // Pasting that text back inside the sheet is a `copy` op, so references shift.
    await click(page, 'G2');
    await pasteInto(page, { 'text/plain': copied.plain });
    await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'copy', tab: 'Budget', from: 'A2:B3', to: 'G2', cut: false }] });
    assert.equal(await shown(page, 'H3'), '34000');
    assert.equal(await addr(page), 'G2:H3', 'the pasted block is selected');
    // A formula keeps its meaning when it moves.
    await click(page, 'E2');
    const formula = await copyEvent(page, 'copy');
    await click(page, 'E3');
    await pasteInto(page, { 'text/plain': formula.plain });
    await settle(page);
    assert.equal(await inputOf(page, 'E3'), '=SUM(B3:D3)', 'relative references shift');
    // Cut moves the cells.
    await click(page, 'A9');
    const cut = await copyEvent(page, 'cut');
    await click(page, 'G9');
    await pasteInto(page, { 'text/plain': cut.plain });
    await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'copy', tab: 'Budget', from: 'A9', to: 'G9', cut: true }] });
    assert.equal(await inputOf(page, 'A9'), undefined); assert.equal(await inputOf(page, 'G9'), 'Training');
    assert.equal(await page.locator('#sheet-app .sg-copy').isVisible(), false, 'a cut is pasted once');
    await page.close();
  },
  async pasteFromOutside() {
    const page = await open('dark');
    await click(page, 'G5');
    await pasteInto(page, { 'text/plain': 'a\tb\n1\t2\n"x\ty"\t"say ""hi"""\n' });
    await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'set_cells', tab: 'Budget', at: 'G5', values: [['a', 'b'], ['1', '2'], ['x\ty', 'say "hi"']] }] });
    assert.equal(await addr(page), 'G5:H7');
    // Larger than the selection: placed whole at its corner.
    await click(page, 'A14'); await click(page, 'A15', { modifiers: ['Shift'] });
    await pasteInto(page, { 'text/plain': '1\t2\t3\n4\t5\t6\n7\t8\t9' });
    await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].at, 'A14');
    assert.equal((await lastApply(page)).ops[0].values.length, 3);
    // One value over a selection fills it.
    await drag(page, 'G10', 'H11');
    await pasteInto(page, { 'text/plain': 'z' });
    await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0], { op: 'set_cells', tab: 'Budget', at: 'G10', values: [['z', 'z'], ['z', 'z']] });
    // An HTML table with no text flavour (some apps) pastes its cell text.
    await click(page, 'J1');
    await pasteInto(page, { 'text/html': '<table><tr><td>p</td><td>q&amp;r</td></tr><tr><td>s</td><td>t<br>u</td></tr></table>' });
    await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].values, [['p', 'q&r'], ['s', 't\nu']]);
    // A copy since our own wins: text from outside is not mistaken for our clipboard.
    await click(page, 'B2');
    await copyEvent(page, 'copy');
    await click(page, 'J5');
    await pasteInto(page, { 'text/plain': 'outside' });
    await settle(page);
    assert.equal((await lastApply(page)).ops[0].op, 'set_cells');
    await page.close();
  },
  async fillHandle() {
    const page = await open('dark');
    await drag(page, 'B2', 'B3');
    const handle = page.locator('#sheet-app .sg-handle');
    await handle.waitFor();
    const h = await centerOf(handle), target = await centerOf(A(page, 'B6'));
    await page.mouse.move(h.x, h.y); await page.mouse.down();
    await page.mouse.move(target.x, target.y, { steps: 4 });
    assert.equal(await page.locator('#sheet-app .sg-fill').isVisible(), true, 'the target is previewed');
    await page.mouse.up();
    await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'fill', tab: 'Budget', from: 'B2:B3', to: 'B2:B6' }] });
    assert.equal(await addr(page), 'B2:B6', 'the filled range is selected');
    assert.equal(await inputOf(page, 'B4'), '-52000', 'two numbers continue as a series, as the core does');
    // Dragging right fills across.
    await click(page, 'B10');
    const h2 = await centerOf(page.locator('#sheet-app .sg-handle')), t2 = await centerOf(A(page, 'D10'));
    await page.mouse.move(h2.x, h2.y); await page.mouse.down(); await page.mouse.move(t2.x, t2.y, { steps: 4 }); await page.mouse.up();
    await settle(page);
    assert.equal((await lastApply(page)).ops[0].to, 'B10:D10');
    // Double-clicking it fills down as far as the neighbouring column goes.
    await click(page, 'F2');
    await page.keyboard.type('x'); await page.keyboard.press('Enter');
    await click(page, 'F2');
    await page.locator('#sheet-app .sg-handle').dblclick();
    await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0], { op: 'fill', tab: 'Budget', from: 'F2', to: 'F2:F10' });
    await page.close();
  },
  async zooming() {
    const page = await open('dark');
    const before = await A(page, 'B2').boundingBox();
    await click(page, 'B2');
    await page.keyboard.press(`${MOD}+=`); await page.keyboard.press(`${MOD}+=`); await settle(page);
    const big = await A(page, 'B2').boundingBox();
    assert.ok(Math.abs(big.width / before.width - 1.2) < .02 && Math.abs(big.height / before.height - 1.2) < .06, 'cells grow with the zoom');
    assert.equal(await A(page, 'B2').evaluate(n => getComputedStyle(n).fontSize), '15.6px', 'text grows with them');
    assert.equal(await page.evaluate(() => localStorage.getItem('nexus-sheet-zoom')), '1.2', 'the zoom is remembered');
    // A dragged border stores the unzoomed width.
    await click(page, 'C3');
    assert.equal(await page.locator('#sheet-app .sg-addr').inputValue(), 'C3');
    const c = await page.locator('#sheet-app .sg-ch', { hasText: /^C$/ }).boundingBox();
    const start = (await tabOf(page)).cols?.C ?? 100;
    await page.mouse.move(c.x + c.width - 1, c.y + c.height / 2); await page.mouse.down();
    await page.mouse.move(c.x + c.width + 59, c.y + c.height / 2, { steps: 4 }); await page.mouse.up(); await settle(page);
    assert.equal((await lastApply(page)).ops[0].width, start + 50, '60 screen pixels at 120% are 50 sheet pixels');
    await page.keyboard.press(`${MOD}+-`); await settle(page);
    assert.equal(await page.evaluate(() => localStorage.getItem('nexus-sheet-zoom')), '1.1');
    await page.keyboard.press(`${MOD}+0`); await settle(page);
    assert.equal(Math.round((await A(page, 'B2').boundingBox()).width), Math.round(before.width), 'Mod+0 goes back to 100%');
    // A pinch is many small control-wheels; they add up.
    const under = await A(page, 'D6').boundingBox(), spot = { x: under.x + under.width / 2, y: under.y + under.height / 2 };
    await page.mouse.move(spot.x, spot.y);
    await page.keyboard.down('Control');
    for (let i = 0; i < 20; i++) await page.mouse.wheel(0, -1);
    await page.keyboard.up('Control'); await settle(page);
    const pinched = Number(await page.evaluate(() => localStorage.getItem('nexus-sheet-zoom')));
    assert.ok(pinched > 1.15 && pinched < 1.3, `twenty small pinch steps add up (${pinched})`);
    const after = await A(page, 'D6').boundingBox();
    assert.ok(spot.x >= after.x && spot.x <= after.x + after.width && spot.y >= after.y && spot.y <= after.y + after.height, 'the cell under the pointer stays under it');
    await page.evaluate(() => localStorage.removeItem('nexus-sheet-zoom'));
    await page.close();
  },
  async resizing() {
    const page = await open('dark');
    // Drag the border between C and D.
    const c = await page.locator('#sheet-app .sg-ch', { hasText: /^C$/ }).boundingBox();
    await page.mouse.move(c.x + c.width - 1, c.y + c.height / 2);
    assert.equal(await page.locator('#sheet-app .sg-scroll').evaluate(n => n.style.cursor), 'col-resize');
    await page.mouse.down(); await page.mouse.move(c.x + c.width + 40, c.y + c.height / 2, { steps: 4 });
    assert.equal(await page.locator('#sheet-app .sg-resize').isVisible(), true, 'a guide shows the new width');
    await page.mouse.up(); await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'set_col_width', tab: 'Budget', cols: 'C:C', width: 141 }] });
    assert.equal(Math.round((await A(page, 'C2').boundingBox()).width), 141);
    // The selected columns resize together.
    await page.locator('#sheet-app .sg-ch', { hasText: /^B$/ }).click();
    await page.locator('#sheet-app .sg-ch', { hasText: /^D$/ }).click({ modifiers: ['Shift'] });
    const d = await page.locator('#sheet-app .sg-ch', { hasText: /^D$/ }).boundingBox();
    await page.mouse.move(d.x + d.width - 1, d.y + d.height / 2); await page.mouse.down(); await page.mouse.move(d.x + d.width + 20, d.y + d.height / 2, { steps: 3 }); await page.mouse.up(); await settle(page);
    assert.equal((await lastApply(page)).ops[0].cols, 'B:D');
    // Double-click fits the widest value.
    await click(page, 'A2');
    const a = await page.locator('#sheet-app .sg-ch', { hasText: /^A$/ }).boundingBox();
    await page.mouse.dblclick(a.x + a.width - 1, a.y + a.height / 2); await settle(page);
    const fit = (await lastApply(page)).ops[0];
    assert.deepEqual(fit, { op: 'fit_cols', tab: 'Budget', cols: 'A:A' }, 'double-clicking a border fits the column to its data');
    assert.equal(Math.round((await A(page, 'A2').boundingBox()).width), Math.round((await tabOf(page)).cols.A));
    assert.ok((await tabOf(page)).cols.A < 150, 'narrower than the hand-set width');
    // A row too.
    const r = await page.locator('#sheet-app .sg-rh', { hasText: /^3$/ }).boundingBox();
    await page.mouse.move(r.x + r.width / 2, r.y + r.height - 1); await page.mouse.down(); await page.mouse.move(r.x + r.width / 2, r.y + r.height + 15, { steps: 3 }); await page.mouse.up(); await settle(page);
    assert.equal((await lastApply(page)).ops[0].op, 'set_row_height'); assert.equal((await lastApply(page)).ops[0].rows, '3:3');
    assert.ok((await tabOf(page)).rows?.[3], 'the dragged row has a set height');
    // Double-clicking the row border hands the height back to its text.
    const r3 = await page.locator('#sheet-app .sg-rh', { hasText: /^3$/ }).boundingBox();
    await page.mouse.dblclick(r3.x + r3.width / 2, r3.y + r3.height - 1); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0], { op: 'fit_rows', tab: 'Budget', rows: '3:3' }, 'double-clicking a row border fits the row');
    assert.ok(!(await tabOf(page)).rows?.[3], 'and drops its set height');
    await page.close();
  },
  async wrappingAndFitting() {
    const page = await open('dark');
    const long = 'a sentence long enough to need several lines inside a narrow cell';
    // Wrap: the menu sends the format, and the row grows when the backend answers.
    await click(page, 'G2');
    await page.keyboard.type(long); await page.keyboard.press('Enter'); await settle(page);
    const plain = Math.round((await A(page, 'G2').boundingBox()).height);
    assert.equal(plain, 24, 'an unwrapped row keeps the default height');
    await click(page, 'G2');
    await page.locator('#sheet-app [data-tb="wrap"]').click();
    assert.deepEqual(await page.locator('.sg-menu .menu-label').allTextContents(), ['Overflow', 'Wrap', 'Clip']);
    await page.getByRole('menuitemradio', { name: 'Wrap' }).click(); await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'format', tab: 'Budget', range: 'G2', patch: { wrap: 'wrap' } }] });
    const wrapped = Math.round((await A(page, 'G2').boundingBox()).height);
    assert.ok(wrapped > 40, `the row grew to fit: ${wrapped}`);
    assert.ok((await A(page, 'G2').getAttribute('class')).includes('wr-wrap'));
    assert.equal(Math.round((await page.locator('#sheet-app .sg-rh', { hasText: /^2$/ }).boundingBox()).height), wrapped, 'the row header matches');
    assert.equal(await page.locator('#sheet-app [data-tb="wrap"] .sg-tb-label').textContent(), 'Wrap', 'the toolbar shows the active cell');
    // Vertical alignment is a format too.
    await page.locator('#sheet-app [data-tb="valign"]').click();
    assert.deepEqual(await page.locator('.sg-menu .menu-label').allTextContents(), ['Top', 'Middle', 'Bottom']);
    await page.getByRole('menuitemradio', { name: 'Top' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { valign: 'top' });
    assert.ok((await A(page, 'G2').getAttribute('class')).includes('va-top'));
    // Clip: cut at the cell border.
    await click(page, 'G3');
    await page.keyboard.type(long); await page.keyboard.press('Enter'); await settle(page);
    await click(page, 'G3');
    await page.locator('#sheet-app [data-tb="wrap"]').click();
    await page.getByRole('menuitemradio', { name: 'Clip' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { wrap: 'clip' });
    assert.ok((await A(page, 'G3').getAttribute('class')).includes('wr-clip'));
    assert.equal(Math.round((await A(page, 'G3').boundingBox()).height), 24, 'clip does not grow the row');
    const clipped = await A(page, 'G3').locator('.sg-text').boundingBox();
    const cellWidth = (await A(page, 'G3').boundingBox()).width;
    assert.ok(clipped.width <= cellWidth && clipped.width >= cellWidth - 2, `clipped at the cell: ${clipped.width} of ${cellWidth}`);
    // Overflow: text spills into an empty neighbour and stops at a filled one.
    await click(page, 'G4');
    await page.keyboard.type(long); await page.keyboard.press('Enter'); await settle(page);
    await click(page, 'G4');
    await page.locator('#sheet-app [data-tb="wrap"]').click();
    await page.getByRole('menuitemradio', { name: 'Overflow' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { wrap: null }, 'overflow removes the field');
    assert.ok((await A(page, 'G4').getAttribute('class')).includes('ov'));
    const spilled = await A(page, 'G4').locator('.sg-text').boundingBox();
    assert.ok(spilled.width > (await A(page, 'G4').boundingBox()).width + 100, `it spans the empty neighbours: ${spilled.width}`);
    await click(page, 'H4');
    await page.keyboard.type('full'); await page.keyboard.press('Enter'); await settle(page);
    assert.ok(!(await A(page, 'G4').getAttribute('class')).includes('ov'), 'a filled neighbour stops the spill');
    const stopped = await A(page, 'G4').locator('.sg-text').boundingBox();
    assert.ok(stopped.width <= (await A(page, 'G4').boundingBox()).width, 'the spill stops at the filled cell');
    // Cmd/Ctrl+Alt+W toggles wrap on the selection.
    await click(page, 'G5');
    await page.keyboard.type('toggle me please, with enough words to wrap'); await page.keyboard.press('Enter'); await settle(page);
    await click(page, 'G5');
    await page.keyboard.press(`${MOD}+Alt+w`); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { wrap: 'wrap' });
    await page.keyboard.press(`${MOD}+Alt+w`); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { wrap: null }, 'and back to overflow');
    await shot(page, 'sheet-wrap');
    // Fitting: the header menu, for one column and for a selection.
    await page.locator('#sheet-app .sg-ch', { hasText: /^B$/ }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Fit to data' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0], { op: 'fit_cols', tab: 'Budget', cols: 'B:B' });
    await page.locator('#sheet-app .sg-ch', { hasText: /^B$/ }).click();
    await page.locator('#sheet-app .sg-ch', { hasText: /^D$/ }).click({ modifiers: ['Shift'] });
    await page.locator('#sheet-app .sg-ch', { hasText: /^C$/ }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Fit to data' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0], { op: 'fit_cols', tab: 'Budget', cols: 'B:D' }, 'the selected columns fit together');
    // And rows, from the row header menu.
    await page.locator('#sheet-app .sg-rh', { hasText: /^4$/ }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Fit to data' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0], { op: 'fit_rows', tab: 'Budget', rows: '4:4' });
    await page.close();
  },
  async wrappedRowsVirtualize() {
    const cells = { A1: { input: 'Note', fmt: { bold: true } } };
    for (let r = 2; r <= 400; r++) cells[`A${r}`] = { input: `Row ${r} has a sentence long enough to wrap over two or three lines in a narrow column`, fmt: { wrap: 'wrap' } };
    const fixture = { sheet: { ...structuredClone(sample.sheet), title: 'Wrapped', tabs: [{ id: 't1', name: 'Wrapped', cells, cols: { A: 120 } }] }, threads: [] };
    const page = await open('dark', undefined, { fixture });
    const tall = Math.round((await A(page, 'A2').boundingBox()).height);
    assert.ok(tall > 24, `wrapped rows are taller: ${tall}`);
    assert.ok((await rowCount(page)) < 80, 'a window of rows');
    assert.ok((await page.locator('#sheet-app .sg-cell').count()) < 1500);
    const height = await page.locator('#sheet-app .sg-inner').evaluate(n => n.getBoundingClientRect().height);
    assert.ok(height > 400 * 24, `the full height counts the tall rows: ${height}`);
    await page.locator('#sheet-app .sg-scroll').evaluate((n, y) => { n.scrollTop = y; }, 24 + 298 * tall);
    await page.locator('#sheet-app .sg-cell[data-r="300"][data-c="1"]').waitFor();
    assert.ok((await page.locator('#sheet-app .sg-cell').count()) < 1500, 'still a window after scrolling');
    assert.ok((await rowCount(page)) < 80);
    assert.equal(Math.round((await A(page, 'A300').boundingBox()).height), tall, 'every wrapped row keeps its height');
    await page.close();
  },
  async headerMenus() {
    const page = await open('dark');
    // Insert a row above row 5.
    await page.locator('#sheet-app .sg-rh', { hasText: /^5$/ }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Insert 1 row above' }).click();
    await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'insert_rows', tab: 'Budget', at: 5, count: 1 }] });
    assert.equal(await shown(page, 'A6'), 'Travel', 'the rows below moved down');
    // Insert two columns to the right of C:D.
    await page.locator('#sheet-app .sg-ch', { hasText: /^C$/ }).click();
    await page.locator('#sheet-app .sg-ch', { hasText: /^D$/ }).click({ modifiers: ['Shift'] });
    await page.locator('#sheet-app .sg-ch', { hasText: /^D$/ }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Insert 2 columns right' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops, [{ op: 'insert_cols', tab: 'Budget', at: 'E', count: 2 }]);
    // Delete a column; sort by one.
    await page.locator('#sheet-app .sg-ch', { hasText: /^F$/ }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Delete 1 column' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops, [{ op: 'delete_cols', tab: 'Budget', at: 'F', count: 1 }]);
    await page.locator('#sheet-app .sg-ch', { hasText: /^B$/ }).click({ button: 'right' });
    await shot(page, 'sheet-menu');
    await page.getByRole('menuitem', { name: 'Sort sheet Z → A' }).click(); await settle(page);
    const sort = (await lastApply(page)).ops[0];
    assert.equal(sort.op, 'sort'); assert.equal(sort.by, 'B'); assert.equal(sort.desc, true); assert.equal(sort.header, true, 'row 1 is frozen, so it is the header');
    assert.equal(await shown(page, 'A2'), 'Budget cap', 'the largest first');
    assert.equal(await shown(page, 'A1'), 'Category', 'the header stays');
    await page.locator('#sheet-app .sg-ch', { hasText: /^B$/ }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Sort sheet A → Z' }).click(); await settle(page);
    assert.equal((await lastApply(page)).ops[0].desc, false);
    // Freeze and unfreeze.
    await page.locator('#sheet-app .sg-rh', { hasText: /^3$/ }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Freeze up to row 3' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0], { op: 'freeze', tab: 'Budget', rows: 3, cols: 0 });
    await page.locator('#sheet-app .sg-ch', { hasText: /^B$/ }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Freeze up to column B' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0], { op: 'freeze', tab: 'Budget', rows: 3, cols: 2 });
    await page.locator('#sheet-app .sg-ch', { hasText: /^B$/ }).click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Unfreeze' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0], { op: 'freeze', tab: 'Budget', rows: 0, cols: 0 });
    // A cell's menu.
    await A(page, 'C4').click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Insert 1 column left' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops, [{ op: 'insert_cols', tab: 'Budget', at: 'C', count: 1 }]);
    await page.close();
  },
  async tabs() {
    const page = await open('dark');
    assert.deepEqual(await tabsBar(page).allTextContents(), ['Budget', 'Data']);
    assert.equal(await tabsBar(page).first().getAttribute('aria-selected'), 'true');
    // Switch: the other tab's cells show, and each tab keeps its own selection.
    await click(page, 'C4');
    await tabsBar(page).nth(1).click();
    await page.locator('#sheet-app .sg-cell', { hasText: 'Region' }).waitFor();
    assert.equal(await shown(page, 'B2'), '1200');
    assert.equal(await addr(page), 'A1');
    await tabsBar(page).first().click();
    assert.equal(await addr(page), 'C4', 'the selection came back with the tab');
    // Add a tab: one op, and the new tab opens.
    await page.getByRole('button', { name: 'Add a sheet' }).click(); await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'add_tab', name: 'Sheet3', after: 'Data' }] });
    assert.deepEqual(await tabsBar(page).allTextContents(), ['Budget', 'Data', 'Sheet3']);
    assert.equal(await page.locator('#sheet-app .sg-tab.on').textContent(), 'Sheet3');
    // Rename by double-click.
    await tabsBar(page).nth(1).dblclick();
    const input = page.locator('#sheet-app .sg-tab-input');
    await input.fill('Regions'); await input.press('Enter'); await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'rename_tab', tab: 'Data', name: 'Regions' }] });
    assert.deepEqual(await tabsBar(page).allTextContents(), ['Budget', 'Regions', 'Sheet3']);
    // Escape cancels a rename.
    await tabsBar(page).nth(1).dblclick();
    await page.locator('#sheet-app .sg-tab-input').fill('Nope'); await page.keyboard.press('Escape');
    assert.deepEqual(await tabsBar(page).allTextContents(), ['Budget', 'Regions', 'Sheet3']);
    await shot(page, 'sheet-tabs');
    // Drag the last tab to the front: tabs reorder as ops that put each after its predecessor.
    const last = await centerOf(tabsBar(page).nth(2)), first = await tabsBar(page).first().boundingBox();
    await page.mouse.move(last.x, last.y); await page.mouse.down(); await page.mouse.move(first.x + 4, last.y, { steps: 6 });
    await page.mouse.up(); await settle(page);
    assert.deepEqual(await tabsBar(page).allTextContents(), ['Sheet3', 'Budget', 'Regions']);
    assert.ok((await lastApply(page)).ops.every(o => o.op === 'move_tab'));
    assert.equal(await page.evaluate(() => window.__mock.undoDepth), 3, 'add, rename and one drag: three undo steps');
    // Delete from the context menu; the last tab cannot go.
    await tabsBar(page).first().click({ button: 'right' });
    await page.getByRole('menuitem', { name: 'Delete' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops, [{ op: 'delete_tab', tab: 'Sheet3' }]);
    assert.deepEqual(await tabsBar(page).allTextContents(), ['Budget', 'Regions']);
    // The address box jumps across tabs.
    await page.locator('#sheet-app .sg-addr').fill('Regions!B3');
    await page.keyboard.press('Enter');
    assert.equal(await page.locator('#sheet-app .sg-tab.on').textContent(), 'Regions');
    assert.equal(await addr(page), 'B3'); assert.equal(await barText(page), '950');
    await page.locator('#sheet-app .sg-addr').fill('nonsense'); await page.keyboard.press('Enter');
    await page.locator('.sg-toast', { hasText: 'not a cell' }).waitFor();
    await page.close();
  },
  async formatting() {
    const page = await open('dark');
    await drag(page, 'B2', 'C3');
    await page.locator('#sheet-app [data-tb="bold"]').click(); await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'format', tab: 'Budget', range: 'B2:C3', patch: { bold: true } }] });
    assert.ok((await A(page, 'C3').getAttribute('class')).split(' ').includes('b'), 'bold shows at once');
    assert.equal(await page.locator('#sheet-app [data-tb="bold"]').getAttribute('aria-pressed'), 'true');
    await page.keyboard.press(`${MOD}+b`); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { bold: false }, 'bold toggles off when the active cell has it');
    await page.keyboard.press(`${MOD}+i`); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { italic: true });
    await page.locator('#sheet-app [data-tb="underline"]').click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { underline: true });
    // Number formats.
    await page.locator('#sheet-app [data-tb="num"]').click();
    assert.deepEqual(await page.locator('.sg-menu .menu-label').allTextContents(), ['Automatic', 'Number', 'Percent', 'Currency', 'Date', 'Plain text']);
    await page.getByRole('menuitemradio', { name: /Percent/ }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0], { op: 'format', tab: 'Budget', range: 'B2:C3', patch: { num: '0.00%' } });
    assert.equal(await shown(page, 'B2'), '12000000.00%');
    assert.equal(await page.locator('#sheet-app [data-tb="num"] .sg-tb-label').textContent(), 'Percent');
    await page.locator('#sheet-app [data-tb="num"]').click();
    await page.getByRole('menuitemradio', { name: /Automatic/ }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { num: null }, 'Automatic removes the format');
    // Colours come from the sheet's theme.
    await page.locator('#sheet-app [data-tb="color"]').click();
    assert.ok((await page.locator('.sg-swatch').count()) >= 6);
    await page.locator('.sg-swatch[data-token="accent"]').click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { color: 'accent' });
    assert.equal(await A(page, 'B2').evaluate(n => n.style.color), 'rgb(174, 89, 62)', 'the token resolves through the sheet theme');
    await page.locator('#sheet-app [data-tb="fill"]').click();
    await page.locator('.sg-swatch[data-token="accent3"]').click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { fill: 'accent3' });
    assert.equal(await A(page, 'B2').evaluate(n => n.style.background.includes('212, 162, 92')), true);
    await page.locator('#sheet-app [data-tb="fill"]').click();
    await page.getByRole('menuitem', { name: 'No fill' }).click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { fill: null });
    // Alignment, and merge.
    await page.locator('#sheet-app [data-tb="align-center"]').click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops[0].patch, { align: 'center' });
    assert.ok((await A(page, 'B2').getAttribute('class')).includes('al-center'));
    await page.locator('#sheet-app [data-tb="merge"]').click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops, [{ op: 'merge', tab: 'Budget', range: 'B2:C3' }]);
    assert.equal(await A(page, 'C3').count(), 0, 'merged cells draw as one');
    await page.locator('#sheet-app [data-tb="merge"]').click(); await settle(page);
    assert.deepEqual((await lastApply(page)).ops, [{ op: 'unmerge', tab: 'Budget', range: 'B2:C3' }]);
    // A column header selection formats the column's data.
    await page.locator('#sheet-app .sg-ch', { hasText: /^E$/ }).click();
    await page.locator('#sheet-app [data-tb="italic"]').click(); await settle(page);
    assert.equal((await lastApply(page)).ops[0].range, 'E1:E16');
    await page.close();
  },
  async comments() {
    const page = await open('dark');
    // The pin opens the thread in the sidebar and selects its cells.
    await page.locator('#sheet-app .sg-pin').click();
    assert.equal(await addr(page), 'B3');
    assert.equal(await page.locator('#sheet-app .card.active').count(), 1);
    assert.ok((await page.locator('#sheet-app .card.active').textContent()).includes('Contractors jumped'));
    // Reply.
    await page.locator('#sheet-app .reply textarea').fill('A one-off, yes.');
    await page.getByRole('button', { name: 'Send to nexus' }).click(); await settle(page);
    assert.deepEqual(await page.evaluate(() => window.__mock.sent.at(-1)), { type: 'Reply', text: 'A one-off, yes.', thread: 'th1' });
    // A new comment on the selection: the composer, then SheetCommentCreate with send on.
    await click(page, 'B5');
    await page.keyboard.press(`${MOD}+Alt+m`);
    const composer = page.locator('#sheet-app ~ .sg-overlay .sg-composer, .sg-overlay .sg-composer');
    await composer.waitFor();
    assert.ok((await composer.textContent()).includes('Budget!B5'));
    assert.ok((await composer.textContent()).includes('9400'), 'the cell is quoted');
    await composer.locator('textarea').fill('Why did travel drop?');
    await shot(page, 'sheet-comment-composer');
    await page.keyboard.press(`${MOD}+Enter`); await settle(page);
    assert.deepEqual((await commands(page, 'SheetCommentCreate')).at(-1), { command: 'SheetCommentCreate', tab: 'Budget', range: 'B5', text: 'Why did travel drop?', send: true });
    assert.deepEqual(await page.evaluate(() => window.__mock.sent.at(-1)), { type: 'Comment', tab: 'Budget', range: 'B5', text: 'Why did travel drop?' });
    assert.equal(await composer.count(), 0, 'the composer closes');
    assert.equal(await page.locator('#sheet-app .sg-pin').count(), 2, 'the new thread has its pin');
    // The toolbar button and a note that stays out of the conversation.
    await click(page, 'C6');
    await page.locator('#sheet-app [data-tb="comment"]').click();
    await composer.locator('textarea').fill('For me');
    await composer.getByRole('button', { name: 'Comment', exact: true }).click(); await settle(page);
    assert.equal((await commands(page, 'SheetCommentCreate')).at(-1).send, false);
    // A range anchor; Escape cancels the composer.
    await drag(page, 'B7', 'C8');
    await page.keyboard.press(`${MOD}+Alt+m`);
    assert.ok((await composer.textContent()).includes('B7:C8'));
    await page.keyboard.press('Escape');
    assert.equal(await composer.count(), 0);
    // Resolving hides the pin.
    await page.locator('#sheet-app .card', { hasText: 'Contractors' }).getByRole('button', { name: 'Resolve thread' }).click(); await settle(page);
    assert.equal(await A(page, 'B3').locator('.sg-pin').count(), 0);
    await page.close();
  },
  async focusAndExternalEdits() {
    const page = await open('dark');
    // Selecting sends the focus to nexus, debounced.
    await click(page, 'C4');
    await click(page, 'D5');
    await page.waitForTimeout(450);
    const focus = await page.evaluate(() => window.__mock.sent.filter(s => s.type === 'Focus'));
    assert.deepEqual(focus.at(-1), { type: 'Focus', tab: 'Budget', range: 'D5' });
    assert.ok(focus.length <= 2, 'rapid moves are one message');
    // A focus from nexus scrolls to the range and outlines it.
    await page.evaluate(() => window.__mock.remoteFocus('Budget', 'D8:E9'));
    await page.locator('#sheet-app .sg-remote').waitFor();
    const box = await page.locator('#sheet-app .sg-remote').boundingBox(), cellBox = await A(page, 'D8').boundingBox();
    assert.ok(Math.abs(box.x - cellBox.x) < 3 && Math.abs(box.y - cellBox.y) < 3, 'outlined at the range');
    assert.equal(Math.round(box.width), 220); assert.equal(Math.round(box.height), 48);
    assert.equal(await addr(page), 'D5', 'the user keeps their selection');
    // Far away: it scrolls there. (A tab switch too.)
    await page.evaluate(() => window.__mock.remoteFocus('Data', 'B3'));
    await page.locator('#sheet-app .sg-tab.on', { hasText: 'Data' }).waitFor();
    await page.locator('#sheet-app .sg-remote').waitFor();
    await page.evaluate(() => window.__mock.remoteFocus('Budget', 'A60:B61'));
    await page.locator('#sheet-app .sg-tab.on', { hasText: 'Budget' }).waitFor();
    await page.waitForTimeout(100);
    assert.ok((await page.locator('#sheet-app .sg-scroll').evaluate(n => n.scrollTop)) > 0 || (await A(page, 'A60').count()) > 0);
    assert.ok(await page.locator('#sheet-app .sg-remote').isVisible());
    // An external edit flashes the changed cells and keeps selection and scroll.
    await page.evaluate(() => { document.querySelector('#sheet-app .sg-scroll').scrollTop = 0; });
    await click(page, 'C3');
    await page.evaluate(() => window.__mock.externalEdit([{ op: 'set_cells', tab: 'Budget', at: 'B2', values: [['1']] }, { op: 'set_cells', tab: 'Budget', at: 'B4', values: [['2']] }]));
    await page.locator('#sheet-app .sg-cell.sg-flash').first().waitFor();
    assert.equal(await page.locator('#sheet-app .sg-cell.sg-flash').count(), 2, 'the two cells that changed; formula results are not stored changes');
    assert.equal(await shown(page, 'B2'), '1'); assert.equal(await shown(page, 'E2'), String(1 + 124000 + 128500));
    assert.equal(await addr(page), 'C3', 'the selection stays');
    await page.locator('.sg-toast', { hasText: 'reloaded' }).waitFor();
    await shot(page, 'sheet-external-edit');
    await page.waitForTimeout(2000);
    assert.equal(await page.locator('#sheet-app .sg-cell.sg-flash').count(), 0, 'the flash fades');
    await page.close();
  },
  async frozenAndVirtual() {
    const page = await open('dark', { width: 1440, height: 900 }, { fixture: bigSheet(10000) });
    const total = await page.locator('#sheet-app .sg-cell').count();
    assert.ok(total > 0 && total < 1500, `only a window of cells exists: ${total}`);
    assert.ok((await rowCount(page)) < 80, 'a window of rows');
    assert.equal(await shown(page, 'D2'), String(((2 * 37) % 1000) * 2));
    // The inner element is as tall as the whole sheet, so the scrollbar is honest.
    const height = await page.locator('#sheet-app .sg-inner').evaluate(n => n.getBoundingClientRect().height);
    assert.ok(height > 240000, `the full height: ${height}`);
    // Scroll far down: the window follows, values arrive, and the DOM stays small.
    const timings = [];
    for (const top of [24 * 2500, 24 * 6000, 24 * 9900]) {
      const t0 = Date.now();
      await page.locator('#sheet-app .sg-scroll').evaluate((n, y) => { n.scrollTop = y; }, top);
      await page.locator(`#sheet-app .sg-cell[data-r="${top / 24 + 3}"][data-c="2"]`).waitFor();
      timings.push(Date.now() - t0);
      assert.ok((await page.locator('#sheet-app .sg-cell').count()) < 1500);
      assert.ok((await rowCount(page)) < 80);
    }
    assert.ok(timings.every(t => t < 1500), `scrolling stays quick: ${timings}`);
    await page.waitForFunction(() => window.__mock.commands.some(c => c.command === 'SheetValues'));
    await page.locator('#sheet-app .sg-cell', { hasText: 'Item 9903' }).waitFor();
    // Values that were not in the first window come from SheetValues, requested for the window in view.
    await page.waitForFunction(() => document.querySelector('#sheet-app .sg-cell[data-r="9903"][data-c="4"]')?.textContent === String(((9903 * 37) % 1000) * 2));
    assert.ok((await commands(page, 'SheetValues')).length > 0);
    assert.ok((await commands(page, 'SheetValues')).every(c => { const r = c.range.split(':'); return r.length === 2; }));
    // The frozen header row stays put.
    const header = await A(page, 'A1').boundingBox(), bar = await page.locator('#sheet-app .sg-colbar').boundingBox();
    assert.ok(Math.abs(header.y - (bar.y + bar.height)) < 2, 'row 1 is frozen under the column headers');
    assert.equal(await shown(page, 'B1'), 'Name');
    await shot(page, 'sheet-big');
    // Keyboard jumps reach the far end quickly, and typing there works.
    await page.locator('#sheet-app .sg-scroll').focus();
    await click(page, 'A' + (9900 + 3));
    await page.keyboard.press(`${MOD}+ArrowDown`);
    assert.equal(await addr(page), 'A10000');
    await page.keyboard.press(`${MOD}+Home`);
    assert.equal(await addr(page), 'A1');
    assert.equal(await page.locator('#sheet-app .sg-scroll').evaluate(n => n.scrollTop), 0);
    await page.close();
  },
  async frozenColumns() {
    const fixture = structuredClone(sample);
    Object.assign(fixture.sheet.tabs[0], { frozen: { rows: 2, cols: 1 } });
    fixture.sheet.tabs[0].cells.AB3 = { input: 'far right' };
    const page = await open('dark', undefined, { fixture });
    const before = await A(page, 'A5').boundingBox(), head = await page.locator('#sheet-app .sg-ch', { hasText: /^A$/ }).first().boundingBox();
    await page.locator('#sheet-app .sg-scroll').evaluate(n => { n.scrollLeft = 2000; n.scrollTop = 24 * 3; });
    await page.locator('#sheet-app .sg-cell', { hasText: 'far right' }).waitFor();
    const after = await A(page, 'A5').boundingBox(), headAfter = await page.locator('#sheet-app .sg-ch', { hasText: /^A$/ }).first().boundingBox();
    assert.equal(Math.round(after.x), Math.round(before.x), 'the frozen column stays put when the grid scrolls right');
    assert.equal(Math.round(headAfter.x), Math.round(head.x), 'and so does its header');
    assert.ok(after.y < before.y, 'while the rows scroll');
    // Frozen rows and the corner stay too.
    const corner = await A(page, 'A1').boundingBox(), top = await A(page, 'A2').boundingBox();
    assert.equal(Math.round(corner.x), Math.round(before.x)); const bar = await page.locator('#sheet-app .sg-colbar').boundingBox();
    assert.ok(Math.abs(corner.y - (bar.y + bar.height)) < 2, 'row 1 sits right under the column headers');
    assert.ok(Math.abs(top.y - (corner.y + corner.height)) < 2, 'row 2 follows it');
    assert.equal(await shown(page, 'A1'), 'Category');
    assert.ok((await A(page, 'AB3').boundingBox()).x > before.x + 100, 'far cells are drawn where the scroll put them');
    // Selecting a cell under the frozen area works and the edit keeps its place.
    await click(page, 'A5');
    assert.equal(await addr(page), 'A5');
    await page.close();
  },
  async errorsResync() {
    const page = await open('dark');
    await page.evaluate(() => { window.__mock.rejectNext = true; });
    await click(page, 'G2');
    await page.keyboard.type('9'); await page.keyboard.press('Enter');
    await page.locator('.sg-toast', { hasText: 'Mock rejected' }).waitFor();
    await settle(page);
    assert.equal(await shown(page, 'G2'), '', 'a rejected edit goes back to what the backend has');
    assert.ok((await commands(page, 'Load')).length >= 1);
    await page.close();
  },
  async threadsListOrphans() {
    const fixture = structuredClone(sample);
    fixture.threads.push({ id: 'gone', anchor: { tab: 't1', range: 'Z9', quote: 'old' }, orphaned: true, severity: 'consider', created: 1700000000, resolved: null, messages: [{ id: 'm', role: 'you', text: 'About cells that were deleted', created: 1700000000, edits: [] }] });
    fixture.threads.push({ id: 'suggest', anchor: { tab: 't1', range: 'C5', quote: '12100' }, orphaned: false, severity: 'should', created: 1700000000, resolved: null, messages: [{ id: 'm2', role: 'model', text: 'Travel looks low.', created: 1700000000, edits: [{ id: 'e1', status: 'pending', revision: 'r1', ops: [{ op: 'set_cells', tab: 'Budget', at: 'C5', values: [['14000']] }, { op: 'format', tab: 'Budget', range: 'C5', patch: { bold: true } }] }] }] });
    const page = await open('dark', undefined, { fixture });
    assert.equal(await page.locator('#sheet-app .sg-pin').count(), 2, 'an orphaned thread has no pin');
    const card = page.locator('#sheet-app .card', { hasText: 'About cells that were deleted' });
    assert.ok((await card.textContent()).includes('deleted'), 'it is listed in the pane');
    const edit = page.locator('#sheet-app .card', { hasText: 'Travel looks low' });
    assert.ok((await edit.textContent()).includes('2 changes: set cells, format'));
    await edit.getByRole('button', { name: 'Apply suggestion' }).click();
    assert.deepEqual((await commands(page, 'ApplyEdit')).at(-1), { command: 'ApplyEdit', owner: 'suggest', edit: 'e1' });
    await page.close();
  },
  async exportTitleAndLibrary() {
    const page = await open('dark');
    await page.getByRole('button', { name: 'Export' }).click();
    await page.getByRole('menuitem', { name: /Excel workbook/ }).click();
    await page.locator('.sg-toast', { hasText: 'Exported q3-budget.xlsx' }).waitFor();
    assert.deepEqual((await commands(page, 'ExportAs')).at(-1), { command: 'ExportAs', format: 'xlsx' });
    // "Save to exports/ without asking" exports the open tab as CSV straight away.
    await page.getByRole('button', { name: 'Export' }).click();
    await page.getByRole('menuitemcheckbox', { name: /without asking/ }).click();
    await page.getByRole('button', { name: 'Export' }).click();
    await page.getByRole('menuitem', { name: /CSV/ }).click();
    await settle(page);
    assert.deepEqual((await commands(page, 'SheetExport')).at(-1), { command: 'SheetExport', format: 'csv', path: null, tab: 'Budget' });
    // Renaming the sheet is one op.
    await page.locator('#sheet-app .sg-title').fill('Q4 budget');
    await page.keyboard.press('Enter'); await settle(page);
    assert.deepEqual(await lastApply(page), { command: 'SheetApply', ops: [{ op: 'set_title', title: 'Q4 budget' }] });
    assert.equal(await page.title(), 'Q4 budget — nexus studio');
    await page.close();
    // The library lists sheets and starts new ones.
    const now = new Date().toISOString();
    const items = [{ kind: 'sheet', relative: 'sheets/q3-budget.sheet.json', title: 'Q3 budget', modified: now, count: 57, design: 'Nexus', thumb: null, current: true }, { kind: 'doc', relative: 'docs/notes.doc.json', title: 'Notes', modified: now, count: 3, design: 'Nexus', thumb: null, current: false }];
    const lib = await open('dark', undefined, { scripts: [`window.__libraryItems = ${JSON.stringify(items)}; window.__libraryCurrent = 'sheets/q3-budget.sheet.json';`, libraryMock] });
    await lib.locator('#sheet-app .crumb-link').click();
    await lib.locator('#library .lib-card').first().waitFor();
    assert.equal(await lib.locator('#library .lib-tab', { hasText: 'Sheets' }).locator('.lib-tab-count').textContent(), '1');
    assert.ok((await lib.locator('#library .lib-card', { hasText: 'Q3 budget' }).textContent()).includes('Sheet'));
    assert.ok((await lib.locator('#library .lib-card', { hasText: 'Q3 budget' }).getByRole('button', { name: /^Open Q3 budget/ }).getAttribute('aria-label')).includes('57 cells'));
    await lib.locator('#library .lib-tab', { hasText: 'Sheets' }).click();
    assert.equal(await lib.locator('#library .lib-card').count(), 1);
    await lib.getByRole('button', { name: 'New sheet' }).click();
    await lib.waitForFunction(() => window.__mock.commands.some(c => c.command === 'NewSheet'));
    assert.deepEqual((await commands(lib, 'NewSheet')).at(-1), { command: 'NewSheet', title: 'Untitled' });
    await lib.close();
  },
};

const only = process.argv.slice(2);
let failed = 0;
for (const [name, run] of Object.entries(tests)) {
  if (only.length && !only.includes(name)) continue;
  try { await run(); console.log(`ok   ${name}`); } catch (error) { failed++; console.log(`FAIL ${name}\n${error.stack}`); }
}
await finish();
if (failed) process.exitCode = 1;
