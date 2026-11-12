//! Braille rasterizer for `Canvas` and chart plots: a 2×4 dot grid per cell,
//! roughly square on the terminal. The last shape to touch a cell picks its color.

use super::safe;
use crate::theme::Theme;
use nexus_core::interactive::{Drawn, Session, Shape};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use unicode_width::UnicodeWidthStr;

/// Braille bit for the dot at `[row][column]` within one cell.
const BITS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];
/// First codepoint of the Unicode braille block (no dots raised).
const BRAILLE: u32 = 0x2800;

/// A dot grid covering `cols`×`rows` terminal cells.
pub(super) struct Dots {
    cols: u16,
    rows: u16,
    cells: Vec<(u8, Option<Color>)>,
    /// Cells raised since the last `take_touched`: left, top, right, bottom.
    touched: Option<(u16, u16, u16, u16)>,
}

impl Dots {
    pub(super) fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cells: vec![(0, None); usize::from(cols) * usize::from(rows)],
            touched: None,
        }
    }
    pub(super) fn take_touched(&mut self, area: Rect) -> Option<Rect> {
        self.touched.take().map(|(left, top, right, bottom)| {
            Rect::new(
                area.x + left,
                area.y + top,
                right - left + 1,
                bottom - top + 1,
            )
        })
    }
    pub(super) fn width(&self) -> f64 {
        f64::from(self.cols) * 2.0
    }
    pub(super) fn height(&self) -> f64 {
        f64::from(self.rows) * 4.0
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub(super) fn set(&mut self, x: f64, y: f64, color: Color) {
        let (x, y) = (x.round(), y.round());
        if !(0.0..self.width()).contains(&x) || !(0.0..self.height()).contains(&y) {
            return;
        }
        let (x, y) = (x as usize, y as usize);
        let (col, row) = (
            u16::try_from(x / 2).unwrap_or(u16::MAX),
            u16::try_from(y / 4).unwrap_or(u16::MAX),
        );
        self.touched = Some(self.touched.map_or((col, row, col, row), |(l, t, r, b)| {
            (l.min(col), t.min(row), r.max(col), b.max(row))
        }));
        let cell = &mut self.cells[(y / 4) * usize::from(self.cols) + x / 2];
        cell.0 |= BITS[y % 4][x % 2];
        cell.1 = Some(color);
    }
    /// A straight segment, clipped to the grid first so far-off endpoints cost nothing.
    pub(super) fn line(&mut self, from: (f64, f64), to: (f64, f64), color: Color) {
        let Some(((x0, y0), (x1, y1))) = self.clip(from, to) else {
            return;
        };
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).ceil().max(1.0);
        let mut step = 0.0;
        while step <= steps {
            let t = step / steps;
            self.set(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t, color);
            step += 1.0;
        }
    }
    /// Liang–Barsky clip against the grid, padded by a dot for rounding.
    fn clip(&self, (x0, y0): (f64, f64), (x1, y1): (f64, f64)) -> Option<((f64, f64), (f64, f64))> {
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
            return None;
        }
        let (dx, dy) = (x1 - x0, y1 - y0);
        let (mut low, mut high) = (0.0_f64, 1.0_f64);
        for (p, q) in [
            (-dx, x0 + 1.0),
            (dx, self.width() - x0),
            (-dy, y0 + 1.0),
            (dy, self.height() - y0),
        ] {
            if p == 0.0 {
                if q < 0.0 {
                    return None;
                }
            } else {
                let r = q / p;
                if p < 0.0 {
                    low = low.max(r);
                } else {
                    high = high.min(r);
                }
            }
        }
        (low <= high).then_some({
            (
                (x0 + dx * low, y0 + dy * low),
                (x0 + dx * high, y0 + dy * high),
            )
        })
    }
    fn each_in(&self, left: f64, top: f64, right: f64, bottom: f64) -> Vec<(f64, f64)> {
        let left = left.floor().max(0.0);
        let top = top.floor().max(0.0);
        let right = right.ceil().min(self.width() - 1.0);
        let bottom = bottom.ceil().min(self.height() - 1.0);
        let mut points = Vec::new();
        let mut y = top;
        while y <= bottom {
            let mut x = left;
            while x <= right {
                points.push((x, y));
                x += 1.0;
            }
            y += 1.0;
        }
        points
    }
    pub(super) fn circle(&mut self, (cx, cy): (f64, f64), r: f64, fill: bool, color: Color) {
        if r < 0.5 {
            self.set(cx, cy, color);
            return;
        }
        for (x, y) in self.each_in(cx - r - 1.0, cy - r - 1.0, cx + r + 1.0, cy + r + 1.0) {
            let distance = (x - cx).hypot(y - cy);
            if distance <= r + 0.5 && (fill || distance >= r - 0.5) {
                self.set(x, y, color);
            }
        }
    }
    /// `sweep` degrees of a circle's outline, clockwise from `start`
    /// (degrees clockwise from twelve o'clock).
    pub(super) fn arc(
        &mut self,
        (cx, cy): (f64, f64),
        r: f64,
        start: f64,
        sweep: f64,
        color: Color,
    ) {
        if r <= 0.0 || sweep.abs() < f64::EPSILON {
            return;
        }
        // About one sample per dot along the curve.
        let steps = (r * sweep.abs().to_radians()).ceil().max(1.0);
        let mut step = 0.0;
        while step <= steps {
            let angle = (start + sweep * step / steps).to_radians();
            self.set(cx + r * angle.sin(), cy - r * angle.cos(), color);
            step += 1.0;
        }
    }
    pub(super) fn rect(
        &mut self,
        (x, y): (f64, f64),
        (w, h): (f64, f64),
        fill: bool,
        color: Color,
    ) {
        if fill {
            for (px, py) in self.each_in(x, y, x + w, y + h) {
                self.set(px, py, color);
            }
        } else {
            let corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
            for index in 0..4 {
                self.line(corners[index], corners[(index + 1) % 4], color);
            }
        }
    }
    pub(super) fn poly(&mut self, points: &[(f64, f64)], fill: bool, color: Color) {
        if fill && points.len() > 2 {
            let (mut left, mut top) = (f64::MAX, f64::MAX);
            let (mut right, mut bottom) = (f64::MIN, f64::MIN);
            for &(x, y) in points {
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
            }
            for (x, y) in self.each_in(left, top, right, bottom) {
                if inside(points, x, y) {
                    self.set(x, y, color);
                }
            }
        }
        for index in 0..points.len() {
            self.line(points[index], points[(index + 1) % points.len()], color);
        }
    }
    pub(super) fn shade(&mut self, points: &[(f64, f64)], color: Color) {
        if points.len() < 3 {
            return;
        }
        let (mut left, mut top) = (f64::MAX, f64::MAX);
        let (mut right, mut bottom) = (f64::MIN, f64::MIN);
        for &(x, y) in points {
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
        }
        for (x, y) in self.each_in(left, top, right, bottom) {
            if (x + y) % 2.0 == 0.0 && inside(points, x, y) {
                self.set(x, y, color);
            }
        }
    }
    /// Write raised cells into `area`; untouched cells keep their content.
    pub(super) fn render(&self, area: Rect, buffer: &mut Buffer) {
        for row in 0..self.rows.min(area.height) {
            for col in 0..self.cols.min(area.width) {
                let (bits, color) =
                    self.cells[usize::from(row) * usize::from(self.cols) + usize::from(col)];
                if bits == 0 {
                    continue;
                }
                let Some(glyph) = char::from_u32(BRAILLE + u32::from(bits)) else {
                    continue;
                };
                if let Some(cell) = buffer.cell_mut((area.x + col, area.y + row)) {
                    cell.set_char(glyph);
                    if let Some(color) = color {
                        cell.set_fg(color);
                    }
                }
            }
        }
    }
}

fn inside(points: &[(f64, f64)], x: f64, y: f64) -> bool {
    let mut result = false;
    let mut previous = points[points.len() - 1];
    for &current in points {
        let ((x1, y1), (x2, y2)) = (previous, current);
        if (y1 > y) != (y2 > y) && x < (x2 - x1) * (y - y1) / (y2 - y1) + x1 {
            result = !result;
        }
        previous = current;
    }
    result
}

/// Draw a canvas's shapes at the session's clock, returning each clickable shape's cell box.
pub(super) fn draw(
    shapes: &[Shape],
    session: &Session,
    area: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) -> Vec<(usize, Rect)> {
    let mut targets = Vec::new();
    if area.width == 0 || area.height == 0 {
        return targets;
    }
    let mut dots = Dots::new(area.width, area.height);
    // 0–100 canvas units onto dot centres; radii and ray lengths use the
    // vertical scale so they stay round.
    let sx = (dots.width() - 1.0) / 100.0;
    let sy = (dots.height() - 1.0) / 100.0;
    let at = |x: f64, y: f64| (x * sx, y * sy);
    let frame = session.canvas_frame(shapes);
    let mut labels = Vec::new();
    for (index, drawn, tone) in frame {
        let color = super::components::color(tone, session, theme);
        let clickable = shapes[index].action().is_some();
        dots.take_touched(area);
        match drawn {
            Drawn::Circle { x, y, r, fill } => dots.circle(at(x, y), r * sy, fill, color),
            Drawn::Rect { x, y, w, h, fill } => dots.rect(at(x, y), (w * sx, h * sy), fill, color),
            Drawn::Line { x1, y1, x2, y2 } => dots.line(at(x1, y1), at(x2, y2), color),
            Drawn::Ray { x, y, len, angle } => {
                let (cx, cy) = at(x, y);
                let radians = angle.to_radians();
                let reach = len * sy;
                dots.line(
                    (cx, cy),
                    (cx + reach * radians.sin(), cy - reach * radians.cos()),
                    color,
                );
            }
            Drawn::Arc {
                x,
                y,
                r,
                start,
                sweep,
            } => dots.arc(at(x, y), r * sy, start, sweep, color),
            Drawn::Dot { x, y } => {
                let (x, y) = at(x, y);
                dots.set(x, y, color);
            }
            Drawn::Poly { points, fill } => {
                let points: Vec<_> = points.into_iter().map(|(x, y)| at(x, y)).collect();
                dots.poly(&points, fill, color);
            }
            Drawn::Label { text, x, y } => {
                labels.push((index, clickable, safe(&text), at(x, y), color));
            }
        }
        if clickable && let Some(rect) = dots.take_touched(area) {
            targets.push((index, rect));
        }
    }
    // Draw in left to right the first time the panel shows.
    let shown = super::bold::revealed(area.width, session);
    dots.render(Rect::new(area.x, area.y, shown, area.height), buffer);
    // Text sits over the dots, centred on its point.
    for (index, clickable, text, (x, y), color) in labels {
        let row = (y / 4.0).floor();
        let width = text.width();
        #[allow(clippy::cast_precision_loss)]
        let col = (x / 2.0 - width as f64 / 2.0).round().max(0.0);
        if !(0.0..f64::from(area.height)).contains(&row) || col >= f64::from(shown) {
            continue;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (row, col) = (row as u16, col as u16);
        buffer.set_stringn(
            area.x + col,
            area.y + row,
            &text,
            usize::from(area.width - col),
            Style::default().fg(color),
        );
        if clickable {
            let width = u16::try_from(text.width())
                .unwrap_or(u16::MAX)
                .min(area.width - col);
            targets.push((index, Rect::new(area.x + col, area.y + row, width, 1)));
        }
    }
    targets.sort_by_key(|(index, _)| *index);
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(dots: &Dots) -> String {
        let mut buffer = Buffer::empty(Rect::new(0, 0, dots.cols, dots.rows));
        dots.render(buffer.area, &mut buffer);
        (0..dots.rows)
            .map(|y| {
                (0..dots.cols)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn dots_pack_into_braille_cells() {
        let mut dots = Dots::new(2, 1);
        dots.set(0.0, 0.0, Color::Red);
        dots.set(3.0, 3.0, Color::Red);
        dots.set(9.0, 9.0, Color::Red);
        dots.set(-1.0, 0.0, Color::Red);
        assert_eq!(picture(&dots), "\u{2801}\u{2880}");
    }

    #[test]
    fn lines_clip_to_the_grid_and_fill_every_step() {
        let mut dots = Dots::new(2, 1);
        dots.line((-1e12, 0.0), (1e12, 0.0), Color::Red);
        assert_eq!(picture(&dots), "\u{2809}\u{2809}");
        let mut dots = Dots::new(1, 1);
        dots.line((0.0, 0.0), (0.0, 3.0), Color::Red);
        assert_eq!(picture(&dots), "\u{2847}");
        let mut dots = Dots::new(1, 1);
        dots.line((50.0, 50.0), (60.0, 60.0), Color::Red);
        assert_eq!(picture(&dots), " ");
    }

    const BALL: &str = r#"root = Panel("Ball", [art])
art = Canvas([
  Rect(0, 0, 100, 100, "muted"),
  Circle(10, 50, 20, "warning", true, [Anim("x", [10, 90], 2, "bounce", "linear")]),
  Label("hi", 50, 0, "normal")
], 4, "A ball bouncing in a box")"#;

    fn canvas_rows(entry: &super::super::Entry, width: u16) -> Vec<String> {
        let (buffer, _) = entry.canvas(width, &Theme::default(), false, false);
        (2..6)
            .map(|y| {
                (2..width - 2)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }
    fn ball_column(rows: &[String]) -> usize {
        rows[1]
            .chars()
            .enumerate()
            .skip(1)
            .find(|&(_, ch)| ch != ' ' && ch != '\u{28b8}')
            .expect("ball drawn")
            .0
    }

    #[test]
    fn canvas_draws_shapes_labels_and_moves_with_the_clock() {
        let mut entry = super::super::Entry::new(
            Session::new(nexus_core::interactive::Document::parse(BALL).unwrap())
                .map_err(|error| error.to_string()),
        );
        let start = canvas_rows(&entry, 40);
        assert!(start[0].contains("hi"), "{start:?}");
        assert!(
            start[3]
                .chars()
                .all(|ch| ('\u{2800}'..='\u{28ff}').contains(&ch))
        );
        let left = ball_column(&start);
        entry.session.as_mut().unwrap().set_clock(2.0);
        let right = ball_column(&canvas_rows(&entry, 40));
        assert!(right > left + 20, "ball moved from {left} to {right}");
        let narrow = canvas_rows(&entry, 24);
        let wide = canvas_rows(&entry, 40);
        let ball = |rows: &[String]| {
            rows[1]
                .chars()
                .filter(|&ch| ch != ' ' && ch != '\u{28b8}')
                .count()
        };
        assert_eq!(ball(&narrow), ball(&wide));
        assert!(entry.session.as_ref().unwrap().animating());
    }

    #[test]
    fn widgets_request_frames_only_for_moving_canvases_in_focus() {
        let mut widgets = super::super::Widgets::default();
        let ball = super::super::tests::placement(BALL, 1);
        let _ = super::super::tests::draw(&mut widgets, &[ball], 0);
        assert!(widgets.animating());
        widgets.window_focus(false);
        assert!(!widgets.animating());
        let mut widgets = super::super::Widgets::default();
        let still = super::super::tests::placement(
            r#"root = Panel("Still", [Canvas([Line(0, 0, 100, 100)])])"#,
            2,
        );
        let _ = super::super::tests::draw(&mut widgets, &[still], 0);
        assert!(!widgets.animating());
    }

    #[test]
    fn filled_shapes_cover_their_interior() {
        let mut dots = Dots::new(2, 1);
        dots.rect((0.0, 0.0), (3.0, 3.0), true, Color::Red);
        assert_eq!(picture(&dots), "\u{28ff}\u{28ff}");
        let mut dots = Dots::new(4, 2);
        dots.poly(&[(0.0, 0.0), (7.0, 0.0), (0.0, 7.0)], true, Color::Red);
        let filled = picture(&dots);
        assert!(filled.starts_with('\u{28ff}'), "{filled}");
        assert!(filled.ends_with(' '), "{filled}");
    }
}
