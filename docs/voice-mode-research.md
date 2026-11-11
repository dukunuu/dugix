# Research: speaking mode for the terminal

**Status: research only.** No code, dependency or configuration changes.

**Checked:** 2026-11-08 against repository commit
`2ba141a78748d217efab23902dfbbbf2fa3dd246`. Provider facts come from the
official OpenAI, OpenRouter and Ollama pages listed in [Sources](#sources).
Crate versions come from `cargo search`. Every claim marked *(unverified)*
comes from a search summary or a third-party page, not from the owning
source.

**Use cases:** practising a foreign language by speaking, and having
fun voice conversations. Both point the design the same way:

- The model should **hear your audio**, not a transcript of it. A
  transcript tidies away the mispronunciations a learner wants
  feedback on.
- The replies should be **audible quickly**, in the target language,
  and easy to **replay slowly**.

## Bottom line

Speaking mode fits the existing architecture without leaving the terminal.
The terminal only needs a status-line indicator and a key, and audio goes
straight to the sound card. There are three ways to build it, shown below.
**Build A first, keep B as the fallback for any model, and leave C for later.**

| | A. Audio turns | B. Voice wrapper | C. Live mode |
|---|---|---|---|
| How it works | Audio goes **in** as an `input_audio` part. Audio streams **out** as `delta.audio` from the same `/chat/completions` stream. | Speech-to-text, then *any* chat model, then text-to-speech per sentence | Speech-to-speech over the Realtime WebSocket |
| Hears your pronunciation | **Yes** | No (sees the transcript only) | Yes |
| Works with any model and tools | Audio-capable models only (the `gpt-audio` family) | **Yes** | Realtime models only |
| Fits "one provider client" | **Yes**: request flags and one new stream delta in `openrouter.rs` | Yes: two plain HTTP endpoints | No: new WebSocket transport |
| Latency feel | Good (streamed) | Fair. The reply is spoken one sentence at a time. | Best (server VAD, barge-in) |
| Local / offline option | No | **Yes**: whisper.cpp in, Ollama Phonon out (macOS) | No |
| Rough size | Medium | Small | Large |

## 1. What the providers offer (as of 2026-11-08)

### Audio in and out on chat completions (option A)

- **OpenRouter** accepts audio on chat completions as base64 `input_audio`
  content parts. The `input_audio.format` field takes any format the
  upstream model supports, for example `aiff`, `aac`, `ogg`, `flac`,
  `m4a`, `pcm16` or `pcm24`. Direct URLs are not supported. [O1]
- **Audio output** works on chat completions only (the Responses API has no
  `audio` parameter). Set `modalities: ["text","audio"]` and send an `audio`
  object with `voice` and `format`. Streaming is **required**. Chunks arrive as
  `delta.audio` with base64 `data` **plus a `transcript`**, so you get the
  sound and the text of the reply from one stream. [O1]
- OpenAI's model is `gpt-audio` (newer snapshot `gpt-audio-1.5`). Its inputs
  and outputs are text and audio, on chat completions. Audio tokens are
  priced at $32 in and $64 out per 1M, against $2.50 and $10 for text.
  *(Pricing unverified: it comes from a search summary of the model page.)*
  [A1]
- *(Unverified)* Whether `gpt-audio` models call tools well, and how good
  their pronunciation feedback is. Neither is documented. Both need a hands-on
  trial before they are relied on.

### Speech to text (option B)

- **OpenRouter** `POST /api/v1/audio/transcriptions`, with a JSON body
  (`input_audio.data` base64, `format`) or OpenAI-style multipart. Fields:
  `language` (ISO-639-1), `response_format` `json`/`verbose_json`,
  and word/segment timestamps. The response includes `usage.cost`. The
  multipart cap is 25 MB and the upstream timeout is 60 s. Streaming is not
  mentioned. [O2]
- **OpenAI** `POST /v1/audio/transcriptions`, with models `gpt-transcribe`,
  `gpt-4o-transcribe`, `gpt-4o-mini-transcribe` and `whisper-1`. `stream=true`
  returns incremental text for a *finished* recording. Live audio goes
  through the Realtime API instead. [A2]
- **For learners:** always send `language` set to the target language.
  Otherwise the model may detect your native language and transcribe or
  translate the wrong way. [O2][A2]

### Text to speech (option B)

- **OpenAI `/v1/audio/speech` is being retired.** `tts-1`, `tts-1-hd` and
  `gpt-4o-mini-tts` **shut down on 2027-01-06**. OpenAI's migration target
  is `gpt-realtime-2.1-mini` through the Realtime API, which `/v1/audio/speech`
  does not serve. Do not build on OpenAI-direct TTS. [A3]
  - Still useful as a reference format: PCM output there is 24 kHz, 16-bit
    signed little-endian, with no header. Mono is implied by the examples.
    Streaming uses chunked transfer. [A3]
  - Languages "generally follow Whisper" (about 57 are listed), but "voices are
    currently optimized for English". That matters when the voice is meant to
    model native pronunciation. [A3]
- **OpenRouter `POST /api/v1/audio/speech`** has an OpenAI-compatible shape
  and routes to several providers. Fields: `model`, `input`, `voice`,
  `instructions` (honoured by Gemini TTS and Fish Audio, as well as OpenAI),
  `response_format` `mp3`|`pcm` (**default `pcm`**), and `speed`. The
  response is a raw byte stream. This is the provider-neutral way to keep
  cloud TTS after the OpenAI shutdown. [O3]
- **Ollama Phonon (local, macOS only for now).** Ollama 0.40.11 (2026-10-24)
  added speech output through an OpenAI-compatible `/v1/audio/speech`,
  streamed as it is generated. The output is 24 kHz mono, WAV by default. Run
  `ollama pull phonon` for English (Apache-2.0). There are separate 40M models
  for French, German, Spanish and Portuguese, plus `phonon:40m-multilingual`,
  all **CC BY-NC 4.0**. Windows and Linux are "coming soon". [L1][L2][L3]
  Nexus already talks to Ollama at `localhost:11434/v1`
  (`app/backends.rs:134`), so this needs no new transport.

### Live speech-to-speech (option C)

- OpenAI Realtime GA: model `gpt-realtime-2.1` (and a `-mini`), over
  WebRTC (`/v1/realtime/calls`) or a server WebSocket. Tools and MCP are
  supported. Events include `response.output_audio.delta` and
  `response.output_audio_transcript.delta`. Drop the beta
  `OpenAI-Beta: realtime=v1` header. [A4]
- *(Unverified)* A 60-minute session cap. The voice is fixed after the first
  audio response. [A5]
- A Realtime client is a second transport. It runs its own conversation
  loop, its own tool-call events and its own session state. It is the most
  fun to talk to, and the most code that sits outside the existing chat loop.

### Pronunciation scoring (optional, for learners)

- Option A gives *qualitative* feedback ("your ü sounds like u"). For
  *scored* feedback, Azure's Pronunciation Assessment returns accuracy,
  fluency, completeness and prosody scores, with phoneme-level errors. But it
  means a new vendor and key, a 30 s cap on the REST endpoint, and user
  reports of inflated scores on short phrases. Prosody may be English-only.
  *(Unverified: forum reports from 2024.)* [Z1][Z2] Not worth it until A has
  been tried.

## 2. Audio I/O in Rust

Current crate versions, from `cargo search` on 2026-11-08:

| Crate | Version | Role |
|---|---|---|
| `cpal` | 0.18.2 | Low-level capture and playback (CoreAudio, WASAPI, ALSA, with optional PulseAudio/PipeWire/JACK features) [C1] |
| `rodio` | 0.23.0 | Playback queue on top of cpal ("play this, then this", stop) |
| `ringbuf` | 0.6.0 | Lock-free SPSC buffer between the audio callback and tokio |
| `rubato` | 5.0.1 | Resampling (device rate, often 48 kHz, to the 16/24 kHz models expect) |
| `hound` | 3.5.1 | WAV encoding for uploads |
| `voice_activity_detector` | 0.2.1 | Silero VAD for auto-stop on silence |
| `sherpa-onnx` | 2.0.1 | Official bindings: VAD, offline and streaming ASR, offline TTS (Kokoro); links statically and fetches a prebuilt library [C2] |
| `whisper-rs` | 0.16.0 | whisper.cpp bindings, with a `metal` feature [C3] |
| `tokio-tungstenite` | 0.30.0 | WebSocket client, needed only for option C |

Build cost to plan for:

- On Linux, cpal's default ALSA backend needs the ALSA development headers
  (`libasound2-dev`) at build time. That affects `cargo install nexus-chat`
  users and CI. **Put audio behind a `voice` cargo feature** on the TUI
  crate. *(Expected from cpal's ALSA backend; not re-checked for 0.18.)*
- whisper-rs and sherpa-onnx compile or download native libraries. Make them a
  separate opt-in `voice-local` feature, so a default build never compiles
  C++.
- **Zero-dependency prototype path:** shell out to `sox`/`rec` for capture
  and `afplay` (macOS) or `paplay` for playback. This is useful for trying the
  experience in an afternoon before taking on cpal. It is not a shipping
  answer.

## 3. Terminal-specific constraints

### Push-to-talk keys

- The terminal never sends key **release** events by default. crossterm
  reports `KeyEventKind::Release` only after pushing
  `KeyboardEnhancementFlags::REPORT_EVENT_TYPES`. Plain character keys also
  need `REPORT_ALL_KEYS_AS_ESCAPE_CODES`. [K1]
- Nexus currently pushes only `DISAMBIGUATE_ESCAPE_CODES`
  (`crates/tui/src/main.rs:77-82`), behind `supports_keyboard_enhancement()`.
  Adding `REPORT_EVENT_TYPES` doubles key events everywhere: every handler
  would need to ignore `Release`. The safe change is to filter release events
  once, at the top of the event loop, and route them only to the voice key.
- The kitty keyboard protocol, which carries release events, is implemented
  by kitty, WezTerm, foot, Ghostty and others, but **not everywhere**: the
  macOS Terminal.app does not have it. [K2] *(Terminal list partly
  unverified.)*
- **Design:** make **toggle** the baseline. Press the key to start talking,
  and press it again (or pause, with VAD) to send. Offer **hold-to-talk**
  only where `supports_keyboard_enhancement()` is true. Toggle plus VAD is
  also the more relaxed mode for a long language-practice conversation.

### Microphone permission on macOS

- macOS attributes the microphone request to the **responsible app**, which
  is the terminal emulator (Terminal, iTerm2, Ghostty and so on), not
  `nexus`. The first capture triggers a prompt for the *terminal*. If access
  is denied or attributed to an app without a grant, capture returns
  **digital silence with no error**. [P1]
- *Therefore:* detect an all-zero capture buffer and show a status hint, for
  example "no audio — allow Microphone for your terminal in System Settings →
  Privacy & Security". *(Behaviour under tmux and SSH: unverified. Over SSH
  the microphone is on the wrong machine anyway.)*
- `tccutil reset Microphone` re-arms the prompt. [P1]

### UI vocabulary

- State lives in the status line: `listening` with a level meter,
  `transcribing`, `thinking`, and `speaking`. Glyphs come from `ui/style.rs`,
  with no emoji (a test enforces this).
- Your spoken turn lands in history as a normal user message (the
  transcript). An attached audio badge allows replay.
- The reply text still streams on screen as it does today, so a learner
  **reads along while listening**.

## 4. How it maps onto the codebase

The seams in `CLAUDE.md` decide where each part goes:

| Piece | Lives in | Why |
|---|---|---|
| Mic capture, playback, level meter, VAD | `crates/tui` (`audio.rs`, behind the `voice` feature) | Devices belong to the frontend. `nexus host` is a daemon with no microphone, and core must stay free of TUI dependencies. |
| `/transcriptions` and `/speech` HTTP clients | `crates/core/src/provider/` (next to `embedding.rs`) | Plain reqwest calls, which the CLI can use too (`nexus transcribe file.wav` and `nexus say` come for free) |
| `input_audio` parts, `modalities`/`audio` flags, `delta.audio` parsing | `provider/openrouter.rs` and `provider/mod.rs` | Backend differences are flags in the one client. Add a `StreamEvent::Audio { pcm, transcript }` variant beside `Token` (`provider/mod.rs:494`). |
| Sentence chunker (Token stream to TTS-sized pieces) | core | Pure function, unit-testable, used by option B |
| `/voice` on/off/hold, `/voice lang <code>`, `/say` (replay the last reply), `/say slow` | `AppCommand` and the `COMMANDS` catalog | Every user action is a command |
| Voice, language, TTS model and speed settings | `config.rs` | No secrets: keys stay where they are |
| Captured audio files | `spaces/<name>/` media, or not kept at all | Ask the user about this. Audio is large, and the durable db should hold only a reference. |

Data flow for option A, one turn:

```
key ─▶ cpal capture (48k f32) ─▶ resample 24k/16k ─▶ VAD stop ─▶ WAV/pcm16 b64
   ─▶ AppCommand::SendAudio ─▶ chat loop: user msg {input_audio}
   ─▶ /chat/completions  modalities=[text,audio] stream=true
   ─▶ StreamEvent::Audio{pcm,transcript}  ─▶ AppEvent::Stream ─▶ AppView
        ├─ transcript ─▶ history (same rendering as Token)
        └─ pcm ─▶ playback queue (barge-in: voice key stops it)
```

Option B swaps the middle step: the WAV goes to `/audio/transcriptions`, the
text goes into the **normal** `run_command` send path, and each finished
sentence of `StreamEvent::Token` goes to `/audio/speech` (pcm) and then to
the playback queue.

## 5. Language-learning and fun specifics

- **A tutor prompt as a skill, not a hardcoded mode.** It is a `SKILL.md`
  that says: speak mostly in the target language at my level, correct one
  thing per turn, and repeat the corrected sentence slowly. That keeps
  `base.md` (and the prompt snapshots) untouched.
- **Slow replay:** `/say slow` re-requests TTS with `speed` (OpenRouter
  supports it) or an `instructions` string such as "slowly and clearly, like
  a teacher" where the provider honours `instructions`. [O3]
- **Read-along:** keep streaming the text. Optionally show the
  target-language reply with a toggleable translation line, coloured
  `dim`.
- **Local coverage is narrow:** Phonon covers en, fr, de, es and pt only.
  Whisper-family STT covers many more languages. For other target languages,
  TTS must be cloud via OpenRouter. [L2][A3] See §5a for Japanese and
  Mongolian.

## 5a. Japanese and Mongolian

**Japanese is well covered. Mongolian is the hard one:** it works only
with a few specific providers, and mostly at Preview quality.

| Need | Japanese (`ja`) | Mongolian (`mn`) |
|---|---|---|
| OpenAI TTS language list | Listed [A3] | **Not listed** [A3] |
| Gemini TTS (also on OpenRouter as `google/gemini-3.8-flash-lite-tts`) | `ja-JP` **GA** [G1] | `mn-MN` **Preview** [G1] |
| Azure TTS | Many voices, including HD and MAI-Voice [Z3] | `mn-MN-YesuiNeural`, `mn-MN-BataaNeural`, with no phoneme or lexicon control [Z3] *(voice names from a truncated fetch)* |
| Local TTS | Kokoro v1.0 `jf_*`/`jm_*` voices, graded about C+ by its own card [K3] | Meta MMS `facebook/mms-tts-mon` (VITS, through sherpa-onnx), **CC BY-NC**, robotic [M1] |
| Cloud STT | Whisper and gpt-transcribe are strong on Japanese. Google Chirp 3 `ja-JP` GA. [A2][G2] | Azure `mn-MN` real-time and batch [Z3]. Google Chirp 3 `mn-MN` **Preview** [G2]. |
| Local STT | whisper.cpp (large-v3 or turbo) is good | **Whisper zero-shot is poor**: about 34% CER on FLEURS, from a model card [W1]. Community fine-tunes exist, such as `whisper-small-mn`. [W2] |
| Hears pronunciation (option A) | `gpt-audio` should cope *(untested)* | **Unknown.** Gemini audio input is the better bet. Needs a hands-on test. |

What this means for the design:

- **Japanese works on every path.** Option A with `gpt-audio`, option B
  with any STT plus Gemini or OpenRouter TTS, and the local track with
  whisper.cpp plus Kokoro. OpenAI says its voices are "optimized for English",
  so a non-native accent on Japanese is likely. Gemini or Azure Japanese
  voices are the safer model to imitate. [A3]
- **Mongolian should start in the cloud, through OpenRouter, with Gemini
  for both directions.** Use Gemini TTS through `/api/v1/audio/speech`, and
  a Gemini model for audio input (option A). It is one key and one client,
  and the only cloud TTS reachable through the existing client that lists
  `mn-MN`. Azure is the backup: it has the most mature Mongolian STT and TTS,
  but it means a separate vendor and key. Skip local Mongolian for now.
- **Make the provider choice per language,** not global. For example,
  `[voice.lang.mn] tts = "google/gemini-3.8-flash-lite-tts"` and
  `[voice.lang.ja] tts = "…"`. STT gets the same treatment, always sending
  the `language` hint, which Mongolian badly needs.
- **Text display in the terminal:**
  - Japanese needs double-width cells, which `unicode-width` already
    handles. Ruby text (furigana) cannot be drawn above characters in a
    terminal. Put the kana or romaji reading on a `dim` line under the
    sentence instead, as the tutor skill's output format.
  - Mongolian **Cyrillic** renders fine. **Traditional Mongolian script** is
    written vertically and mostly cannot be displayed in a terminal, so it is
    out of scope for this app.
- **Pitch accent (Japanese)** is invisible in a transcript. Only option A,
  or a scored assessment API, can comment on it. That is one more reason to
  build option A for learners.
- **Fun:** voice personas are just `voice` plus `instructions` presets. A
  `/voice persona pirate` command costs a config entry, not code.

## 6. Suggested phases

1. **V0 dictation (small).** Press a key to record, send to
   `/audio/transcriptions`, and put the text in the composer for you to edit
   and send. Ships cpal capture, the `voice` feature, the permission hint and
   the status meter. It is useful on its own.
2. **V1 spoken replies (option B).** A sentence chunker, `/audio/speech` pcm
   with a playback queue, barge-in, `/voice` and `/say`. Works with every
   model and every tool.
3. **V2 audio turns (option A).** `input_audio` parts plus audio output on
   chat completions. The tutor skill. This is the pronunciation-feedback
   step. Needs a hands-on check of `gpt-audio` quality and tool calling first.
4. **V3 local track.** Phonon TTS through Ollama (macOS). Optionally
   whisper-rs STT behind `voice-local`.
5. **V4 live mode (option C).** A Realtime WebSocket client with
   `ToolExecutor` bridging. Only if V2 feels too turn-based.

## 7. Open questions for the user

- Which **target languages**? This decides whether local TTS is viable at
  all.
- Which **key**: OpenRouter only, or also OpenAI direct? Option A works
  through both. Option C needs OpenAI direct.
- **Linux builds:** is the ALSA header requirement acceptable behind a
  feature flag, or should Linux be macOS-first and later?
- **Keep recordings?** Replaying your own attempts later is a nice learning
  feature, but it is the only part that touches storage and sync.

## Sources

- [O1] OpenRouter, Audio: https://openrouter.ai/docs/guides/overview/multimodal/audio
- [O2] OpenRouter, Speech-to-Text: https://openrouter.ai/docs/guides/overview/multimodal/stt
- [O3] OpenRouter, Text-to-Speech: https://openrouter.ai/docs/guides/overview/multimodal/tts
- [A1] OpenAI, GPT-Audio model: https://developers.openai.com/api/docs/models/gpt-audio (search summary only)
- [A2] OpenAI, Speech to text: https://developers.openai.com/api/docs/guides/speech-to-text (search summary only)
- [A3] OpenAI, Text to speech: https://developers.openai.com/api/docs/guides/text-to-speech
- [A4] OpenAI, Realtime: https://developers.openai.com/api/docs/guides/realtime
- [A5] OpenAI, Realtime conversations: https://developers.openai.com/api/docs/guides/realtime-conversations (search summary only)
- [L1] Ollama v0.40.11 release: https://github.com/ollama/ollama/releases/tag/v0.40.11 (search summary only)
- [L2] Ollama blog, Speech models: https://ollama.com/blog/speech-models (search summary only)
- [L3] Ollama docs, Speech: https://docs.ollama.com/capabilities/speech (search summary only)
- [C1] cpal features: https://docs.rs/crate/cpal/latest/features
- [C2] sherpa-onnx Rust crate: https://docs.rs/sherpa-onnx
- [C3] whisper-rs: https://docs.rs/crate/whisper-rs/latest
- [K1] crossterm `KeyboardEnhancementFlags`: https://docs.rs/crossterm/latest/crossterm/event/struct.KeyboardEnhancementFlags.html
- [K2] kitty keyboard protocol: https://sw.kovidgoyal.net/kitty/keyboard-protocol/
- [P1] macOS TCC responsible-process behaviour: https://github.com/pingdotgg/t3code/discussions/7268 (third-party report)
- [Z1] Azure Pronunciation Assessment transparency note: https://learn.microsoft.com/en-us/azure/foundry/responsible-ai/speech-service/pronunciation-assessment/transparency-note-pronunciation-assessment (search summary only)
- [Z3] Azure Speech language support: https://learn.microsoft.com/en-us/azure/ai-services/speech-service/language-support
- [G1] Google Cloud, Gemini-TTS: https://docs.cloud.google.com/text-to-speech/docs/gemini-tts
- [G2] Google Cloud, Chirp 3 STT: https://docs.cloud.google.com/speech-to-text/v2/docs/chirp-model
- [K3] Kokoro-82M model card (voices and grades): https://huggingface.co/hexgrad/Kokoro-82M (via a mirror; search summary only)
- [M1] sherpa-onnx MMS TTS: https://k2-fsa.github.io/sherpa/onnx/tts/mms.html (search summary only)
- [W1] BuzzASR Mongolian model card (Whisper large-v3 zero-shot baseline): https://huggingface.co/BuzzASR/mongolian (search summary only)
- [W2] whisper-small-mn-v3: https://huggingface.co/Blgn94/whisper-small-mn-v3 (search summary only)
- [Z2] Azure STT REST, short audio: https://docs.azure.cn/en-us/ai-services/speech-service/rest-speech-to-text-short (search summary only)
