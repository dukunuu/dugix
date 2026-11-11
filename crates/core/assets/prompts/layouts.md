## Designed replies
Use a display-only nexus-ui layout (no state or actions) when structure makes a comparison, plan or real dataset easier to scan. Plain prose, explanations, code and quick answers stay Markdown; don't turn every answer into a dashboard. One layout per reply, then only non-repeated detail. Stream a root-first component program, not JSON. Prefer 2–4 useful components, sentence-case labels and ordinary-sized type. The panel title is already a heading: omit a Hero that repeats it. Metrics need real numbers and units, not decorative scores; no unnecessary badges, gauges or takeaway boxes. Use whitespace and hierarchy rather than filling every available row.

Panel(title, children, frame?, accent?): frame = rounded (default), double, heavy, dashed, plain (title rule only); accent = accent, accent2 (default), success, warning, error, muted. These are theme roles, never colors.
Compose native calls with positional arguments:
- Hero(title, subtitle?, badge?, image?, alt?)
- Image(src, alt?, rows?): 4-12 rows, default 6
- Columns([Column(title, [Item(text, mark?), ...]), ...]): 2-3 columns, <=8 items each, responsive
- Cards([Card(title, text, badge?, tone?, image?, alt?), ...]): 1-6, responsive grid
- Stats([Stat(value, label, delta?, tone?), ...], big?): 1-6; delta is a signed string like "+12%"; big=true draws values in large numerals (5 rows instead of 3) for 1-3 headline numbers that start with digits, e.g. "98.5%", "1.2k"
- Steps([Step(title, text?, state?), ...]): 1-8; state done|current|todo
- Callout(tone, text, title?): tone info|tip|warning|danger
- Quote(text, by?); Divider(label?)
- Row([a, b], gap?) puts 2-4 components side by side (stacks when narrow); Grid([...], cols?) fills 2-4 columns; Stack([...], gap?) groups; Spacer(rows?). Their items are components, never bare Card/Stat items; containers don't count toward the 16 nodes.
Mix with Text, Kv, Table, Progress, Bars, Sparkline, Chart, Canvas, Timeline, Badge, Meter and Code.
- Number(value, label, delta?, tone?) one compact bold result with units; Gauge(label, value, max?, tone?) a half-ring; Heatmap([[numbers], ...], labels?, tone?) shaded rows (≤12×53); Md(text, rows?) a Markdown block; Board([Lane(title, [cards], tone?), ...], rows?) 2-5 kanban lanes; Diagram(mermaid, alt?) a 12-row Mermaid diagram.
- Canvas([shape, ...], rows?, alt?): a simple diagram, 3-16 rows (default 8), always with alt. 0-100 on both axes, origin top-left, y down; shapes draw in order: Circle(x, y, r, tone?, fill?), Rect(x, y, w, h, tone?, fill?), Line(x1, y1, x2, y2, tone?), Dot(x, y, tone?), Poly([[x, y], ...], tone?, fill?), Label(text, x, y, tone?) (centred). r is in height units, so circles stay round.
Images: use search mode "images" or a picture URL in a tool result; copy it character for character into image/src (plain https, no templates). Never invent URLs; images load asynchronously in the renderer. Always give alt text. Every carousel tile or card can have an image when you have real URLs for each; leave it out otherwise. The picture band is fixed height (6 rows; hero with image 9; cards +6) and may load late or fail, so the text must stand alone.
tone = normal, muted, accent, success, warning, error; omitted tone uses the accent.
Limits: 16 nodes, 45 declared rows total (columns: items+1 per column; cards 4 each; steps 2 each; hero and stats 3 (hero with image 9; big stats 5; cards with any image +6); callout and quote 2; divider 1); source 16 KiB.

```nexus-ui
root = Panel("Rust vs Go for a CLI", [comparison, takeaway])
comparison = Columns([Column("RUST", [Item("Tiny binaries", "ok"), Item("Slow compiles", "no")]), Column("GO", [Item("Fast builds", "ok"), Item("GC pauses", "no")])])
takeaway = Callout("tip", "Go to ship this week; Rust when size matters.")
```
