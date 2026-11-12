//! One terminal-native contact sheet. These are the actual renderer's cells,
//! not browser approximations. Export is opt-in; normal tests stay hermetic.

use super::*;
use ratatui::backend::{Backend, CrosstermBackend};
use serde_json::json;

const COLUMN_WIDTH: u16 = 46;
const GAP: u16 = 3;
const WIDTH: u16 = 4 * COLUMN_WIDTH + 3 * GAP + 4;

const COLUMNS: &[(&str, &[&str])] = &[
    (
        "01 / Make it yours",
        &[
            r#"root = Panel("Form · Input · Textarea", [form])
$name = "A little adventure"
$notes = "A slow morning. Somewhere green."
form = Form("A page of your own", [Input($name, "Name"), Textarea($notes, "A thought", 2)], "Keep this thought", @ToAssistant("A thought"))"#,
            r#"root = Panel("Select · Stepper · Toggle · Slider · Rating", [mode, days, slow, sun, stars])
$mode = "Woodland"
$days = 3
$slow = true
$sun = 8
$stars = 4
mode = Select($mode, "Somewhere", ["Woodland", "Coast", "Hills"])
days = Stepper($days, "Days", 1, 14, 1)
slow = Toggle($slow, "Take the long way")
sun = Slider($sun, "Daylight", 4, 16, 1)
stars = Rating($stars, "Rating", 5)"#,
            r#"root = Panel("Checklist · Check · Choice", [pack, pick])
$tea = true
$book = false
$pick = "The pine loop"
pack = Checklist([Check("Something warm to drink", $tea), Check("A notebook", $book)])
pick = Choice($pick, "A place to begin", ["The pine loop", "Along the water"])"#,
            r#"root = Panel("Buttons · Button", [Buttons([Button("Make a plan", @ToAssistant("Make a plan"), true), Button("Surprise me", @Reset($n))])])
$n = 0"#,
            r#"root = Panel("Tabs · Tab · Accordion · Section", [tabs, more])
tabs = Tabs([Tab("The idea", [Text("Somewhere slow. A little water.")]), Tab("Details", [Text("Two nights. One good walk.")])])
more = Accordion([Section("What if it rains?", [Text("Find a window seat. Stay a little.")], true)])"#,
            r#"root = Panel("Editor · Code", [editor, snippet])
$code = "def wander():\n    return 'somewhere quiet'"
editor = Editor($code, "A little script", "python", 3)
snippet = Code("const pace = 'unhurried';", "javascript")"#,
        ],
    ),
    (
        "02 / A little perspective",
        &[
            r#"root = Panel("Stats · Stat · Number", [stats, result])
stats = Stats([Stat("12.4 km", "on foot"), Stat("640 m", "above the rush"), Stat("3.5 h", "no hurry")])
result = Number("€24.00", "per person · a picnic for three")"#,
            r#"root = Panel("Table · Kv", [table, details])
$row = 0
table = Table(["Walk", "Time", "Mood"], [["Pine loop", "2.5 h", "Quiet"], ["The water", "1.5 h", "Gentle"]], $row)
details = Kv([["Somewhere", "The foothills"], ["Bring", "A little curiosity"]])"#,
            r#"root = Panel("Progress · Meter · Bars · Bar", [progress, energy, bars])
progress = Progress("Journal", 4, 10)
energy = Meter("Energy", 0.68, 1, "success")
bars = Bars([Bar("Walking", 7), Bar("Wandering", 5), Bar("Doing less", 9)])"#,
            r#"root = Panel("Sparkline · Chart · Series", [rise, view])
rise = Sparkline([2, 3, 2, 5, 4, 7, 6, 8], 2)
view = Chart("area", [Series("Elevation", [2, 3, 2, 5, 4, 7, 6, 8], "accent")], ["The start", "The view"], 4)"#,
            r#"root = Panel("Gauge · Heatmap", [gauge, days])
gauge = Gauge("Room for daylight", 0.68, 1, "warning")
days = Heatmap([[0, 1, 3, 5, 2, 0, 1, 4], [2, 4, 6, 8, 3, 1, 0, 3], [1, 1, 2, 9, 7, 2, 1, 6]], ["Mon", "Tue", "Wed"])"#,
            r#"root = Panel("Board · Lane", [Board([Lane("Ideas", ["A small guide", "A long walk"]), Lane("Growing", ["Field notes"]), Lane("Made", ["A good day"], "success")], 2)])"#,
        ],
    ),
    (
        "03 / Tell a small story",
        &[
            r#"root = Panel("Hero · Text · Badge", [hero, thought, badge])
hero = Hero("Take the scenic route.", "The detour is the whole point.")
thought = Text("Leave room for something unplanned.")
badge = Badge("A LITTLE ADVENTUROUS", "muted")"#,
            r#"root = Panel("Cards · Card", [Cards([Card("A quieter kind of day", "Walk. Wander. Find a good tree.", "FIELD NOTE", "muted")])])"#,
            r#"root = Panel("Columns · Column · Item", [Columns([Column("The coast", [Item("Salt in the air", "dot"), Item("A swim, maybe", "ok")]), Column("The hills", [Item("Pine underfoot", "dot"), Item("A view worth it", "ok")])])])"#,
            r#"root = Panel("Quote · Divider · Callout", [quote, Divider("a little room"), note])
quote = Quote("Attention is the beginning of devotion.", "Mary Oliver")
note = Callout("tip", "Not every good day needs a plan.", "A thought to keep")"#,
            r#"root = Panel("Steps · Step · Timeline · Event", [steps, time])
steps = Steps([Step("Notice", null, "done"), Step("Wonder", null, "current"), Step("Make", null, "todo")])
time = Timeline([Event("08:30", "Coffee. A map. A possibility."), Event("11:00", "The view you didn't expect.")])"#,
            r#"root = Panel("Md · Row · Grid · Stack · Spacer", [words, row, grid, stack, Spacer(1)])
words = Md("**Pack light.** Notice more.\nKeep your `curiosity` close.", 2)
row = Row([Text("One good question."), Text("A little room to explore.")])
grid = Grid([Text("01 / Observe"), Text("02 / Wonder")], 2)
stack = Stack([Text("Then follow the thread."), Text("See what happens.")], 0)"#,
        ],
    ),
    (
        "04 / A little wonder",
        &[
            r#"root = Panel("Canvas · Circle · Ray · Anim", [scene])
scene = Canvas([Circle(50, 50, 40, "muted"), Circle(50, 50, 9, "warning", true), Circle(50, 10, 4, "accent", true), Ray(50, 50, 38, 20, "accent", [Anim("angle", [0, 360], 6, "loop", "linear")]), Label("A world of your own", 50, 98, "muted")], 8, "A small orbital scene")"#,
            r#"root = Panel("Rect · Line · Arc · Dot · Poly · Label", [shapes])
shapes = Canvas([Rect(4, 16, 27, 55, "muted"), Line(0, 82, 100, 82, "muted"), Arc(50, 48, 26, 0, 260, "accent"), Dot(50, 48, "warning"), Poly([[72, 70], [84, 18], [96, 70]], "success"), Label("planes", 18, 94), Label("orbits", 50, 94), Label("a peak", 84, 94)], 8, "A cabinet of geometric curiosities")"#,
            r#"root = Panel("Tree · Branch", [Tree([Branch("field-notes", [Branch("small-adventures", [Branch("the-pine-loop.md"), Branch("a-good-window-seat.md")], true), Branch("a-blank-page.md")], true)])])"#,
            r#"root = Panel("Quiz · Question", [quiz])
$pick = ""
quiz = Quiz([Question("Where would a fern feel at home?", [Choice($pick, "Your answer", ["Soft, filtered light", "Afternoon sun"])], "Soft, filtered light.", "A forest-floor sort of plant.")])"#,
            r#"root = Panel("Image · Diagram", [Row([Image("https://example.test/field-note.png", "Image preview slot", 4), Diagram("flowchart LR\nA[Notice]-->B[Wonder]-->C[Make]", "Notice → wonder → make")])])"#,
        ],
    ),
];

pub(super) fn palette() -> Theme {
    let mut theme = Theme::default();
    theme.bg = Color::Rgb(13, 18, 24);
    theme.surface = theme.bg;
    theme.fg = Color::Rgb(220, 226, 228);
    theme.fg_dim = Color::Rgb(126, 145, 155);
    theme.accent = Color::Rgb(117, 196, 176);
    theme.accent2 = Color::Rgb(220, 199, 157);
    theme.border_dim = Color::Rgb(42, 54, 65);
    theme.raised = Color::Rgb(21, 29, 38);
    theme.control = Color::Rgb(34, 46, 57);
    theme.selection = Color::Rgb(36, 65, 66);
    theme.success = Color::Rgb(137, 186, 139);
    theme.warning = Color::Rgb(225, 177, 109);
    theme
}

fn overview() -> (Buffer, Vec<String>) {
    let theme = palette();
    let columns: Vec<_> = COLUMNS
        .iter()
        .map(|(title, sources)| {
            let panels: Vec<_> = sources
                .iter()
                .map(|source| {
                    let document =
                        Document::parse(source).unwrap_or_else(|error| panic!("{source}: {error}"));
                    Entry::new(Ok(Session::new(document).unwrap()))
                        .canvas(COLUMN_WIDTH, &theme, false, true)
                        .0
                })
                .collect();
            (*title, panels)
        })
        .collect();
    let height = columns
        .iter()
        .map(|(_, panels)| {
            panels
                .iter()
                .map(|panel| panel.area.height.saturating_sub(2))
                .sum::<u16>()
        })
        .max()
        .unwrap()
        + 7;
    let mut canvas = Buffer::empty(Rect::new(0, 0, WIDTH, height));
    canvas.set_style(canvas.area, theme.background_style());
    canvas.set_string(
        2,
        0,
        "nexus / the terminal collection",
        Style::default().fg(theme.fg).add_modifier(Modifier::BOLD),
    );
    canvas.set_string(
        2,
        1,
        "Every component family, together. Actual ratatui cells · no browser-designed widgets.",
        Style::default().fg(theme.fg_dim),
    );
    canvas.set_string(
        WIDTH - 27,
        0,
        "72 contracts / one canvas",
        Style::default().fg(theme.accent),
    );
    for (column, (title, panels)) in columns.into_iter().enumerate() {
        let x = 2 + u16::try_from(column).unwrap() * (COLUMN_WIDTH + GAP);
        canvas.set_string(
            x,
            3,
            title,
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
        let mut y = 5;
        for panel in panels {
            // Omit only the known blank padding below the title and above the footer.
            for row in
                (0..panel.area.height).filter(|row| *row != 1 && *row != panel.area.height - 2)
            {
                for col in 0..panel.area.width {
                    canvas[(x + col, y)] = panel[(col, row)].clone();
                }
                y += 1;
            }
        }
    }
    canvas.set_string(2,height-1,"NATIVE MOCKUP / 4 columns · image and diagram show their real text fallbacks · static local sample data",Style::default().fg(theme.fg_dim));
    let names = nexus_core::interactive::component_signatures()
        .lines()
        .map(|line| line.split('(').next().unwrap().to_string())
        .collect();
    (canvas, names)
}

#[test]
fn terminal_overview_covers_every_contract_in_one_native_frame() {
    let (canvas, names) = overview();
    assert_eq!(names.len(), 72);
    let sources = COLUMNS
        .iter()
        .flat_map(|(_, sources)| sources.iter().copied())
        .collect::<Vec<_>>()
        .join("\n");
    for name in names {
        assert!(sources.contains(&format!("{name}(")), "missing {name}");
    }
    assert_eq!(canvas.area.width, WIDTH);
    assert!(
        canvas.area.height <= 70,
        "contact sheet became too tall: {}",
        canvas.area.height
    );
}

#[test]
#[ignore = "opt-in native contact-sheet export; scripts/preview-terminal-panels.sh"]
fn export_terminal_overview() {
    let directory = std::path::PathBuf::from(
        std::env::var_os("NEXUS_TERMINAL_PREVIEW_DIR").expect("set NEXUS_TERMINAL_PREVIEW_DIR"),
    );
    std::fs::create_dir_all(&directory).unwrap();
    let (canvas, names) = overview();
    let mut rows = Vec::new();
    let mut text = Vec::new();
    for y in 0..canvas.area.height {
        let mut runs = Vec::new();
        let mut x = 0;
        let mut line = String::new();
        while x < canvas.area.width {
            let start = x;
            let style = canvas[(x, y)].style();
            let mut content = String::new();
            while x < canvas.area.width && canvas[(x, y)].style() == style {
                content.push_str(canvas[(x, y)].symbol());
                x += 1;
            }
            line.push_str(&content);
            runs.push(json!({"x":start,"text":content,"fg":format!("{:?}",style.fg.unwrap_or(Color::Reset)),"bg":format!("{:?}",style.bg.unwrap_or(Color::Reset)),"bold":style.add_modifier.contains(Modifier::BOLD),"italic":style.add_modifier.contains(Modifier::ITALIC)}));
        }
        rows.push(runs);
        text.push(line.trim_end().to_string());
    }
    std::fs::write(directory.join("terminal-frame.json"),serde_json::to_vec(&json!({"width":canvas.area.width,"height":canvas.area.height,"names":names,"rows":rows})).unwrap()).unwrap();
    std::fs::write(directory.join("terminal-frame.txt"), text.join("\n")).unwrap();
    let mut bytes = b"\x1b[2J\x1b[H".to_vec();
    {
        let mut backend = CrosstermBackend::new(&mut bytes);
        backend
            .draw(
                (0..canvas.area.height)
                    .flat_map(|y| (0..canvas.area.width).map(move |x| (x, y)))
                    .map(|(x, y)| (x, y, &canvas[(x, y)])),
            )
            .unwrap();
        backend.flush().unwrap();
    }
    bytes.extend_from_slice(b"\x1b[0m\n");
    std::fs::write(directory.join("terminal-frame.ansi"), bytes).unwrap();
    eprintln!(
        "Native frame: {} × {} cells; {}",
        canvas.area.width,
        canvas.area.height,
        directory.display()
    );
}
