# The Glyph sequencer

A 16-voice subtractive synthesizer and pattern sequencer, written entirely
in Glyph. It plays a bundled song live through your speakers, or renders the
identical audio to a WAV file.

```sh
cd examples/sequencer

glyph run                        # play song.json live (macOS)
glyph run -- --offline out.wav   # render the same audio to out.wav
```

`song.json` is two bars at 120 bpm across three tracks — a sine bass, a
square stab, and a triangle lead. The demo plays it through twice so the
loop point is audible.

The two modes are not approximations of each other. Both drive the same
engine with the same commands over the same frame range, and CI asserts
that the offline render is byte-identical to the sequencer acceptance
suite's canonical render.

## How it is put together

Two threads, and exactly one owner for everything:

```
CONTROL THREAD (main)                 ENGINE THREAD (spawned FnOnce)
─────────────────────                 ──────────────────────────────
owns: the Song, as JSON               owns: the 16-voice pool
      the command Sender                    the sink (WAV or live device)
      the report Receiver                   the musical clock
      ControllerState (a mirror)            three pattern buffers

        ── EngineCommand ──▶  lossless: control retries on Full
        ◀── EngineReport ───  lossy: engine never blocks to report
```

Nothing is shared. There is no mutex anywhere in the design. The two
threads only ever exchange owned messages down two bounded SPSC channels,
and each channel has exactly one policy:

- **Commands are lossless.** `try_send` hands the command back on `Full`,
  and the controller keeps it — plus everything queued behind it, so
  ordering survives — for its next poll.
- **Reports are lossy.** The engine never blocks and never retries inside
  the render loop. Dropped reports are counted and announced with an
  `Overflow(n)` before the next one that gets through. Telemetry must never
  be able to stall audio.

**The engine owns the clock.** Control expresses intent ("play", "120 bpm",
"here is a pattern"); the engine converts ticks to frames at render time.
That is what makes note placement sample-accurate instead of hostage to
control-thread scheduling jitter. The playhead is recomputed each block
from the absolute frame counter rather than accumulated, which bounds
rounding error at about 1e-6 tick over an hour.

The full contract, including every message and the reasoning behind the
constraints, is [`docs/design/SEQUENCER_CORE.md`](../../docs/design/SEQUENCER_CORE.md).
The book chapter [Building the sequencer](../../docs/book/src/sequencer.md)
walks through the same code as a worked example of threads, channels and
audio.

## The files

| File | What lives there |
|---|---|
| `src/seq_proto.glyph` | the wire vocabulary and the constants — types only, shared by both threads |
| `src/seq_model.glyph` | the control-side data model and its JSON round-trip |
| `src/seq_voice.glyph` | the voice pool: oscillators, ADSR, note stealing, block rendering |
| `src/seq_engine.glyph` | the engine thread: command loop, tick scheduler, sink |
| `src/seq_control.glyph` | the controller: `apply_json_command`, the MCP-shaped seam |
| `src/main.glyph` | this demo — argument handling and transport, nothing musical |
| `src/seq_server.glyph` | `--server` mode: the same engine over stdio, one JSON command per line |
| `song.json` | the bundled song, also the acceptance suite's canonical fixture |

## Driving it from another program

`--server` hands the same binary over to a line protocol: one JSON request
per line in, one JSON reply per line out, interleaved with a stream of
engine reports. This is how the GlyphAudio app drives the engine, and it is
the same executable — there is only ever one to build, ship and bundle.

```sh
glyph build
./target/debug/sequencer --server --wav out.wav    # or --live
```

```
>  {"cmd":"load_song","song":{ ... }}
<  {"ok":true,"cmd":"load_song","tracks":3,"notes":14,"length_ticks":7680}
>  {"cmd":"render_to","frame":384000}
<  {"ok":true,"cmd":"render_to"}
<  {"ev":"position","frame":256,"tick":10,"playing":true}
<  {"ev":"block","frame":256,"peak":0.35}
   …
<  {"ev":"stopped","frame":384000}
```

Closing stdin shuts it down cleanly — that is how it learns its parent is
gone, and it finalizes the sink rather than leaving a half-written WAV.
A render driven this way is byte-identical to the demo's, which CI asserts.

Three threads and no shared mutable state: the engine, a reader that owns
the command side, and a report writer. Reading a line blocks, so a single
thread doing both would stall telemetry every time it waited for input.

## A pattern is streamed, not sent

You will notice there is no `LoadPattern(Pattern)` message. A `Vec` cannot
cross a spawn boundary today — the thread-safety checker fails closed on
source-spelled generics — so a pattern reaches the engine as a *stream*:

```
PatternBegin · PatternTrack* · PatternNote* · PatternCommit
```

The engine assembles it into thread-local buffers and swaps it in at the
next bar line (immediately, if the transport is stopped). Everything the
controller does to a song ends up as some sequence of those four messages.

## The MCP seam

`apply_json_command(json) -> json` in `seq_control.glyph` is the whole
control surface, and it is deliberately pure: it parses, validates, updates
the mirror, and appends the engine commands it wants sent. It never touches
a channel. That makes the entire command surface testable without starting
a thread, and it leaves one obvious place for an agent-facing server:

```
read a line → apply_json_command → send_commands → write the reply
```

```json
{"cmd":"load_song","song":{ ... }}
{"cmd":"play"}
{"cmd":"set_tempo","bpm":140}
{"cmd":"note_on","track":0,"pitch":60,"velocity":0.9}
{"cmd":"render_to","frame":96000}
{"cmd":"status"}
```

Every reply is a JSON object with `"ok"`. A malformed or out-of-range
command produces a structured error and plans *nothing*, so a bad request
cannot reach the engine at all.

## Why `RenderTo` exists

Offline, the engine renders as fast as the CPU allows — there is no sleep
in the loop, because pacing comes only from a live device's blocking write.
So "render four bars" cannot be expressed as "play, wait a bit, stop": the
length would depend on how quickly the control thread happened to wake up.

`RenderTo(n)` tells the engine to play until frame `n` exactly — using a
short final block if needed — then stop and report `Stopped(n)`. That is
what makes an offline WAV byte-reproducible, and it is why this demo waits
for `Stopped` before it sends `Shutdown`: shutdown is immediate, so queuing
it behind the render would cancel the render.

## Tests

| Suite | Covers |
|---|---|
| `crates/glyph-cli/tests/seq_voice.rs` | the voice pool and its envelopes |
| `crates/glyph-cli/tests/seq_model.rs` | the data model and JSON round-trip |
| `crates/glyph-cli/tests/seq_engine.rs` | the engine thread, sink and shutdown |
| `crates/glyph-cli/tests/seq_transport.rs` | transport, looping, tempo, pattern swaps |
| `crates/glyph-cli/tests/seq_control.rs` | the JSON command surface |
| `crates/glyph-cli/tests/seq_acceptance.rs` | determinism, floods, chaos, long-run, latency |

The live-audio smoke test needs a real device, so it is opt-in:

```sh
GLYPH_AUDIO_LIVE_TEST=1 cargo test -p glyph-cli --features codegen \
  --test seq_acceptance live_output_smoke
```
