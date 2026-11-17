// Component sheet: one realistic dugix-ui program per component family, in
// the chosen "Chart" direction. The Rust exporter renders every program with
// the real renderer (crates/tui/src/interactive/terminal_library_tests.rs).
// Generate: node component-sheet.mjs --export
import { contracts } from './catalog.js';

const q = JSON.stringify;
const panel = (title, view, state = {}) => {
  const lines = Object.entries(state).map(([key, value]) => `$${key} = ${typeof value === 'string' ? q(value) : value}`);
  return [`root = Panel(${q(title)}, [${view}])`, ...lines].join('\n');
};

// Full small apps first: how the components read together.
const experiences = [
  {
    id: 'trip-budget', names: ['Trip budget'], group: 'Experiences',
    note: 'The approved Chart composition: a picture of the answer, totals beside it, compact controls below.',
    source: panel('Lisbon · 12–16 May', `
      Row([
        Chart("bar", [Series("Spend per day", [720, 175, 170, 175])], ["Thu 12", "Fri 13", "Sat 14", "Sun 15"], 8),
        Stack([Number("€760", "left of €2,000", null, "success"), Kv([["Flights", "€540"], ["Hotel", "€480"], ["Food & transit", "€220"], ["Total", "€1,240"]])], 1)
      ], 4),
      Divider(),
      Stepper($people, "Travellers", 1, 8, 1),
      Select($stay, "Stay", ["Hotel", "Flat", "Hostel"]),
      Buttons([Button("Reset", @Reset($people)), Button("Save budget", @ToAssistant("Save this budget"), true)])`,
    { people: 3, stay: 'Hotel' }),
  },
  {
    id: 'deploy', names: ['Deploy'], group: 'Experiences',
    note: 'A release flow: progress through stages, live metrics, a checklist and one primary action.',
    source: panel('Deploy v0.1.42', `
      Steps([Step("Build", "2m 04s", "done"), Step("Test", "412 passed", "done"), Step("Canary", "5% traffic", "current"), Step("Rollout", null, "todo")]),
      Row([Number("0.02%", "error rate", "-0.01%", "success"), Number("182 ms", "p99 latency"), Number("5%", "traffic on canary")], 4),
      Checklist([Check("Migrations applied", $migrated), Check("Dashboards watched", $watched)]),
      Select($region, "Region", ["eu-west", "us-east", "ap-south"]),
      Buttons([Button("Roll back", @Set($halt, true)), Button("Promote to 100%", @ToAssistant("Promote the canary"), true)])`,
    { migrated: true, watched: false, region: 'eu-west', halt: false }),
  },
  {
    id: 'run-pace', names: ['Run pace'], group: 'Experiences',
    note: 'A small calculator: two inputs, a live answer and a chart of the plan.',
    source: panel('Half marathon pace', `
      Stepper($minutes, "Goal (min)", 80, 150, 5),
      Number("5:05 /km", "pace for 1:47:00", null, "accent"),
      Chart("line", [Series("Planned split (s/km)", [310, 306, 304, 305, 303, 300])], ["km 1", "km 21"], 5),
      Toggle($negative, "Negative split"),
      Buttons([Button("Copy splits", @ToAssistant("Copy my splits"), true)])`,
    { minutes: 107, negative: true }),
  },
];

const S = {
  hero: ['Hero', 'Foundations', panel('Trip', 'Hero("Lisbon, 12–16 May", "4 nights · 3 travellers · €2,000 budget", "PLANNED")')],
  text: ['Text', 'Foundations', panel('Flights', 'Text("Flights are the biggest cost. Booking three weeks earlier usually saves about 15%."), Text("Prices from the last 30 days", "muted")')],
  image: ['Image', 'Foundations', panel('Photo', 'Image("https://example.test/lisbon.png", "Alfama rooftops at dusk", 6)')],
  quote: ['Quote', 'Foundations', panel('On reliability', 'Quote("Simplicity is prerequisite for reliability.", "Edsger W. Dijkstra")')],
  divider: ['Divider', 'Foundations', panel('Itinerary', 'Text("Land at 11:40, taxi to Alfama."), Divider("Day 2"), Text("Tram 28 before the crowds.")')],
  badge: ['Badge', 'Foundations', panel('Build', 'Row([Badge("PASSING", "success"), Badge("2 WARNINGS", "warning"), Badge("v0.1.42", "muted")])')],
  md: ['Md', 'Foundations', panel('Debugging', `Md(${q('**Tip:** set `RUST_LOG=debug` to see each request.\n- Retries back off exponentially\n- Timeouts default to 30 s')}, 5)`)],
  callout: ['Callout', 'Foundations', panel('Migration', 'Callout("warning", "The migration locks the users table for about 40 s. Run it outside peak hours.", "Before you deploy")')],
  columns: ['Columns', 'Composition', panel('Serverless?', 'Columns([Column("For", [Item("No servers to run", "ok"), Item("Generous free tier", "ok")]), Column("Against", [Item("Cold starts", "no"), Item("15 minute limit", "no")])])')],
  cards: ['Cards', 'Composition', panel('Plans', 'Cards([Card("Starter", "3 projects · community support", "€0", "muted"), Card("Team", "Unlimited projects · SSO · audit log", "€12", "accent")])')],
  row: ['Row', 'Composition', panel('Latency', 'Row([Number("42 ms", "p50"), Number("180 ms", "p99"), Number("0.02%", "errors", null, "success")], 4)')],
  grid: ['Grid', 'Composition', panel('Host db-2', 'Grid([Meter("CPU", 0.62, 1), Meter("Memory", 0.81, 1, "warning"), Meter("Disk", 0.34, 1), Meter("Network", 0.12, 1)], 2)')],
  stack: ['Stack', 'Composition', panel('Build 1842', 'Stack([Number("4m 12s", "build time", "-38s", "success"), Progress("Tests", 412, 412)], 1)')],
  spacer: ['Spacer', 'Composition', panel('Setup', 'Text("1  Install the CLI"), Spacer(1), Text("2  Sign in with your token")')],
  buttons: ['Buttons', 'Controls', panel('Ship it?', 'Text("Staging has been green for 2 hours."), Buttons([Button("Cancel", @Reset($env)), Button("Preview", @Set($env, "preview")), Button("Deploy to production", @ToAssistant("Deploy to production"), true)])', { env: 'staging' })],
  input: ['Input', 'Controls', panel('Invite', 'Input($email, "Work email", {required:true}, "you@company.com", "The invite goes to this address")', { email: '' })],
  textarea: ['Textarea', 'Controls', panel('Release notes', 'Textarea($notes, "What changed for users?", 3, "Write a short summary", null, "Markdown is fine")', { notes: 'Faster sync on large spaces.\nFixes a crash when pasting images.' })],
  select: ['Select', 'Controls', panel('Server', 'Select($region, "Region", ["eu-west", "us-east", "ap-south"]), Select($size, "Instance size", ["small", "medium", "large", "xlarge", "metal"])', { region: 'eu-west', size: 'medium' })],
  stepper: ['Stepper', 'Controls', panel('Scaling', 'Stepper($replicas, "Replicas", 1, 12, 1), Stepper($workers, "Workers", 1, 4, 1)', { replicas: 3, workers: 4 })],
  toggle: ['Toggle', 'Controls', panel('Project settings', 'Toggle($autoscale, "Autoscale on CPU"), Toggle($previews, "Preview deploys for pull requests")', { autoscale: true, previews: false })],
  slider: ['Slider', 'Controls', panel('Spending cap', 'Slider($cap, "Monthly cap (€)", 0, 500, 25)', { cap: 200 })],
  checklist: ['Checklist', 'Controls', panel('Before launch', 'Checklist([Check("Run migrations", $migrate), Check("Warm the cache", $cache), Check("Announce in #releases", $announce)])', { migrate: true, cache: true, announce: false })],
  rating: ['Rating', 'Controls', panel('Feedback', 'Rating($stars, "How useful was this?", 5)', { stars: 4 })],
  choice: ['Choice', 'Controls', panel('Pick a plan', 'Choice($plan, "Which plan fits your team?", ["Starter · €0", "Team · €12 per seat", "Enterprise · talk to us"])', { plan: 'Team · €12 per seat' })],
  stats: ['Stats', 'Data', panel('This week', 'Stats([Stat("1,284", "active users", "+12%", "success"), Stat("3.4 min", "median session"), Stat("2.1%", "churn", "-0.3%", "success")])')],
  number: ['Number', 'Data', panel('Revenue', 'Number("€12,480", "this month", "+8.2%", "success")')],
  kv: ['Kv', 'Data', panel('Release', 'Kv([["Version", "0.1.42"], ["Commit", "26cda05"], ["Built", "9 Nov, 14:20"], ["Size", "18.4 MB"]])')],
  table: ['Table', 'Data', panel('Services', 'Table(["Service", "Status", "p99"], [["api", "healthy", "180 ms"], ["worker", "degraded", "2.4 s"], ["web", "healthy", "95 ms"]], $row, true)', { row: 0 })],
  progress: ['Progress', 'Data', panel('Upload', 'Progress("Photos", 18, 24)')],
  bars: ['Bars', 'Data', panel('Languages', 'Bars([Bar("Rust", 48), Bar("TypeScript", 31), Bar("Python", 12), Bar("Shell", 9)])')],
  sparkline: ['Sparkline', 'Data', panel('Traffic', 'Text("Requests per minute, last hour", "muted"), Sparkline([12, 18, 15, 22, 30, 28, 35, 41, 38, 44, 52, 47], 2)')],
  chart: ['Chart', 'Data', panel('Commits', 'Chart("bar", [Series("This week", [12, 19, 8, 23, 17]), Series("Last week", [9, 14, 11, 15, 20])], ["Mon", "Tue", "Wed", "Thu", "Fri"], 7)')],
  gauge: ['Gauge', 'Data', panel('Storage', 'Gauge("Disk used", 0.72, 1, "warning")')],
  meter: ['Meter', 'Data', panel('Laptop', 'Meter("Battery", 0.84, 1, "success"), Meter("Storage", 0.91, 1, "warning")')],
  heatmap: ['Heatmap', 'Data', panel('Deploys by hour', 'Heatmap([[0, 2, 5, 3, 1, 0, 0], [1, 4, 8, 6, 2, 0, 1], [0, 3, 7, 9, 4, 1, 0]], ["Week 1", "Week 2", "Week 3"])')],
  tabs: ['Tabs', 'Flow', panel('dugix-api', 'Tabs([Tab("Overview", [Kv([["Owner", "platform"], ["Language", "Rust"], ["Uptime", "99.98%"]])]), Tab("Deploys", [Text("3 this week, last one 2 h ago.")]), Tab("Settings", [Toggle($autoscale, "Autoscale")])])', { autoscale: true })],
  accordion: ['Accordion', 'Flow', panel('FAQ', 'Accordion([Section("Why is my build slow?", [Text("Most time goes to linking. Try the mold linker.")], true), Section("How do I cache dependencies?", [Text("Use sccache with a shared bucket.")])])')],
  form: ['Form', 'Flow', panel('Team', 'Form("Invite a teammate", [Input($email, "Email", {required:true}, "name@company.com"), Select($role, "Role", ["Viewer", "Editor", "Admin"])], "Send invite", @ToAssistant("Send the invite"))', { email: 'ana@company.com', role: 'Editor' })],
  steps: ['Steps', 'Flow', panel('Pipeline', 'Steps([Step("Build", "2m 04s", "done"), Step("Test", "412 passed", "done"), Step("Deploy", "2 of 3 regions", "current"), Step("Verify", null, "todo")])')],
  timeline: ['Timeline', 'Flow', panel('Incident 231', 'Timeline([Event("14:02", "Deploy started", "v0.1.42"), Event("14:05", "Health checks passing", null, "success"), Event("14:09", "Error rate spike", "api · 2.1%", "warning")])')],
  tree: ['Tree', 'Flow', panel('Repository', 'Tree([Branch("src", [Branch("app", [Branch("mod.rs"), Branch("commands.rs")], true), Branch("main.rs")], true), Branch("Cargo.toml")])')],
  board: ['Board', 'Flow', panel('Sprint 14', 'Board([Lane("To do", ["Rate limiter", "Billing email"]), Lane("Doing", ["SSO login"], "warning"), Lane("Done", ["Audit log"], "success")], 2)')],
  diagram: ['Diagram', 'Flow', panel('Request path', 'Diagram("flowchart LR\\nA[Request]-->B[Auth]-->C[Handler]", "Request → auth → handler")')],
  code: ['Code', 'Flow', panel('Health check', `Code(${q('curl -s https://api.example.com/v1/health | jq .status')}, "bash")`)],
  editor: ['Editor', 'Flow', panel('Fare formula', 'Editor($code, "Try it", "python", 3)', { code: 'def fare(km):\n    return 2.5 + 1.2 * km' })],
  quiz: ['Quiz', 'Flow', panel('Git check', 'Quiz([Question("Which command shows staged changes?", [Choice($pick, "Your answer", ["git diff --staged", "git log -p", "git status -s"])], "git diff --staged", "--staged compares the index with HEAD.")])', { pick: '' })],
  canvas: ['Canvas', 'Drawing', panel('Clock', 'Canvas([Circle(50, 50, 38, "muted"), Ray(50, 50, 30, 300, "accent"), Ray(50, 50, 20, 30, "warning"), Dot(50, 50, "accent"), Label("10:50", 50, 96, "muted")], 8, "A clock face at 10:50")')],
  circle: ['Circle', 'Drawing', panel('Coverage', 'Canvas([Circle(50, 50, 36, "muted"), Circle(50, 50, 24, "accent"), Circle(50, 50, 5, "warning", true)], 6, "Concentric rings")')],
  rect: ['Rect', 'Drawing', panel('Layout', 'Canvas([Rect(10, 15, 80, 70, "muted"), Rect(20, 30, 25, 40, "accent", true), Rect(55, 30, 25, 40, "warning")], 6, "Two panes in a frame")')],
  line: ['Line', 'Drawing', panel('Route', 'Canvas([Line(10, 80, 90, 80, "muted"), Line(20, 65, 45, 25, "accent"), Line(45, 25, 70, 45, "accent"), Line(70, 45, 85, 15, "accent")], 6, "An elevation profile")')],
  ray: ['Ray', 'Drawing', panel('Wind', 'Canvas([Circle(50, 50, 36, "muted"), Ray(50, 50, 32, 15, "accent"), Ray(50, 50, 25, 120, "warning"), Dot(50, 50, "accent")], 6, "Two bearings")')],
  arc: ['Arc', 'Drawing', panel('Quota', 'Canvas([Arc(50, 50, 36, 0, 320, "muted"), Arc(50, 50, 30, 0, 245, "accent"), Arc(50, 50, 22, 0, 160, "warning")], 6, "Nested partial readings")')],
  dot: ['Dot', 'Drawing', panel('Scale', 'Canvas([Line(10, 50, 90, 50, "muted"), Dot(20, 50, "accent"), Dot(50, 50, "warning"), Dot(80, 50, "success")], 6, "Three points on a scale")')],
  poly: ['Poly', 'Drawing', panel('Region', 'Canvas([Poly([[15, 70], [30, 25], [55, 15], [80, 45], [70, 80]], "accent"), Line(15, 70, 80, 45, "muted")], 6, "A polygon with a chord")')],
  label: ['Label', 'Drawing', panel('Map', 'Canvas([Dot(35, 50, "accent"), Label("You are here", 60, 50, "normal"), Label("38.7° N", 50, 80, "muted")], 6, "Annotated point")')],
  anim: ['Anim', 'Drawing', panel('Loading', 'Canvas([Circle(50, 50, 30, "muted"), Ray(50, 50, 27, 0, "accent", [Anim("angle", [0, 360], 6, "loop", "linear")])], 6, "A turning hand")')],
};

// Map every family to its contract names (nested items included).
const families = {
  hero: ['Hero'], text: ['Text'], image: ['Image'], quote: ['Quote'], divider: ['Divider'], badge: ['Badge'],
  md: ['Md'], callout: ['Callout'], columns: ['Columns', 'Column', 'Item'], cards: ['Cards', 'Card'],
  row: ['Row'], grid: ['Grid'], stack: ['Stack'], spacer: ['Spacer'], buttons: ['Buttons', 'Button'],
  input: ['Input'], textarea: ['Textarea'], select: ['Select'], stepper: ['Stepper'], toggle: ['Toggle'],
  slider: ['Slider'], checklist: ['Checklist', 'Check'], rating: ['Rating'], choice: ['Choice'],
  stats: ['Stats', 'Stat'], number: ['Number'], kv: ['Kv'], table: ['Table'], progress: ['Progress'],
  bars: ['Bars', 'Bar'], sparkline: ['Sparkline'], chart: ['Chart', 'Series'], gauge: ['Gauge'],
  meter: ['Meter'], heatmap: ['Heatmap'], tabs: ['Tabs', 'Tab'], accordion: ['Accordion', 'Section'],
  form: ['Form'], steps: ['Steps', 'Step'], timeline: ['Timeline', 'Event'], tree: ['Tree', 'Branch'],
  board: ['Board', 'Lane'], diagram: ['Diagram'], code: ['Code'], editor: ['Editor'],
  quiz: ['Quiz', 'Question'], canvas: ['Canvas'], circle: ['Circle'], rect: ['Rect'], line: ['Line'],
  ray: ['Ray'], arc: ['Arc'], dot: ['Dot'], poly: ['Poly'], label: ['Label'], anim: ['Anim'],
};

const cases = [
  ...experiences.map(item => ({ ...item, names: [...item.names, ...(item.id === 'trip-budget' ? ['Panel'] : [])] })),
  ...Object.entries(S).map(([id, [name, group, source]]) => ({ id, names: families[id] ?? [name], group, source })),
];

const contractList = contracts;
for (const { name } of contractList) {
  if (!cases.some(item => item.names.includes(name))) throw new Error(`No case covers ${name}`);
}
export const sheet = { contracts: contractList, cases };

if (process.argv.includes('--export')) {
  const { writeFile } = await import('node:fs/promises');
  await writeFile(new URL('./component-sheet.json', import.meta.url), JSON.stringify(sheet, null, 1));
  console.log(`${cases.length} cases covering ${contractList.length} contracts`);
}
