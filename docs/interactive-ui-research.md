# Better interactive text fields in the terminal

## Primary-source findings

1. **Match the interaction model, not browser pixels.** OpenAI recommends native,
   lightweight inline cards for a single action or decision; direct edits; one
   primary action plus an optional secondary; and expansion for more complex
   work. Its guidelines discourage duplicate chat composers and deep navigation
   inside inline cards. Keep Nexus's existing inline/expanded panel and reviewed
   send behavior rather than adding a second chat UI. [OpenAI UI guidelines][openai]
2. **Use restrained visual hierarchy.** OpenAI calls for system colors, consistent
   spacing and corner rounding, a headline/supporting-text/action hierarchy, and
   legible contrast. A terminal cannot reproduce its fonts, CSS, iframe widgets
   or browser accessibility tree. Rounded field borders, theme colors, persistent
   labels and quieter supporting text are a suitable terminal adaptation—not a
   claim of pixel parity or full accessibility conformance. [OpenAI][openai]
3. **Focus navigation is not text editing.** WAI's keyboard guidance separates
   focus from selection, requires a visible focus indicator, and makes Tab/
   Shift+Tab the conventional means of moving between components. Its shortcut
   guidance discusses avoiding conflicts with editing contexts. Inside a textbox,
   arrows must move the caret, Shift+arrows must select text, and ordinary digits
   must not activate unrelated answer shortcuts. [WAI APG][wai]
4. **Single-line inputs and multiline textareas serve different needs.** A textarea
   supports multiline text and an explicit visible row count. Placeholder text
   illustrates expected input and is not a substitute for a label. Keep persistent
   help text distinct from both placeholders and validation errors. [MDN textarea][textarea]
5. **Reuse the editor already shipped with Nexus.** `tui-textarea-2` supplies cursor
   movement, selection, insertion/deletion, undo/redo, wrapping and viewport
   behavior. Its render-time screen/data map is private. Reusing it avoids another
   hand-written editing engine; mouse hit-testing can use bounded visual cursor
   moves against that same renderer rather than an approximate character-column
   conversion. [Crate docs][editor]; see also `src/cursor.rs`, `src/widget.rs`,
   `src/screen_map.rs` in the installed 0.12.0 crate.

## Reproduced failures

`cargo test -p nexus-chat interactive::input_tests -- --nocapture` initially failed
all three tests in 0.01 seconds after compilation:

- Left followed by `X` left `Ada` unchanged instead of producing `AdXa`.
- Shift+Left followed by paste failed to replace the selected final character.
- Typing `1` into a field returned the unrelated `Yes` answer action.

The controller treated arrows as focus traversal, edited values with string
append/pop, and dispatched panel shortcuts before textbox editing. Both the
public TUI entry point and widget controller rejected modified editing keys.
The underline at the end of a value was only a drawing, not a movable caret.

## Implemented scope

- Keep existing `Input($value, "Label", rules?)` positional arguments compatible.
  Append optional placeholder and hint arguments.
- Add `Textarea($value, "Label", rows?, placeholder?, rules?, hint?)`, default four
  visible text rows. Input state remains bounded to 1024 bytes on one line;
  multiline fields permit 4096 bytes. All existing safety, validation, local-state
  and reviewed-send boundaries still apply.
- Give fields rounded neutral surfaces, persistent labels, required markers,
  placeholders, help/error rows and distinct focused borders.
- Route editing before panel shortcuts. Support arrows, Home/End, word movement,
  selection, select-all, deletion, undo/redo, sanitized paste, and mouse caret/
  drag selection. Tab/Shift+Tab move between controls; Enter advances a single-line
  input but inserts a newline in a textarea. Esc returns to the composer.
- Render textareas with soft wrapping and bounded height. Keep the terminal caret
  aligned with the focused field for terminal input methods. Failed oversized
  edits restore both the prior value and editor state; reset/patch changes rebuild
  stale editor state.

## Boundaries and follow-ups

OpenAI discourages nested scrolling in inline cards. Terminal screen space and
Nexus's bounded panel contract mean long textarea content still scrolls inside
its declared region; expansion offers more room for the panel, not an unbounded
browser document. Do not remove transcript pinning or safety caps to imitate CSS.

This change improves model-authored `nexus-ui` fields, not every popup/composer in
Nexus. Terminal/IME and pointer behavior should also be manually exercised in the
user's terminal; hermetic `TestBackend` tests cannot prove OS input-method support.
Future work can assess richer typed controls and a browser-based surface separately.

[openai]: https://developers.openai.com/plugins/concepts/ui-guidelines
[wai]: https://www.w3.org/WAI/ARIA/apg/practices/keyboard-interface/
[textarea]: https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/textarea
[editor]: https://docs.rs/tui-textarea-2/0.12.0/tui_textarea/
