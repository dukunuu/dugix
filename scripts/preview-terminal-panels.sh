#!/usr/bin/env bash
# One actual-ratatui contact sheet. No boot(), keys, chats or network calls.
set -euo pipefail
cd "$(dirname "$0")/.."
root="$PWD"
output="$root/web/prototypes/interactive"
node "$output/component-sheet.mjs" --export
NEXUS_TERMINAL_PREVIEW_DIR="$output" cargo test -p nexus-chat \
  interactive::terminal_ -- --ignored --nocapture

case "${1:-}" in
  --export-only) exit 0 ;;
  --terminal)
    python3 - "$output" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
frame = json.loads((root / 'terminal-frame.json').read_text())
if sys.stdout.isatty():
    import shutil
    size = shutil.get_terminal_size()
    if size.columns < frame['width'] or size.lines < frame['height']:
        print(f"This full contact sheet is {frame['width']} × {frame['height']} terminal cells.")
        print("Zoom out/enlarge the terminal, or use the one-screen fitted browser viewer:")
        print("http://127.0.0.1:8843/prototypes/interactive/")
        raise SystemExit(0)
sys.stdout.buffer.write((root / 'terminal-frame.ansi').read_bytes())
PY
    exit 0 ;;
  "") ;;
  *) printf 'Usage: %s [--export-only|--terminal]\n' "$0" >&2; exit 2 ;;
esac
url="http://127.0.0.1:8843/prototypes/interactive/"
printf '\nNative terminal overview: %s\n' "$url"
if curl -fsS --max-time 1 "$url" >/dev/null 2>&1; then
  printf 'Preview server is already running.\n'
else
  exec npm --prefix web run prototype:panels
fi
