# Building the sequencer

`examples/sequencer` is a 16-voice synthesizer and pattern sequencer written
entirely in Glyph. It is worth walking through because it is the first
program in this book that has to be correct in three dimensions at once:
it is concurrent, it is real-time, and it is deterministic. Each of those
pushed back on the design, and the shape the program ended up with is mostly
a record of that argument.

Run it with:

```sh
cd examples/sequencer
glyph run                        # live playback (macOS)
glyph run -- --offline out.wav   # the identical audio, rendered to a file
```

## Two threads, one owner per thing

The whole program is two threads that never share memory:

```
CONTROL THREAD (main)                 ENGINE THREAD (spawned FnOnce)
─────────────────────                 ──────────────────────────────
owns: the Song, as JSON               owns: the 16-voice pool
      the command Sender                    the sink (WAV or live device)
      the report Receiver                   the musical clock
      a mirror of engine state              three pattern buffers

        ── EngineCommand ──▶
        ◀── EngineReport ───
```

There is no `Mutex` anywhere in it. That is not asceticism — it falls out of
what the two threads actually need. Control needs to express intent; the
engine needs to make sound. Neither needs to read the other's memory, so
two bounded SPSC channels carrying owned messages are enough, and the
absence of shared state means there is no lock to hold during a render.

The engine body is an ordinary function that takes ownership of both
endpoints:

```glyph
fn engine_run(rx: Receiver<EngineCommand>, tx: Sender<EngineReport>,
              sink_kind: u32, wav_handle: i32, rate: u32, channels: u32) -> i32
```

so the spawned closure is a one-line forward:

```glyph
let engine: FnOnce<(), i32> = move () -> {
  ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
}
let started = spawn(engine)
```

Keeping the loop in a named function rather than inside the closure body is
worth doing: it is several hundred lines, and a closure body is an awkward
place to read them.

## Each channel gets one policy

The two directions are not symmetric, and pretending they were would be a
bug.

**Commands must not be lost.** If the engine misses a `NoteOff`, a note
rings forever. So when `try_send` reports `Full`, it hands the command back,
and the controller keeps it for the next poll:

```glyph
let r = tx.try_send(c)
match r {
  Sent => {},
  Full(back) => {
    next_backlog.push(back)
    blocked = true
  },
  Disconnected(_d) => { blocked = true },
}
```

Note the `blocked` flag. Once one command is deferred, every command behind
it must be deferred too, or a later command would overtake an earlier one.
Losslessness alone is not enough; ordering has to survive as well.

**Reports must not block.** Telemetry exists to tell a UI where the playhead
is. If the engine ever waited to report, a slow reader would stall audio —
the tail wagging the dog. So the engine never retries inside the render
loop: on `Full` it counts a drop and moves on, and the next report that gets
through is preceded by an `Overflow(n)`.

That asymmetry — lossless down, lossy up — is the single most important
design decision in the program.

## The engine owns the clock

The tempting design is for control to decide when notes happen, since
control is where the user is. It is also wrong. Control runs on an ordinary
thread subject to ordinary scheduling; if it decided note times, every note
would land wherever the OS happened to wake it up.

Instead control expresses *intent* — play, 120 bpm, here is a pattern — and
the engine converts musical time to frames while rendering. A note lands on
an exact sample because the engine computes which frame it belongs to at the
moment it fills that block:

```glyph
fn ticks_per_frame(bpm: f64) -> f64 {
  ret bpm * ppq() as f64 / (60.0 * sample_rate() as f64)
}
```

The playhead is recomputed each block from the absolute frame counter:

```glyph
fn playhead_at(span_start_tick: f64, span_start_frame: u64, frame: u64, tpf: f64) -> f64 {
  ret span_start_tick + (frame - span_start_frame) as f64 * tpf
}
```

rather than accumulated with `+=`. One multiply per block keeps rounding
error near 1e-6 tick over an hour; repeated addition would drift audibly.
The span base is reset on play, rewind, and every tempo change, so a tempo
change is continuous in musical time rather than jumping the playhead.

Inside a block, the scheduler collects the note edges that fall in this
block's tick range, sorts them by frame offset, and renders the block in
segments between them. That is what makes placement sample-accurate rather
than block-quantized. Live notes typed by a human *are* block-quantized, to
frame 0 of the next block — 5.33 ms, which nobody can hear.

## A pattern is streamed, not sent

Here the language pushed back. A `Vec` cannot cross a spawn boundary today:
the thread-safety checker fails closed on source-spelled generics, because
`Vec` has no canonical resolver identity to check `Send` against. So there
is no `LoadPattern(Pattern)` message.

Instead a pattern crosses as a stream of flat, scalar-only messages:

```
PatternBegin · PatternTrack* · PatternNote* · PatternCommit
```

and the engine reassembles it into thread-local buffers. It holds three:
*staging* (an open `Begin` window), *pending* (committed, waiting for its
swap point) and *active* (playing). A commit while stopped swaps
immediately; a commit while playing waits for the next bar line, so a new
pattern never arrives halfway through a beat.

This constraint sounds like a limitation, and it is, but the streamed form
turned out to be a reasonable wire format anyway — it is incremental, it
needs no allocation on the engine side beyond the buffers, and it maps
cleanly onto a network protocol later.

## Determinism needs `RenderTo`

Offline there is no sleep anywhere in the engine loop, because pacing comes
only from a live device's blocking write. An offline render therefore runs
as fast as the CPU allows, and "play, wait, stop" would produce a file whose
length depended on thread scheduling.

`RenderTo(n)` fixes that: play until frame `n` exactly — with a short final
block if the target is not a multiple of the block size — then stop and
report `Stopped(n)`. The WAV is byte-reproducible, and the acceptance suite
pins its content hash.

It also imposes an ordering rule on callers. `Shutdown` is immediate, so a
`Shutdown` queued behind a `RenderTo` cancels the render. Control must wait
for `Stopped` first:

```glyph
while st.stopped_seen == 0 && spins < 600000 {
  let got = rep_rx.try_recv()
  match got {
    Value(rep) => { let _k = note_report(&mut st, rep) },
    Empty => { let _sl = sleep_ms(1) spins = spins + 1 },
    Disconnected => { spins = 600000 },
  }
}
```

## Where the sink lives

The obvious design — an enum `Sink { Wav(WavWriter), Live(AudioOut) }` owned
by the engine closure — does not work, for two independent reasons worth
knowing about.

`AudioOut` is deliberately not `Send`. Try to capture one in a spawned
closure and the checker says so: *live audio devices are engine-thread-
affine*. An audio device belongs to the thread that feeds it.

And a `match` needs an enum *value*, not a reference, so there is no way to
write through a writer held inside an enum without moving it out.

So the sink crosses the boundary as scalars — a kind and a handle — and each
side opens what it owns. Control opens the WAV, because control owns the
path string (a borrowed `str` cannot escape into a closure either). The
engine opens the live device, because that is where it has to live:

```glyph
let mut handle: i32 = wav_handle
if sink_kind == sink_live() {
  let opened = out_open(rate, channels)
  handle = match opened {
    Ok(a) => a.handle,
    Err(_e) => { ret 90 },
  }
}
```

Both `std/audio` writers are transparent `{ handle: i32 }` wrappers, so
rebuilding the typed writer at each call site costs nothing and keeps the
`Result`-returning API.

## One more thing: allocas in loops

An engine loops for as long as it is playing, which turns out to be an
unusually demanding thing to ask of a compiler. Early versions of this
program died after a few thousand blocks with `EXC_BAD_ACCESS`, always on
the engine thread, always with a two-frame backtrace.

The cause was that scratch slots for temporaries were emitted wherever the
builder happened to be. LLVM only treats an `alloca` in a function's *entry*
block as a static frame slot; anywhere else it becomes a dynamic stack
allocation that is not reclaimed until the function returns. Every pass
through the render loop leaked a little stack, and a spawned thread only has
512 KB of it.

The fix was to hoist those allocas to the entry block. It is mentioned here
because it is the kind of bug that only a long-running loop finds, and an
audio engine is the most patient loop you will write.

## Where to look next

- [`examples/sequencer/README.md`](https://github.com/emeraldsystems/glyph/tree/master/examples/sequencer)
  — the file-by-file map and the JSON command surface.
- `docs/design/SEQUENCER_CORE.md` — the full contract, including every
  message and the verified platform constraints behind it.
- `crates/glyph-cli/tests/seq_acceptance.rs` — determinism, command floods,
  transport chaos, a long-run heap check, and the latency bound.
