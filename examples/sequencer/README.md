# Threaded SPSC sequencer

This example builds four sample-frame-timestamped control events on an owned
worker thread, transfers them through `std/sync/spsc`, and renders a
deterministic 48 kHz mono WAV on the ordinary render thread. The source in
[`src/main.glyph`](src/main.glyph) is also the CI golden fixture, so the
checked-in example cannot drift away from the tested program.

```sh
cd examples/sequencer
glyph run
```

The output is `sequencer.wav`: 0.25 seconds of 440 Hz tone, 0.25 seconds of
silence, 0.25 seconds of 660 Hz tone, then 0.25 seconds of silence.

## Acceptance report

| Metric | Offline golden | Opt-in live demo |
|---|---:|---:|
| Queue capacity | 8 events | 2 events |
| Measured peak queue depth | 4 events | 1 event |
| Render segment/block | 12,000 frames (250 ms) | 4,800 frames (100 ms) |
| Late events | 0 | Reported at exit |
| Underruns/empty polls | 0 | Reported at exit |
| Scheduling jitter | 0 ns by sample-clock construction | `produced_ns - deadline_ns`, clamped at 0 |

The offline producer is joined before rendering, making peak depth exactly
four and removing host-scheduler timing from the golden WAV. Each event's
`at_frame` is compared with the render cursor. An event behind that cursor is
late; an event ahead of it causes deterministic silence to be inserted until
its timestamp. An `Empty` result during an active render interval is an
underrun and the live policy is to preserve the current block as silence
rather than block the consumer. The offline test rejects any late event,
empty poll, wrong peak depth, or wrong rendered-frame count.

The live producer uses `sleep_until_ns` with an absolute monotonic deadline,
then records the actual production time. Absolute deadlines avoid cumulative
drift from a chain of relative sleeps. The source computes scheduling jitter
before its ordinary blocking audio write; the table above is the authoritative
acceptance report. CI compiles and lowers this complete live source without
opening a device.

## Hard-real-time boundary

Channel construction, closure allocation, thread spawn/join, vector growth,
printing, WAV I/O, and the blocking `AudioOut.write` call all happen on
ordinary Glyph threads. They are not hard-real-time safe. The SPSC
`try_send`/`try_recv` control path performs no allocation or locking after
construction, but arbitrary payload destruction can still run user drop work.

The macOS live-audio acceptance demo is test-gated by
`GLYPH_AUDIO_LIVE_TEST=1`; CI never opens an audio device. Glyph code remains
on the normal render thread and never runs in the AudioQueue device callback.
The callback consumes already-submitted native buffers only. Run the opt-in
test with:

```sh
GLYPH_AUDIO_LIVE_TEST=1 cargo test -p glyph-cli --features codegen \
  --test sequencer_spsc live_threaded_sequencer_demo_is_environment_gated
```
