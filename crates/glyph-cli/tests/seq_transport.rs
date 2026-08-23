//! GLYPH-58 acceptance: transport + pattern playback — play/stop/rewind,
//! tempo, and the tick→frame scheduler that places pattern notes at exact
//! frame offsets inside each block.
//!
//! The engine owns the musical clock, so every timing assertion here is
//! made against the rendered audio rather than against control-side
//! wall-clock timing.

#![cfg(all(feature = "codegen", unix))]

use std::fs;
use std::path::Path;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use tempfile::TempDir;

const SEQ_PROTO_SRC: &str = include_str!("../../../examples/sequencer/src/seq_proto.glyph");
const SEQ_VOICE_SRC: &str = include_str!("../../../examples/sequencer/src/seq_voice.glyph");
const SEQ_ENGINE_SRC: &str = include_str!("../../../examples/sequencer/src/seq_engine.glyph");

const PRELUDE: &str = r#"
import std
from seq_proto import EngineCommand, EngineReport, PatternHeader, PatternNoteMsg, TrackParamsMsg, NoteCmd, GainCmd, PositionReport, BlockReport, sink_wav
from seq_engine import engine_run
from std/audio import WavWriter, wav_create
from std/enums import Result
from std/sync/spsc import channel, Sender, Receiver, TrySendResult, TryRecvResult
from std/thread import JoinHandle, ThreadError, spawn
from std/time import sleep_ms
from std/vec import Vec

// kind: 1 Position, 2 BlockRendered, 3 Overflow, 4 Stopped.
// Returns everything at once: Glyph moves the report into the helper, so
// the value cannot be inspected twice.
struct RepInfo {
  kind: i32,
  frame: u64,
  playing: i32,
  peak: f64,
}

fn rep_info(rep: EngineReport) -> RepInfo {
  ret match rep {
    Position(p) => RepInfo { kind: 1, frame: p.frame, playing: if p.playing { 1 } else { 0 }, peak: 0.0 },
    BlockRendered(b) => RepInfo { kind: 2, frame: b.frame, playing: 0, peak: b.peak },
    Overflow(o) => RepInfo { kind: 3, frame: o as u64, playing: 0, peak: 0.0 },
    Stopped(s) => RepInfo { kind: 4, frame: s, playing: 0, peak: 0.0 },
  }
}
"#;

fn build_and_run(main_src: &str) -> (Option<i32>, TempDir) {
    let temp = TempDir::new().unwrap();

    if std::env::var("GLYPH_SKIP_RUN").is_ok() || std::env::var("GLYPH_SKIP_RUN_MAIN").is_ok() {
        return (None, temp);
    }

    let root = temp.path();
    fs::write(
        root.join("glyph.toml"),
        r#"[package]
name = "seqtransport"
version = "0.1.0"

[[bin]]
name = "seqtransport"
path = "src/main.glyph"
"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/seq_proto.glyph"), SEQ_PROTO_SRC).unwrap();
    fs::write(root.join("src/seq_voice.glyph"), SEQ_VOICE_SRC).unwrap();
    fs::write(root.join("src/seq_engine.glyph"), SEQ_ENGINE_SRC).unwrap();
    fs::write(
        root.join("src/main.glyph"),
        format!("{}\n{}", PRELUDE, main_src),
    )
    .unwrap();

    let glyph_bin = env!("CARGO_BIN_EXE_glyph");
    let build = Command::new(glyph_bin)
        .arg("build")
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "glyph build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );

    let exe = root.join("target/debug/seqtransport");
    assert!(exe.exists(), "expected built binary at {}", exe.display());
    let run = Command::new(&exe).current_dir(root).output().unwrap();
    // A signal death yields code() == None, which callers treat as "skipped".
    // Fail loudly instead: this is how a stack-overflow crash once hid.
    if let Some(sig) = run.status.signal() {
        panic!(
            "program was killed by signal {}\nstdout: {}\nstderr: {}",
            sig,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }
    (run.status.code(), temp)
}

fn read_wav_samples(path: &Path) -> Vec<i16> {
    let bytes =
        fs::read(path).unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));
    assert!(bytes.len() >= 44, "WAV too short: {} bytes", bytes.len());
    assert_eq!(&bytes[0..4], b"RIFF");
    bytes[44..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Frames where a note starts: the signal rises out of a sustained silence.
///
/// The crossing is detected with a single threshold and hysteresis, then
/// walked back to the exact onset frame. Two independent thresholds do NOT
/// work here: a 5 ms attack ramps gradually (…120, 163, 212, 268, 330…), so
/// the signal leaves a "silence" threshold several samples before it
/// reaches an "onset" one, and a detector that requires both on the same
/// sample never fires at all.
///
/// Walking back is exact rather than approximate because the engine
/// guarantees both ends: every attack renders its first frame at level 0
/// (seq_voice), and the gaps between notes are exact zeros.
fn note_onsets(samples: &[i16]) -> Vec<usize> {
    let mut onsets = Vec::new();
    let mut silent_run = 100_000usize; // treat the start of the file as silence
    let mut in_note = false;
    for (i, &s) in samples.iter().enumerate() {
        let amplitude = s.unsigned_abs();
        if amplitude < 100 {
            silent_run += 1;
            if silent_run >= 1000 {
                in_note = false;
            }
        } else {
            silent_run = 0;
        }
        if !in_note && amplitude > 300 {
            in_note = true;
            let mut start = i;
            while start > 0 && samples[start - 1] != 0 {
                start -= 1;
            }
            onsets.push(start.saturating_sub(1));
        }
    }
    onsets
}

/// Largest sample-to-sample step. A click reads as a step far larger than
/// anything the waveform itself can produce.
fn max_step(samples: &[i16]) -> i32 {
    samples
        .windows(2)
        .map(|w| (w[1] as i32 - w[0] as i32).abs())
        .max()
        .unwrap_or(0)
}

/// Dominant frequency of a window, by zero crossings.
fn zero_cross_freq(samples: &[i16], start: usize, len: usize) -> f64 {
    let seg = &samples[start..(start + len).min(samples.len())];
    let crossings = seg
        .windows(2)
        .filter(|w| (w[0] < 0) != (w[1] < 0))
        .count();
    crossings as f64 / 2.0 / (seg.len() as f64 / 48000.0)
}

/// A 2-bar pattern rendered for 4 bars must repeat bar-for-bar: the
/// scheduler's loop wrap has to be sample-exact, not merely close.
#[test]
fn looped_pattern_repeats_bar_for_bar() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("loop.wav", 48000, 1)
  let wav_handle: i32 = match made { Ok(w) => w.handle, Err(_e) => { ret 10 }, }

  // Two bars = 7680 ticks. Every note ends well before its bar line, so
  // each bar starts from silence and repeats can be compared exactly.
  let c1 = cmd_tx.try_send(EngineCommand::PatternBegin(PatternHeader { length_ticks: 7680, track_count: 1 }))
  match c1 { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }
  let c2 = cmd_tx.try_send(EngineCommand::PatternTrack(TrackParamsMsg { track: 0, waveform: 0, gain: 0.8, attack_ms: 5.0, decay_ms: 50.0, sustain: 0.7, release_ms: 80.0 }))
  match c2 { Sent => {}, Full(_c) => { ret 22 }, Disconnected(_d) => { ret 23 }, }
  let n1 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 0, length_ticks: 480, pitch: 60, velocity: 0.9 }))
  match n1 { Sent => {}, Full(_e1) => { ret 24 }, Disconnected(_f1) => { ret 25 }, }
  let n2 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 1920, length_ticks: 480, pitch: 64, velocity: 0.85 }))
  match n2 { Sent => {}, Full(_e2) => { ret 26 }, Disconnected(_f2) => { ret 27 }, }
  let n3 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 3840, length_ticks: 480, pitch: 67, velocity: 0.8 }))
  match n3 { Sent => {}, Full(_e3) => { ret 28 }, Disconnected(_f3) => { ret 29 }, }
  let n4 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 5760, length_ticks: 480, pitch: 72, velocity: 0.75 }))
  match n4 { Sent => {}, Full(_e4) => { ret 30 }, Disconnected(_f4) => { ret 31 }, }
  let cc = cmd_tx.try_send(EngineCommand::PatternCommit)
  match cc { Sent => {}, Full(_g) => { ret 32 }, Disconnected(_h) => { ret 33 }, }
  // 4 bars at 120 bpm = 384000 frames.
  let cr = cmd_tx.try_send(EngineCommand::RenderTo(384000))
  match cr { Sent => {}, Full(_i) => { ret 34 }, Disconnected(_j) => { ret 35 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  let mut seen: bool = false
  let mut spins: i32 = 0
  while seen == false && spins < 60000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => { let info = rep_info(rep) if info.kind == 4 { seen = true } },
      Empty => { let _sl = sleep_ms(1) spins = spins + 1 },
      Disconnected => { seen = true },
    }
  }
  if seen == false { ret 60 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_m) => { ret 36 }, Disconnected(_n) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "loop session exited nonzero");

    let s = read_wav_samples(&temp.path().join("loop.wav"));
    assert_eq!(s.len(), 384_000, "expected exactly 4 bars");

    let bar = 96_000;
    assert_eq!(
        &s[0..bar],
        &s[2 * bar..3 * bar],
        "bar 3 is not a sample-exact repeat of bar 1"
    );
    assert_eq!(
        &s[bar..2 * bar],
        &s[3 * bar..4 * bar],
        "bar 4 is not a sample-exact repeat of bar 2"
    );

    // And the loop is actually playing notes, not repeating silence.
    let onsets = note_onsets(&s);
    assert_eq!(
        onsets.len(),
        8,
        "expected 8 note onsets across 4 bars, got {:?}",
        onsets
    );
}

/// A tempo change rebases the musical span, so every onset after it lands
/// at the new rate — exactly, not approximately.
#[test]
fn tempo_change_shifts_subsequent_onsets() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("tempo.wav", 48000, 1)
  let wav_handle: i32 = match made { Ok(w) => w.handle, Err(_e) => { ret 10 }, }

  let c1 = cmd_tx.try_send(EngineCommand::PatternBegin(PatternHeader { length_ticks: 3840, track_count: 1 }))
  match c1 { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }
  let c2 = cmd_tx.try_send(EngineCommand::PatternTrack(TrackParamsMsg { track: 0, waveform: 0, gain: 0.8, attack_ms: 5.0, decay_ms: 50.0, sustain: 0.7, release_ms: 80.0 }))
  match c2 { Sent => {}, Full(_c) => { ret 22 }, Disconnected(_d) => { ret 23 }, }
  let n1 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 0, length_ticks: 480, pitch: 60, velocity: 0.9 }))
  match n1 { Sent => {}, Full(_e1) => { ret 24 }, Disconnected(_f1) => { ret 25 }, }
  let n2 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 1920, length_ticks: 480, pitch: 67, velocity: 0.85 }))
  match n2 { Sent => {}, Full(_e2) => { ret 26 }, Disconnected(_f2) => { ret 27 }, }
  let cc = cmd_tx.try_send(EngineCommand::PatternCommit)
  match cc { Sent => {}, Full(_g) => { ret 28 }, Disconnected(_h) => { ret 29 }, }
  // Segment 1: one bar at the default 120 bpm.
  let cr = cmd_tx.try_send(EngineCommand::RenderTo(96000))
  match cr { Sent => {}, Full(_i) => { ret 30 }, Disconnected(_j) => { ret 31 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  let mut seen1: bool = false
  let mut spins: i32 = 0
  while seen1 == false && spins < 60000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => { let info = rep_info(rep) if info.kind == 4 { seen1 = true } },
      Empty => { let _sl = sleep_ms(1) spins = spins + 1 },
      Disconnected => { seen1 = true },
    }
  }
  if seen1 == false { ret 60 }

  // Segment 2: same engine, same sink, double tempo.
  let ct = cmd_tx.try_send(EngineCommand::SetTempo(240.0))
  match ct { Sent => {}, Full(_k) => { ret 32 }, Disconnected(_l) => { ret 33 }, }
  let cr2 = cmd_tx.try_send(EngineCommand::RenderTo(192000))
  match cr2 { Sent => {}, Full(_m) => { ret 34 }, Disconnected(_n) => { ret 35 }, }

  let mut seen2: bool = false
  let mut spins2: i32 = 0
  while seen2 == false && spins2 < 60000 {
    let got2 = rep_rx.try_recv()
    match got2 {
      Value(rep2) => { let info2 = rep_info(rep2) if info2.kind == 4 { seen2 = true } },
      Empty => { let _sl2 = sleep_ms(1) spins2 = spins2 + 1 },
      Disconnected => { seen2 = true },
    }
  }
  if seen2 == false { ret 61 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_o) => { ret 36 }, Disconnected(_p) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "tempo session exited nonzero");

    let s = read_wav_samples(&temp.path().join("tempo.wav"));
    assert_eq!(s.len(), 192_000, "expected two 96000-frame segments");

    // 120 bpm: 1 tick = 25 frames, so ticks 0 and 1920 land at 0 and 48000.
    // 240 bpm: 1 tick = 12.5 frames, so the same pattern repeats twice as
    // fast — onsets every 24000 frames from the segment boundary.
    let onsets = note_onsets(&s);
    let expected = [0usize, 48_000, 96_000, 120_000, 144_000, 168_000];
    assert_eq!(
        onsets.len(),
        expected.len(),
        "expected {} onsets, got {:?}",
        expected.len(),
        onsets
    );
    for (got, want) in onsets.iter().zip(expected.iter()) {
        let drift = (*got as i64 - *want as i64).abs();
        assert!(
            drift <= 2,
            "onset at {} drifted {} frames from the expected {}",
            got,
            drift,
            want
        );
    }
}

/// Rewind puts the playhead back to 0 whether or not the transport is
/// running, so the next render replays the pattern from the top.
#[test]
fn rewind_replays_the_pattern_from_the_top() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("rewind.wav", 48000, 1)
  let wav_handle: i32 = match made { Ok(w) => w.handle, Err(_e) => { ret 10 }, }

  let c1 = cmd_tx.try_send(EngineCommand::PatternBegin(PatternHeader { length_ticks: 3840, track_count: 1 }))
  match c1 { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }
  let c2 = cmd_tx.try_send(EngineCommand::PatternTrack(TrackParamsMsg { track: 0, waveform: 0, gain: 0.8, attack_ms: 5.0, decay_ms: 50.0, sustain: 0.7, release_ms: 80.0 }))
  match c2 { Sent => {}, Full(_c) => { ret 22 }, Disconnected(_d) => { ret 23 }, }
  let n1 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 0, length_ticks: 480, pitch: 60, velocity: 0.9 }))
  match n1 { Sent => {}, Full(_e1) => { ret 24 }, Disconnected(_f1) => { ret 25 }, }
  let n2 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 1920, length_ticks: 480, pitch: 67, velocity: 0.85 }))
  match n2 { Sent => {}, Full(_e2) => { ret 26 }, Disconnected(_f2) => { ret 27 }, }
  let cc = cmd_tx.try_send(EngineCommand::PatternCommit)
  match cc { Sent => {}, Full(_g) => { ret 28 }, Disconnected(_h) => { ret 29 }, }
  let cr = cmd_tx.try_send(EngineCommand::RenderTo(96000))
  match cr { Sent => {}, Full(_i) => { ret 30 }, Disconnected(_j) => { ret 31 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  let mut seen1: bool = false
  let mut spins: i32 = 0
  while seen1 == false && spins < 60000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => { let info = rep_info(rep) if info.kind == 4 { seen1 = true } },
      Empty => { let _sl = sleep_ms(1) spins = spins + 1 },
      Disconnected => { seen1 = true },
    }
  }
  if seen1 == false { ret 60 }

  let crw = cmd_tx.try_send(EngineCommand::Rewind)
  match crw { Sent => {}, Full(_k) => { ret 32 }, Disconnected(_l) => { ret 33 }, }
  let cr2 = cmd_tx.try_send(EngineCommand::RenderTo(192000))
  match cr2 { Sent => {}, Full(_m) => { ret 34 }, Disconnected(_n) => { ret 35 }, }

  let mut seen2: bool = false
  let mut spins2: i32 = 0
  while seen2 == false && spins2 < 60000 {
    let got2 = rep_rx.try_recv()
    match got2 {
      Value(rep2) => { let info2 = rep_info(rep2) if info2.kind == 4 { seen2 = true } },
      Empty => { let _sl2 = sleep_ms(1) spins2 = spins2 + 1 },
      Disconnected => { seen2 = true },
    }
  }
  if seen2 == false { ret 61 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_o) => { ret 36 }, Disconnected(_p) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "rewind session exited nonzero");

    let s = read_wav_samples(&temp.path().join("rewind.wav"));
    assert_eq!(s.len(), 192_000);
    assert_eq!(
        &s[0..96_000],
        &s[96_000..192_000],
        "after Rewind the second bar is not an exact replay of the first"
    );
}

/// Swapping the active pattern while the transport is running must not
/// click: the outgoing pattern's sounding notes are released on their own
/// release ramps rather than cut.
#[test]
fn pattern_swap_while_playing_is_click_free() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("swap.wav", 48000, 1)
  let wav_handle: i32 = match made { Ok(w) => w.handle, Err(_e) => { ret 10 }, }

  // Pattern A: one nearly bar-long note per bar, so something is always
  // sounding when the swap lands.
  let c1 = cmd_tx.try_send(EngineCommand::PatternBegin(PatternHeader { length_ticks: 3840, track_count: 1 }))
  match c1 { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }
  let c2 = cmd_tx.try_send(EngineCommand::PatternTrack(TrackParamsMsg { track: 0, waveform: 0, gain: 0.8, attack_ms: 5.0, decay_ms: 50.0, sustain: 0.7, release_ms: 80.0 }))
  match c2 { Sent => {}, Full(_c) => { ret 22 }, Disconnected(_d) => { ret 23 }, }
  let n1 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 0, length_ticks: 3600, pitch: 60, velocity: 0.9 }))
  match n1 { Sent => {}, Full(_e1) => { ret 24 }, Disconnected(_f1) => { ret 25 }, }
  let cc = cmd_tx.try_send(EngineCommand::PatternCommit)
  match cc { Sent => {}, Full(_g) => { ret 26 }, Disconnected(_h) => { ret 27 }, }
  // 30 s of audio: long enough that the swap below lands mid-render even
  // though offline rendering runs at CPU speed.
  let cr = cmd_tx.try_send(EngineCommand::RenderTo(1440000))
  match cr { Sent => {}, Full(_i) => { ret 28 }, Disconnected(_j) => { ret 29 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  // Let the render get going, then swap in pattern B (an octave up) while
  // the transport is running.
  let _sl0 = sleep_ms(100)
  let b1 = cmd_tx.try_send(EngineCommand::PatternBegin(PatternHeader { length_ticks: 3840, track_count: 1 }))
  match b1 { Sent => {}, Full(_k) => { ret 30 }, Disconnected(_l) => { ret 31 }, }
  let b2 = cmd_tx.try_send(EngineCommand::PatternTrack(TrackParamsMsg { track: 0, waveform: 0, gain: 0.8, attack_ms: 5.0, decay_ms: 50.0, sustain: 0.7, release_ms: 80.0 }))
  match b2 { Sent => {}, Full(_m) => { ret 32 }, Disconnected(_n) => { ret 33 }, }
  let b3 = cmd_tx.try_send(EngineCommand::PatternNote(PatternNoteMsg { track: 0, tick: 0, length_ticks: 3600, pitch: 72, velocity: 0.9 }))
  match b3 { Sent => {}, Full(_o) => { ret 34 }, Disconnected(_p) => { ret 35 }, }
  let b4 = cmd_tx.try_send(EngineCommand::PatternCommit)
  match b4 { Sent => {}, Full(_q) => { ret 36 }, Disconnected(_r) => { ret 37 }, }

  let mut seen: bool = false
  let mut spins: i32 = 0
  while seen == false && spins < 60000 {
    let got = rep_rx.try_recv()
    match got {
      Value(rep) => { let info = rep_info(rep) if info.kind == 4 { seen = true } },
      Empty => { let _sl = sleep_ms(1) spins = spins + 1 },
      Disconnected => { seen = true },
    }
  }
  if seen == false { ret 60 }

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_s) => { ret 38 }, Disconnected(_t) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "swap session exited nonzero");

    let s = read_wav_samples(&temp.path().join("swap.wav"));
    assert_eq!(s.len(), 1_440_000, "expected 30 s of audio");

    // The swap must actually have happened: C4 at the start, C5 at the end.
    let head = zero_cross_freq(&s, 10_000, 20_000);
    let tail = zero_cross_freq(&s, 1_400_000, 20_000);
    assert!(
        (head - 261.6).abs() < 15.0,
        "expected pattern A (C4, 261.6 Hz) at the head, measured {:.1} Hz",
        head
    );
    assert!(
        (tail - 523.3).abs() < 25.0,
        "expected pattern B (C5, 523.3 Hz) at the tail — the swap did not \
         take effect; measured {:.1} Hz",
        tail
    );

    // A C5 sine at this amplitude steps by at most ~3600 per sample; a cut
    // waveform would step by tens of thousands.
    let step = max_step(&s);
    assert!(
        step < 6000,
        "pattern swap produced a discontinuity of {} — that is a click",
        step
    );
}

/// Stop chokes sounding voices on a short fixed release rather than
/// cutting them, and leaves nothing ringing.
#[test]
fn stop_chokes_voices_without_clicking() {
    let main_src = r#"
fn main() -> i32 {
  let mut cmd_rx: Receiver<EngineCommand>
  let cmd_tx: Sender<EngineCommand> = channel(64, &mut cmd_rx)
  let mut rep_rx: Receiver<EngineReport>
  let rep_tx: Sender<EngineReport> = channel(64, &mut rep_rx)

  let made = wav_create("stop.wav", 48000, 1)
  let wav_handle: i32 = match made { Ok(w) => w.handle, Err(_e) => { ret 10 }, }

  let kind: u32 = sink_wav()
  let engine: FnOnce<(), i32> = move () -> {
    ret engine_run(cmd_rx, rep_tx, kind, wav_handle, 48000, 1)
  }
  let started = spawn(engine)
  let handle = match started { Ok(h) => h, Err(_se) => { ret 41 }, }

  // A held live note, then Stop. Nothing may ring past the choke.
  let n1 = cmd_tx.try_send(EngineCommand::NoteOn(NoteCmd { track: 0, pitch: 69, velocity: 1.0 }))
  match n1 { Sent => {}, Full(_a) => { ret 20 }, Disconnected(_b) => { ret 21 }, }
  let _s1 = sleep_ms(20)
  let st = cmd_tx.try_send(EngineCommand::Stop)
  match st { Sent => {}, Full(_c) => { ret 22 }, Disconnected(_d) => { ret 23 }, }
  let _s2 = sleep_ms(60)

  let cs = cmd_tx.try_send(EngineCommand::Shutdown)
  match cs { Sent => {}, Full(_m) => { ret 24 }, Disconnected(_n) => {}, }
  ret match handle.join() { Ok(code) => code, Err(_je) => 40, }
}
"#;

    let (code, temp) = build_and_run(main_src);
    let Some(code) = code else { return };
    assert_eq!(code, 0, "stop session exited nonzero");

    let s = read_wav_samples(&temp.path().join("stop.wav"));
    assert!(!s.is_empty(), "engine rendered nothing");
    assert!(
        s.iter().any(|&x| x.unsigned_abs() > 1000),
        "the note never sounded"
    );

    // Offline, the engine renders only while something is sounding, so the
    // file ends exactly where the choke finished. The contract is that the
    // choke RAMPED to zero rather than cutting: the last audible sample
    // must be a small one. A hard cut would leave the tone at whatever
    // amplitude it happened to be at (up to ~26000 here).
    let last_audible = s
        .iter()
        .rposition(|&x| x != 0)
        .expect("engine rendered pure silence");
    assert!(
        s[last_audible].unsigned_abs() < 1500,
        "Stop cut the voice at amplitude {} instead of choking it to zero",
        s[last_audible].unsigned_abs()
    );
    assert!(
        s[last_audible..].iter().skip(1).all(|&x| x == 0),
        "something was still ringing after the choke completed"
    );

    let step = max_step(&s);
    assert!(step < 6000, "Stop produced a click: step of {}", step);
}
