// Paint exported ratatui cells the way a terminal emulator does.
//
// Terminals do not draw box-drawing, block and braille characters from the
// text font: they rasterize them to fill each cell exactly, so rules join,
// bars sit on the baseline and half blocks split the cell evenly. Drawing
// them with fillText (as the first preview did) leaves gaps, misaligned
// partial bars and faint dots that Nexus never shows. This painter keeps text
// in a monospace font on an integer pixel grid and draws those ranges as
// geometry. Nothing is redesigned: every cell's symbol and colors come from
// the native export.

const ANSI = ['#000000', '#cd0000', '#00cd00', '#cdcd00', '#0000ee', '#cd00cd', '#00cdcd', '#e5e5e5',
  '#7f7f7f', '#ff0000', '#00ff00', '#ffff00', '#5c5cff', '#ff00ff', '#00ffff', '#ffffff'];
const NAMES = ['Black', 'Red', 'Green', 'Yellow', 'Blue', 'Magenta', 'Cyan', 'Gray', 'DarkGray',
  'LightRed', 'LightGreen', 'LightYellow', 'LightBlue', 'LightMagenta', 'LightCyan', 'White'];

export function color(value, fallback) {
  if (!value || value === 'Reset') return fallback;
  if (value.startsWith('Rgb(')) return `rgb(${value.slice(4, -1)})`;
  if (value.startsWith('Indexed(')) {
    const n = Number(value.slice(8, -1));
    if (n < 16) return ANSI[n];
    if (n >= 232) { const v = 8 + (n - 232) * 10; return `rgb(${v},${v},${v})`; }
    const k = n - 16, level = x => (x === 0 ? 0 : 55 + x * 40);
    return `rgb(${level(Math.floor(k / 36))},${level(Math.floor(k / 6) % 6)},${level(k % 6)})`;
  }
  const index = NAMES.indexOf(value);
  return index >= 0 ? ANSI[index] : fallback;
}

// Box drawing: [up, right, down, left], 1 light, 2 heavy, 3 double.
const BOX = {
  '─': [0, 1, 0, 1], '━': [0, 2, 0, 2], '│': [1, 0, 1, 0], '┃': [2, 0, 2, 0],
  '┌': [0, 1, 1, 0], '┐': [0, 0, 1, 1], '└': [1, 1, 0, 0], '┘': [1, 0, 0, 1],
  '┏': [0, 2, 2, 0], '┓': [0, 0, 2, 2], '┗': [2, 2, 0, 0], '┛': [2, 0, 0, 2],
  '├': [1, 1, 1, 0], '┤': [1, 0, 1, 1], '┬': [0, 1, 1, 1], '┴': [1, 1, 0, 1], '┼': [1, 1, 1, 1],
  '┣': [2, 2, 2, 0], '┫': [2, 0, 2, 2], '┳': [0, 2, 2, 2], '┻': [2, 2, 0, 2], '╋': [2, 2, 2, 2],
  '┝': [1, 2, 1, 0], '┥': [1, 0, 1, 2], '┯': [0, 2, 1, 2], '┷': [1, 2, 0, 2], '┿': [1, 2, 1, 2],
  '═': [0, 3, 0, 3], '║': [3, 0, 3, 0], '╔': [0, 3, 3, 0], '╗': [0, 0, 3, 3], '╚': [3, 3, 0, 0],
  '╝': [3, 0, 0, 3], '╠': [3, 3, 3, 0], '╣': [3, 0, 3, 3], '╦': [0, 3, 3, 3], '╩': [3, 3, 0, 3],
  '╬': [3, 3, 3, 3], '╞': [1, 3, 1, 0], '╡': [1, 0, 1, 3], '╤': [0, 3, 1, 3], '╧': [1, 3, 0, 3],
  '╴': [0, 0, 0, 1], '╵': [1, 0, 0, 0], '╶': [0, 1, 0, 0], '╷': [0, 0, 1, 0],
  '╸': [0, 0, 0, 2], '╹': [2, 0, 0, 0], '╺': [0, 2, 0, 0], '╻': [0, 0, 2, 0],
  '╼': [0, 2, 0, 1], '╽': [1, 0, 2, 0], '╾': [0, 1, 0, 2], '╿': [2, 0, 1, 0],
};
// Dashed rules: [horizontal?, weight, dashes].
const DASH = {
  '╌': [true, 1, 2], '╍': [true, 2, 2], '╎': [false, 1, 2], '╏': [false, 2, 2],
  '┄': [true, 1, 3], '┅': [true, 2, 3], '┆': [false, 1, 3], '┇': [false, 2, 3],
  '┈': [true, 1, 4], '┉': [true, 2, 4], '┊': [false, 1, 4], '┋': [false, 2, 4],
};
const ARC = { '╭': [0, 1, 1, 0], '╮': [0, 0, 1, 1], '╯': [1, 0, 0, 1], '╰': [1, 1, 0, 0] };

// Block elements as filled rectangles in eighths: [x0, y0, x1, y1].
const BLOCK = {
  '▀': [[0, 0, 8, 4]], '▄': [[0, 4, 8, 8]], '█': [[0, 0, 8, 8]], '▌': [[0, 0, 4, 8]], '▐': [[4, 0, 8, 8]],
  '▔': [[0, 0, 8, 1]], '▕': [[7, 0, 8, 8]],
  '▖': [[0, 4, 4, 8]], '▗': [[4, 4, 8, 8]], '▘': [[0, 0, 4, 4]], '▝': [[4, 0, 8, 4]],
  '▙': [[0, 0, 4, 8], [4, 4, 8, 8]], '▚': [[0, 0, 4, 4], [4, 4, 8, 8]], '▛': [[0, 0, 8, 4], [0, 4, 4, 8]],
  '▜': [[0, 0, 8, 4], [4, 4, 8, 8]], '▞': [[4, 0, 8, 4], [0, 4, 4, 8]], '▟': [[4, 0, 8, 8], [0, 4, 4, 8]],
};
for (let i = 1; i <= 7; i++) {
  BLOCK[String.fromCodePoint(0x2580 + i)] = [[0, 8 - i, 8, 8]]; // ▁..▇ lower eighths
  BLOCK[String.fromCodePoint(0x2590 - i)] = [[0, 0, i, 8]]; // ▏..▉ left eighths
}
const SHADE = { '░': 0.25, '▒': 0.5, '▓': 0.75 };

export function metrics(fontSize, family) {
  const ratio = window.devicePixelRatio || 1;
  const probe = document.createElement('canvas').getContext('2d');
  probe.font = `${fontSize}px ${family}`;
  // Snap cells to whole device pixels so every cell edge is crisp.
  const width = Math.round(probe.measureText('M'.repeat(20)).width / 20 * ratio) / ratio;
  const height = Math.round(fontSize * 1.32 * ratio) / ratio;
  return { fontSize, family, width, height, ratio };
}

function boxGlyph(ctx, char, x, y, w, h, light) {
  const cx = Math.round(x + w / 2), cy = Math.round(y + h / 2);
  const heavy = Math.max(2, Math.round(light * 2));
  const line = (weight, x0, y0, x1, y1) => {
    if (!weight) return;
    if (weight === 3) {
      const gap = Math.max(2, Math.round(light * 1.5));
      if (y0 === y1) { ctx.fillRect(Math.min(x0, x1), y0 - gap - light / 2, Math.abs(x1 - x0), light); ctx.fillRect(Math.min(x0, x1), y0 + gap - light / 2, Math.abs(x1 - x0), light); }
      else { ctx.fillRect(x0 - gap - light / 2, Math.min(y0, y1), light, Math.abs(y1 - y0)); ctx.fillRect(x0 + gap - light / 2, Math.min(y0, y1), light, Math.abs(y1 - y0)); }
      return;
    }
    const t = weight === 2 ? heavy : light;
    if (y0 === y1) ctx.fillRect(Math.min(x0, x1), y0 - Math.floor(t / 2), Math.abs(x1 - x0), t);
    else ctx.fillRect(x0 - Math.floor(t / 2), Math.min(y0, y1), t, Math.abs(y1 - y0));
  };
  if (BOX[char]) {
    const [u, r, d, l] = BOX[char];
    const reach = Math.max(u, r, d, l) === 3 ? Math.round(light * 2) : Math.ceil(heavy / 2);
    line(u, cx, y, cx, cy + (u ? reach : 0));
    line(d, cx, cy - (d ? reach : 0), cx, y + h);
    line(l, x, cy, cx + (l ? reach : 0), cy);
    line(r, cx - (r ? reach : 0), cy, x + w, cy);
    return true;
  }
  if (DASH[char]) {
    const [horizontal, weight, count] = DASH[char];
    const t = weight === 2 ? heavy : light;
    const span = horizontal ? w : h, step = span / count, dash = step * 0.6;
    for (let i = 0; i < count; i++) {
      const start = (horizontal ? x : y) + i * step + (step - dash) / 2;
      if (horizontal) ctx.fillRect(start, cy - Math.floor(t / 2), dash, t);
      else ctx.fillRect(cx - Math.floor(t / 2), start, t, dash);
    }
    return true;
  }
  if (ARC[char]) {
    const [u, r, d, l] = ARC[char];
    const rad = Math.min(w, h) / 2;
    ctx.save();
    ctx.lineWidth = light;
    ctx.strokeStyle = ctx.fillStyle;
    ctx.beginPath();
    const ex = r ? x + w : x, vy = u ? y : y + h;
    ctx.moveTo(ex, cy);
    ctx.lineTo(cx + (r ? rad : -rad), cy);
    ctx.arcTo(cx, cy, cx, cy + (d ? rad : -rad), rad);
    ctx.lineTo(cx, vy);
    ctx.stroke();
    ctx.restore();
    void l;
    return true;
  }
  return false;
}

function blockGlyph(ctx, char, x, y, w, h, bg) {
  const rects = BLOCK[char];
  if (rects) {
    for (const [x0, y0, x1, y1] of rects) {
      const left = Math.round(x + (w * x0) / 8), right = Math.round(x + (w * x1) / 8);
      const top = Math.round(y + (h * y0) / 8), bottom = Math.round(y + (h * y1) / 8);
      ctx.fillRect(left, top, right - left, bottom - top);
    }
    return true;
  }
  if (SHADE[char]) {
    ctx.save();
    ctx.globalAlpha = SHADE[char];
    ctx.fillRect(x, y, w, h);
    ctx.restore();
    void bg;
    return true;
  }
  const code = char.codePointAt(0);
  // Powerline round caps: half ellipses spanning the whole cell, flat side
  // against the shape they close (as Ghostty, kitty and WezTerm draw them).
  if (code === 0xe0b6 || code === 0xe0b4) {
    const left = code === 0xe0b6;
    ctx.beginPath();
    ctx.ellipse(left ? x + w : x, y + h / 2, w, h / 2, 0, left ? Math.PI / 2 : -Math.PI / 2, left ? Math.PI * 1.5 : Math.PI / 2);
    ctx.closePath();
    ctx.fill();
    return true;
  }
  if (code >= 0x2800 && code <= 0x28ff) {
    const bits = code - 0x2800;
    const map = [[0, 0], [0, 1], [0, 2], [1, 0], [1, 1], [1, 2], [0, 3], [1, 3]];
    const dot = Math.max(1.5, w / 4.2);
    map.forEach(([col, row], bit) => {
      if (!(bits & (1 << bit))) return;
      const px = x + w * (col === 0 ? 0.3 : 0.7), py = y + h * (0.14 + row * 0.24);
      ctx.beginPath();
      ctx.arc(px, py, dot / 2, 0, Math.PI * 2);
      ctx.fill();
    });
    return true;
  }
  return false;
}

/**
 * Paint `frame` (an exported cell grid) into `canvas` at an exact cell size.
 * The canvas is sized in CSS pixels to whole cells and never scaled, so text
 * stays as crisp as it is in a terminal at that font size.
 */
export function paintFrame(canvas, frame, cell) {
  const { width: cw, height: ch, ratio, fontSize, family } = cell;
  canvas.width = Math.round(frame.width * cw * ratio);
  canvas.height = Math.round(frame.height * ch * ratio);
  canvas.style.width = `${frame.width * cw}px`;
  canvas.style.height = `${frame.height * ch}px`;
  const ctx = canvas.getContext('2d');
  ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
  const foreground = color(frame.foreground, '#dce2e4');
  const background = color(frame.background, '#0d1218');
  ctx.fillStyle = background;
  ctx.fillRect(0, 0, frame.width * cw, frame.height * ch);
  const light = Math.max(1, Math.round(fontSize / 14 * ratio) / ratio);
  const baseline = Math.round((ch - fontSize) / 2 + fontSize * 0.8);
  frame.rows.forEach((runs, row) => runs.forEach(run => {
    let fg = color(run.fg, foreground), bg = color(run.bg, background);
    if (run.reverse) [fg, bg] = [bg, fg];
    if (run.dim) fg = `color-mix(in srgb, ${fg} 55%, ${bg})`;
    const chars = [...run.text];
    const y = row * ch;
    // Cell backgrounds are whole-pixel rectangles so adjacent fills join.
    ctx.fillStyle = bg;
    ctx.fillRect(Math.round(run.x * cw), y, Math.round((run.x + chars.length) * cw) - Math.round(run.x * cw), ch);
    ctx.font = `${run.italic ? 'italic ' : ''}${run.bold ? 'bold ' : ''}${fontSize}px ${family}`;
    chars.forEach((char, i) => {
      if (char === ' ') return;
      const x = Math.round((run.x + i) * cw), w = Math.round((run.x + i + 1) * cw) - x;
      ctx.fillStyle = fg;
      if (boxGlyph(ctx, char, x, y, w, ch, light) || blockGlyph(ctx, char, x, y, w, ch, bg)) return;
      ctx.save();
      ctx.beginPath();
      ctx.rect(x, y, w * 2, ch);
      ctx.clip();
      ctx.fillText(char, x, y + baseline);
      ctx.restore();
    });
    if (run.underline) {
      ctx.fillStyle = fg;
      ctx.fillRect(Math.round(run.x * cw), y + baseline + Math.max(2, Math.round(fontSize / 8)), Math.round(chars.length * cw), light);
    }
  }));
}
