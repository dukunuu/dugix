//! Visual regression net: every component family as plain text at 72 columns.
//! Regenerate with `NEXUS_UPDATE_SNAPSHOTS=1 cargo test -p nexus-chat gallery`.

use super::*;

/// One panel per component family, at a typical transcript width.
pub(super) const PANELS: &[&str] = &[
    r#"root = Panel("Rust vs Go for a CLI", [hero, stats, cmp, cards, tip])
hero = Hero("Rust or Go for your next CLI?", "Both ship single binaries; they differ in build speed and footprint.", "COMPARISON")
stats = Stats([Stat("2.1 MB", "Rust binary", "-40%", "success"), Stat("38 s", "Rust build", "+5x", "warning"), Stat("0.4 s", "Go build")])
cmp = Columns([Column("RUST", [Item("Tiny binaries", "ok"), Item("Slow compiles", "no")]), Column("GO", [Item("Fast builds", "ok"), Item("GC pauses", "no")])])
cards = Cards([Card("Ship fast", "Go gets you to v1 this week.", "GO"), Card("Ship small", "Rust when size and latency matter.", "RUST", "accent")])
tip = Callout("tip", "Go to ship this week; Rust when size matters.", "Takeaway")"#,
    r#"root = Panel("Trip planner", [days, activities, planned, save])
$days = 5
$beach = true
$hiking = false
days = Stepper($days, "Days", 1, 30, 1)
activities = Checklist([Check("Beach (2 days)", $beach), Check("Hiking (3 days)", $hiking)])
planned = Progress("Planned days", @If($beach, 2, 0) + @If($hiking, 3, 0), $days)
save = Buttons([Button("Save plan", @ToAssistant("Plan {{days}} days"), true), Button("Reset", @Reset($days))])"#,
    r#"root = Panel("Sign up", [form])
$name = ""
$bio = ""
$plan = "Pro"
$news = true
$rate = 3
$vol = 40
form = Form("Your details", [Input($name, "Name", {required: true}, "Ada Lovelace", "As on your ID"), Textarea($bio, "About you", 3, "A sentence or two"), Select($plan, "Plan", ["Free", "Pro", "Team"]), Toggle($news, "Newsletter"), Rating($rate, "Experience"), Slider($vol, "Volume", 0, 100, 10)], "Create account", Action([@ToAssistant("signed up")]))"#,
    r#"root = Panel("Team dashboard", [tabs])
$row = 0
tabs = Tabs([Tab("People", [Table(["Name", "Role", "Status"], [["Ada", "Eng", "Active"], ["Bob", "Design", "Away"], ["Cy", "PM", "Active"]], $row, true)]), Tab("Trend", [Chart("line", [Series("Signups", [3, 5, 4, 8, 12, 10, 15])], ["Mon", "Sun"], 6)]), Tab("Notes", [Text("x")])])"#,
    r#"root = Panel("Release overview", [big, steps, tl, kv, meter, badge, bars, spark])
big = Stats([Stat("42", "builds", "+6", "success"), Stat("98.5%", "uptime"), Stat("1.2k", "users")], true)
steps = Steps([Step("Design", "Locked", "done"), Step("Build", "In review", "current"), Step("Ship", null, "todo")])
tl = Timeline([Event("Mon", "Kickoff", "Scope agreed"), Event("Wed", "Beta", null, "success")])
kv = Kv([["Owner", "Ada"], ["Due", "Friday"]])
meter = Meter("CPU", 0.72, 1, "warning")
badge = Badge("BETA", "accent")
bars = Bars([Bar("Rust", 42), Bar("Go", 30), Bar("Zig", 8)])
spark = Sparkline([1, 4, 2, 6, 3, 8, 5], 2)"#,
    r#"root = Panel("Python interview", [quiz])
$pick = ""
$code = "def two_sum(nums, target):\n    pass"
quiz = Quiz([Question("Which structure gives O(1) lookup?", [Choice($pick, "Pick one", ["Hash map", "List", "Tree"], "Hash map")], "Hash map", "Average O(1)."), Question("Write two_sum", [Editor($code, "Solution", "python", 5)])])"#,
    r#"root = Panel("Pick a destination", [hero, picks])
hero = Hero("Where to next?", "Three quick picks for a long weekend")
picks = Buttons([Button("Lisbon", @Answer()), Button("Kyoto", @Answer()), Button("Oaxaca", @Answer())])"#,
    r#"root = Panel("Ops dashboard", [top, Spacer(), grid, controls])
$load = 64
$auto = true
top = Row([Stats([Stat("99.9%", "uptime", "+0.1%", "success")], true), Stack([Meter("CPU", 0.72, 1, "warning"), Meter("Memory", 0.41, 1), Sparkline([3, 5, 4, 7, 6, 9, 8], 2)], 0)])
grid = Grid([Callout("tip", "Healthy", "API"), Callout("warning", "Backlog 1.2k", "Queue"), Callout("tip", "Healthy", "DB"), Callout("info", "Cold start", "Cache")], 2)
controls = Row([Slider($load, "Load", 0, 100, 1), Stack([Toggle($auto, "Autoscale"), Buttons([Button("Deploy", @ToAssistant("deploy"), true), Button("Roll back", @Set($load, 0))])])])"#,
    r#"root = Panel("Team pulse", [top, heat, notes])
top = Row([Number("128", "signups this week", "+12%", "success"), Gauge("Capacity", 0.68, 1, "warning")])
heat = Heatmap([[0, 1, 3, 5, 2, 0, 1], [2, 4, 6, 8, 3, 1, 0], [1, 1, 2, 9, 7, 2, 1]], ["Mon", "Tue", "Wed"])
notes = Md("**Ship** the `beta` today:\n- write release notes\n- tag v0.2", 4)"#,
    r#"root = Panel("Planning", [outline, board, flow])
$todo = ["Docs", "Tests", "Polish"]
outline = Tree([Branch("crates", [Branch("core", [Branch("interactive.rs")]), Branch("tui")], true), Branch("docs")])
board = Board([Lane("Todo", $todo), Lane("Doing", ["Specs"], "warning"), Lane("Done", ["Kickoff", "Design"], "success")], 3)
flow = Diagram("flowchart LR\nA[Plan] --> B[Build] --> C[Ship]", "Plan to build to ship")"#,
    r#"root = Panel("Orbit", [art])
art = Canvas([Circle(50, 50, 40, "muted"), Circle(50, 50, 8, "warning", true), Ray(50, 50, 38, 0, "accent", [Anim("angle", [0, 360], 6, "loop", "linear")]), Label("noon", 50, 0, "normal")], 8, "A sun with a sweeping hand")"#,
];

fn render(index: usize, source: &str) -> String {
    let document = Document::parse(source).unwrap_or_else(|error| panic!("panel {index}: {error}"));
    let entry = Entry::new(Session::new(document).map_err(|error| error.to_string()));
    let (buffer, _) = entry.canvas(72, &Theme::default(), index == 1, true);
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn gallery_snapshots_cover_every_component_family() {
    let gallery = PANELS
        .iter()
        .enumerate()
        .map(|(index, source)| render(index, source))
        .collect::<Vec<_>>()
        .join("\n\n");
    if std::env::var_os("NEXUS_UPDATE_SNAPSHOTS").is_some() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/interactive/gallery_snapshots.txt"
        );
        std::fs::write(path, format!("{gallery}\n")).unwrap();
        return;
    }
    assert_eq!(gallery, include_str!("gallery_snapshots.txt").trim_end());
}
